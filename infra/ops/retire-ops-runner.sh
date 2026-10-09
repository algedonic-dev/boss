#!/usr/bin/env bash
#
# retire-ops-runner — retire THIS host's ops-request runner by stopping and
# disabling exactly boss-ops-runner.timer, from inside the runner pass that
# runs this verb, and nothing else, ever.
#
# WHY IT EXISTS (backlog 98eb9349; design a79a8067, decided by David
# 2026-10-01)
# ---------------------------------------------------------------------
# Drop the `ops-runner` role from a host and its converge stops installing
# a runner there — but the one already present keeps running, keeps
# claiming the ops-request packets filed for that host, and the converge
# says so as STILL INSTALLED on every packet (infra/ops/install-ops-
# runner.sh, backlog 1c7f8f23). That report named a deliberate step the
# tree had no door for, and the only way out was a hand SSH.
#
# THE DOOR CANNOT BE A DIFFERENT EXECUTOR. Every bounded host verb arrives
# as an ops-request drained by the host's own runner, so a verb that stops
# the runner stops the thing running it, and its report would travel
# through the thing it removes. A second host's runner reaching across
# would need SSH authority for every verb it runs and only moves the
# problem one machine over (which host retires the last one?). So the
# runner retires ITSELF, and the cut is behind the run, not under it:
#
#   * The runner is two units. boss-ops-runner.timer (OnUnitActiveSec=1min)
#     is what keeps the door open; boss-ops-runner.service is the
#     Type=oneshot pass the timer fires, and THIS script runs inside one.
#   * Stopping and disabling a timer does not stop a service run it
#     already started: a timer TRIGGERS its service (Triggers=/
#     TriggeredBy=), which is not a dependency that propagates a stop the
#     way Requires=, BindsTo= or PartOf= do, and the service declares none
#     of those on the timer. The timer's stop job is not ordered behind the
#     running pass either: a stop and a start on two ordered units run the
#     stop first, whichever way the ordering points (systemd.unit(5),
#     Before=/After=). So `systemctl disable --now boss-ops-runner.timer`
#     returns while this pass is still running, and the same pass then
#     completes its own execute step through the usual two writes.
#   * THE SERVICE IS NEVER TOUCHED. Stopping it would kill the process
#     that has to report this run. The script names it only to read it,
#     after the stop, so the record shows the pass survived its own cut
#     (crates/core/boss-testing/tests/retire_ops_runner_sh.rs pins that the
#     only systemctl call that acts names the timer).
#
# THE REPORT HAS TWO WITNESSES, and the second owes nothing to the runner.
# This script reads the timer back (is-enabled must answer disabled or
# masked, is-active must answer inactive) before its OK line, which the
# verb file's `effect` judges. And BEFORE it stops anything it writes one
# line to /var/lib/boss/ops-runner.retired naming the request and the
# time, which the host's converge — boss-gcp-converge / forge-converge, on
# their own timers and their own packets — reads: a timer that is off with
# that line is RETIRED, one off without it is DISABLED BY HAND (loud,
# because nothing on the record did it), and in either case the converge
# then removes the unit files and says REMOVED, because the deliberate act
# already stopped the executor and what is left is the mechanical work a
# converge may do. If this pass dies between the stop and its report, the
# runner's execute step stays `active` ("claimed, outcome unknown") and the
# converge settles what actually happened. Silence is not an outcome.
#
# THE BOUNDS, in the order they are applied — each refuses by name, and
# nothing is stopped:
#
#   1. THE ARGUMENTS. `--dry-run`, or `--for-real <plan-sha256>`. The
#      allowlist admits only those shapes; the script re-checks rather
#      than relying on one layer.
#   2. THE REQUEST AND THE HOST. HOST_ID (the runner's drop-in names it)
#      and OPS_REQUEST_ID (the runner hands every verb the request it is
#      running) are required: the marker names the request, and the queue
#      read below uses it as its control. A hand run is not this verb.
#   3. THE ROLE IS ALREADY UNDECLARED, read LIVE through
#      infra/estate/node-roles.sh — the reader the converge uses — and
#      judged by install-ops-runner.sh --in-role, the one predicate for
#      whether a host gets a runner (CLAUDE.md §9a). A host that still
#      declares ops-runner, or declares no roles at all (which installs a
#      runner), is refused: its next converge would `enable --now` the
#      timer within half an hour and the record would lie. A cached or
#      dark read is refused too: a bound that cannot be evaluated is not
#      passed. The live read also refreshes the converge's roles cache, so
#      a dark registry at the converge's next tick falls back to a
#      declaration WITHOUT ops-runner and cannot reinstall what this
#      retired.
#   4. ANOTHER HOST STILL DECLARES ops-runner, from the same registry —
#      and only a host this verb's own file serves counts, because those
#      are the hosts that have a runner installer at all. The estate never
#      retires its last DECLARED door; if that is ever wanted it is a
#      different decision. Declared is all this reads: whether the other
#      runner is ALIVE is not checked, and the plan says so. If it is
#      dead, the recovery needs no SSH — declare ops-runner on this host
#      again and its converge, which owes nothing to any runner,
#      reinstalls it.
#   5. NO OTHER OPEN ops-request TARGETS THIS HOST. A request filed for a
#      host whose door is closing would never be answered, and because of
#      this bound the pass that runs the retire has nothing after it. The
#      read is held to its own control: THIS request must be on it (a
#      denied scope or a wrong instance answers `total: 0` instead of
#      erroring), every row must be this host's, and the rows must be the
#      server's whole `total`.
#   6. ONE UNIT, a literal here and never a param: boss-ops-runner.timer.
#      Already off is refused (nothing to retire; the converge reports
#      what turned it off).
#   7. THE SIGNED PLAN. `--dry-run` prints the plan on stdout and
#      `plan-sha256:` on stderr — the plan verb plan-retire-ops-runner,
#      whose bytes David's passkey signs (design 17835005). `--for-real`
#      re-renders, running every bound again, and stops nothing unless
#      today's plan hashes to the signed one; a second run of an applied
#      plan is refused, because the timer it names is no longer running.
#
# WHAT IT DOES NOT DO. It deletes no file — the converge removes the unit
# files once the timer is off and the role undeclared. It never touches
# boss-ops-runner.service, another host, or infra/gcp/uninstall-not-in-
# role.sh's keep set (the runner's lifecycle belongs to install-ops-
# runner.sh). UNDO NEEDS NO SSH: declare ops-runner again in the estate
# registry, and the next converge installs the files and runs
# `systemctl enable --now boss-ops-runner.timer`, removing the marker.
#
# USAGE
#   retire-ops-runner.sh --dry-run                  every bound, then the plan; stops nothing
#   retire-ops-runner.sh --for-real <plan-sha256>   stop+disable the timer, if the signed plan still holds
#
# EXIT
#   0  done (or, with --dry-run, every bound passed and this is the plan)
#   2  refused — the reason names the bound; nothing stopped
#   1  failed part-way, or a tool this needs could not answer — the record
#      states what was done
#
# ENV (test seams — the ops-runner passes no packet-supplied environment,
# only an argv built from the allowlist, so a packet cannot set these)
#   HOST_ID, OPS_REQUEST_ID   set by the runner (its drop-in; its exec)
#   BOSS_JOBS_URL             the system of record (/etc/boss/sor.env)
#   BOSS_OPS_ACTOR            who the queue read signs as (the runner's own)
#   BOSS_ESTATE_NODES_URL     see infra/estate/node-roles.sh
#   BOSS_NODE_ROLES_CACHE     see infra/estate/node-roles.sh
#   BOSS_OPS_RUNNER_RETIRED   the marker (install-ops-runner.sh reads it)
#   INSTALL_SYSTEMCTL         systemctl (the installer's own knob)

set -uo pipefail

ME="retire-ops-runner"
say() { echo "$ME: $*" >&2; }
refuse() { say "REFUSED — $*"; exit 2; }

SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$SELF_DIR/../.." && pwd)"
SYSTEMCTL="${INSTALL_SYSTEMCTL:-systemctl}"
TIMER="boss-ops-runner.timer"
SERVICE="boss-ops-runner.service"
# The marker the host's converge reads. The SAME line is in
# install-ops-runner.sh, pinned equal by retire_ops_runner_sh.rs (§9a).
RETIRED_MARKER="${BOSS_OPS_RUNNER_RETIRED:-/var/lib/boss/ops-runner.retired}"

# --- bound 1: the arguments --------------------------------------------------
usage() {
    say "usage: $ME --dry-run | --for-real <plan-sha256>"
    say "  --dry-run                  every bound, then the plan on stdout and plan-sha256 on stderr; stops nothing"
    say "  --for-real <plan-sha256>   stop+disable $TIMER, if today's plan still hashes to the signed one"
    exit 2
}
DRY=""
APPROVED=""
case "$#:${1:-}" in
    1:--dry-run) DRY=1 ;;
    2:--for-real)
        DRY=0
        APPROVED="$2"
        [[ "$APPROVED" =~ ^[0-9a-f]{64}$ ]] || { say "--for-real takes a 64-hex plan hash, not \`$APPROVED\`"; usage; }
        ;;
    *) usage ;;
esac

# --- bound 2: the request and the host ---------------------------------------
HOST="${HOST_ID:-}"
[[ "$HOST" =~ ^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$ ]] \
    || refuse "HOST_ID is '${HOST}', not an estate node id — the runner's drop-in names its host, and this verb runs only inside the runner it retires. Nothing was stopped."
REQUEST="${OPS_REQUEST_ID:-}"
[[ "$REQUEST" =~ ^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$ ]] \
    || refuse "OPS_REQUEST_ID is '${REQUEST}', not a request id — the runner hands every verb the request it runs, and the marker and the queue read both need it. Nothing was stopped."
for tool in curl jq "$SYSTEMCTL"; do
    command -v "$tool" >/dev/null 2>&1 || refuse "$tool is not on PATH, so a bound cannot be evaluated. Nothing was stopped."
done

TMP=$(mktemp -d) || { say "FAILED — no working directory"; exit 1; }
trap 'rm -rf "$TMP"' EXIT

# shellcheck source=infra/lib/sor.sh
. "$REPO/infra/lib/sor.sh"
[ -n "${BOSS_JOBS_URL:-}" ] \
    || refuse "BOSS_JOBS_URL is not set (${BOSS_SOR_ENV:-/etc/boss/sor.env} carries none), so the registry and the queue cannot be read and no bound can be evaluated. Nothing was stopped."
BASE="${BOSS_JOBS_URL%/}"

# --- bound 3: the role is already undeclared, read live ------------------------
# shellcheck source=infra/estate/node-roles.sh
. "$REPO/infra/estate/node-roles.sh"
BOSS_CONVERGE_NAME="$ME" read_node_roles "$HOST" > "$TMP/roles.note" 2>&1
sed 's/^/  /' "$TMP/roles.note" >&2
if [ "${BOSS_NODE_ROLES_SOURCE:-}" != "registry" ]; then
    refuse "$HOST's roles did not come from a live read of the estate registry (source: ${BOSS_NODE_ROLES_SOURCE:-none}, roles: ${BOSS_NODE_ROLES:-}), so whether it still declares ops-runner cannot be evaluated. A bound that cannot be evaluated is not passed. Nothing was stopped."
fi
ROLES="${BOSS_NODE_ROLES:-}"
BOSS_NODE_ROLES="$ROLES" bash "$SELF_DIR/install-ops-runner.sh" --in-role
in_role=$?
case "$in_role" in
    0) refuse "$HOST still gets a runner by its declaration (roles: ${ROLES:-none — a host that declares no roles installs one}). Its converge would enable --now $TIMER again within the half hour and this record would lie. Drop ops-runner from $HOST in the estate registry first, then ask again. Nothing was stopped." ;;
    1) ;;
    *) refuse "install-ops-runner.sh --in-role exited $in_role for roles '$ROLES' — the one predicate for whether $HOST gets a runner could not answer. Nothing was stopped." ;;
esac

# --- bound 4: another host still declares ops-runner ---------------------------
# The machine token, PRESENTED and never required (design 6805c764;
# backlog 44b2087e): this read and the queue read below went out with
# x-boss-user alone. One header per URL, because the reader decides the
# host where it is called and BOSS_ESTATE_NODES_URL may name another.
# Made in the script's own shell, after the trap above. No token on the
# host, no lib in this checkout, or a header file that could not be
# written: the variable is empty and the read is the one it was.
# The identity and the URL stay on consecutive lines: that pair is what
# every_estate_read_is_signed.rs reads as this door's evidence.
NODES_MT_HDR=""
MT_HDR=""
if [ -r "$REPO/infra/lib/secret-header.sh" ]; then
    # shellcheck source=infra/lib/secret-header.sh
    . "$REPO/infra/lib/secret-header.sh"
    machine_token_header NODES_MT_HDR "${BOSS_ESTATE_NODES_URL:-$BASE/api/estate/nodes}" || NODES_MT_HDR=""
    machine_token_header MT_HDR "$BASE" || MT_HDR=""
fi
if ! curl -fsS --max-time 15 ${NODES_MT_HDR:+-H "$NODES_MT_HDR"} \
        -H "x-boss-user: $(sor_reader_header "automation:$ME")" \
        "${BOSS_ESTATE_NODES_URL:-$BASE/api/estate/nodes}" > "$TMP/nodes.json" 2> "$TMP/nodes.err"; then
    refuse "the estate registry did not answer the read for the other runners: $(head -c 500 "$TMP/nodes.err" | tr '\n' ' ') — whether $HOST is the last door cannot be evaluated. Nothing was stopped."
fi
# jq_doc_file: on jq-1.6 `jq -e` over NO document exits 0, so the guard
# first asks whether the registry sent one (d96e38ab).
# shellcheck source=infra/lib/jq.sh
. "$REPO/infra/lib/jq.sh"
if ! { jq_doc_file "$TMP/nodes.json" \
        && jq -e --arg h "$HOST" '(.data | type) == "array" and any(.data[]; .id == $h)' "$TMP/nodes.json" > /dev/null 2>&1; }; then
    refuse "the estate registry's node list does not name $HOST (or is not a node list) — the read cannot be trusted to say who else runs a door. Nothing was stopped."
fi
# ONLY A HOST THAT CAN RUN A DOOR COUNTS (adversarial review 9c02f3b0,
# F2): the hosts this verb's own file serves, which are the hosts whose
# converge calls install-ops-runner.sh. A Talos node that declared
# ops-runner by mistake has no installer and no runner, and must not be
# what lets the last real one go. Read from the verb file, never typed
# here (CLAUDE.md §9a).
VERB_FILE="$REPO/infra/ops/verbs/retire-ops-runner.json"
SERVED=$(jq -c '.hosts | if type == "array" and length > 0 and all(.[]; type == "string") then . else error("no hosts") end' "$VERB_FILE" 2> "$TMP/served.err") \
    || refuse "the hosts $VERB_FILE serves could not be read ($(head -c 300 "$TMP/served.err" | tr '\n' ' ')), so which hosts can run a door is unknown. Nothing was stopped."
OTHERS=$(jq -r --arg h "$HOST" --argjson served "$SERVED" '[.data[] | select(.id != $h and (.retired // false) != true
        and (.id as $id | any($served[]; . == $id))
        and any((.roles // [])[]; . == "ops-runner")) | .id] | sort | join(" ")' "$TMP/nodes.json") \
    || refuse "the estate registry's node list could not be read for the other runners. Nothing was stopped."
[ -n "$OTHERS" ] \
    || refuse "no other host this verb serves ($(jq -r 'join(" ")' <<<"$SERVED")) declares ops-runner, so $HOST runs the estate's last door. Retiring it would leave no host able to answer an ops-request — that is a different decision, not this verb. Nothing was stopped."

# --- bound 5: no other open ops-request for this host ---------------------------
ACTOR="${BOSS_OPS_ACTOR:-automation:ops-runner}"
BOSS_USER="{\"id\":\"$ACTOR\",\"role\":\"platform-admin\",\"access_tier\":\"operator\",\"territory_account_ids\":[],\"direct_report_ids\":[],\"department\":\"platform\"}"
host_doc=$(jq -rn --arg h "$HOST" '{host: $h} | tojson | @uri') \
    || refuse "could not encode the host filter. Nothing was stopped."
QUEUE_URL="$BASE/api/jobs?kind=ops-request&status=open&metadata=$host_doc&limit=1000"
if ! curl -fsS --max-time 15 -H "x-boss-user: $BOSS_USER" ${MT_HDR:+-H "$MT_HDR"} \
        "$QUEUE_URL" > "$TMP/queue.json" 2> "$TMP/queue.err"; then
    refuse "the system of record did not answer the queue read ($QUEUE_URL): $(head -c 500 "$TMP/queue.err" | tr '\n' ' ') — whether another request waits on $HOST cannot be evaluated. Nothing was stopped."
fi
jq_doc_file "$TMP/queue.json" \
    || refuse "the queue read answered no JSON document, so whether another request waits on $HOST cannot be evaluated. Nothing was stopped."
QUEUE=$(jq -c --arg h "$HOST" --arg me "$REQUEST" '
    (if type == "object" and has("data") then .data else . end) as $rows
    | {rows: ($rows | length),
       total: (if type == "object" and (.total | type) == "number" then .total else null end),
       foreign: [$rows[] | select((.metadata.host // "") != $h) | .id],
       mine: any($rows[]; .id == $me),
       others: [$rows[] | select(.id != $me) | "\(.id[0:8]) (\(.metadata.verb // "no verb"))"]}' \
    "$TMP/queue.json" 2> "$TMP/queue.jq") \
    || refuse "the queue read did not answer a job list: $(head -c 300 "$TMP/queue.jq") — nothing can be judged from it. Nothing was stopped."
q_rows=$(jq -r .rows <<<"$QUEUE")
q_total=$(jq -r '.total // empty' <<<"$QUEUE")
[ "$(jq -r '.foreign | length' <<<"$QUEUE")" = 0 ] \
    || refuse "the queue read was not narrowed to $HOST (it holds $(jq -r '.foreign | join(", ")' <<<"$QUEUE")), so it is not this host's queue. Nothing was stopped."
case "${q_total:-empty}" in
    empty | *[!0-9]*) refuse "the queue read carried no total, so how much of $HOST's queue it did not hold is unknown. Nothing was stopped." ;;
esac
[ "$q_rows" = "$q_total" ] \
    || refuse "the queue read holds $q_rows of $q_total open requests for $HOST — one page is not the queue. Nothing was stopped."
[ "$(jq -r .mine <<<"$QUEUE")" = true ] \
    || refuse "the queue read does not show this very request ($REQUEST) open for $HOST, so it is not a read of this queue — a denied scope or a wrong instance answers an empty list instead of erroring. Nothing was stopped."
if [ "$(jq -r '.others | length' <<<"$QUEUE")" != 0 ]; then
    refuse "other ops-requests are open for $HOST: $(jq -r '.others | join(", ")' <<<"$QUEUE"). Once the door closes nothing answers them; let them be answered or closed, then ask again. Nothing was stopped."
fi

# --- bound 6: the one unit, still running ------------------------------------
# Read by the WORD: is-enabled exits 1 for `disabled` and is-active exits
# 3 for `inactive`, so the exit cannot be the reading.
t_en=$("$SYSTEMCTL" is-enabled -- "$TIMER" 2> "$TMP/en.err")
t_en="${t_en%%$'\n'*}"
t_act=$("$SYSTEMCTL" is-active -- "$TIMER" 2> "$TMP/act.err")
t_act="${t_act%%$'\n'*}"
if [ -z "$t_en" ] || [ -z "$t_act" ]; then
    say "CANNOT ANSWER — systemctl gave no reading of $TIMER (is-enabled '${t_en}': $(head -c 300 "$TMP/en.err" | tr '\n' ' '); is-active '${t_act}': $(head -c 300 "$TMP/act.err" | tr '\n' ' ')). Nothing was stopped."
    exit 1
fi
case "$t_en:$t_act" in
    disabled:inactive | masked:inactive)
        refuse "$TIMER is already off on $HOST (is-enabled answers $t_en, is-active answers $t_act) — there is nothing to retire, and this pass is not the one that turned it off. The host's converge reports what did. Nothing was stopped." ;;
esac

# --- the plan: deterministic bytes, every bound's reading -------------------
# No clock in the bytes: the plan is rendered once for the passkey and
# again just before the write, and the two must hash the same.
render() {
    echo "retire the ops-request runner on $HOST, for ops-request $REQUEST"
    echo "roles $HOST declares, read live from the estate registry: ${ROLES}"
    echo "  install-ops-runner.sh --in-role: no runner for these roles"
    echo "other hosts declaring ops-runner, among the hosts this verb serves ($(jq -r 'join(" ")' <<<"$SERVED")): $OTHERS"
    echo "  declared, not proven alive: whether that runner answers is not checked; if it does not, declare ops-runner on $HOST again and its converge reinstalls this one"
    echo "open ops-requests for $HOST besides this one: none ($q_total open, this request the only one)"
    echo "$TIMER now: is-enabled answers $t_en, is-active answers $t_act"
    echo "would write $RETIRED_MARKER: ops-request=$REQUEST host=$HOST at=<the UTC time of the run>"
    echo "would stop+disable $TIMER (systemctl disable --now $TIMER)"
    echo "would not touch $SERVICE: the pass running this verb is that service, and it reports this run"
    echo "the unit files stay; $HOST's next converge (install-ops-runner.sh) reads the marker, says RETIRED, removes them and says REMOVED"
}
render > "$TMP/plan"
HASH=$(sha256sum "$TMP/plan" | cut -c1-64)
[[ "$HASH" =~ ^[0-9a-f]{64}$ ]] || { say "FAILED — sha256sum gave no hash of the plan"; exit 1; }

if [ "$DRY" = 1 ]; then
    cat "$TMP/plan"
    say "DRY RUN — every bound passed; would stop+disable $TIMER on $HOST. Nothing was stopped."
    echo "plan-sha256: $HASH" >&2
    exit 0
fi

# --- the write: only the plan that was signed ---------------------------------
if [ "$HASH" != "$APPROVED" ]; then
    cat "$TMP/plan" >&2
    refuse "today's plan (above) hashes to $HASH, not the approved $APPROVED — a reading changed since the plan was signed. Render and approve it again. Nothing was stopped."
fi
cat "$TMP/plan"
say "plan $APPROVED still holds"

# The marker FIRST: the converge's independent witness must be able to
# name this request whatever happens to this pass after the stop.
AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
mkdir -p "$(dirname "$RETIRED_MARKER")" 2> "$TMP/mk.err" \
    || refuse "cannot create $(dirname "$RETIRED_MARKER") for the marker: $(head -c 300 "$TMP/mk.err" | tr '\n' ' ') — nothing was stopped."
printf 'ops-request=%s host=%s at=%s\n' "$REQUEST" "$HOST" "$AT" > "$RETIRED_MARKER.tmp" 2> "$TMP/mk.err" \
    && mv -f "$RETIRED_MARKER.tmp" "$RETIRED_MARKER" 2>> "$TMP/mk.err" \
    || refuse "cannot write the marker $RETIRED_MARKER: $(head -c 300 "$TMP/mk.err" | tr '\n' ' ') — without it the converge would call this DISABLED BY HAND, so nothing was stopped."
echo "wrote $RETIRED_MARKER: $(cat "$RETIRED_MARKER")"

# The one act. `--` so the unit can never be read as an option. Its exit
# is recorded, not acted on yet: a `disable --now` that fails part-way
# may still have disabled or stopped the timer, and what the timer reads
# afterwards — not the exit — decides whether the marker stands.
stop_rc=0
"$SYSTEMCTL" disable --now -- "$TIMER" > "$TMP/stop.out" 2>&1 || stop_rc=$?
if [ "$stop_rc" -ne 0 ]; then
    cat "$TMP/stop.out" >&2
    echo "systemctl disable --now $TIMER exited $stop_rc (above) — reading the timer back to see what took"
else
    echo "stopped+disabled $TIMER"
fi

# --- verified, because exit 0 and off are different claims -------------------
w_en=$("$SYSTEMCTL" is-enabled -- "$TIMER" 2> "$TMP/en.err")
w_en="${w_en%%$'\n'*}"
w_act=$("$SYSTEMCTL" is-active -- "$TIMER" 2> "$TMP/act.err")
w_act="${w_act%%$'\n'*}"
echo "read back: systemctl is-enabled $TIMER answers ${w_en:-nothing}"
echo "read back: systemctl is-active $TIMER answers ${w_act:-nothing}"
# The pass that is running this, read and never acted on: the record shows
# the service run outlived the stop of the timer that fired it.
s_act=$("$SYSTEMCTL" is-active -- "$SERVICE" 2> /dev/null)
s_act="${s_act%%$'\n'*}"
echo "read only: systemctl is-active $SERVICE answers ${s_act:-nothing} — this pass, still running to report"
# WHOSE ACT THE TIMER'S STATE IS (adversarial review 9c02f3b0, F3). The
# marker is what lets the converge credit an off timer to this request
# rather than to a hand, so it follows what TOOK, never the exit alone:
#   * no reading at all             it stands — the converge's own
#                                   reading settles it (CANNOT ANSWER);
#   * is-enabled disabled or masked it stands — the disable is this
#                                   request's act, so if the timer is or
#                                   ends up off the converge says RETIRED
#                                   by this request, agreeing with the
#                                   FAILED this run records when the rest
#                                   did not take;
#   * anything else                 it goes — nothing durable took, and a
#                                   later stop must not be credited here.
did="exited $stop_rc"
[ "$stop_rc" -eq 0 ] && did="returned success"
if [ -z "$w_en" ] || [ -z "$w_act" ]; then
    say "CANNOT ANSWER — disable --now $did but systemctl gave no reading of $TIMER afterwards (is-enabled: $(head -c 300 "$TMP/en.err" | tr '\n' ' '); is-active: $(head -c 300 "$TMP/act.err" | tr '\n' ' ')), so it is not proven off. The marker stands: the converge's own reading settles it."
    exit 1
fi
case "$w_en" in
    disabled | masked) ;;
    *)
        rm -f "$RETIRED_MARKER"
        say "FAILED — disable --now $did and $TIMER still answers is-enabled $w_en, so it would start again at the next boot. The marker was removed, so no later state of the timer is credited to this request; this verb will not try again."
        exit 1
        ;;
esac
if [ "$w_act" != inactive ]; then
    say "FAILED — disable --now $did and $TIMER answers is-enabled $w_en but is-active $w_act, so the door is still open. The marker stands: the disable is this request's act, and the converge credits it if the timer stops."
    exit 1
fi
if [ "$stop_rc" -ne 0 ]; then
    say "FAILED — disable --now exited $stop_rc, yet $TIMER reads off (is-enabled $w_en, is-active inactive). The marker stands, so the converge credits the off timer to ops-request $REQUEST (RETIRED by it), not to a hand; this run is recorded FAILED because its one act did not report success."
    exit 1
fi
say "OK — stopped+disabled boss-ops-runner.timer on $HOST; read back: is-enabled answers $w_en, is-active answers inactive; the marker $RETIRED_MARKER names ops-request $REQUEST. $SERVICE was not touched: this pass is that service, and it reports this run. Its next converge says RETIRED and removes the unit files."
exit 0
