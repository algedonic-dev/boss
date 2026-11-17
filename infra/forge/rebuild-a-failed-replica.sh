#!/usr/bin/env bash
# rebuild-a-failed-replica.sh — delete ONE stuck, non-RW replica of ONE
# Longhorn volume whose every other copy is RW and healthy, so Longhorn
# rebuilds it fresh at the SAME count, through a rendered plan a passkey
# signs. The ops verbs `plan-a-failed-replica-rebuild` (--plan) and
# `rebuild-a-failed-replica`.
#
#   rebuild-a-failed-replica.sh --plan <volume> <replica>
#   rebuild-a-failed-replica.sh <volume> <replica> <plan-sha256>
#
# WHY IT EXISTS (backlog 776bcb59, 2026-11-04). The system of record's
# database volume, boss/pgdata-postgres-0 (Longhorn volume pvc-93e11a6e-…,
# 68 GiB, three replicas), has read robustness DEGRADED since 2026-10-30:
# its cp-2 replica r-1269eb6e sits in mode WO with the engine's rebuild
# status at 100 percent, state complete, not rebuilding, and healthyAt
# empty — it failed on 10-30 and never turned RW — while the other two
# replicas are RW and healthy on distinct nodes. It holds 96.1 GiB actual
# against the 68 GiB volume, and that is what filled cp-2's disk (estate
# alarm d661803b). Every volume door refused the shape: the retirement
# plan by its floor of 3 (3 -> 2, ops d050359b), the purge and move plans
# because the volume is not healthy. David, 2026-11-04: build the
# maintenance protocol and use it rather than a hand command.
#
# WHY A DELETE IS THE WHOLE FIX, as read from longhorn-manager v1.11.3 for
# this car (not pinned by a test here): the volume controller builds no
# replacement while the engine counts a rebuild in progress, and that
# count includes every replica the engine holds in mode WO — so a WO copy
# that never turns RW holds the volume degraded for ever. With that one
# object gone, N-1 replicas stand against a count of N, nothing is
# rebuilding and no failed copy is left to wait for, so Longhorn
# replenishes one. The declared count is NEVER changed by this verb.
#
# THE SHAPE is design 17835005's, as retire-volume-replica's: the verb
# file declares requires_approval and names plan-a-failed-replica-rebuild
# as its plan_verb, so the runner renders this script's --plan onto the
# request's approve step and hands the write sha256 of the SIGNED plan.
# The write re-renders and changes nothing unless today's plan hashes to
# it.
#
# THE BOUND is this script's, never a parameter beyond the two it checks:
#   * the volume is one pvc-<uuid> Longhorn holds in longhorn-system, and
#     the replica is one of ITS replicas, named <volume>-r-<8 hex>;
#   * the volume is attached, served by exactly ONE engine that reads
#     running, and holds exactly spec.numberOfReplicas = N replicas, none
#     being deleted, with N at least 3 — so at least two RW copies serve
#     the volume while the third is rebuilt;
#   * every replica BUT the named one is healthy as Longhorn counts it
#     (healthyAt set, failedAt empty, spec.active true) AND reads RW in the
#     engine's replicaModeMap, on N-1 distinct nodes;
#   * the named replica is the sole copy that is not: its engine mode is
#     not RW (WO, ERR, or absent from the map) and Longhorn does not count
#     it healthy;
#   * nothing is rebuilding now (no rebuildStatus entry of the engine
#     reads isRebuilding true): a WO copy mid-rebuild is a rebuild, not a
#     stall, and deleting it would throw that work away;
#   * the named copy has STALLED: the latest of its engine transition
#     time and its replica's failedAt, lastFailedAt and lastHealthyAt is
#     at least BOSS_REBUILD_STALL_S (default 3600) old. A replica with no
#     such mark has no history to judge a stall by, and is refused;
#   * robustness reads degraded — the state the one non-RW copy implies.
#
# THE PLAN (stdout; sha256 on stderr as `plan-sha256:`, since a hash
# cannot be inside what it hashes) names the volume and its uid, the
# engine, the count N and that it does not change, the replica to delete
# with its uid, node, disk, mode and history marks, the copies that
# remain, every step, and every replica with its uid. No clock reading is
# in the bytes — only that the stall bound was met, which stays true —
# so two renders of one state hash alike. The replicas and their uids are
# IN the bytes, so a second run of an applied plan re-renders to a
# refusal: at most once.
#
# THE WRITE, in this order, each step only when the one before held:
#   1. re-render; refuse (78, nothing written) unless it hashes to the
#      approved plan;
#   2. immediately before the delete, read the volume, its engine and its
#      replicas again and judge them against the plan: the same volume
#      uid, still attached and degraded, the count N, exactly ONE engine
#      and it the one the plan signed (name and uid) reading running,
#      exactly the planned replicas with the same uids, nodes, disks and
#      engine modes, the named one still not RW and not healthy, with the
#      history marks the plan signed (healthyAt, failedAt, lastHealthyAt,
#      lastFailedAt, its engine transition) unchanged and its latest mark
#      still past the stall bound at the render's clock, the others RW and
#      healthy on distinct nodes, nothing rebuilding, nothing deleting.
#      Any difference, or a read that cannot look, stops the run with
#      nothing deleted and names what moved. The engine is held because
#      the modes are its answer: a replacement engine, or the signed one
#      stopped with a cached replicaModeMap, passed this step until review
#      run 9b9e2275 (R1) showed it deleting; the history is held because
#      the stall is what the passkey signed: a fresh failure mark at this
#      read, a rebuild that just restarted, passed it until review run
#      ffd7de14 (R2) showed it deleting;
#   3. `kubectl delete --raw` of the named replica with
#      preconditions.uid set to the uid the plan signed, so a replica
#      recreated under the same name is not the one deleted;
#   4. for BOSS_REBUILD_START_S (default 300) watch the rebuild START:
#      the named replica gone (a terminating one is waited for, never
#      counted gone) and a new replica of the volume listed. A retained
#      copy that goes or stops reading RW and healthy stops the watch
#      loudly — there is nothing further this verb would do;
#   5. read back for BOSS_REBUILD_READBACK_S (default 1200): N replicas,
#      each healthy and RW, on N distinct nodes, the retained ones the
#      same objects, spec.numberOfReplicas still N, robustness healthy.
#      Only then is the line the verb file declares as `effect` printed.
#      A rebuild that started and has not finished in the bound is said
#      so, exit 1: the delete stands and Longhorn goes on rebuilding, and
#      the plan verb or read-volume-replica-history re-measures it.
#
# NO EVIDENCE IS NOT A PASS: a read that could not look is CANNOT ANSWER
# and exit 1, never a plan; a read that fails before the delete stops the
# run with nothing deleted. Exit 78 is a refusal (the request was wrong,
# or is no longer true).
set -uo pipefail

ME=rebuild-a-failed-replica
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# jq-1.6 can accept silence: every guard must first establish a document.
# shellcheck source=infra/lib/jq.sh
. "$HERE/../lib/jq.sh"
# shellcheck source=infra/lib/sor.sh
. "$HERE/../lib/sor.sh"
# shellcheck source=infra/estate/ops-credentials.sh
. "$HERE/../estate/ops-credentials.sh"
# lh_healthy and lh_deleting: Longhorn's own replica predicates, one copy.
# shellcheck source=infra/lib/longhorn-ledger.sh
. "$HERE/../lib/longhorn-ledger.sh"

LH=longhorn-system
UUID_RE='[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}'
VOL_RE="^pvc-${UUID_RE}\$"
REP_RE="^pvc-${UUID_RE}-r-[0-9a-f]{8}\$"

refuse() { echo "$ME: REFUSED — $*" >&2; exit 78; }
fail() { echo "$ME: FAILED — $*" >&2; exit 1; }
unread() { fail "CANNOT ANSWER — $* — an unread volume has no plan, and nothing was written"; }

PLAN=0
if [ "${1-}" = "--plan" ]; then
    PLAN=1
    shift
    [ $# -eq 2 ] || refuse "usage: rebuild-a-failed-replica.sh --plan <volume> <replica> — the plan takes the volume and the replica and nothing else"
else
    [ $# -eq 3 ] || refuse "usage: rebuild-a-failed-replica.sh <volume> <replica> <plan-sha256> — this deletes only under an APPROVED plan, and the hash is what the approval signed. Render one with --plan (the plan-a-failed-replica-rebuild verb)"
    [[ "$3" =~ ^[0-9a-f]{64}$ ]] || refuse "the plan hash must be 64 hex characters, got '$3'"
    APPROVED="$3"
fi
VOL="$1"
REP="$2"
[[ "$VOL" =~ $VOL_RE ]] || refuse "the volume must be one Longhorn volume name, pvc-<uuid> in lower case, got '$VOL'"
[[ "$REP" =~ $REP_RE ]] || refuse "the replica must be one Longhorn replica name, <volume>-r-<8 hex> in lower case, got '$REP'"
[ "${REP%-r-*}" = "$VOL" ] || refuse "the replica $REP is named for volume ${REP%-r-*}, not $VOL"

STALL_S="${BOSS_REBUILD_STALL_S:-3600}"
START_S="${BOSS_REBUILD_START_S:-300}"
READBACK_S="${BOSS_REBUILD_READBACK_S:-1200}"
POLL_S="${BOSS_REBUILD_POLL_S:-15}"
case "${STALL_S:-empty}${START_S:-empty}${READBACK_S:-empty}${POLL_S:-empty}" in
    *[!0-9]*) fail "BOSS_REBUILD_STALL_S, BOSS_REBUILD_START_S, BOSS_REBUILD_READBACK_S and BOSS_REBUILD_POLL_S must be whole seconds, got '$STALL_S', '$START_S', '$READBACK_S' and '$POLL_S'" ;;
esac

command -v jq >/dev/null 2>&1 || fail "jq is not on PATH — Longhorn's answer cannot be read, and no evidence is not a plan"

WORK="$(mktemp -d)" || fail "cannot make a scratch directory"
trap 'rm -rf "$WORK"' EXIT

# The kubectl image, present and pinned (a bounded pull, named when it
# fails), before any read.
if ! ready="$(ops_image_ready 2> "$WORK/door.err")"; then
    fail "the kubectl door cannot run: $(tr '\n' ' ' < "$WORK/door.err")"
fi
echo "$ME: kubectl image $ready" >&2
K="$(ops_kubectl)"

# A Kubernetes response is ONE typed document for the requested subject.
typed_doc_file() {
    jq_doc_file "$1" && jq -e -s --arg shape "$2" --arg vol "$VOL" '
        length == 1 and (.[0] | type == "object" and
            if $shape == "list" then
                (.items | type == "array" and all(.[]; type == "object"))
            elif $shape == "volume" then
                .metadata.name == $vol and
                (.spec | type == "object") and (.status | type == "object")
            else true end)
    ' "$1" >/dev/null
}

# read_all <prefix> — the volume, the replicas and the engines into
# $WORK/<prefix>-{vol,reps,engs}.json. Returns 1 with the reason in
# $WORK/<prefix>.why when a read could not look, 2 when the volume is not
# there; the caller decides whether that is a refusal or a stop.
read_all() {
    local p="$1" r res file shape
    : > "$WORK/$p.why"
    for r in "volumes.longhorn.io $VOL:vol:volume" "replicas.longhorn.io:reps:list" "engines.longhorn.io:engs:list"; do
        res="${r%%:*}"
        file="$WORK/$p-$(echo "$r" | cut -d: -f2).json"
        shape="${r##*:}"
        # shellcheck disable=SC2086 # K and res are words the door and this list print
        if ! $K get $res -n "$LH" -o json --request-timeout=30s > "$file" 2> "$WORK/$p.err"; then
            if [ "$shape" = volume ] && grep -q NotFound "$WORK/$p.err"; then
                echo "volumes.longhorn.io $VOL is not there ($(tr '\n' ' ' < "$WORK/$p.err"))" > "$WORK/$p.why"
                return 2
            fi
            echo "$res: $(tr '\n' ' ' < "$WORK/$p.err" | cut -c1-300)" > "$WORK/$p.why"
            return 1
        fi
        if ! typed_doc_file "$file" "$shape" 2>> "$WORK/$p.err"; then
            echo "$res did not read back as one $shape (its first 200 bytes: '$(head -c 200 "$file" | tr '\n' ' ')')" > "$WORK/$p.why"
            return 1
        fi
    done
}

# facts <prefix> — one JSON object judged from the reads, into
# $WORK/<prefix>.json. The plan and every later read judge by this one
# program, so the write compares like with like.
FACTS_JQ='
def ts: if type == "string" and length > 0
        then (sub("\\.[0-9]+Z$"; "Z") | (try fromdateiso8601 catch null)) else null end;
def mode_in($m): $m[.metadata.name] // "absent";
def state_of: if lh_deleting then "being deleted" elif lh_failed then "failed"
              elif lh_healthy then "healthy"
              elif (.spec.healthyAt // "") != "" and lh_inactive then "healthy but not active"
              else "not healthy (healthyAt empty)" end;
$v[0] as $V
| [$reps[0].items[] | select(.spec.volumeName == $vol)] | sort_by(.metadata.name) as $mine
| [$engs[0].items[] | select(.spec.volumeName == $vol)] as $E
| (if ($E | length) == 1 then $E[0] else null end) as $e
| (if $e == null then {} else ($e.status.replicaModeMap // {}) end) as $modes
| (if $e == null then {} else ($e.status.replicaTransitionTimeMap // {}) end) as $tt
| ([$mine[] | select(.metadata.name == $rep)] | .[0] // null) as $R
| [$mine[] | {name: .metadata.name, uid: (.metadata.uid // ""), node: (.spec.nodeID // ""),
              disk: (.spec.diskID // ""), mode: mode_in($modes), healthy: lh_healthy,
              deleting: lh_deleting, state: state_of}] as $all
| [$all[] | select(.name != $rep)] as $keep
| [$keep[] | select(.mode == "RW" and .healthy)] as $ok
| (if $R == null then null else
     ([$tt[$rep], $R.spec.failedAt, $R.spec.lastFailedAt, $R.spec.lastHealthyAt]
      | map(select(type == "string" and length > 0)) | max) end) as $since
| {vuid: ($V.metadata.uid // ""),
   size: ($V.spec.size // "unreported"),
   current: ($V.spec.numberOfReplicas // null),
   claim: (($V.status.kubernetesStatus // {}) as $k
           | if ($k.pvcName // "") != "" then "\($k.namespace)/\($k.pvcName)" else "none" end),
   state: ($V.status.state // "unreported"),
   on: ($V.status.currentNodeID // ""),
   robustness: ($V.status.robustness // "unreported"),
   engines: ($E | length),
   engine: (if $e == null then null
            else {name: $e.metadata.name, uid: ($e.metadata.uid // ""),
                  state: ($e.status.currentState // "unreported")} end),
   rebuilding: [($e // {}) | (.status.rebuildStatus // {}) | to_entries[]
                | select(.value.isRebuilding == true) | .key],
   n: ($all | length),
   all: $all,
   assigned: all($all[]; .uid != "" and .node != "" and .disk != ""),
   deleting: [$all[] | select(.deleting) | .name],
   keep: [$keep[] | {name, uid, node, disk}],
   keep_ok: ($ok | length),
   keep_distinct: ([$ok[] | .node] | unique | length),
   keep_nodes: ([$keep[] | .node] | sort),
   keep_bad: [$keep[] | select((.mode == "RW" and .healthy) | not)
              | "\(.name) on \(.node) (mode \(.mode), \(.state))"],
   replicas: [$all[] | "replica \(.name) (uid \(.uid)) on \(.node) (disk \(.disk)): mode \(.mode), \(.state)\(if .name == $rep then " — TO REBUILD" else "" end)"],
   rep: (if $R == null then null else
         ($all[] | select(.name == $rep)) + {
           healthy_at: ($R.spec.healthyAt // ""), failed_at: ($R.spec.failedAt // ""),
           last_healthy_at: ($R.spec.lastHealthyAt // ""), last_failed_at: ($R.spec.lastFailedAt // ""),
           transition: ($tt[$rep] // ""),
           since: $since,
           age: (($since | ts) as $s | if $s == null then null else $now - $s end)} end)}
'
facts() {
    jq -n --arg vol "$VOL" --arg rep "$REP" --argjson now "$NOW" \
        --slurpfile v "$WORK/$1-vol.json" \
        --slurpfile reps "$WORK/$1-reps.json" \
        --slurpfile engs "$WORK/$1-engs.json" \
        "$LONGHORN_LEDGER_JQ$FACTS_JQ" > "$WORK/$1.json" 2> "$WORK/$1.err"
}

# render — read Longhorn now, apply the bounds, and write the plan to
# $WORK/plan. Sets N and REP_UID. Nothing in the bytes is a clock.
render() {
    NOW="$(date -u +%s)"
    case "${NOW:-empty}" in *[!0-9]*) fail "the clock did not read as whole seconds ('$NOW')" ;; esac
    read_all plan
    case $? in
        0) ;;
        2) refuse "$(cat "$WORK/plan.why")" ;;
        *) unread "$(cat "$WORK/plan.why")" ;;
    esac
    facts plan || unread "Longhorn's answer would not parse: $(tr '\n' ' ' < "$WORK/plan.err")"
    local f="$WORK/plan.json" rob
    q() { jq -r "$1" "$f"; }
    N="$(q '.current // "none"')"
    case "$N" in '' | *[!0-9]*) unread "volume $VOL reports no spec.numberOfReplicas ('$N')" ;; esac
    [ "$(q '.rep != null')" = true ] \
        || refuse "$REP is not a replica of $VOL — its replicas are: $(q '.replicas | join("; ")')"
    [ "$(q .assigned)" = true ] \
        || unread "a replica of $VOL reports no uid, node or disk: $(q '.replicas | join("; ")')"
    [ "$(q .state)" = attached ] \
        || refuse "$VOL is $(q .state), not attached — only an attached volume's engine reports the modes this rebuild stands on"
    [ "$(q .engines)" = 1 ] \
        || refuse "$VOL has $(q .engines) engines — exactly one engine must say which copies are RW (two is a live migration, none is detached)"
    [ "$(q .engine.state)" = running ] \
        || refuse "$VOL's engine $(q .engine.name) reads $(q .engine.state), not running — its replica modes are not a current answer"
    [ "$(q .n)" = "$N" ] \
        || refuse "$VOL declares spec.numberOfReplicas $N and has $(q .n) replica(s) — a count its replicas do not match is not the stuck-copy shape this verb rebuilds: $(q '.replicas | join("; ")')"
    [ "$(q '.deleting | length')" -eq 0 ] \
        || refuse "$(q '.deleting | join(", ")') is being deleted already — a volume mid-removal is no volume to remove from: $(q '.replicas | join("; ")')"
    [ "$N" -ge 3 ] \
        || refuse "$VOL declares $N replicas — deleting one would leave fewer than two RW copies serving it while the replacement builds; this verb needs at least 3"
    [ "$(q .rep.mode)" != RW ] \
        || refuse "$REP reads RW in the engine — it serves the volume; this verb rebuilds only a copy that is not RW: $(q '.replicas | join("; ")')"
    [ "$(q .rep.healthy)" = false ] \
        || refuse "Longhorn counts $REP healthy (healthyAt set, failedAt empty, active) — it is not the failed copy: $(q '.replicas | join("; ")')"
    if [ "$(q .keep_ok)" != "$((N - 1))" ] || [ "$(q .keep_distinct)" != "$((N - 1))" ]; then
        refuse "every replica of $VOL but $REP must read RW and healthy, one per node, or $REP is not the sole failed copy and deleting it costs more than it rebuilds — $(q .keep_ok) of $((N - 1)) do, on $(q .keep_distinct) distinct node(s); not RW and healthy: $(q '.keep_bad | if . == [] then "none (two share a node)" else join("; ") end')"
    fi
    [ "$(q '.rebuilding | length')" -eq 0 ] \
        || refuse "the engine reads a rebuild in progress ($(q '.rebuilding | join(", ")')) — a copy being rebuilt is not stalled, and deleting it throws that work away"
    rob="$(q .robustness)"
    [ "$rob" = degraded ] \
        || refuse "$VOL reads robustness $rob — one non-RW copy beside RW ones reads degraded, so this is not the state the plan was written for"
    [ "$(q '.rep.since // ""')" != "" ] \
        || refuse "$REP has no engine transition time and no failedAt, lastFailedAt or lastHealthyAt — no history to judge a stall by"
    local age
    age="$(q '.rep.age // "none"')"
    case "$age" in '' | *[!0-9-]*) unread "$REP's latest mark '$(q .rep.since)' did not read as a time" ;; esac
    [ "$age" -ge "$STALL_S" ] \
        || refuse "$REP has been in mode $(q .rep.mode) only since $(q .rep.since), ${age}s ago — under the stall bound of ${STALL_S}s (BOSS_REBUILD_STALL_S); a rebuild may still finish by itself"
    REP_UID="$(q .rep.uid)"
    {
        echo "plan: rebuild-a-failed-replica"
        echo "volume: $VOL (uid $(q .vuid))"
        echo "claim: $(q .claim)"
        echo "size: $(q .size) bytes"
        echo "state: attached$( [ -n "$(q .on)" ] && echo " on $(q .on)")"
        echo "robustness: degraded"
        echo "engine: $(q .engine.name) (uid $(q .engine.uid)), running, no rebuild in progress"
        echo "numberOfReplicas: $N -> $N — unchanged: this verb never changes the declared count"
        echo "rebuild: replica $REP (uid $REP_UID) on $(q .rep.node) (disk $(q .rep.disk)): mode $(q .rep.mode), $(q .rep.state) — healthyAt '$(q .rep.healthy_at)', failedAt '$(q .rep.failed_at)', lastHealthyAt '$(q .rep.last_healthy_at)', lastFailedAt '$(q .rep.last_failed_at)', engine transition '$(q .rep.transition)'"
        echo "stalled: not RW since $(q .rep.since) at the latest, past the stall bound of ${STALL_S}s"
        echo "remain: $((N - 1)) RW healthy replicas on $((N - 1)) distinct nodes ($(q '.keep_nodes | join(", ")'))"
        echo "step 1: re-render this plan; nothing is written unless it hashes to the approved one"
        echo "step 2: immediately before the delete, re-read the volume, its engine and its replicas: volume uid $(q .vuid) still attached and degraded, spec.numberOfReplicas $N, the one engine still $(q .engine.name) (uid $(q .engine.uid)) and running, exactly the replicas below with their uids, nodes, disks and modes, $REP still not RW and not healthy with its history marks as signed above and still past the stall bound of ${STALL_S}s, every other RW and healthy on distinct nodes, no rebuild in progress, none deleting — any difference or unread answer stops the run with nothing deleted"
        echo "step 3: kubectl delete --raw /apis/longhorn.io/v1beta2/namespaces/$LH/replicas/$REP with preconditions.uid $REP_UID — that object and no other; the count stays $N, so Longhorn builds one replacement"
        echo "step 4: watch for up to ${START_S}s: $REP gone and one new replica of $VOL listed, the retained replicas still RW and healthy"
        echo "step 5: read back for up to ${READBACK_S}s: $N replicas, each healthy and RW, on $N distinct nodes, the retained replicas the same objects, spec.numberOfReplicas $N, robustness healthy"
        echo "placement: which node and disk the replacement lands on is Longhorn's scheduler's choice, not this plan's"
        echo
        echo "== replicas of $VOL ($(q .n)) =="
        q '.replicas[]'
    } > "$WORK/plan"
    cp "$f" "$WORK/planned.json"
}

render
HASH="$(sha256sum "$WORK/plan" | cut -d' ' -f1)"

if [ "$PLAN" -eq 1 ]; then
    cat "$WORK/plan"
    echo "plan-sha256: $HASH" >&2
    exit 0
fi

# --- the write --------------------------------------------------------------
if [ "$HASH" != "$APPROVED" ]; then
    cat "$WORK/plan" >&2
    refuse "today's plan for $VOL (above) hashes to $HASH, not the approved plan $APPROVED — its replicas, their modes or uids, the engine or the count moved since the plan was signed (or this plan already ran). Nothing was changed; render and approve it again"
fi
cat "$WORK/plan"
echo
echo "$ME: plan $APPROVED still holds — deleting replica $REP of $VOL so Longhorn rebuilds it: numberOfReplicas $N unchanged"

# observe <prefix> — read and judge now against the plan into
# $WORK/<prefix>-judged.json. Returns 1 with the reason in
# $WORK/<prefix>.why when the reads or the judgement could not look.
observe() {
    read_all "$1" || return 1
    if ! facts "$1"; then
        echo "Longhorn's answer would not parse: $(tr '\n' ' ' < "$WORK/$1.err" | cut -c1-200)" > "$WORK/$1.why"
        return 1
    fi
    if ! jq --slurpfile p "$WORK/planned.json" --argjson stall "$STALL_S" '
        def eng: if . == null then "no single engine"
                 else "engine \(.name) (uid \(.uid)) \(.state)" end;
        def reps: map("\(.name) (uid \(.uid)) on \(.node) disk \(.disk) mode \(.mode)") | join("; ");
        def shown: if . == "" then "empty" else . end;
        $p[0] as $p
        | ($p.keep | map(.uid)) as $kept
        | ([.all[] | .uid]) as $uids
        | [.all[] | select(.uid as $u | ($kept | index($u)) != null)] as $still
        | [.all[] | select(.uid as $u | ($p.all | map(.uid) | index($u)) == null)] as $new
        | {vuid_same: (.vuid == $p.vuid),
           spec: .current,
           robustness: .robustness,
           rep_gone: (($uids | index($p.rep.uid)) == null),
           kept_all: (($still | length) == ($kept | length)),
           kept_same: (([$still[] | {name, uid, node, disk}] | sort_by(.uid))
                       == ($p.keep | sort_by(.uid))),
           kept_unwell: [$still[] | select(.mode != "RW" or (.healthy | not) or .deleting)
                         | "\(.name) on \(.node) (mode \(.mode), \(.state))"],
           kept_gone: [$p.keep[] | select(.uid as $u | ($uids | index($u)) == null) | .name],
           new: [$new[] | {name, node, mode, healthy, state}],
           n: (.all | length),
           all_ok: all(.all[]; .mode == "RW" and .healthy and (.deleting | not)),
           distinct: ([.all[] | .node] | unique | length),
           nodes: ([.all[] | .node] | unique),
           names: [.all[] | "\(.name) on \(.node) (mode \(.mode), \(.state))"],
           # The plan exactly as signed, re-judged: what step 2 asks. Each
           # fact that moved is named, so a stop says what it stopped on.
           # The ENGINE is held to the one the plan signed — its name, its
           # uid and running — because the replica modes the plan stands
           # on are the answer of that engine: a replacement engine, or
           # the signed one stopped with its last replicaModeMap still
           # cached, is not the basis the passkey approved (review run
           # 9b9e2275, R1). The modes themselves are held as signed, and
           # so are the attached state and the robustness of the volume.
           drift: [
             (if .vuid != $p.vuid then "volume uid \(.vuid), signed \($p.vuid)" else empty end),
             (if .state != $p.state then "volume \(.state), signed \($p.state)" else empty end),
             (if .robustness != $p.robustness then "robustness \(.robustness), signed \($p.robustness)" else empty end),
             (if .current != $p.current then "spec.numberOfReplicas \(.current), signed \($p.current)" else empty end),
             (if .engines != 1 then "\(.engines) engines, signed exactly one" else empty end),
             (if .engine != $p.engine or (.engine.state // "") != "running"
              then "\(.engine | eng), signed \($p.engine | eng)" else empty end),
             (if ([.all[] | {name, uid, node, disk, mode}] != [$p.all[] | {name, uid, node, disk, mode}])
              then "replicas \(.all | reps), signed \($p.all | reps)" else empty end),
             (if (.rep.mode // "RW") == "RW" or (.rep.healthy // false)
              then "\($p.rep.name) no longer reads not-RW and not healthy (mode \(.rep.mode // "absent"), \(.rep.state // "not listed"))" else empty end),
             (if .keep_ok != ($p.current - 1) or .keep_distinct != ($p.current - 1)
              then "retained RW and healthy: \(.keep_ok) on \(.keep_distinct) distinct node(s), signed \($p.current - 1)" else empty end),
             (if (.rebuilding | length) > 0 then "rebuild in progress (\(.rebuilding | join(", ")))" else empty end),
             (if (.deleting | length) > 0 then "being deleted: \(.deleting | join(", "))" else empty end),
             # The HISTORY of the named copy as the plan signed it, and the
             # stall bound the plan says was met, re-applied at the same
             # clock. Review run ffd7de14 (R2): a fresh lastFailedAt landing
             # only at this read (a rebuild that just restarted) moved none
             # of the facts above, and the copy was deleted under a stall
             # the passkey signed and the record no longer held.
             ((.rep // {}) as $r | $p.rep as $s
              | [["healthyAt", "healthy_at"], ["failedAt", "failed_at"],
                 ["lastHealthyAt", "last_healthy_at"], ["lastFailedAt", "last_failed_at"],
                 ["engine transition", "transition"]][]
              | . as [$field, $k]
              | ($r[$k] // "") as $mark
              | if $mark != $s[$k]
                then "\($s.name) \($field) \($mark | shown), signed \($s[$k] | shown)" else empty end),
             (if (.rep.age // null) == null or .rep.age < $stall
              then "\($p.rep.name) latest mark \(.rep.since // "none") is \(.rep.age // "no")s old, under the stall bound of \($stall)s the plan signed as met"
              else empty end)
           ]}
        | .as_planned = (.drift == [])
        ' "$WORK/$1.json" > "$WORK/$1-judged.json" 2> "$WORK/$1.err" \
        || ! jq_doc_file "$WORK/$1-judged.json"; then
        echo "the judgement would not compute: $(tr '\n' ' ' < "$WORK/$1.err" | cut -c1-200)" > "$WORK/$1.why"
        return 1
    fi
}
j() { jq -r "$1" "$WORK/$2-judged.json"; }
none() { j "$1 | if type == \"array\" then (map(tostring) | join(\", \")) else tostring end | if . == \"\" then \"none\" else . end" "$2"; }

# --- step 2: the identities, straight before the delete ----------------------
observe pre || fail "the read straight before the delete could not look ($(cat "$WORK/pre.why")) — $REP is NOT deleted and nothing was written. Read the volume again with plan-a-failed-replica-rebuild"
[ "$(j .as_planned pre)" = true ] \
    || fail "between the plan's read and the delete, $VOL moved off the signed plan: $(j '.drift | join("; ")' pre) — $REP is NOT deleted and nothing was written. Read the volume again with plan-a-failed-replica-rebuild"

# --- step 3: delete exactly the planned object -------------------------------
K_IN="$(ops_kubectl --stdin)"
jq -cn --arg uid "$REP_UID" '{apiVersion: "v1", kind: "DeleteOptions", preconditions: {uid: $uid}}' > "$WORK/delete-options.json" \
    || fail "the delete options would not render — $REP is NOT deleted"
# shellcheck disable=SC2086 # K_IN is one line the door prints to be word-split
if ! $K_IN delete --raw "/apis/longhorn.io/v1beta2/namespaces/$LH/replicas/$REP" -f - --request-timeout=20s \
    < "$WORK/delete-options.json" > "$WORK/delete.out" 2> "$WORK/delete.err"; then
    sed 's/^/    /' "$WORK/delete.err" >&2
    fail "kubectl delete of $REP (preconditions.uid $REP_UID) did not succeed (its words above) — spec.numberOfReplicas is $N and nothing else was written; read the volume again with plan-a-failed-replica-rebuild"
fi
echo "$ME: deleted $REP (uid $REP_UID) — watching up to ${START_S}s for it to go and for Longhorn to start one replacement"

# --- step 4: the rebuild starts ------------------------------------------------
start="$(date -u +%s)"
while :; do
    if observe start; then
        if [ "$(j '.kept_gone | length' start)" -gt 0 ] || [ "$(j '.kept_unwell | length' start)" -gt 0 ]; then
            fail "after $REP was deleted, a retained copy of $VOL went or stopped reading RW and healthy (gone: $(none .kept_gone start); not RW and healthy: $(none .kept_unwell start)) — replicas now $(none .names start). Nothing more is done; read the volume with plan-a-failed-replica-rebuild or read-volume-replica-history"
        fi
        if [ "$(j .rep_gone start)" = true ] && [ "$(j '.new | length' start)" -ge 1 ]; then
            echo "$ME: Longhorn is rebuilding $VOL: $REP is gone and $(j '[.new[] | "\(.name) on \(.node) (mode \(.mode))"] | join(", ")' start) is listed — reading back for up to ${READBACK_S}s"
            break
        fi
        why="$REP $(if [ "$(j .rep_gone start)" = true ]; then echo gone; else echo "still listed"; fi), new replicas: $(none '[.new[] | .name]' start); replicas now $(none .names start)"
    else
        why="$(cat "$WORK/start.why")"
    fi
    elapsed=$(($(date -u +%s) - start))
    if [ "$elapsed" -ge "$START_S" ]; then
        fail "$REP was deleted, but the rebuild did NOT start in ${elapsed}s: $why. spec.numberOfReplicas is $N; read the volume with plan-a-failed-replica-rebuild or read-volume-replica-history"
    fi
    sleep "$POLL_S"
done

# --- step 5: read back ----------------------------------------------------------
start="$(date -u +%s)"
while :; do
    if observe back; then
        if [ "$(j .vuid_same back)" = true ] && [ "$(j .rep_gone back)" = true ] \
            && [ "$(j .kept_all back)" = true ] && [ "$(j .kept_same back)" = true ] \
            && [ "$(j '.kept_unwell | length' back)" = 0 ] && [ "$(j '.new | length' back)" = 1 ] \
            && [ "$(j .n back)" = "$N" ] && [ "$(j .spec back)" = "$N" ] \
            && [ "$(j .all_ok back)" = true ] && [ "$(j .distinct back)" = "$N" ] \
            && [ "$(j .robustness back)" = healthy ]; then
            echo "$ME: $VOL has $N healthy RW replicas on $N distinct nodes ($(j '.nodes | join(", ")' back)) — $REP is gone, rebuilt as $(j '.new[0].name' back) on $(j '.new[0].node' back) — read back: spec.numberOfReplicas=$N unchanged, robustness healthy (after $(($(date -u +%s) - start))s)"
            exit 0
        fi
        why="spec.numberOfReplicas $(j .spec back), robustness $(j .robustness back), replicas $(none .names back); retained not RW and healthy: $(none .kept_unwell back); retained gone: $(none .kept_gone back)"
    else
        why="$(cat "$WORK/back.why")"
    fi
    elapsed=$(($(date -u +%s) - start))
    if [ "$elapsed" -ge "$READBACK_S" ]; then
        fail "the rebuild of $VOL started but is NOT proven after ${elapsed}s: $why. The delete stands and Longhorn goes on rebuilding; re-measure with plan-a-failed-replica-rebuild or read-volume-replica-history"
    fi
    sleep "$POLL_S"
done
