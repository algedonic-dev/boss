# credential-delivery.sh — a forge-host deposit records, on the rotation
# packet, that the host now holds the value the broker installed. SOURCED,
# not run. The ONE definition both forgejo deposits use:
# infra/forge/credential-deposit.sh (the checkout token) and
# infra/forge/registry-token-deposit.sh (the registry push token).
#
# WHY A FILE (backlog 683d7fe9). The record is what the broker's revoke
# waits for (`credential.rotate.forgejo`, delivery = "off-host"): the
# packet's `delivered` step, completed with the last eight of the value
# the host installed. It was written inline in credential-deposit.sh, and
# every line of it was paid for in review — the row count held to
# `.total`, two packets refused rather than guessed between, the Secret
# read AGAIN after the packet is found (review F3 of car 85b7b55f), the
# machine token in a 0600 file and never an argv (backlog 5f3ad356). A
# second deposit needs the same record for the same handler; a second
# copy of that judgement would be a fact living twice (CLAUDE.md §9a),
# so it was moved here verbatim and the first deposit calls it.
#
# THE CALLER'S SHELL SUPPLIES, by these names:
#   INFRA             the tree's infra directory (for boss-api-curl.sh)
#   CRED             the credential's registry id (the packet's Subject)
#   $1                the file the value was installed into
#   SECRET_VALUE      the Secret's value as this pass read it at its start
#   read_secret       re-reads the Secret into READ_VALUE and READ_STATE
#   last8             the last eight characters of its argument
#   delivery_proof    returns 0 when the INSTALLED value is proven to
#                     work on this host; on 1, DELIVERY_PROOF_WHY says why
#   rc                the script's exit status, set to 1 on a fault here
#   infra/lib/sor.sh and infra/lib/secret-header.sh already sourced, and
#   the caller's EXIT trap already set (secret-header.sh, ORDER)
# AND RECEIVES: DELIVERY_STATE, one line for the converge packet.
#
# WHO IT SIGNS AS: automation:credential-deposit, for both callers — the
# id the checkout token's deposit has always signed this record with,
# spelled as a literal so the roster pin can hold it to a row
# (every_script_sender_signs_as_a_registered_automation.rs). Which
# deposit wrote a record is read off `delivered_to`, the installed path.
#
# NO VALUE LEAVES A VARIABLE HERE: the request carries a last eight and a
# path. Always returns 0; the line and `rc` are the verdict.

record_credential_delivery() {
    local dest="$1" jobs rows total pending n job step body l8 hdrs boss_user api_curl
    boss_user="{\"id\":\"automation:credential-deposit\",\"role\":\"platform-admin\",\"access_tier\":\"operator\",\"territory_account_ids\":[],\"direct_report_ids\":[],\"department\":\"platform\"}"
    api_curl="$INFRA/boss-api-curl.sh"
    [ -x "$api_curl" ] || api_curl=boss-api-curl.sh
    if [ -z "${BOSS_JOBS_URL:-}" ]; then
        DELIVERY_STATE="not recorded: BOSS_JOBS_URL is unset (/etc/boss/sor.env)"
        return 0
    fi
    hdrs=(-H "x-boss-user: $boss_user")
    # The machine token rides to curl in a 0600 file, never in its argv,
    # where every local user reads it in ps — the defect the publish
    # review of 8d7a3507 found at this line (backlog 5f3ad356). This
    # runs in the script's own shell, after its EXIT trap. It is the
    # mounted Secret's `current` slot, stamped only on an estate host
    # (infra/lib/secret-header.sh machine_token_header; design 6805c764
    # car 4).
    if ! machine_token_header MT_HDR "$BOSS_JOBS_URL"; then
        DELIVERY_STATE="not recorded: the machine token's header file could not be written; the next pass retries"
        return 0
    fi
    [ -z "$MT_HDR" ] || hdrs+=(-H "$MT_HDR")
    if ! jobs="$("$api_curl" -fsS "${hdrs[@]}" \
        "$BOSS_JOBS_URL/api/jobs?kind=rotate-a-credential&status=open&limit=500" 2>/dev/null)"; then
        # Not a failure of the deposit: the value is installed and the
        # next pass records it. The packet line says so.
        DELIVERY_STATE="not recorded: the jobs API did not answer; the next pass retries"
        return 0
    fi
    rows="$(jq '(if type == "object" and has("data") then .data else . end) | length' <<< "$jobs")"
    total="$(jq '(if type == "object" then .total else null end) // empty' <<< "$jobs")"
    if [ -n "$total" ] && [ "$total" != "$rows" ]; then
        DELIVERY_STATE="refused: the open-rotation read held $rows of $total rows — a limit is not a filter"
        rc=1
        return 0
    fi
    pending="$(jq -c --arg c "$CRED" '[(if type == "object" and has("data") then .data else . end)[]
        | select(.status == "open" and (.subject.id // .subject_id) == $c)
        | {job: .id, step: ((.steps // [])[] | select(.spec_slug == "delivered" and (.status == "ready" or .status == "active")) | .id)}]' <<< "$jobs")"
    n="$(jq 'length' <<< "$pending")"
    if [ "$n" -eq 0 ]; then
        DELIVERY_STATE="no open rotation of $CRED awaits delivery"
        return 0
    fi
    if [ "$n" -gt 1 ]; then
        DELIVERY_STATE="refused: $n open rotations of $CRED await delivery ($(jq -r '[.[].job[:8]] | join(", ")' <<< "$pending")) — not guessing which"
        rc=1
        return 0
    fi
    DELIVERY_PROOF_WHY=""
    if ! delivery_proof; then
        DELIVERY_STATE="not recorded: the installed value did not authenticate — $DELIVERY_PROOF_WHY"
        rc=1
        return 0
    fi
    # THE SECRET AGAIN, AFTER THE PACKET WAS FOUND (review F3 of car
    # 85b7b55f, 2026-09-26). This pass read the Secret at its start; the
    # broker may since have installed a NEW value and readied this very
    # step — it readies only after its install, so a read made now sees
    # whatever value readied it. Recording the start-of-pass value would
    # complete `delivered` with the OLD last eight: the broker refuses
    # the mismatch, and a completed step cannot be recorded again, so
    # the rotation could never finish. A value that moved is recorded
    # by the next pass, which installs it first.
    read_secret
    if [ "$READ_VALUE" != "$SECRET_VALUE" ]; then
        case "$READ_STATE" in
            unreadable*)
                DELIVERY_STATE="not recorded: the Secret could not be read again before the record — $READ_STATE"
                rc=1
                ;;
            *)
                DELIVERY_STATE="not recorded: the Secret moved during this pass (…$(last8 "$SECRET_VALUE") when read, $READ_STATE now); the next pass installs it and records that"
                ;;
        esac
        return 0
    fi
    job="$(jq -r '.[0].job' <<< "$pending")"
    step="$(jq -r '.[0].step' <<< "$pending")"
    l8="$(last8 "$SECRET_VALUE")"
    body="$(jq -nc --arg l8 "$l8" --arg to "${BOSS_NODE_ID:-$(hostname)}:$dest" \
        '{delivered_last_eight: $l8, delivered_to: $to}')"
    if ! "$api_curl" -fsS -X PATCH "${hdrs[@]}" -H "content-type: application/json" \
            -d "$body" "$BOSS_JOBS_URL/api/jobs/$job/steps/$step/metadata" >/dev/null 2>&1 \
        || ! "$api_curl" -fsS -X PUT "${hdrs[@]}" -H "content-type: application/json" \
            -d '{"status":"completed"}' "$BOSS_JOBS_URL/api/jobs/$job/steps/$step" >/dev/null 2>&1; then
        DELIVERY_STATE="not recorded: the write to ${job:0:8}'s delivered step failed; the next pass retries"
        rc=1
        return 0
    fi
    DELIVERY_STATE="recorded on ${job:0:8}: delivered …$l8"
}
