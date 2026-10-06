#!/usr/bin/env bash
#
# reclaim-gcp-root — free boss-gcp's 48 GB root by removing exactly four
# kinds of thing, and nothing else, ever: the obsolete binary backup
# directories /opt/boss-binbak-* and /opt/boss-dev-bak, the retired second
# stack's database capture /var/backups/boss/second-stack/
# second-stack-<stamp>.sql, the dumps the retired boss-gcp off-site leg
# deposited, /var/backups/boss-cluster-pg/boss-<stamp>.sql.gz, and the
# systemd journal beyond a fixed 1G — through a rendered plan a passkey
# signs.
#
#   reclaim-gcp-root.sh --dry-run          render the plan; removes nothing
#   reclaim-gcp-root.sh <plan-sha256>      remove what the SIGNED plan names
#
# WHY IT EXISTS (backlog d3c7eada car 2, 2026-09-26)
# ---------------------------------------------------------------------
# boss-gcp's root sat at 11-13 GB free for nine days against the
# estate's 17 GB floor for a 47 GB disk, and estate.alarm filed it as
# `disk_tight:boss-gcp`. Car 1 let disk-report measure the host; its
# reading (ops-request b21ddeb3, 2026-09-26) found four hand-made
# pre-deploy binary backups from July 2026 under /opt —
# boss-binbak-pre-pr73-0702-1801 (600M), boss-binbak-pre-latest-194355
# (586M), boss-binbak-pre-pr5-20260701-032322 (558M) and boss-dev-bak
# (536M), ~2.3 GB that no commit in the tree names — and a 4.1 GB
# journal. Everything else the reading named is DATA and David's call
# (retention policy), so it is out of this verb's reach by construction:
# /var/backups/* — with TWO named exceptions, below — every home
# directory, /usr/local, and the live /opt/boss and /opt/boss-cli.
#
# THE ONE EXCEPTION: THE SECOND-STACK CAPTURE (backlog f44ca628,
# 2026-09-28). David made the retention call on this one file: "Delete
# the second-stack capture, build it as a verb." retire-second-stack took
# it on 2026-09-15 (ops-request 7912c9ae) as
# second-stack-20260915T211123Z.sql, 2.3 GB; the /opt backups and the
# journal free ~4.6 GB, which leaves boss-gcp ~16.4 GB free against its
# 17 GB floor, so disk_tight:boss-gcp never cleared without it. Only
# files retire-second-stack's own naming writes, directly in its own
# directory, are in reach (bound 7).
#
# THE SECOND EXCEPTION: THE CLUSTER-PG DUMPS (backlog 4bf7bdd1,
# 2026-10-01). David: "boss-gcp doesn't need to be storage backup." The
# GCS bucket is the off-site copy (boss-backup.yaml's offsite-gcs leg,
# David 2026-08-29), and the leg that shipped each nightly dump here over
# a deposit-only key is deleted from that manifest. What it left — 9.5 GB
# of /var/backups/boss-cluster-pg on a root at 16 GB free against its
# 17 GB floor (estate alarm a9476cbf) — is a second copy of what the
# bucket holds. Only boss-*.sql.gz directly in that one directory is in
# reach (bound 8); every other name under /var/backups stays out of it.
#
# THE LASTING GAIN IS THE ~2.3 GB OF BACKUPS. The journal has no
# SystemMaxUse drop-in on this host, so journald grows it back toward
# its default ceiling (~4 GB) after the vacuum; the vacuum buys time,
# not space. A SystemMaxUse bound is the converge's business, not this
# verb's (adversarial review of car 2, S7).
#
# THE APPROVAL (design 17835005's shape; reap-terminated-pods is the
# worked example). `plan-a-gcp-root-reclaim` runs `--dry-run`, which
# prints the plan on stdout and `plan-sha256:` on stderr; the ops
# runner writes it onto the request's approve step, David's passkey
# signs those bytes, and only then does the runner hand this script the
# signed hash. The write re-renders the plan and removes nothing unless
# today's plan hashes to it — so a backup that changed, appeared or went
# since the signature voids the approval, and a second run of an
# applied plan (its backups gone) is a refusal: at most once.
#
# THE BOUNDS, in the order they are applied — each refuses loudly and
# names the path or the bound; a refusal removes NOTHING, and a bound
# that cannot be EVALUATED (a read that fails) is a refusal, never a
# pass:
#
#   1. THE ARGUMENT. Exactly one: `--dry-run`, or a 64-hex plan hash.
#      Anything else — a path included — is refused before anything is
#      looked at.
#   2. THE NAMES. Candidates are what two globs match under /opt —
#      `boss-binbak-*` and the one name `boss-dev-bak` — and each must
#      be a REAL directory whose realpath is /opt/<its own name>. A
#      symlink is refused (this verb never removes what a link points
#      at), and so is any name outside ^boss-binbak-[A-Za-z0-9._-]+$.
#      The paths are the script's, never a param: no packet can name one.
#   3. NOT A CHECKOUT. A candidate holding a `.git` anywhere is KEPT —
#      skipped, with its reason in the plan — and the rest of the plan
#      stands: boss-dev-bak may be a dev checkout with unpushed commits,
#      and that is work, not a backup. Until 2026-09-27 it refused the
#      WHOLE run, so the first plan on boss-gcp (ops-request 8d334ca2)
#      freed nothing, not even the three boss-binbak-* beside it
#      (backlog d3c7eada). Each checkout's unpushed state rides in the
#      plan's bytes as evidence, so the passkey signs what it read:
#      local branches with commits on no remote (the first 20 named, all
#      counted), commits at HEAD on no branch and no remote, staged
#      changes, tracked files whose bytes differ from the index,
#      untracked files and stashes, and (adversarial review 2435f065,
#      2026-09-30) commits on ANY ref — tags, notes, other refs, a linked
#      worktree's HEAD — and the entries under .git/worktrees (a linked
#      worktree elsewhere keeps its HEAD and index there) and .git/modules
#      (a deinit'd submodule's repository) — measured against the
#      remote-tracking refs of the remotes its config NAMES as of its last
#      fetch (this verb never fetches; a refs/remotes/<x> no remote.<x>.url
#      names counts as nothing; and, review d83b2898 B2, a named remote
#      counts only when every URL it has is a NETWORK URL — a local path,
#      file:// or anything unparsable counts as nothing and keeps the
#      checkout, because the copy it "holds" may be inside what this plan
#      removes; every remote's URL rides in the plan, userinfo stripped),
#      read with plumbing that runs nothing the checkout's
#      config names, as its owner (see checkout_git). A state that
#      cannot be read says so in the plan, and the checkout is kept.
#      THE ONE CHECKOUT THAT MAY GO (David, 2026-09-30 ~20:16Z, backlog
#      d3c7eada `decided_2026_09_30`: "/opt/boss-dev-bak is disposable;
#      drop it"): /opt/boss-dev-bak — and no other name — is listed for
#      REMOVAL when this plan PROVES it holds nothing unpushed: every
#      checkout in it (nested ones included) read, and every count above
#      0. Plan f57448bc had read it so and kept it anyway, leaving the
#      root ~1 GB under its floor. The proof rides in the plan's bytes
#      under its removal line, so the passkey signs it; the candidate goes
#      FIRST in the write, and immediately before its rm the write reads
#      every checkout in it again and requires the same bytes the signed
#      plan held, and re-reads its ages — any change refuses the WHOLE run
#      with exit 78, and because it goes first, nothing has been removed.
#      Bounds 4 and 5 apply to it exactly as to a backup. Any other
#      candidate holding a .git is kept, and the write checks each one
#      for a .git once more just before its rm.
#   4. THE AGE, BY CTIME. Nothing inside a candidate may have changed
#      (ctime) in the last 30 days. Not mtime: `cp -a` preserves mtimes,
#      so a rollback backup made TODAY from an old tree reads as July by
#      mtime (measured in review). ctime cannot be copied.
#   5. NOT LIVE. A candidate is refused when it is, contains or sits
#      inside what /opt/boss or /opt/boss-cli resolves to; when any mount
#      in /proc/self/mountinfo is at or under it (a same-filesystem bind
#      mount would defeat --one-file-system); when a symlink in /opt,
#      /usr/local/bin, /usr/bin, /usr/sbin or /etc/systemd/system
#      resolves into it; when any running process's executable lives in
#      it (/proc/*/exe); when any loaded service unit's
#      ExecStart/ExecStartPre/WorkingDirectory/FragmentPath resolves into
#      it; or when any unit FILE on disk (/etc/systemd/system,
#      /lib/systemd/system, /usr/lib/systemd/system, drop-ins included)
#      names it — the retired second stack's units are stopped and
#      disabled, not loaded, and kept for a restore.
#   6. THE JOURNAL. `journalctl --vacuum-size=1G` — a fixed bound, not a
#      param, so no request can ask for 0 and throw away the host's
#      diagnosis. The newest 1G stays.
#   7. THE CAPTURE. Candidates are what ONE glob matches in ONE fixed
#      directory — /var/backups/boss/second-stack/second-stack-*.sql,
#      where retire-second-stack writes (its BACKUP_DIR default) — and
#      nothing else under /var/backups is listed at all. The directory
#      must be a real directory whose realpath is its own spelling (no
#      link anywhere on the way), and it and its parent must be owned by
#      root and writable by no group and no other account (their owner
#      and mode ride in the plan). Each match must be named
#      ^second-stack-[0-9]{8}T[0-9]{6}Z\.sql$ — retire-second-stack's
#      `date -u +%Y%m%dT%H%M%SZ` stamp; any other name the glob matches
#      refuses the run — must be a REGULAR file (a symlink, a directory
#      or a device by that name is refused) with exactly ONE link (a
#      hardlinked dump under a capture's name is refused: removing the
#      name would free nothing and the record would claim it had),
#      whose realpath is <dir>/<its own name>, must not be a mount
#      point, and must not be held open by any process (/proc/*/fd,
#      compared by device and inode, so a process in another mount
#      namespace is seen too): a pg_dump still writing it, or a restore
#      reading it. The fd scan refuses on any fd it cannot follow for a
#      reason but "gone", and on a table without init's (pid 1) fds. The
#      adversarial review of 7bca1fee added the link count, the owner
#      and mode, and the stricter fd scan. There is NO age
#      bound: bound 4's 30 days is for the /opt backups, and the capture
#      David decided on was 13 days old. Its size in bytes and its
#      sha256 ride in the plan, so the passkey signs those exact bytes;
#      the write re-hashes each capture just before its `rm -f` and
#      removes nothing that no longer matches. The capture goes after
#      every backup directory, so a run that stops part-way stops with
#      the data still standing.
#   8. THE CLUSTER-PG DUMPS. Candidates are what ONE glob matches in ONE
#      fixed directory — /var/backups/boss-cluster-pg/boss-*.sql.gz, where
#      the retired leg's receiver deposited — and each must be named
#      ^boss-[A-Za-z0-9._:+-]+\.sql\.gz$ (any other name the glob matches
#      refuses the run: the receiver stamped its own names and its script
#      was never in the tree, so the bound is the shape it shares with the
#      cluster's boss-<stamp>.sql.gz, minus every character a line, a word
#      or a glob would split on), be a REGULAR file with exactly ONE link
#      whose realpath is <dir>/<its own name>, be no mount point, and be
#      held open by no process — bound 7's checks, by bound 7's helpers.
#      The directory must be a real directory whose realpath is its own
#      spelling and writable by no group and no other account; its PARENT
#      must be root's and closed, as bound 7's are. The directory itself
#      may be owned by the account the deposit key landed as, and its
#      owner rides in the plan: that account already holds every byte in
#      it, so what it could plant there under a dump's name is only more
#      of its own files for this verb to remove, while the closed parent
#      keeps the directory from being swapped for a link. No age bound:
#      the leg is retired, so every dump it left goes. THE PLAN BINDS EACH
#      DUMP'S IDENTITY, NOT ITS BYTES: size, inode, mtime and ctime (to the
#      nanosecond, as stat prints them in UTC). Hashing 9.5 GB three times
#      — the plan, the write's re-render, the last-moment re-read — on a
#      disk where a du of /home outlasted the runner's 30 s would not fit
#      the verb's timeout, and a ctime cannot be set from user space (bound
#      4's reason), so any write, rename onto the name, chmod or new file
#      under it moves the identity. The write reads the identity again just
#      before each `rm -f` and removes nothing that no longer matches. The
#      dumps go LAST, after the capture.
#
# THE PLAN'S BYTES ARE DETERMINISTIC: each candidate's path, its size
# in MiB and its top-level entries, sorted (a proven checkout first,
# with its proof), then each capture's path,
# size in bytes and sha256, then each dump's path and identity, then
# each kept checkout with its evidence, then the fixed journal line.
# No clock, no free space, no journal usage — those move on their own
# and ride stderr, where the hash does not reach.
#
# WHAT IT DOES NOT SEE. It reads units, unit files, mounts and
# processes, not every script on the host: a cron line or a shell script
# naming a backup directory by path would not stop it. A capture or a
# dump held open only through a memory map (not an fd) is not seen either.
#
# EXIT
#   0  done (or, with --dry-run, every bound passed and this is the plan)
#   2  refused — the reason names the path or the bound; nothing removed
#   1  failed part-way — the record states what was already removed and
#      what was not touched
#  78  refused at the last moment — the proven checkout changed between
#      the signed render and its rm, or it could not be read again then
#      (its search for checkouts or its ctime read failed); what was
#      read is printed, and nothing was removed (review 2435f065, R6)
#
# ENV (test seams — the ops-runner passes no packet-supplied environment,
# only an argv built from the allowlist, so a packet cannot set these)
#   BOSS_RECLAIM_OPT_DIR     where the candidates and live trees live (/opt)
#   BOSS_RECLAIM_PROC_DIR    the process table (/proc)
#   BOSS_RECLAIM_CAPTURE_DIR where the second-stack capture lives
#                            (/var/backups/boss/second-stack)
#   BOSS_RECLAIM_CAPTURE_OWNER the uid that must own it and its parent,
#                            and the dump directory's parent (0)
#   BOSS_RECLAIM_DUMP_DIR    where the retired leg's dumps live
#                            (/var/backups/boss-cluster-pg)
#   BOSS_RECLAIM_MOUNTINFO   the mount table (/proc/self/mountinfo)
#   BOSS_RECLAIM_LINK_DIRS   directories scanned for symlinks into a candidate
#   BOSS_RECLAIM_UNIT_DIRS   directories of unit files grepped for a candidate
#   BOSS_RECLAIM_NOW         the clock, epoch seconds (default: now)

set -uo pipefail

ME="reclaim-gcp-root"
say() { echo "$ME: $*" >&2; }
refuse() { say "REFUSED — $*"; say "  nothing was removed."; exit 2; }

OPT="${BOSS_RECLAIM_OPT_DIR:-/opt}"
PROC="${BOSS_RECLAIM_PROC_DIR:-/proc}"
MOUNTINFO="${BOSS_RECLAIM_MOUNTINFO:-/proc/self/mountinfo}"
UNIT_DIRS="${BOSS_RECLAIM_UNIT_DIRS:-/etc/systemd/system /lib/systemd/system /usr/lib/systemd/system}"
CAPTURE_DIR="${BOSS_RECLAIM_CAPTURE_DIR:-/var/backups/boss/second-stack}"
CAPTURE_OWNER="${BOSS_RECLAIM_CAPTURE_OWNER:-0}"
JOURNAL_KEEP="1G"
MIN_AGE_DAYS=30
NAME_RE='^boss-binbak-[A-Za-z0-9._-]+$'
# retire-second-stack.sh: DUMP=$BACKUP_DIR/second-stack-$(date -u +%Y%m%dT%H%M%SZ).sql
CAPTURE_RE='^second-stack-[0-9]{8}T[0-9]{6}Z\.sql$'
DUMP_DIR="${BOSS_RECLAIM_DUMP_DIR:-/var/backups/boss-cluster-pg}"
# boss-backup.yaml's dump leg names boss-$(date -u +%Y%m%d-%H%M%S).sql.gz;
# the retired receiver stamped its own (bound 8).
DUMP_RE='^boss-[A-Za-z0-9._:+-]+\.sql\.gz$'

# --- bound 1: the argument -------------------------------------------------
usage() {
    say "usage: $ME --dry-run | <plan-sha256>"
    say "  --dry-run       every bound, then the plan on stdout and plan-sha256 on stderr; removes nothing"
    say "  <plan-sha256>   remove what the signed plan names, if today's plan still hashes to it"
    say "  this verb takes no path: what it may remove is fixed in the script"
    exit 2
}
[ "$#" -eq 1 ] || usage
APPROVED=""
case "$1" in
    --dry-run) DRY=1 ;;
    *)
        [[ "$1" =~ ^[0-9a-f]{64}$ ]] || { say "the argument is --dry-run or a 64-hex plan hash, not \`$1\`"; usage; }
        DRY=0; APPROVED="$1" ;;
esac

TMP=$(mktemp -d) || exit 1
trap 'rm -rf "$TMP"' EXIT

under() { # <path> <dir> — path is dir or inside it
    [ "$1" = "$2" ] || [[ "$1" == "$2"/* ]]
}

OPT_REAL=$(realpath -e -- "$OPT" 2>/dev/null) \
    || refuse "$OPT does not resolve on this host"
LINK_DIRS="${BOSS_RECLAIM_LINK_DIRS:-$OPT_REAL /usr/local/bin /usr/bin /usr/sbin /etc/systemd/system}"
free_mib() { df -Pm -- "$OPT_REAL" 2>/dev/null | awk 'NR == 2 { print $4 }'; }

# READING A CHECKOUT RUNS NOTHING IT NAMES (adversarial review of car 3,
# 2026-09-27). A repository's .git/config can name commands — a clean
# filter, a process filter, an fsmonitor, a gpg program — and porcelain
# runs them: `git status` re-hashes every stat-dirty file through the
# clean filter, and a `cp -a` copy is stat-dirty everywhere, so the
# review planted a filter and the plan verb ran it, as root. So the read
# is PLUMBING that consults no command at all: for-each-ref, rev-list,
# ls-files, diff-index --cached --name-only, and hash-object
# --no-filters, with fsmonitor forced off on the command line (which
# outranks the repository's config).
#
# GIT NEVER RUNS AS ROOT (re-review of f5bd6eac, reproduced). Plumbing is
# not command-free under a partial clone: with a promisor remote and a
# missing object, rev-list and diff-index --cached HEAD lazily FETCH, and
# the fetch runs the remote's `uploadpack` — as root, before the read
# failed closed. So (a) a checkout whose config names a partial clone or
# a promisor remote is not read at all; (b) GIT_NO_LAZY_FETCH=1 (git
# 2.44+, ignored below); and (c) this verb, when root, reads a checkout
# AS ITS OWNER through setpriv — and a ROOT-owned one as nobody (uid
# 65534), so whatever path to a command remains runs with no authority.
# Reading as someone other than the owner needs safe.directory, given on
# the command line (git's command scope, which a repository's config
# cannot set); it is safe because the reader is never root. A file
# nobody cannot read makes the state "could not be read", and the
# checkout is kept either way.
#
# checkout_git <dir> <git args...> — git on the checkout at <dir> and on
# nothing else:
#   - the environment is scrubbed of every variable that redirects git to
#     another repository, index, object store or config (a GIT_DIR in the
#     runner's environment must not change the evidence), and no system
#     or global config is read;
#   - DISCOVERY, not --git-dir, with safe.directory naming <dir> alone;
#   - the ceiling stops discovery at <dir>, so a broken .git is an error,
#     never the state of a repository above it;
#   - --no-optional-locks: nothing rewrites the index.
# CHECKOUT_AS is the setpriv prefix checkout_state chose, or empty.
CHECKOUT_AS=()
checkout_git() {
    local r="$1"; shift
    env -u GIT_DIR -u GIT_WORK_TREE -u GIT_INDEX_FILE -u GIT_OBJECT_DIRECTORY \
        -u GIT_ALTERNATE_OBJECT_DIRECTORIES -u GIT_COMMON_DIR -u GIT_NAMESPACE \
        -u GIT_CONFIG -u GIT_CONFIG_PARAMETERS -u GIT_CONFIG_COUNT \
        -u GIT_EXTERNAL_DIFF -u GIT_PAGER -u GIT_ASKPASS -u GIT_SSH_COMMAND \
        GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null GIT_NO_LAZY_FETCH=1 \
        GIT_CEILING_DIRECTORIES="${r%/*}" \
        "${CHECKOUT_AS[@]+"${CHECKOUT_AS[@]}"}" \
        git -C "$r" --no-pager --no-optional-locks -c core.fsmonitor=false \
        -c safe.directory="$r" "$@"
}

# ascii — a variable piece of text (a path, a branch name, git's error)
# reduced to printable ASCII before it enters the plan: the runner
# refuses a plan that is not UTF-8, and a byte cut can split a character.
ascii() { LC_ALL=C tr -c '\n -~' '?'; }

# At most this many unpushed branches are named in the plan; the count
# covers all of them. The runner refuses a plan over OPS_OUTPUT_CAP
# (100 KiB), and one line per branch reached 112,659 bytes at 1690
# branches in review — the reclaim blocked again, by its own evidence.
BRANCHES_SHOWN=20

# checkout_state <dir> — the evidence lines for one checkout on stdout;
# exit non-zero, the reason in $TMP/git.err, on any read that failed.
# The caller prints either all of it or the refusal to read, never half.
checkout_state() {
    local r="$1" top ref n head_n heads=0 owner group me meta path mode sha stage rec changed=0
    local staged untracked stashes shown more rn all_n worktrees modules glob='[][*?\\]'
    local net u not_network urls_shown
    local -a pushed
    command -v git >/dev/null 2>&1 || { echo "git is not on PATH" > "$TMP/git.err"; return 1; }
    [ -d "$r/.git" ] && ! [ -L "$r/.git" ] \
        || { echo ".git is not a directory (a linked worktree or submodule keeps its repository elsewhere)" > "$TMP/git.err"; return 1; }
    owner=$(stat -c %u -- "$r/.git" 2> "$TMP/git.err") || return 1
    group=$(stat -c %g -- "$r/.git" 2> "$TMP/git.err") || return 1
    me=$(id -u 2> "$TMP/git.err") || return 1
    CHECKOUT_AS=()
    if [ "$me" = 0 ]; then
        # Never as root: as the owner, and a root-owned checkout as nobody.
        if [ "$owner" = 0 ]; then owner=65534; group=65534; fi
        command -v setpriv >/dev/null 2>&1 || { echo "setpriv is not on PATH, so it cannot be read as an account other than root (uid $owner)" > "$TMP/git.err"; return 1; }
        CHECKOUT_AS=(setpriv "--reuid=$owner" "--regid=$group" --clear-groups --)
    elif [ "$owner" != "$me" ]; then
        echo "its .git is owned by uid $owner, and only root can read it as that account" > "$TMP/git.err"; return 1
    fi
    top=$(checkout_git "$r" rev-parse --show-toplevel 2> "$TMP/git.err") || return 1
    [ "$top" = "$r" ] || { echo "git answered for $top, not this checkout" > "$TMP/git.err"; return 1; }

    # A partial clone is not read at all: a missing object makes plumbing
    # fetch from the promisor remote, which runs that remote's upload-pack.
    checkout_git "$r" config --get-regexp '^(extensions\.partialclone|remote\..+\.promisor)$' > "$TMP/promisor" 2> "$TMP/git.err"
    case $? in
        0) echo "it is a partial clone ($(awk '{ print $1 }' "$TMP/promisor" | LC_ALL=C sort | paste -sd, -)): reading it could fetch a missing object from its promisor remote, which runs that remote's upload-pack, so this plan does not read it" > "$TMP/git.err"; return 1 ;;
        1) ;;
        *) return 1 ;;
    esac

    # PUSHED MEANS A REMOTE THE CONFIG NAMES HOLDS IT (review 2435f065,
    # R3). A ref under refs/remotes/<x> whose <x> no remote.<x>.url names
    # was written by hand, or outlived its remote, and vouches for nothing,
    # so only the named remotes' refs are subtracted below. A name git
    # would read as a glob is refused rather than widened.
    checkout_git "$r" config --name-only --get-regexp '^remote\..+\.url$' > "$TMP/remote-keys" 2> "$TMP/git.err"
    case $? in
        0|1) ;;
        *) return 1 ;;
    esac
    sed -e 's/^remote\.//' -e 's/\.url$//' "$TMP/remote-keys" | LC_ALL=C sort -u > "$TMP/remotes" 2> "$TMP/git.err" || return 1
    # ONLY A NETWORK REMOTE COUNTS (review d83b2898, B2, reproduced): a
    # named remote whose URL is a local path — the checkout itself, a bare
    # repo inside it, one inside a sibling backup this same plan removes —
    # counted as pushed, so the signed write deleted the only copy. Each
    # remote's URLs (fetch and push, insteadOf expanded by `remote get-url`)
    # must ALL be network URLs (is_network_url) for its refs to count; any
    # other remote counts as nothing pushed and keeps the checkout. Every
    # URL is printed with its userinfo stripped, so the signer reads where
    # "pushed" means and no credential enters the signed bytes.
    pushed=()
    not_network=0
    : > "$TMP/remote-lines"
    while IFS= read -r rn; do
        if [ -z "$rn" ] || [[ "$rn" =~ $glob ]] || [[ "$rn" == -* ]]; then
            echo "its config names a remote '$(printf '%s' "$rn" | ascii)' that git would read as a pattern or an option, so what counts as pushed cannot be bounded" > "$TMP/git.err"; return 1
        fi
        checkout_git "$r" remote get-url --all "$rn" > "$TMP/urls" 2> "$TMP/git.err" || return 1
        checkout_git "$r" remote get-url --push --all "$rn" >> "$TMP/urls" 2> "$TMP/git.err" || return 1
        LC_ALL=C sort -u "$TMP/urls" > "$TMP/urls.u" 2> "$TMP/git.err" || return 1
        net=1
        urls_shown=""
        [ -s "$TMP/urls.u" ] || net=0
        while IFS= read -r u; do
            is_network_url "$u" || net=0
            urls_shown="${urls_shown:+$urls_shown, }$(redact_url "$u")"
        done < "$TMP/urls.u"
        if [ "$net" = 1 ]; then
            pushed+=("--remotes=$rn")
            printf '      %s: %s\n' "$(printf '%s' "$rn" | ascii)" "$urls_shown"
        else
            not_network=$((not_network + 1))
            printf '      %s: %s — not a network URL\n' "$(printf '%s' "$rn" | ascii)" "$urls_shown"
        fi >> "$TMP/remote-lines"
    done < "$TMP/remotes"

    # Branches with commits on no remote.
    checkout_git "$r" for-each-ref --format='%(refname)' refs/heads > "$TMP/heads" 2> "$TMP/git.err" || return 1
    : > "$TMP/unpushed"
    while IFS= read -r ref; do
        n=$(checkout_git "$r" rev-list --count "$ref" --not ${pushed[@]+"${pushed[@]}"} 2> "$TMP/git.err") || return 1
        [[ "$n" =~ ^[0-9]+$ ]] || { echo "rev-list counted '$n' for $ref" > "$TMP/git.err"; return 1; }
        if [ "$n" -gt 0 ]; then
            heads=$((heads + 1))
            printf '      %s: %s commit(s)\n' "${ref#refs/heads/}" "$n" >> "$TMP/unpushed"
        fi
    done < "$TMP/heads"
    LC_ALL=C sort "$TMP/unpushed" > "$TMP/unpushed.sorted" 2> "$TMP/git.err" || return 1
    head -n "$BRANCHES_SHOWN" "$TMP/unpushed.sorted" > "$TMP/unpushed.shown" 2> "$TMP/git.err" || return 1
    shown=$(wc -l < "$TMP/unpushed.shown") || return 1
    more=$((heads - shown))

    head_n=$(checkout_git "$r" rev-list --count HEAD --not --branches ${pushed[@]+"${pushed[@]}"} 2> "$TMP/git.err") || return 1
    [[ "$head_n" =~ ^[0-9]+$ ]] || { echo "rev-list counted '$head_n' for HEAD" > "$TMP/git.err"; return 1; }

    # Every ref at once (review 2435f065, R1): tags, notes, stashes,
    # refs/wip-style refs and every linked worktree's HEAD hold commits the
    # counts above never read. The same plumbing, so it runs nothing.
    all_n=$(checkout_git "$r" rev-list --count --all --not ${pushed[@]+"${pushed[@]}"} 2> "$TMP/git.err") || return 1
    [[ "$all_n" =~ ^[0-9]+$ ]] || { echo "rev-list counted '$all_n' for every ref" > "$TMP/git.err"; return 1; }

    # Work this plan cannot read, counted so it keeps the checkout (review
    # 2435f065). B1: a linked worktree made elsewhere with `git worktree
    # add` keeps its HEAD and INDEX in .git/worktrees/<name> — removing
    # this directory destroyed a staged change OUTSIDE /opt while the plan
    # read six zeros. R2: a deinit'd submodule keeps its repository only in
    # .git/modules/<name>, which the nested search never enters.
    worktrees=$(git_dir_entries "$r/.git/worktrees") || return 1
    modules=$(git_dir_entries "$r/.git/modules") || return 1

    # Staged: the index against HEAD, blob ids only — no file is read.
    checkout_git "$r" diff-index --cached --name-only -z HEAD > "$TMP/staged" 2> "$TMP/git.err" || return 1
    staged=$(tr -cd '\0' < "$TMP/staged" | wc -c) || return 1

    # Tracked files whose bytes differ from the index: each file hashed
    # with NO filters and compared to its index entry. A file a filter
    # would normalise may count as changed — the evidence errs toward
    # work. A missing file, a conflict, or a path hash-object cannot be
    # handed (a newline) counts as changed; a submodule is not read.
    checkout_git "$r" ls-files -s -z > "$TMP/stage" 2> "$TMP/git.err" || return 1
    : > "$TMP/paths"; : > "$TMP/want"
    while IFS= read -r -d '' rec; do
        meta="${rec%%$'\t'*}"; path="${rec#*$'\t'}"
        read -r mode sha stage <<< "$meta"
        if [ "$stage" != 0 ]; then changed=$((changed + 1)); continue; fi
        case "$mode" in
            160000) continue ;;
            120000)
                if [ -L "$r/$path" ]; then
                    n=$(printf '%s' "$(readlink -- "$r/$path")" | checkout_git "$r" hash-object --stdin 2> "$TMP/git.err") || return 1
                    [ "$n" = "$sha" ] || changed=$((changed + 1))
                else
                    changed=$((changed + 1))
                fi ;;
            *)
                if [[ "$path" == *$'\n'* ]] || [ -L "$r/$path" ] || ! [ -f "$r/$path" ]; then
                    changed=$((changed + 1))
                else
                    printf '%s\n' "$path" >> "$TMP/paths"
                    printf '%s\n' "$sha" >> "$TMP/want"
                fi ;;
        esac
    done < "$TMP/stage"
    checkout_git "$r" hash-object --no-filters --stdin-paths < "$TMP/paths" > "$TMP/got" 2> "$TMP/git.err" || return 1
    n=$(paste -d ' ' "$TMP/want" "$TMP/got" | awk '$1 != $2 { d++ } END { print d + 0 }') || return 1
    [[ "$n" =~ ^[0-9]+$ ]] || { echo "the tracked-file comparison counted '$n'" > "$TMP/git.err"; return 1; }
    changed=$((changed + n))

    # Untracked: the directory walk against the index and .gitignore.
    checkout_git "$r" ls-files --others --exclude-standard -z > "$TMP/untracked" 2> "$TMP/git.err" || return 1
    untracked=$(tr -cd '\0' < "$TMP/untracked" | wc -c) || return 1

    # Stashes: the stash reflog's length, read without log's pretty
    # machinery (log.showSignature would run the gpg program).
    checkout_git "$r" for-each-ref --format='%(refname)' refs/stash > "$TMP/stashref" 2> "$TMP/git.err" || return 1
    stashes=0
    if [ -s "$TMP/stashref" ]; then
        stashes=$(checkout_git "$r" rev-list --walk-reflogs --count refs/stash 2> "$TMP/git.err") || return 1
        [[ "$stashes" =~ ^[0-9]+$ ]] || { echo "rev-list counted '$stashes' stashes" > "$TMP/git.err"; return 1; }
    fi

    echo "    remotes its config names, with their URLs (userinfo removed; only a network remote's refs count as pushed):"
    if [ -s "$TMP/remote-lines" ]; then cat "$TMP/remote-lines"; else echo "      (none)"; fi
    echo "    remotes whose URL is not a network URL (a local path, file:// or anything else; their refs count as nothing pushed, and any keeps the checkout): $not_network"
    echo "    branches with commits on no remote: $heads"
    ascii < "$TMP/unpushed.shown"
    [ "$more" -le 0 ] || echo "      ...and $more more"
    echo "    commits at HEAD on no branch and no remote: $head_n"
    echo "    commits reachable from any ref (tags, notes, stashes, other refs and linked worktrees' HEADs included) and on no configured remote: $all_n"
    echo "    linked worktrees (their HEADs and indexes live in this repository and are not read): $worktrees"
    echo "    submodule repositories under .git/modules (not read): $modules"
    echo "    staged changes not committed: $staged"
    echo "    tracked files whose bytes differ from the index (read without filters): $changed"
    echo "    untracked files (ignored ones not counted): $untracked"
    echo "    stashes: $stashes"
    echo "    not measured: reflogs, ignored files (what .gitignore and .git/info/exclude hide), and any repository elsewhere that borrows this one's objects (objects/info/alternates); a branch squash-merged upstream still counts as unpushed, and a remote-tracking ref is trusted as of the last fetch."
    CHECKOUT_UNSAVED=$((not_network + heads + head_n + all_n + worktrees + modules + staged + changed + untracked + stashes))
}

# is_network_url <url> — exit 0 when <url> names ANOTHER machine over the
# network, the only kind of remote whose refs count as pushed (review
# d83b2898, B2): `scheme://host/...` for scheme https, http, ssh, git,
# git+ssh or ssh+git with a non-empty host, or scp-style `[user@]host:path`
# (no slash before the colon, not starting with / . or ~). Everything else
# is not: file://, a path, `~`, a `<transport>::<address>` remote helper,
# an empty host, localhost or a loopback address, anything unparsable.
# An allowlist of schemes rather than "any but file": an unknown helper
# scheme keeps the checkout.
is_network_url() {
    local u="$1" scheme auth host
    if [[ "$u" =~ ^([A-Za-z][A-Za-z0-9+.-]*)://(.*)$ ]]; then
        scheme="${BASH_REMATCH[1],,}"
        case "$scheme" in https|http|ssh|git|git+ssh|ssh+git) ;; *) return 1 ;; esac
        auth="${BASH_REMATCH[2]%%/*}"
    elif [[ "$u" == *::* ]]; then
        return 1
    elif [[ "$u" =~ ^([^/:]+): ]]; then
        auth="${BASH_REMATCH[1]}"
        case "$u" in [/.~]*) return 1 ;; esac
    else
        return 1
    fi
    host="${auth##*@}"
    if [[ "$host" == \[* ]]; then host="${host%%]*}]"; else host="${host%%:*}"; fi
    host="${host,,}"
    case "$host" in
        ''|localhost|localhost.|0.0.0.0|127.*|'[::1]'|'[::]'|'[0:0:0:0:0:0:0:1]') return 1 ;;
    esac
    return 0
}

# redact_url <url> — <url> as the plan prints it: any userinfo (`user@`,
# `user:token@`) stripped, and the query and fragment dropped, so no
# credential a URL carries enters the signed bytes. It strips through the
# LAST `@`, so a password holding a `/` cannot survive; a path with an `@`
# is over-redacted, which errs toward hiding.
redact_url() {
    local u="$1" pre rest
    if [[ "$u" =~ ^([A-Za-z][A-Za-z0-9+.-]*://)(.*)$ ]]; then
        pre="${BASH_REMATCH[1]}"
        rest="${BASH_REMATCH[2]}"
        [[ "$rest" == *@* ]] && rest="${rest##*@}"
        u="$pre$rest"
    elif [[ "$u" == *@* ]] && ! [[ "$u" == [/.~]* ]]; then
        u="${u##*@}"
    fi
    u="${u%%\?*}"
    u="${u%%#*}"
    printf '%s' "$u" | ascii | cut -c1-300
}

# git_dir_entries <dir> — how many entries <dir> (inside a .git) holds, 0
# when it does not exist. Exit 1, the reason in $TMP/git.err, when it is
# not a real directory or cannot be listed: a link there is neither
# followed nor passed. Read as the verb's own account — listing a
# directory runs nothing.
git_dir_entries() {
    local d="$1" n
    if ! [ -e "$d" ] && ! [ -L "$d" ]; then echo 0; return 0; fi
    if [ -L "$d" ] || ! [ -d "$d" ]; then
        echo "$d is not a real directory, so what it holds cannot be judged" > "$TMP/git.err"; return 1
    fi
    n=$(find "$d" -mindepth 1 -maxdepth 1 -printf 'x\n' 2> "$TMP/git.err" | wc -l) || return 1
    [[ "$n" =~ ^[0-9]+$ ]] || { echo "counted '$n' entries in $d" > "$TMP/git.err"; return 1; }
    echo "$n"
}

# checkouts_evidence <candidate> — every checkout under <candidate> (each
# `.git`, found without crossing a filesystem and without descending into
# one), in C order, each with its unpushed state or why that could not be
# read, on stdout. CHECKOUTS_CLEAN is 1 when EVERY one was read and every
# count is 0 — the proof that the candidate holds nothing unpushed — and 0
# otherwise: one unreadable or unclean nested checkout keeps the whole
# candidate. Exit 0 when a checkout was found, 1 when none was, 2 (the
# reason in $TMP/find.err) when the search failed.
CHECKOUT_UNSAVED=1
CHECKOUTS_CLEAN=0
checkouts_evidence() {
    local g
    CHECKOUTS_CLEAN=0
    find "$1" -xdev -name .git -prune -print > "$TMP/gits.raw" 2> "$TMP/find.err" || return 2
    [ -s "$TMP/gits.raw" ] || return 1
    LC_ALL=C sort "$TMP/gits.raw" > "$TMP/gits" 2> "$TMP/find.err" || return 2
    CHECKOUTS_CLEAN=1
    while IFS= read -r g; do
        echo "  checkout $(printf '%s' "${g%/.git}" | ascii):"
        CHECKOUT_UNSAVED=1
        if checkout_state "${g%/.git}" > "$TMP/state"; then
            cat "$TMP/state"
            [ "$CHECKOUT_UNSAVED" = 0 ] || CHECKOUTS_CLEAN=0
        else
            echo "    its unpushed state could not be read: $(head -n 1 "$TMP/git.err" | ascii | cut -c1-300)"
            CHECKOUTS_CLEAN=0
        fi
    done < "$TMP/gits"
    return 0
}

# The one name whose checkout may be removed, and only when proven clean.
CHECKOUT_NAME="boss-dev-bak"
FREE_BEFORE=$(free_mib)
say "root free before: ${FREE_BEFORE:-unknown} MiB (df $OPT_REAL)"

# --- bound 2, 3 + 4: the names, not a checkout, the age --------------------
CANDS=()
KEPT=()
PROVEN=()
: > "$TMP/kept"
now="${BOSS_RECLAIM_NOW:-$(date +%s)}"
cutoff=$((now - MIN_AGE_DAYS * 86400))
for d in "$OPT"/boss-binbak-* "$OPT"/boss-dev-bak; do
    [ -e "$d" ] || [ -L "$d" ] || continue
    name="${d##*/}"
    if [ "$name" != "boss-dev-bak" ] && ! [[ "$name" =~ $NAME_RE ]]; then
        refuse "\`$d\` matched the glob but its name is not ^boss-binbak-[A-Za-z0-9._-]+\$ — a name this verb was not reviewed to remove"
    fi
    if [ -L "$d" ]; then
        refuse "\`$d\` is a symlink (to $(readlink -- "$d")) — this verb removes only real directories under $OPT, never what a link points at"
    fi
    [ -d "$d" ] || refuse "\`$d\` is not a directory — this verb removes backup directories only"
    real=$(realpath -e -- "$d") || refuse "\`$d\` does not resolve"
    [ "$real" = "$OPT_REAL/$name" ] \
        || refuse "\`$d\` resolves to $real, not $OPT_REAL/$name — it is not the directory its name says"
    checkouts_evidence "$real" > "$TMP/evidence"
    case $? in
        0) # A checkout (bound 3).
            if [ "$name" = "$CHECKOUT_NAME" ] && [ "$CHECKOUTS_CLEAN" = 1 ]; then
                # Proven clean: a removal candidate, under every other
                # bound below. Its proof rides in the plan's bytes.
                cp -- "$TMP/evidence" "$TMP/proof.$name" \
                    || refuse "could not keep the proof read from \`$real\`"
                PROVEN+=("$real")
            else
                # Kept, not refused: the rest of the plan stands.
                {
                    if [ "$name" = "$CHECKOUT_NAME" ]; then
                        echo "would keep $real — it holds a git checkout this plan could not prove holds nothing unpushed, and a checkout carrying unpushed work is work, not a backup. Its unpushed state, against its own remote-tracking refs as of its last fetch:"
                    else
                        echo "would keep $real — it holds a git checkout, and this verb removes only $OPT_REAL/$CHECKOUT_NAME's (when its plan proves it holds nothing unpushed). Its unpushed state, against its own remote-tracking refs as of its last fetch:"
                    fi
                    cat "$TMP/evidence"
                } >> "$TMP/kept"
                KEPT+=("$real")
                say "keeping \`$real\` — it holds $(wc -l < "$TMP/gits") .git; its unpushed state is in the plan"
                continue
            fi ;;
        1) ;;
        *) refuse "could not search \`$real\` for a .git ($(head -c 300 "$TMP/find.err")) — a bound that cannot be evaluated is not passed" ;;
    esac
    young=$(find "$real" -xdev -newerct "@$cutoff" -print -quit 2> "$TMP/find.err") \
        || refuse "could not read the ages under \`$real\` ($(head -c 300 "$TMP/find.err")) — a bound that cannot be evaluated is not passed"
    [ -z "$young" ] \
        || refuse "\`$real\` holds \`$young\`, changed (ctime) in the last $MIN_AGE_DAYS days — a fresh backup, even one copied with its old mtimes, may be somebody's rollback target, and this verb removes only old ones"
    if [ -e "$TMP/proof.$name" ]; then
        # A proven checkout goes FIRST, so a write whose re-proof refuses
        # has removed nothing at all.
        CANDS=("$real" "${CANDS[@]+"${CANDS[@]}"}")
    else
        CANDS+=("$real")
    fi
done

# --- bound 5: not live -----------------------------------------------------
# (a) the live trees themselves.
for live in "$OPT/boss" "$OPT/boss-cli"; do
    [ -e "$live" ] || continue
    lr=$(realpath -e -- "$live") || refuse "$live does not resolve, so whether it points into a backup cannot be judged"
    for c in "${CANDS[@]+"${CANDS[@]}"}"; do
        if under "$lr" "$c" || under "$c" "$lr"; then
            refuse "$live resolves to $lr, which is \`$c\` or overlaps it — that backup is LIVE"
        fi
    done
done
if [ "${#CANDS[@]}" -gt 0 ]; then
    # (b) mounts at or under a candidate. Field 5 is the mount point,
    # with space, tab, newline and backslash octal-escaped.
    [ -r "$MOUNTINFO" ] || refuse "$MOUNTINFO cannot be read, so whether a backup has a mount inside it cannot be judged"
    while read -r _ _ _ _ mp _; do
        mp=$(printf '%b' "$mp")
        for c in "${CANDS[@]}"; do
            under "$mp" "$c" && refuse "$mp is a mount point at or under \`$c\` — rm would reach through a bind mount into another tree"
        done
    done < "$MOUNTINFO"
    # (c) symlinks elsewhere that resolve into a candidate.
    for ld in $LINK_DIRS; do
        [ -d "$ld" ] || continue
        find "$ld" -maxdepth 2 -type l -print0 > "$TMP/links" 2> "$TMP/find.err" \
            || refuse "could not list the symlinks under $ld ($(head -c 300 "$TMP/find.err")) — a bound that cannot be evaluated is not passed"
        while IFS= read -r -d '' link; do
            skip=0
            for c in "${CANDS[@]}"; do under "$link" "$c" && skip=1; done
            [ "$skip" = 1 ] && continue
            t=$(realpath -m -- "$link" 2>/dev/null) || continue
            for c in "${CANDS[@]}"; do
                under "$t" "$c" && refuse "the symlink $link resolves to $t, inside \`$c\` — that backup is still linked to"
            done
        done < "$TMP/links"
    done
    # (d) running processes. A table that cannot be read passed silently
    # here until the adversarial review of 7bca1fee (f44ca628): the glob
    # matched nothing and the loop ran zero times.
    { [ -d "$PROC" ] && [ -r "$PROC" ] && [ -x "$PROC" ]; } \
        || refuse "$PROC cannot be read, so whether a running process executes from a backup cannot be judged — a bound that cannot be evaluated is not passed"
    for exe in "$PROC"/[0-9]*/exe; do
        t=$(readlink -- "$exe" 2>/dev/null) || continue
        t="${t% (deleted)}"
        for c in "${CANDS[@]}"; do
            if under "$t" "$c"; then
                pid="${exe%/exe}"; pid="${pid##*/}"
                refuse "process $pid ($(cat "$PROC/$pid/comm" 2>/dev/null || echo '?')) runs $t, inside \`$c\` — that backup is LIVE"
            fi
        done
    done
    # (e) loaded service units.
    if ! systemctl list-units --type=service --all --no-pager --plain --no-legend > "$TMP/units" 2> "$TMP/units.err"; then
        refuse "systemctl could not list this host's service units ($(head -c 300 "$TMP/units.err")), so whether a backup is a unit's binary cannot be judged — a bound that cannot be evaluated is not passed"
    fi
    while read -r unit _; do
        [ -n "$unit" ] || continue
        props=$(systemctl show -p ExecStart -p ExecStartPre -p WorkingDirectory -p FragmentPath --value -- "$unit" 2> "$TMP/show.err") \
            || refuse "systemctl show $unit failed ($(head -c 300 "$TMP/show.err")), so whether it runs from a backup cannot be judged"
        set -f
        for p in $(printf '%s\n' "$props" | grep -oE '/[^ ;]+'); do
            t=$(realpath -m -- "$p" 2>/dev/null) || continue
            for c in "${CANDS[@]}"; do
                under "$t" "$c" && refuse "the unit $unit names $p (resolving to $t), inside \`$c\` — that backup is LIVE"
            done
        done
        set +f
    done < "$TMP/units"
    # (f) unit FILES on disk, loaded or not, drop-ins included.
    for ud in $UNIT_DIRS; do
        [ -d "$ud" ] || continue
        for c in "${CANDS[@]}"; do
            for form in "$c" "$OPT/${c##*/}"; do
                grep -rlF -- "$form" "$ud" > "$TMP/grep" 2> "$TMP/grep.err"
                case $? in
                    0) refuse "the unit file $(head -n 1 "$TMP/grep") names \`$form\` — a unit kept on disk (stopped, disabled, kept for a restore) still runs from that backup" ;;
                    1) ;;
                    *) refuse "could not search the unit files under $ud ($(head -c 300 "$TMP/grep.err")) — a bound that cannot be evaluated is not passed" ;;
                esac
            done
        done
    done
fi

# --- bound 7: the second-stack capture ------------------------------------
# capture_holder <file> — the first process holding <file> open, as
# "<pid> (<comm>)" on stdout. Exit 0 when one does, 1 when none does, 2
# (the reason in $TMP/holder.err) when the table cannot be read — which
# the caller refuses. Compared by device:inode through `stat -L` on each
# /proc/<pid>/fd entry, one stat per process: the kernel follows the fd to
# the open file itself, so neither a rename nor another mount namespace's
# spelling of the path hides it.
capture_holder() {
    local id p init=0
    id=$(LC_ALL=C stat -c '%d:%i' -- "$1" 2> "$TMP/holder.err") || return 2
    if ! [ -d "$PROC" ] || ! [ -r "$PROC" ] || ! [ -x "$PROC" ]; then
        echo "$PROC cannot be read" > "$TMP/holder.err"; return 2
    fi
    for p in "$PROC"/[0-9]*; do
        [ -e "$p/fd" ] || continue # it exited (or, in a fixture, has no fd table)
        if ! [ -r "$p/fd" ] || ! [ -x "$p/fd" ]; then
            [ -e "$p/fd" ] || continue
            echo "the open files of process ${p##*/} ($p/fd) cannot be read" > "$TMP/holder.err"; return 2
        fi
        # An fd closed, or a process gone, between the glob and the stat
        # says "No such file or directory", and so does an empty table's
        # unexpanded glob. ANY other complaint — a denial, EIO, E2BIG, a
        # link loop — means an fd went unread, so the bound was not
        # evaluated (adversarial review of 7bca1fee: only EACCES refused).
        LC_ALL=C stat -L -c '%d:%i' -- "$p/fd"/* > "$TMP/fds" 2> "$TMP/fds.err"
        grep -vF 'No such file or directory' "$TMP/fds.err" > "$TMP/fds.bad"
        case $? in
            0) echo "the open files of process ${p##*/} cannot be followed: $(head -n 1 "$TMP/fds.bad")" > "$TMP/holder.err"; return 2 ;;
            1) ;;
            *) echo "could not read stat's complaints about process ${p##*/}" > "$TMP/holder.err"; return 2 ;;
        esac
        [ "${p##*/}" = 1 ] && init=1
        if grep -qxF -- "$id" "$TMP/fds"; then
            echo "${p##*/} ($(cat "$p/comm" 2>/dev/null || echo '?'))"
            return 0
        fi
    done
    # A table without init's open files is not the host's whole table
    # (a partial or foreign /proc): a holder could be among the missing.
    [ "$init" = 1 ] || { echo "the open files of process 1 (init) were not read from $PROC, so the table is not the whole host's" > "$TMP/holder.err"; return 2; }
    return 1
}

# capture_links <file> — its hard-link count, or exit 2 (reason in
# $TMP/links.err). A capture with a second name is not freed by removing
# this one, and a boss-cluster-pg dump hardlinked in under a capture's
# name would otherwise pass every other bound (adversarial review of
# 7bca1fee, MUST-FIX 1: the write removed the name and the OK line
# claimed bytes that were never freed).
capture_links() {
    local n
    n=$(LC_ALL=C stat -c %h -- "$1" 2> "$TMP/links.err") || return 2
    [[ "$n" =~ ^[0-9]+$ ]] || { echo "stat counted '$n' links" > "$TMP/links.err"; return 2; }
    echo "$n"
}

# dir_sound <dir> — "owner uid <u>, mode <m>" on stdout when <dir> is
# owned by CAPTURE_OWNER and writable by no group and no other account;
# exit 2 with the reason in $TMP/dir.err otherwise. Anyone who can write
# the capture directory or its parent can put a file under a capture's
# name (review of 7bca1fee, item 3).
dir_sound() {
    local s u m
    s=$(LC_ALL=C stat -c '%u %a' -- "$1" 2> "$TMP/stat.err") \
        || { echo "could not read the owner and mode of \`$1\` ($(head -c 300 "$TMP/stat.err"))" > "$TMP/dir.err"; return 2; }
    read -r u m <<< "$s"
    [[ "$u" =~ ^[0-9]+$ && "$m" =~ ^[0-7]+$ ]] \
        || { echo "stat answered owner '$u' mode '$m' for \`$1\`" > "$TMP/dir.err"; return 2; }
    [ "$u" = "$CAPTURE_OWNER" ] \
        || { echo "\`$1\` is owned by uid $u, not uid $CAPTURE_OWNER — a directory another account owns can be given any file" > "$TMP/dir.err"; return 2; }
    [ $(( 8#$m & 8#022 )) -eq 0 ] \
        || { echo "\`$1\` is group- or other-writable (mode $m) — another account could put a file there under a capture's name" > "$TMP/dir.err"; return 2; }
    echo "owner uid $u, mode $m"
}

# The plan's line for each capture: "<bytes> <sha256> <path>"; the
# directory's owner and mode, once, in $TMP/capdir.
: > "$TMP/captures"
: > "$TMP/capdir"
if [ -e "$CAPTURE_DIR" ] || [ -L "$CAPTURE_DIR" ]; then
    [ -L "$CAPTURE_DIR" ] \
        && refuse "\`$CAPTURE_DIR\` is a symlink (to $(readlink -- "$CAPTURE_DIR")) — the capture is removed only from the directory retire-second-stack wrote it to, never through a link"
    [ -d "$CAPTURE_DIR" ] || refuse "\`$CAPTURE_DIR\` is not a directory"
    cap_real=$(realpath -e -- "$CAPTURE_DIR" 2>/dev/null) \
        || refuse "\`$CAPTURE_DIR\` does not resolve"
    [ "$cap_real" = "$CAPTURE_DIR" ] \
        || refuse "\`$CAPTURE_DIR\` resolves to $cap_real — a link on the way means it is not the directory retire-second-stack wrote its capture to"
    { [ -r "$CAPTURE_DIR" ] && [ -x "$CAPTURE_DIR" ]; } \
        || refuse "\`$CAPTURE_DIR\` cannot be listed, so what it holds cannot be judged — a bound that cannot be evaluated is not passed"
    # Owned by root and writable by nobody else, the directory and its
    # parent both; the two facts ride in the plan's bytes.
    cap_parent="${CAPTURE_DIR%/*}"
    dir_note=$(dir_sound "$CAPTURE_DIR") || refuse "$(cat "$TMP/dir.err")"
    parent_note=$(dir_sound "$cap_parent") || refuse "$(cat "$TMP/dir.err")"
    echo "  capture directory $CAPTURE_DIR: $dir_note; its parent $cap_parent: $parent_note" > "$TMP/capdir"
    for f in "$CAPTURE_DIR"/second-stack-*.sql; do
        [ -e "$f" ] || [ -L "$f" ] || continue
        name="${f##*/}"
        [[ "$name" =~ $CAPTURE_RE ]] \
            || refuse "\`$f\` matched the glob but its name is not ^second-stack-[0-9]{8}T[0-9]{6}Z\\.sql\$ — retire-second-stack names its capture second-stack-<date -u +%Y%m%dT%H%M%SZ>.sql, and this verb was reviewed to remove that and nothing else"
        [ -L "$f" ] \
            && refuse "\`$f\` is a symlink (to $(readlink -- "$f")) — this verb removes the capture itself, never what a link points at"
        [ -f "$f" ] || refuse "\`$f\` is not a regular file — the capture retire-second-stack writes is one"
        links=$(capture_links "$f") \
            || refuse "could not count the links to \`$f\` ($(head -c 300 "$TMP/links.err")) — a bound that cannot be evaluated is not passed"
        [ "$links" = 1 ] \
            || refuse "\`$f\` has $links links — another name holds the same bytes, so removing this one frees nothing, and it may be a dump that is not the capture"
        real=$(realpath -e -- "$f" 2>/dev/null) || refuse "\`$f\` does not resolve"
        [ "$real" = "$CAPTURE_DIR/$name" ] \
            || refuse "\`$f\` resolves to $real, not $CAPTURE_DIR/$name — it is not the file its name says"
        [ -r "$MOUNTINFO" ] || refuse "$MOUNTINFO cannot be read, so whether a capture is a mount point cannot be judged"
        while read -r _ _ _ _ mp _; do
            [ "$(printf '%b' "$mp")" = "$f" ] \
                && refuse "\`$f\` is a mount point — rm would not remove what is mounted there"
        done < "$MOUNTINFO"
        holder=$(capture_holder "$f")
        case $? in
            0) refuse "process $holder holds \`$f\` open — a capture being written (pg_dump) or read (a restore) is in use, and removing it frees nothing while that process lives" ;;
            1) ;;
            *) refuse "whether a process holds \`$f\` open cannot be judged ($(head -c 300 "$TMP/holder.err")) — a bound that cannot be evaluated is not passed" ;;
        esac
        bytes=$(LC_ALL=C stat -c %s -- "$f" 2> "$TMP/stat.err") \
            || refuse "could not size \`$f\` ($(head -c 300 "$TMP/stat.err"))"
        sha256sum -- "$f" > "$TMP/sha" 2> "$TMP/sha.err" \
            || refuse "could not hash \`$f\` ($(head -c 300 "$TMP/sha.err"))"
        sha=$(cut -c1-64 "$TMP/sha")
        [[ "$sha" =~ ^[0-9a-f]{64}$ ]] || refuse "sha256sum answered '$sha' for \`$f\`"
        echo "$bytes $sha $f" >> "$TMP/captures"
    done
fi
CAP_N=$(wc -l < "$TMP/captures")
CAP_BYTES=$(awk '{ s += $1 } END { print s + 0 }' "$TMP/captures")
[ "$CAP_N" -gt 0 ] || say "no second-stack capture in $CAPTURE_DIR on this host — no capture to remove"

# --- bound 8: the cluster-pg dumps the retired boss-gcp leg deposited ------
# dir_closed <dir> — "owner uid <u>, mode <m>" on stdout when <dir> is
# writable by no group and no other account, WHOEVER owns it; exit 2 with
# the reason in $TMP/dir.err otherwise. The dumps' directory may be the
# deposit account's own (bound 8 says why); its parent is held to
# dir_sound.
dir_closed() {
    local s u m
    s=$(LC_ALL=C stat -c '%u %a' -- "$1" 2> "$TMP/stat.err") \
        || { echo "could not read the owner and mode of \`$1\` ($(head -c 300 "$TMP/stat.err"))" > "$TMP/dir.err"; return 2; }
    read -r u m <<< "$s"
    [[ "$u" =~ ^[0-9]+$ && "$m" =~ ^[0-7]+$ ]] \
        || { echo "stat answered owner '$u' mode '$m' for \`$1\`" > "$TMP/dir.err"; return 2; }
    [ $(( 8#$m & 8#022 )) -eq 0 ] \
        || { echo "\`$1\` is group- or other-writable (mode $m) — another account could put a file there under a dump's name" > "$TMP/dir.err"; return 2; }
    echo "owner uid $u, mode $m"
}

# dump_identity <file> — what the plan binds a dump by: its size, inode,
# mtime and ctime, in UTC to the nanosecond (bound 8 says why not its
# sha256). Exit non-zero, the reason in $TMP/stat.err, when stat fails.
dump_identity() {
    TZ=UTC LC_ALL=C stat -c '%s bytes, inode %i, modified %y, changed %z' -- "$1" 2> "$TMP/stat.err"
}

# The plan's line for each dump: "<path><TAB><identity>"; the directory's
# owner and mode, once, in $TMP/dumpdir. A dump's name holds no tab (the
# name bound), and the directory is fixed.
: > "$TMP/dumps"
: > "$TMP/dumpdir"
if [ -e "$DUMP_DIR" ] || [ -L "$DUMP_DIR" ]; then
    [ -L "$DUMP_DIR" ] \
        && refuse "\`$DUMP_DIR\` is a symlink (to $(readlink -- "$DUMP_DIR")) — the dumps are removed only from the directory the retired leg deposited them in, never through a link"
    [ -d "$DUMP_DIR" ] || refuse "\`$DUMP_DIR\` is not a directory"
    dump_real=$(realpath -e -- "$DUMP_DIR" 2>/dev/null) \
        || refuse "\`$DUMP_DIR\` does not resolve"
    [ "$dump_real" = "$DUMP_DIR" ] \
        || refuse "\`$DUMP_DIR\` resolves to $dump_real — a link on the way means it is not the directory the retired leg deposited its dumps in"
    { [ -r "$DUMP_DIR" ] && [ -x "$DUMP_DIR" ]; } \
        || refuse "\`$DUMP_DIR\` cannot be listed, so what it holds cannot be judged — a bound that cannot be evaluated is not passed"
    dump_parent="${DUMP_DIR%/*}"
    dir_note=$(dir_closed "$DUMP_DIR") || refuse "$(cat "$TMP/dir.err")"
    parent_note=$(dir_sound "$dump_parent") || refuse "$(cat "$TMP/dir.err")"
    echo "  dump directory $DUMP_DIR: $dir_note; its parent $dump_parent: $parent_note" > "$TMP/dumpdir"
    for f in "$DUMP_DIR"/boss-*.sql.gz; do
        [ -e "$f" ] || [ -L "$f" ] || continue
        name="${f##*/}"
        [[ "$name" =~ $DUMP_RE ]] \
            || refuse "\`$f\` matched the glob but its name is not ^boss-[A-Za-z0-9._:+-]+\\.sql\\.gz\$ — this verb was reviewed to remove the retired leg's dumps by that shape and nothing else"
        [ -L "$f" ] \
            && refuse "\`$f\` is a symlink (to $(readlink -- "$f")) — this verb removes the dump itself, never what a link points at"
        [ -f "$f" ] || refuse "\`$f\` is not a regular file — a dump the retired leg deposited is one"
        links=$(capture_links "$f") \
            || refuse "could not count the links to \`$f\` ($(head -c 300 "$TMP/links.err")) — a bound that cannot be evaluated is not passed"
        [ "$links" = 1 ] \
            || refuse "\`$f\` has $links links — another name holds the same bytes, so removing this one frees nothing"
        real=$(realpath -e -- "$f" 2>/dev/null) || refuse "\`$f\` does not resolve"
        [ "$real" = "$DUMP_DIR/$name" ] \
            || refuse "\`$f\` resolves to $real, not $DUMP_DIR/$name — it is not the file its name says"
        [ -r "$MOUNTINFO" ] || refuse "$MOUNTINFO cannot be read, so whether a dump is a mount point cannot be judged"
        while read -r _ _ _ _ mp _; do
            [ "$(printf '%b' "$mp")" = "$f" ] \
                && refuse "\`$f\` is a mount point — rm would not remove what is mounted there"
        done < "$MOUNTINFO"
        holder=$(capture_holder "$f")
        case $? in
            0) refuse "process $holder holds \`$f\` open — a dump being written (a deposit) or read (a restore) is in use, and removing it frees nothing while that process lives" ;;
            1) ;;
            *) refuse "whether a process holds \`$f\` open cannot be judged ($(head -c 300 "$TMP/holder.err")) — a bound that cannot be evaluated is not passed" ;;
        esac
        ident=$(dump_identity "$f") \
            || refuse "could not read the identity of \`$f\` ($(head -c 300 "$TMP/stat.err"))"
        printf '%s\t%s\n' "$f" "$ident" >> "$TMP/dumps"
    done
fi
DUMP_N=$(wc -l < "$TMP/dumps")
DUMP_BYTES=$(awk -F'\t' '{ split($2, a, " "); s += a[1] } END { print s + 0 }' "$TMP/dumps")
[ "$DUMP_N" -gt 0 ] || say "no cluster-pg dump in $DUMP_DIR on this host — no dump to remove"

# --- the plan: deterministic bytes ---------------------------------------
: > "$TMP/cands"
for c in "${CANDS[@]+"${CANDS[@]}"}"; do
    mib=$(du -sxm -- "$c" 2> "$TMP/du.err" | awk '{ print $1 }')
    [ -n "$mib" ] || refuse "du could not size \`$c\` ($(head -c 300 "$TMP/du.err"))"
    echo "$c $mib" >> "$TMP/cands"
done
render() {
    local c mib
    while read -r c mib; do
        echo "would remove $c ($mib MiB), holding:"
        find "$c" -mindepth 1 -maxdepth 1 -printf '  %f\n' | LC_ALL=C sort
        if [ -e "$TMP/proof.${c##*/}" ]; then
            echo "  it holds a git checkout, removed because this plan proves it holds nothing unpushed — every count below reads 0, against its own remote-tracking refs as of its last fetch; the write re-proves it just before its rm, and any line that reads differently refuses the whole run (exit 78), removing nothing:"
            cat "$TMP/proof.${c##*/}"
        fi
    done < "$TMP/cands"
    local bytes sha f
    while read -r bytes sha f; do
        echo "would remove $f ($bytes bytes, sha256 $sha) — the retired second stack's database capture (retire-second-stack, ops-request 7912c9ae), which David decided on 2026-09-28 to delete (backlog f44ca628)"
    done < "$TMP/captures"
    [ -s "$TMP/captures" ] && cat "$TMP/capdir"
    local ident
    while IFS=$'\t' read -r f ident; do
        echo "would remove $f ($ident) — a dump the retired boss-gcp off-site leg deposited; the GCS bucket is the off-site copy (David, 2026-10-01, backlog 4bf7bdd1)"
    done < "$TMP/dumps"
    [ -s "$TMP/dumps" ] && cat "$TMP/dumpdir"
    cat "$TMP/kept"
    echo "would vacuum the journal to $JOURNAL_KEEP (journalctl --vacuum-size=$JOURNAL_KEEP)"
}
render > "$TMP/plan"
HASH=$(sha256sum "$TMP/plan" | cut -c1-64)
TOTAL=$(awk '{ s += $2 } END { print s + 0 }' "$TMP/cands")
if [ "${#CANDS[@]}" -eq 0 ]; then
    say "no $OPT/boss-binbak-* or $OPT/boss-dev-bak on this host that is not a checkout — no backup directory to remove"
fi
KEPT_NOTE="${#KEPT[@]} checkout kept (${KEPT[*]:-none}; its unpushed state is in the plan); ${#PROVEN[@]} checkout proven to hold nothing unpushed and removed with the backups (${PROVEN[*]:-none})"
command -v journalctl >/dev/null 2>&1 \
    || refuse "journalctl is not on PATH, so the journal bound cannot be applied"
say "journal: $(journalctl --disk-usage 2>&1)"

if [ "$DRY" = 1 ]; then
    cat "$TMP/plan"
    say "DRY RUN — every bound passed: ${#CANDS[@]} backup directories (${TOTAL} MiB), $CAP_N second-stack capture(s) ($CAP_BYTES bytes), $DUMP_N cluster-pg dump(s) ($DUMP_BYTES bytes) and the journal beyond $JOURNAL_KEEP would go; $KEPT_NOTE. Nothing was removed."
    echo "plan-sha256: $HASH" >&2
    exit 0
fi

# --- the write: only the plan that was signed ------------------------------
if [ "$HASH" != "$APPROVED" ]; then
    cat "$TMP/plan" >&2
    refuse "today's plan (above) hashes to $HASH, not the approved $APPROVED — a backup appeared, went or changed since the plan was signed (or this plan was already applied). Render and approve it again"
fi
# The capture before the removal: the approved plan, on stdout, first.
cat "$TMP/plan"
echo "$ME: plan $APPROVED still holds"

# Every path removed, in order — backup directories and captures alike —
# so a run that stops part-way leaves an exact record.
DONE=()
DIRS_DONE=0
CAP_DONE=0
DUMP_DONE=0
# stop <exit> <what stopped it> — the record, then the exit.
stop() {
    say "$2"
    say "  already removed (${#DONE[@]}): ${DONE[*]:-none}"
    say "  not removed: $((CAP_N - CAP_DONE)) second-stack capture(s) and $((DUMP_N - DUMP_DONE)) cluster-pg dump(s) (the data goes last, the dumps after the capture); the journal was not vacuumed."
    exit "$1"
}
while read -r c mib; do
    # The internal guard: no path through this loop removes anything but
    # a real /opt/boss-binbak-* or /opt/boss-dev-bak directory — even if
    # a future edit widens the plan, this refuses.
    name="${c##*/}"
    if [ "${c%/*}" != "$OPT_REAL" ] || [ -L "$c" ] || ! [ -d "$c" ] \
        || { [ "$name" != "boss-dev-bak" ] && ! [[ "$name" =~ $NAME_RE ]]; }; then
        stop 2 "REFUSED — \`$c\` is not a backup directory this verb may remove; the loop was handed a path it must not touch."
    fi
    if [ -e "$TMP/proof.$name" ]; then
        # A PROVEN checkout (bound 3): re-prove every part at the last
        # moment. The re-render above matched the signed hash, so the
        # proof file holds exactly the bytes the passkey signed; the
        # checkout must read the same now — the same checkouts, each
        # read, every count 0 — and still be old by ctime. Any change
        # refuses the WHOLE run: it goes first, so nothing is removed.
        [ "$name" = "$CHECKOUT_NAME" ] \
            || stop 2 "REFUSED — \`$c\` carries a checkout proof, and only $CHECKOUT_NAME's checkout may be removed; the loop was handed a path it must not touch."
        checkouts_evidence "$c" > "$TMP/reproof"
        rc=$?
        if [ "$rc" != 0 ] || [ "$CHECKOUTS_CLEAN" != 1 ] || ! cmp -s -- "$TMP/proof.$name" "$TMP/reproof"; then
            { echo "  the signed proof read:"; sed 's/^/  /' "$TMP/proof.$name"
              echo "  it reads now (search exit $rc):"; sed 's/^/  /' "$TMP/reproof"
              [ "$rc" = 2 ] && sed 's/^/    /' "$TMP/find.err"; } >&2
            stop 78 "REFUSED — the checkout \`$c\` changed since the signed plan proved it holds nothing unpushed (both readings above); the whole run is refused."
        fi
        if ! young=$(find "$c" -xdev -newerct "@$cutoff" -print -quit 2> "$TMP/find.err"); then
            stop 78 "REFUSED — could not re-read the ages under the checkout \`$c\` just before removing it ($(head -c 300 "$TMP/find.err" | ascii)); a bound that cannot be evaluated is not passed."
        fi
        [ -z "$young" ] \
            || stop 78 "REFUSED — the checkout \`$c\` changed since the signed plan: \`$young\` changed (ctime) in the last $MIN_AGE_DAYS days; the whole run is refused."
        echo "re-proved $c holds nothing unpushed (the proof the plan signed, read again just before its rm)"
    else
        # Bound 3 again, at the last moment: the re-render above keeps
        # any candidate holding a .git it did not prove, but a checkout
        # could appear between that render and this rm. This verb never
        # removes a checkout it did not prove.
        if ! late_git=$(find "$c" -xdev -name .git -print -quit 2> "$TMP/find.err"); then
            stop 2 "REFUSED — could not search \`$c\` for a .git just before removing it ($(head -c 300 "$TMP/find.err" | ascii)); a bound that cannot be evaluated is not passed."
        fi
        if [ -n "$late_git" ]; then
            stop 2 "REFUSED — \`$c\` holds \`$late_git\` now, a checkout that appeared after the plan was rendered; this verb never removes a checkout its plan did not prove."
        fi
    fi
    if ! rm -rf --one-file-system -- "$c" 2> "$TMP/rm.err" || [ -e "$c" ]; then
        sed 's/^/    /' "$TMP/rm.err" >&2
        stop 1 "FAILED at \`$c\` — rm did not remove it."
    fi
    DONE+=("$c")
    DIRS_DONE=$((DIRS_DONE + 1))
    echo "removed $c ($mib MiB)"
done < "$TMP/cands"

# The capture, after every directory (bound 7). Its own loop, with its own internal guard
# — the directory loop's guard refuses anything outside /opt, correctly —
# and every bound again at the moment before rm: a regular file, not a
# link, named as retire-second-stack names it, directly in the capture
# directory, held open by no process, and holding the bytes the signed
# plan named. `rm -f`, never -r: a capture is one file.
while read -r bytes sha f; do
    name="${f##*/}"
    if [ "${f%/*}" != "$CAPTURE_DIR" ] || ! [[ "$name" =~ $CAPTURE_RE ]] \
        || [ -L "$CAPTURE_DIR" ] || [ -L "$f" ] || ! [ -f "$f" ]; then
        stop 2 "REFUSED — \`$f\` is not a second-stack capture this verb may remove (a regular file named second-stack-<stamp>.sql directly in $CAPTURE_DIR); it changed since the render, or the loop was handed a path it must not touch."
    fi
    links=$(capture_links "$f") \
        || stop 2 "REFUSED — could not count the links to \`$f\` just before removing it ($(head -c 300 "$TMP/links.err")); a bound that cannot be evaluated is not passed."
    [ "$links" = 1 ] \
        || stop 2 "REFUSED — \`$f\` has $links links now: another name holds the same bytes, so removing this one would free nothing the record could claim."
    real=$(realpath -e -- "$f" 2>/dev/null) \
        || stop 2 "REFUSED — \`$f\` does not resolve now; a bound that cannot be evaluated is not passed."
    [ "$real" = "$f" ] \
        || stop 2 "REFUSED — \`$f\` resolves to $real now, not itself; it is not the file the plan named."
    holder=$(capture_holder "$f")
    case $? in
        0) stop 2 "REFUSED — process $holder holds \`$f\` open now; a capture in use is not removed." ;;
        1) ;;
        *) stop 2 "REFUSED — whether a process holds \`$f\` open cannot be judged now ($(head -c 300 "$TMP/holder.err")); a bound that cannot be evaluated is not passed." ;;
    esac
    if ! sha256sum -- "$f" > "$TMP/sha" 2> "$TMP/sha.err"; then
        stop 2 "REFUSED — could not re-hash \`$f\` just before removing it ($(head -c 300 "$TMP/sha.err")); a bound that cannot be evaluated is not passed."
    fi
    now_sha=$(cut -c1-64 "$TMP/sha")
    [ "$now_sha" = "$sha" ] \
        || stop 2 "REFUSED — \`$f\` hashes to $now_sha now, not the planned $sha: its bytes changed after the plan was rendered, and the signature does not cover them."
    if ! rm -f -- "$f" 2> "$TMP/rm.err" || [ -e "$f" ] || [ -L "$f" ]; then
        sed 's/^/    /' "$TMP/rm.err" >&2
        stop 1 "FAILED at \`$f\` — rm did not remove it."
    fi
    DONE+=("$f")
    CAP_DONE=$((CAP_DONE + 1))
    echo "removed $f ($bytes bytes, sha256 $sha)"
done < "$TMP/captures"

# The cluster-pg dumps, LAST (bound 8). Their own loop and their own
# internal guard, every bound again at the moment before rm, and the
# identity the signed plan named read once more: a dump written, renamed
# onto or replaced since the render is not removed. `rm -f`, never -r.
while IFS=$'\t' read -r f ident; do
    name="${f##*/}"
    if [ "${f%/*}" != "$DUMP_DIR" ] || ! [[ "$name" =~ $DUMP_RE ]] \
        || [ -L "$DUMP_DIR" ] || [ -L "$f" ] || ! [ -f "$f" ]; then
        stop 2 "REFUSED — \`$f\` is not a cluster-pg dump this verb may remove (a regular file named boss-<stamp>.sql.gz directly in $DUMP_DIR); it changed since the render, or the loop was handed a path it must not touch."
    fi
    links=$(capture_links "$f") \
        || stop 2 "REFUSED — could not count the links to \`$f\` just before removing it ($(head -c 300 "$TMP/links.err")); a bound that cannot be evaluated is not passed."
    [ "$links" = 1 ] \
        || stop 2 "REFUSED — \`$f\` has $links links now: another name holds the same bytes, so removing this one would free nothing the record could claim."
    real=$(realpath -e -- "$f" 2>/dev/null) \
        || stop 2 "REFUSED — \`$f\` does not resolve now; a bound that cannot be evaluated is not passed."
    [ "$real" = "$f" ] \
        || stop 2 "REFUSED — \`$f\` resolves to $real now, not itself; it is not the file the plan named."
    holder=$(capture_holder "$f")
    case $? in
        0) stop 2 "REFUSED — process $holder holds \`$f\` open now; a dump in use is not removed." ;;
        1) ;;
        *) stop 2 "REFUSED — whether a process holds \`$f\` open cannot be judged now ($(head -c 300 "$TMP/holder.err")); a bound that cannot be evaluated is not passed." ;;
    esac
    if ! now_ident=$(dump_identity "$f"); then
        stop 2 "REFUSED — could not read the identity of \`$f\` just before removing it ($(head -c 300 "$TMP/stat.err")); a bound that cannot be evaluated is not passed."
    fi
    [ "$now_ident" = "$ident" ] \
        || stop 2 "REFUSED — \`$f\` reads ($now_ident) now, not the planned ($ident): it changed after the plan was rendered, and the signature does not cover it."
    if ! rm -f -- "$f" 2> "$TMP/rm.err" || [ -e "$f" ] || [ -L "$f" ]; then
        sed 's/^/    /' "$TMP/rm.err" >&2
        stop 1 "FAILED at \`$f\` — rm did not remove it."
    fi
    DONE+=("$f")
    DUMP_DONE=$((DUMP_DONE + 1))
    echo "removed $f ($ident)"
done < "$TMP/dumps"

if ! journalctl --vacuum-size="$JOURNAL_KEEP" > "$TMP/vacuum" 2>&1; then
    sed 's/^/    /' "$TMP/vacuum" >&2
    say "FAILED — journalctl --vacuum-size=$JOURNAL_KEEP exited non-zero; the $DIRS_DONE backup directories, $CAP_DONE second-stack capture(s) and $DUMP_DONE cluster-pg dump(s) above are removed."
    exit 1
fi
sed 's/^/  /' "$TMP/vacuum"
echo "vacuumed the journal to $JOURNAL_KEEP: $(journalctl --disk-usage 2>&1)"
FREE_AFTER=$(free_mib)
say "OK — removed $DIRS_DONE backup directories (${TOTAL} MiB) and $CAP_DONE second-stack capture(s) ($CAP_BYTES bytes), $DUMP_DONE cluster-pg dump(s) ($DUMP_BYTES bytes), and vacuumed the journal to $JOURNAL_KEEP; root free ${FREE_BEFORE:-unknown} → ${FREE_AFTER:-unknown} MiB; $KEPT_NOTE. Everything else under /var/backups, homes, /usr/local, $OPT/boss and $OPT/boss-cli are untouched. The journal grows back without a SystemMaxUse bound; the lasting gain is the backups, the capture and the dumps."
exit 0
