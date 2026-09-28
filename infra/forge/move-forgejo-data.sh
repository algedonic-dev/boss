#!/usr/bin/env bash
# move-forgejo-data — move the forge's Forgejo data off the root
# filesystem onto the boss-data drive, keeping its path; and put it back.
#
#   move-forgejo-data.sh --plan                  render the move's plan; changes nothing
#   move-forgejo-data.sh <plan-sha256>           run the move the SIGNED plan names
#   move-forgejo-data.sh --plan-rollback         render the rollback's plan; changes nothing
#   move-forgejo-data.sh --rollback <plan-sha256>  run the rollback the SIGNED plan names
#   move-forgejo-data.sh --plan-restart          render a restart's plan; changes nothing
#   move-forgejo-data.sh --restart <plan-sha256>   start a STOPPED Forgejo, as the SIGNED plan names
#   move-forgejo-data.sh --plan-delete           render the pre-move copy's delete; changes nothing
#   move-forgejo-data.sh --delete <plan-sha256>  delete the pre-move copy the SIGNED plan names
#
# MUTATING, and a trust boundary: it stops Forgejo — the repository,
# the CI runner's queue and the OCI registry every train pushes to and
# every cluster pulls from — copies its data, and edits /etc/fstab, the
# file a bad line in bricks the forge's boot.
#
# WHY (backlog 016aeea3, car 2 of b2d5b546, 2026-09-27). The package
# registry under /opt/forgejo/data was 92G of the forge's root disk and
# drove the urgent disk alarm c4da71b0. A second drive went in for it and
# commission-a-disk (car 1) mounts it by UUID with the label boss-data.
# This verb moves the data there.
#
# KEEP THE PATH, MOVE WHAT BACKS IT. /opt/forgejo/data becomes a bind
# mount of <boss-data mount>/forgejo-data, by path, in an fstab line
# beneath the drive's by-UUID line. So the compose file and the five
# scripts that read /opt/forgejo/data (forge-repo-path.sh, forge-log.sh,
# forge-backup.sh, disk-report.sh, prune-registry-versions.sh) do not
# change: the fact of where the data lives stays in one place.
#
# PLAN, THEN APPROVE (design 17835005; commission-a-disk after b2d5b546
# is the worked example). --plan evaluates every precondition the write
# evaluates, from the tree and the host, never from a param, and prints
# a plan document (move-forgejo-data.plan.jq) with its sha256 on stderr
# as `plan-sha256: <hex>`. David's passkey signs those bytes; the runner
# hands the write that hash; the write renders the plan AGAIN with the
# same command and changes nothing unless the bytes hash alike.
#
# THE PRECONDITIONS, each a refusal (exit 78, nothing changed):
#   - the source is a real directory (not a symlink), on the root
#     filesystem, not itself a mount, with no mount beneath it, and its
#     top-level entries have plain names;
#   - the compose file binds exactly that directory at the container's
#     /data (forge-repo-path.sh's reading — one definition), the service
#     has exactly one container and it is running, and no OTHER running
#     container mounts the source, anything under it, or a directory
#     above it (which sees the source too);
#   - the process table answers for this process, so a host process
#     holding the source can be named after the stop;
#   - exactly one filesystem is labelled boss-data; it is ext4, mounted
#     at exactly one plain path that is not /, not under the source and
#     not over it; its UUID is not the root filesystem's; and /etc/fstab
#     carries its by-UUID line — so the bind appended beneath it has a
#     mount to wait for at boot;
#   - /etc/fstab has no entry for the source yet and verifies clean
#     (findmnt --verify), and the pre-move name is free;
#   - the target's free space covers the source's size ceiling plus a
#     margin;
#   - the registry proof can be read: the converge's stamp and its
#     docker login are where registry-login.lib.sh reads them.
#
# THE WRITE, in order — pinned by reading this file in
# crates/core/boss-testing/tests/move_forgejo_data_sh.rs:
#   re-render and compare; check the proof can be read (healthz, main
#   inside the container, the stamp's manifest, and the served-copy leg
#   against the source as it stands) — all of that changes nothing, so a
#   failure there is still exit 78. Then: hold the
#   converge; stop Forgejo (-t 60) and record its running state,
#   StartedAt and RestartCount; read main with host git, off the disk;
#   re-check no running container mounts the source and no HOST process
#   holds anything under it; measure the source and hold it to the
#   ceiling and the free space; copy (rsync -aHAXS --numeric-ids
#   --delete); verify (rsync -aHAXSc … --dry-run --itemize-changes must
#   list nothing); measure the target, which must equal the source
#   entry by entry; build and verify the new fstab in a temporary file;
#   confirm Forgejo is still exactly as the stop left it; mark the target
#   as served (its record: this plan's hash, the stamp and the manifest
#   sha the before half read).
#   THE COMMIT POINT: rename the source to its pre-move name; create the
#   empty mountpoint and make it immutable; move the verified fstab into
#   place; daemon-reload; mount THAT path (never `mount -a`); confirm the
#   mount is the target's UUID at /forgejo-data; confirm Forgejo is
#   still as the stop left it; start it, and
#   the start must be real (StartedAt moves); prove — healthz, main
#   equal to the disk's, the container's /data the moved copy by device
#   and inode, the manifest; release the hold.
#
# WHY THE CONTAINER IS READ THREE TIMES (adversarial review of this car,
# 2026-09-27, finding 1). The healthz, main and manifest legs read the
# same off the old copy and the new, so none of them can tell a Forgejo
# serving the moved copy from one serving the pre-move copy. dockerd
# starts a restart:always container again on its own — a daemon restart
# mid-copy is enough — and that Forgejo writes to files the checksum pass
# already vouched for; the rename then happens under it, `compose start`
# is a no-op, and the proof says OK. So the stop's StartedAt and
# RestartCount must still read the same before the rename and before the
# start, the start must move StartedAt, and one proof leg ties what the
# container sees at /data to the moved copy's own inode.
#
# FAILURE. Before the commit point nothing the old layout depends on has
# changed, so the EXIT trap starts Forgejo again on the untouched
# source and releases this move's own hold (a partial copy stays in the
# target; a retry mirrors over it with --delete, which is why a target
# Forgejo ever SERVED is refused: a rollback leaves Forgejo's writes
# there). After it, nothing is undone by guess: the state is printed,
# the hold stays, a failed proof stops Forgejo again, exit 1, and the
# rollback is its own approved verb (--plan-rollback / --rollback),
# which reads whichever partial state it finds — including a drive that
# is gone.
#
# A STOPPED FORGE IS NEVER A ROOT SHELL (re-review of the released car,
# backlog 85d29e33, 2026-09-27). A proof that fails stops Forgejo, and
# until this there was no verb that could start it again. So:
#   - the served-copy leg (docker exec … stat) was first run AFTER the
#     commit point, so a Forgejo image without stat, or whose stat
#     prints otherwise, stopped the forge on the move's proof and again
#     on the rollback's. It is now read in the before half too, against
#     the source, as a refusal: a pure read that proves the leg works
#     before anything moves (finding 2).
#   - the rollback proved the registry against the CURRENT stamp. After
#     a later converge that image exists only in the moved copy, so its
#     manifest 404s off the copy put back. The move now records the
#     stamp and the manifest sha it proved in the target's marker, and
#     the rollback proves THAT; with no record to read (the drive is
#     gone, and the marker with it) the registry leg is a reported
#     finding, not a stop, and the plan says which (finding 1).
#   - the marker is written BEFORE the rename, not after the mount: from
#     the rename on, a restart:always Forgejo that dockerd starts on its
#     own may write to the target, and a failure in that window must not
#     leave such writes unmarked (finding 3). A failure before the
#     rename removes the marker this run wrote: nothing was served.
#   - a Forgejo found started behind this verb's back once the source is
#     renamed is stopped like a failed proof, never left serving data
#     this run cannot vouch for (finding 6).
#   - --plan-restart / --restart (plan-a-forgejo-restart,
#     restart-forgejo) start a STOPPED Forgejo on whatever the source is
#     — the move's bind or a directory on / — under the same signed-plan
#     approval. The start is its only act; it proves a real start and a
#     passing healthz, reports the other legs, and never stops Forgejo.
#
# THE RENAME IS THE COMMIT POINT, WHATEVER THE STAGE SAYS (review of the
# released follow-up car, backlog ed7702c3, 2026-09-27):
#   - a TERM/INT/HUP landing after `mv -T` returned and before
#     STAGE=committed ran the EXIT trap's pre-rename branch: `compose
#     start` on a source that no longer existed — compose creates a
#     missing bind path, so a fresh EMPTY Forgejo on the root disk — the
#     hold released, and the marker of a run that did rename removed.
#     RENAMING is set immediately before the rename, and the trap reads
#     the tree (rename(2) is atomic) for which side of it the run is on
#     (finding 1).
#   - restart-forgejo exits 1 when any leg it reports is a FINDING:
#     Forgejo stays running, but the run is started, not proven. Its plan
#     names any pre-move copy, the served marker's record and the
#     converge hold, so an approver sees a copy that never passed its
#     move's proof (finding 2).
#   - a served marker is refused at plan time whether or not its target
#     directory stands, not after the copy (finding 4); every plan says
#     which layer named the repository path (finding 5).
#
# THE PRE-MOVE COPY IS DELETED BY ITS OWN APPROVED VERB (backlog
# 192a9003, 2026-09-27). The move leaves the old copy on / as
# $SRC.pre-move-<8 hex>, so the root disk — the whole point, alarm
# c4da71b0 — gets nothing back until it goes, and nothing could delete
# it. --plan-delete / --delete (plan-a-forgejo-premove-delete,
# delete-forgejo-premove-copy) delete exactly that directory, and only
# while every one of these holds, each a refusal before anything moves:
#   - Forgejo runs, its healthz passes, the container's /data is the
#     moved copy by device and inode (and is not the copy to delete),
#     the repository it serves is the moved copy's, and the registry
#     serves the converge's current stamp — the rollback target, which
#     the registry prune always keeps, so the leg cannot age out the
#     way the marker's recorded stamp will;
#   - $SRC is mounted as the move's bind of /forgejo-data on the drive,
#     and fstab carries exactly the move's bind line and the drive's own
#     by-UUID line;
#   - the served marker is the record a move writes, and it is at least
#     SOAK_HOURS (24) old — the move's rollback window. 24 h because the
#     root disk is the alarm this move answers, and a day of real pushes,
#     trains and registry pulls off the new drive is the evidence the
#     rollback exists to wait for;
#   - no move or rollback still holds the converge (each releases its
#     hold only once its proof passes, so a hold of theirs is a run that
#     may still need this copy);
#   - the copy is EXACTLY the move's rename target, derived the way the
#     move derived it (the first 8 hex of the bind's drive UUID) — and
#     the only one: a second pre-move-* is refused, not chosen among;
#     a real directory on /, not a mount, nothing mounted beneath it, no
#     running container mounting it and no host process holding it.
# The act: rename it to $SRC.deleting-<8 hex> (atomic — from here no
# rollback plan finds a pre-move copy, so a copy half deleted is never
# put back), rm -rf --one-file-system that name, then prove it gone,
# that / grew by its measured size less a slack, and read the proof
# again. Forgejo is never stopped or started and the converge is never
# held. A delete stopped part-way leaves only the deleting- name, and
# the same plan verb renders the delete that finishes it.
#
# THE MOUNTPOINT IS IMMUTABLE (chattr +i). The fstab line carries
# nofail, so a bind that cannot mount at boot does not stop the forge
# booting — and would leave an EMPTY directory at the path Forgejo
# serves from. An immutable empty directory makes that a Forgejo that
# cannot start (loud), never a fresh empty forge accepting pushes into
# the root disk (silent).
#
# EXIT: 0 done / plan printed; 78 refused, NOTHING changed; 1 failed
# after something changed — the output says what stands.
#
# ENV (test seams — the ops runner passes no packet-supplied environment,
# only an argv built from the allowlist, so a packet cannot set these)
#   BOSS_FORGEJO_DATA         the source (default /opt/forgejo/data)
#   BOSS_FORGE_COMPOSE        the compose file (forge-repo-path.sh's)
#   BOSS_FORGE_SERVICE        the compose service (default forgejo)
#   BOSS_FSTAB                the fstab (default /etc/fstab)
#   BOSS_CONVERGE_HOLD        the hold file (forge-defaults.sh's)
#   BOSS_FORGE_LAST_BUILT     the converge's stamp (forge-defaults.sh's)
#   BOSS_MOVE_DOCKER_CONFIG   the converge's docker login
#   BOSS_MOVE_DOCKER_HOST     the daemon Forgejo runs on (the system one)
#   BOSS_MOVE_HEALTH_WAIT     seconds to wait for healthz after a start
#   BOSS_MOVE_PROC_DIR        the process table read for host holders (/proc)
#   BOSS_MOVE_RETRY_SLEEP     seconds between umount tries and free-space reads (default 5)
#   BOSS_MOVE_DELETE_SLACK_FLOOR  bytes the delete's free-space proof forgives at least (2 GiB)
set -uo pipefail

ME="move-forgejo-data"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"

say() { echo "$ME: $*" >&2; }
# A refusal: nothing has changed. The runner and the reader both take
# 78 to mean exactly that, so it is never used after a mutation.
refuse() { say "REFUSED — $*"; say "  Nothing was changed."; exit 78; }
# A failure after something changed: exit 1, and the EXIT trap says
# what stands.
fail() { say "FAILED — $*"; exit 1; }

# shellcheck source=infra/lib/jq.sh
. "$REPO/infra/lib/jq.sh"
# HOLD_FILE, LAST_BUILT_NAME, forge_need: one definition for every verb.
# shellcheck source=infra/forge/forge-defaults.sh
. "$HERE/forge-defaults.sh"
# shellcheck source=infra/forge/registry-login.lib.sh
. "$HERE/registry-login.lib.sh"

# ---- the arguments -----------------------------------------------------
usage() {
    say "usage: $ME --plan | <plan-sha256> | --plan-rollback | --rollback <plan-sha256> | --plan-restart | --restart <plan-sha256> | --plan-delete | --delete <plan-sha256>"
    say "  this verb takes no path: what it moves, and where, is read from the host"
    exit 78
}
is_hash() { [[ "$1" =~ ^[0-9a-f]{64}$ ]]; }
APPROVED=""
case "${1-}" in
    --plan)          [ $# -eq 1 ] || usage; MODE=plan ;;
    --plan-rollback) [ $# -eq 1 ] || usage; MODE=plan-rollback ;;
    --rollback)
        [ $# -eq 2 ] || usage
        is_hash "$2" || refuse "the plan hash must be 64 lowercase hex characters, got '$2'"
        MODE=rollback; APPROVED="$2" ;;
    --plan-restart)  [ $# -eq 1 ] || usage; MODE=plan-restart ;;
    --restart)
        [ $# -eq 2 ] || usage
        is_hash "$2" || refuse "the plan hash must be 64 lowercase hex characters, got '$2'"
        MODE=restart; APPROVED="$2" ;;
    --plan-delete)   [ $# -eq 1 ] || usage; MODE=plan-delete ;;
    --delete)
        [ $# -eq 2 ] || usage
        is_hash "$2" || refuse "the plan hash must be 64 lowercase hex characters, got '$2'"
        MODE=delete; APPROVED="$2" ;;
    *)
        [ $# -eq 1 ] || usage
        is_hash "$1" || refuse "the argument is --plan, --plan-rollback, --rollback <plan-sha256>, or a 64 lowercase hex plan-sha256 — got '$1'. This writes only an APPROVED plan; render one with --plan (the plan-a-forgejo-data-move verb)"
        MODE=move; APPROVED="$1" ;;
esac

# ---- the fixed facts ---------------------------------------------------
SRC="${BOSS_FORGEJO_DATA:-/opt/forgejo/data}"
SERVICE="${BOSS_FORGE_SERVICE:-forgejo}"
FSTAB="${BOSS_FSTAB:-/etc/fstab}"
LABEL="boss-data"
TGT_NAME="forgejo-data"
GIB=1073741824
BUCKET_GIB=64
MARGIN_GIB=16
HEALTH_WAIT="${BOSS_MOVE_HEALTH_WAIT:-180}"
# The delete's rollback window after the move went live, and what its
# free-space proof forgives for whatever else writes to / meanwhile: the
# larger of a tenth of the copy and this floor.
SOAK_HOURS=24
SLACK_FLOOR="${BOSS_MOVE_DELETE_SLACK_FLOOR:-$((2 * GIB))}"
# Forgejo runs on the SYSTEM daemon; a DOCKER_HOST inherited from the
# converge's rootless one answers "no such container" about a running
# one (forge-backup.sh).
export DOCKER_HOST="${BOSS_MOVE_DOCKER_HOST:-unix:///var/run/docker.sock}"

# A path this verb may act on: absolute, plain characters only (so an
# fstab field can never split or carry an escape), no `.` or `..`
# component, no doubled or trailing slash.
plain_path() {
    [[ "$1" =~ ^/[A-Za-z0-9._/-]+$ ]] || return 1
    case "$1/" in */./*|*/../*|*//*) return 1 ;; esac
    [ "${1%/}" = "$1" ]
}
under() { [ "$1" = "$2" ] || [[ "$1" == "$2"/* ]]; }
# above <dir> <path> — <dir> is a proper ancestor of <path>; / is above
# every path.
above() { [ "$1" != "$2" ] && { [ "$1" = / ] || [[ "$2" == "$1"/* ]]; }; }

RETRY_SLEEP="${BOSS_MOVE_RETRY_SLEEP:-5}"
PROC="${BOSS_MOVE_PROC_DIR:-/proc}"
# fstab_verifies tolerates errors only under these targets (the
# rollback's absent drive); empty everywhere else.
TOLERATED=""

for t in jq sha256sum findmnt lsblk du find df docker rsync chattr lsattr mount umount systemctl curl awk stat mktemp base64 git date; do
    command -v "$t" >/dev/null 2>&1 || refuse "$t is not on PATH, and this verb needs it"
done
plain_path "$SRC" || refuse "the source '$SRC' is not a plain absolute path"
PRE_GLOB="$SRC.pre-move-"

WORK=$(mktemp -d) || refuse "cannot make a work directory"
chmod 700 "$WORK"
NEWTAB=""
STAGE=idle         # idle -> mutating -> committed -> done; a restart: idle -> restarting -> done
STOPPED=0
RENAMING=0         # 1 from immediately before the move's rename (finding 1)
MARKER_WRITTEN=0
HELD_BY_US=0
HOLD_REASON=""

# ---- observations shared by both plans --------------------------------
# The mount table, read once, whole. A read that fails is a refusal,
# never an empty table (a failed read is not an empty one).
findmnt -rno TARGET > "$WORK/mounts" 2> "$WORK/findmnt.err" \
    || refuse "findmnt could not list the mount table ($(head -c 300 "$WORK/findmnt.err"))"
[ -s "$WORK/mounts" ] || refuse "findmnt listed no mounts at all, which is not a table this verb can judge"
is_mounted() { grep -qxF -- "$1" "$WORK/mounts"; }
ROOT_UUID=$(findmnt -nro UUID / 2> "$WORK/findmnt.err") \
    || refuse "findmnt could not name the root filesystem's UUID ($(head -c 300 "$WORK/findmnt.err"))"
[ -n "$ROOT_UUID" ] || refuse "the root filesystem has no UUID that findmnt can read, so 'not backing /' cannot be judged"

[ -r "$FSTAB" ] || refuse "$FSTAB cannot be read"
SRC_LINES=$(awk -v s="$SRC" '$1 !~ /^#/ && $2 == s' "$FSTAB" | wc -l) \
    || refuse "cannot read the entries of $FSTAB"

# The compose file binds the source at /data: forge-repo-path.sh is the
# one reading of that declaration, and FORGE_REPO is the repository the
# proof reads.
# shellcheck source=infra/forge/forge-repo-path.sh
. "$HERE/forge-repo-path.sh"
bound=$(compose_data_dir "$FORGE_COMPOSE") \
    || refuse "$FORGE_COMPOSE does not declare a host directory at the container's /data, so what Forgejo serves from cannot be tied to $SRC"
[ "$bound" = "$SRC" ] || refuse "$FORGE_COMPOSE binds $bound at /data, not $SRC — this verb moves only the directory Forgejo serves"
under "$FORGE_REPO" "$SRC" && [ "$FORGE_REPO" != "$SRC" ] \
    || refuse "the forge repository $FORGE_REPO is not under $SRC ($FORGE_REPO_FROM)"
REPO_IN_CONTAINER="/data${FORGE_REPO#"$SRC"}"

docker compose version > /dev/null 2> "$WORK/docker.err" \
    || refuse "docker compose does not answer ($(head -c 300 "$WORK/docker.err"))"
docker compose -f "$FORGE_COMPOSE" config --services > "$WORK/services" 2> "$WORK/docker.err" \
    || refuse "docker compose could not read $FORGE_COMPOSE ($(head -c 300 "$WORK/docker.err"))"
grep -qxF -- "$SERVICE" "$WORK/services" || refuse "$FORGE_COMPOSE declares no service named $SERVICE"
docker compose -f "$FORGE_COMPOSE" ps -a -q "$SERVICE" > "$WORK/cid" 2> "$WORK/docker.err" \
    || refuse "docker compose could not list $SERVICE's container ($(head -c 300 "$WORK/docker.err"))"
[ "$(grep -c . "$WORK/cid")" = 1 ] || refuse "$SERVICE has $(grep -c . "$WORK/cid") containers, not exactly one"
CID=$(awk 'NR == 1' "$WORK/cid")
running_state() { # <container id> -> true|false on stdout, rc 1 when docker cannot say
    local s
    s=$(docker inspect -f '{{.State.Running}}' "$1" 2> "$WORK/docker.err") || return 1
    case "$s" in true|false) printf '%s\n' "$s" ;; *) return 1 ;; esac
}
RUNNING=$(running_state "$CID") || refuse "docker could not say whether $SERVICE ($CID) is running ($(head -c 300 "$WORK/docker.err"))"

# container_facts <id> -> "<running> <StartedAt> <RestartCount>". Read at
# the stop and compared again immediately before the rename and before
# the start. dockerd starts a restart:always container again on its own
# (a daemon restart mid-copy is enough), and a Forgejo started that way
# writes to files the checksum pass has already vouched for; the pair
# moves even when the container has since stopped again (adversarial
# review of 016aeea3, 2026-09-27, finding 1).
container_facts() {
    local f
    f=$(docker inspect -f '{{.State.Running}} {{.State.StartedAt}} {{.RestartCount}}' "$1" 2> "$WORK/docker.err") || return 1
    [[ "$f" =~ ^(true|false)\ [^\ ]+\ [0-9]+$ ]] || return 1
    printf '%s\n' "$f"
}
# The restart policy the host's compose file gave the container: the
# policy under which dockerd may start it again behind this verb's back.
# Named on the plan, because it is what the approver is trusting the
# re-checks against. Docker answers "no" (or nothing, on old engines)
# for none; any word it has no name for is refused.
RESTART_POLICY=$(docker inspect -f '{{.HostConfig.RestartPolicy.Name}}' "$CID" 2> "$WORK/docker.err") \
    || refuse "docker could not read $SERVICE's restart policy ($(head -c 300 "$WORK/docker.err"))"
case "$RESTART_POLICY" in
    ''|no) RESTART_POLICY=no ;;
    always|unless-stopped|on-failure) ;;
    *) refuse "$SERVICE's restart policy reads '$RESTART_POLICY', which is not one docker defines" ;;
esac

# Every OTHER running container that mounts something at or under the
# source, or a directory ABOVE it (a bind of /opt/forgejo, or of /, sees
# the source as well — re-review finding 5): stopping Forgejo alone
# would copy data another container is still writing.
others_mounting() { # -> prints "<id> <source>" lines; rc 1 when docker cannot say
    local id
    docker ps -q --no-trunc > "$WORK/ps" 2> "$WORK/docker.err" || return 1
    : > "$WORK/others"
    while IFS= read -r id; do
        [ -n "$id" ] && [ "$id" != "$CID" ] || continue
        docker inspect -f '{{range .Mounts}}{{.Source}}{{"\n"}}{{end}}' "$id" > "$WORK/m" 2> "$WORK/docker.err" || return 1
        while IFS= read -r m; do
            [ -n "$m" ] || continue
            if under "$m" "$SRC" || above "$m" "$SRC"; then echo "$id $m" >> "$WORK/others"; fi
        done < "$WORK/m"
    done < "$WORK/ps"
    cat "$WORK/others"
}

# The registry proof's inputs: the converge's image repo, its stamp (the
# rollback target) and its docker login, all where the converge keeps
# them. Checked here so a plan is never approved for a proof that cannot
# be read.
# sor_require and forge_need exit 1 when an address is missing; here
# nothing has changed yet, so that is asked first and refused as 78.
[ -n "${BOSS_FORGE_URL:-}" ] \
    || refuse "BOSS_FORGE_URL is not set (/etc/boss/sor.env), so Forgejo's healthz has no address"
HEALTHZ="$BOSS_FORGE_URL/api/healthz"
# A restart proves no registry leg (it reports, never stops), so it
# needs neither the login nor the stamp: a recovery verb that refused
# for a missing login would leave the forge down for a reason of its
# own. The rollback proves the STAMP ITS MOVE RECORDED, never the
# current one, so only the move reads the stamp file (finding 1).
case "$MODE" in
    plan|move|plan-rollback|rollback|plan-delete|delete)
        [ -n "${BOSS_FORGE_REGISTRY:-}${REGISTRY:-}${BOSS_FORGE_REGISTRY_HOST:-}" ] \
            || refuse "no registry is named (BOSS_FORGE_REGISTRY_HOST in /etc/boss/sor.env), so the registry proof has nothing to read"
        forge_need REGISTRY
        REG_HOST="${REGISTRY%%/*}"
        reg_rest="${REGISTRY#*/}"
        REG_OWNER="${reg_rest%%/*}"
        REG_NAME="${reg_rest#*/}"
        [ "$REG_HOST/$REG_OWNER/$REG_NAME" = "$REGISTRY" ] || refuse "the registry '$REGISTRY' is not of the shape host[:port]/owner/name"
        if [ -z "${BOSS_MOVE_DOCKER_CONFIG:-}" ] || [ -z "${BOSS_FORGE_LAST_BUILT:-}" ]; then
            OWNER_HOME="$(checkout_owner_home "$REPO")" || refuse "cannot resolve the owner of $REPO, so the converge's docker login and stamp have no path; the registry proof could not be read"
        fi
        DOCKER_CONFIG_FILE="${BOSS_MOVE_DOCKER_CONFIG:-$OWNER_HOME/.docker/config.json}"
        STAMP_FILE="${BOSS_FORGE_LAST_BUILT:-$OWNER_HOME/$LAST_BUILT_NAME}"
        registry_login_read "$DOCKER_CONFIG_FILE" "$REG_HOST" || refuse "$REGISTRY_LOGIN_WHY"
        ( umask 077; printf 'header = "Authorization: Basic %s"\n' "$AUTH_B64" > "$WORK/basic.cfg" ) ;;
esac
case "$MODE" in
    plan|move|plan-delete|delete)
        [ -r "$STAMP_FILE" ] || refuse "the converge's stamp $STAMP_FILE cannot be read, so there is no known manifest to prove the registry with" ;;
esac

# fstab_line <target dir> <target mount> — the ONE spelling of the bind
# line: the move appends it, and the rollback removes the line only
# when it is exactly what this function says for the fields it holds.
#   nofail                      a bind that cannot mount never stops the boot
#   x-systemd.requires-mounts-for  it waits for the drive's own mount
#   x-systemd.before=docker.service  and the system daemon waits for it
#   x-systemd.device-timeout    bounded, per the review of commission-a-disk
fstab_line() {
    printf '%s %s none bind,nofail,x-systemd.requires-mounts-for=%s,x-systemd.before=docker.service,x-systemd.device-timeout=30s 0 0' \
        "$1" "$SRC" "$2"
}

# fstab_verifies <file> — findmnt --verify is clean, or every error it
# reports sits under a target named in TOLERATED. findmnt prints each
# target unindented and its messages indented beneath it, so an error is
# attributed to the target line above it. Its output is left in
# $WORK/verify-out for the caller to print.
#
# WHY A TOLERANCE AT ALL (review of 016aeea3, finding 2). With the
# boss-data drive gone, findmnt --verify reports its by-UUID line as
# "[E] unreachable on boot required source" — even with nofail,
# measured on util-linux 2.38.1 — and that is exactly when the rollback
# is needed. So the rollback, and only the rollback, sets TOLERATED to
# the drive's own mount and the bind it is removing, and only when lsblk
# no longer lists the drive. Any other error, or an error that cannot be
# attributed to a target, is still a refusal.
#
# AND ONLY AN UNREACHABLE SOURCE (re-review of the released car, finding
# 4). A parse error drops its line from findmnt's reading, so no target
# carries it and the attribution above would never see it: any "parse
# error" line but the summary's own "0 parse errors" is refused
# outright. And under the tolerated targets only an "[E] unreachable …"
# error is the absent drive's; any other error there — a bad option, an
# unknown filesystem type — is refused like an error anywhere else.
fstab_verifies() {
    findmnt --verify --tab-file "$1" > "$WORK/verify-out" 2>&1 && return 0
    [ -n "$TOLERATED" ] || return 1
    awk '/parse error/ && !/^0 parse errors/ { bad = 1 } END { exit bad }' "$WORK/verify-out" || return 1
    # A target for each error, or an empty line for an error that is not
    # an unreachable source, which no target can excuse.
    awk '/^[^ \t]/ { t = $0; next } /\[E\]/ { print (/\[E\] unreachable / ? t : "") }' \
        "$WORK/verify-out" > "$WORK/verify-err-targets" || return 1
    [ -s "$WORK/verify-err-targets" ] || return 1
    local t
    while IFS= read -r t; do
        [ -n "$t" ] || return 1
        case " $TOLERATED " in *" $t "*) ;; *) return 1 ;; esac
    done < "$WORK/verify-err-targets"
    say "findmnt --verify on $1: every error is the absent drive's (under $TOLERATED) — tolerated for the rollback"
    sed 's/^/    /' "$WORK/verify-out" >&2
    return 0
}

# holders_under <dir> — "<pid> <comm> <path>" for every HOST process
# whose cwd, root, executable or an open file is at or under <dir>. Not
# `fuser -m`: the source is a directory on /, and -m names every process
# using the root filesystem. A container process's links read as paths
# in its own mount namespace (/data/...), which the container check
# covers; this is the host's own processes (a backup, a shell, a du). A
# link that cannot be read belongs to a process that has exited.
#
# A TABLE THAT CANNOT BE READ IS NOT AN EMPTY ONE (re-review finding 5):
# with /proc unmounted, or read through a seam that is not a process
# table, every link fails and the loop names no holder at all. So the
# table must first answer for the process reading it ($PROC/self, the
# kernel's own name for the reader), or this returns 1.
proc_readable() { readlink -- "$PROC/self/cwd" > /dev/null 2>&1; }
holders_under() {
    local l t pid
    proc_readable || return 1
    for l in "$PROC"/[0-9]*/cwd "$PROC"/[0-9]*/root "$PROC"/[0-9]*/exe "$PROC"/[0-9]*/fd/*; do
        t=$(readlink -- "$l" 2> /dev/null) || continue
        t="${t% (deleted)}"
        under "$t" "$1" || continue
        pid="${l#"$PROC"/}"
        pid="${pid%%/*}"
        echo "$pid $(cat "$PROC/$pid/comm" 2> /dev/null) $t"
    done
    return 0
}

# The served marker's record — ONE spelling for the move that writes it
# and the rollback that reads it: this plan's sha256, the converge stamp
# whose manifest the move's before half read off the pre-move copy, and
# that manifest's sha256 (finding 1).
marker_record() { printf 'plan %s\nstamp %s\nmanifest %s\n' "$1" "$2" "$3"; }
read_marker() { # <file> -> PROVE_PLAN, PROVE_STAMP, PROVE_MANIFEST; rc 1 unless it is exactly a record
    local body re=$'^plan ([0-9a-f]{64})\nstamp ([0-9a-f]{7,64})\nmanifest ([0-9a-f]{64})$'
    body=$(cat -- "$1" 2> /dev/null) || return 1
    [[ "$body" =~ $re ]] || return 1
    PROVE_PLAN="${BASH_REMATCH[1]}"
    PROVE_STAMP="${BASH_REMATCH[2]}"
    PROVE_MANIFEST="${BASH_REMATCH[3]}"
}

# ---- the move's observations ------------------------------------------
observe_move() {
    [ -e "$SRC" ] || refuse "$SRC does not exist"
    [ ! -L "$SRC" ] || refuse "$SRC is a symlink — this verb moves only a real directory"
    [ -d "$SRC" ] || refuse "$SRC is not a directory"
    [ "$SRC_LINES" = 0 ] || refuse "$FSTAB already has $SRC_LINES entr(y/ies) for $SRC — the move has been made, or something else mounts there"
    fstab_verifies "$FSTAB" \
        || { sed 's/^/    /' "$WORK/verify-out" >&2; refuse "$FSTAB does not verify clean today (above), so no edit to it can be shown safe"; }
    is_mounted "$SRC" && refuse "$SRC is itself a mount — the data is not on the root filesystem, so there is nothing for this verb to move"
    local on
    on=$(findmnt -nro TARGET -T "$SRC" 2> "$WORK/findmnt.err") \
        || refuse "findmnt could not say which filesystem holds $SRC ($(head -c 300 "$WORK/findmnt.err"))"
    [ "$on" = "/" ] || refuse "$SRC is on the filesystem mounted at '$on', not on / — this verb moves the data off the root filesystem"
    awk -v p="$SRC/" 'index($0, p) == 1' "$WORK/mounts" > "$WORK/beneath" \
        || refuse "cannot read the mount table for mounts beneath $SRC"
    [ ! -s "$WORK/beneath" ] \
        || refuse "something is mounted beneath $SRC ($(tr '\n' ' ' < "$WORK/beneath")) — a copy would cross into it"
    [ "$RUNNING" = true ] || refuse "$SERVICE is not running — the proof reads it before the stop, and this verb does not start a Forgejo someone stopped"
    others_mounting > "$WORK/o" || refuse "docker could not list the running containers' mounts ($(head -c 300 "$WORK/docker.err"))"
    [ ! -s "$WORK/o" ] || refuse "another running container mounts $SRC, under it or above it: $(tr '\n' ' ' < "$WORK/o")— stopping $SERVICE alone would copy data it is still writing"
    proc_readable \
        || refuse "the process table $PROC does not answer for this process ($PROC/self/cwd cannot be read), so a host process holding files under $SRC could not be named after the stop"
    lsattr -d "$SRC" > /dev/null 2> "$WORK/lsattr.err" \
        || refuse "lsattr cannot read $SRC's attributes ($(head -c 300 "$WORK/lsattr.err")), so the empty mountpoint could not be made immutable"

    # The top-level entries, by name and type: stable while Forgejo runs.
    find "$SRC" -mindepth 1 -maxdepth 1 -printf '%y %f\n' > "$WORK/top" 2> "$WORK/find.err" \
        || refuse "cannot list $SRC ($(head -c 300 "$WORK/find.err"))"
    [ -s "$WORK/top" ] || refuse "$SRC is empty — there is nothing to move"
    LC_ALL=C sort -o "$WORK/top" "$WORK/top"
    local ty nm
    while read -r ty nm; do
        [[ "$nm" =~ ^[A-Za-z0-9._-]+$ ]] || refuse "$SRC holds a top-level entry whose name is not plain ('$nm')"
    done < "$WORK/top"
    ENTRIES=$(jq -Rsc 'split("\n") | map(select(length > 0) | split(" ") | {name: .[1], type: .[0]})' "$WORK/top") \
        || refuse "the top-level listing of $SRC did not read as a list"

    # The target: exactly one filesystem labelled boss-data.
    lsblk -J -l -p -o NAME,LABEL,UUID,FSTYPE,MOUNTPOINTS > "$WORK/lsblk" 2> "$WORK/lsblk.err" \
        || refuse "lsblk could not list the block devices ($(head -c 300 "$WORK/lsblk.err"))"
    jq_doc_file "$WORK/lsblk" && jq -e '.blockdevices | type == "array"' "$WORK/lsblk" > /dev/null 2>&1 \
        || refuse "lsblk's listing is not the JSON this verb reads"
    jq -c --arg l "$LABEL" '[.blockdevices[] | select(.label == $l)
        | {uuid: (.uuid // ""), fstype: (.fstype // ""), mounts: [(.mountpoints // [])[] | select(. != null)]}]' \
        "$WORK/lsblk" > "$WORK/labelled" || refuse "cannot read the $LABEL devices out of lsblk's listing"
    local n
    n=$(jq 'length' "$WORK/labelled") || refuse "cannot count the $LABEL devices"
    [ "$n" = 1 ] || refuse "$n filesystems are labelled $LABEL, not exactly one — commission the drive with commission-a-disk, and never label a second one alike"
    TUUID=$(jq -r '.[0].uuid' "$WORK/labelled")
    FSTYPE=$(jq -r '.[0].fstype' "$WORK/labelled")
    [[ "$TUUID" =~ ^[0-9a-f-]{8,64}$ ]] || refuse "the $LABEL filesystem's UUID '$TUUID' is not one this verb can name in fstab"
    [ "$FSTYPE" = ext4 ] || refuse "the $LABEL filesystem is $FSTYPE, not the ext4 commission-a-disk makes"
    n=$(jq '.[0].mounts | length' "$WORK/labelled") || refuse "cannot count the $LABEL filesystem's mounts"
    [ "$n" = 1 ] || refuse "the $LABEL filesystem is mounted $n times, not exactly once"
    MNT=$(jq -r '.[0].mounts[0]' "$WORK/labelled")
    [ "$MNT" != / ] || refuse "the $LABEL filesystem is mounted at / — it would back the root filesystem"
    plain_path "$MNT" || refuse "the $LABEL filesystem is mounted at '$MNT', which is not a plain absolute path"
    [ "$TUUID" != "$ROOT_UUID" ] || refuse "the $LABEL filesystem ($TUUID) is the root filesystem"
    under "$MNT" "$SRC" && refuse "the $LABEL mount $MNT is inside $SRC"
    under "$SRC" "$MNT" && refuse "$SRC is inside the $LABEL mount $MNT"
    local seen
    seen=$(findmnt -nro UUID --mountpoint "$MNT" 2> "$WORK/findmnt.err") \
        || refuse "findmnt does not see a mount at $MNT ($(head -c 300 "$WORK/findmnt.err"))"
    [ "$seen" = "$TUUID" ] || refuse "findmnt says $MNT is UUID '$seen', lsblk says the $LABEL filesystem is $TUUID — two readings disagree"
    UUID_LINE=$(awk -v u="UUID=$TUUID" -v m="$MNT" '$1 == u && $2 == m' "$FSTAB") \
        || refuse "cannot read $FSTAB"
    [ "$(printf '%s\n' "$UUID_LINE" | grep -c .)" = 1 ] \
        || refuse "$FSTAB does not carry exactly one line mounting UUID=$TUUID at $MNT — the bind needs the drive's own by-UUID mount to wait for at boot"

    TGT="$MNT/$TGT_NAME"
    MARKER="$TGT.went-live"
    PRE="$PRE_GLOB${TUUID//-/}"
    PRE="${PRE:0:$((${#PRE_GLOB} + 8))}"
    plain_path "$TGT" && plain_path "$PRE" || refuse "the derived paths '$TGT' / '$PRE' are not plain"
    [ ! -e "$PRE" ] && [ ! -L "$PRE" ] || refuse "$PRE already exists — a pre-move copy is standing; roll it back or delete it before moving again"
    # A target Forgejo has SERVED holds writes that exist nowhere else
    # once a rollback puts Forgejo back on the old copy, and the copy's
    # --delete would destroy them (review of 016aeea3, finding 4). The
    # move marks a target when it goes live, and this refuses a marked
    # one outright — read here whether or not the target directory
    # stands, and a dangling link counts: until the review of the
    # released follow-up car (backlog ed7702c3, finding 4) a marker whose
    # target was gone passed the plan, and only the write's re-check
    # found it, after the ~100 GB copy.
    [ ! -e "$MARKER" ] && [ ! -L "$MARKER" ] \
        || refuse "$TGT was served by Forgejo once ($MARKER, which a move leaves when it goes live, stands whether or not $TGT does), so what Forgejo wrote there exists nowhere else, and a move that copies $SRC over it with --delete would destroy it. Move the marker and its target aside or delete them through an approved step first"
    if [ -e "$TGT" ] || [ -L "$TGT" ]; then
        [ ! -L "$TGT" ] && [ -d "$TGT" ] || refuse "$TGT exists and is not a real directory"
        on=$(findmnt -nro TARGET -T "$TGT" 2> "$WORK/findmnt.err") || refuse "findmnt could not place $TGT"
        [ "$on" = "$MNT" ] || refuse "$TGT is on '$on', not on the $LABEL mount $MNT"
        # A target holding anything newer than the source's newest file
        # is refused too, marker or not. What is left is a copy a failed
        # move left before its rename, which Forgejo never served.
        local newest late
        newest=$(find "$SRC" -printf '%T@\n' 2> "$WORK/find.err" | awk '{ if ($1 > m) m = $1 } END { printf "%d\n", m + 1 }') \
            || refuse "cannot read the newest time under $SRC ($(head -c 300 "$WORK/find.err"))"
        [[ "$newest" =~ ^[0-9]+$ ]] && [ "$newest" -gt 1 ] || refuse "no modification time could be read under $SRC"
        late=$(find "$TGT" -newermt "@$newest" -print -quit 2> "$WORK/find.err") \
            || refuse "cannot read the times under $TGT ($(head -c 300 "$WORK/find.err"))"
        [ -z "$late" ] \
            || refuse "$TGT holds $late, newer than anything in $SRC — it holds writes the source does not, and mirroring over it with --delete would destroy them"
        n=$(find "$TGT" -mindepth 1 -maxdepth 1 -printf . 2> "$WORK/find.err" | wc -c) \
            || refuse "cannot list $TGT ($(head -c 300 "$WORK/find.err"))"
        TGT_STATE="present, $n top-level entries, never served and nothing in it newer than the source — a copy a failed move left before its rename, which the copy mirrors the source over with --delete"
    else
        TGT_STATE="absent"
    fi

    # The size ceiling. du reads a RUNNING Forgejo, so a file can vanish
    # under it; a failed du is retried and never read as a number.
    local alloc="" i
    for i in 1 2 3; do
        if alloc=$(du -sxB1 -- "$SRC" 2> "$WORK/du.err" | awk 'NR == 1 { print $1 }') \
            && [[ "$alloc" =~ ^[0-9]+$ ]]; then
            break
        fi
        alloc=""
    done
    [ -n "$alloc" ] || refuse "du could not size $SRC in three tries ($(head -c 300 "$WORK/du.err"))"
    local bucket=$((BUCKET_GIB * GIB))
    SRC_CEIL_GIB=$(( (alloc + bucket - 1) / bucket * BUCKET_GIB ))
    [ "$SRC_CEIL_GIB" -ge "$BUCKET_GIB" ] || SRC_CEIL_GIB=$BUCKET_GIB
    local avail
    avail=$(df -B1 --output=avail -- "$MNT" 2> "$WORK/df.err" | awk 'NR == 2 { print $1 }') \
        || refuse "df could not read $MNT ($(head -c 300 "$WORK/df.err"))"
    [[ "$avail" =~ ^[0-9]+$ ]] || refuse "df gave no free-space figure for $MNT ('$avail')"
    FREE_GIB=$((avail / GIB))
    [ "$FREE_GIB" -ge $((SRC_CEIL_GIB + MARGIN_GIB)) ] \
        || refuse "$MNT has $FREE_GIB GiB free, and the source's ceiling of $SRC_CEIL_GIB GiB plus the $MARGIN_GIB GiB margin needs $((SRC_CEIL_GIB + MARGIN_GIB))"
    say "measured now (not signed: Forgejo is running): $SRC uses $alloc bytes; $MNT has $avail bytes free"

    FSTAB_LINE=$(fstab_line "$TGT" "$MNT")
    # The act, as the argv the write executes — the plan renders these
    # same arrays (§9a, and the review's point (e)).
    # -S keeps sparse files sparse (a database or a pack preallocated
    # with holes would otherwise land at its full apparent size).
    COPY=(rsync -aHAXS --numeric-ids --delete "$SRC/" "$TGT/")
    VERIFY=(rsync -aHAXSc --numeric-ids --delete --dry-run --itemize-changes "$SRC/" "$TGT/")
}

# ---- the rollback's observations --------------------------------------
observe_rollback() {
    local found=() p
    for p in "$PRE_GLOB"*; do
        [ -e "$p" ] || [ -L "$p" ] || continue
        found+=("$p")
    done
    [ "${#found[@]}" = 1 ] || refuse "${#found[@]} pre-move copies match $PRE_GLOB*, not exactly one: ${found[*]:-none} — there is nothing unambiguous to roll back to"
    PRE="${found[0]}"
    [[ "${PRE#"$PRE_GLOB"}" =~ ^[0-9a-f]{8}$ ]] || refuse "$PRE is not a name this verb makes (pre-move-<8 hex>)"
    [ ! -L "$PRE" ] && [ -d "$PRE" ] || refuse "$PRE is not a real directory"
    local on
    on=$(findmnt -nro TARGET -T "$PRE" 2> "$WORK/findmnt.err") || refuse "findmnt could not place $PRE"
    [ "$on" = "/" ] || refuse "$PRE is on '$on', not on the root filesystem where the move left it"

    # The drive, found from the pre-move name — its suffix is the first 8
    # hex of the drive's UUID — through the drive's own by-UUID fstab
    # line, so it is found whether or not the drive still answers. When
    # lsblk no longer lists it, the drive's errors are the ones
    # fstab_verifies may tolerate (finding 2): the rollback is needed
    # most when the drive is gone.
    local suffix="${PRE#"$PRE_GLOB"}" drive_line n drive_mnt=""
    drive_line=$(awk -v s="$suffix" '$1 ~ /^UUID=/ { u = substr($1, 6); gsub("-", "", u); if (index(u, s) == 1) print }' "$FSTAB") \
        || refuse "cannot read $FSTAB"
    DRIVE_UUID=""
    DRIVE_PRESENT=""
    if [ "$(printf '%s\n' "$drive_line" | grep -c .)" = 1 ]; then
        DRIVE_UUID=$(printf '%s\n' "$drive_line" | awk '{ print substr($1, 6) }')
        drive_mnt=$(printf '%s\n' "$drive_line" | awk '{ print $2 }')
        plain_path "$drive_mnt" || refuse "the drive's fstab line mounts at '$drive_mnt', which is not a plain absolute path"
        lsblk -J -l -p -o NAME,UUID > "$WORK/lsblk" 2> "$WORK/lsblk.err" \
            || refuse "lsblk could not list the block devices ($(head -c 300 "$WORK/lsblk.err"))"
        jq_doc_file "$WORK/lsblk" && jq -e '.blockdevices | type == "array"' "$WORK/lsblk" > /dev/null 2>&1 \
            || refuse "lsblk's listing is not the JSON this verb reads"
        n=$(jq --arg u "$DRIVE_UUID" '[.blockdevices[] | select(.uuid == $u)] | length' "$WORK/lsblk") \
            || refuse "cannot read lsblk's listing for UUID $DRIVE_UUID"
        if [ "$n" = 0 ]; then
            DRIVE_PRESENT=false
            TOLERATED="$drive_mnt $SRC"
        else
            DRIVE_PRESENT=true
        fi
    fi
    fstab_verifies "$FSTAB" \
        || { sed 's/^/    /' "$WORK/verify-out" >&2; refuse "$FSTAB does not verify (above), and not only for an absent drive, so no edit to it can be shown safe"; }

    # WHAT THE REGISTRY LEG PROVES (re-review of the released car,
    # finding 1). The current stamp names whatever the converge built
    # LAST, and after the move every image went into the moved copy, so
    # its manifest 404s off the copy this puts back — a proof that would
    # stop Forgejo for the rollback's own reason. The move recorded the
    # stamp and manifest sha it proved off this very copy in the target's
    # marker, before the rename; that manifest must be served again. The
    # marker is on the drive: with the drive gone, or no marker there,
    # the leg is reported rather than proved, and the plan says why.
    RB_MARKER=""
    PROVE_STAMP=""
    PROVE_MANIFEST=""
    PROVE_WHY=""
    if [ -z "$DRIVE_UUID" ]; then
        PROVE_WHY="$FSTAB carries no drive line for $PRE, so there is no record of the stamp its move proved"
    elif [ "$DRIVE_PRESENT" != true ]; then
        PROVE_WHY="the drive $DRIVE_UUID is gone, and the record of the stamp its move proved ($drive_mnt/$TGT_NAME.went-live) was on it"
    else
        RB_MARKER="$drive_mnt/$TGT_NAME.went-live"
        if [ ! -e "$RB_MARKER" ] && [ ! -L "$RB_MARKER" ]; then
            PROVE_WHY="$RB_MARKER does not exist (the drive is not mounted at $drive_mnt, or the move never reached its rename), so there is no record of the stamp its move proved"
            RB_MARKER=""
        else
            [ -f "$RB_MARKER" ] && [ ! -L "$RB_MARKER" ] || refuse "$RB_MARKER is not a regular file, so it is not the record a move writes"
            read_marker "$RB_MARKER" \
                || refuse "$RB_MARKER does not read as the record a move writes (three lines: plan <sha256>, stamp <sha>, manifest <sha256>), so what the registry leg must prove cannot be said"
        fi
    fi

    FSTAB_LINE=""
    TGT=""
    case "$SRC_LINES" in
        0) ;;
        1)
            local line mnt
            line=$(awk -v s="$SRC" '$1 !~ /^#/ && $2 == s' "$FSTAB") || refuse "cannot read $FSTAB"
            TGT=$(printf '%s\n' "$line" | awk '{ print $1 }')
            mnt=$(printf '%s\n' "$line" | awk '{ print $4 }' | tr ',' '\n' | sed -n 's/^x-systemd.requires-mounts-for=//p')
            plain_path "$TGT" && plain_path "$mnt" \
                || refuse "the $FSTAB line for $SRC is not one this verb writes: $line"
            [ "$TGT" = "$mnt/$TGT_NAME" ] && [ "$line" = "$(fstab_line "$TGT" "$mnt")" ] \
                || refuse "the $FSTAB line for $SRC is not exactly the line this verb writes, so it is not this verb's to remove: $line"
            FSTAB_LINE="$line" ;;
        *) refuse "$FSTAB has $SRC_LINES entries for $SRC — not a state this verb made" ;;
    esac

    MOUNTED=false
    MOUNT_UUID=""
    if is_mounted "$SRC"; then
        local fsroot
        read -r MOUNT_UUID fsroot < <(findmnt -nro UUID,FSROOT --mountpoint "$SRC" 2> "$WORK/findmnt.err") \
            || refuse "findmnt could not read the mount at $SRC ($(head -c 300 "$WORK/findmnt.err"))"
        [ "$fsroot" = "/$TGT_NAME" ] || refuse "$SRC is mounted from '$fsroot', not the /$TGT_NAME bind this verb makes"
        MOUNTED=true
    fi
    if [ -e "$SRC" ] || [ -L "$SRC" ]; then
        [ ! -L "$SRC" ] && [ -d "$SRC" ] || refuse "$SRC is not a directory"
        if [ "$MOUNTED" = true ]; then
            SRC_STATE="the bind mountpoint"
        else
            local n
            n=$(find "$SRC" -mindepth 1 -maxdepth 1 -printf . 2> "$WORK/find.err" | wc -c) \
                || refuse "cannot list $SRC ($(head -c 300 "$WORK/find.err"))"
            [ "$n" = 0 ] || refuse "$SRC is not mounted and holds $n entries — it is not the empty mountpoint this verb leaves, so it is not this verb's to remove"
            SRC_STATE="an empty mountpoint"
        fi
    else
        SRC_STATE="absent"
    fi
    [ -n "$TGT" ] || TGT="(the moved copy's directory; this plan found no fstab line naming it)"
}

# ---- the restart's observations ---------------------------------------
# restart-forgejo exists because a failed proof STOPS Forgejo, and until
# it no verb could start it again (re-review of the released car,
# finding 1): the recovery was a root shell. It starts a stopped
# Forgejo on exactly one of the two states the move and the rollback
# leave at the source — the move's bind, mounted, or a directory on the
# root filesystem holding data — and refuses every other, because a
# Forgejo started on the empty mountpoint or on nothing serves nothing.
observe_restart() {
    [ "$RUNNING" = false ] || refuse "$SERVICE is running — there is nothing to restart"
    MOUNTED=false
    MOUNT_UUID=""
    if is_mounted "$SRC"; then
        local fsroot
        read -r MOUNT_UUID fsroot < <(findmnt -nro UUID,FSROOT --mountpoint "$SRC" 2> "$WORK/findmnt.err") \
            || refuse "findmnt could not read the mount at $SRC ($(head -c 300 "$WORK/findmnt.err"))"
        [ "$fsroot" = "/$TGT_NAME" ] \
            || refuse "$SRC is mounted from '$fsroot', not the /$TGT_NAME bind the move makes — not a state this verb starts Forgejo on"
        [[ "$MOUNT_UUID" =~ ^[0-9a-f-]{8,64}$ ]] || refuse "the mount at $SRC names UUID '$MOUNT_UUID', which is not one this verb can name"
        MOUNTED=true
        SRC_STATE="the move's bind of /$TGT_NAME on the filesystem with UUID $MOUNT_UUID"
    else
        [ -e "$SRC" ] || [ -L "$SRC" ] \
            || refuse "$SRC does not exist, so Forgejo would start on nothing — the rollback (plan-a-forgejo-data-rollback) puts the pre-move copy back"
        [ ! -L "$SRC" ] && [ -d "$SRC" ] || refuse "$SRC is not a real directory"
        local on n
        on=$(findmnt -nro TARGET -T "$SRC" 2> "$WORK/findmnt.err") \
            || refuse "findmnt could not say which filesystem holds $SRC ($(head -c 300 "$WORK/findmnt.err"))"
        [ "$on" = / ] || refuse "$SRC is on the filesystem mounted at '$on', which is neither the move's bind nor the root filesystem"
        n=$(find "$SRC" -mindepth 1 -maxdepth 1 -printf . 2> "$WORK/find.err" | wc -c) \
            || refuse "cannot list $SRC ($(head -c 300 "$WORK/find.err"))"
        [ "$n" != 0 ] \
            || refuse "$SRC is not mounted and is empty — the move's mountpoint without its bind — so Forgejo would serve nothing; mount the bind or roll back first"
        SRC_STATE="a directory on the root filesystem, not a mount"
    fi
    [ -d "$FORGE_REPO" ] \
        || refuse "$FORGE_REPO, the repository the proof reads, is not under $SRC as it stands — not a state this verb starts Forgejo on"

    # WHAT THE APPROVER IS STARTING FORGEJO ON (review of the released
    # follow-up car, backlog ed7702c3, finding 2). The restart's typical
    # case is a move whose proof failed: Forgejo stopped on the bind, the
    # pre-move copy still on /, and the move's hold kept. A plan that
    # named only the bind let an approver start Forgejo on a copy that
    # never passed its proof without the plan saying so. So it names any
    # pre-move copy, the moved copy's served marker and its record, and
    # the converge hold's contents — and, when the hold is a move's or a
    # rollback's own, that the run did not finish. All of it is read, and
    # none of it is a refusal: this is the recovery verb.
    local p
    PRE_COPIES=()
    for p in "$PRE_GLOB"*; do
        [ -e "$p" ] || [ -L "$p" ] || continue
        plain_path "$p" || refuse "a pre-move copy's name '$p' is not a plain path, so the plan cannot name it"
        PRE_COPIES+=("$p")
    done
    RS_MARKER=""
    RS_MARKER_STATE=""
    PROVE_PLAN=""
    PROVE_STAMP=""
    PROVE_MANIFEST=""
    if [ "$SRC_LINES" = 1 ]; then
        local tgt
        tgt=$(awk -v s="$SRC" '$1 !~ /^#/ && $2 == s { print $1 }' "$FSTAB") || refuse "cannot read $FSTAB"
        if plain_path "$tgt"; then
            RS_MARKER="$tgt.went-live"
            if [ ! -e "$RS_MARKER" ] && [ ! -L "$RS_MARKER" ]; then
                RS_MARKER_STATE=absent
            elif [ -f "$RS_MARKER" ] && [ ! -L "$RS_MARKER" ] && read_marker "$RS_MARKER"; then
                RS_MARKER_STATE=record
            else
                RS_MARKER_STATE=unreadable
            fi
        fi
    fi
    HOLD_STATE=none
    HOLD_BODY=""
    UNFINISHED=""
    if [ -e "$HOLD_FILE" ] || [ -L "$HOLD_FILE" ]; then
        HOLD_BODY=$(cat -- "$HOLD_FILE" 2> "$WORK/hold.err") \
            || refuse "the converge hold $HOLD_FILE exists but cannot be read ($(head -c 300 "$WORK/hold.err")), so the plan cannot say what holds it"
        HOLD_STATE=held
        case "$HOLD_BODY" in
            forgejo-data-move-*)
                UNFINISHED="the converge is held by a move-forgejo-data run that did not finish (it releases its hold only once its proof passes), so the copy this starts Forgejo on never passed that move's proof — roll-back-forgejo-data-move puts the pre-move copy back" ;;
            forgejo-data-rollback-*)
                UNFINISHED="the converge is held by a roll-back-forgejo-data-move run that did not finish (it releases its hold only once its proof passes), so the copy this starts Forgejo on never passed that rollback's proof" ;;
        esac
    fi
}

# ---- the delete's observations ------------------------------------------
# delete-forgejo-premove-copy exists because the move leaves its old copy
# on / and nothing could delete it (backlog 192a9003): the root disk got
# no space back. It deletes exactly the move's rename target, and only
# after proving — through Forgejo itself, not from the files — that
# Forgejo serves the moved copy. Every check here is a refusal, read by
# the plan and the write alike; only stable facts reach the plan's bytes.

# The container's /data, and a host path, as device:inode. A read that
# fails, or prints anything else, is unreadable, never a value.
data_inode() {
    local d
    d=$(docker exec -u git "$CID" stat -c %d:%i /data 2> "$WORK/docker.err") || return 1
    [[ "$d" =~ ^[0-9]+:[0-9]+$ ]] || return 1
    printf '%s\n' "$d"
}
host_inode() {
    local h
    h=$(stat -c %d:%i -- "$1" 2> "$WORK/stat.err") || return 1
    [[ "$h" =~ ^[0-9]+:[0-9]+$ ]] || return 1
    printf '%s\n' "$h"
}

# delete_legs — the proof that Forgejo serves the moved copy, read before
# the delete (as refusals) and after it (as failures): healthz; the
# container's /data IS $TGT by device and inode, and is not the copy to
# delete while it stands; the repository it serves is $TGT's
# (serves_from, the move's own leg); and the registry serves the
# manifest of the converge's CURRENT stamp. The current stamp and not
# the marker's recorded one: the recorded stamp is a train the registry
# prune deletes once ten more have landed, while the current one is the
# rollback target the prune always keeps — and after a converge it
# exists only in the moved copy. rc 1 with $LEG_WHY set.
delete_legs() {
    local data tgt victim stamp
    LEG_WHY=""
    healthz_ok || { LEG_WHY="$HEALTHZ does not pass"; return 1; }
    data=$(data_inode) \
        || { LEG_WHY="stat inside $SERVICE could not read /data as device:inode ($(head -c 300 "$WORK/docker.err"))"; return 1; }
    tgt=$(host_inode "$TGT") \
        || { LEG_WHY="stat on this host could not read $TGT as device:inode ($(head -c 300 "$WORK/stat.err"))"; return 1; }
    [ "$data" = "$tgt" ] \
        || { LEG_WHY="the container's /data is device:inode $data and the moved copy $TGT is $tgt — $SERVICE is not serving the moved copy"; return 1; }
    if [ -e "$VICTIM" ]; then
        victim=$(host_inode "$VICTIM") \
            || { LEG_WHY="stat on this host could not read $VICTIM as device:inode ($(head -c 300 "$WORK/stat.err"))"; return 1; }
        [ "$victim" != "$data" ] || { LEG_WHY="$VICTIM is the container's /data — it is the live data"; return 1; }
    fi
    # On stderr, every line: during a plan, stdout is the signed bytes.
    echo "$ME: the container's /data is the moved copy $TGT (device:inode $data)" >&2
    serves_from "$TGT" > "$WORK/serves" \
        || { LEG_WHY="the repository $SERVICE serves is not the moved copy's: $(cat "$WORK/serves")"; return 1; }
    cat "$WORK/serves" >&2
    stamp=$(awk 'NR == 1 { print $1 }' "$STAMP_FILE" 2> /dev/null)
    [[ "$stamp" =~ ^[0-9a-f]{7,64}$ ]] \
        || { LEG_WHY="the converge's stamp $STAMP_FILE holds '${stamp:0:80}', not a sha, so the registry leg has no manifest to read"; return 1; }
    manifest_sha "$stamp" > "$WORK/manifest.sha" \
        || { LEG_WHY="the registry does not serve $REGISTRY:$stamp: $(cat "$WORK/manifest.why")"; return 1; }
    echo "$ME: healthz passes; $REGISTRY:$stamp manifest sha256 $(cat "$WORK/manifest.sha")" >&2
}

# containers_under <dir> — "<id> <source>" for every running container,
# Forgejo's included, that mounts <dir> or anything under it.
containers_under() {
    local id m
    docker ps -q --no-trunc > "$WORK/ps" 2> "$WORK/docker.err" || return 1
    : > "$WORK/under"
    while IFS= read -r id; do
        [ -n "$id" ] || continue
        docker inspect -f '{{range .Mounts}}{{.Source}}{{"\n"}}{{end}}' "$id" > "$WORK/m" 2> "$WORK/docker.err" || return 1
        while IFS= read -r m; do
            [ -n "$m" ] && under "$m" "$1" && echo "$id $m" >> "$WORK/under"
        done < "$WORK/m"
    done < "$WORK/ps"
    cat "$WORK/under"
}

observe_delete() {
    [ "$RUNNING" = true ] \
        || refuse "$SERVICE is not running — the delete proves Forgejo serves the moved copy before it deletes the other one, and a stopped Forgejo proves nothing"

    # The bind, exactly as the move writes it, and mounted as that bind.
    [ "$SRC_LINES" = 1 ] \
        || refuse "$FSTAB has $SRC_LINES entries for $SRC, not the one bind line the move writes — with no move made there is no pre-move copy"
    local line fsroot seen on
    line=$(awk -v s="$SRC" '$1 !~ /^#/ && $2 == s' "$FSTAB") || refuse "cannot read $FSTAB"
    TGT=$(printf '%s\n' "$line" | awk '{ print $1 }')
    MNT=$(printf '%s\n' "$line" | awk '{ print $4 }' | tr ',' '\n' | sed -n 's/^x-systemd.requires-mounts-for=//p')
    plain_path "$TGT" && plain_path "$MNT" || refuse "the $FSTAB line for $SRC is not one the move writes: $line"
    [ "$TGT" = "$MNT/$TGT_NAME" ] && [ "$line" = "$(fstab_line "$TGT" "$MNT")" ] \
        || refuse "the $FSTAB line for $SRC is not exactly the line the move writes: $line"
    FSTAB_LINE="$line"
    fstab_verifies "$FSTAB" \
        || { sed 's/^/    /' "$WORK/verify-out" >&2; refuse "$FSTAB does not verify clean (above)"; }
    is_mounted "$SRC" \
        || refuse "$SRC is not mounted — the bind in $FSTAB is not up, so Forgejo is not serving the moved copy through it"
    read -r TUUID fsroot < <(findmnt -nro UUID,FSROOT --mountpoint "$SRC" 2> "$WORK/findmnt.err") \
        || refuse "findmnt could not read the mount at $SRC ($(head -c 300 "$WORK/findmnt.err"))"
    [ "$fsroot" = "/$TGT_NAME" ] || refuse "$SRC is mounted from '$fsroot', not the /$TGT_NAME bind the move makes"
    [[ "$TUUID" =~ ^[0-9a-f-]{8,64}$ ]] || refuse "the mount at $SRC names UUID '$TUUID', which is not one this verb can name"
    [ "$TUUID" != "$ROOT_UUID" ] || refuse "the bind at $SRC is on the root filesystem ($TUUID) — nothing has moved off it"
    is_mounted "$MNT" || refuse "the drive's mount $MNT is not mounted"
    seen=$(findmnt -nro UUID --mountpoint "$MNT" 2> "$WORK/findmnt.err") \
        || refuse "findmnt does not see a mount at $MNT ($(head -c 300 "$WORK/findmnt.err"))"
    [ "$seen" = "$TUUID" ] || refuse "findmnt says $MNT is UUID '$seen', and the bind at $SRC is UUID $TUUID — two readings disagree"
    UUID_LINE=$(awk -v u="UUID=$TUUID" -v m="$MNT" '$1 == u && $2 == m' "$FSTAB") || refuse "cannot read $FSTAB"
    [ "$(printf '%s\n' "$UUID_LINE" | grep -c .)" = 1 ] \
        || refuse "$FSTAB does not carry exactly one line mounting UUID=$TUUID at $MNT"
    [ ! -L "$TGT" ] && [ -d "$TGT" ] || refuse "$TGT is not a real directory"
    on=$(findmnt -nro TARGET -T "$TGT" 2> "$WORK/findmnt.err") || refuse "findmnt could not place $TGT"
    [ "$on" = "$MNT" ] || refuse "$TGT is on '$on', not on the drive's mount $MNT"

    # The marker the move leaves when it goes live, and the soak since.
    MARKER="$TGT.went-live"
    [ -e "$MARKER" ] || [ -L "$MARKER" ] \
        || refuse "$MARKER does not exist — no move put $TGT live, so no copy on / is the one it left"
    [ -f "$MARKER" ] && [ ! -L "$MARKER" ] || refuse "$MARKER is not a regular file, so it is not the record a move writes"
    read_marker "$MARKER" \
        || refuse "$MARKER does not read as the record a move writes (three lines: plan <sha256>, stamp <sha>, manifest <sha256>)"
    local now
    MARKED_AT=$(stat -c %Y -- "$MARKER" 2> "$WORK/stat.err") || refuse "cannot read $MARKER's time ($(head -c 300 "$WORK/stat.err"))"
    now=$(date +%s) || refuse "date could not read the clock"
    [[ "$MARKED_AT" =~ ^[0-9]+$ ]] && [[ "$now" =~ ^[0-9]+$ ]] || refuse "the marker's time or the clock did not read as seconds"
    DELETABLE_FROM=$((MARKED_AT + SOAK_HOURS * 3600))
    MARKED_ISO=$(date -u -d "@$MARKED_AT" +%Y-%m-%dT%H:%M:%SZ) && DELETABLE_ISO=$(date -u -d "@$DELETABLE_FROM" +%Y-%m-%dT%H:%M:%SZ) \
        || refuse "date could not render the marker's time"
    [ "$now" -ge "$DELETABLE_FROM" ] \
        || refuse "the move went live at $MARKED_ISO ($MARKER), and the $SOAK_HOURS h soak — the window in which roll-back-forgejo-data-move may still need the copy on / — runs until $DELETABLE_ISO"

    # A move or a rollback that has not finished may still need it.
    local hold
    if [ -e "$HOLD_FILE" ] || [ -L "$HOLD_FILE" ]; then
        hold=$(cat -- "$HOLD_FILE" 2> "$WORK/hold.err") \
            || refuse "the converge hold $HOLD_FILE exists but cannot be read ($(head -c 300 "$WORK/hold.err")), so whether a move or a rollback is unfinished cannot be said"
        case "$hold" in
            forgejo-data-move-*|forgejo-data-rollback-*)
                refuse "the converge is held by '$hold' — a move or a rollback that did not finish (each releases its hold only once its proof passes), and the copy on / is what it may still need" ;;
        esac
    fi

    # The copy: exactly the move's rename target, derived the way the
    # move derived it, or — when a delete was stopped part-way — its
    # deleting- twin, and never both, never another.
    local p found=()
    PRE="$PRE_GLOB${TUUID//-/}"
    PRE="${PRE:0:$((${#PRE_GLOB} + 8))}"
    DOOMED="$SRC.deleting-${PRE#"$PRE_GLOB"}"
    plain_path "$PRE" && plain_path "$DOOMED" || refuse "the derived names '$PRE' / '$DOOMED' are not plain"
    for p in "$PRE_GLOB"* "$SRC.deleting-"*; do
        [ -e "$p" ] || [ -L "$p" ] || continue
        found+=("$p")
    done
    case "${#found[@]}:${found[*]:-}" in
        "1:$PRE")    VICTIM="$PRE";    VICTIM_STATE="the pre-move copy, whole" ;;
        "1:$DOOMED") VICTIM="$DOOMED"; VICTIM_STATE="the remainder of an interrupted delete" ;;
        0:*) refuse "there is no pre-move copy: nothing matches $PRE_GLOB* or $SRC.deleting-* — it is deleted, or was never made" ;;
        *) refuse "${#found[@]} path(s) match, not exactly $PRE (the move's rename target, from the drive UUID $TUUID) or its interrupted delete $DOOMED: ${found[*]}" ;;
    esac
    [ ! -L "$VICTIM" ] && [ -d "$VICTIM" ] || refuse "$VICTIM is not a real directory"
    is_mounted "$VICTIM" && refuse "$VICTIM is itself a mount"
    awk -v p="$VICTIM/" 'index($0, p) == 1' "$WORK/mounts" > "$WORK/beneath" \
        || refuse "cannot read the mount table for mounts beneath $VICTIM"
    [ ! -s "$WORK/beneath" ] || refuse "something is mounted beneath $VICTIM ($(tr '\n' ' ' < "$WORK/beneath"))"
    on=$(findmnt -nro TARGET -T "$VICTIM" 2> "$WORK/findmnt.err") \
        || refuse "findmnt could not say which filesystem holds $VICTIM ($(head -c 300 "$WORK/findmnt.err"))"
    [ "$on" = / ] || refuse "$VICTIM is on the filesystem mounted at '$on', not on the root filesystem where the move left it"
    under "$VICTIM" "$MNT" && refuse "$VICTIM is inside the drive's mount $MNT"
    under "$MNT" "$VICTIM" && refuse "the drive's mount $MNT is inside $VICTIM"
    containers_under "$VICTIM" > "$WORK/o" || refuse "docker could not list the running containers' mounts ($(head -c 300 "$WORK/docker.err"))"
    [ ! -s "$WORK/o" ] || refuse "a running container mounts $VICTIM or something under it: $(tr '\n' ' ' < "$WORK/o")"
    holders_under "$VICTIM" > "$WORK/holders" \
        || refuse "the process table $PROC does not answer for this process, so a host process holding files under $VICTIM could not be named"
    [ ! -s "$WORK/holders" ] \
        || refuse "host processes hold files under $VICTIM (pid, command, path): $(tr '\n' ';' < "$WORK/holders")"

    # Forgejo serves the moved copy, proved through Forgejo.
    delete_legs || refuse "$LEG_WHY"

    # What goes, exactly: nothing writes the copy, so its size is exact.
    VICTIM_BYTES=$(du -sxB1 -- "$VICTIM" 2> "$WORK/du.err" | awk 'NR == 1 { print $1 }') \
        || refuse "du could not size $VICTIM ($(head -c 300 "$WORK/du.err"))"
    [[ "$VICTIM_BYTES" =~ ^[0-9]+$ ]] || refuse "du gave no size for $VICTIM ('$VICTIM_BYTES')"
    VICTIM_ENTRIES=$(find "$VICTIM" -mindepth 1 -printf . 2> "$WORK/find.err" | wc -c) \
        || refuse "cannot count the entries under $VICTIM ($(head -c 300 "$WORK/find.err"))"
    find "$VICTIM" -mindepth 1 -maxdepth 1 -printf '%y %f\n' > "$WORK/top" 2> "$WORK/find.err" \
        || refuse "cannot list $VICTIM ($(head -c 300 "$WORK/find.err"))"
    LC_ALL=C sort -o "$WORK/top" "$WORK/top"
    ENTRIES=$(jq -Rsc 'split("\n") | map(select(length > 0) | capture("^(?<type>.) (?<name>.*)$"))' "$WORK/top") \
        || refuse "the top-level listing of $VICTIM did not read as a list"
    [[ "$SLACK_FLOOR" =~ ^[0-9]+$ ]] || refuse "the slack floor '$SLACK_FLOOR' is not a number of bytes"
    local slack=$((VICTIM_BYTES / 10))
    [ "$slack" -ge "$SLACK_FLOOR" ] || slack=$SLACK_FLOOR
    NEED=$((VICTIM_BYTES - slack))
    [ "$NEED" -ge 0 ] || NEED=0

    if [ "$VICTIM" = "$PRE" ]; then RENAME=(mv -T -- "$PRE" "$DOOMED"); else RENAME=(); fi
    REMOVE=(rm -rf --one-file-system -- "$DOOMED")
}

# ---- the one render per plan ------------------------------------------
# An argv as a JSON array. Not `jq --args`: jq-1.6 still parses a
# positional that starts with a dash (`-aHAX`) as an option. Every word
# here is a plain path or a flag, so one per line is exact.
json_words() { printf '%s\n' "$@" | jq -R . | jq -sc .; }
# To a FILE, never through $( ), which would strip the trailing newline
# the runner hashed.
render_move() {
    jq -n --arg src "$SRC" --arg pre "$PRE" --arg tgt "$TGT" --arg tgt_name "$TGT_NAME" \
          --arg mnt "$MNT" --arg uuid "$TUUID" --arg fstype "$FSTYPE" --arg fslabel "$LABEL" \
          --arg uuid_line "$UUID_LINE" --arg tgt_state "$TGT_STATE" --arg marker "$MARKER" \
          --arg restart_policy "$RESTART_POLICY" \
          --arg src_ceil_gib "$SRC_CEIL_GIB" --arg bucket_gib "$BUCKET_GIB" \
          --arg free_gib "$FREE_GIB" --arg margin_gib "$MARGIN_GIB" \
          --argjson entries "$ENTRIES" \
          --arg compose "$FORGE_COMPOSE" --arg service "$SERVICE" --arg fstab "$FSTAB" \
          --arg hold_file "$HOLD_FILE" --arg fstab_line "$FSTAB_LINE" \
          --arg healthz "$HEALTHZ" --arg repo "$REPO_IN_CONTAINER" --arg registry "$REGISTRY" \
          --arg repo_host "$FORGE_REPO" --arg repo_from "$FORGE_REPO_FROM" \
          --argjson copy "$(json_words "${COPY[@]}")" \
          --argjson verify "$(json_words "${VERIFY[@]}")" \
          -f "$HERE/move-forgejo-data.plan.jq" > "$WORK/plan"
}
render_rollback() {
    jq -n --arg src "$SRC" --arg pre "$PRE" --arg tgt "$TGT" --arg fstab "$FSTAB" \
          --arg fstab_line "$FSTAB_LINE" --arg mounted "$MOUNTED" --arg mount_uuid "$MOUNT_UUID" \
          --arg src_state "$SRC_STATE" --arg running "$RUNNING" --arg restart_policy "$RESTART_POLICY" \
          --arg drive_uuid "$DRIVE_UUID" --arg drive_present "$DRIVE_PRESENT" \
          --arg compose "$FORGE_COMPOSE" --arg service "$SERVICE" --arg hold_file "$HOLD_FILE" \
          --arg healthz "$HEALTHZ" --arg repo "$REPO_IN_CONTAINER" --arg registry "$REGISTRY" \
          --arg repo_host "$FORGE_REPO" --arg repo_from "$FORGE_REPO_FROM" \
          --arg record "$RB_MARKER" --arg prove_stamp "$PROVE_STAMP" \
          --arg prove_manifest "$PROVE_MANIFEST" --arg prove_why "$PROVE_WHY" \
          -f "$HERE/move-forgejo-data.rollback.plan.jq" > "$WORK/plan"
}
render_restart() {
    local pre_copies='[]'
    if [ "${#PRE_COPIES[@]}" != 0 ]; then
        pre_copies=$(json_words "${PRE_COPIES[@]}") || return 1
    fi
    jq -n --arg src "$SRC" --arg src_state "$SRC_STATE" --arg mounted "$MOUNTED" \
          --arg mount_uuid "$MOUNT_UUID" --arg restart_policy "$RESTART_POLICY" \
          --arg compose "$FORGE_COMPOSE" --arg service "$SERVICE" --arg hold_file "$HOLD_FILE" \
          --arg healthz "$HEALTHZ" --arg repo "$REPO_IN_CONTAINER" \
          --arg repo_host "$FORGE_REPO" --arg repo_from "$FORGE_REPO_FROM" \
          --argjson pre_copies "$pre_copies" \
          --arg marker "$RS_MARKER" --arg marker_state "$RS_MARKER_STATE" \
          --arg marker_plan "$PROVE_PLAN" --arg marker_stamp "$PROVE_STAMP" \
          --arg marker_manifest "$PROVE_MANIFEST" \
          --arg hold_state "$HOLD_STATE" --arg hold_body "$HOLD_BODY" --arg unfinished "$UNFINISHED" \
          -f "$HERE/move-forgejo-data.restart.plan.jq" > "$WORK/plan"
}
render_delete() {
    local rename='[]'
    if [ "${#RENAME[@]}" != 0 ]; then
        rename=$(json_words "${RENAME[@]}") || return 1
    fi
    jq -n --arg src "$SRC" --arg victim "$VICTIM" --arg victim_state "$VICTIM_STATE" \
          --arg pre "$PRE" --arg doomed "$DOOMED" \
          --arg bytes "$VICTIM_BYTES" --arg entries "$VICTIM_ENTRIES" --argjson top "$ENTRIES" \
          --arg tgt "$TGT" --arg tgt_name "$TGT_NAME" --arg mnt "$MNT" --arg uuid "$TUUID" \
          --arg uuid_line "$UUID_LINE" --arg fstab_line "$FSTAB_LINE" --arg fstab "$FSTAB" \
          --arg marker "$MARKER" --arg marker_plan "$PROVE_PLAN" --arg marker_stamp "$PROVE_STAMP" \
          --arg marker_manifest "$PROVE_MANIFEST" --arg marked_at "$MARKED_ISO" \
          --arg soak_hours "$SOAK_HOURS" --arg deletable_from "$DELETABLE_ISO" \
          --arg compose "$FORGE_COMPOSE" --arg service "$SERVICE" \
          --arg healthz "$HEALTHZ" --arg repo "$REPO_IN_CONTAINER" --arg registry "$REGISTRY" \
          --arg repo_host "$FORGE_REPO" --arg repo_from "$FORGE_REPO_FROM" \
          --arg need "$NEED" \
          --argjson rename "$rename" \
          --argjson remove "$(json_words "${REMOVE[@]}")" \
          -f "$HERE/move-forgejo-data.delete.plan.jq" > "$WORK/plan"
}

# ---- the proof: what Forgejo SERVES, read the same way before and after
# Every leg reads through Forgejo itself — its HTTP health, git inside
# its container, its registry over HTTP — because the claim is that
# Forgejo serves the moved bytes, not that the bytes exist somewhere.
healthz_ok() {
    local code
    code=$(curl -sS --max-time 10 -o "$WORK/healthz" -w '%{http_code}' "$HEALTHZ" 2> "$WORK/curl.err") || return 1
    [ "$code" = 200 ] && jq_doc_file "$WORK/healthz" \
        && [ "$(jq -r '.status // empty' "$WORK/healthz" 2> /dev/null)" = pass ]
}
main_sha() { # -> refs/heads/main's sha, as git inside the container reads it
    local s
    s=$(docker exec -u git "$CID" git --git-dir="$REPO_IN_CONTAINER" rev-parse --verify --quiet refs/heads/main 2> "$WORK/git.err") || return 1
    [[ "$s" =~ ^[0-9a-f]{40,64}$ ]] || return 1
    printf '%s\n' "$s"
}
# manifest_sha <tag> — sha256 of the manifest body the registry serves
# for <tag>, minted the way docker (and prune-registry-versions.sh)
# mints a bearer. The credential rides curl config files, never argv,
# and nothing here prints it. On failure, $WORK/manifest.why says why.
manifest_sha() {
    local code bearer
    : > "$WORK/manifest.why"
    code=$(curl -sS --max-time 30 -K "$WORK/basic.cfg" -o "$WORK/token.json" -w '%{http_code}' \
        "http://$REG_HOST/v2/token?service=container_registry" 2> "$WORK/curl.err") \
        || { echo "the token request failed: $(head -c 300 "$WORK/curl.err")" > "$WORK/manifest.why"; return 1; }
    bearer=$(jq -r '.token // empty' "$WORK/token.json" 2> /dev/null)
    [ "$code" = 200 ] && [ -n "$bearer" ] \
        || { echo "/v2/token answered HTTP $code with no token" > "$WORK/manifest.why"; return 1; }
    ( umask 077; printf 'header = "Authorization: Bearer %s"\n' "$bearer" > "$WORK/bearer.cfg" )
    code=$(curl -sS --max-time 60 -K "$WORK/bearer.cfg" \
        -H 'Accept: application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.list.v2+json, application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json' \
        -o "$WORK/manifest" -w '%{http_code}' \
        "http://$REG_HOST/v2/$REG_OWNER/$REG_NAME/manifests/$1" 2> "$WORK/curl.err") \
        || { echo "the manifest request failed: $(head -c 300 "$WORK/curl.err")" > "$WORK/manifest.why"; return 1; }
    [ "$code" = 200 ] && [ -s "$WORK/manifest" ] \
        || { echo "$REGISTRY:$1 answered HTTP $code" > "$WORK/manifest.why"; return 1; }
    sha256sum "$WORK/manifest" | cut -c1-64
}
# host_main_sha <bare repo> — refs/heads/main as host git reads it, off
# the disk, with Forgejo stopped. The move's BEFORE half is read here
# and not through Forgejo before the stop, so a train merging between
# that read and the stop cannot fail the proof falsely (finding 5).
# safe.directory, because the repository is the git user's and this
# runs as root.
host_main_sha() {
    local s
    s=$(git -c safe.directory='*' --git-dir="$1" rev-parse --verify --quiet refs/heads/main 2> "$WORK/git.err") || return 1
    [[ "$s" =~ ^[0-9a-f]{40,64}$ ]] || return 1
    printf '%s\n' "$s"
}
# serves_from <dir> — the repository the container sees at /data is
# <dir>'s own: the same device and inode. The healthz, main and manifest
# legs read alike off the pre-move copy and the moved one, so none of
# them can say WHICH copy Forgejo serves; this leg can (finding 1).
# It is read against the source in the move's before half as well, as a
# refusal, so an image whose stat is missing or prints otherwise is
# found while nothing has moved — not by a proof that stops Forgejo
# (re-review of the released car, finding 2). Each side must read as
# <digits>:<digits>, or the leg is unreadable, never a match.
serves_from() {
    local rel inside="" host=""
    rel="${FORGE_REPO#"$SRC"}"
    if ! inside=$(docker exec -u git "$CID" stat -c %d:%i "$REPO_IN_CONTAINER" 2> "$WORK/docker.err") \
        || [[ ! "$inside" =~ ^[0-9]+:[0-9]+$ ]]; then
        echo "$ME: stat inside $SERVICE could not read $REPO_IN_CONTAINER as device:inode (it answered '${inside:0:200}';$(head -c 300 "$WORK/docker.err"))"
        return 1
    fi
    if ! host=$(stat -c %d:%i -- "$1$rel" 2> "$WORK/stat.err") \
        || [[ ! "$host" =~ ^[0-9]+:[0-9]+$ ]]; then
        echo "$ME: stat on this host could not read $1$rel as device:inode ($(head -c 300 "$WORK/stat.err"))"
        return 1
    fi
    echo "$ME: the container's $REPO_IN_CONTAINER is device:inode $inside; $1$rel is $host"
    [ "$inside" = "$host" ]
}
# still_stopped <when> — the container is exactly as the stop left it:
# not running, and neither started nor restarted since (finding 1).
# Before the commit point a failure here leaves the EXIT trap to start
# it again on the untouched source. After it, a Forgejo started behind
# this verb's back may be serving the empty mountpoint, the bind before
# its proof, or a copy half put back — so it is stopped first, like a
# failed proof (re-review of the released car, finding 6).
still_stopped() {
    local now why
    if ! now=$(container_facts "$CID"); then
        why="docker could not read $SERVICE's state $1 ($(head -c 300 "$WORK/docker.err"))"
    elif [ "$now" != "$FACTS_STOPPED" ]; then
        why="$SERVICE is not as the stop left it, $1: it read '$FACTS_STOPPED' (running, StartedAt, RestartCount) at the stop and reads '$now' now — something started it again (restart policy $RESTART_POLICY), so it may have written to $SRC after the copy was checked"
    else
        echo "$ME: $SERVICE is still as the stop left it, $1 ($now)"
        return 0
    fi
    [ "$STAGE" = committed ] && stop_then_fail "$why"
    fail "$why"
}
# stop_then_fail <why> — Forgejo is stopped, then the run fails: a
# Forgejo whose data this run cannot vouch for must not take pushes
# meanwhile (finding 5). restart-forgejo is the approved way back.
stop_then_fail() {
    say "stopping $SERVICE: $*"
    if docker compose -f "$FORGE_COMPOSE" stop -t 60 "$SERVICE" > "$WORK/stop" 2>&1; then
        say "  $SERVICE is stopped; restart-forgejo (plan-a-forgejo-restart) starts it again under an approved plan"
    else
        sed 's/^/    /' "$WORK/stop" >&2
        say "  COULD NOT stop $SERVICE — it is RUNNING on data this run could not vouch for"
    fi
    fail "$*"
}
proof_fail() { stop_then_fail "the proof failed — $*"; }
# real_start — `compose start` of a container already running is a
# no-op that succeeds, and a proof read off it proves nothing about
# this start: StartedAt must have moved since the stop.
real_start() {
    local now r s was
    now=$(container_facts "$CID") || proof_fail "docker could not read $SERVICE's state after the start"
    read -r r s _ <<< "$now"
    read -r _ was _ <<< "$FACTS_STOPPED"
    [ "$r" = true ] && [ "$s" != "$was" ] \
        || proof_fail "the start was not a real start: $SERVICE reads '$now' after it and read '$FACTS_STOPPED' at the stop"
    echo "$ME: $SERVICE started (StartedAt $s)"
}
wait_healthy() {
    local waited=0
    until healthz_ok; do
        [ "$waited" -lt "$HEALTH_WAIT" ] || return 1
        sleep 5
        waited=$((waited + 5))
    done
    echo "$ME: $HEALTHZ passes (after ${waited}s)"
}
root_avail() { df -B1 --output=avail / 2> /dev/null | awk 'NR == 2 { print $1 }'; }

# census <dir> — per top-level entry: name, entries under it, bytes of
# its regular files. Read after the stop, of both sides, and compared
# whole: the verify pass says the trees are identical, and this says so
# again in numbers a reader can check.
census() {
    local nm
    find "$1" -mindepth 1 -maxdepth 1 -printf '%f\n' > "$WORK/names" 2> "$WORK/find.err" || return 1
    LC_ALL=C sort -o "$WORK/names" "$WORK/names" || return 1
    while IFS= read -r nm; do
        find "$1/$nm" -printf '%y %s\n' 2> "$WORK/find.err" \
            | awk -v n="$nm" '{ c++ } $1 == "f" { b += $2 } END { printf "%s %.0f %.0f\n", n, c, b }' \
            || return 1
    done < "$WORK/names"
}

# ---- the hold ----------------------------------------------------------
place_hold() { # <reason>
    if [ -f "$HOLD_FILE" ]; then
        say "the converge is already held ($(cat "$HOLD_FILE" 2> /dev/null)) — that hold stands, and this verb will not release it"
        return 0
    fi
    bash "$HERE/converge-hold.sh" hold "$1" > "$WORK/hold" 2>&1 || { cat "$WORK/hold" >&2; return 1; }
    HELD_BY_US=1
    HOLD_REASON="$1"
    cat "$WORK/hold"
}
release_own_hold() {
    [ "$HELD_BY_US" = 1 ] || return 0
    if [ "$(cat "$HOLD_FILE" 2> /dev/null)" != "$HOLD_REASON" ]; then
        say "the converge hold is no longer this verb's ($HOLD_FILE) — left as it stands"
        return 0
    fi
    if bash "$HERE/converge-hold.sh" release > "$WORK/release" 2>&1; then
        HELD_BY_US=0
        cat "$WORK/release"
    else
        cat "$WORK/release" >&2
        say "could not release the converge hold $HOLD_REASON — release-converge lifts it"
    fi
}

# ---- what stands, when a write stops part-way ---------------------------
state_report() {
    local s
    say "what stands now:"
    [ -d "${PRE:-/nonexistent}" ] && say "  the pre-move copy $PRE exists"
    if ! findmnt -rno TARGET > "$WORK/mounts-now" 2> /dev/null; then
        say "  whether $SRC is mounted: unknown (findmnt could not list the mounts)"
    elif grep -qxF -- "$SRC" "$WORK/mounts-now"; then
        say "  $SRC is mounted ($(findmnt -nro SOURCE --mountpoint "$SRC" 2> /dev/null))"
    elif [ -d "$SRC" ]; then
        say "  $SRC is a directory, not mounted"
    else
        say "  $SRC does not exist"
    fi
    if [ -r "$FSTAB" ] && [ -n "${FSTAB_LINE:-}" ] && grep -qxF -- "$FSTAB_LINE" "$FSTAB"; then
        say "  $FSTAB carries: $FSTAB_LINE"
    else
        say "  $FSTAB carries no bind line for $SRC"
    fi
    s=$(running_state "$CID") || s="unknown (docker did not say)"
    say "  $SERVICE running: $s"
    if [ -f "$HOLD_FILE" ]; then say "  the converge is HELD: $(cat "$HOLD_FILE" 2> /dev/null)"; else say "  the converge is not held"; fi
}

finish() {
    local rc=$?
    trap - EXIT
    # THE RENAME IS THE COMMIT POINT, WHATEVER STAGE SAYS (review of the
    # released follow-up car, backlog ed7702c3, finding 1). A TERM, INT
    # or HUP that lands after `mv -T` returns and before STAGE=committed
    # reaches here as `mutating`, and the pre-rename branch would start
    # Forgejo on a source that no longer exists — compose creates a
    # missing bind path, so a fresh EMPTY Forgejo on the root disk —
    # release the hold and remove the marker of a run that renamed.
    # rename(2) is atomic, so the tree says which side of it this run
    # stopped on: the source gone, or the pre-move name taken, is past it.
    if [ "$STAGE" = mutating ] && [ "$RENAMING" = 1 ] \
        && { { [ ! -e "$SRC" ] && [ ! -L "$SRC" ]; } || [ -e "$PRE" ] || [ -L "$PRE" ]; }; then
        say "the rename of $SRC to $PRE had happened when this run stopped — it is past the commit point"
        STAGE=committed
    fi
    case "$STAGE" in
        mutating)
            say "restoring — nothing the old layout depends on has changed yet:"
            if [ "$STOPPED" = 1 ]; then
                if docker compose -f "$FORGE_COMPOSE" start "$SERVICE" > "$WORK/start" 2>&1; then
                    say "  started $SERVICE again on the untouched $SRC"
                else
                    sed 's/^/    /' "$WORK/start" >&2
                    say "  COULD NOT start $SERVICE again — it is STOPPED on the untouched $SRC: docker compose -f $FORGE_COMPOSE start $SERVICE"
                fi
            fi
            release_own_hold
            # The marker is written just before the rename; a failure
            # between the two leaves a target Forgejo never served, so
            # the marker this run wrote is not left to refuse a retry.
            if [ "$MARKER_WRITTEN" = 1 ]; then
                if rm -f -- "$MARKER"; then
                    say "  removed $MARKER: the rename never happened, so Forgejo never served $TGT"
                else
                    say "  COULD NOT remove $MARKER — a retry will refuse $TGT as served, though the rename never happened"
                fi
            fi
            [ -d "${TGT:-/nonexistent}" ] && say "  a partial copy may stand in $TGT; a retry mirrors the source over it"
            [ "$rc" != 0 ] || rc=1 ;;
        committed)
            state_report
            say "  nothing is undone by guess: the converge hold stays, and the rollback is its own approved verb — plan-a-forgejo-data-rollback renders it, roll-back-forgejo-data-move runs it; a stopped $SERVICE is started again by restart-forgejo (plan-a-forgejo-restart)"
            [ "$rc" != 0 ] || rc=1 ;;
        restarting)
            state_report
            say "  restart-forgejo changed nothing but the start, and it does not stop $SERVICE again; the converge hold is as it found it"
            [ "$rc" != 0 ] || rc=1 ;;
        deleting)
            local s
            say "what stands now:"
            if [ -e "$PRE" ] || [ -L "$PRE" ]; then
                say "  $PRE stands, whole: it was never renamed, so nothing was deleted"
            elif [ -e "$DOOMED" ] || [ -L "$DOOMED" ]; then
                say "  $DOOMED stands, partly deleted: no rollback plan can find it, and plan-a-forgejo-premove-delete renders the delete that finishes it"
            else
                say "  $VICTIM is gone"
            fi
            say "  / has $(root_avail) bytes free; it had $ROOT_BEFORE before the delete"
            s=$(running_state "$CID") || s="unknown (docker did not say)"
            say "  $SERVICE running: $s — this verb never stops or starts it"
            [ "$rc" != 0 ] || rc=1 ;;
    esac
    [ -n "$NEWTAB" ] && rm -f -- "$NEWTAB"
    rm -rf -- "$WORK"
    exit "$rc"
}
trap finish EXIT
trap 'exit 1' TERM INT HUP

# ---- render, and for a plan, print it ------------------------------------
case "$MODE" in
    plan|move)
        observe_move
        render_move || refuse "the plan template did not render" ;;
    plan-rollback|rollback)
        observe_rollback
        render_rollback || refuse "the rollback plan template did not render" ;;
    plan-restart|restart)
        observe_restart
        render_restart || refuse "the restart plan template did not render" ;;
    plan-delete|delete)
        observe_delete
        render_delete || refuse "the delete plan template did not render" ;;
esac
HASH=$(sha256sum "$WORK/plan" | cut -c1-64)
case "$MODE" in
    plan|plan-rollback|plan-restart|plan-delete)
        cat "$WORK/plan"
        echo "plan-sha256: $HASH" >&2
        exit 0 ;;
esac

# ---- the write, only under the plan that was approved -------------------
# Everything the plan names was observed moments ago by this run and
# rendered with the command --plan uses: a match means the host is in
# exactly the state the approver saw; a mismatch changes nothing.
if [ "$HASH" != "$APPROVED" ]; then
    cat "$WORK/plan" >&2
    refuse "the plan now hashes to $HASH, not the approved $APPROVED — something it names has moved since it was signed (above: the plan as it stands). Render and approve it again"
fi
cat "$WORK/plan"
echo "$ME: plan $HASH still holds"
HASH12="${HASH:0:12}"
ROOT_BEFORE=$(root_avail)

move_write() {
    STAMP=$(awk 'NR == 1 { print $1 }' "$STAMP_FILE") || refuse "cannot read the converge's stamp $STAMP_FILE"
    [[ "$STAMP" =~ ^[0-9a-f]{7,64}$ ]] || refuse "the converge's stamp $STAMP_FILE holds '$STAMP', not a sha — there is no known manifest to prove the registry with"
    # The proof's BEFORE half — read while nothing has changed, so a
    # failure here is still a refusal.
    healthz_ok || refuse "$HEALTHZ does not pass before anything is touched — a forge that is already unwell is not one to move"
    main_sha > /dev/null || refuse "git inside $SERVICE could not read refs/heads/main of $REPO_IN_CONTAINER ($(head -c 300 "$WORK/git.err")), so the proof could not be read after the start"
    # The served-copy leg, against the source as it stands: the proof
    # reads this same leg after the start, and there a failure stops
    # Forgejo — so it is proved to work here, where a failure changes
    # nothing (re-review of the released car, finding 2).
    serves_from "$SRC" \
        || refuse "the served-copy leg of the proof does not tie the running $SERVICE's /data to $SRC before anything is touched (above) — after the start the same leg would stop Forgejo"
    MANIFEST_BEFORE=$(manifest_sha "$STAMP") || refuse "the registry proof cannot be read before the move: $(cat "$WORK/manifest.why")"
    echo "$ME: before: healthz passes; $REGISTRY:$STAMP manifest sha256 $MANIFEST_BEFORE; / has $ROOT_BEFORE bytes free; restart policy: $RESTART_POLICY"

    place_hold "forgejo-data-move-$HASH12" || refuse "could not hold the converge (above)"
    STAGE=mutating
    echo "$ME: stopping $SERVICE — the forge's git, CI and registry are down from here until the start"
    # STOPPED first: a stop that fails part-way may still have stopped
    # it, and the restore must then start it again (finding 6).
    STOPPED=1
    docker compose -f "$FORGE_COMPOSE" stop -t 60 "$SERVICE" > "$WORK/stop" 2>&1 \
        || { cat "$WORK/stop" >&2; fail "docker compose stop $SERVICE failed (above)"; }
    local rc alloc avail muuid mroot
    FACTS_STOPPED=$(container_facts "$CID") || fail "docker could not read $SERVICE's state after the stop ($(head -c 300 "$WORK/docker.err"))"
    [ "${FACTS_STOPPED%% *}" = false ] || fail "$SERVICE still reads as running after the stop ($FACTS_STOPPED)"
    echo "$ME: $SERVICE stopped (running, StartedAt, RestartCount: $FACTS_STOPPED; restart policy $RESTART_POLICY)"
    MAIN_BEFORE=$(host_main_sha "$FORGE_REPO") || fail "host git could not read refs/heads/main of $FORGE_REPO with $SERVICE stopped ($(head -c 300 "$WORK/git.err"))"
    echo "$ME: main, read off the disk with $SERVICE stopped: $MAIN_BEFORE"
    others_mounting > "$WORK/o" || fail "docker could not list the running containers' mounts ($(head -c 300 "$WORK/docker.err"))"
    [ ! -s "$WORK/o" ] || fail "another running container mounts $SRC, under it or above it: $(tr '\n' ' ' < "$WORK/o")"
    holders_under "$SRC" > "$WORK/holders" \
        || fail "the process table $PROC could not be read with $SERVICE stopped, so a host process holding files under $SRC could not be named"
    [ ! -s "$WORK/holders" ] \
        || fail "host processes hold files under $SRC with $SERVICE stopped (pid, command, path): $(tr '\n' ';' < "$WORK/holders") — a copy taken now could miss their writes"

    census "$SRC" > "$WORK/census-src" || fail "could not measure $SRC ($(head -c 300 "$WORK/find.err"))"
    alloc=$(du -sxB1 -- "$SRC" 2> "$WORK/du.err" | awk 'NR == 1 { print $1 }') \
        || fail "du could not size the stopped $SRC ($(head -c 300 "$WORK/du.err"))"
    [[ "$alloc" =~ ^[0-9]+$ ]] || fail "du gave no size for $SRC ('$alloc')"
    [ "$alloc" -le $((SRC_CEIL_GIB * GIB)) ] \
        || fail "$SRC uses $alloc bytes, over the approved ceiling of $SRC_CEIL_GIB GiB"
    avail=$(df -B1 --output=avail -- "$MNT" 2> "$WORK/df.err" | awk 'NR == 2 { print $1 }') \
        || fail "df could not read $MNT ($(head -c 300 "$WORK/df.err"))"
    [[ "$avail" =~ ^[0-9]+$ ]] || fail "df gave no free-space figure for $MNT ('$avail')"
    [ "$avail" -ge $((alloc + MARGIN_GIB * GIB)) ] \
        || fail "$MNT has $avail bytes free; $SRC needs $alloc plus the $MARGIN_GIB GiB margin"
    echo "$ME: source, stopped: $alloc bytes; per top-level entry (name, entries, bytes):"
    sed 's/^/  /' "$WORK/census-src"

    if [ ! -d "$TGT" ]; then
        mkdir -- "$TGT" && chown --reference="$SRC" -- "$TGT" && chmod --reference="$SRC" -- "$TGT" \
            || fail "could not create $TGT"
    fi
    echo "$ME: copying: ${COPY[*]}"
    "${COPY[@]}" > "$WORK/rsync" 2>&1
    rc=$?
    [ "$rc" = 0 ] || { cat "$WORK/rsync" >&2; fail "the copy failed (rsync exit $rc, above)"; }
    echo "$ME: verifying: ${VERIFY[*]}"
    "${VERIFY[@]}" > "$WORK/verify" 2>&1
    rc=$?
    [ "$rc" = 0 ] || { cat "$WORK/verify" >&2; fail "the verify pass could not run (rsync exit $rc, above)"; }
    if [ -s "$WORK/verify" ]; then
        head -n 200 "$WORK/verify" >&2
        fail "the verify pass lists $(wc -l < "$WORK/verify") difference(s) between $SRC and $TGT (the first 200 above) — the copy is not identical"
    fi
    census "$TGT" > "$WORK/census-tgt" || fail "could not measure $TGT ($(head -c 300 "$WORK/find.err"))"
    if ! cmp -s "$WORK/census-src" "$WORK/census-tgt"; then
        diff "$WORK/census-src" "$WORK/census-tgt" >&2
        fail "the target's counts differ from the source's (above: < source, > target)"
    fi
    echo "$ME: verified — the checksum pass lists nothing, and $TGT's counts equal the source's entry by entry"

    # The new fstab, built and verified BEFORE the commit point, so a
    # line findmnt refuses leaves the old layout whole.
    local size last
    NEWTAB=$(mktemp "$FSTAB.move-forgejo-data.XXXXXX") || fail "could not create a temporary fstab beside $FSTAB"
    cp -p -- "$FSTAB" "$NEWTAB" || fail "could not copy $FSTAB"
    last=$(tail -c 1 -- "$FSTAB") || fail "could not read $FSTAB's last byte"
    if [ -n "$last" ]; then printf '\n' >> "$NEWTAB" || fail "could not write $NEWTAB"; fi
    printf '%s\n' "$FSTAB_LINE" >> "$NEWTAB" || fail "could not write $NEWTAB"
    size=$(stat -c %s -- "$FSTAB") || fail "could not size $FSTAB"
    cmp -s <(head -c "$size" -- "$NEWTAB") "$FSTAB" \
        && [ "$(tail -n 1 -- "$NEWTAB")" = "$FSTAB_LINE" ] \
        && [ "$(grep -cxF -- "$FSTAB_LINE" "$NEWTAB")" = 1 ] \
        || fail "the new fstab is not the old one plus exactly the one line; $FSTAB is unchanged"
    fstab_verifies "$NEWTAB" \
        || { sed 's/^/    /' "$WORK/verify-out" >&2; fail "the new fstab does not verify (above); $FSTAB is unchanged"; }

    # ---- THE COMMIT POINT ----------------------------------------------
    [ ! -e "$PRE" ] && [ ! -L "$PRE" ] || fail "$PRE appeared since the plan; nothing was renamed"
    still_stopped 'before the rename'
    # The marker goes down BEFORE the rename, not after the mount
    # (re-review of the released car, finding 3): from the rename on, a
    # restart:always Forgejo that dockerd starts on its own — a reboot is
    # enough — may write to the target, and a failure in that window must
    # not leave its writes unmarked. It records what the rollback must
    # prove: this plan, the stamp and the manifest sha the before half
    # read off the copy that becomes $PRE (finding 1). A failure before
    # the rename removes it again (the EXIT trap): nothing was served.
    [ ! -e "$MARKER" ] && [ ! -L "$MARKER" ] || fail "$MARKER appeared since the plan; nothing was renamed"
    MARKER_WRITTEN=1
    marker_record "$HASH" "$STAMP" "$MANIFEST_BEFORE" > "$MARKER" || fail "could not write $MARKER; nothing was renamed"
    echo "$ME: marked $TGT as served from here ($MARKER: plan, stamp $STAMP, manifest $MANIFEST_BEFORE)"
    # From here until STAGE=committed a signal may land on either side
    # of the rename; the EXIT trap reads the tree to say which (finding 1).
    RENAMING=1
    mv -T -- "$SRC" "$PRE" || fail "could not rename $SRC to $PRE; nothing was renamed"
    STAGE=committed
    echo "$ME: renamed $SRC to $PRE"
    mkdir -- "$SRC" && chown --reference="$PRE" -- "$SRC" && chmod --reference="$PRE" -- "$SRC" \
        || fail "could not create the mountpoint $SRC"
    chattr +i -- "$SRC" || fail "could not make the empty mountpoint $SRC immutable"
    mv -f -- "$NEWTAB" "$FSTAB" || fail "could not move the verified fstab into place; $FSTAB is unchanged"
    NEWTAB=""
    [ "$(grep -cxF -- "$FSTAB_LINE" "$FSTAB")" = 1 ] || fail "$FSTAB does not read back with the line exactly once"
    echo "$ME: $FSTAB now carries: $FSTAB_LINE"
    systemctl daemon-reload || fail "systemctl daemon-reload failed"
    mount --fstab "$FSTAB" "$SRC" || fail "mount $SRC failed"
    read -r muuid mroot < <(findmnt -nro UUID,FSROOT --mountpoint "$SRC" 2> /dev/null)
    [ "${muuid:-}" = "$TUUID" ] && [ "${mroot:-}" = "/$TGT_NAME" ] \
        || fail "$SRC is mounted as UUID '${muuid:-none}' root '${mroot:-none}', not UUID $TUUID at /$TGT_NAME"
    echo "$ME: $SRC is the bind of $TGT (UUID $TUUID, root /$TGT_NAME)"

    still_stopped 'before the start'
    docker compose -f "$FORGE_COMPOSE" start "$SERVICE" > "$WORK/start" 2>&1 \
        || { cat "$WORK/start" >&2; fail "docker compose start $SERVICE failed (above)"; }
    STOPPED=0
    real_start
    wait_healthy || proof_fail "$HEALTHZ did not pass within ${HEALTH_WAIT}s of the start"
    local main_after manifest_after
    main_after=$(main_sha) || proof_fail "git inside $SERVICE could not read refs/heads/main after the start ($(head -c 300 "$WORK/git.err"))"
    [ "$main_after" = "$MAIN_BEFORE" ] || proof_fail "refs/heads/main reads $main_after after the start, $MAIN_BEFORE off the disk after the stop"
    serves_from "$TGT" || proof_fail "Forgejo's /data is not the moved copy $TGT (above) — it may be serving $PRE or the empty mountpoint"
    echo "$ME: the container's /data is the moved copy $TGT (same device and inode)"
    manifest_after=$(manifest_sha "$STAMP") || proof_fail "the registry proof failed after the start: $(cat "$WORK/manifest.why")"
    [ "$manifest_after" = "$MANIFEST_BEFORE" ] \
        || proof_fail "$REGISTRY:$STAMP's manifest hashes to $manifest_after after the start, $MANIFEST_BEFORE before"
    echo "$ME: proof: healthz passes; main $main_after before and after; $REGISTRY:$STAMP manifest $manifest_after before and after"
    echo "$ME: / free: $ROOT_BEFORE bytes before, $(root_avail) after — the old copy $PRE still holds its space until delete-forgejo-premove-copy (plan-a-forgejo-premove-delete) deletes it, no sooner than $SOAK_HOURS h from now"
    release_own_hold
    STAGE=done
    echo "$ME: OK — Forgejo serves from $TGT through $SRC"
}

rollback_write() {
    local main_before="" main_after manifest_after last_n new_n
    if [ "$RUNNING" = true ]; then
        main_before=$(main_sha) || main_before="unreadable"
        echo "$ME: before: $SERVICE running, main $main_before (the moved copy)"
    fi
    place_hold "forgejo-data-rollback-$HASH12" || refuse "could not hold the converge (above)"
    # A rollback repairs a partial state; restarting Forgejo half-way
    # through one is a guess, so any failure from here stops and says
    # what stands.
    STAGE=committed
    if [ "$RUNNING" = true ]; then
        STOPPED=1
        docker compose -f "$FORGE_COMPOSE" stop -t 60 "$SERVICE" > "$WORK/stop" 2>&1 \
            || { cat "$WORK/stop" >&2; fail "docker compose stop $SERVICE failed (above)"; }
        echo "$ME: stopped $SERVICE"
    fi
    FACTS_STOPPED=$(container_facts "$CID") || fail "docker could not read $SERVICE's state ($(head -c 300 "$WORK/docker.err"))"
    [ "${FACTS_STOPPED%% *}" = false ] || fail "$SERVICE reads as running ($FACTS_STOPPED) — nothing under it is touched while it runs"
    if [ "$MOUNTED" = true ]; then
        # An umount answering EBUSY is retried, and each refusal names
        # who holds the mount (finding 6): here $SRC IS its own mount,
        # so fuser -m names exactly its holders.
        local try
        for try in 1 2 3; do
            umount -- "$SRC" 2> "$WORK/umount.err" && break
            say "umount $SRC failed (try $try of 3): $(head -c 300 "$WORK/umount.err")"
            if command -v fuser > /dev/null 2>&1; then
                fuser -vm "$SRC" > "$WORK/fuser" 2>&1
                sed 's/^/    /' "$WORK/fuser" >&2
            else
                say "  fuser is not installed on this host, so who holds $SRC cannot be named"
            fi
            [ "$try" -lt 3 ] || fail "umount $SRC failed three times — who holds it is above"
            sleep "$RETRY_SLEEP"
        done
        findmnt -rno TARGET > "$WORK/mounts" 2> "$WORK/findmnt.err" || fail "findmnt could not re-read the mount table"
        is_mounted "$SRC" && fail "$SRC is still a mount after the umount"
        echo "$ME: unmounted $SRC"
    fi
    if [ -n "$FSTAB_LINE" ]; then
        NEWTAB=$(mktemp "$FSTAB.move-forgejo-data.XXXXXX") || fail "could not create a temporary fstab beside $FSTAB"
        cp -p -- "$FSTAB" "$NEWTAB" || fail "could not copy $FSTAB"
        awk -v l="$FSTAB_LINE" '$0 == l && !done { done = 1; next } { print }' "$FSTAB" > "$NEWTAB" \
            || fail "could not write $NEWTAB"
        last_n=$(awk 'END { print NR }' "$FSTAB") && new_n=$(awk 'END { print NR }' "$NEWTAB") \
            || fail "could not count the lines of $FSTAB"
        [ "$new_n" = $((last_n - 1)) ] && [ "$(grep -cxF -- "$FSTAB_LINE" "$NEWTAB")" = 0 ] \
            || fail "the new fstab is not the old one less exactly the one line; $FSTAB is unchanged"
        fstab_verifies "$NEWTAB" \
            || { sed 's/^/    /' "$WORK/verify-out" >&2; fail "the new fstab does not verify (above); $FSTAB is unchanged"; }
        mv -f -- "$NEWTAB" "$FSTAB" || fail "could not move the verified fstab into place; $FSTAB is unchanged"
        NEWTAB=""
        [ "$(grep -cxF -- "$FSTAB_LINE" "$FSTAB")" = 0 ] || fail "$FSTAB still carries the bind line after the edit"
        systemctl daemon-reload || fail "systemctl daemon-reload failed"
        echo "$ME: removed from $FSTAB: $FSTAB_LINE"
    fi
    if [ -e "$SRC" ]; then
        chattr -i -- "$SRC" || fail "chattr -i $SRC failed"
        rmdir -- "$SRC" || fail "rmdir $SRC failed — it is not the empty mountpoint"
        echo "$ME: removed the empty mountpoint $SRC"
    fi
    mv -T -- "$PRE" "$SRC" || fail "could not rename $PRE back to $SRC"
    echo "$ME: renamed $PRE back to $SRC"
    still_stopped 'before the start'
    docker compose -f "$FORGE_COMPOSE" start "$SERVICE" > "$WORK/start" 2>&1 \
        || { cat "$WORK/start" >&2; fail "docker compose start $SERVICE failed (above)"; }
    STOPPED=0
    real_start
    wait_healthy || proof_fail "$HEALTHZ did not pass within ${HEALTH_WAIT}s of the start"
    main_after=$(main_sha) || proof_fail "git inside $SERVICE could not read refs/heads/main after the start ($(head -c 300 "$WORK/git.err"))"
    serves_from "$SRC" || proof_fail "Forgejo's /data is not $SRC, the copy this rollback put back (above)"
    echo "$ME: the container's /data is $SRC (same device and inode)"
    # The registry leg proves the stamp THE MOVE recorded, read off this
    # very copy before the move — never the current stamp, whose image
    # may exist only in $TGT (finding 1). With no record, it is reported,
    # as the signed plan says.
    local registry_said
    if [ -n "$PROVE_STAMP" ]; then
        manifest_after=$(manifest_sha "$PROVE_STAMP") \
            || proof_fail "$SERVICE came up on the pre-move copy, but $REGISTRY:$PROVE_STAMP — the manifest $RB_MARKER records the move proving off this copy — is not served: $(cat "$WORK/manifest.why")"
        [ "$manifest_after" = "$PROVE_MANIFEST" ] \
            || proof_fail "$REGISTRY:$PROVE_STAMP's manifest hashes to $manifest_after after the start; the move recorded $PROVE_MANIFEST off this copy ($RB_MARKER)"
        registry_said="$REGISTRY:$PROVE_STAMP manifest $manifest_after, as the move recorded it"
    else
        say "FINDING — the registry leg is not proved: $PROVE_WHY (the signed plan says so)"
        registry_said="registry not proved ($PROVE_WHY)"
    fi
    echo "$ME: proof: healthz passes; main $main_after (was ${main_before:-not read} on the moved copy); $registry_said"
    release_own_hold
    STAGE=done
    echo "$ME: OK — Forgejo serves from $SRC on the root filesystem again; $TGT is left in place"
}

# The restart's one act is the start. What it proves is that the start
# was real and that Forgejo answers healthz; the other legs are printed,
# and one that cannot be read or differs is a FINDING, never a stop —
# this verb exists to recover a Forgejo that a failed proof stopped, and
# stopping it again for a leg of its own would put the forge back where
# it found it (re-review of the released car, finding 1).
#
# A FINDING IS NOT A PASS (review of the released follow-up car, backlog
# ed7702c3, finding 2). It exited 0 "OK" with its main and served-copy
# legs FINDINGs — green with warnings, which reads exactly like green.
# Forgejo stays running either way; the run exits 1 when any leg it
# reports is a finding, so the record says started, not proven.
restart_write() {
    local now r s was main_now findings=0
    FACTS_STOPPED=$(container_facts "$CID") || refuse "docker could not read $SERVICE's state ($(head -c 300 "$WORK/docker.err"))"
    [ "${FACTS_STOPPED%% *}" = false ] || refuse "$SERVICE reads as running ($FACTS_STOPPED) — there is nothing to restart"
    STAGE=restarting
    echo "$ME: starting $SERVICE on $SRC ($SRC_STATE)"
    docker compose -f "$FORGE_COMPOSE" start "$SERVICE" > "$WORK/start" 2>&1 \
        || { cat "$WORK/start" >&2; fail "docker compose start $SERVICE failed (above)"; }
    now=$(container_facts "$CID") || fail "docker could not read $SERVICE's state after the start ($(head -c 300 "$WORK/docker.err"))"
    read -r r s _ <<< "$now"
    read -r _ was _ <<< "$FACTS_STOPPED"
    [ "$r" = true ] && [ "$s" != "$was" ] \
        || fail "the start was not a real start: $SERVICE reads '$now' after it and read '$FACTS_STOPPED' before"
    echo "$ME: $SERVICE started (StartedAt $s)"
    wait_healthy || fail "$HEALTHZ did not pass within ${HEALTH_WAIT}s of the start"
    if main_now=$(main_sha); then
        echo "$ME: refs/heads/main inside $SERVICE: $main_now"
    else
        say "FINDING — git inside $SERVICE could not read refs/heads/main of $REPO_IN_CONTAINER ($(head -c 300 "$WORK/git.err"))"
        findings=$((findings + 1))
    fi
    if ! serves_from "$SRC"; then
        say "FINDING — the served-copy leg does not tie $SERVICE's /data to $SRC (above)"
        findings=$((findings + 1))
    fi
    if [ -f "$HOLD_FILE" ]; then
        echo "$ME: the converge is still held ($(cat "$HOLD_FILE" 2> /dev/null)) — this verb neither places nor releases a hold"
    fi
    [ "$findings" = 0 ] \
        || fail "$SERVICE runs on $SRC ($SRC_STATE), started, not proven: $findings leg(s) of the proof are FINDINGs (above). It is left running — this verb never stops Forgejo"
    STAGE=done
    echo "$ME: OK — $SERVICE runs on $SRC ($SRC_STATE)"
}

# The delete's one act is the removal of the path the plan names. Its
# proof was read by the observation this run just made, as refusals;
# from the rename on, nothing is a refusal. The rename goes first and is
# atomic: from it on no rollback plan finds a pre-move copy, so a copy
# half deleted is never put back (backlog 192a9003). It never stops,
# starts or holds anything — the moved copy is not touched.
delete_write() {
    local after grown tries=0
    [[ "$ROOT_BEFORE" =~ ^[0-9]+$ ]] || refuse "df gave no free-space figure for / ('$ROOT_BEFORE'), so the space the delete frees could not be proved"
    echo "$ME: before: / has $ROOT_BEFORE bytes free; $VICTIM ($VICTIM_STATE) uses $VICTIM_BYTES bytes in $VICTIM_ENTRIES entries"
    STAGE=deleting
    if [ "${#RENAME[@]}" != 0 ]; then
        "${RENAME[@]}" || fail "could not rename $PRE to $DOOMED; nothing was deleted"
        echo "$ME: renamed $PRE to $DOOMED — no rollback plan can find a pre-move copy from here"
    fi
    "${REMOVE[@]}" > "$WORK/rm" 2>&1 || { head -n 50 "$WORK/rm" >&2; fail "the delete of $DOOMED failed (the first 50 lines of rm's answer above)"; }
    [ ! -e "$DOOMED" ] && [ ! -L "$DOOMED" ] || fail "$DOOMED still exists after the delete"
    echo "$ME: $DOOMED is gone"
    # ext4 hands freed blocks back at its journal commit, so the figure
    # is re-read while the filesystem settles, never judged on one read.
    while :; do
        after=$(root_avail)
        [[ "$after" =~ ^[0-9]+$ ]] || fail "df gave no free-space figure for / after the delete ('$after')"
        grown=$((after - ROOT_BEFORE))
        [ "$grown" -ge "$NEED" ] && break
        tries=$((tries + 1))
        [ "$tries" -lt 6 ] \
            || fail "/ did not grow by the $NEED bytes the delete must free ($VICTIM_BYTES measured, less the slack): it had $ROOT_BEFORE bytes free before and $after after, $grown more — a process may hold deleted files open, or something else wrote to / meanwhile"
        sleep "$RETRY_SLEEP"
    done
    echo "$ME: / has grown by $grown bytes ($ROOT_BEFORE free before, $after after; the copy measured $VICTIM_BYTES, and at least $NEED was required)"
    delete_legs || fail "the copy is deleted, but the proof read after it fails: $LEG_WHY"
    STAGE=done
    echo "$ME: OK — $VICTIM is deleted; Forgejo serves $TGT through $SRC, and that copy is now the only one"
}

case "$MODE" in
    move)     move_write ;;
    rollback) rollback_write ;;
    restart)  restart_write ;;
    delete)   delete_write ;;
esac
exit 0
