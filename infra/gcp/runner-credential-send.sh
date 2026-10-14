#!/usr/bin/env bash
# runner-credential-send — the sending half of boss-gcp's ops runner
# credential delivery (backlog 1e50e66b). The forge runs it on every
# forge-converge tick through infra/forge/runner-credential-push.sh, which
# reads this script's transport — the broker-prepared deposit key, boss-gcp's
# pinned host key and the deposit account — out of the cluster through the
# forge's admin kubeconfig and hands them in. The receiver it speaks to is
# infra/gcp/runner-credential-recv.sh, behind the deposit key's forced
# command on boss-gcp.
#
# NOTHING MOVES WITHOUT THE SIGNED ENROLLMENT. The key's prepare-a-deposit-key
# packet must carry a completed install receipt naming THIS key and a
# completed, approved enroll step whose signed line is exactly that key
# behind this receiver's forced command; until enroll is signed the pass is
# `not yet` (75), and a packet that disagrees is a refusal (78). No key
# mint, receiver enrollment or host-key discovery happens here.
#
# ONE PASS, ONE OF THREE THINGS:
#   1. DELIVER. Secret boss/ops-runner-credential stages `boss-gcp.next` for
#      a rotation: the envelope (value, packet, attempt, Secret uid, bound to
#      the broker's original recovery witness) goes to the receiver, which
#      installs it, proves it through the jobs API's credential door and
#      records delivery on the rotation packet; its receipt is read back.
#   2. VERIFY. Nothing is staged, and the deposit key's `verify` step is
#      still open with no record on it: the receiver is reached with the
#      enrolled key, says its greeting, is handed NO byte, and refuses the
#      empty envelope in its own words. That is the machine-run effect the
#      step's procedure asks for — this key, this pinned host, this
#      account, this forced command — and it is written onto the step as
#      `context_md` (the one key a machine may write on a human-only step,
#      boss-jobs human_only.rs) for the person who completes it.
#   3. NOTHING. Nothing staged and nothing owed: no connection is made.
#
# THE RECEIVER SPEAKS FIRST, OR NOTHING IS SENT (backlog dea2236f). ssh's
# stdin and stdout are two fifos, so the far side's first line is read
# while nothing has been written to it, and only the receiver's own
# greeting lets the envelope go. Text that is not the greeting, or a clean
# end on an empty stream, is a key that reaches a SHELL — RED on its own
# route, naming the line to remove — and is handed nothing. An older
# receiver that does not speak first refuses the empty stream in its own
# words: `not yet` while the tree's receiver is younger than boss-gcp's
# converge can have installed (BOSS_RUNNER_RECV_SINCE, the epoch the
# forge's tree last changed the receiver), RED after.
#
# NO VALUE EVER LEAVES A FILE OR A VARIABLE except on ssh's stdin: every
# line here names slots, packets and lengths. The receiver's receipt is
# checked never to contain the value.
#
# Exit: 0 delivered, verified, or nothing to do; 75 not yet; 1 failed or
# RED, named; 78 a refusal of how it was invoked or of the enrollment.
# Tracing off, first: an exported SHELLOPTS=xtrace would print the value.
set +x
set -euo pipefail
export LC_ALL=C
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INFRA="$(cd "$HERE/.." && pwd)"
ME=runner-credential-send
refuse() { echo "$ME: refused: $1" >&2; exit 78; }
fail() { echo "$ME: failed; remote delivery not proved${1:+ — $1}" >&2; exit 1; }
not_yet() { echo "$ME: not yet — $1"; exit 75; }
say() { echo "$ME: $*"; }
# red ROUTE WHY — a refusal a person must read, on a line of its own.
red() { echo "RED $ME/$1 refused: $2" >&2; exit 1; }
ENROLLMENT="${BOSS_RUNNER_DEPOSIT_ENROLLMENT:-}"
[[ $ENROLLMENT =~ ^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$ ]] || refuse 'recorded enrollment packet absent'
[ "$#" -eq 0 ] || refuse 'no transport-selected arguments are accepted'
. "$INFRA/lib/sor.sh"
. "$INFRA/lib/jq.sh"
[ -n "${BOSS_JOBS_URL:-}" ] || refuse 'system-of-record configuration absent'
KEY="${BOSS_RUNNER_DEPOSIT_KEY:-/etc/boss/runner-deposit-key/private_key}"
KNOWN="${BOSS_RUNNER_DEPOSIT_KNOWN_HOSTS:-/etc/boss/runner-deposit-key/known_hosts}"
[ -f "$KEY" ] && [ -f "$KNOWN" ] || refuse 'enrollment transport or pinned host key absent'
# The whole transfer (connect, the receiver's work, its answer), and the
# wait for its first line inside it: connect, sudo, and one printf.
SHIP_TIMEOUT_S="${BOSS_RUNNER_SEND_TIMEOUT_S:-60}"
GREETING_TIMEOUT_S="${BOSS_RUNNER_SEND_GREETING_TIMEOUT_S:-30}"
[[ $SHIP_TIMEOUT_S =~ ^[1-9][0-9]*$ ]] || refuse 'BOSS_RUNNER_SEND_TIMEOUT_S is not a number of seconds'
[[ $GREETING_TIMEOUT_S =~ ^[1-9][0-9]*$ ]] || refuse 'BOSS_RUNNER_SEND_GREETING_TIMEOUT_S is not a number of seconds'
# How long after the tree changed the receiver an older one on boss-gcp is
# still `not yet`: four of boss-gcp's half-hourly converges.
RECV_GRACE_S=7200
umask 077
WORK="$(mktemp -d)" || fail
trap 'rm -rf "$WORK"' EXIT
# A Secret's read-only mode is not the host SSH client's private-key mode,
# and the broker stores the PEM without its last newline, which some key
# formats' parsers want back. The private copy is scoped to this run and
# removed on every exit.
{ cat -- "$KEY" && printf '\n'; } > "$WORK/key" 2>/dev/null || refuse 'enrollment transport key unreadable'
chmod 600 "$WORK/key" || fail
. "$INFRA/lib/secret-header.sh"
machine_token_header MT_HDR "$BOSS_JOBS_URL" || fail
HDRS=(-H "x-boss-user: $(sor_reader_header automation:runner-credential-send)")
[ -z "$MT_HDR" ] || HDRS+=(-H "$MT_HDR")
API_CURL="$INFRA/boss-api-curl.sh"
CODE="$("$API_CURL" -sS "${HDRS[@]}" -o "$WORK/enrollment" -w '%{http_code}' "$BOSS_JOBS_URL/api/jobs/$ENROLLMENT" 2>/dev/null)" || fail
[ "$CODE" = 200 ] || refuse 'enrollment packet unreadable'
PUBLIC="$(ssh-keygen -y -f "$WORK/key" 2>/dev/null)" || refuse 'enrollment key unreadable'
PUBLIC="$(printf '%s\n' "$PUBLIC" | awk 'NF >= 2 {print $1 " " $2}')" || refuse 'enrollment key unreadable'
# WHAT WAS SIGNED IS THE LINE, AND THE KEY IS THE INSTALL STEP'S
# (prepare-a-deposit-key v2, item 3dc0b247; item dea2236f). The enroll
# step carries no bare key any more: its signer placed the whole
# authorized_keys line, which the broker joined from its rule's
# forced_command and the public half it prepared. So the key this
# transport holds must be the public_key on the COMPLETED install
# receipt, and the signed line must be exactly that key behind this
# receiver's forced command — the rule row's own text, held equal to
# this constant by runner_credential_recv_sh.rs.
RECEIVER_COMMAND='exec sudo -n /usr/local/libexec/boss/runner-delivery/infra/gcp/runner-credential-recv.sh'
# The receiver's first line, word for word (infra/gcp/runner-credential-recv.sh
# prints it before it reads a byte; runner_credential_recv_sh.rs holds the
# two equal), and its refusal of the empty envelope a verify pass hands it.
GREETING='runner-credential-recv: boss-gcp: ready for one envelope'
EMPTY_REFUSAL='runner-credential-recv: refused an empty envelope; installed credential unchanged'
# What an OLDER receiver — one that reads its whole stdin before it says
# anything — answers an empty stream with.
OLD_REFUSAL='runner-credential-recv: refused; installed credential unchanged'
jq_doc_file "$WORK/enrollment" || refuse 'enrollment packet contains no document'
# NOT SIGNED YET is not a refusal: the broker prepares the pair, David
# places the line and signs, and between the two this pass has nothing it
# may use the key for.
if jq -e '.kind == "prepare-a-deposit-key"
    and ([.steps[] | select(.spec_slug == "enroll" and .status != "completed" and .status != "skipped")] | length == 1)' \
    "$WORK/enrollment" >/dev/null 2>&1; then
    not_yet "the enroll step of prepare-a-deposit-key packet ${ENROLLMENT:0:8} is not signed, so the deposit key is not authorized to reach boss-gcp's receiver; nothing was read or sent"
fi
jq_doc_file "$WORK/enrollment" || refuse 'enrollment packet contains no document'
jq -e --arg public "$PUBLIC" --arg enrollment "$ENROLLMENT" \
   --arg prefix "command=\"$RECEIVER_COMMAND\",restrict " '
  [.steps[] | select(.spec_slug == "install" and .status == "completed") | .metadata.public_key] as $installed
  | .id == $enrollment and .kind == "prepare-a-deposit-key" and (.subject.id // .subject_id) == "runner-deposit-key"
  and ((.metadata.abandoned // "") | tostring) != "true"
  and ($installed | length == 1) and ($installed[0] | type == "string")
  and (($installed[0] | split(" ") | .[0:2] | join(" ")) == $public)
  and ([.steps[] | select(.spec_slug == "enroll" and .status == "completed"
    and .metadata.decision == "approved" and .metadata.receiver_host == "boss-gcp"
    and .metadata.purpose == "ops-runner credential deposit"
    and (.metadata.enrollment_record | type == "string" and length > 0)
    and .metadata.authorized_keys_line == ($prefix + $installed[0]))] | length == 1)
' "$WORK/enrollment" >/dev/null 2>&1 || refuse 'exact receiver enrollment absent or mismatched'
# The verify step: its id, and whether a record is still owed on it — open,
# and nothing written for the person yet.
VERIFY_ID="$(jq -r '[.steps[] | select(.spec_slug == "verify"
    and (.status == "ready" or .status == "active")
    and ((.metadata.context_md // "") | tostring | length) == 0)][0].id // empty' "$WORK/enrollment" 2>/dev/null)" || VERIFY_ID=""
[[ $VERIFY_ID =~ ^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$ ]] || VERIFY_ID=""
ADDRESS="$(awk '
  /^\[\[node\]\]$/ {selected=0}
  /^id = "boss-gcp"$/ {selected=1}
  selected && /^address = / {gsub(/"/, "", $3); print $3}
' "$INFRA/estate/estate.toml")" || refuse 'enrolled estate host unavailable'
TARGET="${BOSS_RUNNER_DEPOSIT_TARGET:-}"
[[ $TARGET =~ ^[a-z_][a-z0-9_-]*@[^@[:space:]]+$ ]] || refuse 'enrolled receiver account configuration absent'
[ "${TARGET#*@}" = "$ADDRESS" ] || refuse 'receiver target differs from the declared estate host'
# Only the existing broker-owned runner Secret; a recovery transport is never
# read. kubectl is the deployment's own: BOSS_RUNNER_SEND_KUBECTL names the
# command line the forge reaches the cluster with (its admin kubeconfig
# through the kubectl container, as runner-credential-deposit.sh does).
if [ -n "${BOSS_RUNNER_SEND_KUBECTL:-}" ]; then
    read -r -a KUBECTL <<< "$BOSS_RUNNER_SEND_KUBECTL"
else
    KUBECTL=(kubectl)
fi
"${KUBECTL[@]}" -n boss get secret ops-runner-credential -o json > "$WORK/secret" 2>/dev/null || fail 'Secret boss/ops-runner-credential could not be read'
jq_doc_file "$WORK/secret" || fail 'Secret boss/ops-runner-credential is not one document'
MODE=deliver
if ! jq -e '(.data["boss-gcp.next"] // "") | length > 0' "$WORK/secret" >/dev/null 2>&1; then
    if [ -z "$VERIFY_ID" ]; then
        say "nothing to deliver: Secret boss/ops-runner-credential stages no boss-gcp.next, and the verify step of ${ENROLLMENT:0:8} owes no record; no connection was made"
        exit 0
    fi
    MODE=verify
fi
if [ "$MODE" = deliver ]; then
    jq_doc_file "$WORK/secret" || fail
    jq -er '.data["boss-gcp.next"]' "$WORK/secret" 2>/dev/null | base64 -d > "$WORK/value" || fail
    jq -er '.data["boss-gcp.next.minted-for"]' "$WORK/secret" 2>/dev/null | base64 -d > "$WORK/job" || fail
    JOB="$(cat "$WORK/job")"
    [[ $JOB =~ ^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$ ]] || fail
    jq_doc_file "$WORK/secret" || fail
    jq -er --arg key "boss-gcp.recovery.$JOB.witness" '.data[$key]' "$WORK/secret" 2>/dev/null | base64 -d > "$WORK/witness" || fail
    jq_doc_file "$WORK/witness" || fail
    jq -en --slurpfile witness "$WORK/witness" --slurpfile secret "$WORK/secret" \
      --rawfile value "$WORK/value" --arg job "$JOB" '
      $witness[0] as $w | select($w.version == 1 and $w.host == "boss-gcp"
        and $w.credential_id == "ops-runner-credential-boss-gcp"
        and $w.job_id == $job and $w.uid == $secret[0].metadata.uid and $w.promoted == false)
      | {version:1,host:$w.host,credential_id:$w.credential_id,job_id:$w.job_id,
          attempt:$w.attempt,secret_uid:$w.uid,value:$value}
    ' > "$WORK/envelope" 2>/dev/null || fail
    [ "$(stat -c %s "$WORK/envelope")" -le 8192 ] || fail
fi
# --- the transport: two steps, and the far side takes the first ---------
# The receiver forced command ignores the client command. No accept-new, agent,
# proxy, password fallback or dynamically discovered host key is permitted.
# -T: no pty — the receiver reads bytes, and a pty would translate them.
SSH_OPTS=(-F /dev/null -T -o BatchMode=yes -o IdentitiesOnly=yes -o IdentityAgent=none
    -o StrictHostKeyChecking=yes -o "UserKnownHostsFile=$KNOWN"
    -o GlobalKnownHostsFile=/dev/null -o ConnectTimeout=10
    -o ServerAliveInterval=10 -o ServerAliveCountMax=3 -i "$WORK/key")
mkfifo "$WORK/to" "$WORK/from" || fail 'the transfer could not hold its two pipes'
src=0
timeout -k 5 "$SHIP_TIMEOUT_S" ssh "${SSH_OPTS[@]}" "$TARGET" < "$WORK/to" > "$WORK/from" 2> "$WORK/ssh-error" &
SSH_PID=$!
# After the fork, so ssh keeps its own handling: a far side that closes
# early must be an exit status read below, never this shell killed by a
# write to a closed pipe with the envelope half out.
trap '' PIPE
exec {TO}> "$WORK/to" {FROM}< "$WORK/from"
# One line, at most 256 bytes of it: a far side that streams without a
# newline is not read without end.
FIRST="" heard=0
IFS= read -r -t "$GREETING_TIMEOUT_S" -n 256 -u "$FROM" FIRST || heard=$?
GREETED=0
if [ "$heard" -eq 0 ] && [ "$FIRST" = "$GREETING" ]; then
    GREETED=1
    if [ "$MODE" = deliver ]; then
        # A write the far side did not take shows as a receipt that is not one.
        cat "$WORK/envelope" >&"$TO" 2>/dev/null || true
    fi
fi
# Closing the stream is the end of the envelope — or, with nothing written,
# the question put to a far side that said nothing: an older receiver
# answers an empty deposit in its own words, a shell ends without one.
exec {TO}>&-
cat <&"$FROM" > "$WORK/receipt" || true
exec {FROM}<&-
wait "$SSH_PID" || src=$?
# The far side's stderr, as one bounded printable line for a reason. The
# receiver never echoes the value, and the value never reached a far side
# that did not greet.
why() { tr '\n' ' ' < "$WORK/ssh-error" | LC_ALL=C tr -c '[:print:]' '?' | cut -c1-300; }
HOST="${TARGET#*@}"
ACCOUNT="${TARGET%@*}"
FIX="The runner deposit key's line in ~$ACCOUNT/.ssh/authorized_keys on boss-gcp ($HOST) does not force the receiver, so this key opens that account where it may run one receiver command. REMOVE THAT LINE NOW, then place the authorized_keys_line on the enroll step of prepare-a-deposit-key packet $ENROLLMENT, whole — it forces: $RECEIVER_COMMAND. No credential byte was sent."
if [ "$GREETED" -eq 0 ]; then
    # NOTHING WAS SENT. What follows only names why, strongest evidence first.
    if [ -n "$FIRST" ]; then
        SAID="$(printf '%s' "$FIRST" | LC_ALL=C tr -c '[:print:]' '?' | cut -c1-120)"
        red key-unconfined "this key reaches a shell on boss-gcp: the far side accepted the key and answered with its own text ('$SAID') where the receiver's first line was expected. $FIX"
    fi
    LATE=""
    IFS= read -r LATE < "$WORK/receipt" || true
    if [ "$LATE" = "$GREETING" ]; then
        fail "boss-gcp's receiver said it was ready only after the $GREETING_TIMEOUT_S seconds this pass waits for that line; nothing was sent, and the next pass asks again"
    fi
    if [ -s "$WORK/receipt" ]; then
        red key-unconfined "this key reaches a shell on boss-gcp: the far side accepted the key, said nothing for the $GREETING_TIMEOUT_S seconds the receiver's first line is waited for, and then answered with its own text. $FIX"
    fi
    if [ "$src" -eq 124 ] || [ "$src" -eq 137 ]; then
        fail "the transfer to $TARGET did not finish inside $SHIP_TIMEOUT_S seconds and was stopped before the far side had said anything; nothing was sent"
    fi
    # SIGNED, AND REFUSED. This pass runs only once the enroll step is
    # signed, and its signer placed the line first — so sshd refusing the
    # key is a line that was never placed, or was removed, and a person
    # must look (ssh's own exit, 255, with sshd's own words).
    if [ "$src" -eq 255 ] && grep -Eq '^[^ ]+@[^ ]+: Permission denied \(publickey' "$WORK/ssh-error"; then
        red key-refused "boss-gcp refuses the runner deposit key although enroll step of prepare-a-deposit-key packet $ENROLLMENT is signed: the authorized_keys_line on that step is not in ~$ACCOUNT/.ssh/authorized_keys on boss-gcp. Place it, whole; nothing was sent"
    fi
    # An OLDER receiver, which reads its whole stdin before it says
    # anything, refused the empty stream in its own words. While the tree's
    # receiver is younger than boss-gcp's converge can have installed it,
    # that is the two halves landing apart; after, it is an install that
    # failed, and it is red (the machine token push's review N2).
    if [ "$src" -eq 65 ] && grep -Fxq "$OLD_REFUSAL" "$WORK/ssh-error"; then
        since="${BOSS_RUNNER_RECV_SINCE:-}"
        now="$(date -u +%s)"
        case "${since:-empty}" in
            empty | *[!0-9]*) ;;
            *)
                if [ "$since" -le "$now" ] && [ $((now - since)) -lt "$RECV_GRACE_S" ]; then
                    not_yet "boss-gcp's receiver does not speak first yet, so nothing was sent to it: the tree changed it $(( (now - since) / 60 )) minutes ago and boss-gcp's converge installs it within the half hour"
                fi
                ;;
        esac
        red receiver-stale "boss-gcp's receiver does not speak first, so nothing was sent to it: its converge has not installed this tree's runner-credential-recv.sh (infra/gcp/install-runner-credential-receiver.sh), or installed an older one; boss-gcp holds the runner credential it held"
    fi
    if [ "$src" -eq 0 ]; then
        red key-unconfined "this key reaches a shell on boss-gcp: the far side accepted the key, said nothing for the $GREETING_TIMEOUT_S seconds the receiver's first line is waited for, and ended cleanly on an empty stream, which the receiver never does. $FIX"
    fi
    fail "the transfer to $TARGET failed (ssh exit $src) before the receiver said it was ready: $(why); nothing was sent"
fi
# THE RECEIVER GREETED.
NOW="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
if [ "$MODE" = verify ]; then
    [ "$src" -eq 65 ] && grep -Fxq "$EMPTY_REFUSAL" "$WORK/ssh-error" \
        || red receiver "boss-gcp's receiver greeted and was handed no byte, and did not refuse the empty envelope in its own words (ssh exit $src: $(why)) — the program behind the key is not the receiver this tree installs"
    EFFECT="Verified by effect at $NOW by the forge's runner credential sender (infra/gcp/runner-credential-send.sh, run by forge-converge through infra/forge/runner-credential-push.sh). With the transport key this packet prepared (its public half is the one on the completed install step), against boss-gcp's pinned host key, the account $ACCOUNT at $HOST answered first with the runner receiver's greeting '$GREETING', was handed no byte, and refused the empty envelope in the receiver's own words ('$EMPTY_REFUSAL', exit 65). Nothing was delivered: Secret boss/ops-runner-credential stages no boss-gcp.next."
else
    [ "$src" -eq 0 ] || fail "the receiver greeted, was sent the envelope, and did not end cleanly (ssh exit $src): $(why)"
    # The greeting is the first line; the receipt is everything after it.
    jq_doc_file "$WORK/receipt" || fail 'the receiver answered with no single receipt'
    jq -e --slurpfile sent "$WORK/envelope" '
      $sent[0] as $s | .version == 1 and .recorded == true and .host == $s.host
      and .credential_id == $s.credential_id and .job_id == $s.job_id
      and .attempt == $s.attempt and .secret_uid == $s.secret_uid
      and (.original_delivery_receipt as $r | $r.version == 1
        and $r.credential_id == $s.credential_id and $r.phase == "verified"
        and $r.actor == "automation:ops-runner" and $r.observation_id == ($s.attempt + ":delivered")
        and ($r.event_id | type == "string") and ($r.timestamp | type == "string")
        and ($r.evidence_json | fromjson | .purpose == "authenticated-runner-delivery"
          and .job_id == $s.job_id and .host == $s.host and .attempt == $s.attempt))
    ' "$WORK/receipt" >/dev/null 2>&1 || fail
    # Preserve the receiver's complete original owner record for the caller.
    # Credential input remains private; it cannot ride a returned field.
    jq -s -e 'length == 1' "$WORK/receipt" >/dev/null 2>&1 || fail
    jq_doc_file "$WORK/receipt" || fail
    jq -e --rawfile value "$WORK/value" 'tostring | contains($value) | not' "$WORK/receipt" >/dev/null 2>&1 || fail
    EFFECT="Verified by effect at $NOW by the forge's runner credential sender (infra/gcp/runner-credential-send.sh): with the transport key this packet prepared, against boss-gcp's pinned host key, the account $ACCOUNT at $HOST answered first with the runner receiver's greeting, received the ops-runner-credential-boss-gcp envelope for rotation packet $(jq -r .job_id "$WORK/receipt"), installed it and recorded its delivery (event $(jq -r .original_delivery_receipt.event_id "$WORK/receipt"))."
fi
# THE RECORD ON THE PERSON'S STEP, once: context_md, which a machine may
# write on a human-only step while it is open; the answer (`verified`) and
# the completion stay the person's. A record that cannot be written leaves
# the step without one, so the next pass proves it and writes it again.
if [ -n "$VERIFY_ID" ]; then
    WHDRS=(-H "x-boss-user: {\"id\":\"automation:runner-credential-send\",\"role\":\"platform-admin\",\"access_tier\":\"operator\",\"territory_account_ids\":[],\"direct_report_ids\":[],\"department\":\"platform\"}")
    [ -z "$MT_HDR" ] || WHDRS+=(-H "$MT_HDR")
    jq -nc --arg t "$EFFECT" '{context_md: $t}' > "$WORK/record" || fail 'the verify record did not serialize'
    CODE="$("$API_CURL" -sS -X PATCH "${WHDRS[@]}" -H 'content-type: application/json' \
        --data-binary "@$WORK/record" -o "$WORK/recorded" -w '%{http_code}' \
        "$BOSS_JOBS_URL/api/jobs/$ENROLLMENT/steps/$VERIFY_ID/metadata" 2>/dev/null)" || CODE=""
    case "$CODE" in
        200 | 204) ;;
        *) fail "boss-gcp's receiver was verified by effect, but the record on the verify step of ${ENROLLMENT:0:8} was not written (HTTP ${CODE:-no answer}); the next pass proves it again" ;;
    esac
fi
if [ "$MODE" = verify ]; then
    say "verified: $ACCOUNT@boss-gcp answered first as the runner receiver under the enrolled key and refused the empty envelope; nothing was sent; recorded on the verify step of ${ENROLLMENT:0:8}"
    exit 0
fi
cat "$WORK/receipt"
