#!/usr/bin/env bash
#
# publish-pool — put ONE resource pool row live through the live pools
# door, from the bytes a publish-a-protocol packet its owner signed
# carries. The automaton step behind the website approval of a pool row.
#
# WHY IT EXISTS (backlog f399458f, ask 2)
# ---------------------------------------------------------------------
# Until 2026-11-08 a pool row reached the registry only through the
# platform seed on a converge, so a value change — robots:opus from 4 to
# 6, item 89b17d21 — rode a code car, a gate and a train. David,
# 2026-11-08: "we should be able to make these registry changes via a
# different protocol than the code changes." Ask 1 built the door
# (POST /api/pools/{name}/publish, `boss pool publish`); this verb is
# what the owner's approval fires. The row's bytes ride ON THE PACKET
# (`row_toml`), because the point is that no train carries them first;
# the tree write-back is the drift comparator's data car afterwards.
#
# WHAT IT PUBLISHES, AND ONLY THAT
# ---------------------------------------------------------------------
# The one arg is the pool's name. The script follows the request it
# answers (OPS_REQUEST_ID, handed by the runner) through
# `metadata.for_protocol_publish` — the edge the dispatcher rule
# publish-a-protocol-files-its-pool-publish writes — to a
# publish-a-protocol packet, READ BACK from the record, and judges it
# with infra/lib/signed-publish.jq, the one program publish-workflow.sh
# runs too: open, registry pool, this pool, its publish-pool step not
# done, approve completed `approved` with a live presence stamp per
# required role bound to approve's CURRENT shape by an employee
# infra/ops/verbs/publish-pool.json names in `approvers`, the signed
# spec_path this pool's tree path, the signed sha256 equal to the
# sha256 of the packet's `row_toml` — taken ONCE into this run's own
# copy, the bytes then published — and a numeric signed live version.
# Any failed check refuses 9 before the CLI runs. The publish is the
# CLI's checked sequence with `--over <signed live version>`: the CLI
# refuses early on a lineage read, and carries the same version to the
# door as `?over=`, where POST /api/pools/{name}/publish holds the write
# to it INSIDE its own transaction — so a version landed between the
# CLI's read and its write is refused 409 by the door, never published
# over (review R1 of car 4019ba69, 2026-11-08).
#
# USAGE
#   publish-pool.sh <pool-name>
# EXIT
#   0  published and confirmed by reading the lineage back
#   2  usage: no name, or a name the pattern refuses
#   4  the CLI refused the row (lint, moved lineage, the door said no)
#   7  NOT CONFIRMED: the CLI ran and did not print its confirmation
#   9  REFUSED: the request names no packet that signs this publish
#   78 configuration: no SoR, no jq/curl, no CLI, a CLI with no door
set -uo pipefail

# shellcheck source=infra/lib/jq.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/jq.sh"

NAME="publish-pool"
SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

say() { printf '%s: %s\n' "$NAME" "$*"; }
refuse() { # <exit> <message...>
    local rc="$1"; shift
    printf '%s: REFUSED — %s\n' "$NAME" "$*"
    exit "$rc"
}

# --- arguments ------------------------------------------------------------
POOL="${1:-}"
if [ -z "$POOL" ] || [ $# -gt 1 ]; then
    echo "$NAME: usage: $(basename "$0") <pool-name>" >&2
    exit 2
fi
# The allowlist's pattern, applied again here: the runner is one reader
# of it and this script is another.
if ! grep -Eqx -- '^[a-z][a-z0-9_-]{0,40}(:[a-z0-9][a-z0-9_.-]{0,40})?$' <<<"$POOL"; then
    echo "$NAME: usage: '$POOL' is not a pool name (^[a-z][a-z0-9_-]{0,40}(:[a-z0-9][a-z0-9_.-]{0,40})?$)" >&2
    exit 2
fi
POOL_PATH="infra/platform/pools/$POOL.toml"

# --- configuration --------------------------------------------------------
if [ -z "${BOSS_JOBS_URL:-}" ]; then
    echo "$NAME: BOSS_JOBS_URL is not set, and there is no safe default — a publish against the wrong instance answers instead of erroring (CLAUDE.md §Doors). The ops-runner's unit pins it." >&2
    exit 78
fi
for tool in jq curl sha256sum; do
    command -v "$tool" >/dev/null 2>&1 || { echo "$NAME: no $tool on PATH — nothing judged, nothing published" >&2; exit 78; }
done
BOSS_BIN="${BOSS_BIN:-}"
if [ -z "$BOSS_BIN" ]; then
    if command -v boss >/dev/null 2>&1; then BOSS_BIN="$(command -v boss)"; else BOSS_BIN=/usr/local/bin/boss; fi
fi
if [ ! -x "$BOSS_BIN" ]; then
    echo "$NAME: no boss CLI at $BOSS_BIN — the publish IS the CLI's checked sequence (boss pool publish). boss-gcp-converge installs it from the cluster image on each tick." >&2
    exit 78
fi

# Who the publish signs as: the account running it (the runner's
# automation). The same identity is the header the reads present.
export BOSS_ACTOR="${BOSS_ACTOR:-automation:ops-runner}"
BOSS_USER="{\"id\":\"$BOSS_ACTOR\",\"role\":\"platform-admin\",\"access_tier\":\"operator\",\"territory_account_ids\":[],\"direct_report_ids\":[],\"department\":\"platform\"}"

TMP=$(mktemp -d) || exit 1
trap 'rm -rf "$TMP"' EXIT

# The machine token rides every read of the record, never required
# (design 6805c764), exactly as publish-workflow.sh presents it.
PP_MT_HDR=""
# shellcheck source=infra/lib/secret-header.sh
if [ -r "$SELF_DIR/../lib/secret-header.sh" ]; then . "$SELF_DIR/../lib/secret-header.sh"; fi
if declare -F machine_token_header >/dev/null; then
    machine_token_header PP_MT_HDR "$BOSS_JOBS_URL" || PP_MT_HDR=""
fi

# --- the host's CLI has the door ---------------------------------------------
# A CLI built before backlog f399458f has no `pool publish --over`; it
# would refuse the flag, a fault of the host and not of the packet.
if ! "$BOSS_BIN" pool publish --help > "$TMP/help.out" 2>&1 || ! grep -q -- '--over' "$TMP/help.out"; then
    refuse 78 "$BOSS_BIN has no \`boss pool publish --over\` (it predates the pools door's approval road, backlog f399458f), so it cannot publish over a signed live version; boss-gcp-converge refreshes it from the cluster image"
fi

# --- 1. the signature, read back from the record ---------------------------------
UUID_RE='^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
read_job() { # <id> <out>: 0 with the job written, else refuses 9
    local code
    code=$(curl -q -sS -m 20 -o "$2" -w '%{http_code}' -H "x-boss-user: $BOSS_USER" \
        ${PP_MT_HDR:+-H "$PP_MT_HDR"} "$BOSS_JOBS_URL/api/jobs/$1" 2>"$TMP/job-curl.err")
    if [ "$code" != "200" ] || ! jq_doc_file "$2" || ! jq -e 'type == "object"' "$2" >/dev/null 2>&1; then
        refuse 9 "packet ${1:0:8} could not be read from $BOSS_JOBS_URL (HTTP ${code:-000}: $(head -c 200 "$TMP/job-curl.err" | tr '\n' ' ')), and a signature that cannot be read is not one — nothing published"
    fi
    jq '(if has("data") and (.data | type) == "object" then .data else . end)' "$2" > "$2.job" && mv "$2.job" "$2"
}
if ! [[ ${OPS_REQUEST_ID:-} =~ $UUID_RE ]]; then
    refuse 9 "OPS_REQUEST_ID ('${OPS_REQUEST_ID:0:40}') names no ops-request, so no signed packet can be followed from it — nothing published"
fi
read_job "$OPS_REQUEST_ID" "$TMP/request.json"
PACKET=$(jq -r '.metadata.for_protocol_publish // empty | strings' "$TMP/request.json")
if ! [[ $PACKET =~ $UUID_RE ]]; then
    refuse 9 "request ${OPS_REQUEST_ID:0:8} names no publish-a-protocol packet (metadata.for_protocol_publish is '${PACKET:0:40}') — nothing published"
fi
read_job "$PACKET" "$TMP/packet.json"

# The bytes, taken ONCE into this run's copy and hashed once: what is
# judged is what is published (review R1 of car 214ae147, the reason
# publish-workflow.sh copies its file before it signs).
ROW="$TMP/row/$POOL.toml"
mkdir -p "$TMP/row"
if ! jq -j '.metadata.row_toml // empty | strings' "$TMP/packet.json" > "$ROW" || [ ! -s "$ROW" ]; then
    refuse 9 "publish-a-protocol ${PACKET:0:8} carries no row_toml, so there are no signed bytes to publish — nothing published"
fi
ROW_SHA="$(sha256sum "$ROW" | cut -c1-64)"

approve=$(jq -c '((.steps // []) | map(select(.spec_slug == "approve")) | .[0]) // empty' "$TMP/packet.json")
[ -n "$approve" ] || refuse 9 "packet ${PACKET:0:8} has no approve step, so nothing on it signed a publish"
# shellcheck source=infra/lib/step-shape.sh
. "$SELF_DIR/../lib/step-shape.sh" || refuse 78 "infra/lib/step-shape.sh is missing, so no stamp can be checked against its shape"
shape=$(shape_hash_of "$approve") || refuse 9 "the shape of ${PACKET:0:8}'s approve step could not be computed"
# WHO MAY SIGN IS A NAMED LIST, NEVER A ROLE: this verb's own file.
VERB_FILE_REL="infra/ops/verbs/publish-pool.json"
approvers=$(jq -c '.approvers // empty
    | select(type == "array" and length > 0 and all(.[]; type == "string" and . != ""))' \
    "$SELF_DIR/../../$VERB_FILE_REL" 2>/dev/null) || approvers=""
[ -n "$approvers" ] || refuse 78 "$VERB_FILE_REL names no approvers (a non-empty list of employee ids), so no passkey can sign a pool publish"
verdict=$(jq -c -f "$SELF_DIR/../lib/signed-publish.jq" --arg shape "$shape" \
    --argjson approvers "$approvers" --arg verb_file "$VERB_FILE_REL" \
    --arg registry pool --arg name "$POOL" --arg path "$POOL_PATH" \
    --arg sha "$ROW_SHA" --arg step publish-pool "$TMP/packet.json" 2>"$TMP/signed.err") \
    || refuse 9 "packet ${PACKET:0:8} could not be judged — jq failed over it ($(head -c 200 "$TMP/signed.err" | tr '\n' ' '))"
why=$(printf '%s' "$verdict" | jq -r '.refuse // empty')
[ -z "$why" ] || refuse 9 "publish-a-protocol ${PACKET:0:8} is not a signature of this publish: $why — nothing published (packet ${OPS_REQUEST_ID})"
SIGNED_BY=$(printf '%s' "$verdict" | jq -r '.by')
SIGNED_AT=$(printf '%s' "$verdict" | jq -r '.at')
SIGNED_LIVE=$(printf '%s' "$verdict" | jq -r '.live')
say "signed: publish-a-protocol $PACKET, by $SIGNED_BY at $SIGNED_AT, over $POOL_PATH sha256 ${ROW_SHA:0:12} against live v$SIGNED_LIVE"

# --- 2. the publish — the CLI's own checked sequence ---------------------------------
if [ "$(sha256sum "$ROW" | cut -c1-64)" != "$ROW_SHA" ]; then
    refuse 9 "this run's copy of the signed row changed after it was judged — nothing published"
fi
if ! "$BOSS_BIN" pool publish "$POOL" "$ROW" --over "$SIGNED_LIVE" > "$TMP/publish.out" 2>&1; then
    say "boss pool publish refused; its output, whole:"
    sed 's/^/  /' "$TMP/publish.out"
    refuse 4 "the pools door did not take $POOL over live v$SIGNED_LIVE (above), so nothing went live (packet ${OPS_REQUEST_ID})"
fi
sed 's/^/  /' "$TMP/publish.out"
NEW_VERSION="$(sed -n "s/^boss pool: published .* v\([0-9][0-9]*\) — confirmed by reading the lineage back$/\1/p" "$TMP/publish.out")"
NEW_VERSION="${NEW_VERSION%%$'\n'*}"
case "${NEW_VERSION:-empty}" in
    empty|*[!0-9]*)
        say "NOT CONFIRMED — boss pool publish exited 0 without its confirmation line, so what went live is unknown (packet ${OPS_REQUEST_ID})"
        exit 7 ;;
esac
say "$POOL v$SIGNED_LIVE -> v$NEW_VERSION live at $BOSS_JOBS_URL — confirmed by reading the lineage back and comparing it to the signed row (equal); packet ${OPS_REQUEST_ID}"
exit 0
