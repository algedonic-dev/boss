#!/usr/bin/env bash
#
# publish-workflow — put the tree's row for one platform workflow kind
# live in the registry, from the converged checkout on boss-gcp, with
# the refusals that make it safe to hand to a button.
#
# WHY IT EXISTS (backlog 3ce95b85, car 3a of 8f4e9cc0)
# ---------------------------------------------------------------------
# A change to an EXISTING platform workflow kind goes live only when an
# operator runs `boss workflow publish <kind> <toml>` on a host with the
# checkout — the seed is insert-if-missing by decision. That publish is
# the one act that lands a data car for an existing kind, and until this
# verb it had no door but an operator's terminal: the daily drift
# measurement (19dec171) and the Drift tab (#378) show the drift and
# offer nothing, because the server has no tree. CLAUDE.md §Doors: an
# ops verb is a path made safe — correct target, correct actor,
# pre-approved. David, 2026-09-11: "We might need some sort of doc diff
# view for me to approve." This is the publish that approve fires.
#
# Measured while building it, 2026-09-15, against the system of record:
# sixteen `maintenance-*` kinds carry a `failed` step in the tree that
# the live row lacks — landed on 2026-09-08, never published — while
# `ship-a-change`'s live v31 carries `procedure` texts and a required
# `proof` field that no revision of the tree ever said, so publishing
# the tree over it would REGRESS the protocol. A publish verb that
# cannot tell those two apart is a footgun with a button on it.
#
# THE RULE
# ---------------------------------------------------------------------
# The live row is safe to overwrite iff it is a row THE TREE ONCE SAID:
# some revision of the bundle under infra/platform/ renders to the same
# row, on EVERY key either side states (the comparator's header names
# the few left out, each with its reason). Then the tree moved ahead and
# live lags — publish. Otherwise the live row carries an edit the tree lacks:
# REFUSE, name the drift field by field, and publish only when the
# request says `--force-tree`. The walk covers the bundle DIRECTORY's
# history, not one file's, because the bundle was one file until
# 2026-09-08 and `git log --follow` cannot trace a split — the sixteen
# lagging rows match the OLD `workflows.toml`, not any revision of their
# own file (measured: design-doc matched its own file's first revision,
# maintenance-views-catchup matched 8cc7a03e:infra/platform/workflows.toml,
# pr-train and ship-a-change matched nothing).
#
# WHAT IT DOES NOT COMPARE: only what no file can say — the row's
# version, status, created_at, authoring_job_id, and the owning_team the
# loader stamps. Until 2026-10-08 this paragraph read "and why that is
# not a hole", and listed metadata_schema, entitlements, the workflow
# `metadata` and sign_offs_required as left out of the equality. It WAS
# a hole (backlog aa28f921; review 4746c046, F1): a field's `writer` and
# a step's `executor` and `assurance_required` were out too, unlisted,
# so a live row that lost the keys deciding who may write read "nothing
# to publish" forever, and "confirmed equal" after a publish proved
# nothing about them. The comparison is infra/gcp/workflow-row-compare.py
# now, and it has no list to widen.
#
# THE SEQUENCE, each step checked rather than assumed
# ---------------------------------------------------------------------
#   1. the kind's file exists in the tree               else exit 3
#   1'. its bytes are read ONCE into this run's own copy, and every
#      step below reads that copy, re-hashed before each use (review R1
#      of car 214ae147)              else exit 78; moved later, exit 75
#   1b. the host's CLI can read a bundle: `boss --version` names a
#      commit at or after the loader's arrival (the floor) else exit 78
#   2. the live active row is read                       else exit 75 / 8
#   3. tree vs live: equal is nothing to publish         else exit 5
#      differing: the history walk decides; a live row the tree never
#      said is refused unless --force-tree               else exit 6
#   4. `boss workflow publish <kind> <file> --dry-run` lints the row and
#      refuses a dirty registry (a stale draft)          else exit 4
#   4'. the server says whether the live row is passkey-protected;
#      a protected one is the owner's to sign, as a held kind is
#      (unread: 75)                                       else exit 9
#   5. --check stops here, exit 0, saying what it would do
#   6. `boss workflow publish <kind> <file>` — the CLI's own checked
#      sequence: create the draft, read it back, publish, confirm
#   7. the live row is read back and compared to the tree AGAIN; the
#      version must have moved and the view must be equal   else exit 7
#
# The binary is the CLI's — the TOML is read by `boss_jobs::seed_loader`,
# the same loader the seed uses, so the row this publishes and the row
# a fresh database seeds are one definition (§9a); a shell re-derivation
# of that loader would be a second copy of it. The binary on boss-gcp is
# the one boss-gcp-converge installs at /usr/local/bin/boss out of the
# cluster image (infra/estate/install-cli-from-image.sh); a missing one is
# a configuration refusal naming that, never an ENOENT dressed as a
# verdict.
#
# AND A STALE ONE IS REFUSED BY NAME (backlog fec2851f). The first live
# run (ops-request 25cb2f71, 2026-09-15 16:36Z) reached step 4 with a
# host CLI older than the loader: it rejected the kind file as "is not
# JSON" — the branch an older `load_spec` falls into for any path —
# and this script reported exit 4, "the tree's file does not lint
# clean", about a file that lints clean. Nothing refreshed that binary
# then (boss-gcp-converge installed units only; `prod` is a deliberate
# human run), so the misnamed refusal would have sent an operator to
# the tree every time. Since 6f58e9a1 the converge installs the CLI
# from the cluster image at the sha it converged to and records
# `cli_sha` / `cli_result` on its packet, so the refresh path the
# refusal names is a packet read. `boss --version` is read first: the commit it
# names must sit in this checkout's history at or after CLI_FLOOR —
# the train that taught the CLI to read a bundle — else the refusal
# names the binary, its commit, the floor and the refresh path, as a
# host-configuration fault (78, where "no boss CLI" lives). A binary
# that cannot say (`unknown`) or names a commit this checkout does not
# hold is refused the same way: unplaceable is not assumed current.
#
# USAGE
#   publish-workflow.sh <kind> [--check | --force-tree]
#
# ENV
#   BOSS_JOBS_URL                (required) the system of record
#   BOSS_PUBLISH_WORKFLOW_REPO   the checkout whose bundle and history
#                                are read (default: the one this script
#                                is in — /opt/boss on boss-gcp)
#   BOSS_BIN                     the CLI (default: `boss` on PATH, else
#                                /usr/local/bin/boss)
#   BOSS_PUBLISH_CLI_FLOOR       the oldest commit a usable CLI may be
#                                built from (default CLI_FLOOR_DEFAULT
#                                below; the test fixture's history has
#                                its own)
#   BOSS_ACTOR                   who the publish signs as (default
#                                automation:ops-runner — the account
#                                RUNNING it, the identity rule every
#                                `boss` verb applies; the approving
#                                human is the ops-request's filer)
#   OPS_REQUEST_ID               the packet, from the runner; printed —
#                                and, for a HELD kind in real mode, READ:
#                                its metadata.for_protocol_publish names
#                                the publish-a-protocol packet whose
#                                signature is the only thing that lifts
#                                the hold for one publish (see 1a)
#   BOSS_PUBLISH_HISTORY_LIMIT   commits walked at most (default 500)
#
# EXIT
#   0  published and confirmed (or, with --check, would publish)
#   2  usage: a kind outside ^[a-z][a-z0-9-]{1,60}$, or a foreign mode
#   3  REFUSED: the tree has no infra/platform/workflows/<kind>.toml
#   4  REFUSED: the tree's row does not lint / the registry is dirty
#   5  REFUSED: nothing to publish — live already equals the tree
#   6  REFUSED: the live row carries what the tree never said
#   7  the publish ran and was NOT confirmed by the read-back
#   8  the kind has no live active row (the seed admits new kinds)
#   9  REFUSED: the kind is HELD out of every unattended publish
#      (infra/platform/workflow-holds/, or a row that declares a writer
#      or an executor) — under --force-tree before anything is read; in
#      real mode unless the request names a publish-a-protocol packet
#      signed for exactly this publish (the refusal says which check
#      failed), and again when live moved off the version it signed;
#      under --check, where `--check ok` would have been. AND a kind the
#      SERVER judges passkey-protected (GET /api/workflows/<kind>/passkey,
#      backlog 5af64799), the same way where a publish would follow:
#      `--check HELD`, refused under --force-tree, and in real mode
#      unless the request names a packet that signs this publish
#  75  cannot answer: the registry could not be read, or this run's one
#      copy of the kind's file changed before a later step read it (the
#      bytes judged are not the bytes in hand; after the publish, 7)
#  78  configuration: no BOSS_JOBS_URL, no `boss`, a `boss` older than
#      the bundle loader (or of unknown provenance), no python3/tomllib,
#      no comparator beside this script, holds that cannot be read, a
#      copy of the bundle that cannot be taken, or a tenant manifest
#      above that copy (REFUSED in every mode)
#
# Runs as root under the ops-runner with NO HOME; every git read drops
# to the checkout's owner (root cannot even READ a checkout it does not
# own — "dubious ownership", measured on ops-request c9877f75).
set -uo pipefail

# shellcheck source=infra/lib/jq.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/jq.sh"

NAME="publish-workflow"
SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

say() { printf '%s: %s\n' "$NAME" "$*"; }
refuse() { # <exit> <message...>
    local rc="$1"; shift
    printf '%s: REFUSED — %s\n' "$NAME" "$*"
    exit "$rc"
}

usage() {
    sed -n '/^# USAGE/,/^# EXIT/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//' >&2
}

# --- arguments ------------------------------------------------------------
KIND="${1:-}"
MODE="${2:-}"
if [ -z "$KIND" ] || [ $# -gt 2 ]; then
    echo "$NAME: usage: $(basename "$0") <kind> [--check | --force-tree]" >&2
    usage; exit 2
fi
# The allowlist's pattern, applied again here: the runner is one reader
# of it and this script is another, and a script that trusts its caller
# is a script that is one caller away from `../`.
if ! grep -Eqx -- '^[a-z][a-z0-9-]{1,60}$' <<<"$KIND"; then
    echo "$NAME: usage: kind '$KIND' is not a workflow kind (^[a-z][a-z0-9-]{1,60}$)" >&2
    exit 2
fi
case "$MODE" in
    ""|--check|--force-tree) ;;
    *)
        echo "$NAME: usage: mode '$MODE' is not one of --check, --force-tree" >&2
        exit 2 ;;
esac

# --- configuration --------------------------------------------------------
if [ -z "${BOSS_JOBS_URL:-}" ]; then
    echo "$NAME: BOSS_JOBS_URL is not set, and there is no safe default — a publish against the wrong instance answers instead of erroring (CLAUDE.md §Doors). The ops-runner's unit pins it." >&2
    exit 78
fi
for tool in jq curl python3; do
    command -v "$tool" >/dev/null 2>&1 || { echo "$NAME: no $tool on PATH — nothing compared, nothing published" >&2; exit 78; }
done
python3 -c 'import tomllib' 2>/dev/null || { echo "$NAME: python3 has no tomllib (3.11+) — the bundle cannot be read, so nothing compared" >&2; exit 78; }

# The floor: train #268 (2026-09-08), where `load_spec` in
# crates/orchestrators/boss-cli/src/workflow.rs began reading a `.toml`
# path through boss_jobs::seed_loader. A CLI built from any commit
# before it cannot read what this verb publishes. One constant, pinned
# by crates/core/boss-testing/tests/publish_workflow_sh.rs to the
# commit whose workflow.rs first calls the loader.
CLI_FLOOR_DEFAULT="c17827f37b3a464c7e2377281ff3dd8206f6ee8a"
CLI_FLOOR="${BOSS_PUBLISH_CLI_FLOOR:-$CLI_FLOOR_DEFAULT}"

BOSS_BIN="${BOSS_BIN:-}"
if [ -z "$BOSS_BIN" ]; then
    if command -v boss >/dev/null 2>&1; then BOSS_BIN="$(command -v boss)"; else BOSS_BIN=/usr/local/bin/boss; fi
fi
if [ ! -x "$BOSS_BIN" ]; then
    echo "$NAME: no boss CLI at $BOSS_BIN — the publish IS the CLI's checked sequence (boss workflow publish), and this host has none installed. boss-gcp-converge installs it as /usr/local/bin/boss from the cluster image on each tick (cli_result on its packet says why it has not); until then this verb cannot act." >&2
    exit 78
fi

REPO="${BOSS_PUBLISH_WORKFLOW_REPO:-$SELF_DIR/../..}"
REPO="$(cd "$REPO" 2>/dev/null && pwd)" || { echo "$NAME: checkout ${BOSS_PUBLISH_WORKFLOW_REPO:-$SELF_DIR/../..} is not a directory this process can enter" >&2; exit 78; }
BUNDLE_REL="infra/platform/workflows"
KIND_FILE_REL="$BUNDLE_REL/$KIND.toml"
KIND_FILE="$REPO/$KIND_FILE_REL"
HISTORY_LIMIT="${BOSS_PUBLISH_HISTORY_LIMIT:-500}"

# Who the publish signs as: the account running it. The runner signs
# its own SoR calls as automation:ops-runner; the `boss` binary refuses
# an unnamed write, and there is no HOME here for the actor file.
export BOSS_ACTOR="${BOSS_ACTOR:-automation:ops-runner}"
# The same identity, as the header the jobs API reads: the held-kind
# door below READS the request and its packet, and an unauthenticated
# read is handed a smaller world (total: 0) rather than an error.
BOSS_USER="{\"id\":\"$BOSS_ACTOR\",\"role\":\"platform-admin\",\"access_tier\":\"operator\",\"territory_account_ids\":[],\"direct_report_ids\":[],\"department\":\"platform\"}"

TMP=$(mktemp -d) || exit 1
trap 'rm -rf "$TMP"' EXIT

# THE MACHINE TOKEN IS PRESENTED ON THE LIVE READ, NEVER REQUIRED
# (design 6805c764; backlog 2710c8fc). read_live's GET carries no
# identity — /api/workflows is a public registry surface — but the jobs
# port's machine gate records every request without the token, and this
# one arrived from boss-gcp once per kind on every publish-drift tick
# (38 would-refuse facts in the 72 hours read on 2026-10-06). Made once,
# here: after the trap above, which the reader's cleanup chains in front
# of, and in this script's own shell. No reader beside the checkout, no
# slot, a slot the reader refuses, a host off the estate's list or a
# header file that cannot be made: the read goes out as before.
PW_MT_HDR=""
# shellcheck source=infra/lib/secret-header.sh
if [ -r "$SELF_DIR/../lib/secret-header.sh" ]; then . "$SELF_DIR/../lib/secret-header.sh"; fi
if declare -F machine_token_header >/dev/null; then
    machine_token_header PW_MT_HDR "$BOSS_JOBS_URL" || PW_MT_HDR=""
fi

# --- 1. the tree authors the kind -----------------------------------------
if [ ! -f "$KIND_FILE" ]; then
    refuse 3 "the tree has no $KIND_FILE_REL (checkout $REPO), so there is nothing to publish for '$KIND'. A NEW kind is admitted by the seed (insert-if-missing) once its file lands; this verb republishes kinds the tree already authors."
fi

# --- 1'. the bytes are read ONCE ---------------------------------------------
# Review R1 of car 214ae147 (Codex run 753b6bd7, 2026-11-05). The
# signature check hashed the CHECKOUT's file, and the lint, the publish
# and the read-back each read that mutable path again — so a file that
# moved during the lint (a converge pulling the next edit) was published
# under the old signature: signed 7708bbb7, published 4da8ebc8, and read
# back "equal" to the new bytes. So the bytes are taken once, into this
# run's own copy, hashed once, and EVERY reader below takes that copy:
# the hold reader, the signature, the compare, the lint, the publish and
# the read-back. The copy is re-hashed before each use; a copy that
# changed is not the bytes that were judged, and the run stops.
#
# The hold reader reads the bundle and the holds directory, so both are
# copied (-H follows only a link named here, -R keeps the links inside);
# the kind's own file is then read through any link, once, by cat.
HOLDS_REL="infra/platform/workflow-holds"
SNAP="$(cd "$TMP" && pwd -P)/tree"
SPEC="$SNAP/$KIND_FILE_REL"
if ! mkdir -p "$SNAP/infra/platform" \
    || ! cp -RH "$REPO/$BUNDLE_REL" "$SNAP/$BUNDLE_REL" 2>"$TMP/snap.err" \
    || [ -L "$SNAP/$BUNDLE_REL" ] || [ ! -d "$SNAP/$BUNDLE_REL" ] \
    || { { [ -e "$REPO/$HOLDS_REL" ] || [ -L "$REPO/$HOLDS_REL" ]; } \
         && ! cp -RH "$REPO/$HOLDS_REL" "$SNAP/$HOLDS_REL" 2>>"$TMP/snap.err"; } \
    || ! rm -f "$SPEC" || ! cat "$KIND_FILE" > "$SPEC" 2>>"$TMP/snap.err"; then
    refuse 78 "this run could not take its one copy of $BUNDLE_REL and $HOLDS_REL out of $REPO ($(head -c 300 "$TMP/snap.err" | tr '\n' ' ')), and bytes read more than once are not the bytes that were judged — nothing compared, nothing published"
fi
SPEC_SHA="$(sha256sum "$SPEC" | cut -c1-64)"
spec_unchanged() { # <before what> <exit>: refuses <exit> when the copy moved
    local now
    now="$(sha256sum "$SPEC" 2>/dev/null | cut -c1-64)"
    [ "$now" = "$SPEC_SHA" ] && return 0
    refuse "$2" "this run's copy of $KIND_FILE_REL was read once at sha256 ${SPEC_SHA:0:12} and hashes to ${now:-nothing} before $1: the bytes judged are not the bytes in hand — nothing published past this point (packet ${OPS_REQUEST_ID:-none})"
}
# The copy sits outside the checkout, so the CLI's tenant lookup (the
# nearest ancestor holding one of these — boss-cli tenant.rs
# manifest_of, held equal to this list by publish_workflow_sh.rs) walks
# the TEMP directory's ancestry instead of the checkout's. A manifest
# there (a shared /tmp) would publish a platform row as that tenant's,
# and the compare leaves the loader-stamped owning_team out. Refused.
TENANT_MANIFESTS="tenant.toml seeds/tenant.toml"
up="$(dirname "$SPEC")"
while :; do
    for manifest in $TENANT_MANIFESTS; do
        if [ -f "$up/$manifest" ]; then
            refuse 78 "$up/$manifest sits above this run's copy of $KIND_FILE_REL, so the CLI would read the platform row as that tenant's — move it or point TMPDIR elsewhere; nothing compared, nothing published"
        fi
    done
    [ "$up" = / ] && break
    up="$(dirname "$up")"
done
say "spec: $KIND_FILE_REL read once into this run's copy, sha256 $SPEC_SHA — the holds, the signature, the compare, the lint, the publish and the read-back all read that copy"

# --- 1a. a HELD kind is not published by any machine road -------------------
# Backlog 083d240e; review R1 of the signer car (run 7cee49b9,
# 2026-10-06). A row that turns on a REFUSAL goes live at a deliberate
# registry publish (design b08725c2). Two machine roads publish a tree
# row with no approval, and BOTH pass through this script: publish-drift
# calls it per kind, and the `publish-workflow` verb is it — a verb that
# declares no approval, so any actor who may file an ops-request could
# have published a held row as automation:ops-runner, and published it
# again after a rollback (a rolled-back live row is "a row the tree once
# said"). So the hold sits HERE, where both roads pass.
#
# infra/gcp/workflow-holds.py is the one reader (its header is the
# contract: infra/platform/workflow-holds/<kind>.toml, and by default
# every row that declares a field writer or a step executor). This
# script derives nothing of its own.
#   - holds that cannot be read: REFUSED, exit 78, in EVERY mode — an
#     unreadable hold is not an absent one;
#   - a held kind, real mode or --force-tree: REFUSED, exit 9, before
#     the registry is read — --force-tree forces the tree over a live
#     edit, never over a hold;
#   - a held kind, --check: says HELD here, goes on to say what the
#     registry holds, and where it would have answered `--check ok` it
#     answers `--check HELD`, exit 9.
# THE DELIBERATE DOOR IS A PACKET (design b1f319bc Q held-kind, David
# 2026-11-05: "the packet IS the deliberate door; the terminal command
# stays as the fallback when the API is dark"). In real mode a held
# kind is published when, and only when, the ops-request this run
# answers (OPS_REQUEST_ID, handed by the runner) names a
# `publish-a-protocol` packet through `metadata.for_protocol_publish` —
# the edge the dispatcher writes when that packet's `publish` step
# becomes ready — and the packet, READ BACK HERE from the record, is a
# signature of exactly this publish:
#   - the packet is an open publish-a-protocol for a `workflow` row
#     (`registry`, absent on a v1 packet, reads as workflow) whose
#     `row_name` (or v1's `protocol_kind`) is this kind, and its
#     `publish` step is not done (single use);
#   - `approve` is completed `approved`, declares presence and a
#     required role, carries no live stamp by anyone this verb file's
#     `approvers` does not name, and the NEWEST stamp per required role
#     is presence-assured, carries a nonce, is by a named approver and
#     is bound to approve's shape AS IT STANDS NOW (infra/lib/
#     step-shape.sh, the ops-runner's one copy) — so every key below is
#     one the passkey signed;
#   - the signed `spec_path` is this kind's file and the signed
#     `spec_sha256` is the sha256 of the bytes about to be published;
#   - the signed `live_version` is the live version read in step 2: a
#     diff signed against vN is no approval to publish over vM.
# Nothing here is a flag: the request is read, never trusted, and a
# hand-filed request naming a signed packet can publish only the bytes
# that packet signed, once. Any check that fails refuses 9, naming it.
# `boss workflow publish <kind> <file>`, run by a person from a
# checkout, is the fallback when the API is dark; a hold does not bind
# it (it is also the rollback) and it does not lift one.
#
# signed_publish: sets SIGNED_WHY empty when the request's packet signs
# this publish (and SIGNED_PACKET / _BY / _AT / _SHA / _LIVE), else to
# why not, in words. It reads; it writes nothing.
SIGNED_WHY=""; SIGNED_PACKET=""; SIGNED_BY=""; SIGNED_AT=""; SIGNED_SHA=""; SIGNED_LIVE=""
UUID_RE='^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
read_job() { # <id> <out>: 0 with the job written, else SIGNED_WHY set
    local code
    code=$(curl -q -sS -m 20 -o "$2" -w '%{http_code}' -H "x-boss-user: $BOSS_USER" \
        ${PW_MT_HDR:+-H "$PW_MT_HDR"} "$BOSS_JOBS_URL/api/jobs/$1" 2>"$TMP/job-curl.err")
    # A 200 with no body is no document (jq-1.6 passes silence; d96e38ab).
    if [ "$code" != "200" ] || ! jq_doc_file "$2" || ! jq -e 'type == "object"' "$2" >/dev/null 2>&1; then
        SIGNED_WHY="packet ${1:0:8} could not be read from $BOSS_JOBS_URL (HTTP ${code:-000}: $(head -c 200 "$TMP/job-curl.err" | tr '\n' ' ')), and a signature that cannot be read is not one"
        return 1
    fi
    jq '(if has("data") and (.data | type) == "object" then .data else . end)' "$2" > "$2.job" && mv "$2.job" "$2"
}
signed_publish() {
    local req="$TMP/request.json" pkt="$TMP/packet.json" approve verb_file approvers shape verdict
    if ! [[ ${OPS_REQUEST_ID:-} =~ $UUID_RE ]]; then
        SIGNED_WHY="OPS_REQUEST_ID ('${OPS_REQUEST_ID:0:40}') names no ops-request, so no packet can be followed from it"
        return 0
    fi
    read_job "$OPS_REQUEST_ID" "$req" || return 0
    SIGNED_PACKET=$(jq -r '.metadata.for_protocol_publish // empty | strings' "$req")
    if ! [[ $SIGNED_PACKET =~ $UUID_RE ]]; then
        SIGNED_WHY="request ${OPS_REQUEST_ID:0:8} names no publish-a-protocol packet (metadata.for_protocol_publish is '${SIGNED_PACKET:0:40}')"
        return 0
    fi
    read_job "$SIGNED_PACKET" "$pkt" || return 0
    approve=$(jq -c '((.steps // []) | map(select(.spec_slug == "approve")) | .[0]) // empty' "$pkt")
    if [ -z "$approve" ]; then
        SIGNED_WHY="packet ${SIGNED_PACKET:0:8} has no approve step, so nothing on it signed a publish"
        return 0
    fi
    # shellcheck source=infra/lib/step-shape.sh
    . "$SELF_DIR/../lib/step-shape.sh" || { SIGNED_WHY="infra/lib/step-shape.sh is missing, so no stamp can be checked against its shape"; return 0; }
    shape=$(shape_hash_of "$approve") || { SIGNED_WHY="the shape of ${SIGNED_PACKET:0:8}'s approve step could not be computed"; return 0; }
    # WHO MAY SIGN IS A NAMED LIST, NEVER A ROLE (design 03451237 q2):
    # this verb's own file, reviewed with it, and no env knob.
    verb_file="$SELF_DIR/../ops/verbs/publish-workflow.json"
    approvers=$(jq -c '.approvers // empty
        | select(type == "array" and length > 0 and all(.[]; type == "string" and . != ""))' \
        "$verb_file" 2>/dev/null) || approvers=""
    if [ -z "$approvers" ]; then
        SIGNED_WHY="$verb_file names no approvers (a non-empty list of employee ids), so no passkey can sign a held publish"
        return 0
    fi
    # The run's one copy, hashed once at 1' — never the checkout again.
    SIGNED_SHA="$SPEC_SHA"
    # The judgement is ONE jq program, shared with publish-pool.sh
    # (backlog f399458f): what a signature is lives once.
    verdict=$(jq -c -f "$SELF_DIR/../lib/signed-publish.jq" --arg shape "$shape" \
        --argjson approvers "$approvers" --arg verb_file "infra/ops/verbs/publish-workflow.json" \
        --arg registry workflow --arg name "$KIND" --arg path "$KIND_FILE_REL" \
        --arg sha "$SIGNED_SHA" --arg step publish "$pkt" 2>"$TMP/signed.err") \
        || { SIGNED_WHY="packet ${SIGNED_PACKET:0:8} could not be judged — jq failed over it ($(head -c 200 "$TMP/signed.err" | tr '\n' ' '))"; return 0; }
    SIGNED_WHY=$(printf '%s' "$verdict" | jq -r '.refuse // empty')
    if [ -n "$SIGNED_WHY" ]; then
        SIGNED_WHY="publish-a-protocol ${SIGNED_PACKET:0:8} is not a signature of this publish: $SIGNED_WHY"
        return 0
    fi
    SIGNED_BY=$(printf '%s' "$verdict" | jq -r '.by')
    SIGNED_AT=$(printf '%s' "$verdict" | jq -r '.at')
    SIGNED_LIVE=$(printf '%s' "$verdict" | jq -r '.live')
}
HOLDS_PY="$SELF_DIR/workflow-holds.py"
HELD_SOURCE=""; HELD_WHY=""; HELD_LIFTS=""
if [ ! -f "$HOLDS_PY" ]; then
    refuse 78 "the hold reader $HOLDS_PY is missing, so whether '$KIND' is held out of every unattended publish cannot be read, and a hold that cannot be read is not a hold that is absent — nothing compared, nothing published"
fi
# Read over the run's copy (1'), so whether the kind declares a writer
# or an executor is judged on the bytes that will be published.
if ! python3 "$HOLDS_PY" "$SNAP" > "$TMP/holds.tsv" 2>"$TMP/holds.err"; then
    say "the tree's holds cannot be read ($HOLDS_REL, and each row's writers, executors and bills):"
    awk -F'\t' '$1 == "problem" { print "  " $2 }' "$TMP/holds.tsv"
    sed 's/^/  /' "$TMP/holds.err"
    refuse 78 "whether '$KIND' is held out of every unattended publish cannot be read, and a hold that cannot be read is not a hold that is absent — nothing compared, nothing published, in any mode. Fix the file named above in a car."
fi
HELD_SOURCE="$(awk -F'\t' -v k="$KIND" '$1 == "held" && $2 == k { print $3; exit }' "$TMP/holds.tsv")"
if [ -n "$HELD_SOURCE" ]; then
    HELD_WHY="$(awk -F'\t' -v k="$KIND" '$1 == "held" && $2 == k { print $4; exit }' "$TMP/holds.tsv")"
    HELD_LIFTS="$(awk -F'\t' -v k="$KIND" '$1 == "held" && $2 == k { print $5; exit }' "$TMP/holds.tsv")"
    HELD_SAYS="'$KIND' is HELD out of every unattended publish ($HELD_SOURCE): $HELD_WHY — lifts: $HELD_LIFTS. The deliberate door is a publish-a-protocol packet its owner signs with a passkey (design b1f319bc); when the API is dark, \`boss workflow publish $KIND $KIND_FILE_REL\`, run by a person from a checkout, which the hold does not bind and does not lift"
    if [ "$MODE" = "--force-tree" ]; then
        refuse 9 "$HELD_SAYS. This verb does not publish a held kind, and --force-tree forces the tree over a live edit, never over a hold — nothing compared, nothing published (packet ${OPS_REQUEST_ID:-none})."
    fi
    if [ "$MODE" = "" ]; then
        signed_publish
        if [ -n "$SIGNED_WHY" ]; then
            refuse 9 "$HELD_SAYS. This verb publishes a held kind only for the request of a publish-a-protocol packet its owner signed for exactly this publish, and $SIGNED_WHY — nothing compared, nothing published (packet ${OPS_REQUEST_ID:-none})."
        fi
        say "HELD ($HELD_SOURCE), and lifted for this one publish by publish-a-protocol $SIGNED_PACKET: signed with a passkey by $SIGNED_BY at $SIGNED_AT over $KIND_FILE_REL sha256 ${SIGNED_SHA:0:12} against live v$SIGNED_LIVE (the hold itself stands)"
    else
        say "HELD: $HELD_SAYS. --check goes on to say what the registry holds; a publish through this verb would be REFUSED (exit 9) unless its request names a signed publish-a-protocol packet."
    fi
fi

# --- 1a'. the passkey travels to the SERVER, which judges it -----------------
# Backlog a4b57aca. POST /api/workflows/{kind}/publish now refuses a
# publish over a passkey-protected LIVE row (a presence step on it, or a
# protected child it admits or fulfils) unless the request names the
# publish-a-protocol packet that signed it, with the signed bytes; the
# server reads the packet back and judges it with the Rust port of
# infra/lib/signed-publish.jq (pinned equal to it). So the signature is
# no longer only a hold's: whenever this run's request names a packet
# that signs this publish, the CLI hands it on. The check above stays as
# the early refusal of a held kind; the server's is the boundary.
PUBLISH_SIGNED_ARGS=()
if [ "$MODE" = "" ] && [ -z "$HELD_SOURCE" ] && [[ ${OPS_REQUEST_ID:-} =~ $UUID_RE ]]; then
    signed_publish
fi
if [ "$MODE" = "" ] && [ -n "$SIGNED_PACKET" ] && [ -z "$SIGNED_WHY" ]; then
    PUBLISH_SIGNED_ARGS=(--approval "$SIGNED_PACKET" --signed-path "$KIND_FILE_REL")
    say "the publish carries publish-a-protocol $SIGNED_PACKET to the server, signed by $SIGNED_BY over $KIND_FILE_REL sha256 ${SIGNED_SHA:0:12}"
fi

# --- git, as the checkout's owner -------------------------------------------
OWNER="$(stat -c %U "$REPO" 2>/dev/null)"
OWNER_UID="$(stat -c %u "$REPO" 2>/dev/null)"
if [ -z "$OWNER" ] || [ "$OWNER" = "UNKNOWN" ]; then
    refuse 78 "cannot resolve the owner of $REPO (stat says '${OWNER:-}', uid ${OWNER_UID:-?}) — no passwd entry, so there is no account to read git as"
fi
OWNER_HOME="$(getent passwd "$OWNER" | cut -d: -f6)"
as_owner() { # <command string>
    if [ "$(id -un)" = "$OWNER" ]; then
        bash -c "$1"
    else
        runuser -u "$OWNER" -- env HOME="${OWNER_HOME:-/}" PATH="$PATH" bash -c "$1"
    fi
}

# --- 1b. the host's CLI can read what it will publish ---------------------------
# `boss --version` prints `boss <crate> built from <sha>` (built_from.rs).
# Read before the registry: a host that cannot act is a host fault, and
# an operator should not be sent to the SoR or the tree for it.
# Captured whole, first line taken in the shell: an external producer
# piped into an early-exiting reader is the SIGPIPE coin-flip the lint
# no-lint-flips-a-coin-on-an-external-producer refuses (2026-09-15).
BOSS_VERSION_LINE="$("$BOSS_BIN" --version 2>&1)"
BOSS_VERSION_LINE="${BOSS_VERSION_LINE%%$'\n'*}"
BUILT_FROM="$(printf '%s\n' "$BOSS_VERSION_LINE" | sed -n 's/.*built from \([0-9a-f]\{7,40\}\|unknown\).*/\1/p')"
REFRESH="the binary is refreshed by boss-gcp-converge from the cluster image at the sha it converges to (every 30 min; cli_sha and cli_result on the maintenance-boss-gcp-converge packet say what it installed and why not)"
case "${BUILT_FROM:-}" in
    "")
        refuse 78 "$BOSS_BIN cannot say what it was built from ('$BOSS_VERSION_LINE' names no commit), so whether it reads a bundle is unknown and is not assumed; $REFRESH" ;;
    unknown)
        refuse 78 "$BOSS_BIN cannot say what it was built from (built from unknown — compiled without git and no BOSS_BUILD_COMMIT), so whether it reads a bundle is unknown and is not assumed; $REFRESH" ;;
esac
if ! as_owner "git -C '$REPO' cat-file -e '$BUILT_FROM^{commit}'" 2>/dev/null; then
    refuse 78 "$BOSS_BIN is built from $BUILT_FROM, a commit not in the history of $REPO, so it cannot be placed before or after the bundle loader ($CLI_FLOOR); an unplaceable binary is not assumed current. $REFRESH"
fi
if ! as_owner "git -C '$REPO' merge-base --is-ancestor '$CLI_FLOOR' '$BUILT_FROM'" 2>/dev/null; then
    refuse 78 "$BOSS_BIN is built from ${BUILT_FROM:0:8}, older than the bundle loader (floor ${CLI_FLOOR:0:8}, train #268 2026-09-08): it would reject $KIND_FILE_REL as not JSON, a fault of the host and not of the tree. $REFRESH"
fi
say "boss: $BOSS_BIN built from ${BUILT_FROM:0:8} (floor ${CLI_FLOOR:0:8} reached)"

# --- the comparator -----------------------------------------------------------
# infra/gcp/workflow-row-compare.py, beside this script: `python3 <it>
# <toml> <live.json> <kind>` exits 0 and prints EQUAL when the kind's row
# in the TOML renders to the live row, else exits 1 and prints one
# `DIFF <path> <tree> <live>` line per differing key, the path naming
# the step by title and the field by name. Exit 3 when the TOML does not
# parse or lacks the kind (a historical revision may legitimately lack
# it). It compares EVERY key either side states — its header says why
# there is no list, and names the few keys left out and the reason for
# each. It was a heredoc here until 2026-10-08 (backlog aa28f921); it is
# a file so a test and an operator can run it offline over a saved row,
# which is how the review that found its blind spot had to measure it —
# by lifting it out of this script by hand.
COMPARE_PY="$SELF_DIR/workflow-row-compare.py"
if [ ! -f "$COMPARE_PY" ]; then
    refuse 78 "the comparator $COMPARE_PY is missing, so whether the live '$KIND' says what the tree says cannot be judged, and an unjudged row is neither equal nor adrift — nothing compared, nothing published"
fi

# --- 2. the live active row ---------------------------------------------------
# Unauthenticated, like the drift lint's read: /api/workflows is a public
# registry surface and answered 86 rows to a bare GET on 2026-09-15.
read_live() { # <out file>; exit 0 with the active row written, 75 otherwise
    local body="$TMP/live.raw" code
    code=$(curl -q -sS -m 20 -o "$body" -w '%{http_code}' ${PW_MT_HDR:+-H "$PW_MT_HDR"} "$BOSS_JOBS_URL/api/workflows/$KIND" 2>"$TMP/curl.err")
    if [ "$code" != "200" ]; then
        printf '%s: could not read %s/api/workflows/%s — HTTP %s. %s\n' "$NAME" "$BOSS_JOBS_URL" "$KIND" "${code:-000}" "$(tr '\n' ' ' <"$TMP/curl.err")" >&2
        say "cannot answer: nothing compared, nothing published (a verdict from an unread registry would be the confident wrong answer)"
        return 75
    fi
    # The endpoint answers the active row as an object (or an envelope,
    # or every version as an array — the CLI tolerates all three, so do
    # we): keep the active one.
    # A 200 carrying NO BODY is the case `jq -e` calls a pass, and it
    # would write an empty $1 for the version read below to find
    # (d96e38ab). Asked separately so each refusal keeps its own words —
    # and because the shared one quotes a jq.err that never gets written.
    if ! jq_doc_file "$body"; then
        printf '%s: %s/api/workflows/%s answered %s with no readable body, so nothing was compared\n' "$NAME" "$BOSS_JOBS_URL" "$KIND" "$code" >&2
        return 8
    fi
    if ! jq -e '
        (if type == "object" and has("data") then .data else . end)
        | (if type == "array" then (map(select(.status == "active")) | last) else . end)
        | select(type == "object" and has("version"))' "$body" > "$1" 2>"$TMP/jq.err"; then
        printf '%s: %s/api/workflows/%s answered 200 but no active row with a version could be read from it: %s\n' "$NAME" "$BOSS_JOBS_URL" "$KIND" "$(tr '\n' ' ' <"$TMP/jq.err")" >&2
        return 8
    fi
}
read_live "$TMP/live.json"; rc=$?
case "$rc" in
    0) ;;
    8) refuse 8 "'$KIND' has no live active row at $BOSS_JOBS_URL — the seed admits a new kind (insert-if-missing); this verb republishes one that exists." ;;
    *) exit "$rc" ;;
esac
LIVE_VERSION=$(jq -r '.version' "$TMP/live.json")
# A held kind's signature names the live version its diff was rendered
# against; live having moved since is no approval to publish over it.
if [ -n "$HELD_SOURCE" ] && [ "$MODE" = "" ] && [ "$SIGNED_LIVE" != "$LIVE_VERSION" ]; then
    refuse 9 "$HELD_SAYS. publish-a-protocol ${SIGNED_PACKET:0:8} signed a diff against live v$SIGNED_LIVE, and live is v$LIVE_VERSION now: what was signed is not what this publish would overwrite — nothing published (packet ${OPS_REQUEST_ID:-none}). The next landed edit files a fresh packet; until then read the live row."
fi

# --- 3. tree vs live ------------------------------------------------------------
spec_unchanged "the compare" 75
python3 "$COMPARE_PY" "$SPEC" "$TMP/live.json" "$KIND" > "$TMP/diff.txt"; cmp_rc=$?
case "$cmp_rc" in
    0)
        if [ "$MODE" != "--force-tree" ]; then
            # Reconciliation consumes the exact snapshot compared, not a
            # later hash of a checkout that may have changed underneath it.
            spec_unchanged "the equality witness" 75
            say "EQUAL: $KIND v$LIVE_VERSION; spec sha256 $SPEC_SHA"
            refuse 5 "nothing to publish — the live $KIND v$LIVE_VERSION already says what $KIND_FILE_REL says (every key either side states compared equal, each step and each field's writer among them). A publish now would mint an identical version; --force-tree does that anyway."
        fi
        say "FORCED: live $KIND v$LIVE_VERSION equals the tree on every compared field; publishing an identical row because the request said --force-tree"
        ;;
    1)
        say "live $KIND v$LIVE_VERSION differs from $KIND_FILE_REL:"
        sed 's/^DIFF\t/  /; s/\t/  /g' "$TMP/diff.txt"
        # THE HISTORY WALK. Every commit that touched infra/platform,
        # newest first; at each, every TOML there that names the kind is
        # rendered and compared to the live row. The first match ends
        # it: live is a row the tree once said.
        matched=""
        walked=0
        if ! as_owner "git -C '$REPO' rev-parse --git-dir" >/dev/null 2>"$TMP/git.err"; then
            say "cannot read $REPO as a git checkout (as '$OWNER'): $(tr '\n' ' ' <"$TMP/git.err")"
            say "so whether the tree ever said the live row cannot be judged; treating it as never said"
        else
            while IFS= read -r sha; do
                [ -n "$sha" ] || continue
                walked=$((walked + 1))
                while IFS= read -r path; do
                    [ -n "$path" ] || continue
                    case "$path" in *.toml) ;; *) continue ;; esac
                    as_owner "git -C '$REPO' show '$sha:$path'" > "$TMP/rev.toml" 2>/dev/null || continue
                    if python3 "$COMPARE_PY" "$TMP/rev.toml" "$TMP/live.json" "$KIND" >/dev/null 2>&1; then
                        matched="${sha:0:8}:$path"
                        break 2
                    fi
                done <<PATHS
$(as_owner "git -C '$REPO' grep -l -F 'kind = \"$KIND\"' '$sha' -- infra/platform" 2>/dev/null | sed 's/^[0-9a-f]*://')
PATHS
            done <<SHAS
$(as_owner "git -C '$REPO' log --format=%H -n '$HISTORY_LIMIT' -- infra/platform" 2>/dev/null)
SHAS
        fi
        if [ -n "$matched" ]; then
            say "the tree moved ahead: live $KIND v$LIVE_VERSION is the row at $matched (found after $walked commit(s)) — live lags the tree, and the publish brings it forward"
        elif [ "$MODE" = "--force-tree" ]; then
            say "FORCED: live $KIND v$LIVE_VERSION matches no revision of infra/platform in $walked commit(s) walked — an edit the tree never said — and the request said --force-tree, so the tree's row is published over the drift named above"
        else
            refuse 6 "live $KIND v$LIVE_VERSION carries what the tree never said: no revision of infra/platform in $walked commit(s) walked renders to it, so the drift named above is an edit made live (a publish from a JSON spec, an authoring surface) that $KIND_FILE_REL lacks. Publishing the tree would erase it. Either write the live edit into the file and land that car, or, if the tree is what should win, file this verb again with --force-tree (the Drift tab's approve)."
        fi
        ;;
    *)
        say "the tree's $KIND_FILE_REL could not be read as a [[workflow]] file for '$KIND':"
        sed 's/^/  /' "$TMP/diff.txt"
        refuse 4 "the row does not parse, so nothing was compared or published"
        ;;
esac

# --- 4. lint, and refuse a dirty registry, without writing -----------------------
spec_unchanged "the lint" 75
if ! "$BOSS_BIN" workflow publish "$KIND" "$SPEC" --dry-run > "$TMP/lint.out" 2>&1; then
    say "boss workflow publish --dry-run refused the tree's row; its output, whole:"
    sed 's/^/  /' "$TMP/lint.out"
    refuse 4 "the tree's $KIND_FILE_REL does not lint clean (or the registry holds a stale draft — see above), so nothing was written"
fi
say "$(grep -m1 'lints clean' "$TMP/lint.out" || tail -n1 "$TMP/lint.out")"

# --- 4'. a passkey-protected live row is the owner's, as a held kind is ------------
# Backlog 5af64799. Measured 2026-11-18 01:29Z: David approved
# publish-to-github from the Drift tab; publish-drift --for-real
# (ops-request 489680c9) ran this verb for it, the CLI created draft v18,
# and the server refused the unsigned publish 409 — publish-to-github's
# approve step declares presence (rule a4b57aca), though no hold names
# it — stranding v18. A kind the server will refuse unsigned is the
# owner's to sign exactly as a held kind is, so it answers 9 here like
# one: --check says `--check HELD`, which publish-drift --file-held
# files as a publish-a-protocol packet and --for-real skips by name;
# real mode publishes it only with a signature that request carries.
#
# THE SERVER JUDGES, THIS VERB ASKS (CLAUDE.md §9a). The rule —
# presence on a step of the live row, or an admission/fulfilment edge
# into a protected kind, closed transitively, a retired row standing in
# for a kind with none active — lives once, in boss_jobs::publish_passkey,
# and GET /api/workflows/{kind}/passkey answers it. Asked signed, as the
# held-kind door reads a packet: the edges come from the dispatcher's
# rules, which it withholds from a caller that cannot read every packet.
# Asked only where this verb would publish, and only for a kind not held
# (a held kind is already on the passkey road). An answer that cannot be
# read is not "unprotected": 75, nothing written.
PASSKEY_WHY=""
if [ -z "$HELD_SOURCE" ]; then
    code=$(curl -q -sS -m 20 -o "$TMP/passkey.json" -w '%{http_code}' -H "x-boss-user: $BOSS_USER" \
        ${PW_MT_HDR:+-H "$PW_MT_HDR"} "$BOSS_JOBS_URL/api/workflows/$KIND/passkey" 2>"$TMP/passkey.err")
    if [ "$code" != "200" ] || ! jq_doc_file "$TMP/passkey.json" \
        || ! jq -e '(.protected | type) == "boolean"' "$TMP/passkey.json" >/dev/null 2>&1; then
        say "cannot answer: whether live $KIND v$LIVE_VERSION is passkey-protected could not be read from $BOSS_JOBS_URL/api/workflows/$KIND/passkey (HTTP ${code:-000}: $(head -c 300 "$TMP/passkey.json" 2>/dev/null | tr '\n' ' ')$(head -c 200 "$TMP/passkey.err" | tr '\n' ' ')), and an unread judgement is not an unprotected row — nothing written (packet ${OPS_REQUEST_ID:-none})"
        exit 75
    fi
    judged_v=$(jq -r '.live_version // empty' "$TMP/passkey.json")
    if [ "$(jq -r '.protected' "$TMP/passkey.json")" = "true" ]; then
        if [ "$judged_v" != "$LIVE_VERSION" ]; then
            say "cannot answer: the server judged $KIND's passkey on live v${judged_v:-none} and this run read live v$LIVE_VERSION — live moved between the two reads; nothing written (packet ${OPS_REQUEST_ID:-none})"
            exit 75
        fi
        PASSKEY_WHY=$(jq -r '.why // "the server names no reason"' "$TMP/passkey.json")
        say "PASSKEY-PROTECTED (judged by the server, GET /api/workflows/$KIND/passkey): $PASSKEY_WHY"
    fi
fi
if [ -n "$PASSKEY_WHY" ] && [ "$MODE" = "--force-tree" ]; then
    refuse 9 "live $KIND v$LIVE_VERSION is passkey-protected, and --force-tree forces the tree over a live edit, never over a passkey: the server refuses this publish unsigned, so nothing was written. The owner's road is a publish-a-protocol packet signed with a passkey (publish-drift --file-held files it) (packet ${OPS_REQUEST_ID:-none})."
fi
if [ -n "$PASSKEY_WHY" ] && [ "$MODE" = "" ]; then
    if [ -z "$SIGNED_PACKET" ] || [ -n "$SIGNED_WHY" ]; then
        unsigned_why="$SIGNED_WHY"
        [ -n "$unsigned_why" ] || unsigned_why="OPS_REQUEST_ID (${OPS_REQUEST_ID:0:40}) names no ops-request, so no packet can be followed from it"
        refuse 9 "live $KIND v$LIVE_VERSION is passkey-protected, so the server publishes it only with a publish-a-protocol packet its owner signed for exactly this publish, and $unsigned_why — nothing written, no draft created (packet ${OPS_REQUEST_ID:-none}). publish-drift --file-held files that packet."
    fi
    if [ "$SIGNED_LIVE" != "$LIVE_VERSION" ]; then
        refuse 9 "live $KIND v$LIVE_VERSION is passkey-protected, and publish-a-protocol ${SIGNED_PACKET:0:8} signed a diff against live v$SIGNED_LIVE: what was signed is not what this publish would overwrite — nothing written (packet ${OPS_REQUEST_ID:-none})."
    fi
fi

# --- 5. --check stops here ------------------------------------------------------------
if [ "$MODE" = "--check" ] && [ -n "$PASSKEY_WHY" ]; then
    # The held answer's own shape, so publish-drift files it on the same
    # line: the version phrase and the spec sha256 the owner signs.
    spec_unchanged "the answer" 75
    refuse 9 "--check HELD: would NOT publish $KIND_FILE_REL over live v$LIVE_VERSION at $BOSS_JOBS_URL — the tree is ahead of live and the kind is passkey-protected; nothing written; packet ${OPS_REQUEST_ID:-none}; spec sha256 $SPEC_SHA"
fi
if [ "$MODE" = "--check" ] && [ -n "$HELD_SOURCE" ]; then
    # Not `--check ok`: the verb file's effect pattern, and every reader
    # of this line, must not take a held kind for one this verb would
    # publish. The version phrase is the one publish-drift.sh reads.
    # `spec sha256` is the hash of the copy this answer's diff and lint
    # read: publish-drift --file-held signs THAT, never a fresh hash of a
    # checkout that may have moved since.
    spec_unchanged "the answer" 75
    refuse 9 "--check HELD: would NOT publish $KIND_FILE_REL over live v$LIVE_VERSION at $BOSS_JOBS_URL — the tree is ahead of live and the kind is held ($HELD_SOURCE); nothing written; packet ${OPS_REQUEST_ID:-none}; spec sha256 $SPEC_SHA"
fi
if [ "$MODE" = "--check" ]; then
    say "--check ok: would publish $KIND_FILE_REL over live v$LIVE_VERSION at $BOSS_JOBS_URL (nothing written; packet ${OPS_REQUEST_ID:-none})"
    exit 0
fi

# --- 6. the publish — the CLI's own checked sequence --------------------------------
spec_unchanged "the publish" 75
say "publishing $KIND_FILE_REL (sha256 ${SPEC_SHA:0:12}, this run's copy) as $BOSS_ACTOR for packet ${OPS_REQUEST_ID:-none} (boss: $BOSS_BIN)"
if ! "$BOSS_BIN" workflow publish "$KIND" "$SPEC" "${PUBLISH_SIGNED_ARGS[@]}" > "$TMP/publish.out" 2>&1; then
    say "boss workflow publish failed; its output, whole:"
    sed 's/^/  /' "$TMP/publish.out"
    say "NOT CONFIRMED — the CLI did not report $KIND live; read GET $BOSS_JOBS_URL/api/workflows/$KIND/versions before retrying (a failed create can leave a draft armed — the v16 regression)"
    exit 7
fi
sed 's/^/  /' "$TMP/publish.out"

# --- 7. observed, never assumed ---------------------------------------------------------
read_live "$TMP/after.json" || { say "NOT CONFIRMED — the publish ran but the registry could not be read back (live was v$LIVE_VERSION)"; exit 7; }
NEW_VERSION=$(jq -r '.version' "$TMP/after.json")
case "${NEW_VERSION:-empty}" in empty|*[!0-9]*) say "NOT CONFIRMED — the row read back carries no numeric version (live was v$LIVE_VERSION)"; exit 7 ;; esac
if [ "$NEW_VERSION" -le "$LIVE_VERSION" ]; then
    say "NOT CONFIRMED — the active row is still v$NEW_VERSION after a publish over v$LIVE_VERSION; the CLI's report above is not borne out by the registry"
    exit 7
fi
# The publish already ran, so a copy that moved now is NOT CONFIRMED (7).
spec_unchanged "the read-back" 7
if ! python3 "$COMPARE_PY" "$SPEC" "$TMP/after.json" "$KIND" > "$TMP/after-diff.txt"; then
    say "NOT CONFIRMED — v$NEW_VERSION is live but does not equal $KIND_FILE_REL:"
    sed 's/^DIFF\t/  /; s/\t/  /g' "$TMP/after-diff.txt"
    exit 7
fi
say "$KIND v$LIVE_VERSION -> v$NEW_VERSION live at $BOSS_JOBS_URL — confirmed by reading the active row back and comparing it to $KIND_FILE_REL (equal); packet ${OPS_REQUEST_ID:-none}"
exit 0
