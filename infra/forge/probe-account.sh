#!/usr/bin/env bash
# probe-account.sh ensure | verify-tick | reap | controls — the account a
# landed car's recorded probe runs as on the forge host, the read-only
# tree it reads, what is left of it after a runner tick, and the controls
# that say whether any of that is true on THIS host.
#
# WHY THIS EXISTS (backlog 703358ce; decided by David 2026-10-06 on
# design-doc bdc60b65, question probe-user). A recorded probe is shell
# text a car's builder wrote. Until this file it ran here as `david`
# (BOSS_PROBE_USER's default in boss-cli prove.rs), in david's own
# checkout. Review 9a1e289b measured what that is one step from: david
# reaches root on this host by a passwordless `sudo docker` the tree
# relies on (cluster-watchdog and disk-floor-sweep run User=david and
# call it) and by owning the checkout root's converge executes from
# (forge-converge.service runs /home/david/boss/infra/forge/
# forge-converge.sh as root). Root here holds the admin kubeconfig and
# the forge's tokens. So probe text, hostile or merely wrong, was one
# step from the estate, and a root-only token file isolated nothing from
# the one account that runs text nobody on this host wrote.
#
# WHAT THE ACCOUNT IS. `boss-probe`: a system account with a group of its
# own and no other, no password (locked), /usr/sbin/nologin for a shell,
# /nonexistent for a home, no sudoers rule, no cron and no at. It is not
# in `docker`, so neither daemon's socket opens for it. `runuser -u
# boss-probe -- bash -c …` — the form prove.rs uses — execs the command
# directly, so nologin stops a login and not a probe.
#
# THE FOUR MODES
#
#   ensure        ROOT, from infra/forge/install.sh on every forge
#                 converge. Installs a ROOT-OWNED COPY of this file
#                 outside the checkout, creates the account when absent,
#                 repairs the three things about it that drift
#                 (supplementary groups, shell, lock), denies it cron and
#                 at, bounds its process count, makes the view (below),
#                 then VERIFIES BY EFFECT and writes the drop-in that
#                 points the ops runner's probes at the account and at
#                 the marker that says it verified. Every fact lands on
#                 the converge packet. Exit 1 names what did not verify;
#                 install.sh carries the code, so no unit goes
#                 uninstalled for it.
#   verify-tick   ROOT, ExecStartPre=- of the ops runner (every tick, so
#                 before any probe that tick runs). Removes the marker,
#                 brings the view to the converged checkout's HEAD,
#                 verifies the account again, and writes the marker only
#                 if all of it held.
#   reap          ROOT, ExecStopPost=- of the ops runner: every process
#                 the account still has is stopped and killed, and every
#                 file it left in the shared temp directories is removed.
#   controls      ROOT, through the READ ops verb probe-account-controls:
#                 the negative and positive controls, one line each, and
#                 the host facts the tree only assumes (what `sudo -l`
#                 says for the checkout's owner). Exit 0 every control
#                 held; 1 one did not (named); 2 the controls could not
#                 be run at all — which is never a pass.
#
# THE ONE PLACE BOSS_PROBE_USER IS SET is the drop-in `ensure` writes,
# /etc/systemd/system/boss-ops-runner.service.d/probe-account.conf. The
# ops runner is the only thing on this host that runs `boss prove
# --unattended` (the run-car-probe verb, filed on train arrival and by
# the hourly recheck), and a verb inherits the runner unit's environment.
#
# STOP, NEVER FALL BACK (decided by David on design-doc c98c79aa,
# question `fallback`, 2026-10-06). Once this has converged on a host, a
# probe runs ONLY as the verified account. The first cut of this file
# withheld the drop-in until the account verified and so kept running
# probes as the checkout's owner on a host where it could not be made;
# that was the wrong way round and is gone. Now:
#
#   * the drop-in names the account AND a marker file in the runner's
#     own runtime directory (BOSS_PROBE_VERIFIED_FILE; tmpfs, 0700,
#     removed by systemd when the tick ends);
#   * `verify-tick` writes that marker ahead of a tick only when the
#     account verifies THEN — absent, drifted (a group, a sudo rule, a
#     shell), or with a view it cannot read or can write, there is none;
#   * the unattended door (boss-cli prove.rs, `probe_account_refusal`)
#     runs nothing without it: an environment refusal, before runuser,
#     recorded on the car as did-not-run with the cause and retried by
#     the hourly recheck. Never NOT PROVEN — `runuser -u <absent>` exits
#     1, which is what a probe that ran and failed exits, and that is
#     why the stop is in the door and not left to runuser.
#
# So the drop-in is written whether or not the account verified, and an
# unverified host stops proving, loudly: its converge is red every tick
# with `probe_account: STOPPED: <why>` on the packet. It is recoverable
# without a hand because the converge runs as root, not as the account:
# the next tick repairs what it can and the marker returns by itself.
# BEFORE the first converge that carries this, a host has no drop-in and
# is simply not converged onto this rule yet. The one delay after it is
# stated in `ensure`: a `boss` on the host too old to know the marker.
#
# THE VIEW (/var/lib/boss/probe-view). A root-owned clone with a working
# tree, which the account can read and cannot write, and which nothing
# privileged executes from: the installed copy of this file lives in
# /usr/local/libexec/boss and sources nothing. It is fetched from the
# converged checkout BY RUNNING THE SERVING SIDE AS THE CHECKOUT'S OWNER
# (`--upload-pack 'runuser -u <owner> -- git upload-pack'`), so root's
# git never opens a repository another account owns — neither "dubious
# ownership" (the refusal that bit the publish verb on 2026-09-19) nor
# that repository's config is in play — and what root parses is a pack,
# checked (`fetch.fsckObjects`), which is what any fetch parses.
#
# THE OTHER HALF OF THE OWNERSHIP RULE: the account reading a repository
# ROOT owns is the same refusal from the other side. `ensure` adds the
# view, and only the view, to `safe.directory` in the SYSTEM gitconfig.
# System and not the runner's environment, because a probe's git must
# find it whatever environment its door hands over; and safe to say,
# because the directory is root's — the config and hooks git would read
# there are root's own.
#
# WHAT IT DOES NOT DO, and the controls say so rather than hide it:
#   * It does not take david's `sudo docker` away, nor move root's
#     converge out of david's checkout. Those are the two roads FROM
#     david; this closes the road TO david from probe text. (The second
#     is infra/forge/root-tree.sh's, backlog a604a35b: root's converge,
#     its other units and the watchdog run from a tree root fetched; the
#     ops runner's verbs and the cluster converge do not yet.)
#   * Leftovers are reaped per runner TICK, not per probe. Two probes in
#     one tick share the account. Per-probe needs the door itself
#     (prove.rs) to run each probe in a scope of its own.
#   * The account can still reach every address the host can. A probe is
#     promised the system of record; what answers a tokenless caller on
#     the LAN is the machine gate's business (design b08725c2).
#   * The shared temp directories are writable by it, as by everyone.
#     `reap` removes what it left; a file named to be mistaken for
#     another account's is that other reader's mktemp to refuse. THREE
#     READERS DID NOT REFUSE (review acab446c F8a, backlog dda26693): the
#     alert spool, the door observer's spool and its dark-since state
#     were names under /var/tmp that the checkout owner's units replay,
#     and the account could make each first. They moved to that owner's
#     home (cluster-watchdog.service says why), and `controls` now tries
#     every spool the installed units name.
#
# ENV (seams; the host sets none): BOSS_PROBE_ACCOUNT, BOSS_PROBE_VIEW,
# BOSS_PROBE_VIEW_SOURCE (the converged checkout), PROBE_RUNAS (the words
# that run a command as the account; default `runuser -u <account> --`),
# PROBE_SOURCE_RUNAS (the same for the checkout's owner), INSTALL_ETC,
# INSTALL_PROBE_LIBEXEC, INSTALL_PROBE_CLI (the boss binary asked whether
# it knows the marker), INSTALL_PROBE_MARKER, BOSS_PROBE_VERIFIED_FILE
# (what verify-tick writes), INSTALL_PROBE_GITCONFIG (a file instead of
# --system), INSTALL_PROBE_ETC (where cron.deny, at.deny and
# security/limits.d live), PROBE_TMP_DIRS, PROBE_CONTROL_SECRETS,
# PROBE_CONTROL_NOWRITE, PROBE_CONTROL_SOCKETS, PROBE_FETCH_TIMEOUT.
# Pinned by crates/core/boss-testing/tests/probe_account_sh.rs.
set -uo pipefail
export LC_ALL=C

ME="probe-account"
MODE="${1:-}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ACCOUNT="${BOSS_PROBE_ACCOUNT:-boss-probe}"
VIEW="${BOSS_PROBE_VIEW:-/var/lib/boss/probe-view}"
SRC="${BOSS_PROBE_VIEW_SOURCE:-${BOSS_FORGE_REPO_DIR:-/home/david/boss}}"
LIBEXEC="${INSTALL_PROBE_LIBEXEC:-/usr/local/libexec/boss}"
ETC="${INSTALL_ETC:-/etc/systemd/system}"
HOST_ETC="${INSTALL_PROBE_ETC:-/etc}"
NOLOGIN="/usr/sbin/nologin"
NPROC_LIMIT=256
# Every call that crosses to another account, or to git, is bounded: this
# runs inside the converge and ahead of every ops-runner tick, and
# neither may wait on it (review 9a1e289b B1 is the same class).
BOUND=(timeout -k 5 20)

say() { echo "$ME: $*"; }
[[ "$ACCOUNT" =~ ^[a-z_][a-z0-9_-]{0,30}$ ]] || { echo "$ME: '$ACCOUNT' is not an account name" >&2; exit 2; }
[ "$ACCOUNT" != "root" ] || { echo "$ME: the probe account cannot be root" >&2; exit 2; }
case "$VIEW" in /*) ;; *) echo "$ME: the view '$VIEW' is not an absolute path" >&2; exit 2 ;; esac

# As the account. PROBE_RUNAS set-but-empty runs the command as this
# process — the test seam for a host with no second uid — which is why
# `reap` measures who it would be before it signals anything.
as_account() {
    if [ -n "${PROBE_RUNAS+set}" ]; then
        # shellcheck disable=SC2086 # a word list, split on purpose
        "${BOUND[@]}" $PROBE_RUNAS "$@"
    else
        "${BOUND[@]}" runuser -u "$ACCOUNT" -- "$@"
    fi
}

# The words that run a command as the converged checkout's owner. Empty
# when this process IS the owner.
source_runas() {
    if [ -n "${PROBE_SOURCE_RUNAS+set}" ]; then
        printf '%s' "$PROBE_SOURCE_RUNAS"
        return 0
    fi
    local owner
    owner="$(stat -c %U -- "$SRC" 2>/dev/null)" || return 1
    [[ "$owner" =~ ^[a-z_][a-z0-9_-]*$ ]] || return 1
    if [ "$owner" = "$(id -un)" ]; then printf ''; else printf 'runuser -u %s --' "$owner"; fi
}

# Root's own git, in root's own repository: no account's config, no hook.
view_git() {
    GIT_CONFIG_GLOBAL=/dev/null git -c core.hooksPath=/dev/null -c advice.detachedHead=false -C "$VIEW" "$@"
}

# ── refresh-view ─────────────────────────────────────────────────────
# Prints one line saying what it did; returns 0 when the view's HEAD is
# the converged checkout's.
refresh_view() {
    local runas want have got err owner_uid
    umask 022
    runas="$(source_runas)" || { say "refresh-view FAILED — cannot read who owns $SRC"; return 1; }
    # shellcheck disable=SC2086
    want="$("${BOUND[@]}" $runas git -C "$SRC" rev-parse --verify -q 'HEAD^{commit}' 2>/dev/null)" \
        || { say "refresh-view FAILED — $SRC has no HEAD its owner can read"; return 1; }
    # What the owner's git printed is data from another account.
    [[ "$want" =~ ^[0-9a-f]{40,64}$ ]] \
        || { say "refresh-view FAILED — the owner's git answered something that is not a commit for $SRC"; return 1; }

    if [ -L "$VIEW" ]; then say "refresh-view REFUSED — $VIEW is a symlink"; return 1; fi
    if [ ! -d "$VIEW/.git" ]; then
        err="$(install -d -m 0755 -- "$VIEW" 2>&1 && GIT_CONFIG_GLOBAL=/dev/null git init -q -- "$VIEW" 2>&1)" \
            || { say "refresh-view FAILED — could not make $VIEW: $err"; return 1; }
    fi
    # The view is this process's own or it is not the view: a directory
    # another account owns is one that account can rewrite under a probe.
    owner_uid="$(stat -c %u -- "$VIEW" 2>/dev/null)"
    [ "$owner_uid" = "$(id -u)" ] \
        || { say "refresh-view REFUSED — $VIEW is owned by uid ${owner_uid:-unknown}, not this uid ($(id -u))"; return 1; }
    chmod 0755 -- "$VIEW" 2>/dev/null || true

    # `none` for a view just made: a word no commit name can equal.
    have="$(view_git rev-parse --verify -q 'HEAD^{commit}' 2>/dev/null || echo none)"
    if [ "$have" = "$want" ]; then
        say "the view is at ${want:0:12} already"
        return 0
    fi
    # The serving side runs as the checkout's owner; the path is quoted
    # by git when it builds the command line.
    err="$(timeout -k 5 "${PROBE_FETCH_TIMEOUT:-300}" env GIT_CONFIG_GLOBAL=/dev/null \
        git -c core.hooksPath=/dev/null -c fetch.fsckObjects=true -C "$VIEW" \
        fetch -q --no-tags --upload-pack="${runas:+$runas }git upload-pack" -- "$SRC" HEAD 2>&1)" \
        || { say "refresh-view FAILED — fetching $SRC as its owner: $err"; return 1; }
    got="$(view_git rev-parse --verify -q 'FETCH_HEAD^{commit}' 2>/dev/null)" \
        || { say "refresh-view FAILED — the fetch left no FETCH_HEAD"; return 1; }
    [[ "$got" =~ ^[0-9a-f]{40,64}$ ]] || { say "refresh-view FAILED — FETCH_HEAD is not a commit"; return 1; }
    err="$(view_git checkout -qf --detach "$got" 2>&1 && view_git clean -qfdx 2>&1)" \
        || { say "refresh-view FAILED — checking out ${got:0:12}: $err"; return 1; }
    say "the view moved ${have:0:12} -> ${got:0:12}"
    return 0
}

# How many processes a uid has that can still run. A zombie cannot: it
# is a killed process whose parent has not collected it yet, and counting
# it made a successful reap read as a survivor (measured 2026-10-06).
#
# Status 1 is "none": ps exits 1 with no process to list and grep -c
# exits 1 on a count of zero. Anything else is a count that could not be
# taken, and the caller refuses on it rather than read it as zero.
live_count() { # <uid>
    local n rc=0
    n="$(ps -o stat= -U "$1" 2>/dev/null | grep -vc '^Z')" || rc=$?
    [ "$rc" -le 1 ] || return 1
    case "${n:-empty}" in empty | *[!0-9]*) return 1 ;; esac
    printf '%s' "$n"
}

# ── reap ─────────────────────────────────────────────────────────────
# Prints what it found. Returns 0 when the account has no process left.
reap() {
    local uid who n round left
    uid="$(id -u "$ACCOUNT" 2>/dev/null)" || { say "reap: no account $ACCOUNT on this host — nothing to reap"; return 0; }
    # WHO THE KILL WOULD RUN AS, MEASURED FIRST. `kill -- -1` signals
    # every process its caller may signal, so the caller must be the
    # account and nobody else — not root, not this process. A seam or a
    # runuser that lands anywhere else stops here.
    who="$(as_account id -u 2>/dev/null || echo none)"
    if [ "$who" != "$uid" ] || [ "$who" = "0" ] || [ "$who" = "$(id -u)" ]; then
        say "reap REFUSED — a command run as $ACCOUNT answered uid '$who' (the account is $uid, this process $(id -u)); nothing was signalled"
        return 1
    fi
    n="$(live_count "$uid")" || { say "reap FAILED — the processes of uid $uid could not be counted; nothing was signalled"; return 1; }
    left="$n"
    round=0
    while [ "${left:-0}" -gt 0 ] && [ "$round" -lt 5 ]; do
        round=$((round + 1))
        # STOP first, so nothing forks between the two signals.
        # (`kill -KILL -1`, the spelling procps documents; it refuses
        # `-s KILL -- -1` — measured 2026-10-06.)
        as_account kill -STOP -1 2>/dev/null || true
        as_account kill -KILL -1 2>/dev/null || true
        sleep 0.2
        left="$(live_count "$uid")" || { say "reap FAILED — the processes of uid $uid could not be counted after round $round"; return 1; }
    done
    # The account removes its own files: no root process follows a path
    # the account named.
    local d dirs
    dirs="${PROBE_TMP_DIRS:-/tmp /var/tmp /dev/shm}"
    for d in $dirs; do
        [ -d "$d" ] || continue
        as_account find "$d" -xdev -user "$uid" -delete 2>/dev/null || true
    done
    if [ "${left:-0}" -gt 0 ]; then
        say "reap FAILED — $left process(es) of $ACCOUNT (uid $uid) survived $round round(s) of STOP and KILL"
        return 1
    fi
    if [ "$n" -gt 0 ]; then
        say "reap: $n process(es) $ACCOUNT left behind were killed"
    fi
    return 0
}

# ── verify ───────────────────────────────────────────────────────────
# What the host answers NOW about the account and its view: reads only,
# nothing repaired. Returns 0 with V_* set, or 1 with WHY set. Shared by
# `ensure` (after its repairs) and `verify-tick` (ahead of every runner
# tick), so "verified" means one thing.
WHY=""
V_UID="" V_GID="" V_GROUPS="" V_SHELL="" V_SUDO="" V_HEAD=""
verify_now() {
    local seen seen_rc=0 sudo_rc=0
    V_UID="$(id -u "$ACCOUNT" 2>/dev/null)" || { WHY="there is no account $ACCOUNT on this host"; return 1; }
    V_GID="$(id -g "$ACCOUNT" 2>/dev/null)" || { WHY="$ACCOUNT has no primary group"; return 1; }
    [ "$V_UID" != "0" ] || { WHY="$ACCOUNT is uid 0 — an account of that name exists and is root"; return 1; }
    [ "$V_GID" != "0" ] || { WHY="$ACCOUNT's primary group is gid 0"; return 1; }
    V_GROUPS="$(id -G "$ACCOUNT" 2>/dev/null)" || { WHY="the groups of $ACCOUNT could not be read"; return 1; }
    [ "$V_GROUPS" = "$V_GID" ] || { WHY="$ACCOUNT has supplementary groups (id -G says: $V_GROUPS; its own is $V_GID)"; return 1; }
    V_SHELL="$(getent passwd "$ACCOUNT" | cut -d: -f7)"
    case "$V_SHELL" in "$NOLOGIN" | /sbin/nologin | /bin/false | /usr/bin/false) ;; *) WHY="$ACCOUNT's shell is $V_SHELL"; return 1 ;; esac
    local f
    for f in cron at; do
        if grep -qxF -- "$ACCOUNT" "$HOST_ETC/$f.allow" 2>/dev/null; then
            WHY="$HOST_ETC/$f.allow names $ACCOUNT — a probe could schedule itself"
            return 1
        fi
    done
    if command -v sudo >/dev/null 2>&1; then
        # sudo -l exits 1 for an account with no rule, which is the
        # answer wanted; the words decide, and any other words stop.
        V_SUDO="$("${BOUND[@]}" sudo -n -l -U "$ACCOUNT" 2>&1)" || sudo_rc=$?
        case "$V_SUDO" in
            *"is not allowed to run sudo"*) V_SUDO="none" ;;
            *) WHY="sudo -l -U $ACCOUNT does not say the account has no rule (exit $sudo_rc): $(printf '%s' "$V_SUDO" | tr '\n' ' ' | cut -c1-300)"; return 1 ;;
        esac
    else
        V_SUDO="no sudo on this host"
    fi
    V_HEAD="$(view_git rev-parse --verify -q 'HEAD^{commit}' 2>/dev/null)" \
        || { WHY="the view at $VIEW has no HEAD"; return 1; }
    seen="$(cd / && as_account git -C "$VIEW" rev-parse --verify -q 'HEAD^{commit}' 2>&1)" || seen_rc=$?
    if [ "$seen_rc" -ne 0 ] || [ "$seen" != "$V_HEAD" ]; then
        WHY="$ACCOUNT cannot read the view: git (exit $seen_rc) answered '$(printf '%s' "$seen" | tr '\n' ' ' | cut -c1-200)' where root reads ${V_HEAD:0:12}"
        return 1
    fi
    as_account git -C "$VIEW" cat-file -e "HEAD:infra/forge/probe-bin/boss-sor-read" 2>/dev/null \
        || { WHY="$ACCOUNT cannot read HEAD:infra/forge/probe-bin/boss-sor-read in the view"; return 1; }
    if as_account test -w "$VIEW" || as_account test -w "$VIEW/.git"; then
        WHY="$ACCOUNT can WRITE the view at $VIEW"
        return 1
    fi
    return 0
}

# ── verify-tick ──────────────────────────────────────────────────────
# Ahead of every runner tick: the marker the unattended door requires is
# REMOVED FIRST, and written again only when the view is current and the
# account verifies now. Anything else — this copy missing, a hang cut by
# the bound, a drift — leaves no marker, and no probe runs that tick.
verify_tick() {
    local marker="${BOSS_PROBE_VERIFIED_FILE:-}"
    case "$marker" in
        /*) ;;
        *) say "verify-tick REFUSED — BOSS_PROBE_VERIFIED_FILE is not an absolute path ('$marker'); the drop-in sets it"; return 1 ;;
    esac
    rm -f -- "$marker" 2>/dev/null
    if [ -e "$marker" ] || [ -L "$marker" ]; then
        say "verify-tick FAILED — the last tick's marker at $marker could not be removed"
        return 1
    fi
    if ! refresh_view; then
        say "verify-tick: NO PROBE RUNS THIS TICK — the view could not be made current (the line above)"
        return 1
    fi
    if ! verify_now; then
        say "verify-tick: NO PROBE RUNS THIS TICK — $WHY"
        return 1
    fi
    umask 077
    if printf '%s\n' "$ACCOUNT" >"$marker.new" && mv -f -- "$marker.new" "$marker"; then
        return 0
    fi
    rm -f -- "$marker.new" "$marker" 2>/dev/null
    say "verify-tick FAILED — could not write $marker; no probe runs this tick"
    return 1
}

# ── ensure ───────────────────────────────────────────────────────────
# Everything `ensure` makes or repairs, stopping at the first thing it
# cannot. Returns 1 with WHY set; the caller verifies either way.
prepare() {
    local uid gid groups shell lock f
    # The copy root runs, outside any checkout (the recovery-kit reader's
    # rule: review of car 3d12b774, finding 1). First, so the tick's
    # verify exists even on a host where the account cannot be made.
    install -d -m 0755 -- "$LIBEXEC" || { WHY="cannot create $LIBEXEC"; return 1; }
    install -m 0755 -- "$HERE/probe-account.sh" "$LIBEXEC/probe-account" || { WHY="cannot install $LIBEXEC/probe-account"; return 1; }

    # The account. Created once; never re-created over a name that is
    # already something else.
    if ! getent passwd "$ACCOUNT" >/dev/null 2>&1; then
        useradd --system --user-group --no-create-home --home-dir /nonexistent \
            --shell "$NOLOGIN" --comment "BOSS car probes (backlog 703358ce)" "$ACCOUNT" \
            || { WHY="useradd could not create $ACCOUNT"; return 1; }
        say "created the account $ACCOUNT"
        FACTS+=("created")
    fi
    uid="$(id -u "$ACCOUNT" 2>/dev/null)" || { WHY="$ACCOUNT does not resolve after useradd"; return 1; }
    gid="$(id -g "$ACCOUNT" 2>/dev/null)" || { WHY="$ACCOUNT has no primary group"; return 1; }
    [ "$uid" != "0" ] || { WHY="$ACCOUNT is uid 0 — an account of that name already exists and is root"; return 1; }

    # The three things that drift, repaired here and read back by
    # verify_now: a repair is not believed.
    groups="$(id -G "$ACCOUNT" 2>/dev/null)"
    if [ "$groups" != "$gid" ]; then
        usermod -G "" "$ACCOUNT"
        say "removed $ACCOUNT's supplementary groups (it had: $groups)"
        FACTS+=("groups-repaired")
    fi
    shell="$(getent passwd "$ACCOUNT" | cut -d: -f7)"
    case "$shell" in
        "$NOLOGIN" | /sbin/nologin | /bin/false | /usr/bin/false) ;;
        *)
            usermod -s "$NOLOGIN" "$ACCOUNT"
            say "set $ACCOUNT's shell to $NOLOGIN (it was: $shell)"
            FACTS+=("shell-repaired")
            ;;
    esac
    lock="$(passwd -S "$ACCOUNT" 2>/dev/null | cut -d' ' -f2)"
    case "$lock" in L | LK | '!' | '!!') ;; *) usermod -L "$ACCOUNT"; FACTS+=("locked") ;; esac

    # No cron, no at, a bounded process count. cron.allow, where a host
    # has one, already excludes everyone it does not name.
    for f in cron at; do
        if [ -f "$HOST_ETC/$f.allow" ]; then
            if grep -qxF -- "$ACCOUNT" "$HOST_ETC/$f.allow"; then
                WHY="$HOST_ETC/$f.allow names $ACCOUNT — a probe could schedule itself"
                return 1
            fi
        elif ! grep -qxF -- "$ACCOUNT" "$HOST_ETC/$f.deny" 2>/dev/null; then
            printf '%s\n' "$ACCOUNT" >>"$HOST_ETC/$f.deny" || { WHY="could not add $ACCOUNT to $HOST_ETC/$f.deny"; return 1; }
        fi
    done
    install -d -m 0755 -- "$HOST_ETC/security/limits.d" \
        && printf '# Rendered by infra/forge/probe-account.sh ensure (backlog 703358ce).\n%s hard nproc %s\n' \
            "$ACCOUNT" "$NPROC_LIMIT" >"$HOST_ETC/security/limits.d/boss-probe.conf" \
        || { WHY="could not write the process bound under $HOST_ETC/security/limits.d"; return 1; }

    # The view, and the one git setting that lets the account read a
    # repository root owns.
    local gitcfg
    if [ -n "${INSTALL_PROBE_GITCONFIG:-}" ]; then
        gitcfg=(git config --file "$INSTALL_PROBE_GITCONFIG")
    else
        gitcfg=(git config --system)
    fi
    # Captured, then searched: a pipe into `grep -q` under pipefail is a
    # coin (the reader exits on its first match and the writer dies 141),
    # and here a lost toss would add the entry again on every converge.
    # git config exits 1 for "no such key" — the answer "not there yet";
    # anything else is a config that could not be READ, which is said.
    local safe safe_rc=0
    safe="$("${gitcfg[@]}" --get-all safe.directory 2>&1)" || safe_rc=$?
    if [ "$safe_rc" -gt 1 ]; then
        WHY="the gitconfig's safe.directory could not be read (git config exit $safe_rc: $(printf '%s' "$safe" | tr '\n' ' ' | cut -c1-200))"
        return 1
    fi
    if ! grep -qxF -- "$VIEW" <<<"$safe"; then
        "${gitcfg[@]}" --add safe.directory "$VIEW" || { WHY="could not add $VIEW to the system gitconfig's safe.directory"; return 1; }
    fi
    refresh_view || { WHY="the probe's view at $VIEW could not be made current (see the line above)"; return 1; }
    return 0
}

FACTS=()
ensure() {
    local verified=0 dropin runner_unit cli marker knows=0
    # WHICH MACHINE THIS IS, BEFORE THE COPY AND THE ACCOUNT (backlog
    # 62b09c57, N7; infra/lib/host-check.sh carries the incident — the
    # `boss-probe` account this function made on the dev pod on
    # 2026-10-07 is part of it — and the rule). Every place `ensure`
    # writes is named: with each one redirected, and the account commands
    # stubs ahead of the system's on PATH, this is a test and is asked
    # nothing; with any one real this machine must hold the address the
    # estate declares for the forge, or the exit is 78 and no file, no
    # account and no gitconfig line is written. Only `ensure` asks: the
    # other verbs run from root's installed copy, which sits outside every
    # tree and cannot reach the library — and they make nothing an
    # `ensure` on this host did not make first.
    # shellcheck source=infra/lib/host-check.sh
    . "$HERE/../lib/host-check.sh" 2>/dev/null \
        || { echo "$ME: REFUSED — $HERE/../lib/host-check.sh cannot be read, so which machine this is cannot be established; nothing was written" >&2; exit 78; }
    host_seam INSTALL_ETC /etc/systemd/system
    host_seam INSTALL_PROBE_ETC /etc
    host_seam INSTALL_PROBE_LIBEXEC /usr/local/libexec/boss
    host_seam INSTALL_PROBE_GITCONFIG
    host_seam BOSS_PROBE_VIEW /var/lib/boss/probe-view
    host_command useradd
    host_command usermod
    host_check forge "$ME"
    # shellcheck source=infra/run-summary.sh
    . "$HERE/../run-summary.sh"
    if [ "$ETC" = "/etc/systemd/system" ] && [ "$(id -u)" -ne 0 ]; then
        echo "$ME: ensure needs root" >&2
        exit 1
    fi

    # Make and repair what can be, then ask the host — whatever the
    # repairs returned.
    if prepare && verify_now; then
        verified=1
        run_summary_field probe_account_id "uid=$V_UID gid=$V_GID groups=$V_GROUPS shell=$V_SHELL"
        run_summary_field probe_account_sudo "$V_SUDO"
        run_summary_field probe_view "$VIEW at $V_HEAD"
    else
        echo "$ME: NOT VERIFIED — $WHY" >&2
        run_summary_note "$ME: NOT VERIFIED — $WHY"
    fi

    runner_unit="$ETC/boss-ops-runner.service"
    if [ ! -f "$runner_unit" ]; then
        say "this host has no ops runner unit at $runner_unit — no probe runs here, so no drop-in is written"
        if [ "$verified" -eq 1 ]; then
            run_summary_field probe_account "verified; no ops runner on this host"
            return 0
        fi
        run_summary_field probe_account "NOT VERIFIED: $WHY (no ops runner on this host)"
        return 1
    fi

    # THE DROP-IN IS WRITTEN WHETHER OR NOT THE ACCOUNT VERIFIED (decided
    # by David on design-doc c98c79aa, question `fallback`: no probe runs
    # on a host until its account verifies — never a fallback to the
    # checkout's owner). It names the account AND the marker the
    # unattended door requires; verify-tick writes that marker ahead of a
    # tick only when the account verifies then. So an unverified host has
    # a drop-in and no marker: every probe is refused before runuser, the
    # car records did-not-run, and this converge — which does not run as
    # the account — is red and repairs it on a later tick.
    #
    # ONE EXCEPTION, and it only delays: a `boss` on this host that
    # predates the marker would ignore it and hand an unverified name
    # straight to runuser, whose exit 1 reads as NOT PROVEN. With such a
    # CLI and no verified account the drop-in is not written this tick;
    # install.sh installs the tree's CLI ahead of this step, so that is
    # the tick or two before the image for this commit exists.
    cli="${INSTALL_PROBE_CLI:-/usr/local/bin/boss}"
    if grep -aq 'BOSS_PROBE_VERIFIED_FILE' "$cli" 2>/dev/null; then knows=1; fi
    marker="${INSTALL_PROBE_MARKER:-/run/boss-ops-runner/probe-account.verified}"
    dropin="$ETC/boss-ops-runner.service.d/probe-account.conf"
    if [ "$verified" -eq 0 ] && [ "$knows" -eq 0 ]; then
        echo "$ME: $cli does not know the verified-account marker yet, and $ACCOUNT is not verified — no drop-in is written this tick." >&2
        if [ -f "$dropin" ]; then
            run_summary_field probe_account "NOT VERIFIED: $WHY — and this host's boss predates the stop, so the drop-in already in place is left and probes are NOT stopped by it"
        else
            run_summary_field probe_account "NOT CONVERGED: $WHY — this host's boss predates the stop, so no drop-in is written and probes still run as before"
        fi
        return 1
    fi
    install -d -m 0755 -- "$ETC/boss-ops-runner.service.d" \
        || { run_summary_field probe_account "FAILED: could not create the ops runner's drop-in directory"; return 1; }
    {
        echo "# Rendered by infra/forge/probe-account.sh ensure on every forge converge"
        echo "# (backlog 703358ce; decisions bdc60b65 probe-user, c98c79aa fallback)."
        echo "# A probe runs as the account below, and only on a tick whose"
        echo "# verify-tick wrote the marker: no marker, no probe."
        echo "[Service]"
        echo "Environment=BOSS_PROBE_USER=$ACCOUNT"
        echo "Environment=BOSS_PROBE_DIR=$VIEW"
        echo "Environment=BOSS_PROBE_VERIFIED_FILE=$marker"
        echo "Environment=BOSS_PROBE_VIEW_SOURCE=$SRC"
        # The runner fires every minute and a oneshot has no start
        # timeout of its own: ahead of a tick the fetch gets 30 s, not
        # the 300 s the converge allows the first, whole-history one.
        echo "Environment=PROBE_FETCH_TIMEOUT=30"
        echo "ExecStartPre=-$LIBEXEC/probe-account verify-tick"
        echo "ExecStopPost=-$LIBEXEC/probe-account reap"
    } >"$dropin.new" && mv -f -- "$dropin.new" "$dropin" \
        || { run_summary_field probe_account "FAILED: could not write $dropin"; return 1; }

    if [ "$verified" -eq 1 ]; then
        say "$ACCOUNT verified (uid $V_UID, no group but its own, sudo: $V_SUDO); the view is at ${V_HEAD:0:12}; $dropin points the ops runner's probes at both"
        run_summary_field probe_account "verified${FACTS[*]:+ (${FACTS[*]})}; probes run as $ACCOUNT in $VIEW"
        return 0
    fi
    echo "$ME: PROBES ARE STOPPED ON THIS HOST — $WHY. No probe runs until the account verifies; each car records did-not-run, never a failure." >&2
    run_summary_field probe_account "STOPPED: $WHY — no probe runs on this host until the account verifies"
    return 1
}

# ── controls ─────────────────────────────────────────────────────────
# Every directory an installed unit names as a spool or as replayed
# state, one per line, each once. The units are the one place these are
# spelled (a script's default is refused or overridden there), so the
# controls try what this host actually runs with.
spool_dirs() {
    local u
    for u in "$ETC"/*.service; do
        [ -f "$u" ] || continue
        sed -n 's/^Environment=\(ALERT_SPOOL\|DOOR_SPOOL_DIR\|DOOR_STATE_DIR\|SPOOL_DIR\)=\(\/.*\)$/\2/p' "$u"
    done | sort -u
}

controls() {
    local fails=0 uid who owner owner_home owner_uid p f n line
    pass() { echo "PASS    $*"; }
    fail() { echo "FAIL    $*"; fails=$((fails + 1)); }
    absent() { echo "ABSENT  $* (nothing here to refuse — not a pass)"; }
    info() { echo "INFO    $*"; }

    # THE METHOD'S OWN CONTROL, FIRST. Every refusal below is "a command
    # run as the account failed"; if running as the account fails by
    # itself, every one of them would pass for nothing.
    uid="$(id -u "$ACCOUNT" 2>/dev/null)" || { echo "NOT RUN the controls: no account $ACCOUNT on this host"; return 2; }
    who="$(as_account id -u 2>/dev/null || echo none)"
    if [ "$who" != "$uid" ] || [ "$uid" = "0" ]; then
        echo "NOT RUN the controls: a command run as $ACCOUNT answered uid '$who', the account is uid $uid"
        return 2
    fi
    as_account test -r /etc/passwd || { echo "NOT RUN the controls: as $ACCOUNT even /etc/passwd is unreadable, so a refusal below would mean nothing"; return 2; }
    pass "method: commands run as $ACCOUNT are uid $uid and can read a world-readable file"

    # WHO IT IS
    n="$(as_account id -G 2>/dev/null)"
    if [ "$n" = "$(id -g "$ACCOUNT")" ]; then pass "identity: no group but its own (id -G: $n)"; else fail "identity: $ACCOUNT runs with groups '$n'"; fi
    f="$(getent passwd "$ACCOUNT" | cut -d: -f7)"
    case "$f" in */nologin | */false) pass "identity: shell $f" ;; *) fail "identity: shell is $f" ;; esac

    # SUDO
    if command -v sudo >/dev/null 2>&1; then
        if as_account sudo -n true >/dev/null 2>&1; then fail "sudo: $ACCOUNT ran 'sudo -n true'"; else pass "sudo: 'sudo -n true' refused"; fi
        line="$("${BOUND[@]}" sudo -n -l -U "$ACCOUNT" 2>&1 | tr '\n' ' ' | cut -c1-300)"
        case "$line" in *"is not allowed to run sudo"*) pass "sudo: -l -U $ACCOUNT: $line" ;; *) fail "sudo: -l -U $ACCOUNT: $line" ;; esac
    else
        absent "sudo: no sudo on this host"
    fi

    # WHAT IT MUST NOT READ. A directory here is secret whole: root
    # walks it and the account is tried on every file found.
    # WHO OWNS THE CHECKOUT. Unreadable is said, and counted: the
    # owner's home, tokens and docker socket are half of what follows.
    local owner_known=1
    owner="$(stat -c %U -- "$SRC" 2>/dev/null)" || owner_known=0
    owner_home=""
    owner_uid=""
    if [ "$owner_known" -eq 1 ] && getent passwd "$owner" >/dev/null 2>&1; then
        owner_home="$(getent passwd "$owner" | cut -d: -f6)"
        owner_uid="$(getent passwd "$owner" | cut -d: -f3)"
    elif [ -z "${PROBE_CONTROL_SECRETS+set}" ]; then
        echo "NOT RUN owner: cannot read which account owns $SRC, so its home, tokens and docker socket were not tried"
        fails=$((fails + 1))
        owner=""
    else
        owner=""
    fi
    local secrets="${PROBE_CONTROL_SECRETS-/etc/boss/machine-token /etc/boss/ops-runner.credential /etc/boss-ops /etc/boss-train /etc/boss-publish /root /run/boss-ops-runner /run/forge-converge ${owner_home:+$owner_home/.config/boss $owner_home/.ssh $owner_home/.docker $owner_home/.kube}}"
    for p in $secrets; do
        if [ ! -e "$p" ]; then absent "read: $p"; continue; fi
        n=0
        local bad=0
        while IFS= read -r f; do
            n=$((n + 1))
            if as_account test -r "$f"; then
                fail "read: $ACCOUNT can read $f ($(stat -c '%A %U:%G' -- "$f"))"
                bad=$((bad + 1))
            fi
        done < <(find "$p" -maxdepth 3 \( -type f -o -type s \) 2>/dev/null)
        [ "$bad" -gt 0 ] || pass "read: $p refused ($n file(s) tried, $(stat -c '%A %U:%G' -- "$p"))"
        if [ -d "$p" ] && as_account ls -- "$p" >/dev/null 2>&1; then
            info "read: $ACCOUNT can LIST $p, though not read what is in it"
        fi
    done
    if [ -n "$owner_home" ] && [ -d "$owner_home" ]; then
        if as_account ls -- "$owner_home" >/dev/null 2>&1; then
            fail "read: $ACCOUNT can list $owner's home $owner_home ($(stat -c '%A' -- "$owner_home"))"
        else
            pass "read: $owner's home $owner_home refused ($(stat -c '%A' -- "$owner_home"))"
        fi
    fi

    # WHAT IT MUST NOT WRITE: its own view, the checkout root executes
    # from, the units, the binaries, and what decides who may do what.
    # The tree root executes from since backlog a604a35b, its launcher and
    # the watchdog's state ride the same list: each is root's, and a name
    # the account could write under any of them is a name root would run
    # or replay.
    local root_tree="${BOSS_ROOT_TREE:-/var/lib/boss/tree}"
    local nowrite="${PROBE_CONTROL_NOWRITE-$VIEW $VIEW/.git $VIEW/infra/forge/probe-bin/boss-sor-read $root_tree $root_tree/gen $root_tree/current/infra/forge/forge-converge.sh $root_tree/current/infra/forge/cluster-watchdog.sh $LIBEXEC/forge-converge-launch /var/lib/boss/watchdog $SRC $SRC/infra/forge/forge-converge.sh $ETC $ETC/boss-ops-runner.service.d/probe-account.conf /run/boss-ops-runner $LIBEXEC $LIBEXEC/probe-account /usr/local/bin /usr/local/bin/boss /opt/boss-cli /etc/sudoers /etc/sudoers.d /etc/passwd /etc/group /etc/gitconfig /etc/boss /var/lib/boss /etc/cron.d /etc/cron.deny}"
    for p in $nowrite; do
        if [ ! -e "$p" ]; then absent "write: $p"; continue; fi
        if as_account test -w "$p"; then fail "write: $ACCOUNT can write $p ($(stat -c '%A %U:%G' -- "$p"))"; else pass "write: $p refused"; fi
    done

    # THE SPOOLS ANOTHER ACCOUNT REPLAYS (backlog dda26693; review
    # acab446c F8a). The checkout owner's units keep what the record
    # would not take — alerts, door readings, a door's dark-since — and
    # POST it back later as their own. Each directory is made by the
    # first run with something to keep, so "the account cannot write it"
    # is not enough: where it is not there yet, the account must not be
    # able to MAKE it. Read off the units installed on this host, never a
    # list here; a host whose units name none is running the scripts'
    # old defaults under /var/tmp, and that is a FAIL.
    local spools parent
    spools="$(spool_dirs)"
    [ -n "$spools" ] || fail "spool: no installed unit under $ETC names a spool (ALERT_SPOOL, DOOR_SPOOL_DIR, DOOR_STATE_DIR, SPOOL_DIR) — the scripts' defaults are names under /var/tmp, which $ACCOUNT can make first"
    for p in $spools; do
        parent="$(dirname -- "$p")"
        if as_account test -w "$parent"; then
            fail "spool: $p — $ACCOUNT can write $parent ($(stat -c '%A %U:%G' -- "$parent" 2>/dev/null)), so it can make or replace the spool and plant what the next replay files"
        elif [ ! -e "$p" ]; then
            pass "spool: $p is not there yet and $ACCOUNT cannot make it ($parent refused its write)"
        elif as_account test -w "$p"; then
            fail "spool: $p — $ACCOUNT can write it ($(stat -c '%A %U:%G' -- "$p"))"
        else
            pass "spool: $p refused ($(stat -c '%A %U:%G' -- "$p"))"
        fi
    done

    # DOCKER, both daemons.
    local sockets="${PROBE_CONTROL_SOCKETS-/var/run/docker.sock ${owner_uid:+/run/user/$owner_uid/docker.sock}}"
    for p in $sockets; do
        if [ ! -e "$p" ]; then absent "docker: $p"; continue; fi
        if as_account test -r "$p" || as_account test -w "$p"; then
            fail "docker: $ACCOUNT can open $p ($(stat -c '%A %U:%G' -- "$p"))"
        else
            pass "docker: $p refused ($(stat -c '%A %U:%G' -- "$p"))"
        fi
    done

    # OTHER ACCOUNTS' PROCESSES: no signal, and no read of what ptrace
    # would read.
    if as_account kill -0 "$$" 2>/dev/null; then fail "signal: $ACCOUNT may signal this process (uid $(id -u))"; else pass "signal: kill -0 of a uid $(id -u) process refused"; fi
    if as_account cat "/proc/$$/environ" >/dev/null 2>&1; then fail "ptrace: $ACCOUNT read /proc/$$/environ"; else pass "ptrace: /proc/<this pid>/environ refused"; fi
    [ -r /proc/sys/kernel/yama/ptrace_scope ] && info "ptrace: kernel.yama.ptrace_scope=$(cat /proc/sys/kernel/yama/ptrace_scope)"

    # PERSISTENCE: no schedule, and nothing left running.
    if command -v crontab >/dev/null 2>&1; then
        line="$(as_account crontab -l 2>&1 | tr '\n' ' ' | cut -c1-200)"
        case "$line" in *"not allowed"*) pass "cron: $line" ;; *) fail "cron: crontab -l as $ACCOUNT answered: $line" ;; esac
    else
        absent "cron: no crontab on this host"
    fi
    if [ "$uid" != "$(id -u)" ]; then
        as_account setsid sleep 600 >/dev/null 2>&1 &
        sleep 0.5
        n="$(live_count "$uid" || echo uncounted)"
        if [ "$n" = "uncounted" ] || [ "$n" -lt 1 ]; then
            echo "NOT RUN leftover: the control's own setsid child of $ACCOUNT did not start, so reaping it proves nothing"
            fails=$((fails + 1))
        else
            line="$(reap 2>&1)"
            n="$(live_count "$uid" || echo uncounted)"
            if [ "$n" = "0" ]; then pass "leftover: a setsid child of $ACCOUNT was running and is gone after reap ($line)"; else fail "leftover: $n process(es) of $ACCOUNT survive reap ($line)"; fi
        fi
    else
        echo "NOT RUN leftover: this process is the account"
        fails=$((fails + 1))
    fi

    # POSITIVE: what an ordinary recorded probe needs still works.
    local head_root head_acct
    head_root="$(view_git rev-parse --verify -q 'HEAD^{commit}' 2>/dev/null || echo none)"
    head_acct="$(cd / && as_account git -C "$VIEW" rev-parse --verify -q 'HEAD^{commit}' 2>&1 | tr '\n' ' ')"
    if [ "$head_root" != "none" ] && [ "${head_acct% }" = "$head_root" ]; then pass "probe: git reads the view's HEAD ${head_root:0:12}"; else fail "probe: git as $ACCOUNT in $VIEW answered '$head_acct' (root reads '${head_root:0:12}')"; fi
    # Captured, then searched (never piped into `grep -q` under pipefail),
    # so a git read that FAILED and a blob that is not the reader are two
    # different lines.
    local shown shown_rc=0
    shown="$(as_account git -C "$VIEW" show "HEAD:infra/forge/probe-bin/boss-sor-read" 2>&1)" || shown_rc=$?
    if [ "$shown_rc" -ne 0 ]; then
        fail "probe: git show HEAD:infra/forge/probe-bin/boss-sor-read failed as $ACCOUNT (exit $shown_rc: $(printf '%s' "$shown" | tr '\n' ' ' | cut -c1-200))"
    elif grep -q 'boss-sor-read' <<<"$shown"; then
        pass "probe: git show HEAD:<path> reads the converged tree"
    else
        fail "probe: git show HEAD:infra/forge/probe-bin/boss-sor-read answered something that is not the reader"
    fi
    if as_account test -x "$VIEW/infra/forge/probe-bin/boss-sor-read"; then pass "probe: boss-sor-read is executable in the view"; else fail "probe: boss-sor-read is not executable in the view"; fi
    for f in jq curl; do
        if as_account "$f" --version >/dev/null 2>&1; then pass "probe: $f runs"; else fail "probe: $f does not run as $ACCOUNT"; fi
    done
    if [ -e /usr/local/bin/boss ]; then
        if as_account test -x /usr/local/bin/boss; then pass "probe: the tree's boss CLI is executable"; else fail "probe: /usr/local/bin/boss is not executable as $ACCOUNT"; fi
    else
        absent "probe: /usr/local/bin/boss"
    fi

    # THE HOST FACTS THE TREE ONLY ASSUMES (item 703358ce): read, never
    # guessed. Whether the owner KEEPS what this shows is David's call.
    if [ -n "$owner" ] && command -v sudo >/dev/null 2>&1; then
        info "sudo -l -U $owner (the checkout's owner) says:"
        "${BOUND[@]}" sudo -n -l -U "$owner" 2>&1 | sed 's/^/INFO      | /'
    fi
    [ -n "$owner" ] && info "groups of $owner: $(id -Gn "$owner" 2>/dev/null)"
    info "process bound: ulimit -u as $ACCOUNT is $(as_account bash -c 'ulimit -u' 2>&1 | tr '\n' ' ')(declared $NPROC_LIMIT)"

    if [ "$fails" -gt 0 ]; then
        echo "probe-account controls: $fails control(s) did NOT hold on $(hostname) for $ACCOUNT (uid $uid) — each is a FAIL or NOT RUN line above"
        return 1
    fi
    echo "probe-account controls: every control held on $(hostname) for $ACCOUNT (uid $uid); an ABSENT line is a path this host does not have, not a refusal"
    return 0
}

case "$MODE" in
    ensure) ensure ;;
    verify-tick) verify_tick ;;
    refresh-view) refresh_view ;;
    reap) reap ;;
    controls) controls ;;
    *)
        echo "usage: probe-account.sh ensure | verify-tick | refresh-view | reap | controls" >&2
        exit 2
        ;;
esac
