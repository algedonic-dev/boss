#!/usr/bin/env bash
# upgrade-forgejo.sh — upgrade the forge's Forgejo to a newer release,
# through a rendered plan a passkey signs. The ops verbs
# `plan-a-forgejo-upgrade` (--plan) and `upgrade-forgejo` (the write).
#
#   upgrade-forgejo.sh --plan <version>
#   upgrade-forgejo.sh <version> <plan-sha256>
#
# WHY IT EXISTS (backlog cecf62b3, filed by the 2026-10-18 command-line
# gap inventory). There was no runbook and no verb for upgrading
# Longhorn, Kubernetes or Forgejo, and no upgrade appeared in the record:
# each would have been a hand session. David, 2026-10-18: every infra act
# is a website-approved verb. Forgejo is the first of the three because
# its install path is the one the tree already reads end to end — ONE
# container on the forge's system docker daemon, declared by ONE compose
# file (/opt/forgejo/docker-compose.yml, forge-repo-path.sh's reading),
# whose image line is the whole of "which version runs"; a nightly
# verified `forgejo dump` already exists (forge-backup.sh); and the
# move-forgejo-data family already stops, starts and proves this
# container from the ops runner, which owes nothing to Forgejo while it
# runs. Longhorn has no install declaration in the tree at all, and a
# Kubernetes upgrade moves the API the runner reports through.
#
# THE SHAPE is design 17835005's: the verb file declares
# requires_approval and names plan-a-forgejo-upgrade as its plan_verb, so
# the runner renders this script's --plan onto the request's approve
# step, runs the write only after a passkey on the approvers list signs
# those bytes, and hands it sha256 of the SIGNED plan. The write
# re-renders — every check below runs again — and changes nothing unless
# today's plan hashes to the signed one.
#
# THE PLAN carries the VERSION DIFF and the HEALTH PRECONDITIONS:
#   * from: the compose file's one image line, codeberg.org/forgejo/
#     forgejo:<X.Y.Z>; the one container compose lists for the service,
#     running that very image (its id, and the tag's local id, agree);
#     and Forgejo's own /api/v1/version saying X.Y.Z;
#   * to: the version the packet names (digits only), strictly newer and
#     at most one major release ahead — each major's breaking changes are
#     what the approver reads, so they are read one major at a time —
#     pulled now and named by its image id and registry digest, so the
#     passkey signs bytes, not a tag a registry could move;
#   * health: /api/healthz passes, and no CI job container
#     (FORGEJO-ACTIONS-*, the runner's own naming, reap-dead-ci-jobs.sh)
#     is running — a restart under a running job kills it, and a job
#     killed that way reads as a red train that strikes its cars;
#   * rollback target: the image it runs now (tag and id), the compose
#     file as it reads now (kept beside it as .pre-upgrade-<X.Y.Z>), and
#     the forgejo dump this run takes before anything changes. Forgejo
#     migrates its database on the first start of a newer release and
#     does not run an older one against it, so going back is that compose
#     file AND a restore of the dump's forgejo-db.sql — said on the plan
#     in those words, because a rollback target that silently omits the
#     database is the one that restores into a broken forge.
# No clock, no StartedAt and no free space in the bytes: two renders of
# one true state are byte-identical. After a run the container runs the
# new release, so the same approval re-renders as "not newer" and is
# refused: at most once.
#
# EACH STEP RE-READS THE PRECONDITIONS (the item's acceptance). The
# health reads — Forgejo running the plan's image, healthz passing, no
# CI job — plus no open train/gate, running converge or standing
# converge hold — are taken by the render, immediately before the dump,
# again after the dump, and again
# immediately before the compose file is swapped; any that no longer
# holds stops the run with Forgejo untouched (exit 78).
#
# THE WRITE, in order: re-render and compare; take the dump with
# forge-backup.sh — the nightly timer's one definition, so its floor, its
# ceiling and its verification are the same ones (CLAUDE.md §9a) — and
# name the archive; re-read; keep the compose file as
# <compose>.pre-upgrade-<from>; write the new compose file beside it,
# changing that ONE line, and hold it to compose's own reading (the
# service's image is the target, and nothing else in the file moved);
# re-read; rename it into place; `docker compose up -d --no-deps
# --pull never <service>`, which recreates the one container on the
# image the plan pulled and pulls nothing else.
#
# THE EFFECT IS READ BACK. Every BOSS_UPGRADE_POLL_S (default 5) up to
# BOSS_UPGRADE_WAIT_S (default 600 — a release's migrations run on its
# first start): the service's one container, running the target's image
# id; /api/healthz passing; /api/v1/version saying the target; and
# refs/heads/main inside the container holding the main read before the
# swap (equal, or a descendant a merge added). Only when all four hold on
# TWO consecutive reads is the effect line printed. Otherwise exit 1,
# naming what each read said and the rollback target. Nothing is undone
# by guess after the swap: the database may already be migrated.
#
# Exit 78 is a refusal (nothing changed but, after the hash held, a
# verified dump); exit 1 a step that failed or a read that could not look.
set -uo pipefail

ME=upgrade-forgejo
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

say() { echo "$ME: $*"; }
refuse() { echo "$ME: REFUSED — $*" >&2; exit 78; }
fail() { echo "$ME: FAILED — $*" >&2; exit 1; }

VERSION_RE='^[0-9]{1,4}\.[0-9]{1,4}\.[0-9]{1,4}$'
IMAGE_REPO="codeberg.org/forgejo/forgejo"

PLAN=0
if [ "${1-}" = "--plan" ]; then
    PLAN=1
    shift
    [ $# -eq 1 ] || refuse "usage: upgrade-forgejo.sh --plan <version> — the plan takes the target release and nothing else"
    TARGET="$1"
else
    [ $# -eq 2 ] || refuse "usage: upgrade-forgejo.sh <version> <plan-sha256> — this upgrades only to an APPROVED plan, and the hash is what the approval signed. Render one with --plan (the plan-a-forgejo-upgrade verb)"
    TARGET="$1"
    [[ "$2" =~ ^[0-9a-f]{64}$ ]] || refuse "the plan hash must be 64 lowercase hex characters, got '$2'"
    APPROVED="$2"
fi
[[ "$TARGET" =~ $VERSION_RE ]] || refuse "the target must be a release number X.Y.Z, digits only (no pre-release, no 'latest'), got '$TARGET'"
command -v jq > /dev/null 2>&1 || fail "jq is not on PATH — compose's reading cannot be judged, and no evidence is not a plan"

SERVICE="${BOSS_FORGE_SERVICE:-forgejo}"
WAIT_S="${BOSS_UPGRADE_WAIT_S:-600}"
POLL_S="${BOSS_UPGRADE_POLL_S:-5}"
PULL_S="${BOSS_UPGRADE_PULL_S:-300}"
BACKUP="${BOSS_UPGRADE_BACKUP:-$HERE/forge-backup.sh}"
BACKUP_DIR="${BOSS_FORGE_BACKUP_DIR:-/var/backups/boss/forge}"
case "${WAIT_S:-empty}${POLL_S:-empty}${PULL_S:-empty}" in
    *[!0-9]*) fail "BOSS_UPGRADE_WAIT_S, BOSS_UPGRADE_POLL_S and BOSS_UPGRADE_PULL_S must be whole seconds" ;;
esac
# Forgejo runs on the SYSTEM daemon; a DOCKER_HOST inherited from the
# converge's rootless one answers "no such container" about a running
# one (forge-backup.sh, move-forgejo-data.sh).
export DOCKER_HOST="${BOSS_UPGRADE_DOCKER_HOST:-unix:///var/run/docker.sock}"
[ -n "${BOSS_FORGE_URL:-}" ] || refuse "BOSS_FORGE_URL is not set (/etc/boss/sor.env), so Forgejo's healthz and version have no address"

# The compose file and the repository it serves: forge-repo-path.sh is
# the one reading of both.
# shellcheck source=infra/forge/forge-repo-path.sh
. "$HERE/forge-repo-path.sh"
# shellcheck source=infra/forge/forge-defaults.sh
. "$HERE/forge-defaults.sh"
# shellcheck source=infra/lib/jq.sh
. "$HERE/../lib/jq.sh" || exit 1
# shellcheck source=infra/lib/sor.sh
. "$HERE/../lib/sor.sh"
# shellcheck source=infra/lib/secret-header.sh
. "$HERE/../lib/secret-header.sh"
SYSTEMCTL="${BOSS_SYSTEMCTL:-systemctl}"
COMPOSE="$FORGE_COMPOSE"
SAVED="$COMPOSE.pre-upgrade"

WORK="$(mktemp -d)" || fail "cannot make a scratch directory"
NEWFILE=""
trap 'rm -rf "$WORK"; [ -n "$NEWFILE" ] && rm -f "$NEWFILE"' EXIT

# ---- reads -------------------------------------------------------------------
healthz() { # -> pass, or what it said
    local code s
    code=$(curl -q -sS --max-time 10 -o "$WORK/healthz" -w '%{http_code}' "$BOSS_FORGE_URL/api/healthz" 2> "$WORK/curl.err") \
        || { printf 'unreachable: %s' "$(tr '\n' ' ' < "$WORK/curl.err" | cut -c1-160)"; return 0; }
    s=$(jq -r '.status // empty' "$WORK/healthz" 2> /dev/null)
    if [ "$code" = 200 ] && [ "$s" = pass ]; then printf 'pass'; else printf 'HTTP %s status %s' "$code" "${s:-unreadable}"; fi
}
served_version() { # -> X.Y.Z as Forgejo's own /api/v1/version says it, or what it said
    local code v
    code=$(curl -q -sS --max-time 10 -o "$WORK/version" -w '%{http_code}' "$BOSS_FORGE_URL/api/v1/version" 2> "$WORK/curl.err") \
        || { printf 'unreachable: %s' "$(tr '\n' ' ' < "$WORK/curl.err" | cut -c1-160)"; return 0; }
    v=$(jq -r '.version // empty' "$WORK/version" 2> /dev/null)
    # The live forge returned 16.0.2+gitea-1.22.0 without a token on 2026-11-10.
    # The release is before the +.
    v="${v%%+*}"
    if [ "$code" = 200 ] && [[ "$v" =~ $VERSION_RE ]]; then printf '%s' "$v"; else printf 'HTTP %s version %s' "$code" "${v:-unreadable}"; fi
}
the_container() { # -> the service's one container id; rc 1 with the reason
    docker compose -f "$COMPOSE" ps -a -q "$SERVICE" > "$WORK/cid" 2> "$WORK/docker.err" \
        || { printf 'compose could not list %s: %s' "$SERVICE" "$(head -c 200 "$WORK/docker.err")"; return 1; }
    [ "$(grep -c . "$WORK/cid")" = 1 ] \
        || { printf '%s has %s containers, not exactly one' "$SERVICE" "$(grep -c . "$WORK/cid")"; return 1; }
    awk 'NR == 1' "$WORK/cid"
}
ci_jobs() { # -> the names of running CI job containers, one a line; rc 1 when docker cannot say
    docker ps --filter status=running --filter name=FORGEJO-ACTIONS --format '{{.Names}}' 2> "$WORK/docker.err"
}
main_inside() { # <container> -> refs/heads/main as git inside it reads it
    local s
    s=$(docker exec -u git "$1" git --git-dir="$REPO_IN_CONTAINER" rev-parse --verify --quiet refs/heads/main 2> "$WORK/git.err") || return 1
    [[ "$s" =~ ^[0-9a-f]{40,64}$ ]] || return 1
    printf '%s' "$s"
}
# The compose file's image line for the service, read two ways that must
# agree: compose's own reading (what `up` will run) and the raw file
# (the one line the write changes).
compose_image() {
    docker compose -f "$1" config --format json > "$WORK/config.json" 2> "$WORK/docker.err" \
        || { printf 'docker compose could not read %s: %s' "$1" "$(head -c 300 "$WORK/docker.err")"; return 1; }
    jq -r --arg s "$SERVICE" '.services[$s].image // empty' "$WORK/config.json"
}
image_lines() { # <file> <ref> -> how many lines declare exactly that image
    local re
    re=$(printf '%s' "$2" | sed 's/[.]/[.]/g')
    grep -cE "^[[:space:]]*image:[[:space:]]*[\"']?${re}[\"']?[[:space:]]*$" "$1"
}

# Absence requires the named, all-scope read door and a typed zero
# listing. A failed, missing or truncated listing is not an idle yard.
pipeline_idle() {
    local kind unit state load count
    [ -d "$(dirname -- "$HOLD_FILE")" ] && [ -x "$(dirname -- "$HOLD_FILE")" ] \
        || refuse "cannot inspect converge hold directory ($1)"
    [ ! -e "$HOLD_FILE" ] && [ ! -L "$HOLD_FILE" ] \
        || refuse "a converge hold stands at $HOLD_FILE ($1) — do not upgrade under another operation's hold"
    unit=$(timeout -k 1 10 "$SYSTEMCTL" show -p LoadState -p ActiveState cluster-deploy-runner.service 2> "$WORK/unit.err") \
        || refuse "cannot read cluster-deploy-runner.service ($1): $(cat "$WORK/unit.err")"
    load=$(printf '%s\n' "$unit" | sed -n 's/^LoadState=//p')
    state=$(printf '%s\n' "$unit" | sed -n 's/^ActiveState=//p')
    [ "$load" = loaded ] || refuse "cluster-deploy-runner.service is not loaded ($1): $unit"
    case "$state" in
        inactive|failed) ;;
        *) refuse "cluster-deploy-runner.service is $state, not idle ($1)" ;;
    esac
    [ -n "${BOSS_JOBS_URL:-}" ] || refuse "BOSS_JOBS_URL is missing ($1)"
    machine_token_header MT_HDR "$BOSS_JOBS_URL" && [ -n "$MT_HDR" ] \
        || refuse "the system-of-record machine credential is unavailable ($1)"
    local filter reader
    reader=$(sor_reader_header 'automation:upgrade-forgejo-reader')
    for kind in pr-train gate-run; do
        for filter in '&status=open' ''; do
            if ! curl -q -fsS --connect-timeout 5 --max-time 10 -H "$MT_HDR" -H "x-boss-user: $reader" \
                "${BOSS_JOBS_URL%/}/api/jobs?kind=$kind$filter&limit=1&full=false" > "$WORK/jobs.json" 2> "$WORK/jobs.err"; then
                refuse "cannot read $kind packets ($1, filter '$filter'): $(cat "$WORK/jobs.err")"
            fi
            jq_doc_file "$WORK/jobs.json" || refuse "$kind listing has no JSON document ($1, filter '$filter')"
            count=$(jq -ers '
                if length != 1 then error("expected one document") else .[0] end |
                if type == "object" and (.total | type) == "number" and (.data | type) == "array"
                    and .total >= 0 and .total == (.total | floor) and ([.total, 1] | min) == (.data | length)
                then .total else error("not a typed listing") end
                ' "$WORK/jobs.json" 2> "$WORK/jobs-jq.err") \
                || refuse "$kind listing is malformed or incomplete ($1, filter '$filter'): $(cat "$WORK/jobs-jq.err")"
            if [ -n "$filter" ]; then
                [ "$count" = 0 ] || refuse "$count open $kind packet(s) are present ($1) — upgrade between trains and gates; sample packet ID: $(jq -r '.data[0].id' "$WORK/jobs.json")"
            else
                [ "$count" != 0 ] || refuse "the system of record returned no $kind history ($1); cannot establish that the open count is authoritative"
            fi
        done
    done
}

# preconditions <when> — the health half of the plan, read at every step.
# Prints the lines the plan carries; refuses when one no longer holds.
preconditions() {
    local cid img ids h jobs
    cid=$(the_container) || refuse "$cid ($1)"
    docker inspect -f '{{.State.Running}} {{.Image}}' "$cid" > "$WORK/state" 2> "$WORK/docker.err" \
        || fail "docker could not read $SERVICE's container $cid ($1): $(head -c 300 "$WORK/docker.err")"
    read -r running img < "$WORK/state"
    [ "$running" = true ] || refuse "$SERVICE's container $cid is not running ($1) — a stopped forge is restart-forgejo's to start, not an upgrade's"
    [ -z "${FROM_ID:-}" ] || [ "$img" = "$FROM_ID" ] \
        || refuse "$SERVICE's container runs image $img ($1), not $FROM_ID, the one the plan read — something changed Forgejo since"
    h=$(healthz)
    [ "$h" = pass ] || refuse "Forgejo's /api/healthz does not pass ($1): $h — an unhealthy forge is diagnosed first, never upgraded"
    ids=$(ci_jobs) || fail "docker could not list running containers ($1): $(head -c 300 "$WORK/docker.err")"
    jobs=0
    [ -z "$ids" ] || jobs=$(printf '%s\n' "$ids" | wc -l | tr -d ' ')
    [ "$jobs" -eq 0 ] \
        || refuse "$jobs CI job container(s) are running ($1): $(printf '%s' "$ids" | tr '\n' ' ') — restarting Forgejo under a job kills it, and that reads as a red train. Upgrade between trains"
    pipeline_idle "$1"
    CID="$cid"
    echo "container: $SERVICE ($cid) running image $img"
    echo "healthz: pass"
    echo "ci jobs running: 0"
    echo "open trains: 0; open gates: 0; converge: idle; converge hold: absent"
}

# ---- the plan ----------------------------------------------------------------
render() {
    local from raw_lines tag_id served major_from major_to target_id digest
    docker compose version > /dev/null 2> "$WORK/docker.err" \
        || refuse "docker compose does not answer ($(head -c 300 "$WORK/docker.err"))"
    from=$(compose_image "$COMPOSE") || refuse "$from"
    [ -n "$from" ] || refuse "$COMPOSE declares no image for a service named $SERVICE"
    [[ "$from" == "$IMAGE_REPO":* ]] && [[ "${from#"$IMAGE_REPO":}" =~ $VERSION_RE ]] \
        || refuse "$SERVICE's image reads '$from', not $IMAGE_REPO:<X.Y.Z> — this verb upgrades only the release image by its release tag"
    FROM_VERSION="${from#"$IMAGE_REPO":}"
    FROM_REF="$from"
    raw_lines=$(image_lines "$COMPOSE" "$FROM_REF")
    [ "$raw_lines" = 1 ] \
        || refuse "$COMPOSE carries $raw_lines line(s) declaring image $FROM_REF, not exactly one — the write changes one line and must know which"
    # The id the container runs and the id the tag names here must be
    # one image: if the tag was pulled anew since the container started,
    # `up` would recreate it on an image no plan named.
    FROM_ID=""
    preconditions "at the render" > "$WORK/health"
    FROM_ID=$(awk '/^container:/ {print $NF}' "$WORK/health")
    tag_id=$(docker image inspect -f '{{.Id}}' "$FROM_REF" 2> "$WORK/docker.err") \
        || fail "docker could not read the local image $FROM_REF: $(head -c 300 "$WORK/docker.err")"
    [ "$tag_id" = "$FROM_ID" ] \
        || refuse "the tag $FROM_REF names image $tag_id here, but $SERVICE runs $FROM_ID — the compose file and the running container disagree about what runs"
    docker image inspect -f '{{range .RepoDigests}}{{println .}}{{end}}' "$FROM_ID" > "$WORK/from-digests" 2> "$WORK/docker.err" \
        || refuse "cannot read a registry digest for the rollback image $FROM_ID: $(cat "$WORK/docker.err")"
    FROM_DIGEST=$(awk -v r="$IMAGE_REPO@" 'index($0, r) == 1 {print substr($0, length(r) + 1); exit}' "$WORK/from-digests")
    [[ "$FROM_DIGEST" =~ ^sha256:[0-9a-f]{64}$ ]] || refuse "rollback image $FROM_ID has no registry digest for $IMAGE_REPO"
    served=$(served_version)
    [ "$served" = "$FROM_VERSION" ] \
        || refuse "Forgejo's /api/v1/version says '$served', but its image is $FROM_REF — the version diff would start from a guess"
    # The version diff: strictly newer, at most one major ahead.
    [ "$TARGET" != "$FROM_VERSION" ] \
        || refuse "Forgejo already runs $TARGET — nothing to upgrade (an approved plan that already ran re-renders here)"
    [ "$(printf '%s\n%s\n' "$FROM_VERSION" "$TARGET" | sort -V | tail -n 1)" = "$TARGET" ] \
        || refuse "$TARGET is older than the $FROM_VERSION that runs — Forgejo does not run an older release against a migrated database; this verb only upgrades"
    major_from="${FROM_VERSION%%.*}"
    major_to="${TARGET%%.*}"
    [ "$major_to" -le $((major_from + 1)) ] \
        || refuse "$TARGET is more than one major release past $FROM_VERSION — upgrade one major at a time, reading each one's breaking changes"
    # The target, pulled and named by content: the passkey signs an
    # image id, never a tag a registry could move. The pull's output is
    # kept and printed only on failure (CLAUDE.md: quiet keeps evidence).
    TARGET_REF="$IMAGE_REPO:$TARGET"
    if ! timeout "$PULL_S" docker pull "$TARGET_REF" > "$WORK/pull" 2>&1; then
        sed 's/^/    /' "$WORK/pull" >&2
        fail "docker pull $TARGET_REF did not finish (its words above) — a release that cannot be pulled cannot be planned"
    fi
    target_id=$(docker image inspect -f '{{.Id}}' "$TARGET_REF" 2> "$WORK/docker.err") \
        || fail "docker pulled $TARGET_REF but cannot read its id: $(head -c 300 "$WORK/docker.err")"
    [[ "$target_id" =~ ^sha256:[0-9a-f]{64}$ ]] || fail "the image id of $TARGET_REF reads '$target_id', not sha256:<64 hex>"
    [ "$target_id" != "$FROM_ID" ] || refuse "$TARGET_REF is the very image $FROM_REF runs ($FROM_ID) — a new tag on the same bytes is no upgrade"
    # Captured first, then read: an awk that exits at its match must
    # never be the reader of a live producer under pipefail. A failed
    # read leaves no digest line, and the plan then says "unread".
    docker image inspect -f '{{range .RepoDigests}}{{println .}}{{end}}' "$TARGET_REF" > "$WORK/digests" 2> /dev/null
    digest=$(awk -v r="$IMAGE_REPO@" 'index($0, r) == 1 {print substr($0, length(r) + 1); exit}' "$WORK/digests")
    [[ "$digest" =~ ^sha256:[0-9a-f]{64}$ ]] || digest="unread (the image carries no $IMAGE_REPO digest)"
    TARGET_ID="$target_id"
    echo "plan: upgrade-forgejo"
    echo "compose file: $COMPOSE, service $SERVICE"
    echo
    echo "== version diff =="
    echo "from: $FROM_VERSION — image $FROM_REF ($FROM_ID), and /api/v1/version says $served"
    echo "to: $TARGET — image $TARGET_REF ($TARGET_ID), registry digest $digest"
    echo "change: the one line 'image: $FROM_REF' in $COMPOSE becomes 'image: $TARGET_REF'; nothing else in the file moves"
    echo "command: docker compose -f $COMPOSE up -d --no-deps --pull never $SERVICE — recreates the one container on the pulled image and pulls nothing"
    echo "outage: the registry, Actions and clone source are unavailable during recreation and migration; readback waits up to $WAIT_S seconds (normally 600), not a guarantee of recovery"
    echo "release notes: https://codeberg.org/forgejo/forgejo/releases/tag/v$TARGET — Forgejo migrates its database on the first start of $TARGET"
    echo
    echo "== health preconditions (read now, again after the dump, again before the swap) =="
    echo "pipeline guards are also re-read immediately before the dump; unreadable records refuse"
    cat "$WORK/health"
    echo
    echo "== before the swap =="
    echo "dump: $BACKUP — the nightly forge-backup's own definition: repositories, the database as forgejo-db.sql, app.ini; verified before it is named; it refuses on its own disk floor, and then nothing changes"
    echo "dump rotation: forge-backup keeps 14 archives by default (BOSS_FORGE_BACKUP_KEEP); later backups can rotate this dump out"
    echo "compose file kept as: $SAVED-$FROM_VERSION"
    echo
    echo "== rollback target =="
    echo "image: $FROM_REF ($FROM_ID), recover by digest $IMAGE_REPO@$FROM_DIGEST if disk-floor-sweep prunes the local image"
    echo "compose file: $SAVED-$FROM_VERSION"
    echo "database: the dump this run names before the swap — Forgejo does not run $FROM_VERSION against a database $TARGET migrated, so going back is that compose file AND a restore of the dump's forgejo-db.sql; no verb carries that restore yet"
}

[ -n "$FORGE_REPO" ] || refuse "the forge repository's path could not be derived ($FORGE_REPO_FROM)"
FORGE_DATA_DIR=$(compose_data_dir "$COMPOSE") \
    || refuse "$COMPOSE does not declare a host directory at the container's /data, so the repository the read-back proves cannot be named"
REPO_IN_CONTAINER="/data${FORGE_REPO#"$FORGE_DATA_DIR"}"

render > "$WORK/plan"
HASH="$(sha256sum "$WORK/plan" | cut -d' ' -f1)"

if [ "$PLAN" -eq 1 ]; then
    cat "$WORK/plan"
    echo "plan-sha256: $HASH" >&2
    exit 0
fi

# ---- the write ---------------------------------------------------------------
if [ "$HASH" != "$APPROVED" ]; then
    cat "$WORK/plan" >&2
    refuse "today's plan (above) hashes to $HASH, not the approved plan $APPROVED — Forgejo's image, its version, the target's image id or a precondition moved since the plan was signed (or this plan already ran). Nothing was changed; render and approve it again"
fi
cat "$WORK/plan"
echo
preconditions "before the dump" > /dev/null
say "plan $APPROVED still holds — taking the dump before anything changes"

# 1. The dump: forge-backup.sh's own run, its summary file withheld so it
#    never writes into the runner's record of THIS verb.
if ! env -u BOSS_RUN_SUMMARY_FILE BOSS_FORGE_BACKUP_DIR="$BACKUP_DIR" "$BACKUP" > "$WORK/backup" 2>&1; then
    sed 's/^/    /' "$WORK/backup" >&2
    refuse "the dump did not complete (forge-backup's words above) — without it there is no rollback target for the database, so Forgejo was not touched"
fi
sed 's/^/    /' "$WORK/backup"
DUMP=$(head -n 1 "$BACKUP_DIR/.latest" 2> /dev/null)
dump_name=$(awk '$1 == "forge-backup:" && $2 == "OK" {print $3; exit}' "$WORK/backup")
[ -n "$dump_name" ] && [ "$DUMP" = "$BACKUP_DIR/$dump_name" ] && [ -s "$DUMP" ] \
    || refuse "forge-backup exited 0 but its archive could not be named ($BACKUP_DIR/.latest reads '$DUMP', its OK line names '$dump_name') — Forgejo was not touched"
DUMP_SHA=$(sha256sum "$DUMP" | cut -d' ' -f1)
say "dump: $DUMP (sha256 $DUMP_SHA) — the database half of the rollback target"

# 2. Re-read, then keep the compose file and stage the new one.
preconditions "after the dump" > /dev/null
MAIN_BEFORE=$(main_inside "$CID") \
    || refuse "git inside $SERVICE could not read refs/heads/main in $REPO_IN_CONTAINER ($(head -c 300 "$WORK/git.err")) — the read-back would have nothing to hold the repository to; Forgejo was not touched"
if [ -e "$SAVED-$FROM_VERSION" ]; then
    cmp -s "$COMPOSE" "$SAVED-$FROM_VERSION" \
        || refuse "$SAVED-$FROM_VERSION exists and differs from $COMPOSE — an earlier run's rollback file is never overwritten; Forgejo was not touched"
else
    cp -p -- "$COMPOSE" "$SAVED-$FROM_VERSION" || refuse "could not keep $COMPOSE as $SAVED-$FROM_VERSION; Forgejo was not touched"
fi
NEWFILE=$(mktemp -- "$(dirname -- "$COMPOSE")/.upgrade-forgejo.XXXXXXXX.yml") \
    || refuse "could not create a private staged compose file beside $COMPOSE"
awk -v from="$FROM_REF" -v to="$TARGET_REF" '
    { line = $0 }
    line ~ /^[[:space:]]*image:/ {
        v = line; sub(/^[[:space:]]*image:[[:space:]]*/, "", v); sub(/[[:space:]]+$/, "", v); gsub(/["'"'"']/, "", v)
        if (v == from) { i = index(line, from); line = substr(line, 1, i - 1) to substr(line, i + length(from)) }
    }
    { print line }' "$COMPOSE" > "$NEWFILE" || refuse "could not write $NEWFILE; Forgejo was not touched"
chown --reference="$COMPOSE" "$NEWFILE" && chmod --reference="$COMPOSE" "$NEWFILE" \
    || refuse "could not preserve the owner and mode of $COMPOSE; Forgejo was not touched"
diff "$COMPOSE" "$NEWFILE" > "$WORK/diff" 2>&1
[ $? -le 1 ] || refuse "diff could not compare the staged compose file with $COMPOSE ($(head -c 300 "$WORK/diff")); Forgejo was not touched"
changed=$(grep -c '^[<>]' "$WORK/diff") || [ $? -eq 1 ] || refuse "cannot read the staged diff; Forgejo was not touched"
[ "$changed" = 2 ] || refuse "the staged compose file differs from $COMPOSE in $changed line(s), not exactly the one image line; Forgejo was not touched"
staged=$(compose_image "$NEWFILE") || refuse "$staged; Forgejo was not touched"
[ "$staged" = "$TARGET_REF" ] || refuse "compose reads the staged file's $SERVICE image as '$staged', not $TARGET_REF; Forgejo was not touched"
local_id=$(docker image inspect -f '{{.Id}}' "$TARGET_REF" 2> /dev/null)
[ "$local_id" = "$TARGET_ID" ] || refuse "the local $TARGET_REF is now $local_id, not the $TARGET_ID the plan signed; Forgejo was not touched"

# Keep the staged bytes so failure recovery can detect an intervening edit.
cp -- "$NEWFILE" "$WORK/staged" || refuse "could not capture the staged compose file; Forgejo was not touched"

# 3. Re-read immediately before the swap, then swap.
preconditions "before the swap" > /dev/null
mv -f -- "$NEWFILE" "$COMPOSE" || refuse "could not rename the staged compose file into place; Forgejo was not touched"
NEWFILE=""
ROLLBACK="image $FROM_REF ($FROM_ID, $IMAGE_REPO@$FROM_DIGEST), compose file $SAVED-$FROM_VERSION, database dump $DUMP (sha256 $DUMP_SHA) — Forgejo does not run $FROM_VERSION against a database $TARGET migrated, so going back is that compose file AND a restore of the dump's forgejo-db.sql"
say "$COMPOSE now declares $TARGET_REF — recreating $SERVICE"
if ! docker compose -f "$COMPOSE" up -d --no-deps --pull never "$SERVICE" > "$WORK/up" 2>&1; then
    sed 's/^/    /' "$WORK/up" >&2
    # A new container may have migrated the database. Restore only the
    # file, and only while the original container still runs the old image.
    if current=$(the_container) && [ "$current" = "$CID" ] \
        && state=$(docker inspect -f '{{.State.Running}} {{.Image}}' "$current") \
        && [ "$state" = "true $FROM_ID" ] && cmp -s "$COMPOSE" "$WORK/staged"; then
        NEWFILE=$(mktemp -- "$(dirname -- "$COMPOSE")/.upgrade-forgejo.XXXXXXXX.yml") \
            && cp -p -- "$SAVED-$FROM_VERSION" "$NEWFILE" \
            && mv -f -- "$NEWFILE" "$COMPOSE" \
            && fail "docker compose up failed (its words above); restored the original compose file while container $CID still ran $FROM_ID. Rollback target: $ROLLBACK"
        fail "docker compose up failed and the original compose file could not be restored. Rollback target: $ROLLBACK"
    fi
    fail "docker compose up failed (its words above) after $COMPOSE was changed to $TARGET_REF — Forgejo's state is what the next plan reads. Rollback target: $ROLLBACK"
fi
sed 's/^/    /' "$WORK/up"
say "compose accepted the upgrade of $SERVICE to $TARGET — reading it back (up to ${WAIT_S}s: the container's image, /api/healthz, /api/v1/version, and refs/heads/main)"

# ---- the read-back -------------------------------------------------------------
start="$(date -u +%s)"
held=0
while :; do
    img="unread"; h="not read"; v="not read"; m="not read"
    if cid=$(the_container); then
        if docker inspect -f '{{.State.Running}} {{.Image}}' "$cid" > "$WORK/state" 2> "$WORK/docker.err"; then
            read -r running img < "$WORK/state"
            [ "$running" = true ] || img="stopped ($img)"
        else
            img="unread: $(head -c 160 "$WORK/docker.err")"
        fi
    else
        img="$cid"
    fi
    h=$(healthz)
    v=$(served_version)
    if [ "$img" = "$TARGET_ID" ] && now=$(main_inside "$cid"); then
        if [ "$now" = "$MAIN_BEFORE" ] \
                || docker exec -u git "$cid" git --git-dir="$REPO_IN_CONTAINER" merge-base --is-ancestor "$MAIN_BEFORE" "$now" 2> /dev/null; then
            m="holds ${MAIN_BEFORE:0:12}$([ "$now" != "$MAIN_BEFORE" ] && echo " (main is now ${now:0:12})")"
        else
            m="LOST: main reads ${now:0:12}, which does not contain ${MAIN_BEFORE:0:12}"
        fi
    elif [ "$img" = "$TARGET_ID" ]; then
        m="unread: $(head -c 160 "$WORK/git.err")"
    fi
    elapsed=$(( $(date -u +%s) - start ))
    if [ "$img" = "$TARGET_ID" ] && [ "$h" = pass ] && [ "$v" = "$TARGET" ] && [[ "$m" == holds* ]]; then
        held=$((held + 1))
        if [ "$held" -ge 2 ]; then
            say "Forgejo upgraded $FROM_VERSION → $TARGET — read back: container ${cid:0:12} runs image $TARGET_ID, /api/healthz pass, /api/v1/version $TARGET, refs/heads/main holds ${m#holds }, on two reads ${POLL_S}s apart (after ${elapsed}s). Rollback target: $ROLLBACK"
            exit 0
        fi
    else
        held=0
    fi
    if [ "$elapsed" -ge "$WAIT_S" ]; then
        fail "compose accepted the upgrade of $SERVICE to $TARGET, and after ${elapsed}s it is NOT proven: image $img (want $TARGET_ID), /api/healthz $h, /api/v1/version $v, refs/heads/main $m. Rollback target: $ROLLBACK"
    fi
    sleep "$POLL_S"
done
