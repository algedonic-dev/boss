#!/usr/bin/env bash
#
# reclaim-gcp-root — free boss-gcp's 48 GB root by removing exactly two
# kinds of thing, and nothing else, ever: the obsolete binary backup
# directories /opt/boss-binbak-* and /opt/boss-dev-bak, and the systemd
# journal beyond a fixed 1G — through a rendered plan a passkey signs.
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
# /var/backups/* (the cluster-pg dumps and the second-stack capture),
# every home directory, /usr/local, and the live /opt/boss and
# /opt/boss-cli.
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
#      untracked files and stashes — measured against the checkout's OWN
#      remote-tracking refs as of its last fetch (this verb never
#      fetches), read with plumbing that runs nothing the checkout's
#      config names, as its owner (see checkout_git). A state that
#      cannot be read says so in the plan; the checkout is kept either
#      way. The write checks each candidate for a .git once more just
#      before its rm. This verb removes no checkout: that needs a
#      separate plan that proved nothing unpushed, and is not built.
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
#
# THE PLAN'S BYTES ARE DETERMINISTIC: each candidate's path, its size
# in MiB and its top-level entries, sorted, then each kept checkout with
# its evidence, then the fixed journal line.
# No clock, no free space, no journal usage — those move on their own
# and ride stderr, where the hash does not reach.
#
# WHAT IT DOES NOT SEE. It reads units, unit files, mounts and
# processes, not every script on the host: a cron line or a shell script
# naming a backup directory by path would not stop it.
#
# EXIT
#   0  done (or, with --dry-run, every bound passed and this is the plan)
#   2  refused — the reason names the path or the bound; nothing removed
#   1  failed part-way — the record states what was already removed and
#      what was not touched
#
# ENV (test seams — the ops-runner passes no packet-supplied environment,
# only an argv built from the allowlist, so a packet cannot set these)
#   BOSS_RECLAIM_OPT_DIR     where the candidates and live trees live (/opt)
#   BOSS_RECLAIM_PROC_DIR    the process table (/proc)
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
JOURNAL_KEEP="1G"
MIN_AGE_DAYS=30
NAME_RE='^boss-binbak-[A-Za-z0-9._-]+$'

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
    local staged untracked stashes shown more
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

    # Branches with commits on no remote.
    checkout_git "$r" for-each-ref --format='%(refname)' refs/heads > "$TMP/heads" 2> "$TMP/git.err" || return 1
    : > "$TMP/unpushed"
    while IFS= read -r ref; do
        n=$(checkout_git "$r" rev-list --count "$ref" --not --remotes 2> "$TMP/git.err") || return 1
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

    head_n=$(checkout_git "$r" rev-list --count HEAD --not --branches --remotes 2> "$TMP/git.err") || return 1
    [[ "$head_n" =~ ^[0-9]+$ ]] || { echo "rev-list counted '$head_n' for HEAD" > "$TMP/git.err"; return 1; }

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

    echo "    branches with commits on no remote: $heads"
    ascii < "$TMP/unpushed.shown"
    [ "$more" -le 0 ] || echo "      ...and $more more"
    echo "    commits at HEAD on no branch and no remote: $head_n"
    echo "    staged changes not committed: $staged"
    echo "    tracked files whose bytes differ from the index (read without filters): $changed"
    echo "    untracked files (ignored ones not counted): $untracked"
    echo "    stashes: $stashes"
    echo "    not measured: tags, reflogs, ignored files and submodules; a branch squash-merged upstream still counts as unpushed. This plan removes no checkout."
}
FREE_BEFORE=$(free_mib)
say "root free before: ${FREE_BEFORE:-unknown} MiB (df $OPT_REAL)"

# --- bound 2, 3 + 4: the names, not a checkout, the age --------------------
CANDS=()
KEPT=()
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
    find "$real" -xdev -name .git -prune -print > "$TMP/gits.raw" 2> "$TMP/find.err" \
        || refuse "could not search \`$real\` for a .git ($(head -c 300 "$TMP/find.err")) — a bound that cannot be evaluated is not passed"
    if [ -s "$TMP/gits.raw" ]; then
        # Kept, not refused: the rest of the plan stands (bound 3).
        LC_ALL=C sort "$TMP/gits.raw" > "$TMP/gits" \
            || refuse "could not sort the checkouts found under \`$real\`"
        {
            echo "would keep $real — it holds a git checkout, which may carry unpushed commits: that is work, not a backup, and this plan removes no checkout. Its unpushed state, against its own remote-tracking refs as of its last fetch:"
            while IFS= read -r g; do
                echo "  checkout $(printf '%s' "${g%/.git}" | ascii):"
                if checkout_state "${g%/.git}" > "$TMP/state"; then
                    cat "$TMP/state"
                else
                    echo "    its unpushed state could not be read: $(head -n 1 "$TMP/git.err" | ascii | cut -c1-300)"
                fi
            done < "$TMP/gits"
        } >> "$TMP/kept"
        KEPT+=("$real")
        say "keeping \`$real\` — it holds $(wc -l < "$TMP/gits") .git; its unpushed state is in the plan"
        continue
    fi
    young=$(find "$real" -xdev -newerct "@$cutoff" -print -quit 2> "$TMP/find.err") \
        || refuse "could not read the ages under \`$real\` ($(head -c 300 "$TMP/find.err")) — a bound that cannot be evaluated is not passed"
    [ -z "$young" ] \
        || refuse "\`$real\` holds \`$young\`, changed (ctime) in the last $MIN_AGE_DAYS days — a fresh backup, even one copied with its old mtimes, may be somebody's rollback target, and this verb removes only old ones"
    CANDS+=("$real")
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
    # (d) running processes.
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
    done < "$TMP/cands"
    cat "$TMP/kept"
    echo "would vacuum the journal to $JOURNAL_KEEP (journalctl --vacuum-size=$JOURNAL_KEEP)"
}
render > "$TMP/plan"
HASH=$(sha256sum "$TMP/plan" | cut -c1-64)
TOTAL=$(awk '{ s += $2 } END { print s + 0 }' "$TMP/cands")
if [ "${#CANDS[@]}" -eq 0 ]; then
    say "no $OPT/boss-binbak-* or $OPT/boss-dev-bak on this host that is not a checkout — no backup directory to remove"
fi
KEPT_NOTE="${#KEPT[@]} checkout kept (${KEPT[*]:-none}; its unpushed state is in the plan)"
command -v journalctl >/dev/null 2>&1 \
    || refuse "journalctl is not on PATH, so the journal bound cannot be applied"
say "journal: $(journalctl --disk-usage 2>&1)"

if [ "$DRY" = 1 ]; then
    cat "$TMP/plan"
    say "DRY RUN — every bound passed: ${#CANDS[@]} backup directories (${TOTAL} MiB) and the journal beyond $JOURNAL_KEEP would go; $KEPT_NOTE. Nothing was removed."
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

DONE=()
while read -r c mib; do
    # The internal guard: no path through this loop removes anything but
    # a real /opt/boss-binbak-* or /opt/boss-dev-bak directory — even if
    # a future edit widens the plan, this refuses.
    name="${c##*/}"
    if [ "${c%/*}" != "$OPT_REAL" ] || [ -L "$c" ] || ! [ -d "$c" ] \
        || { [ "$name" != "boss-dev-bak" ] && ! [[ "$name" =~ $NAME_RE ]]; }; then
        say "REFUSED — \`$c\` is not a backup directory this verb may remove; the loop was handed a path it must not touch."
        say "  already removed: ${DONE[*]:-none}"
        exit 2
    fi
    # Bound 3 again, at the last moment: the re-render above keeps any
    # candidate holding a .git, but a checkout could appear between that
    # render and this rm. This verb never removes a checkout.
    if ! late_git=$(find "$c" -xdev -name .git -print -quit 2> "$TMP/find.err"); then
        say "REFUSED — could not search \`$c\` for a .git just before removing it ($(head -c 300 "$TMP/find.err" | ascii)); a bound that cannot be evaluated is not passed."
        say "  already removed: ${DONE[*]:-none}"
        exit 2
    fi
    if [ -n "$late_git" ]; then
        say "REFUSED — \`$c\` holds \`$late_git\` now, a checkout that appeared after the plan was rendered; this verb never removes a checkout."
        say "  already removed: ${DONE[*]:-none}"
        exit 2
    fi
    if ! rm -rf --one-file-system -- "$c" 2> "$TMP/rm.err" || [ -e "$c" ]; then
        sed 's/^/    /' "$TMP/rm.err" >&2
        say "FAILED at \`$c\` — rm did not remove it."
        say "  already removed (${#DONE[@]}): ${DONE[*]:-none}"
        say "  the journal was not vacuumed."
        exit 1
    fi
    DONE+=("$c")
    echo "removed $c ($mib MiB)"
done < "$TMP/cands"

if ! journalctl --vacuum-size="$JOURNAL_KEEP" > "$TMP/vacuum" 2>&1; then
    sed 's/^/    /' "$TMP/vacuum" >&2
    say "FAILED — journalctl --vacuum-size=$JOURNAL_KEEP exited non-zero; the ${#DONE[@]} backup directories above are removed."
    exit 1
fi
sed 's/^/  /' "$TMP/vacuum"
echo "vacuumed the journal to $JOURNAL_KEEP: $(journalctl --disk-usage 2>&1)"
FREE_AFTER=$(free_mib)
say "OK — removed ${#DONE[@]} backup directories (${TOTAL} MiB) and vacuumed the journal to $JOURNAL_KEEP; root free ${FREE_BEFORE:-unknown} → ${FREE_AFTER:-unknown} MiB; $KEPT_NOTE. /var/backups, homes, /usr/local, $OPT/boss and $OPT/boss-cli are untouched. The journal grows back without a SystemMaxUse bound; the lasting gain is the backups."
exit 0
