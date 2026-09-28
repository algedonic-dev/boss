#!/usr/bin/env bash
#
# prune-registry-versions — delete the forge registry's image versions
# that no train, no rollback and no cluster can want any more, and
# record exactly what was kept, why, and what went.
#
# WHY IT EXISTS (backlog 9789a827, David 2026-09-17: the disk was not
# delivered; 'that order sounds great' — buy the headroom)
# ---------------------------------------------------------------------
# Measured 2026-09-16: /opt/forgejo/data is 93 GB of the forge's 228 GB
# disk — the Forgejo container registry every train pushes a
# `boss:<sha>` image to (~1–3 GB each, plus `boss-ci:<sha>` per train)
# — and nothing prunes the REGISTRY side. prune-registry-tags.lib.sh
# removes LOCAL docker tags, and only after the registry holds them,
# because the registry IS the rollback path; disk-floor-sweep reaches
# ~76 GB free and stops; the locomotive floor flapped all day until it
# was set to 40. The replacement disk was not delivered, so the
# registry keeps growing ~2 GB per train. Forgejo's own package cleanup
# rules are a dashboard act; this verb is the machine's.
#
# WHAT THE REGISTRY HOLDS (measured 2026-09-17 against the live forge,
# Forgejo 16.0.2: 1,423 container versions). `docker push` of a tag
# stores THREE package versions: the tag (whose one file is the OCI
# index, manifest.json) and two untagged `sha256:<digest>` children —
# the image manifest and buildx's provenance manifest — and the
# CHILDREN are where the layer files live. Deleting only the tag frees
# nothing; deleting a child another tag shares breaks that tag. So the
# index of EVERY tagged version is read (through /v2, the docker pull
# path) before anything is classified, and a child goes only when
# every tag that references it goes. The packages API declares no
# sizes (`files[].size` is null), so bytes are not claimed here: the
# record carries `df` of the data directory before and after, and the
# blobs themselves are reclaimed by Forgejo's own cleanup cron once no
# version references them — the operator reads the difference on the
# packet, this script does not guess it. Measured on the first real
# prune (backlog ea67ad87, 2026-09-17): 1,337 deleted, df moved 724 KB
# — the cron had not run yet. Its cadence, [cron.cleanup_packages] in
# app.ini, is not in this tree; the record reads it off the forge's
# own app.ini at run time (`cleanup_cron`) or says it could not.
#
# THE KEEP SET, derived — never a list typed here — and a half that
# cannot be derived is a REFUSAL that deletes nothing:
#
#   1. THE LIVE IMAGES. Every image tag the cluster runs under the
#      registry's owner (`kubectl get deploy,sts,cronjob -A`, the same
#      kubectl resolution the census uses), and specifically the tag
#      deploy/boss in namespace boss serves — the cluster-watchdog's
#      own read. No deploy/boss image = refuse.
#   2. THE ROLLBACK TARGET. The last-converged stamp the deploy runner
#      writes and the watchdog rolls to BY NAME
#      (`$BOSS_FORGE_LAST_BUILT`, default the checkout owner's
#      ~/.boss-last-built — the runner runs as that user; this verb
#      runs as root under the ops runner with no HOME). Absent or not
#      a sha = refuse: "roll back" is a target, not a verb.
#   3. THE TRAINS. landed-train-shas.lib.sh, the one reader the disk
#      sweep already trusts, over the newest 2N pr-train packets
#      (N = keep_trains, default 10; 2N so that at least N landed
#      trains are covered unless more than half are still open, and
#      more is the safe direction): the shas CLOSED trains name, PLUS
#      every sha-shaped token an OPEN train carries — the union, never
#      the sweep's closed-minus-open (backlog 8d77d670, M3). A read that
#      cannot answer, or no closed train, = refuse.
#   4. `latest` — the quickstart tag and the chores' image, never a
#      build artifact of the converge (prune-registry-tags.lib.sh).
#   5. EVERYTHING NEWER THAN 24 h (BOSS_PRUNE_KEEP_HOURS): a push in
#      flight, a train not yet closed, a gate's hand build.
#   6. EVERYTHING IT CANNOT CLASSIFY: a tag that is not a sha
#      (`rust1.96`), a version whose created_at does not parse, a tag
#      whose index could not be read (its children are unknown), and —
#      while ANY index was unreadable — every untagged version no
#      readable index references. Unclassified is kept and named.
#      A listing that cannot be shown WHOLE judges nothing: fewer
#      versions read than the forge's X-Total-Count, a count that is not
#      a number, or a live / rollback / `latest` tag missing from it is a
#      REFUSAL (backlog 8d77d670, M1; until the re-review of 1bef55a6 it
#      only held the orphans, and a short listing planned 0 and passed).
#
# THE CEILING (backlog 8d77d670, M2). A plan past 1,500 versions or 90%
# of the two packages' versions is refused before the first DELETE,
# naming the numbers: since design 97add747 this runs unattended every
# day, and a plan that size is a keep set that came out wrong more often
# than a registry that grew. The fraction GROWS WITH THE BACKLOG (backlog
# 1bef55a6 (3)): the keep set is roughly constant, so after about eight
# days without a run the plan passes 90% (ee6caa74 planned 89.6%) —
# exactly when the registry most needs the prune. The HAND verb's lifts,
# bounded rather than approved (review finding 3; reasoning at the
# ceiling): a third argument, max_delete, replaces the absolute bound up
# to twice its default; the fraction is lifted only by a larger
# keep_trains, which keeps more and so plans less. Each refusal names
# the lift that fits it, with a margin. The daily verb's argv admits
# nothing, so an unattended run keeps both bounds at their defaults.
#
# A PASS THAT COULD JUDGE TOO LITTLE FAILS (backlog 1bef55a6 (1), review
# finding 2). With no bearer from /v2/token, every manifest read
# failing, or fewer than 90% of the tags' indexes read, tags and orphans
# are held unclassified and the plan shrinks toward 0 — and `deleted 0
# of 0 planned` is exactly what the daily watch's `deleted = planned`
# passed, every night, while the registry grew. It still prints its
# record and its list (the unclassified versions are the evidence), then
# exits 1 naming why. The verdict line carries the read percentage and
# the unclassified count, and the watch judges them again, adding the
# one it alone can: a daily run that planned nothing while it held
# versions it could not classify.
#
# A DELETE's ANSWER IS NOT ITS EFFECT (review finding 1). After the
# deletes the registry is listed again, and a planned version still
# listed is a survivor, whatever its DELETE answered; a run with
# survivors fails, and so does a run whose every DELETE answered 404 —
# the answer a moved route gives to everything. `deleted` (204) and
# `gone` (404) stay two numbers in the record; the verdict's count is
# the planned versions the re-list no longer holds.
#
# THE CREDENTIAL. The converge pushes with david's rootless docker and
# its ambient registry login (cluster-deploy-runner.sh; mirror-base-
# images.sh points root at the same config). That login is
# `auths."<registry host>".auth` in the checkout owner's
# ~/.docker/config.json — base64 of user:token — and it is the one
# credential this verb uses, for the packages API (Basic, the token as
# the password) and for the /v2 manifest reads (a bearer minted from it
# at /v2/token, docker's own flow). IT IS NEVER PRINTED: it rides in a
# curl config file (mode 600, in a private tempdir), never in argv,
# and every line a tool says back is scrubbed of it before it reaches
# the packet. Measured 2026-09-17: the pod's read token answers the
# listing 403 for want of `read:package`, so the scope is measured here
# too — a 403 refuses NAMING THE SCOPE the forge's own message asks
# for (`read:package` on the listing, `write:package` on the first
# DELETE), a ceremony for David, and the real run stops on that first
# DELETE before a second attempt. Forgejo's package scopes are read and
# write; a delete is a write.
#
# USAGE
#   prune-registry-versions.sh --dry-run | --for-real [keep_trains [max_delete]]
#
# EXIT
#   0  done (or, with --dry-run, the keep set and the plan)
#   2  refused — the reason names the bound; nothing was deleted
#   1  failed part-way — the record states what was deleted; or a
#      tool this needs could not answer, including a pass that could
#      read no manifest index and so could classify nothing
#
# The record is ONE JSON line on stdout; every human-readable line is
# on stderr, so the ops runner's captured output carries both and a
# reader can jq the FIRST JSON line. The order is load-bearing (backlog
# 5323f3ef, measured 2026-09-17 05:27Z on the first dry run, ops-request
# 279b8659): the verb printed 1,313 per-version lines (165 KB) and the
# record LAST, the runner keeps the first 100 KB of a verb's output
# (ops-runner.sh, OPS_OUTPUT_CAP), and the packet held the plan's head
# and none of its verdict. So the summary and the record are printed
# BEFORE the per-version lines, in both modes, and the full list is
# also a file on the forge (BOSS_PRUNE_LIST_DIR/<stamp>.txt) the record
# names — written before the first DELETE, and a file that cannot be
# written is a refusal. A verb whose verdict is the last line must fit
# in the runner's window; this one no longer relies on that. And the
# per-version lines a REAL run prints carry the outcome as the verb
# (DELETED / GONE / FAILED <code> / KEPT), joined from the plan and the
# outcomes before printing; only --dry-run prints `would DELETE`
# (backlog ea67ad87: the first real run printed the plan file as-is
# and appended the outcomes after the runner's cut, so the packet read
# 898 conditionals under a verdict that said OK).
#
# ENV (test seams — the ops-runner passes no packet-supplied environment,
# only an argv built from the allowlist, so a packet cannot set these)
#   BOSS_JOBS_URL              the system of record (required, no default:
#                              a guessed instance answers 'no trains')
#   BOSS_FORGE_REGISTRY        the image the converge pushes (default:
#                              forge-defaults.sh's, off the registry
#                              host in /etc/boss/sor.env; host, owner and
#                              the sibling boss-ci are derived from it)
#   BOSS_PRUNE_FORGE_URL       the forge's HTTP base (default: http://<host>,
#                              the registry is plain HTTP on the LAN)
#   BOSS_PRUNE_DOCKER_CONFIG   the docker config holding the login
#                              (default: <checkout owner>/.docker/config.json)
#   BOSS_FORGE_LAST_BUILT      the converge's stamp file
#                              (default: <checkout owner>/.boss-last-built)
#   BOSS_PRUNE_KEEP_HOURS      the freshness window (default: 24)
#   BOSS_PRUNE_DF_PATH         the directory df measures (default:
#                              /opt/forgejo/data; unmeasured if absent);
#                              Forgejo's app.ini is read under it at
#                              gitea/conf/app.ini for the cleanup cadence
#   BOSS_PRUNE_LIST_DIR        where the full per-version list is written
#                              (default: /var/backups/boss/registry-prune,
#                              beside the second stack's capture; the ops
#                              runner is root there)
#   BOSS_KUBECTL / KUBECONFIG  see undeclared-objects.sh; resolved once

set -uo pipefail

# shellcheck source=infra/lib/jq.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/jq.sh"

ME="prune-registry-versions"
say() { echo "$ME: $*" >&2; }
refuse() { say "REFUSED — $*"; say "  Nothing was deleted."; exit 2; }

SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$SELF_DIR/../.." && pwd)"
RESOLVE="$REPO/infra/cluster/undeclared-objects.sh"

# --- bound 1: the arguments -------------------------------------------------
usage() {
    say "usage: $ME --dry-run | --for-real [keep_trains [max_delete]]"
    say "  --dry-run   derive the keep set, read the registry, print the plan; deletes nothing"
    say "  --for-real  the same, then DELETE every version the plan names"
    say "  keep_trains how many of the newest landed trains' shas to keep (default 10)"
    say "  max_delete  the most deletions this run accepts, read off a dry run; replaces the"
    say "              absolute ceiling (default 1,500) for this run only, at most 3,000;"
    say "              the 90% bound holds — a larger keep_trains is what plans less"
    exit 2
}
[ "$#" -ge 1 ] && [ "$#" -le 3 ] || usage
DRY=""
case "$1" in
    --dry-run) DRY=1 ;;
    --for-real) DRY=0 ;;
    *) say "the only modes are --dry-run and --for-real, not \`$1\`"; usage ;;
esac
KEEP_TRAINS="${2:-10}"
# The allowlist checks the shape first; the script re-checks rather
# than relying on one layer (the retire-second-stack convention).
case "$KEEP_TRAINS" in
    ''|*[!0-9]*|0*) refuse "keep_trains must be a positive whole number, not \`$KEEP_TRAINS\`" ;;
esac
[ "$KEEP_TRAINS" -le 999 ] || refuse "keep_trains of $KEEP_TRAINS is beyond the 999 the allowlist admits"
MAX_DELETE_ARG="${3:-}"
if [ "$#" -ge 3 ]; then
    case "$MAX_DELETE_ARG" in
        ''|*[!0-9]*|0*) refuse "max_delete must be a positive whole number, not \`$MAX_DELETE_ARG\`" ;;
    esac
fi
# The absolute ceiling's default, and the most a hand run's max_delete
# may name: twice it (review finding 3 of backlog 1bef55a6; the
# reasoning is at the ceiling below).
MAX_DELETE_DEFAULT=1500
MAX_DELETE_CAP=$((MAX_DELETE_DEFAULT * 2))
if [ -n "$MAX_DELETE_ARG" ] && { [ "${#MAX_DELETE_ARG}" -gt 4 ] || [ "$MAX_DELETE_ARG" -gt "$MAX_DELETE_CAP" ]; }; then
    refuse "max_delete of $MAX_DELETE_ARG is beyond $MAX_DELETE_CAP, twice the default ceiling — a backlog that size is walked down with a larger keep_trains, run by run"
fi
KEEP_HOURS="${BOSS_PRUNE_KEEP_HOURS:-24}"
case "$KEEP_HOURS" in
    ''|*[!0-9]*) refuse "BOSS_PRUNE_KEEP_HOURS must be a whole number of hours, not \`$KEEP_HOURS\`" ;;
esac

for tool in jq curl date; do
    command -v "$tool" >/dev/null 2>&1 || { say "$tool is not on PATH, so nothing can be read. Nothing was deleted."; exit 1; }
done

JOBS_URL="${BOSS_JOBS_URL:-}"
[ -n "$JOBS_URL" ] || refuse "BOSS_JOBS_URL is not set and there is no safe default: the landed trains are read from the system of record, and a read against a guessed instance answers 'no trains' instead of erroring"

# --- the registry, derived from the image the converge pushes --------------
# `<host:port>/<owner>/boss` -> the host, the owner, and the two
# packages this verb touches: boss (the converge's image) and boss-ci
# (the per-train CI image, .forgejo/workflows/ci.yml). Nothing else in
# the registry — boss-ci-cache, the mirrored bases — is ever a
# candidate: a version of another name is not read. The image repo is
# forge-defaults.sh's, the same one the converge pushes.
. "$SELF_DIR/forge-defaults.sh"
forge_need REGISTRY
REG_HOST="${REGISTRY%%/*}"
reg_rest="${REGISTRY#*/}"
OWNER="${reg_rest%%/*}"
IMAGE_NAME="${reg_rest#*/}"
case "$REG_HOST/$OWNER/$IMAGE_NAME" in
    "$REGISTRY") ;;
    *) refuse "BOSS_FORGE_REGISTRY \`$REGISTRY\` is not of the shape host[:port]/owner/name" ;;
esac
PACKAGES="$IMAGE_NAME ${IMAGE_NAME}-ci"
FORGE_URL="${BOSS_PRUNE_FORGE_URL:-http://$REG_HOST}"

# --- the checkout owner: whose docker login and stamp these are -----------
# The ops runner is root with no HOME; the converge runs as the
# checkout's owner and its login and stamp live in that user's home.
# Read off the directory, never hardcoded (delete-orphan-object.sh's
# as_owner; ops-request c9877f75). Only consulted for a default. The
# owner and the login are read by registry-login.lib.sh, which the
# forge data move reads them through too (backlog 016aeea3).
# shellcheck source=infra/forge/registry-login.lib.sh
. "$SELF_DIR/registry-login.lib.sh"
if [ -z "${BOSS_PRUNE_DOCKER_CONFIG:-}" ] || [ -z "${BOSS_FORGE_LAST_BUILT:-}" ]; then
    OWNER_HOME="$(checkout_owner_home "$REPO")" || refuse "cannot resolve the owner of $REPO, so neither the docker login nor the converge's stamp has a default path; set BOSS_PRUNE_DOCKER_CONFIG and BOSS_FORGE_LAST_BUILT"
fi
DOCKER_CONFIG_FILE="${BOSS_PRUNE_DOCKER_CONFIG:-$OWNER_HOME/.docker/config.json}"
STAMP_FILE="${BOSS_FORGE_LAST_BUILT:-$OWNER_HOME/$LAST_BUILT_NAME}"
DF_PATH="${BOSS_PRUNE_DF_PATH:-/opt/forgejo/data}"
LIST_DIR="${BOSS_PRUNE_LIST_DIR:-/var/backups/boss/registry-prune}"

TMP=$(mktemp -d) || exit 1
chmod 700 "$TMP"
trap 'rm -rf "$TMP"' EXIT

# --- bound 2: the credential ------------------------------------------------
# Parsed into variables and never printed. `scrub` removes the literal
# auth string, the token and the bearer from anything a tool said
# back, so even an error that quoted a header cannot carry it onto the
# packet.
AUTH_B64=""
TOKEN=""
BEARER=""
scrub() { # stdin -> stdout, every credential form replaced
    local line
    while IFS= read -r line || [ -n "$line" ]; do
        [ -n "$AUTH_B64" ] && line="${line//"$AUTH_B64"/***}"
        [ -n "$TOKEN" ] && line="${line//"$TOKEN"/***}"
        [ -n "$BEARER" ] && line="${line//"$BEARER"/***}"
        printf '%s\n' "$line"
    done
}
registry_login_read "$DOCKER_CONFIG_FILE" "$REG_HOST" || refuse "$REGISTRY_LOGIN_WHY"
# The curl configs: the header rides in a file the kernel never shows
# in a process listing, and the file dies with the tempdir.
BASIC_CFG="$TMP/basic.cfg"
( umask 077; printf 'header = "Authorization: Basic %s"\n' "$AUTH_B64" > "$BASIC_CFG" )
say "credential: docker login for $REG_HOST as $CRED_USER from $DOCKER_CONFIG_FILE (the converge's push credential; never printed)"

# One HTTP call: prints the status code; the body lands in the named
# file. No -f: the code is read, and a 403 is a verdict, not a failure.
http() { # <cfg> <method> <url> <body-out> [extra curl args] -> stdout: code
    local cfg="$1" method="$2" url="$3" out="$4"
    shift 4
    curl -sS --max-time 60 -K "$cfg" -X "$method" -o "$out" -w '%{http_code}' "$@" "$url" 2> "$TMP/curl.err"
}
# The scope Forgejo's 403 asks for, from its own message — copied, not
# retyped; the fallback is the scope the method needs.
scope_of() { # <body-file> <fallback>
    local named
    # awk drains its input; `head -n 1` would SIGPIPE the producer under
    # pipefail (backlog 76d04429).
    named=$(jq -r '.message // empty' "$1" 2>/dev/null | grep -oE '(read|write|delete):package' | awk 'NR == 1')
    printf '%s' "${named:-$2}"
}

# --- bound 3: the keep set, half by half -----------------------------------
# (a) the live images, cluster-wide, through the one kubectl resolution.
KUBECTL_LINE=$("$RESOLVE" --kubectl) || {
    say "REFUSED — no kubectl to read the cluster's live images with (see above); the keep set cannot be derived."
    say "  Nothing was deleted."
    exit 2
}
read -r -a KUBECTL <<<"$KUBECTL_LINE"
if ! "${KUBECTL[@]}" get deploy,sts,cronjob -A -o json > "$TMP/cluster.json" 2> "$TMP/kubectl.err"; then
    say "REFUSED — the cluster's live images cannot be read (kubectl get deploy,sts,cronjob -A); kubectl said:"
    sed 's/^/    /' "$TMP/kubectl.err" >&2
    say "  A keep set missing the live image is not a keep set. Nothing was deleted."
    exit 2
fi
# Every image under <host>/<owner>/ the cluster's templates name, as
# name:tag — deployments, statefulsets and cronjobs, main and init
# containers alike.
jq -r --arg prefix "$REG_HOST/$OWNER/" '
    [ .items[]
      | (.spec.template.spec // .spec.jobTemplate.spec.template.spec // {})
      | ((.containers // []) + (.initContainers // []))[]
      | .image // empty
      | select(startswith($prefix))
      | ltrimstr($prefix) ]
    | unique[]' "$TMP/cluster.json" > "$TMP/live.txt" 2>/dev/null
LIVE_MAIN=$(jq -r --arg prefix "$REG_HOST/$OWNER/" --arg name "$IMAGE_NAME" '
    .items[]
    | select(.kind == "Deployment" and .metadata.name == "boss" and .metadata.namespace == "boss")
    | .spec.template.spec.containers[0].image // empty
    | select(startswith($prefix + $name + ":"))
    | ltrimstr($prefix + $name + ":")' "$TMP/cluster.json" 2>/dev/null | awk 'NR == 1')
[ -n "$LIVE_MAIN" ] || refuse "the cluster lists no deploy/boss in namespace boss running $REG_HOST/$OWNER/$IMAGE_NAME:<tag> — the live image is the first half of the keep set, and it is not there to keep"
LIVE_COUNT=$(grep -c . "$TMP/live.txt" || true)
say "keep: live: $LIVE_MAIN (deploy/boss in namespace boss); every $OWNER image the cluster runs ($LIVE_COUNT): $(paste -sd ' ' "$TMP/live.txt")"

# (b) the rollback target: the converge's stamp, the watchdog's target.
STAMP=$(head -n 1 "$STAMP_FILE" 2>/dev/null | tr -d '[:space:]')
[ -n "$STAMP" ] || refuse "no rollback target: $STAMP_FILE is absent or empty (cluster-deploy-runner writes it after every converge; cluster-watchdog rolls to it by name), so the keep set cannot be derived"
if ! [[ "$STAMP" =~ ^[0-9a-f]{7,40}$ ]]; then
    refuse "the rollback target in $STAMP_FILE is \`$STAMP\`, not a sha, so the keep set cannot be derived"
fi
say "keep: stamp: $STAMP (the rollback target, $STAMP_FILE)"

# (c) the trains, through the lib the disk sweep trusts — its one read,
# answered as its two HALVES. The sweep asks which images are
# COLLECTABLE and subtracts what open trains mention (closed minus
# open); a KEEP set must ADD them instead (closed union open), or an
# open train makes the prune keep LESS — an in-flight train's image,
# and a landed one an open train still names, both deletable (backlog
# 8d77d670, review M3). A read that cannot answer is a refusal, not a
# fallback: there is no age-only mode for a registry delete.
export BOSS_SWEEP_ACTOR="${BOSS_SWEEP_ACTOR:-automation:prune-registry-versions}"
# shellcheck source=infra/forge/landed-train-shas.lib.sh
. "$SELF_DIR/landed-train-shas.lib.sh"
LOOKBACK=$((KEEP_TRAINS * 2))
if ! train_sha_sets curl "$JOBS_URL" "$LOOKBACK" "$ME" "$TMP/trains-closed.txt" "$TMP/trains-open.txt"; then
    refuse "the landed-train lookup could not answer from $JOBS_URL ($TRAIN_SETS_WHY), so the keep set cannot be derived"
fi
LANDED_COUNT=$(grep -c . "$TMP/trains-closed.txt" || true)
OPEN_COUNT=$(grep -c . "$TMP/trains-open.txt" || true)
[ "$LANDED_COUNT" -gt 0 ] || refuse "no landed train shas could be read from $JOBS_URL (see the lookup's own account above), so the keep set cannot be derived"
say "keep: landed: $LANDED_COUNT sha(s) from the newest $LOOKBACK pr-train packets (keep_trains $KEEP_TRAINS); open: $OPEN_COUNT sha-shaped token(s) open trains carry, kept too"
say "keep: latest, and every version newer than $KEEP_HOURS h"

# The keep keys: seven-hex, the lib's convention — a boss tag is seven
# (git rev-parse --short), a boss-ci tag is forty (github.sha), and the
# first seven is one comparison for both. key<TAB>reason.
{
    for t in $(cat "$TMP/live.txt"); do
        tag="${t#*:}"
        [[ "$tag" =~ ^[0-9a-f]{7,40}$ ]] && printf '%s\tlive (%s)\n' "${tag:0:7}" "$t"
    done
    printf '%s\tstamp (rollback target)\n' "${STAMP:0:7}"
    grep . "$TMP/trains-closed.txt" | sed 's/$/\tlanded train/'
    grep . "$TMP/trains-open.txt" | sed 's/$/\topen train/'
} | sort -u -t "$(printf '\t')" -k1,1 > "$TMP/keys.tsv"

# --- the registry: every version of the two packages ------------------------
# A LIMIT IS NOT A FILTER: pages of 50 until an empty page, with the
# X-Total-Count the forge reports beside the count read, so the record
# says how much of the registry was seen. Versions arrive every train,
# so the two can differ by a push; a push in flight is inside the
# freshness window either way.
NOW=$(date -u +%s)
: > "$TMP/versions.tsv"   # name<TAB>version<TAB>created_at<TAB>epoch, the two
# One reader for the whole listing, used twice: before the plan, and
# after the deletes (backlog 1bef55a6 (1), the re-list below). Returns 0
# with every version in <out> (name<TAB>version<TAB>created_at) and
# L_LISTED / L_TOTAL / L_PAGES set; or non-zero with L_CODE (the HTTP
# code, empty when the page answered but was not a list) and L_WHY.
list_versions() { # <out>
    local out="$1" page=1 code n
    : > "$out"
    L_LISTED=0; L_TOTAL=""; L_PAGES=0; L_CODE=""; L_WHY=""
    while :; do
        code=$(http "$BASIC_CFG" GET "$FORGE_URL/api/v1/packages/$OWNER?type=container&limit=50&page=$page" "$TMP/page.json" -D "$TMP/page.hdr")
        if [ "$code" != 200 ]; then
            L_CODE="$code"; L_WHY="the registry listing (page $page) answered $code, not 200"
            return 1
        fi
        [ -n "$L_TOTAL" ] || L_TOTAL=$(tr -d '\r' < "$TMP/page.hdr" | sed -n 's/^[Xx]-[Tt]otal-[Cc]ount: *//p' | awk 'NR == 1')
        n=$(jq -r 'if type == "array" then length else "notlist" end' "$TMP/page.json" 2>/dev/null)
        case "$n" in
            ''|notlist) L_WHY="the registry listing (page $page) is not a version list this can read"; return 1 ;;
            0) break ;;
        esac
        # Every element must be a version object: a stray element made
        # this jq fail and drop the page's rows while `n` still counted
        # them (the re-review, finding 2). Its exit is read, and the rows
        # it wrote must be the count the page claimed.
        if ! jq -r '.[] | [.name, .version, .created_at] | @tsv' "$TMP/page.json" > "$TMP/page.tsv" 2> "$TMP/page.err" \
            || [ "$(grep -c '' "$TMP/page.tsv" || true)" != "$n" ]; then
            L_WHY="the registry listing (page $page) held an element this cannot read as a version ($(head -c 200 "$TMP/page.err" | tr '\n' ' '))"
            return 1
        fi
        cat "$TMP/page.tsv" >> "$out"
        L_LISTED=$((L_LISTED + n))
        page=$((page + 1))
        [ "$page" -le 400 ] || { L_WHY="the registry listing did not end after 400 pages"; return 1; }
    done
    L_PAGES=$((page - 1))
}
if ! list_versions "$TMP/versions.raw"; then
    case "$L_CODE" in
        403)
            scope=$(scope_of "$TMP/page.json" read:package)
            say "REFUSED — the registry listing answered 403: the docker login for $REG_HOST (user $CRED_USER) lacks $scope; the forge said:"
            jq -r '.message // empty' "$TMP/page.json" 2>/dev/null | scrub | sed 's/^/    /' >&2
            say "  Minting a token with $scope (and write:package for the delete) is a root ceremony, David's; then docker login $REG_HOST with it as the checkout owner."
            say "  Nothing was deleted."
            exit 2 ;;
        '')
            say "$L_WHY; refusing to guess at the rest. Nothing was deleted."
            exit 1 ;;
        *)
            say "$L_WHY:"
            { scrub < "$TMP/curl.err"; head -c 400 "$TMP/page.json" | scrub; } | sed 's/^/    /' >&2
            say "  Nothing was deleted."
            exit 1 ;;
    esac
fi
LISTED="$L_LISTED"
TOTAL="$L_TOTAL"
# THE LISTING MUST BE WHOLE, or nothing is judged on it (the re-review of
# backlog 1bef55a6, finding 1). Until then a short listing only held the
# orphans (backlog 8d77d670, M1): a first page of the newest versions and
# an empty second, against an X-Total-Count of 25, planned 0 and passed
# `deleted 0 of 0 … 100 percent; 0 unclassified` — the read percentage's
# denominator is the tags LISTED, so an unread remainder is invisible to
# it. A listing short of the forge's count, or a count that is not a
# number, is a refusal that deletes nothing. The count is checked for
# digits BEFORE it is printed anywhere: it is a header the forge sends,
# and a line it carried must never read as this verb's verdict.
case "$TOTAL" in
    ''|*[!0-9]*) refuse "the forge sent no numeric X-Total-Count with the listing, so the $LISTED version(s) read cannot be shown to be the whole registry" ;;
esac
[ "$LISTED" -ge "$TOTAL" ] || refuse "the listing read $LISTED version(s) of the $TOTAL the forge counts; the unread remainder may hold a tag that owns an 'orphan', a keep tag, or the versions this run would judge"
say "registry: $LISTED version(s) listed for $OWNER (the forge counts $TOTAL), in $L_PAGES page(s)"

# Only the two packages; the epoch parsed by date, unparsable -> empty,
# and empty is unclassified below.
while IFS=$'\t' read -r name ver created; do
    case " $PACKAGES " in *" $name "*) ;; *) continue ;; esac
    epoch=$(date -u -d "$created" +%s 2>/dev/null || true)
    printf '%s\t%s\t%s\t%s\n' "$name" "$ver" "$created" "$epoch" >> "$TMP/versions.tsv"
done < "$TMP/versions.raw"
CANDIDATES=$(grep -c . "$TMP/versions.tsv" || true)
say "registry: $CANDIDATES version(s) belong to $PACKAGES"

# --- the indexes: what each tag's children are ------------------------------
# A bearer for /v2, minted the way docker mints one (GET /v2/token with
# the login). When the forge will not mint one, no index is readable,
# every tag's children are unknown, and this pass deletes NO tag —
# loudly, below.
BEARER_CFG="$TMP/bearer.cfg"
INDEX_OK=1
code=$(http "$BASIC_CFG" GET "$FORGE_URL/v2/token?service=container_registry" "$TMP/token.json")
TOKEN_CODE="$code"
if [ "$code" = 200 ]; then
    BEARER=$(jq -r '.token // empty' "$TMP/token.json" 2>/dev/null)
fi
if [ -z "$BEARER" ]; then
    INDEX_OK=0
    say "index: /v2/token answered $code with no token for the $REG_HOST login, so no manifest index can be read this pass — every tag's children are unknown, and no tag will be deleted"
else
    ( umask 077; printf 'header = "Authorization: Bearer %s"\n' "$BEARER" > "$BEARER_CFG" )
fi
ACCEPT='Accept: application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.list.v2+json, application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json'
: > "$TMP/children.tsv"     # name<TAB>tag<TAB>child-digest
: > "$TMP/unreadable.tsv"   # name<TAB>tag<TAB>code
if [ "$INDEX_OK" = 1 ]; then
    while IFS=$'\t' read -r name ver created epoch; do
        case "$ver" in sha256:*) continue ;; esac
        code=$(http "$BEARER_CFG" GET "$FORGE_URL/v2/$OWNER/$name/manifests/$ver" "$TMP/manifest.json" -H "$ACCEPT")
        # A 200 with no body is unreadable, not a manifest with no
        # children — and `jq -e` alone calls it a pass (d96e38ab).
        if [ "$code" != 200 ] || ! jq_doc_file "$TMP/manifest.json" \
            || ! jq -e . < "$TMP/manifest.json" > /dev/null 2>&1; then
            printf '%s\t%s\t%s\n' "$name" "$ver" "$code" >> "$TMP/unreadable.tsv"
            continue
        fi
        jq -r --arg n "$name" --arg t "$ver" '(.manifests // [])[] | .digest // empty | [$n, $t, .] | @tsv' "$TMP/manifest.json" >> "$TMP/children.tsv"
    done < "$TMP/versions.tsv"
fi
UNREADABLE=$(grep -c . "$TMP/unreadable.tsv" || true)
# The count only: each unreadable tag is named, with its HTTP code, in
# the per-version list that follows the record (backlog 5323f3ef — a
# per-tag list here sat before the verdict).
[ "$UNREADABLE" -gt 0 ] && say "index: $UNREADABLE tag(s) whose manifest could not be read — kept, and every orphan is unclassified this pass (named below, after the record)"
# How much of the registry this pass could JUDGE (backlog 1bef55a6 (1)
# and its review, finding 2). An unreadable index holds its tag AND every
# orphan, so a pass that read few indexes plans little or nothing — with
# no bearer, or every manifest read failing, the plan is 0 by
# construction, and one readable index of 13 left the plan 0 as well.
# `deleted 0 of 0` is not a verdict: below READ_FLOOR_PCT of the tags'
# indexes read, the pass FAILS in either mode, and INDEX_SHORT names why.
# The percentage rides the verdict line too, where the daily watch judges
# it again (watch-prune-registry-versions-daily, v2).
READ_FLOOR_PCT=90
TAGS_TOTAL=$(awk -F'\t' '$2 !~ /^sha256:/' "$TMP/versions.tsv" | grep -c . || true)
INDEX_READ=0
[ "$INDEX_OK" = 1 ] && INDEX_READ=$((TAGS_TOTAL - UNREADABLE))
READ_PCT=100
[ "$TAGS_TOTAL" -gt 0 ] && READ_PCT=$((INDEX_READ * 100 / TAGS_TOTAL))
INDEX_SHORT=""
if [ "$INDEX_OK" != 1 ]; then
    INDEX_SHORT="/v2/token answered $TOKEN_CODE and minted no bearer"
elif [ "$TAGS_TOTAL" -gt 0 ] && [ "$INDEX_READ" -le 0 ]; then
    INDEX_SHORT="every manifest read failed ($UNREADABLE of $TAGS_TOTAL unreadable)"
elif [ "$READ_PCT" -lt "$READ_FLOOR_PCT" ]; then
    INDEX_SHORT="$UNREADABLE of $TAGS_TOTAL manifest reads failed, $READ_PCT percent read, under the $READ_FLOOR_PCT percent floor"
fi

# --- the positive control: the listing shows what is known to be there ----
# The count can agree and the listing still be wrong. Every keep tag of
# the two packages the cluster and the converge name — each live image
# (a sha by its first seven, any other tag exactly), the rollback target,
# and `latest` — must be a listed version; one that is not means the
# listing does not show what is really there, and nothing is judged on
# it (backlog 8d77d670, M1; until the re-review of 1bef55a6 this only
# held the orphans, and a run whose listing missed its live image still
# deleted tags and passed). A refusal, naming each missing tag.
missing=$({
    for t in $(cat "$TMP/live.txt"); do
        n="${t%%:*}"; tag="${t#*:}"
        case " $PACKAGES " in *" $n "*) ;; *) continue ;; esac
        printf '%s\t%s\tlive\n' "$n" "$tag"
    done
    printf '%s\t%s\tstamp\n' "$IMAGE_NAME" "$STAMP"
    printf '%s\tlatest\tlatest\n' "$IMAGE_NAME"
} | while IFS=$'\t' read -r n tag why; do
    if [[ "$tag" =~ ^[0-9a-f]{7,40}$ ]]; then
        awk -F'\t' -v n="$n" -v k="${tag:0:7}" '$1 == n && substr($2, 1, 7) == k { f = 1 } END { exit !f }' "$TMP/versions.tsv"
    else
        awk -F'\t' -v n="$n" -v k="$tag" '$1 == n && $2 == k { f = 1 } END { exit !f }' "$TMP/versions.tsv"
    fi || printf '%s:%s (%s) ' "$n" "$tag" "$why"
done)
[ -z "$missing" ] || refuse "keep tag(s) not in the listing: ${missing% } — the listing does not show what the cluster and the converge know is there, so nothing can be judged on it"

# --- the classification ------------------------------------------------------
# One pass over the tags, one over the untagged children; the verdicts
# land in classes.tsv as name<TAB>version<TAB>class<TAB>reason, class
# one of keep / delete / unclassified. Every rule keeps by default: a
# version reaches `delete` only by passing every keep test.
FRESH_BEFORE=$((NOW - KEEP_HOURS * 3600))
: > "$TMP/classes.tsv"
classify_tag() { # <name> <ver> <epoch> -> class<TAB>reason
    local name="$1" ver="$2" epoch="$3" key reason
    if [ "$ver" = latest ]; then printf 'keep\tlatest\n'; return; fi
    if ! [[ "$ver" =~ ^[0-9a-f]{7,40}$ ]]; then printf 'unclassified\ttag is not a sha\n'; return; fi
    key="${ver:0:7}"
    reason=$(awk -F'\t' -v k="$key" '$1 == k { print $2; exit }' "$TMP/keys.tsv")
    if [ -n "$reason" ]; then printf 'keep\t%s\n' "$reason"; return; fi
    case "$epoch" in
        ''|*[!0-9]*) printf 'unclassified\tcreated_at does not parse\n'; return ;;
    esac
    if [ "$epoch" -ge "$FRESH_BEFORE" ]; then printf 'keep\tnewer than %s h\n' "$KEEP_HOURS"; return; fi
    if [ "$INDEX_OK" != 1 ]; then printf 'unclassified\tchildren unknown (no bearer)\n'; return; fi
    if awk -F'\t' -v n="$name" -v t="$ver" '$1 == n && $2 == t { f = 1 } END { exit !f }' "$TMP/unreadable.tsv"; then
        printf 'unclassified\tindex unreadable (HTTP %s)\n' "$(awk -F'\t' -v n="$name" -v t="$ver" '$1 == n && $2 == t { print $3; exit }' "$TMP/unreadable.tsv")"
        return
    fi
    printf 'delete\tolder than the keep set\n'
}
while IFS=$'\t' read -r name ver created epoch; do
    case "$ver" in sha256:*) continue ;; esac
    verdict=$(classify_tag "$name" "$ver" "$epoch")
    printf '%s\t%s\t%s\n' "$name" "$ver" "$verdict" >> "$TMP/classes.tsv"
done < "$TMP/versions.tsv"
classify_child() { # <name> <digest> <epoch> -> class<TAB>reason
    local name="$1" dig="$2" epoch="$3" refs kept
    case "$epoch" in
        ''|*[!0-9]*) printf 'unclassified\tcreated_at does not parse\n'; return ;;
    esac
    if [ "$epoch" -ge "$FRESH_BEFORE" ]; then printf 'keep\tnewer than %s h\n' "$KEEP_HOURS"; return; fi
    # The tags whose index names this digest, and whether any is staying.
    refs=$(awk -F'\t' -v n="$name" -v d="$dig" '$1 == n && $3 == d { print $2 }' "$TMP/children.tsv")
    if [ -n "$refs" ]; then
        kept=$(printf '%s\n' "$refs" | while IFS= read -r t; do
            awk -F'\t' -v n="$name" -v t="$t" '$1 == n && $2 == t && $3 != "delete" { print $2; exit }' "$TMP/classes.tsv"
        done | awk 'NR == 1')
        if [ -n "$kept" ]; then printf 'keep\treferenced by a kept tag (%s)\n' "$kept"; return; fi
        printf 'delete\tchild of a deleted tag (%s)\n' "$(printf '%s\n' "$refs" | awk 'NR == 1')"
        return
    fi
    if [ "$INDEX_OK" != 1 ] || [ "$UNREADABLE" -gt 0 ]; then
        printf 'unclassified\treferenced by no readable index while %s index(es) were unreadable\n' "$UNREADABLE"
        return
    fi
    printf 'delete\torphan: referenced by no tag\n'
}
while IFS=$'\t' read -r name ver created epoch; do
    case "$ver" in sha256:*) ;; *) continue ;; esac
    verdict=$(classify_child "$name" "$ver" "$epoch")
    printf '%s\t%s\t%s\n' "$name" "$ver" "$verdict" >> "$TMP/classes.tsv"
done < "$TMP/versions.tsv"

# --- the plan: the counts on the packet, the list in a file ----------------
# The per-version lines are NOT printed here. Measured 2026-09-17 05:27Z
# (backlog 5323f3ef): 1,313 of them, 165 KB, and the runner's 100 KB
# window closed before the record. They go to a file on the forge now,
# and onto the packet only AFTER the record and the verdict (the tail
# of this script); the record names the file. Written before the first
# DELETE so that a real run always has a full list somewhere, and a
# file that cannot be written is a refusal — the alternative is the
# defect again, on the run that cannot be repeated.
for name in $PACKAGES; do
    awk -F'\t' -v n="$name" '$1 == n { c[$3]++ } END { printf "%s: %d keep, %d delete, %d unclassified\n", n, c["keep"], c["delete"], c["unclassified"] }' "$TMP/classes.tsv" | sed "s/^/$ME: plan: /" >&2
done
plan_lines() { # -> one line per planned deletion, then per unclassified version
    awk -F'\t' '$3 == "delete" { printf "would DELETE %s:%s — %s\n", $1, $2, $4 }' "$TMP/classes.tsv"
    awk -F'\t' '$3 == "unclassified" { printf "unclassified %s:%s — %s (kept)\n", $1, $2, $4 }' "$TMP/classes.tsv"
}
LIST_FILE="$LIST_DIR/$(date -u +%Y%m%dT%H%M%SZ).txt"
if ! mkdir -p "$LIST_DIR" 2> "$TMP/list.err" \
    || ! { printf '# %s %s at %s — registry %s/%s, packages %s; the record is on the ops-request packet\n' \
               "$ME" "$1" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$REG_HOST" "$OWNER" "$PACKAGES"
           plan_lines; } > "$LIST_FILE" 2>> "$TMP/list.err"; then
    say "REFUSED — the per-version list cannot be written to $LIST_FILE (BOSS_PRUNE_LIST_DIR); the runner keeps only the first 100 KB of this output, so without the file a real run would leave no full record:"
    sed 's/^/    /' "$TMP/list.err" >&2
    say "  Nothing was deleted."
    exit 2
fi
say "list: every per-version line is in $LIST_FILE on this host (the plan now; each outcome appended as its DELETE answers)"

# --- the ceiling: a plan too big to be a day's growth is refused ---------
# Backlog 8d77d670, review M2: since design 97add747 this runs
# unattended every day, and nothing else bounds how much one run may
# delete. Measured: the largest real run was 1,337 (2026-09-17, a
# catch-up over a month of trains); a daily run plans a few hundred
# (~60 trains a day, three versions a push, two packages). A plan past
# either bound is a keep set that came out wrong more often than a
# registry that grew — so it is REFUSED before the first DELETE, naming
# the numbers, and the plan stays in the list file for whoever reads the
# alarm. A dry run shows the plan and says a real run would refuse.
# The DEFAULTS are a reviewed edit here; the env seams exist for the
# tests, and a packet cannot set them (the runner passes argv only).
#
# What a HAND run may do (backlog 1bef55a6 (3), and finding 3 of its
# adversarial review). The fraction grows with the backlog — the keep set
# is roughly constant, so after ~8 days without a run the plan passes 90%
# (ee6caa74 planned 89.6%). The first cut let the hand verb's max_delete
# lift the fraction outright, up to 99999, for any filer with no
# approval; the review held it. The approval contract (a plan verb and a
# plan_sha256, as move-forgejo-data takes) does not fit a registry prune:
# its plan moves with every push and every hour of the 24 h window, so a
# signed plan would void before the runner picked it up. So the lift is
# bounded instead of approved:
#   * max_delete replaces the ABSOLUTE bound only, and never past
#     MAX_DELETE_CAP (twice the default). The fraction always holds.
#   * The fraction's lift is keep_trains, which the verb already took: a
#     larger keep_trains keeps MORE landed trains' shas, so it plans less
#     — the safe direction, never deleting anything the default keep set
#     would keep. A backlog is walked down in steps, then the daily
#     resumes at the default.
# Each refusal names the lift that fits it, with a margin (review
# finding 5): the number a person copies should not refuse again on the
# next push.
MORE_TRAINS=$((KEEP_TRAINS * 2))
[ "$MORE_TRAINS" -le 999 ] || MORE_TRAINS=999
if [ -n "$MAX_DELETE_ARG" ]; then
    MAX_DELETE="$MAX_DELETE_ARG"
    CEILING_SOURCE=argv
else
    MAX_DELETE="${BOSS_PRUNE_MAX_DELETE:-$MAX_DELETE_DEFAULT}"
    CEILING_SOURCE=default
fi
MAX_PERCENT="${BOSS_PRUNE_MAX_PERCENT:-90}"
case "$MAX_DELETE$MAX_PERCENT" in *[!0-9]*|'') refuse "the ceiling is not two whole numbers (BOSS_PRUNE_MAX_DELETE=$MAX_DELETE, BOSS_PRUNE_MAX_PERCENT=$MAX_PERCENT)" ;; esac
PLANNED_TOTAL=$(awk -F'\t' '$3 == "delete"' "$TMP/classes.tsv" | grep -c . || true)
CEILING=""
LIFT=""
if [ "$PLANNED_TOTAL" -gt "$MAX_DELETE" ]; then
    if [ "$CEILING_SOURCE" = argv ]; then
        CEILING="the plan deletes $PLANNED_TOTAL version(s), beyond the ceiling of $MAX_DELETE this run's max_delete named"
    else
        CEILING="the plan deletes $PLANNED_TOTAL version(s), beyond the ceiling of $MAX_DELETE per run"
    fi
    # The lift named must be one that WORKS (the re-review, finding 4):
    # max_delete lifts only the absolute bound, so when the plan is also
    # past the fraction it would refuse again, and the knob that fits is
    # keep_trains. Otherwise the plan plus 20%, rounded up so a push or
    # two between the read and the run does not refuse it again — held
    # to the cap, so a plan of 2,501..3,000 is named 3000, which lifts it.
    SUGGEST=$(( (PLANNED_TOTAL * 12 + 9) / 10 ))
    [ "$SUGGEST" -le "$MAX_DELETE_CAP" ] || SUGGEST="$MAX_DELETE_CAP"
    if [ "$CANDIDATES" -gt 0 ] && [ $((PLANNED_TOTAL * 100)) -gt $((CANDIDATES * MAX_PERCENT)) ]; then
        LIFT="the plan is ALSO past the ${MAX_PERCENT}% fraction ($PLANNED_TOTAL of $CANDIDATES), which max_delete does not lift: the knob that fits is keep_trains — prune-registry-versions --for-real $MORE_TRAINS keeps twice the landed trains and so plans less, run by run until the default fits again"
    elif [ "$PLANNED_TOTAL" -le "$MAX_DELETE_CAP" ]; then
        LIFT="a person who has read the plan lifts the absolute ceiling for one run on the hand verb: prune-registry-versions --for-real $KEEP_TRAINS $SUGGEST (the plan plus 20%, at most $MAX_DELETE_CAP; the ${MAX_PERCENT}% fraction already holds)"
    else
        LIFT="a plan past $MAX_DELETE_CAP is not liftable by max_delete; walk it down with a larger keep_trains on the hand verb (prune-registry-versions --for-real $MORE_TRAINS keeps twice the trains and so plans less), run by run"
    fi
elif [ "$CANDIDATES" -gt 0 ] && [ $((PLANNED_TOTAL * 100)) -gt $((CANDIDATES * MAX_PERCENT)) ]; then
    CEILING="the plan deletes $PLANNED_TOTAL of $CANDIDATES version(s) of $PACKAGES, beyond the ceiling of ${MAX_PERCENT}% per run"
    LIFT="the fraction is lifted by keeping more, not by naming a number: the hand verb with a larger keep_trains (prune-registry-versions --for-real $MORE_TRAINS keeps twice the landed trains and so plans less, never deleting what this run's keep set keeps), run by run until the daily's default fits again"
fi
if [ -n "$CEILING" ]; then
    if [ "$DRY" = 1 ]; then
        say "ceiling: a real run would be REFUSED — $CEILING; $LIFT"
    else
        say "REFUSED — $CEILING. A plan that size is more often a keep set that came out wrong than a registry that grew; the plan is in $LIST_FILE, and $LIFT."
        say "  Nothing was deleted."
        exit 2
    fi
fi

disk_avail_kb() { # -> KB free where the registry lives, or "null"
    if [ -d "$DF_PATH" ]; then df -Pk "$DF_PATH" 2>/dev/null | awk 'NR == 2 { print $4 }' | grep -E '^[0-9]+$' || echo null
    else echo null; fi
}
DF_BEFORE=$(disk_avail_kb)
[ "$DF_BEFORE" != null ] || say "df: $DF_PATH is not here, so the bytes are unmeasured"

# --- Forgejo's cleanup cadence: read off its app.ini, never typed ---------
# Backlog ea67ad87, measured on the first real prune (ops-request
# 973beaa2, 2026-09-17 13:41Z): 1,337 deleted and df moved 724 KB. The
# blobs are freed by Forgejo's own [cron.cleanup_packages], so the
# disk_tight:forge alarm clears only when that cron runs — and its
# cadence is NOT in this tree: the forge's compose file and data
# directory are unversioned (OPERATIONS.md), so app.ini is read here,
# on the host, under the data dir — the same file publish-github-pr.sh
# reads [repository] ROOT from — and the record carries what was found
# with where it came from. An unreadable file or an unset key is said
# as such; Forgejo's default is named as documented, never as read.
APP_INI="$DF_PATH/gitea/conf/app.ini"
ini_key() { # <file> <section> <key> -> the value, or nothing
    awk -v sec="[$2]" -v key="$3" '
        /^[[:space:]]*\[/ { s = $0; sub(/^[[:space:]]+/, "", s); sub(/[[:space:]]+$/, "", s); next }
        s == sec && index($0, key) && $0 ~ ("^[[:space:]]*" key "[[:space:]]*=") {
            sub(/^[^=]*=[[:space:]]*/, ""); sub(/[[:space:]]+$/, ""); print; exit
        }' "$1"
}
CRON_SOURCE="unread: $APP_INI is not readable here"
CRON_ENABLED=""; CRON_SCHEDULE=""; CRON_OLDER=""; CRON_NOTE=""
if [ -r "$APP_INI" ]; then
    CRON_SOURCE="$APP_INI"
    CRON_ENABLED=$(ini_key "$APP_INI" cron.cleanup_packages ENABLED)
    CRON_SCHEDULE=$(ini_key "$APP_INI" cron.cleanup_packages SCHEDULE)
    CRON_OLDER=$(ini_key "$APP_INI" cron.cleanup_packages OLDER_THAN)
    [ -n "$CRON_SCHEDULE" ] && [ -n "$CRON_OLDER" ] \
        || CRON_NOTE="unset in app.ini, so Forgejo's built-in default applies (documented as SCHEDULE @midnight, OLDER_THAN 24h; not read from anything here)"
fi
case "$CRON_SOURCE" in
    unread:*) CRON_SENTENCE="[cron.cleanup_packages] cadence $CRON_SOURCE" ;;
    *) CRON_SENTENCE="[cron.cleanup_packages] per $APP_INI: schedule ${CRON_SCHEDULE:-unset}, older_than ${CRON_OLDER:-unset}${CRON_ENABLED:+, enabled $CRON_ENABLED}${CRON_NOTE:+; $CRON_NOTE}" ;;
esac
say "cleanup: $CRON_SENTENCE"

# --- the deletes (--for-real only): tags first, then children ---------------
# Tags first so that a run that dies part-way leaves untagged children
# (collectable as orphans next pass) rather than a tag whose index
# points at nothing. Every DELETE is judged by its code: 204 is gone,
# 404 was already gone, 403 is the scope refusal that ends the run
# before a second attempt, anything else stops the run with the count
# so far on the record.
: > "$TMP/deleted.tsv"   # name<TAB>version<TAB>outcome (deleted|gone|failed)
REFUSED_SCOPE=""
FAIL_CODE=""
WRITE_SCOPE="unmeasured: a dry run cannot prove write:package without deleting"
if [ "$DRY" = 0 ]; then
    WRITE_SCOPE="unmeasured: nothing to delete"
    { awk -F'\t' '$3 == "delete" && $2 !~ /^sha256:/' "$TMP/classes.tsv"
      awk -F'\t' '$3 == "delete" && $2 ~ /^sha256:/' "$TMP/classes.tsv"; } > "$TMP/todo.tsv"
    # Each outcome reaches the list file AS ITS DELETE ANSWERS (backlog
    # 8d77d670, review L2): a run the runner kills at its timeout leaves
    # the file saying what went, not only what was planned. Best effort:
    # the deletes are the act, and the record's counts are the verdict.
    LIST_APPEND_OK=1
    outcome() { # <name> <ver> <deleted|gone|failed>
        printf '%s\t%s\t%s\n' "$1" "$2" "$3" >> "$TMP/deleted.tsv"
        [ "$LIST_APPEND_OK" = 1 ] || return 0
        printf '%s %s:%s\n' "$3" "$1" "$2" >> "$LIST_FILE" 2>/dev/null || {
            LIST_APPEND_OK=0
            say "list: the outcomes could not be appended to $LIST_FILE; the record's counts stand"
        }
    }
    while IFS=$'\t' read -r name ver cls reason; do
        code=$(http "$BASIC_CFG" DELETE "$FORGE_URL/api/v1/packages/$OWNER/container/$name/$ver" "$TMP/delete.json")
        case "$code" in
            204) outcome "$name" "$ver" deleted; WRITE_SCOPE="write:package proven by DELETE" ;;
            404) outcome "$name" "$ver" gone; say "DELETE $name:$ver answered 404 — already gone" ;;
            403)
                REFUSED_SCOPE=$(scope_of "$TMP/delete.json" write:package)
                say "REFUSED — DELETE $name:$ver answered 403: the docker login for $REG_HOST (user $CRED_USER) lacks $REFUSED_SCOPE; the forge said:"
                jq -r '.message // empty' "$TMP/delete.json" 2>/dev/null | scrub | sed 's/^/    /' >&2
                say "  Minting a token with read:package and $REFUSED_SCOPE is a root ceremony, David's; then docker login $REG_HOST with it as the checkout owner. No further DELETE was attempted."
                break ;;
            *)
                FAIL_CODE="$code"
                outcome "$name" "$ver" failed
                say "FAILED — DELETE $name:$ver answered $code, not 204; stopping here:"
                { scrub < "$TMP/curl.err"; head -c 400 "$TMP/delete.json" | scrub; } | sed 's/^/    /' >&2
                break ;;
        esac
    done < "$TMP/todo.tsv"
fi

# --- the re-list: a DELETE's answer is the forge's claim, not its effect ----
# Review finding 1 of backlog 1bef55a6: counting a 404 as done let a run
# whose EVERY DELETE answered 404 — a moved Forgejo route answers that to
# everything — say `OK — deleted 8 of 8` and exit 0 while nothing went.
# So a real run that sent its whole plan lists the registry again and
# names every planned version still in it: STILL (still.tsv). The
# verdict's count is planned minus still listed; a survivor fails the
# run, and so does a run whose every DELETE answered 404. A re-list that
# cannot be read fails the run too — the deletes cannot be shown to
# have happened. A run that stopped part-way already fails, so it is not
# re-listed.
: > "$TMP/still.tsv"   # name<TAB>version, planned and still listed
RELIST_WHY=""
RELISTED=0
if [ "$DRY" = 0 ] && [ -z "$REFUSED_SCOPE" ] && [ -z "$FAIL_CODE" ] && [ -s "$TMP/todo.tsv" ]; then
    # The re-list is judged the way the first listing is, and then some
    # (the re-review of 1bef55a6, finding 2): an EMPTY or SHORT re-list
    # holds none of the planned versions, so without these it read
    # `OK — deleted 8 of 8` with nothing deleted.
    #   * it must be WHOLE: a numeric X-Total-Count, and every version it
    #     counts read;
    #   * its POSITIVE CONTROL: every version this run KEPT (the keep
    #     set and the unclassified — nothing deleted them) must be listed
    #     again. A known-kept version missing means the re-list does not
    #     show the registry, and its silence about the planned ones
    #     proves nothing.
    # Any of them fails the run; the awk joins' exits are read too.
    if ! list_versions "$TMP/after.raw"; then
        RELIST_WHY="$L_WHY"
    elif case "$L_TOTAL" in ''|*[!0-9]*) true ;; *) false ;; esac; then
        RELIST_WHY="the re-list came with no numeric X-Total-Count, so it cannot be shown whole"
    elif [ "$L_LISTED" -lt "$L_TOTAL" ]; then
        RELIST_WHY="the re-list read $L_LISTED version(s) of the $L_TOTAL the forge counts"
    elif ! awk -F'\t' 'FILENAME == ARGV[1] { listed[$1 "\t" $2] = 1; next }
                       $3 != "delete" && !(($1 "\t" $2) in listed) { print $1 ":" $2 }' \
            "$TMP/after.raw" "$TMP/classes.tsv" > "$TMP/kept-missing.txt"; then
        RELIST_WHY="the re-list could not be compared with the versions this run kept"
    elif [ -s "$TMP/kept-missing.txt" ]; then
        RELIST_WHY="$(grep -c . "$TMP/kept-missing.txt") version(s) this run KEPT are missing from the re-list (first: $(awk 'NR == 1' "$TMP/kept-missing.txt")), so it does not show the registry"
    elif ! awk -F'\t' 'FILENAME == ARGV[1] { listed[$1 "\t" $2] = 1; next }
                       ($1 "\t" $2) in listed { print $1 "\t" $2 }' "$TMP/after.raw" "$TMP/todo.tsv" > "$TMP/still.tsv"; then
        RELIST_WHY="the re-list could not be compared with the planned versions"
    else
        RELISTED=1
        say "re-list: $L_LISTED of $L_TOTAL version(s) listed after the deletes, every kept version among them; $(grep -c . "$TMP/still.tsv" || true) planned version(s) still listed"
    fi
    [ -z "$RELIST_WHY" ] || say "re-list: the registry cannot be shown after the deletes — $RELIST_WHY"
fi
DF_AFTER=null
[ "$DRY" = 0 ] && DF_AFTER=$(disk_avail_kb)

# --- the record ---------------------------------------------------------------
# Per package: versions, kept (by reason), delete (planned), deleted /
# gone / failed (done), unclassified, index_unreadable. Bytes are not
# declared by the packages API (measured: size null), so `declared_bytes`
# is null and `df` carries the measurement.
PACKAGES_JSON=$(jq -n -R -c --rawfile classes "$TMP/classes.tsv" --rawfile done "$TMP/deleted.tsv" --rawfile unreadable "$TMP/unreadable.tsv" --arg pkgs "$PACKAGES" '
    def rows(s): [ s | split("\n")[] | select(length > 0) | split("\t") ];
    (rows($classes)) as $c | (rows($done)) as $d | (rows($unreadable)) as $u
    | [ ($pkgs | split(" "))[] | . as $n
        | { key: $n, value: {
            versions: ([ $c[] | select(.[0] == $n) ] | length),
            kept: ([ $c[] | select(.[0] == $n and .[2] == "keep") ] | length),
            kept_by: ([ $c[] | select(.[0] == $n and .[2] == "keep") | .[3] | sub(" \\(.*\\)$"; "") ] | group_by(.) | map({ (.[0]): length }) | add // {}),
            delete: ([ $c[] | select(.[0] == $n and .[2] == "delete") ] | length),
            deleted: ([ $d[] | select(.[0] == $n and .[2] == "deleted") ] | length),
            gone: ([ $d[] | select(.[0] == $n and .[2] == "gone") ] | length),
            failed: ([ $d[] | select(.[0] == $n and .[2] == "failed") ] | length),
            unclassified: ([ $c[] | select(.[0] == $n and .[2] == "unclassified") ] | length),
            index_unreadable: ([ $u[] | select(.[0] == $n) ] | length) } } ]
    | from_entries')
LIVE_JSON=$(jq -n -R -c --rawfile live "$TMP/live.txt" '$live | split("\n") | map(select(length > 0))')
DELETED_TOTAL=$(awk -F'\t' '$3 == "deleted"' "$TMP/deleted.tsv" | grep -c . || true)
# What the run did, read off the registry rather than off the DELETEs'
# answers (review finding 1). A 404 alone is a version someone else
# deleted first, and counting it apart made a concurrent delete read
# `deleted 311 of 312` and file a false alarm (backlog 1bef55a6, LOW) —
# but a 404 is the forge's CLAIM, and a moved route answers it to every
# DELETE. So DONE is the planned versions the re-list no longer holds;
# `deleted` (204) and `gone` (404) stay two numbers in the record.
GONE_TOTAL=$(awk -F'\t' '$3 == "gone"' "$TMP/deleted.tsv" | grep -c . || true)
PLANNED_TOTAL=$(awk -F'\t' '$3 == "delete"' "$TMP/classes.tsv" | grep -c . || true)
STILL_TOTAL=$(grep -c . "$TMP/still.tsv" || true)
DONE_TOTAL=$((PLANNED_TOTAL - STILL_TOTAL))
UNCLASSIFIED_TOTAL=$(awk -F'\t' '$3 == "unclassified"' "$TMP/classes.tsv" | grep -c . || true)
jq -n -c \
    --arg verb "$ME" --argjson dry "$( [ "$DRY" = 1 ] && echo true || echo false )" \
    --arg registry "$REG_HOST" --arg owner "$OWNER" --arg pkgs "$PACKAGES" \
    --arg live_main "$LIVE_MAIN" --argjson live "$LIVE_JSON" --arg stamp "$STAMP" \
    --argjson landed "$LANDED_COUNT" --argjson open "$OPEN_COUNT" --argjson keep_trains "$KEEP_TRAINS" --argjson lookback "$LOOKBACK" \
    --arg ceiling "$CEILING" \
    --argjson max_delete "$MAX_DELETE" --argjson max_percent "$MAX_PERCENT" --arg ceiling_source "$CEILING_SOURCE" \
    --argjson bearer "$( [ "$INDEX_OK" = 1 ] && echo true || echo false )" \
    --argjson tags "$TAGS_TOTAL" --argjson read "$INDEX_READ" --argjson read_pct "$READ_PCT" \
    --arg index_short "$INDEX_SHORT" \
    --argjson relisted "$( [ "$RELISTED" = 1 ] && echo true || echo false )" \
    --argjson still "$STILL_TOTAL" --arg relist_why "$RELIST_WHY" \
    --argjson hours "$KEEP_HOURS" --argjson listed "$LISTED" --arg total "${TOTAL:-}" \
    --argjson packages "$PACKAGES_JSON" --argjson planned "$PLANNED_TOTAL" --argjson deleted "$DELETED_TOTAL" \
    --argjson gone "$GONE_TOTAL" \
    --arg write_scope "$WRITE_SCOPE" --arg refused "$REFUSED_SCOPE" --arg fail "$FAIL_CODE" \
    --argjson df_before "$DF_BEFORE" --argjson df_after "$DF_AFTER" --arg df_path "$DF_PATH" \
    --arg cron_source "$CRON_SOURCE" --arg cron_enabled "$CRON_ENABLED" --arg cron_schedule "$CRON_SCHEDULE" \
    --arg cron_older "$CRON_OLDER" --arg cron_note "$CRON_NOTE" \
    --arg list_file "$LIST_FILE" --arg at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" '
    def or_null: if . == "" then null else . end;
    { verb: $verb, dry_run: $dry, registry: $registry, owner: $owner, packages_read: ($pkgs | split(" ")),
      keep: { live_main: $live_main, live: $live, stamp: $stamp, landed_trains: $landed,
              open_trains: $open,
              keep_trains: $keep_trains, trains_read: $lookback, latest: true, newer_than_hours: $hours },
      listed: $listed, forge_total: (if $total == "" then null else ($total | tonumber? // $total) end),
      index: { bearer: $bearer, tags: $tags, read: $read, read_pct: $read_pct,
               short: ($index_short | or_null) },
      relist: { done: $relisted, still_listed: $still, failed: ($relist_why | or_null) },
      ceiling: { max_delete: $max_delete, max_percent: $max_percent, source: $ceiling_source,
                 exceeded: ($ceiling | or_null) },
      packages: $packages, planned: $planned, deleted: $deleted, gone: $gone,
      declared_bytes: null,
      bytes_note: "the packages API declares no sizes (files[].size is null, Forgejo 16.0.2); blobs are freed by Forgejo [cron.cleanup_packages] once unreferenced (cleanup_cron, read off app.ini) — read df on the packet after it has run",
      disk_path: $df_path, disk_avail_kb_before: $df_before, disk_avail_kb_after: $df_after,
      cleanup_cron: ({ source: $cron_source, enabled: ($cron_enabled | or_null),
                       schedule: ($cron_schedule | or_null), older_than: ($cron_older | or_null) }
                     + (if $cron_note == "" then {} else { note: $cron_note } end)),
      list_file: $list_file, write_scope: $write_scope }
    + (if $refused == "" then {} else { refused: $refused } end)
    + (if $fail == "" then {} else { failed_http: $fail } end)
    + { at: $at }'

# --- the verdict, then the list ------------------------------------------------
# The verdict is one line right after the record; the per-version list
# (the file's contents: the plan, and after a real run the outcomes)
# comes LAST, so the runner's window holds the record and the verdict
# whatever the list's length (backlog 5323f3ef).
RC=0
# What the pass could judge, in the shape the daily watch captures
# (watch-prune-registry-versions-daily, v2): `listed <l> of <t>
# versions, indexes read … , <read_pct> percent; <unclassified>
# unclassified kept`. No parentheses and no em-dash between the numbers —
# the pattern crosses two lexers and admits no backslash. TOTAL is here
# only because it was proven digits before anything printed it.
JUDGED="listed $LISTED of $TOTAL versions, indexes read $INDEX_READ of $TAGS_TOTAL tag(s), $READ_PCT percent; $UNCLASSIFIED_TOTAL unclassified kept"
if [ -n "$INDEX_SHORT" ]; then
    # Before the dry-run branch too: a plan built from too few indexes is
    # not a plan, in either mode (backlog 1bef55a6 (1), review finding 2).
    if [ "$INDEX_READ" -le 0 ]; then
        say "FAILED — no manifest index could be read (indexes read 0 of $TAGS_TOTAL tag(s): $INDEX_SHORT), so every tag's children are unknown and every orphan is held: this pass classified nothing, and $DELETED_TOTAL of $PLANNED_TOTAL planned were deleted. A pass that can judge nothing is not a pass — the registry grows every night it repeats. $UNCLASSIFIED_TOTAL version(s) are kept unclassified, listed below."
    else
        say "FAILED — too few manifest indexes could be read ($INDEX_SHORT; indexes read $INDEX_READ of $TAGS_TOTAL tag(s)), so their tags and every orphan are held: this pass judged too little of the registry, and $DELETED_TOTAL of $PLANNED_TOTAL planned were deleted. $UNCLASSIFIED_TOTAL version(s) are kept unclassified, listed below."
    fi
    RC=1
elif [ "$DRY" = 1 ]; then
    say "DRY RUN — would delete $PLANNED_TOTAL version(s) across $PACKAGES; the keep set held $(grep -c . "$TMP/keys.tsv" || true) sha(s), latest, and $KEEP_HOURS h; $JUDGED. Nothing was deleted."
elif [ -n "$REFUSED_SCOPE" ]; then
    say "  $DELETED_TOTAL of $PLANNED_TOTAL deleted before the refusal. Nothing was deleted."
    RC=2
elif [ -n "$FAIL_CODE" ]; then
    say "  deleted $DELETED_TOTAL of $PLANNED_TOTAL planned before the failure (HTTP $FAIL_CODE); the rest stay for the next pass."
    RC=1
elif [ "$PLANNED_TOTAL" -gt 0 ] && [ "$GONE_TOTAL" -eq "$PLANNED_TOTAL" ]; then
    say "FAILED — every one of the $PLANNED_TOTAL planned DELETEs answered 404, which is what a moved or broken route answers to everything; a 404 is the forge's claim, not an effect, so nothing is shown deleted (deleted 0, gone $GONE_TOTAL; the re-list still holds $STILL_TOTAL of them)."
    RC=1
elif [ -n "$RELIST_WHY" ]; then
    say "FAILED — the registry could not be listed again after the deletes ($RELIST_WHY), so none of the $PLANNED_TOTAL planned versions can be shown gone (DELETE answered 204 for $DELETED_TOTAL, 404 for $GONE_TOTAL)."
    RC=1
elif [ "$STILL_TOTAL" -gt 0 ]; then
    say "FAILED — the re-list after the deletes still holds $STILL_TOTAL of the $PLANNED_TOTAL planned version(s) (named STILL LISTED below), whatever their DELETEs answered (204 for $DELETED_TOTAL, 404 for $GONE_TOTAL); $DONE_TOTAL are shown gone."
    RC=1
else
    say "OK — deleted $DONE_TOTAL of $PLANNED_TOTAL planned version(s) across $PACKAGES, $( [ "$RELISTED" = 1 ] && echo "shown gone by a re-list" || echo "nothing to re-list" ), $GONE_TOTAL of them already gone (404); $JUDGED; df $DF_PATH before ${DF_BEFORE} KB, after ${DF_AFTER} KB free. The blobs are Forgejo's to free — $CRON_SENTENCE — so the difference lands when that runs (measured 2026-09-17: 724 KB on 1,337 deletions, then the cron); read df on this packet, not a claim here."
fi
# A REAL RUN'S LINE CARRIES ITS OUTCOME AS THE VERB (backlog ea67ad87,
# measured on the first real prune, ops-request 973beaa2, 2026-09-17
# 13:41Z, 1,337 of 1,337 deleted): the file was printed as-is and the
# outcomes appended after it, so the 898 lines the runner kept all read
# `would DELETE` under a verdict that said OK — a real run described in
# the conditional. Now the plan and the outcomes are joined here, by
# name, before anything is printed: DELETED (204) / GONE (404) / FAILED
# <code> / KEPT (never attempted because the run stopped, or
# unclassified). Only a dry run prints `would DELETE`. The file keeps
# the plan and the appended outcomes, unchanged.
outcome_lines() { # --for-real: one line per planned version, then per unclassified one
    # A planned version the re-list still holds reads STILL LISTED, with
    # what its DELETE answered — the answer is not the effect (review
    # finding 1).
    awk -F'\t' -v fail="$FAIL_CODE" '
        FILENAME == ARGV[1] { still[$1 "\t" $2] = 1; next }
        FILENAME == ARGV[2] { done[$1 "\t" $2] = $3; next }
        $3 == "delete" {
            o = done[$1 "\t" $2]
            if (($1 "\t" $2) in still) printf "STILL LISTED %s:%s — %s (its DELETE answered %s)\n", $1, $2, $4, (o == "deleted" ? "204" : (o == "gone" ? "404" : "nothing"))
            else if (o == "deleted")     printf "DELETED %s:%s — %s\n", $1, $2, $4
            else if (o == "gone")   printf "GONE %s:%s — %s (404: already gone)\n", $1, $2, $4
            else if (o == "failed") printf "FAILED %s %s:%s — %s\n", fail, $1, $2, $4
            else                    printf "KEPT %s:%s — %s (not attempted: the run stopped)\n", $1, $2, $4
        }
        $3 == "unclassified" { printf "KEPT %s:%s — unclassified: %s\n", $1, $2, $4 }
    ' "$TMP/still.tsv" "$TMP/deleted.tsv" "$TMP/classes.tsv"
}
if [ "$DRY" = 1 ]; then
    say "list: every line of $LIST_FILE follows; the runner may cut it, the file is whole"
    sed "s/^/$ME: /" "$LIST_FILE" >&2
else
    say "list: every planned version follows with its outcome as the verb (DELETED / GONE / STILL LISTED / FAILED <code> / KEPT); the runner may cut it, $LIST_FILE is whole"
    { head -n 1 "$LIST_FILE"; outcome_lines; } | sed "s/^/$ME: /" >&2
fi
exit "$RC"
