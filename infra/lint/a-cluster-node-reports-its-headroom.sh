#!/usr/bin/env bash
# a-cluster-node-reports-its-headroom.sh — the cluster observer records
# FREE space per node, not only capacity, and losing that read costs the
# free figure alone rather than the whole observation — and it does so
# WITHOUT `nodes/proxy`.
#
# WHY. Measured 2026-09-10 (packet a520737f): the kubernetes-nodes
# observation carried `{disk_gb: 929}` for w-1 and no `disk_free_gb` on
# any node, so `estate.compare`'s disk floor — free below 16 GiB or
# below 35% of capacity — could never fire for cp-1, cp-2, cp-3 or w-1.
# w-1 is THE BUILD NODE: every gate's warm target lives on its nodefs
# and every gate Job prefers it by affinity. Forge disk exhaustion has
# cost this pipeline four clean cars over five departures (2026-08-22)
# and a full hold day (2026-09-05); the same failure on w-1 would have
# had no alarm at all, because the floor's numerator was never recorded.
#
# AND WHY THE GRANT IS REFUSED (backlog eeac3d56, 2026-09-29). The first
# fix read the kubelet's /stats/summary through the API server, which
# needed `get nodes/proxy` — and the kubelet authorizes a WebSocket exec,
# attach or port-forward opened as a GET as `get nodes/proxy` too, so the
# observer's token could exec into any pod, the boss pod holding the
# credential broker's root tokens included. The figure now comes from
# the forge, through the Talos API (infra/estate/observe-nodefs.sh), as a
# `talos-nodefs` observation the in-cluster observer carries across.
# This check used to REQUIRE nodes/proxy; it now refuses it.
#
# WHAT IT CHECKS, by RUNNING the observer rather than reading it. The
# observer's shell lives inline in a Kubernetes manifest — it has to,
# because the alpine/k8s image it runs in carries none of this repo's
# scripts (wiring in the shared boss-api-curl.sh once broke it with exit
# 127 and no observation landed at all). Inline shell that nothing
# executes is untested shell, so this extracts the `args:` block scalar
# and runs it under stub `kubectl` and `curl`:
#
#   1. every observed node the forge measured carries its `disk_free_gb`
#      from the NEWEST talos-nodefs reading — chosen by its observed_at,
#      not by its place on the page — and the observation names that
#      reading as `disk_free_source`;
#   2. `disk_gb` still comes from status.capacity.ephemeral-storage, and
#      a Talos total that does not match it is REFUSED: the floor's
#      numerator and denominator must describe ONE filesystem, the
#      kubelet's nodefs, and a free figure from another is no reading;
#   3. a node the forge could not read, and every node when the newest
#      reading is too old, still appears, with `disk_free_gb: null`,
#      every other field intact and the reason on stdout, and the run
#      still posts and exits 0 — `estate.compare` turns the null into
#      `disk_unmeasured`, so the blindness is visible rather than
#      indistinguishable from a healthy estate;
#   4. the ClusterRole grants NO nodes/proxy, and the script never asks
#      the API server for a node subresource (`kubectl get --raw`): the
#      grant and the call that needed it leave together.
set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
manifest="$here/../cluster/manifests/boss-estate-observe.yaml"
[[ -f "$manifest" ]] || { echo "a-cluster-node-reports-its-headroom: missing $manifest" >&2; exit 1; }
command -v jq >/dev/null 2>&1 || { echo "a-cluster-node-reports-its-headroom: no jq — the observer is sh + jq, so this check cannot run without it" >&2; exit 1; }

fail() { echo "a-cluster-node-reports-its-headroom: FAIL — $*" >&2; exit 1; }

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# ----- 4: no nodes/proxy, read from the same file -----
# Comment lines are skipped: the header names the grant to say why it is
# gone, and that sentence is not a rule.
granted="$(awk '!/^[ \t]*#/ && /nodes\/proxy/ { printf "    line %d: %s\n", FNR, $0 }' "$manifest")"
[[ -z "$granted" ]] || { printf '%s\n' "$granted" >&2; fail "the manifest grants nodes/proxy (above) — the kubelet authorizes a WebSocket exec, attach or port-forward opened as a GET as that grant, so it is exec into any pod on any node (backlog eeac3d56). The free figure comes from the forge's talos-nodefs reading; nothing here needs it"; }

# ----- extract the observer's shell out of the block scalar -----
# Depth + exact text, never a `{n}` interval: mawk (the awk in the CI
# image) does not support intervals and silently matches nothing, which
# is a scraper that reads as "the file changed".
awk '
    { match($0, /^ */); ind = RLENGTH; body = substr($0, ind + 1) }
    ind == 14 && body == "args:"          { inargs = 1; next }
    inargs && ind == 16 && body == "- |"  { inblock = 1; inargs = 0; next }
    inblock {
        if ($0 ~ /^[ \t]*$/) { print ""; next }
        if (ind < 18) { inblock = 0; next }
        print substr($0, 19)
    }
' "$manifest" >"$tmp/observe.sh"
[[ -s "$tmp/observe.sh" ]] || fail "could not extract the args: block scalar from $manifest (indentation changed?)"
grep -q 'estate/observation' "$tmp/observe.sh" \
    || fail "the extracted block does not post an observation — the scraper found the wrong block"

# ----- the block must write only where its caller says --------------
# A FACT THAT LIVES TWICE (CLAUDE.md §9a): the manifest spells the
# scratch directory `${BOSS_OBSERVE_WORK:-/tmp}` and this script supplies
# the value. Nothing else makes the pair hold, so it is asserted here,
# against the extracted text, on every run.
#
# WHY IT MATTERS HERE AND NOT IN THE POD. In the pod /tmp is the
# container's own writable layer and the observer owns every byte of it.
# This script runs the SAME text on the dev pod, which is long-lived and
# shared: measured 2026-09-11 (packet 5bf96e72), root ran the roster and
# a second uid's run then died writing root's `/tmp/nodes.json`
# — reported, wrongly, as the observer breaking its best-effort
# contract. Comment lines are skipped; any other fixed /tmp path in the
# block is named with its line.
stray="$(awk '
    !/^[ \t]*#/ && /\/tmp/ && !/BOSS_OBSERVE_WORK:-\/tmp/ { printf "    line %d: %s\n", FNR, $0 }
' "$tmp/observe.sh")"
[[ -z "$stray" ]] || { printf '%s\n' "$stray" >&2; fail "the observer's shell names a fixed path under /tmp (above). Every scratch file must hang off \$WORK, which defaults to /tmp for the pod and is set by THIS script to a directory it owns — otherwise two uids running the lint roster on one long-lived host collide on a file neither can write"; }

# ----- fixtures: the estate as `kubectl get nodes -o json` shows it ---
# w-1's capacity is the real reading (974168604Ki → 929 GiB, the figure
# the observation carried on the day the gap was measured). cp-9 stands
# for a node whose Talos API the forge could not reach; cp-8 for a
# reading whose /var is not the kubelet's filesystem (500 G against a
# 236 G ephemeral-storage capacity).
cat >"$tmp/nodes.json" <<'JSON'
{"items":[
 {"metadata":{"name":"w-1","labels":{"boss.dev/purpose":"build"}},
  "status":{"addresses":[{"type":"InternalIP","address":"10.20.0.21"}],
            "capacity":{"cpu":"32","memory":"131497404Ki","ephemeral-storage":"974168604Ki"},
            "conditions":[{"type":"Ready","status":"True"}]}},
 {"metadata":{"name":"cp-9","labels":{"node-role.kubernetes.io/control-plane":""}},
  "status":{"addresses":[{"type":"InternalIP","address":"10.20.0.19"}],
            "capacity":{"cpu":"8","memory":"16273484Ki","ephemeral-storage":"247483648Ki"},
            "conditions":[{"type":"Ready","status":"True"}]}},
 {"metadata":{"name":"cp-8","labels":{"node-role.kubernetes.io/control-plane":""}},
  "status":{"addresses":[{"type":"InternalIP","address":"10.20.0.18"}],
            "capacity":{"cpu":"8","memory":"16273484Ki","ephemeral-storage":"247483648Ki"},
            "conditions":[{"type":"Ready","status":"True"}]}}
]}
JSON

# The talos-nodefs page, as GET /api/estate/observations answers it. The
# OLDER reading sits first on purpose: the newest is chosen by its own
# observed_at, and a reader that took the first row would carry w-1's
# stale `1`.
nodefs_page() { # $1 = observed_at of the newest reading
    jq -cn --arg at "$1" '{ total: 2, data: [
      { payload: { scope: "talos-nodefs", observer: "boss-estate-observe-nodefs", mount: "/var",
                   observed_at: "2026-01-01T00:00:00Z",
                   nodes: [ { id: "w-1", disk_gb: 929, disk_free_gb: 1 } ] } },
      { payload: { scope: "talos-nodefs", observer: "boss-estate-observe-nodefs", mount: "/var",
                   observed_at: $at,
                   nodes: [ { id: "w-1", address: "10.20.0.21", disk_gb: 929, disk_free_gb: 390 },
                            { id: "cp-9", address: "10.20.0.19", disk_gb: null, disk_free_gb: null,
                              unread: "talosctl -n 10.20.0.19 mounts failed: connection refused" },
                            { id: "cp-8", address: "10.20.0.18", disk_gb: 500, disk_free_gb: 480 } ] } } ] }'
}

# ----- stubs -----
mkdir -p "$tmp/bin"
cat >"$tmp/bin/kubectl" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$FIXTURES/kubectl.calls"
if [[ "${1:-}" == "get" && "${2:-}" == "nodes" ]]; then cat "$FIXTURES/nodes.json"; exit 0; fi
echo "kubectl stub: unexpected args: $*" >&2
exit 99
STUB
cat >"$tmp/bin/curl" <<'STUB'
#!/usr/bin/env bash
for a in "$@"; do
    case "$a" in *scope=talos-nodefs*) cat "$FIXTURES/nodefs-page.json"; exit 0 ;; esac
done
prev=""
for a in "$@"; do
    [[ "$prev" == "--data-binary" && "$a" == @* ]] && printf '%s' "$(cat "${a#@}")" >"$CAPTURE"
    prev="$a"
done
printf '{"recorded":true}\n202'
STUB
chmod +x "$tmp/bin/kubectl" "$tmp/bin/curl"

# One run of the observer: $1 names the run, $2 the newest reading's
# observed_at. A directory of its own per run, NOT $tmp: the observer
# writes `nodes.json` there and the kubectl stub reads the fixture of
# that name out of $FIXTURES, so one directory for both would be
# `cat f > f`.
run_observer() {
    local name="$1" at="$2"
    work="$tmp/work-$name"
    mkdir -p "$work"
    nodefs_page "$at" >"$tmp/nodefs-page.json"
    : >"$tmp/kubectl.calls"
    export FIXTURES="$tmp" CAPTURE="$tmp/posted-$name.json"
    PATH="$tmp/bin:$PATH" JOBS_API="http://stub" BOSS_OBSERVE_WORK="$work" \
        DISPATCHER_API="http://stub-dispatcher" \
        bash "$tmp/observe.sh" >"$tmp/out-$name" 2>&1
    rc=$?
    out="$tmp/out-$name"
    [[ $rc -eq 0 ]] || { cat "$out" >&2; fail "[$name] the observer exited $rc although only free-space figures were missing — a best-effort figure took the whole observation with it"; }
    [[ -s "$CAPTURE" ]] || { cat "$out" >&2; fail "[$name] nothing was posted"; }
    # The static pin above reads the text; this reads the behaviour.
    # Both, because a grep can be satisfied by a path that is never
    # written and a run can land its files anywhere.
    [[ -s "$work/observation.json" ]] || { cat "$out" >&2; fail "[$name] the observer built its observation somewhere other than the directory this run owns ($work) — BOSS_OBSERVE_WORK is not reaching every scratch path, so this check still writes where another uid may already have"; }
    ! grep -q -- '--raw' "$tmp/kubectl.calls" \
        || { cat "$tmp/kubectl.calls" >&2; fail "[$name] the observer asked the API server for a node subresource (kubectl get --raw, above) — that is the nodes/proxy read this check refuses"; }
    n=$(jq '.nodes | length' "$CAPTURE")
    [[ "$n" == "3" ]] || { cat "$out" >&2; fail "[$name] expected 3 observed nodes, got $n"; }
}
field() { jq -r --arg id "$2" ".nodes[] | select(.id == \$id) | .$3" "$1"; }

# ----- run 1: a fresh reading --------------------------------------
run_observer fresh "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
posted="$CAPTURE"

# 1 + 2: the free figure, the reading it came from, the filesystem.
free=$(field "$posted" w-1 disk_free_gb)
[[ "$free" == "390" ]] || { cat "$out" >&2; fail "w-1 reported disk_free_gb=$free, expected 390 from the NEWEST talos-nodefs reading (1 would be the older row the page lists first) — without it estate.compare's floor has no numerator and can never fire for a cluster node"; }
total=$(field "$posted" w-1 disk_gb)
[[ "$total" == "929" ]] || { cat "$out" >&2; fail "w-1 reported disk_gb=$total, expected 929 (974168604Ki of ephemeral-storage, nearest GiB) — the floor's denominator must stay the kubelet's own capacity"; }
source_scope=$(jq -r '.disk_free_source.scope // empty' "$posted")
source_at=$(jq -r '.disk_free_source.observed_at // empty' "$posted")
[[ "$source_scope" == "talos-nodefs" && "$source_at" != "2026-01-01T00:00:00Z" && -n "$source_at" ]] \
    || { cat "$out" >&2; fail "the observation does not name the reading its free figures came from (disk_free_source = $(jq -c '.disk_free_source' "$posted")) — a figure copied across without its receipt cannot be traced back to the forge's reading"; }
mismatch=$(field "$posted" cp-8 disk_free_gb)
[[ "$mismatch" == "null" ]] || { cat "$out" >&2; fail "cp-8's Talos /var is 500 G against a 236 G ephemeral-storage capacity, yet it reported disk_free_gb=$mismatch — a free figure from another filesystem is no reading of the kubelet's"; }
grep -q 'cp-8.*not the kubelet filesystem' "$out" || { cat "$out" >&2; fail "the refused cp-8 figure was not explained on stdout"; }

# 3: an unread node is null, intact and named.
blind=$(field "$posted" cp-9 disk_free_gb)
[[ "$blind" == "null" ]] || { cat "$out" >&2; fail "the forge could not read cp-9 but it reported disk_free_gb=$blind — a failed read must be null, never a number and never a silent omission"; }
blind_cpu=$(field "$posted" cp-9 cpu)
[[ "$blind_cpu" == "8" ]] || { cat "$out" >&2; fail "cp-9 lost its other fields (cpu=$blind_cpu) when its free figure was unread"; }
grep -q 'cp-9.*connection refused' "$out" || { cat "$out" >&2; fail "the forge's reason for cp-9 was not reported on stdout — quiet is a loan against the next diagnosis (CLAUDE.md §Diagnosis)"; }

# ----- run 2: the newest reading is too old --------------------------
run_observer stale "2026-01-02T00:00:00Z"
for id in w-1 cp-8 cp-9; do
    v=$(field "$CAPTURE" "$id" disk_free_gb)
    [[ "$v" == "null" ]] || { cat "$out" >&2; fail "[stale] $id reported disk_free_gb=$v from a reading months old — a forge reader that went quiet must look blind, not healthy"; }
done
grep -q 'w-1.*s old, past NODEFS_MAX_AGE_S' "$out" || { cat "$out" >&2; fail "[stale] the reading's age was not given as the reason on stdout"; }

# ----- run 3: the newest reading is dated in the FUTURE ----------------
# Its age is negative, so a one-sided `age > max` passes it, and it sorts
# newest, so it would win over every genuine reading until it fell off
# the five-row page — about 75 minutes at the forge's cadence. A skewed
# clock or a forged post must read blind, not as headroom (review of
# 4327d1a3, finding 1).
run_observer future "2099-01-01T00:00:00Z"
for id in w-1 cp-8 cp-9; do
    v=$(field "$CAPTURE" "$id" disk_free_gb)
    [[ "$v" == "null" ]] || { cat "$out" >&2; fail "[future] $id reported disk_free_gb=$v from a reading dated 2099 — a reading from the future is a skewed clock or a forgery, never headroom"; }
done
grep -q 'w-1.*in the future' "$out" || { cat "$out" >&2; fail "[future] the future date was not given as the reason on stdout"; }

echo "a-cluster-node-reports-its-headroom: ok — no nodes/proxy granted or read; the observer carries each node's free space from the newest talos-nodefs reading (w-1: 390 of 929 GiB) with its receipt, refuses a figure from another filesystem, and an unread or stale figure degrades to disk_free_gb:null with the observation intact and the reason named"
exit 0
