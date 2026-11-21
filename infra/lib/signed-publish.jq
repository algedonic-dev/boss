# signed-publish.jq — is this publish-a-protocol packet a signature of
# exactly the publish in front of the caller? ONE definition, read by
# infra/gcp/publish-workflow.sh (registry workflow) and
# infra/gcp/publish-pool.sh (registry pool). It was inline in
# publish-workflow.sh until backlog f399458f gave the packet a second
# door; two copies of "what a signature is" would drift (CLAUDE.md §9a).
#
# Input: the packet, as GET /api/jobs/{id} answers it (unwrapped).
# Args (all --arg unless said):
#   $shape      approve's step_shape_hash AS IT STANDS NOW (step-shape.sh)
#   $approvers  --argjson, the verb file's non-empty `approvers` list
#   $verb_file  the verb file's path, named in refusals
#   $registry   the registry the caller publishes into: workflow | pool
#   $name       the row's name (a protocol kind, a pool name)
#   $path       the tree path the row lives at
#   $sha        sha256 of the bytes the caller is about to publish
#   $step       the caller's automaton step slug: publish | publish-pool
# Output: {refuse: "<why, in words>"} or {by, at, live}.
#
# A v1 packet (filed before the registry widening) carries no
# `registry` and no `row_name`: it is a protocol row named by
# `protocol_kind`, on the packet and on approve, and reads so here.
def no(m): {refuse: m};
((.steps // []) | map(select(.spec_slug == "approve")) | .[0]) as $a
| ((.steps // []) | map(select(.spec_slug == $step)) | .[0]) as $p
| ($a.metadata // {}) as $am
| (.metadata.registry // "workflow") as $reg
| (.metadata.row_name // .metadata.protocol_kind // "") as $row
| ($am.row_name // $am.protocol_kind // "") as $arow
| [($a.sign_offs // [])[] | select(.voided_at == null) | (.authority_id // "none" | tostring)] as $live
| if .kind != "publish-a-protocol" then
    no("packet \(.id[0:8]) is a \(.kind // "?"), not a publish-a-protocol")
  elif .status != "open" then
    no("packet \(.id[0:8]) is \(.status // "?"), not open")
  elif $reg != $registry then
    no("packet \(.id[0:8]) asks for a \($reg | tostring) row, not a \($registry) row")
  elif $row != $name then
    no("packet \(.id[0:8]) asks to publish \(if $row == "" then "nothing" else $row end | tostring), not \($name)")
  elif ($p == null) or ($p.status == "completed") or ($p.status == "skipped") then
    no("its \($step) step is \(($p // {}).status // "absent"): a signature is spent on one publish")
  elif $a.status != "completed" then
    no("its approve step is \($a.status // "absent"), not completed: a ready publish step is a flag, not a signature")
  elif $am.decision != "approved" then
    no("its approve step records decision \($am.decision // "none" | tojson), not \"approved\"")
  elif $a.assurance_required != "presence" then
    no("its approve step declares assurance \($a.assurance_required // "none"), not presence, so completing it proves no passkey")
  elif (($a.sign_offs_required // []) | length) == 0 then
    no("its approve step requires no sign-off, so nothing on it records who approved")
  elif ($live - $approvers | length) > 0 then
    no("its approve step carries a live sign-off by \($live - $approvers | unique | join(", ")), who is not among the approvers \($verb_file) names (\($approvers | join(", ")))")
  elif ($am.registry // "workflow") != $registry then
    no("its approve step signs a \($am.registry | tostring) row, not a \($registry) row")
  elif $arow != $name or ($am.spec_path // "") != $path then
    no("its approve step signs \(if $arow == "" then "none" else $arow end | tostring) from \($am.spec_path // "none" | tostring), not \($name) from \($path)")
  elif ($am.spec_sha256 // "") != $sha then
    no("its approve step signs \($path) at sha256 \($am.spec_sha256 // "none" | tostring), and the bytes here hash to \($sha): these are not the bytes that were signed")
  elif (($am.live_version // "") | tostring | test("^[0-9]+$")) | not then
    no("its approve step signs no live version (\($am.live_version // "none" | tostring)), so the diff it signed was against nothing")
  else
    [ $a.sign_offs_required[] as $r
      | [($a.sign_offs // [])[] | select(.role == $r)] as $mine
      | if ($mine | length) == 0 then {err: "no \($r) sign-off is stamped on its approve step"}
        else $mine[-1] as $last
        | if $last.assurance != "presence" or (($last.presence_nonce // "") | type) != "string" or ($last.presence_nonce // "") == "" then
            {err: "the newest \($r) sign-off is \($last.assurance // "session")-assured, not presence: no passkey signed it"}
          elif ([$approvers[] | select(. == ($last.authority_id // ""))] | length) == 0 then
            {err: "the newest \($r) sign-off is by \($last.authority_id // "nobody"), who is not among the approvers \($verb_file) names"}
          elif $last.shape_hash != $shape then
            {err: "the newest \($r) sign-off is bound to shape \($last.shape_hash // "none"), and the approve step now hashes to \($shape): what is on the step is not what was signed"}
          elif $last.voided_at != null then
            {err: "the newest \($r) sign-off was voided at \($last.voided_at | tostring)"}
          else {by: $last.authority_id, at: ($last.stamped_at // "" | tostring)} end
        end ] as $per
    | [$per[] | select(has("err")) | .err] as $errs
    | if ($errs | length) > 0 then no($errs | join("; "))
      else {by: ([$per[] | .by] | unique | join(", ")), at: ([$per[] | .at] | max),
            live: ($am.live_version | tostring)} end
  end
