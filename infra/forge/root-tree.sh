#!/usr/bin/env bash
# root-tree.sh refresh | path | head | mark-good <dir> | status — the
# tree ROOT EXECUTES FROM on the forge host: root's own, fetched by root
# from the forge's own repository on local disk, and changed only by a
# whole, verified generation.
#
# WHY THIS EXISTS (backlog a604a35b; decided by David on design-doc
# c98c79aa, question root-tree, 2026-10-06, and as position B on the
# packet, 2026-10-07). Root on the forge ran its converge, its backup,
# its reaper, its observers and its watchdog out of /home/david/boss — a
# checkout its owner can write. Review 9a1e289b measured what that is:
# any shell as that owner edits what root runs within ten minutes, and
# root here holds the admin kubeconfig, the forge's tokens and the
# estate machine token. A `checkout -f` inside the converge cannot close
# it: the owner rewrites a file between the checkout and its execution.
# So root stops executing that checkout at all, and executes this.
#
# THE LAYOUT, all root:root under $BOSS_ROOT_TREE (/var/lib/boss/tree):
#   repo.git/        root's own bare repository — what the fetch fills;
#                    its one ref, refs/boss/accepted, names the commit
#                    `current` names, so the next fetch is incremental
#   gen/<sha>/       one exported tree per commit, complete before it is
#                    named, never written again; `.boss-tree-sha` inside
#                    says which commit it is
#   current          -> gen/<sha>, the newest verified generation; every
#                    unit's ExecStart goes through this one name
#   previous         -> gen/<sha>, what `current` was before the last move
#   good             -> gen/<sha>, the newest generation whose OWN
#                    converge script completed a refresh — proof that
#                    running it can still bring in the next fix
#                    (forge-converge-launch.sh reads it)
#
# THE SOURCE IS THE FORGE'S REPOSITORY, NOT THE CHECKOUT. forge-repo-
# path.sh derives where Forgejo keeps it (the one definition offsite-
# push.sh and the publish verb share). The checkout is not in the path,
# and no credential is: it is a local path. Only refs/heads/main, which
# the forge protects (protect-main.sh) — a PR merge is the one way it
# moves.
#
# THE FETCH IS THE PROBE VIEW'S, SPELLED A SECOND TIME AND HELD EQUAL.
# infra/forge/probe-account.sh refresh_view already fetches into a
# root-owned repository from one another account owns: the SERVING side
# runs as the source's owner (`--upload-pack '<runas> git upload-pack'`),
# so root's git never opens a repository another account owns — neither
# git's "dubious ownership" refusal nor that repository's config and
# hooks are in play — and what root parses is a pack, checked
# (`fetch.fsckObjects`). That file's installed copy must stand alone in
# its directory and source nothing (its own test holds it to that), so
# the fetch cannot be collapsed into a shared library; the two command
# lines are held equal by root_tree_sh.rs instead (CLAUDE.md §9a: pin
# what cannot be collapsed). The view itself cannot BE this tree: it
# follows the checkout's HEAD, which the checkout's owner sets, and it
# moves every minute under whatever is reading it.
#
# WHAT `refresh` ACCEPTS. A commit that (1) the forge's own main names,
# read by the serving side; (2) arrived in a pack every object of which
# checked; (3) is the commit `current` already names or a DESCENDANT of
# it — a fast-forward. A main that rewound (the 2026-09-25 shape) or was
# replaced is REFUSED, exit 3, and `current` stays: a person reads both
# and, if the rewind is the truth, runs `refresh --accept-rewind <sha>`
# as root by hand. (4) exports whole, and carries the converge and the
# installer.
#
# NEVER AN EMPTY TREE, NEVER A HALF TREE. A generation is exported into
# a staging directory, checked, renamed to its name, and only then does
# `current` move — one rename(2) of a symlink. A fetch that fails, a
# commit that does not verify, a full disk, a kill at any line: `current`
# names what it named before, whole, and the run says so on one line.
# Generations other than current, previous and good are removed; those
# three never are.
#
# Exit: 0 current is the forge's main (moved or already there);
#       1 FAILED — current is what it was; 3 REFUSED — not a
#       fast-forward; 2 usage.
#
# ENV (seams; the host sets none): BOSS_ROOT_TREE, BOSS_ROOT_TREE_SOURCE
# (the repository fetched from; default forge-repo-path.sh's FORGE_REPO),
# BOSS_ROOT_TREE_REF, ROOT_TREE_SOURCE_RUNAS (the words that run a
# command as the source's owner; set-but-empty means this process is the
# owner), ROOT_TREE_FETCH_TIMEOUT.
# Pinned by crates/core/boss-testing/tests/root_tree_sh.rs.
set -uo pipefail
export LC_ALL=C

ME="root-tree"
MODE="${1:-}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TREE="${BOSS_ROOT_TREE:-/var/lib/boss/tree}"
STORE="$TREE/repo.git"
GEN="$TREE/gen"
REF="${BOSS_ROOT_TREE_REF:-refs/heads/main}"
# What a generation must carry to be one: the converge that would run
# from it and the installer that converge runs. A tree without them is
# not this repository's. NOT this file, on purpose: a commit from before
# this car carries both and no root-tree.sh, and a REVERT of the car is
# such a commit. Accepted as a generation, its converge and installer —
# the old ones — run on the next tick and put the old units back, so a
# merged revert rolls this host back by itself; refused, the tree would
# hold the host on the very commit being reverted.
NEEDS=(infra/forge/forge-converge.sh infra/forge/install.sh)

say() { echo "$ME: $*"; }
case "$TREE" in /*) ;; *) echo "$ME: the tree '$TREE' is not an absolute path" >&2; exit 2 ;; esac

# WHICH MACHINE THIS IS, asked by the two verbs that write (backlog
# 62b09c57, N7; infra/lib/host-check.sh carries the incident and the
# rule). Pointed at a tree of its caller's choosing this is a test and is
# asked nothing; at the host's own /var/lib/boss/tree, this machine must
# hold the address the estate declares for the forge, or the verb exits
# 78 before it makes a directory. The reads — path, head, status — answer
# on any machine: they write nothing, and `status` is how an operator
# asks. A library that cannot be read is a refusal, never a pass.
on_the_forge() {
    # shellcheck source=infra/lib/host-check.sh
    . "$HERE/../lib/host-check.sh" 2>/dev/null \
        || { echo "$ME: REFUSED — $HERE/../lib/host-check.sh cannot be read, so which machine this is cannot be established; nothing was written" >&2; exit 78; }
    host_seam BOSS_ROOT_TREE /var/lib/boss/tree
    host_check forge "$ME"
}

# Root's own git, in root's own repository: no account's config, no hook.
store_git() {
    GIT_CONFIG_GLOBAL=/dev/null git -c core.hooksPath=/dev/null --git-dir="$STORE" "$@"
}

# The commit a link under $TREE names, or nothing: the link must point
# at gen/<hex> and that directory must say it is that commit.
link_sha() { # <link name>
    local to sha
    [ -L "$TREE/$1" ] || return 1
    to="$(readlink -- "$TREE/$1")" || return 1
    sha="${to#gen/}"
    [[ "$to" == gen/* && "$sha" =~ ^[0-9a-f]{40,64}$ ]] || return 1
    [ -d "$GEN/$sha" ] && [ "$(cat -- "$GEN/$sha/.boss-tree-sha" 2>/dev/null)" = "$sha" ] || return 1
    printf '%s' "$sha"
}

# Point a link at a generation in one rename: a reader sees the old
# target or the new one, never neither.
point() { # <link name> <sha>
    local tmp="$TREE/.$1.new.$$"
    rm -f -- "$tmp"
    ln -s -- "gen/$2" "$tmp" && mv -T -- "$tmp" "$TREE/$1" || { rm -f -- "$tmp"; return 1; }
}

# The words that run a command as the source repository's owner. Empty
# when this process IS the owner. By NAME through runuser — the form the
# probe view's fetch has run on this host since 2026-10-06 — when the
# uid has one; by number through setpriv when it does not (a container's
# uid with no host account).
source_runas() { # <source>
    if [ -n "${ROOT_TREE_SOURCE_RUNAS+set}" ]; then
        printf '%s' "$ROOT_TREE_SOURCE_RUNAS"
        return 0
    fi
    local uid gid name
    uid="$(stat -c %u -- "$1" 2>/dev/null)" || return 1
    gid="$(stat -c %g -- "$1" 2>/dev/null)" || return 1
    [[ "$uid" =~ ^[0-9]+$ && "$gid" =~ ^[0-9]+$ ]] || return 1
    if [ "$uid" = "$(id -u)" ]; then
        printf ''
        return 0
    fi
    name="$(getent passwd "$uid" 2>/dev/null | cut -d: -f1)"
    if [[ "$name" =~ ^[a-z_][a-z0-9_-]*$ ]]; then
        printf 'runuser -u %s --' "$name"
    else
        printf 'setpriv --reuid=%s --regid=%s --clear-groups --' "$uid" "$gid"
    fi
}

source_repo() {
    if [ -n "${BOSS_ROOT_TREE_SOURCE:-}" ]; then
        printf '%s' "$BOSS_ROOT_TREE_SOURCE"
        return 0
    fi
    # shellcheck source=infra/forge/forge-repo-path.sh
    . "$HERE/forge-repo-path.sh" || return 1
    printf '%s' "$FORGE_REPO"
}

# ── refresh ──────────────────────────────────────────────────────────
refresh() {
    local accept="" src runas have got err owner_uid stage d keep sha
    if [ "${1:-}" = "--accept-rewind" ]; then
        accept="${2:-}"
        [[ "$accept" =~ ^[0-9a-f]{40,64}$ ]] || { echo "$ME: --accept-rewind takes the full commit to accept" >&2; return 2; }
    elif [ -n "${1:-}" ]; then
        echo "usage: root-tree.sh refresh [--accept-rewind <full sha>]" >&2
        return 2
    fi
    on_the_forge
    umask 022
    have="$(link_sha current || echo none)"
    keeping() { say "$1 — current stays at ${have:0:12}"; }

    if [ -L "$TREE" ]; then keeping "REFUSED: $TREE is a symlink"; return 1; fi
    # Made when absent, and never re-moded when there: a directory that
    # already stands is JUDGED below, not repaired into passing.
    for d in "$TREE" "$GEN"; do
        [ -d "$d" ] || err="$(install -d -m 0755 -- "$d" 2>&1)" || { keeping "FAILED: could not make $d: $err"; return 1; }
    done
    # The tree is this process's own or it is not the tree: a directory
    # another account owns is one that account can rewrite under root.
    for d in "$TREE" "$GEN"; do
        owner_uid="$(stat -c %u -- "$d" 2>/dev/null)"
        [ "$owner_uid" = "$(id -u)" ] && [ ! -L "$d" ] \
            || { keeping "REFUSED: $d is owned by uid ${owner_uid:-unknown}, not this uid ($(id -u)), or is a symlink"; return 1; }
        [ -z "$(find "$d" -maxdepth 0 -perm /0022 2>/dev/null)" ] \
            || { keeping "REFUSED: $d is writable by its group or by anyone ($(stat -c %A -- "$d"))"; return 1; }
    done
    # One refresh at a time: the converge and a hand run must not stage
    # and prune beside each other.
    #
    # THE LOCK IS ROOT'S ALONE, 0600 (review 5f3736a2, F1). flock(2) asks
    # only for an open descriptor, and this file was made under umask 022:
    # 0644 in a 0755 directory, so ANY account could open it read-only and
    # hold it. Root's refresh then waited its two minutes and failed, the
    # tree never moved, and held across the first ticks after landing no
    # generation was marked good — the launcher kept offering the
    # checkout's copy. Made under a closed umask and re-closed on every
    # run (a lock an older copy of this file left 0644 is closed too); the
    # directory it stands in was judged above — this uid's, not a symlink,
    # writable by nobody else — so no other account can put a name here.
    #
    # JUDGED BEFORE IT IS OPENED (review aa901496, N4). The lines below
    # used to open and chmod the name and only then ask what it was: a
    # symlink standing there had its TARGET made 0600 before the refusal,
    # and a FIFO held the open — and so the refresh, and the converge
    # behind it — for as long as nothing wrote to it. Only root can put
    # either there, so this is order and not exposure; a name that exists
    # and is not a regular file of its own is refused untouched, and the
    # open that makes a missing lock is bounded all the same.
    if [ -L "$TREE/.lock" ] || { [ -e "$TREE/.lock" ] && [ ! -f "$TREE/.lock" ]; }; then
        keeping "REFUSED: $TREE/.lock is not a regular file of its own"
        return 1
    fi
    (umask 077 && timeout -k 2 10 sh -c ': >>"$1"' sh "$TREE/.lock") && chmod 0600 -- "$TREE/.lock" \
        || { keeping "FAILED: could not make $TREE/.lock"; return 1; }
    if [ -L "$TREE/.lock" ] || [ ! -f "$TREE/.lock" ]; then keeping "REFUSED: $TREE/.lock is not a regular file of its own"; return 1; fi
    exec 9>>"$TREE/.lock" || { keeping "FAILED: could not open $TREE/.lock"; return 1; }
    flock -w "${ROOT_TREE_LOCK_WAIT:-120}" 9 \
        || { keeping "FAILED: $TREE/.lock was not free within ${ROOT_TREE_LOCK_WAIT:-120} s — another process of this uid holds it (a refresh still running, or one that hangs)"; return 1; }
    have="$(link_sha current || echo none)"

    src="$(source_repo)" || { keeping "FAILED: could not derive where the forge's repository is"; return 1; }
    [ -d "$src" ] || { keeping "FAILED: the forge's repository is not at $src"; return 1; }
    runas="$(source_runas "$src")" || { keeping "FAILED: cannot read who owns $src"; return 1; }
    if [ ! -d "$STORE" ]; then
        err="$(GIT_CONFIG_GLOBAL=/dev/null git init -q --bare -- "$STORE" 2>&1)" \
            || { keeping "FAILED: could not make $STORE: $err"; return 1; }
    fi

    # The serving side runs as the source's owner; the path is quoted
    # by git when it builds the command line.
    err="$(timeout -k 5 "${ROOT_TREE_FETCH_TIMEOUT:-300}" env GIT_CONFIG_GLOBAL=/dev/null \
        git -c core.hooksPath=/dev/null -c fetch.fsckObjects=true --git-dir="$STORE" \
        fetch -q --no-tags --upload-pack="${runas:+$runas }git upload-pack" -- "$src" "$REF" 2>&1)" \
        || { keeping "FAILED: fetching $REF from $src as its owner: $(printf '%s' "$err" | tr '\n' ' ' | cut -c1-300)"; return 1; }
    got="$(store_git rev-parse --verify -q 'FETCH_HEAD^{commit}' 2>/dev/null)" \
        || { keeping "FAILED: the fetch left no FETCH_HEAD"; return 1; }
    [[ "$got" =~ ^[0-9a-f]{40,64}$ ]] || { keeping "FAILED: FETCH_HEAD is not a commit"; return 1; }

    if [ "$have" = "$got" ]; then
        # A store an older copy of this file filled has no ref yet.
        store_git update-ref refs/boss/accepted "$got" 2>/dev/null || true
        say "current is at ${got:0:12} already (the forge's $REF)"
        return 0
    fi
    # A FAST-FORWARD FROM WHAT ROOT LAST ACCEPTED, or nothing. `have` is
    # in the store (it was fetched to become current); a store that
    # cannot answer is a refusal, never a pass.
    if [ "$have" != "none" ]; then
        if ! store_git merge-base --is-ancestor "$have" "$got" 2>/dev/null; then
            if [ "$accept" != "$got" ]; then
                keeping "REFUSED: the forge's $REF is at ${got:0:12}, which is NOT a descendant of the commit root last accepted — main rewound or was replaced; read both, and if ${got:0:12} is the truth run \`root-tree.sh refresh --accept-rewind $got\` as root"
                return 3
            fi
            say "ACCEPTED A REWIND BY HAND: ${have:0:12} -> ${got:0:12} is not a fast-forward, and --accept-rewind named it"
        fi
    fi

    # Export whole, into a name nothing reads, then rename.
    stage="$GEN/.staging.$$"
    rm -rf -- "$stage" "$stage.idx"
    err="$(install -d -m 0755 -- "$stage" 2>&1 \
        && GIT_INDEX_FILE="$stage.idx" store_git --work-tree="$stage" read-tree "$got" 2>&1 \
        && GIT_INDEX_FILE="$stage.idx" store_git --work-tree="$stage" checkout-index -a -f 2>&1)" \
        || { rm -rf -- "$stage" "$stage.idx"; keeping "FAILED: exporting ${got:0:12}: $(printf '%s' "$err" | tr '\n' ' ' | cut -c1-300)"; return 1; }
    rm -f -- "$stage.idx"
    for d in "${NEEDS[@]}"; do
        [ -f "$stage/$d" ] && [ -x "$stage/$d" ] \
            || { rm -rf -- "$stage"; keeping "REFUSED: ${got:0:12} carries no executable $d — not a tree root can run from"; return 1; }
    done
    printf '%s\n' "$got" >"$stage/.boss-tree-sha" || { rm -rf -- "$stage"; keeping "FAILED: could not stamp the staged tree"; return 1; }
    # A directory already at this name is a past run's that never became
    # current (current != got here): replace it rather than trust it.
    if [ -e "$GEN/$got" ]; then
        [ "$(link_sha good || true)" != "$got" ] && [ "$(link_sha previous || true)" != "$got" ] \
            && rm -rf -- "$GEN/$got"
    fi
    if [ -e "$GEN/$got" ]; then
        rm -rf -- "$stage"
    else
        mv -T -- "$stage" "$GEN/$got" || { rm -rf -- "$stage"; keeping "FAILED: could not name the generation $GEN/$got"; return 1; }
    fi
    [ "$(cat -- "$GEN/$got/.boss-tree-sha" 2>/dev/null)" = "$got" ] \
        || { keeping "FAILED: $GEN/$got does not say it is ${got:0:12}"; return 1; }

    if [ "$have" != "none" ]; then
        point previous "$have" || say "could not record previous -> ${have:0:12} (current still moves)"
    fi
    point current "$got" || { keeping "FAILED: could not move current to ${got:0:12}"; return 1; }
    # THE STORE REMEMBERS WHAT ROOT ACCEPTED (review 5f3736a2, F4). A
    # fetch offers the serving side what the local refs reach; with no
    # ref it offered nothing, and every move of main downloaded the whole
    # history again — four moves, four packs of 31 MB each, measured at
    # this repository's size, on a host whose disk is its recurring
    # problem. One ref, moved only AFTER `current` has: it names what was
    # accepted, never what was merely fetched, so a refused commit is
    # never offered as something root has. A ref that cannot be written
    # costs the next fetch its size and nothing else.
    store_git update-ref refs/boss/accepted "$got" 2>/dev/null \
        || say "could not record refs/boss/accepted -> ${got:0:12}; the next fetch will be a whole one"
    say "current moved ${have:0:12} -> ${got:0:12} (the forge's $REF at $src${runas:+, served as its owner by: $runas})"

    # Prune: every generation no link names. Read after the move.
    keep=" $(link_sha current || true) $(link_sha previous || true) $(link_sha good || true) "
    for d in "$GEN"/* "$GEN"/.staging.*; do
        [ -e "$d" ] || continue
        sha="$(basename -- "$d")"
        case "$keep" in *" $sha "*) continue ;; esac
        rm -rf -- "$d"
    done
    return 0
}

# ── path / head ──────────────────────────────────────────────────────
tree_path() {
    local sha
    sha="$(link_sha current)" || return 1
    printf '%s\n' "$GEN/$sha"
}

# ── mark-good ────────────────────────────────────────────────────────
# <dir>: the generation whose converge script just completed a refresh.
# Only a directory that IS a generation of this tree can be named.
mark_good() {
    local dir sha
    on_the_forge
    dir="$(cd -- "${1:-/nonexistent}" 2>/dev/null && pwd -P)" || { say "mark-good: '${1:-}' is not a directory"; return 1; }
    sha="$(basename -- "$dir")"
    if [ "$dir" != "$(cd -- "$GEN" 2>/dev/null && pwd -P)/$sha" ] || [ "$(cat -- "$dir/.boss-tree-sha" 2>/dev/null)" != "$sha" ]; then
        say "mark-good: $dir is not a generation of $TREE — nothing marked"
        return 1
    fi
    [ "$(link_sha good || true)" = "$sha" ] && return 0
    point good "$sha" || { say "mark-good FAILED: could not point good at ${sha:0:12}"; return 1; }
    say "good -> ${sha:0:12}"
}

# ── status ───────────────────────────────────────────────────────────
# Reads only. One fact a line, for a person or a probe.
status() {
    local n src uid rc=0
    for n in current previous good; do
        echo "$n: $(link_sha "$n" || echo none)"
    done
    link_sha current >/dev/null || rc=1
    echo "tree: $TREE $(stat -c '%A %U:%G' -- "$TREE" 2>/dev/null || echo absent)"
    echo "generations: $(find "$GEN" -mindepth 1 -maxdepth 1 -type d 2>/dev/null | wc -l)"
    if src="$(source_repo 2>/dev/null)" && [ -d "$src" ]; then
        uid="$(stat -c %u -- "$src" 2>/dev/null)"
        echo "source: $src owned by uid ${uid:-unknown} ($(getent passwd "${uid:-x}" 2>/dev/null | cut -d: -f1 || true))"
    else
        echo "source: not found (${src:-underivable})"
        rc=1
    fi
    # WHO DECIDES WHERE THE SOURCE IS (review 5f3736a2, first-hour notes).
    # The path above is derived from Forgejo's compose file: an account
    # that can write that file, or replace it in its directory, chooses
    # which repository root fetches. Printed, never judged here — read
    # each uid against the checkout owner's.
    if [ -z "${BOSS_ROOT_TREE_SOURCE:-}" ]; then
        local compose="${BOSS_FORGE_COMPOSE:-/opt/forgejo/docker-compose.yml}"
        echo "source derived from: $compose $(stat -c 'owned by uid %u, %A' -- "$compose" 2>/dev/null || echo '(not readable)'); its directory $(stat -c 'owned by uid %u, %A' -- "$(dirname -- "$compose")" 2>/dev/null || echo '(not readable)')"
    else
        echo "source derived from: BOSS_ROOT_TREE_SOURCE in the environment"
    fi
    echo "accepted ref: $(store_git rev-parse -q --verify refs/boss/accepted 2>/dev/null || echo none)"
    return "$rc"
}

case "$MODE" in
    refresh) shift; refresh "$@" ;;
    path) tree_path ;;
    head) link_sha current && echo ;;
    mark-good) mark_good "${2:-}" ;;
    status) status ;;
    *)
        echo "usage: root-tree.sh refresh [--accept-rewind <sha>] | path | head | mark-good <dir> | status" >&2
        exit 2
        ;;
esac
