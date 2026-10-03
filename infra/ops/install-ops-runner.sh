#!/usr/bin/env bash
# install-ops-runner.sh — ONE definition of how a host gets an ops
# runner. Usage: install-ops-runner.sh <estate node id>
#                install-ops-runner.sh --in-role
#
# Two hosts answer ops-request packets and both install the runner from
# here: the forge, through infra/forge/install.sh (forge-converge, every
# ten minutes), and boss-gcp, through `install-units.sh units` (its
# self-converge, every half hour).
#
# WHY IT IS ONE SCRIPT. Until 2026-09-11 only the forge installed a
# runner, in a block of its own, and boss-gcp had none at all — so an
# ops-request filed against the WireGuard bastion sat at `ready` with
# nothing behind it and a human went back to being the transport
# (backlog c3d06016; David, design 9e3e093f: boss-gcp "isn't part of
# the kubernetes cluster... I still want it fully managed and
# maintained by BOSS and protocol"). Giving boss-gcp a second,
# near-identical block would have been the §9a defect — this is the
# collapse instead, so "how a host gets an ops runner" has one answer
# and a widening lands once.
#
# WHAT A HOST GETS
#   * the unit pair from infra/ops, BYTE-IDENTICAL on every host: the
#     file is the definition, and everything host-specific is the
#     drop-in's business
#   * a drop-in <host>.conf naming THIS host (HOST_ID, which the runner
#     refuses to run without) and running the runner from THIS
#     checkout
#   * the timer enabled
#
# WHERE THE SYSTEM OF RECORD COMES FROM. The unit reads
# /etc/boss/sor.env (EnvironmentFile=), which each host's converge
# renders from infra/estate/estate.toml before it calls this script —
# ONE spelling for every host (backlog 5222163e). Until 2026-09-18 the
# drop-in pinned it INLINE with env(1) on the Exec line, because
# deploy-services.sh wrote a shared `jobs-url.conf` drop-in pointing the
# timer fleet at `127.0.0.1` — which on boss-gcp was the LEGACY second
# stack, not the system of record (91ddebfb) — and a drop-in's
# `Environment=` outranks the unit's. EnvironmentFile= outranks any
# Environment=, drop-in or not, so the file does what the pin did. This
# matters more here than almost anywhere: a runner pointed at a wrong
# instance finds no ops-request packets, exits 0 every minute, and
# looks perfectly healthy forever — a wrong target answers instead of
# erroring (CLAUDE.md §Doors). So the address is CHECKED here, from the
# same file the unit will read, and a host without it gets no runner
# rather than a runner with no target. The empty `ExecStart=` clears
# the unit's own command before the override, which is how a drop-in
# replaces rather than appends one.
#
# AND IT ONLY CLAIMS WHAT HAPPENED. Until 2026-09-11 the last line here
# was an unconditional "boss-ops-runner installed for HOST_ID=…": with no
# `set -e`, a failed `install` or a refused `enable --now` printed its
# error and the script carried on to print that line and exit 0, so the
# caller — and the converge's packet — read success either way. Every step
# below is checked, each failure NAMES itself, and a runner that did not
# install exits non-zero, which reddens the unit and lands its packet on
# `failed`. Deliberately not the journal door's treatment: the door is
# visibility, this is the host's ability to ACT on a packet at all. It is
# also the last thing either installer does, so failing here cannot stop a
# single unit file from converging.
#
# ENV: INSTALL_ETC / INSTALL_SYSTEMCTL, the same two knobs both
# installers take, so a lint can drive this into a scratch root with a
# stub systemctl and assert what a host would actually get; and
# BOSS_NODE_ROLES, the host's live roles, which decide whether it gets a
# runner at all — see the role block below.
set -uo pipefail

# The verdict rides the run summary onto the converge's packet when the
# caller set BOSS_RUN_SUMMARY_FILE; a no-op otherwise.
# shellcheck source=infra/run-summary.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/run-summary.sh"

# shellcheck source=infra/estate/node-roles.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/../estate" && pwd)/node-roles.sh"

# THE ONE PREDICATE: does a host with these roles get a runner? Read by
# the install below and, through `--in-role`, by anything that must
# agree with it without installing anything. EMPTY roles install as
# before roles existed; the `registry-unread` sentinel names no role, so
# a dark read never widens what a host runs. See the role block below
# for why the declaration is the cause.
wants_runner() {
    [ -z "${BOSS_NODE_ROLES:-}" ] || has_role ops-runner
}

# `--in-role`: exit 0 if BOSS_NODE_ROLES gets a runner, 1 if not, and
# nothing else — no root, no systemctl, no SoR, nothing written. It
# exists for infra/estate/observe-units.sh (backlog bf362f25): the
# runner is not a roles.toml row, so the observer's row-derived roster
# never watched it, and on 2026-09-22/23 boss-ops-runner went red every
# minute on a host whose observer posted a clean reading every five —
# post-mortem 3c3b202c found no estate record of it at all. The observer
# asks HERE rather than restating the predicate, so what a host installs
# and what its observer watches stay one answer (CLAUDE.md §9a).
if [ "${1:-}" = "--in-role" ]; then
    wants_runner
    exit $?
fi

HOST="${1:?usage: install-ops-runner.sh <estate node id>}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
ETC="${INSTALL_ETC:-/etc/systemd/system}"
SYSTEMCTL="${INSTALL_SYSTEMCTL:-systemctl}"

refuse() { # <what failed>
    echo "install-ops-runner: FAILED — $1" >&2
    echo "    HOST_ID=$HOST has no working ops-request runner, so a packet filed for this" >&2
    echo "    host waits at 'ready' with nothing behind it." >&2
    run_summary_field ops_runner "failed: $1"
    run_summary_note "install-ops-runner: FAILED — $1"
    exit 1
}

# WHETHER THIS HOST GETS ONE AT ALL: the `ops-runner` role decides, and
# it decides HERE (backlog cb9eb0f2, 2026-09-22). Until this line both
# callers ran this script unconditionally and the role — declared on the
# forge and boss-gcp in infra/estate/estate.toml, mapped in
# infra/estate/roles.toml — was read only by the world map. Three
# statements of "which hosts run a runner", the third added AS the
# authority and the other two not knowing it existed, so the authority
# could say one thing while the estate did another and nothing
# disagreed out loud: remove the role and the map stopped drawing a
# runner that kept running (CLAUDE.md §9a, with the extra turn). The
# declaration is now the CAUSE, read once in the one definition of how a
# host gets a runner, so neither caller carries a copy of the predicate.
#
# BOSS_NODE_ROLES is what each converge already read off the estate
# registry and exported before calling (infra/estate/node-roles.sh);
# this script never reads the registry itself, so what it installs and
# what install-units.sh installs answer to ONE read. Its conventions are
# that read's: EMPTY means no roles declared and installs as before
# roles existed, and the `registry-unread` sentinel of a dark registry
# names no role — so a failed read never WIDENS what a host runs, and a
# runner already installed keeps running until a tick that can read the
# registry converges it.
#
# SKIPPING IS EXIT 0, not a refusal. Not being an ops-runner is a
# correct outcome, and infra/gcp/install-units.sh exits with this
# script's code — a refusal here would red the whole converge of every
# host that is not one.
#
# AND SKIPPING IS NOT UNINSTALLING (backlog 1c7f8f23, the other half of
# cb9eb0f2). Drop the role from a host that already has a runner and
# this branch declines to install one — while the runner already there
# keeps running, keeps claiming ops-request packets filed for this
# host, and the estate's map, which draws the DECLARATION, shows no
# runner there at all. A second executor on one queue that nothing
# reports is this repo's recurring shape: a wrong target answers
# instead of erroring.
#
# SO THIS CONVERGE REPORTS IT AND DOES NOT REMOVE IT, deliberately.
# Removing would make the declaration authoritative in both directions
# in one step, and it is the wrong step: the runner is the DOOR every
# bounded verb arrives through, which is why infra/gcp/uninstall-not-in-
# role.sh hard-keeps `boss-ops-runner` whatever the roster says ("the
# door and the converge are never in this set") and why
# infra/lint/boss-gcp-converges-itself.sh refuses to let it become a
# roles.toml row at all. A converge that stops an executor because a
# TOML line changed is destructive-by-policy, which CLAUDE.md puts on
# the far side of the line from the mechanical work a converge may do
# — and it would take the host's only protocol door down with it,
# leaving a hand SSH as the way back. Stopping one is a deliberate
# bounded step; SAYING SO is this script's, every half hour, until
# someone acts.
#
# THE LOUDNESS IS THE POINT, and it is graded. A host that never had a
# runner records the skip as a FIELD only: that is the ordinary state
# of most of the estate, and an anomaly filed every half hour by every
# non-runner host is the permanently-warning channel nobody reads by
# the time it matters (CLAUDE.md §Diagnosis). A host that HAS one and
# no longer declares it files an ANOMALY, because the packet is what a
# reader without host access sees and a report that reaches only the
# journal is one they never see (the same reading
# infra/lint/boss-gcp-converges-itself.sh already enforces for a
# not-in-role unit).
#
# The FILE is the fact, not `systemctl list-units`: the 2026-09-15
# retire's after-snapshot showed disabled units gone from the listing
# with their files still on disk (infra/gcp/uninstall-not-in-role.sh,
# bound 5).
#
# AND THE DELIBERATE STEP NOW EXISTS, so this branch is its independent
# witness (backlog 98eb9349; design a79a8067, David 2026-10-01). The
# bounded verb retire-ops-runner (infra/ops/retire-ops-runner.sh) runs
# INSIDE the runner it retires: it writes the marker below naming its
# request, then stops and disables boss-ops-runner.timer and nothing
# else, so the pass running it survives to report. This converge runs on
# its own timer and reports on its own packet, owing nothing to the
# runner, and reads three facts — the unit files, the timer's is-enabled
# and is-active WORDS, and the marker:
#
#   timer still on (any other word, or no answer)  STILL INSTALLED, as before
#   timer off, marker names a request              RETIRED, then REMOVED
#   timer off, no marker                           DISABLED BY HAND (loud:
#                                                  nothing on the record
#                                                  did it), then REMOVED
#
# REMOVING IS THE CONVERGE'S once the timer is off and the role
# undeclared: the executor is already stopped by a deliberate act, so
# deleting its files stops nothing — the mechanical work a converge may
# do, where stopping it never was. Leaving them would repeat the
# 2026-09-15 residue (files on disk, gone from the listing). The removal
# also waits while boss-ops-runner.service is mid-run, so the cut is
# behind the pass that retired it, never under it. The timer file goes
# LAST, so a removal that stops part-way leaves the timer file present
# and the next tick finishes it. infra/gcp/uninstall-not-in-role.sh and
# its keep set stay untouched: the runner's whole lifecycle is here.
#
# The marker's path, the SAME line as in retire-ops-runner.sh, pinned
# equal by crates/core/boss-testing/tests/retire_ops_runner_sh.rs (§9a).
RETIRED_MARKER="${BOSS_OPS_RUNNER_RETIRED:-/var/lib/boss/ops-runner.retired}"

# The marker's line, if it is one the verb wrote for THIS host — anything
# else (absent, unreadable, another shape, another host) answers nothing,
# which is the DISABLED BY HAND reading: only a well-formed record
# credits a request.
retired_by() {
    local line
    [ -r "$RETIRED_MARKER" ] || return 0
    IFS= read -r line < "$RETIRED_MARKER" || [ -n "$line" ] || return 0
    [[ "$line" =~ ^ops-request=[0-9a-f-]{36}\ host=([a-z0-9-]+)\ at=[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$ ]] || return 0
    [ "${BASH_REMATCH[1]}" = "$HOST" ] || return 0
    printf '%s' "$line"
}

if ! wants_runner; then
    declared="$HOST declares $BOSS_NODE_ROLES (source: ${BOSS_NODE_ROLES_SOURCE:-preset})"
    present=""
    for ext in service timer; do
        [ -e "$ETC/boss-ops-runner.$ext" ] && present="${present:+$present }boss-ops-runner.$ext"
    done
    if [ -z "$present" ]; then
        marked="$(retired_by)"
        if [ -n "$marked" ]; then
            echo "install-ops-runner: $HOST does not declare the ops-runner role (roles: $BOSS_NODE_ROLES, source: ${BOSS_NODE_ROLES_SOURCE:-preset}) — its runner was retired ($marked) and its files are gone from $ETC."
            run_summary_field ops_runner "not in role: $declared; retired ($marked), files removed"
            exit 0
        fi
        echo "install-ops-runner: $HOST does not declare the ops-runner role (roles: $BOSS_NODE_ROLES, source: ${BOSS_NODE_ROLES_SOURCE:-preset}) — no runner installed here. An ops-request filed for this host waits at 'ready'; naming the role in infra/estate/estate.toml is what installs one on the next converge."
        run_summary_field ops_runner "not in role: $declared"
        exit 0
    fi
    # Read by the WORD: is-enabled exits 1 for `disabled` and is-active 3
    # for `inactive`, so an exit cannot be the reading. No answer is not
    # off — it keeps the STILL INSTALLED report, and nothing is removed.
    t_en=$("$SYSTEMCTL" is-enabled -- boss-ops-runner.timer 2>/dev/null)
    t_en="${t_en%%$'\n'*}"
    t_act=$("$SYSTEMCTL" is-active -- boss-ops-runner.timer 2>/dev/null)
    t_act="${t_act%%$'\n'*}"
    timer_off=""
    case "$t_en:$t_act" in
        disabled:inactive | masked:inactive) timer_off=1 ;;
    esac
    # A DECLARATION NOBODY READ LIVE REMOVES NOTHING (adversarial review
    # 9c02f3b0, F1). On a dark registry node-roles.sh hands this script
    # its cache or the `registry-unread` sentinel, which names no role —
    # right for "never widen what a host runs", and no evidence at all
    # that the role was dropped: a host that still declares ops-runner,
    # its timer off for maintenance, would lose its files on a packet
    # saying it "does not declare" a role nobody read. Removal is held to
    # the retire verb's own bar, a LIVE read; any other source reports the
    # timer's words and the marker, keeps the files, and leaves the
    # decision to a tick that reads the registry.
    if [ -n "$timer_off" ] && [ "${BOSS_NODE_ROLES_SOURCE:-preset}" != registry ]; then
        marked="$(retired_by)"
        echo "install-ops-runner: $HOST's ops-runner declaration was not read live this tick (roles: $BOSS_NODE_ROLES, source: ${BOSS_NODE_ROLES_SOURCE:-preset}); boss-ops-runner.timer reads is-enabled $t_en, is-active $t_act$([ -n "$marked" ] && printf ', marker %s' "$marked"). Nothing removed: $present stay under $ETC until a tick that reads the registry decides."
        run_summary_field ops_runner "declaration not read live (roles: $BOSS_NODE_ROLES, source: ${BOSS_NODE_ROLES_SOURCE:-preset}); timer is-enabled $t_en, is-active $t_act; ${marked:-no marker}; nothing removed"
        exit 0
    fi
    if [ -n "$timer_off" ]; then
        marked="$(retired_by)"
        if [ -n "$marked" ]; then
            word="RETIRED"
            why="by $marked"
        else
            word="DISABLED BY HAND"
            why="with no record of who: $RETIRED_MARKER does not name a request for $HOST, so no ops-request did it"
        fi
        s_act=$("$SYSTEMCTL" is-active -- boss-ops-runner.service 2>/dev/null)
        s_act="${s_act%%$'\n'*}"
        case "$s_act" in
            inactive | failed) ;;
            *)
                echo "install-ops-runner: $HOST's ops runner is $word ($why): boss-ops-runner.timer is-enabled $t_en, is-active $t_act. Its files stay this tick — boss-ops-runner.service is-active answers ${s_act:-nothing}, and a pass still running is not removed under itself; the next converge removes them."
                run_summary_field ops_runner "not in role: $declared; $word ($why); removal waits on the service run in flight (${s_act:-no answer})"
                [ "$word" = RETIRED ] || run_summary_note "install-ops-runner: $HOST's ops runner was DISABLED BY HAND — boss-ops-runner.timer is off and no ops-request recorded doing it. Roles: $BOSS_NODE_ROLES."
                exit 0
                ;;
        esac
        removed=""
        for f in boss-ops-runner.service.d boss-ops-runner.service boss-ops-runner.timer; do
            [ -e "$ETC/$f" ] || continue
            if ! rm -rf -- "${ETC:?}/$f"; then
                echo "install-ops-runner: FAILED — $HOST's ops runner is $word ($why) and $ETC/$f could not be removed; removed so far: ${removed:-none}. The next converge tries again." >&2
                run_summary_field ops_runner "failed: $word ($why), $ETC/$f could not be removed (removed: ${removed:-none})"
                run_summary_note "install-ops-runner: FAILED to remove $ETC/$f after the runner was $word"
                exit 1
            fi
            removed="${removed:+$removed }$ETC/$f"
        done
        "$SYSTEMCTL" daemon-reload \
            || { echo "install-ops-runner: FAILED — removed $removed but systemctl daemon-reload failed, so systemd may still hold the units" >&2
                 run_summary_field ops_runner "failed: $word ($why), removed $removed, daemon-reload failed"
                 exit 1; }
        left=""
        for f in boss-ops-runner.service.d boss-ops-runner.service boss-ops-runner.timer; do
            [ -e "$ETC/$f" ] && left="${left:+$left }$ETC/$f"
        done
        if [ -n "$left" ]; then
            echo "install-ops-runner: FAILED — rm answered success and $left is still on disk" >&2
            run_summary_field ops_runner "failed: $word ($why), still on disk after rm: $left"
            exit 1
        fi
        echo "install-ops-runner: $HOST does not declare the ops-runner role and its runner is $word ($why): boss-ops-runner.timer read is-enabled $t_en, is-active $t_act. REMOVED $removed, then daemon-reload; read back: none of them is on disk."
        run_summary_field ops_runner "not in role: $declared; $word ($why); REMOVED $removed"
        [ "$word" = RETIRED ] || run_summary_note "install-ops-runner: $HOST's ops runner was DISABLED BY HAND — boss-ops-runner.timer was off and no ops-request recorded doing it; this converge REMOVED its files ($removed). Roles: $BOSS_NODE_ROLES."
        exit 0
    fi
    echo "install-ops-runner: $HOST does not declare the ops-runner role (roles: $BOSS_NODE_ROLES, source: ${BOSS_NODE_ROLES_SOURCE:-preset}) and a runner is STILL INSTALLED here: $present (under $ETC; boss-ops-runner.timer is-enabled ${t_en:-no answer}, is-active ${t_act:-no answer})." >&2
    echo "    It keeps claiming the ops-request packets filed for $HOST while the estate map, which draws the" >&2
    echo "    declaration, shows no runner here — a second executor on one queue that nothing else reports." >&2
    echo "    This converge does not stop it: an executor is not something a TOML edit may take down, and it is" >&2
    echo "    this host's only protocol door. Declare ops-runner again in infra/estate/estate.toml if it should" >&2
    echo "    stay; otherwise retire it with the bounded verb retire-ops-runner on $HOST (an ops-request whose" >&2
    echo "    plan, plan-retire-ops-runner, is signed by passkey), and this converge then removes the files." >&2
    run_summary_field ops_runner "not in role but still installed: $declared; present: $present; timer is-enabled ${t_en:-no answer}, is-active ${t_act:-no answer}"
    run_summary_note "install-ops-runner: $HOST no longer declares the ops-runner role and STILL RUNS one ($present). Not removed by this converge — it is the host's protocol door and an executor mid-claim; declare the role again or retire it with the verb retire-ops-runner on $HOST. Roles: $BOSS_NODE_ROLES (source: ${BOSS_NODE_ROLES_SOURCE:-preset})."
    exit 0
fi

# The address the unit will read, checked from the same file — see
# above. sor_require exits by itself; `refuse` puts the verdict on the
# packet first.
# shellcheck source=infra/lib/sor.sh
. "$REPO/infra/lib/sor.sh"
[ -n "${BOSS_JOBS_URL:-}" ] \
    || refuse "no system of record: ${BOSS_SOR_ENV:-/etc/boss/sor.env} carries no BOSS_JOBS_URL (the converge renders it from infra/estate/estate.toml)"
JOBS_URL="$BOSS_JOBS_URL"

for ext in service timer; do
    src="$HERE/boss-ops-runner.$ext"
    [ -f "$src" ] || refuse "$src is missing from the tree"
    install -m 0644 "$src" "$ETC/boss-ops-runner.$ext" \
        || refuse "could not install $src to $ETC/boss-ops-runner.$ext"
done

install -d -m 0755 "$ETC/boss-ops-runner.service.d" \
    || refuse "could not create $ETC/boss-ops-runner.service.d"
printf '[Service]\nEnvironment=HOST_ID=%s\nExecStart=\nExecStart=%s/infra/ops/ops-runner.sh\n' \
    "$HOST" "$REPO" >"$ETC/boss-ops-runner.service.d/$HOST.conf" \
    || refuse "could not write the $HOST drop-in, without which the runner has no HOST_ID and refuses every tick"

# THE UNDO OF A RETIREMENT IS THIS INSTALL (design a79a8067): the role is
# declared again, so the runner comes back below, and the marker the
# retire verb left goes first — otherwise a later timer turned off by
# hand would read as RETIRED, crediting a request that is long undone.
if [ -e "$RETIRED_MARKER" ]; then
    if rm -f -- "$RETIRED_MARKER"; then
        echo "install-ops-runner: $HOST declares ops-runner again — the retirement marker $RETIRED_MARKER is removed and the runner reinstalled"
        run_summary_note "install-ops-runner: $HOST declares ops-runner again; its retired runner is reinstalled and the marker $RETIRED_MARKER removed"
    else
        refuse "$RETIRED_MARKER (a retirement this install undoes) could not be removed — it would credit a later stop of the timer to a request this install undid"
    fi
fi

"$SYSTEMCTL" daemon-reload || refuse "systemctl daemon-reload failed"
"$SYSTEMCTL" enable --now boss-ops-runner.timer \
    || refuse "systemctl enable --now boss-ops-runner.timer failed — the unit files are in place but nothing fires them"
echo "install-ops-runner: boss-ops-runner installed for HOST_ID=$HOST from $REPO, reporting to $JOBS_URL"
run_summary_field ops_runner installed
