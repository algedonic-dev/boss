#!/bin/bash
# recovery-kit-read — the ONE door a workstation has to a recovery kit the
# forge has cut (backlog c1bb822e). Installed by
# infra/forge/install-recovery-kit-reader.sh as
# /usr/local/libexec/boss/recovery-kit-read, root:root 0755, and granted
# to one account by /etc/sudoers.d/boss-recovery-kit:
#
#   recovery-kit-read --manifest <version>           the MANIFEST (no secret value)
#   recovery-kit-read --readme   <version>           the README (no secret value)
#   recovery-kit-read --stream   <version>           the kit's tar on stdout
#   recovery-kit-read --written  <version> <sha256>  the writing machine read the
#                                                    STICK back and got <sha256>
#
# WHY A SEPARATE, SELF-CONTAINED FILE (adversarial review of car 3d12b774,
# finding 1, HIGH). The first cut had the workstation run
# `sudo -n <checkout>/infra/forge/recovery-kit.sh`, and the checkout
# belongs to the very account that sudo was granted to — so the rule that
# made the stream work was NOPASSWD root for anyone who could edit a file
# in that tree, and the script it ran sourced two more from the same tree.
# This reader SOURCES NOTHING, execs nothing from a checkout, and is
# installed root-owned outside it; the converge installs it from forge
# main on every tick, so what runs as root is what the trains reviewed.
# It cannot cut, re-cut or read a credential that is not already in the
# kit: it serves the kit the ops verb cut-recovery-kit (recovery-kit.sh
# --assemble, run by the ops runner as root) left in the tmpfs, and
# nothing else. Every argument is refused unless it is one of four modes,
# a 12-hex version, or a 64-hex hash.
#
# WHAT IT LEAVES BEHIND. Every --stream and --written is appended to
# BOSS_RECOVERY_KIT_LOG (/var/log/boss/recovery-kit.log, root 0600:
# when, which kit, which sudo user) — which outlives the kit — and, best
# effort, onto the ops-request packet that cut the kit
# (recovery_kit_streamed, a list; recovery_kit_written, the last write).
# A best-effort record never refuses a stream: the packet is visibility,
# the local log is the record that cannot be dark (CLAUDE.md §Diagnosis,
# "an arm that needs the patient"). A --written whose hash matches the
# kit DISCARDS it: the stick holds it now, and a kit left in RAM is a
# copy of every credential for no one's benefit (review finding 5). Cut
# again for a second stick. What --written records is an ATTESTATION by
# the sudo user, not a proof (re-review N2): the hash it checks is one
# --manifest already hands that same user, so a DR-readiness reader
# weighs recovery_kit_written as that person's word, and its `proof`
# field says so.
#
# The machine token: the jobs API's machine gate is `off` today; once it
# enforces, these PATCHes need the token the other forge writers pass
# through infra/lib/secret-header.sh, which this reader cannot source.
# Until then a refused record is exit 5 on --written and a journal line on
# --stream — loud, never silent (review finding 9).
#
# ENV is honoured for the tests only in practice: sudo's env_reset (set
# explicitly for this command in the sudoers file) strips all of it.
#   BOSS_RECOVERY_KIT_DIR, BOSS_RECOVERY_KIT_UID, BOSS_RECOVERY_KIT_MAX_AGE_H,
#   BOSS_RECOVERY_KIT_LOG, BOSS_SOR_ENV, BOSS_RECOVERY_KIT_PATH
#
# PATH IS THIS FILE'S OWN (re-review of 416f3899, N1): `#!/bin/bash`
# rather than env, and a fixed PATH before any tool is named, so what
# findmnt, sed, curl or jq means does not depend on the caller's
# environment or on the host's sudo defaults. The sudoers fragment sets
# secure_path for this command too; the line below holds even where it
# is absent. BOSS_RECOVERY_KIT_PATH is the tests' seam, and env_reset
# strips it at sudo.
PATH="${BOSS_RECOVERY_KIT_PATH:-/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin}"
export PATH
set -uo pipefail

ME="recovery-kit-read"
DIR="${BOSS_RECOVERY_KIT_DIR:-/run/boss-recovery-kit}"
WANT_UID="${BOSS_RECOVERY_KIT_UID:-0}"
MAX_AGE_H="${BOSS_RECOVERY_KIT_MAX_AGE_H:-24}"
LOG="${BOSS_RECOVERY_KIT_LOG:-/var/log/boss/recovery-kit.log}"
SOR_ENV="${BOSS_SOR_ENV:-/etc/boss/sor.env}"
UUID_RE='^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
umask 077

say() { echo "$ME: $*" >&2; }
die() { say "REFUSED — $2"; exit "$1"; }

[ "$(id -u)" = "$WANT_UID" ] || die 1 "must run as uid $WANT_UID: sudo -n /usr/local/libexec/boss/recovery-kit-read $*"

state() { sed -n "s/^$1=//p" "$DIR/state" 2>/dev/null | sed -n 1p; }
is_tmpfs() { [ "$(findmnt -n -o FSTYPE --mountpoint "$DIR" 2>/dev/null)" = "tmpfs" ]; }
caller() { echo "${SUDO_USER:-$(id -un)}"; }

log_line() { # <what>
    mkdir -p "$(dirname "$LOG")" 2>/dev/null
    printf '%s %s by %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$1" "$(caller)" >> "$LOG" \
        || say "could not append to $LOG"
}

# The one key this reader reads from the host's address file — parsed,
# never sourced (sourcing would run whatever the file holds, as root).
jobs_url() { sed -n 's/^BOSS_JOBS_URL=//p' "$SOR_ENV" 2>/dev/null | sed -n 1p; }

BOSS_USER_HDR='{"id":"automation:recovery-kit","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}'

# patch_request <json body> — PATCH the cutting request's metadata; prints
# the HTTP code (or `none`). Never touches stdout's stream: callers
# capture it.
patch_request() {
    local req url
    req="$(state request)"
    url="$(jobs_url)"
    [[ "$req" =~ $UUID_RE ]] || { echo "no-request"; return; }
    [ -n "$url" ] || { echo "no-url"; return; }
    curl -sS -m 10 -o /dev/null -w '%{http_code}' -X PATCH \
        -H "content-type: application/json" -H "x-boss-user: $BOSS_USER_HDR" \
        --data-binary "$1" "$url/api/jobs/$req/metadata" 2>/dev/null || echo "none"
}

kit_here() { # <version>
    local want="${1:-}" have taken now
    [[ "$want" =~ ^[0-9a-f]{12}$ ]] || die 2 "a version is 12 hex characters, got '$want'"
    is_tmpfs || die 4 "no kit is cut: $DIR is not a mounted tmpfs. Cut one with the cut-recovery-kit ops verb"
    have="$(state version)"
    [ -n "$have" ] || die 4 "$DIR holds no kit. Cut one with the cut-recovery-kit ops verb"
    [ "$have" = "$want" ] || die 4 "the kit here is $have, not $want — run the writer with the version the newest cut-recovery-kit answer printed"
    taken="$(state taken_epoch)"
    [[ "$taken" =~ ^[0-9]+$ ]] || die 4 "the kit's state names no time it was cut"
    now="$(date -u +%s)"
    [ $(( (now - taken) / 3600 )) -lt "$MAX_AGE_H" ] \
        || die 4 "kit $have is older than ${MAX_AGE_H}h: a credential may have rotated since. Cut a new one"
}

manifest_tar_sha() { sed -n 's/^tar_sha256 //p' "$DIR/MANIFEST.txt" 2>/dev/null | sed -n 1p; }

take_lock() {
    exec 9>"$DIR.lock" || die 1 "cannot open the lock $DIR.lock"
    flock -n 9 || die 1 "another recovery-kit run holds $DIR.lock"
}

# The same two steps recovery-kit.sh --discard takes; written out here
# because this reader sources nothing.
discard() {
    find "$DIR" -mindepth 1 -maxdepth 1 -exec rm -rf -- {} + 2>/dev/null
    if [ -n "$(findmnt -n -o FSTYPE --mountpoint "$DIR" 2>/dev/null)" ]; then
        umount "$DIR" || say "could not unmount $DIR — its contents were removed; a reboot clears it"
    fi
}

mode="${1:-}"
shift
case "$mode" in
    --manifest) kit_here "${1:-}"; cat "$DIR/MANIFEST.txt" ;;
    --readme) kit_here "${1:-}"; cat "$DIR/README.txt" ;;
    --stream)
        take_lock
        kit_here "${1:-}"
        [ "$(sha256sum "$DIR/kit.tar" 2>/dev/null | cut -d' ' -f1)" = "$(manifest_tar_sha)" ] \
            || die 1 "kit.tar no longer hashes to the MANIFEST's tar_sha256 — nothing streamed. Cut a new kit"
        log_line "streamed kit $1"
        # Best effort, BEFORE the stream and off stdout: a list read and
        # re-written, so every pull is on the packet, not only the last.
        at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
        req="$(state request)"; url="$(jobs_url)"
        if [[ "$req" =~ $UUID_RE ]] && [ -n "$url" ]; then
            prior="$(curl -sS -m 10 -H "x-boss-user: $BOSS_USER_HDR" "$url/api/jobs/$req" 2>/dev/null \
                | jq -c '.metadata.recovery_kit_streamed // [] | if type == "array" then . else [] end' 2>/dev/null)"
            [ -n "$prior" ] || prior='[]'
            body="$(jq -n -c --argjson p "$prior" --arg v "$1" --arg at "$at" --arg who "$(caller)" \
                '{recovery_kit_streamed: ($p + [{version: $v, at: $at, by: $who}])}')"
            code="$(patch_request "$body")"
            case "$code" in 2??) ;; *) say "the stream is recorded in $LOG, NOT on ${req:0:8} (HTTP $code)" ;; esac
        fi
        cat "$DIR/kit.tar"
        ;;
    --written)
        take_lock
        kit_here "${1:-}"
        version="$1"; read_back="${2:-}"
        [[ "$read_back" =~ ^[0-9a-f]{64}$ ]] || die 2 "the read-back hash must be 64 hex characters"
        want="$(manifest_tar_sha)"
        [ "$read_back" = "$want" ] \
            || die 1 "the writing machine read back $read_back and kit $version is $want — the stick does NOT hold this kit"
        complete="$(state complete)"; missing="$(state missing)"
        log_line "wrote kit $version (read back $read_back from the stick; complete=$complete missing=${missing:-none})"
        at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
        body="$(jq -n -c --arg v "$version" --arg s "$read_back" --arg at "$at" --arg who "$(caller)" \
            --arg c "$complete" --arg m "$missing" \
            '{recovery_kit_written: {version: $v, tar_sha256: $s, written_at: $at, over_ssh_as: $who,
              complete: ($c == "yes"), missing: ($m | split(",") | map(select(. != ""))),
              proof: ("attested by " + $who + ": the writing machine reports it re-mounted the stick, decrypted it with the passphrase typed again, and read back this sha256, which equals the manifest tar_sha256")}}')"
        code="$(patch_request "$body")"
        discard
        echo "$ME: kit $version is on the stick (complete=$complete${missing:+, missing $missing}); discarded from the forge's RAM — cut again for another stick"
        case "$code" in
            2??) echo "$ME: recorded on the ops-request as recovery_kit_written" ;;
            no-request) echo "$ME: the kit was cut by hand, so no packet records the write — $LOG does" ;;
            *) say "the write is verified and logged in $LOG, and NOT recorded on the packet (${code})"; exit 5 ;;
        esac
        ;;
    *)
        die 2 "usage: $ME --manifest <version> | --readme <version> | --stream <version> | --written <version> <sha256>"
        ;;
esac
