#!/usr/bin/env bash
#
# dev-memory-watch — keep the last hour of the dev pod's per-process
# memory on the PVC, and turn a container restart into ONE alert packet
# that carries the samples from before the kill.
#
# WHY THIS EXISTS (backlog 9ecd12ea). Measured 2026-10-08: container
# `dev` of the dev pod was OOMKilled (exit 137) at 10:31:52Z at its 32Gi
# limit and restarted four seconds later. The pod was not replaced, so
# it read to the operator as a pod roll; it killed the operator session
# and two reviewer runs, and the only record anywhere was an overdue
# receipt the estate observer filed 2h58m later. What held the 32Gi is
# not known and cannot be recovered: the container's cgroup counters
# were recreated with it (memory.peak read 1.28 GB after the restart)
# and nothing kept a per-process reading from before the kill. Detection
# and diagnosis both failed, separately (CLAUDE.md §Diagnosis): nothing
# said it happened, and nothing could say why.
#
# WHAT IT KEEPS. Every BOSS_DEV_MEMORY_INTERVAL_S seconds, one sample
# file under WORK_MOUNT/telemetry/dev-memory/ring/: per container of the
# pod, the process count and the summed resident and anonymous memory;
# per container and process NAME, the same three and the parents they
# ran under, so a count of compilers is a count and not a lower bound
# (backlog 893ce2b0), with the parent of each crowd named by its command
# line; then the BOSS_DEV_MEMORY_TOP largest processes in the pod by
# resident size, each with its container, pid, name and command line. The ring
# holds the newest BOSS_DEV_MEMORY_RING samples (240 at 15 s: an hour,
# about a megabyte). It lives on the PVC because the PVC is what a
# container kill leaves standing. This is telemetry, so it stays out of
# the audit log; only the kill is a packet.
#
# WHAT IT FILES. Each tick reads this pod's status from the cluster API
# and compares every container's restartCount with the count it last
# acted on (seen/<pod uid>.<container>, on the PVC). A count that rose
# freezes the whole ring to kills/<container>-<when>/ — a copy, so later
# ticks cannot rotate the evidence away — and keeps one alert in a spool
# beside the ring, in the shape infra/forge/alert-lib.sh files: an
# urgent backlog-item for the platform owner naming the container, the
# reason and exit code the kubelet recorded, the limit, and the last
# sample before the kill with its largest processes. Every tick sends
# what the spool holds, so a system of record that is dark costs the
# alert nothing but time. A count never acted on is read as 0, so the
# first tick on a pod whose container has already restarted reports
# that restart — as one that predates the watch.
#
# WHERE IT RUNS, AND WHAT IT CANNOT SEE — both measured 2026-10-08 on
# boss-dev-6fd45cbbbd-rkjq6, not assumed. It runs in the `reclaim`
# sidecar, because a sampler inside the watched container is killed with
# it (the container's memory.oom.group reads 1: the kernel takes the
# whole cgroup). The pod's shareProcessNamespace puts every container's
# processes in the sidecar's /proc, and a process with no capabilities —
# which is what the sidecar is (`capabilities: {drop: [ALL]}`) — reads
# another container's /proc/<pid>/statm, cgroup, comm and cmdline. It
# does NOT read that container's cgroup: /proc/<pid>/root/… (and
# smaps_rollup, io, environ) answer Permission denied, because that door
# is a ptrace check and the dev container's processes hold capabilities
# the sidecar does not. So a sample's `cgroup` line for another
# container says `unreadable` rather than a number, and the summed
# resident size stands in for memory.current — an overcount where
# processes share pages, and blind to page cache and tmpfs charged to
# the cgroup. Run by hand inside a container, the same line carries that
# container's own memory.current / peak / max / oom_kill.
#
# WHAT STARTS IT. Not the manifest: an edit to boss-dev.yaml rolls the
# pod and ends every session in it. dev-scratch-reclaim.sh, which the
# sidecar already runs from the checkout every hour, calls `--ensure`;
# that starts `--loop` in the background when none is alive, and only
# from a container that is NOT the watched one. The loop runs each tick
# as a fresh `bash <this file> --tick`, so a fast-forwarded checkout
# changes what the next tick does; LOOP_GENERATION below is how a change
# to the loop itself replaces a running one.
#
# The cluster API is read with the pod's own service-account token
# (Role dev-session-gates already grants `get` on pods); the alert goes
# out the way the sidecar's maintenance packet does, with no credential
# the sidecar lacks.
#
# COMMAND LINES STAY ON THE VOLUME. The ring records each large
# process's command line after a best-effort scrub that is a denylist
# and passes most credential shapes (`scrub` lists what it does not
# take). So the alert names a process by its name, pid, container and
# size only, and points at the frozen ring for the rest: the ring's
# audience is this pod, the packet's is everyone who reads the queue.
#
# WHAT BOUNDS IT (review of car 7849553a). At most one item per
# container per BOSS_DEV_MEMORY_ALERT_WINDOW_S; the restarts in between
# are counted into the next item. A restart found on the watch's FIRST
# tick for a pod, with no sample before it, predates the watch: it is
# filed at standard priority under a title that says so. A sample is
# evidence only for the pod it was taken in: the ring outlives a roll,
# and a new pod's kill is never described with the old pod's sample
# (backlog 36b1aa00). An alert the
# API REFUSES backs off and is moved aside at the fifth refusal, so it
# cannot hold the ones behind it. The spool keeps its newest 48 and
# counts what it drops, alert-refused/ its newest 20, kills/ the newest
# five rings no open item names plus up to twenty that one does (see
# `prune_kills`), seen/ a week of gone pods, watch.log a megabyte.
#
# HOW A READER KNOWS IT IS RUNNING. `--state` prints the loop's pid and
# generation, the newest sample's age, the alerts kept, refused and
# dropped, and what --ensure last decided; dev-scratch-reclaim.sh puts
# those on the maintenance packet it files, and counts a watch that is
# not sampling as a problem, which files one.
#
#   dev-memory-watch.sh --tick     one restart check and one sample
#   dev-memory-watch.sh --loop     tick forever (what --ensure starts)
#   dev-memory-watch.sh --ensure   start the loop unless one is alive
#   dev-memory-watch.sh --state    the watch's state, key=value lines
#   dev-memory-watch.sh --acknowledge
#                                  a person has read the refused and
#                                  dropped alerts and the retired frozen
#                                  rings: keep them aside, zero
#                                  the counts, and say so on the next
#                                  reclaim packet (see `acknowledge`)
#   dev-memory-watch.sh --recorded <acknowledgement>...
#                                  the reclaim pass: those rode a packet
#
# Tunables (env): BOSS_DEV_MEMORY_INTERVAL_S (15), BOSS_DEV_MEMORY_RING
# (240), BOSS_DEV_MEMORY_TOP (15), BOSS_DEV_MEMORY_CONTAINER (dev — the
# container the loop must stand outside of), BOSS_JOBS_URL (else
# infra/dev/sor-url beside this checkout).
# Paths (env): WORK_MOUNT (/work), BOSS_DEV_MEMORY_DIR
# (WORK_MOUNT/telemetry/dev-memory), PROC_ROOT (/proc — a test fakes
# it), BOSS_K8S_SA_DIR (the service-account mount), BOSS_POD_NAME (the
# hostname).
set -uo pipefail

SELF="$0"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
WORK_MOUNT="${WORK_MOUNT:-/work}"
DIR="${BOSS_DEV_MEMORY_DIR:-$WORK_MOUNT/telemetry/dev-memory}"
PROC_ROOT="${PROC_ROOT:-/proc}"
INTERVAL="${BOSS_DEV_MEMORY_INTERVAL_S:-15}"
RING_KEEP="${BOSS_DEV_MEMORY_RING:-240}"
TOP="${BOSS_DEV_MEMORY_TOP:-15}"
WATCHED="${BOSS_DEV_MEMORY_CONTAINER:-dev}"
SA_DIR="${BOSS_K8S_SA_DIR:-/var/run/secrets/kubernetes.io/serviceaccount}"
POD="${BOSS_POD_NAME:-$(hostname)}"
# Frozen rings kept: five kills is days of history at a megabyte each.
# These are the rings no open item names; the ones an item names are
# bounded apart, below.
KILLS_KEEP=5
# Rings an item not yet read closed holds, at most (about 20 MB), how
# often one such item is read, and how many retirement lines are kept
# (backlog 0143612a; see `prune_kills`).
KILLS_HELD_MAX=20
HELD_CHECK_S=3600
RETIRED_LOG_KEEP=200
# The longest a tick waits on the cluster API before it samples; also
# what --ensure's wait for a first sample is sized from.
POD_READ_MAX_S=8
# One item per container per this many seconds.
ALERT_WINDOW_S="${BOSS_DEV_MEMORY_ALERT_WINDOW_S:-3600}"
# The spool's bounds: alerts waiting, refusals before one is moved
# aside, refused bodies kept, and the loop's own log.
SPOOL_KEEP=48
REFUSALS_MAX=5
REFUSED_KEEP=20
# How long a kept alert may wait before the hold itself is the reclaim
# pass's problem (review fb97dd5d of car 1fe37401, finding F1): twice
# the back-offs a body the API refuses can wait out before its fifth
# refusal moves it aside — 2 + 4 + 8 + 16 minutes, 60 * (2^REFUSALS_MAX
# - 2) s. Past it no refusal of the body explains the wait; only answers
# that count nothing against it do (no answer, a 5xx, a refusal of the
# caller), and nothing else would ever end that silence. `--state`
# prints it beside the oldest kept alert's age, so the bound lives here
# with the back-off it is derived from and the reclaim only compares.
HELD_AFTER_S=$((2 * 60 * ((1 << REFUSALS_MAX) - 2)))
LOG_MAX_BYTES="${BOSS_DEV_MEMORY_LOG_MAX_BYTES:-1048576}"
# Bump when `loop` itself changes: --ensure replaces a running loop
# whose pidfile names another generation.
LOOP_GENERATION=1

log() { echo "dev-memory-watch: $*"; }

# shellcheck source=infra/lib/jq.sh
. "$HERE/../lib/jq.sh"

for name in INTERVAL RING_KEEP TOP ALERT_WINDOW_S LOG_MAX_BYTES; do
    case "${!name}" in
        '' | *[!0-9]* | 0)
            echo "dev-memory-watch: $name must be a whole number above zero, got '${!name}'" >&2
            exit 64
            ;;
    esac
done

# This pod's status, into $1. Unreadable is an ANSWER here, never a
# failure: the sample is still written (containers named by their
# cgroup id) and the restart check waits for the next tick. The token
# rides in a 0600 header file, never in curl's argv — this script
# records command lines.
POD_UNREAD=""
pod_status() {
    local out="$1" ns hdr
    if [ ! -r "$SA_DIR/token" ] || [ -z "${KUBERNETES_SERVICE_HOST:-}" ]; then
        POD_UNREAD="no service-account token or cluster API address in this container"
        return 1
    fi
    ns=$(head -n1 "$SA_DIR/namespace" 2>/dev/null)
    hdr="$TMP/k8s-header"
    (umask 077; printf 'Authorization: Bearer %s\n' "$(head -n1 "$SA_DIR/token")" > "$hdr")
    if ! curl -sS -f --max-time "$POD_READ_MAX_S" --cacert "$SA_DIR/ca.crt" -H "@$hdr" -o "$out" \
        "https://${KUBERNETES_SERVICE_HOST}:${KUBERNETES_SERVICE_PORT:-443}/api/v1/namespaces/$ns/pods/$POD" 2> "$TMP/k8s-err"; then
        POD_UNREAD="the cluster API did not answer for pod $POD: $(tr '\n' ' ' < "$TMP/k8s-err")"
        return 1
    fi
    # jq_doc_file first: on jq-1.6 `jq -e` over NO document exits 0, and
    # an empty 200 body must not read as a pod with no containers.
    if ! { jq_doc_file "$out" && jq -e '.status.containerStatuses | type == "array"' "$out" > /dev/null 2>&1; }; then
        POD_UNREAD="the cluster API's answer for pod $POD carries no containerStatuses"
        return 1
    fi
}

# One row per container: name, cgroup id, restartCount, then what the
# kubelet recorded of the LAST termination (reason, exit code, when) and
# the memory limit. `-` for an absent value: tab is whitespace to
# `read`, so an empty field would collapse into its neighbour.
containers_of() {
    jq -r '. as $p | .status.containerStatuses[] | . as $c
        | [ .name,
            ((.containerID // "") | sub("^.*://"; "") | if . == "" then "-" else . end),
            (.restartCount // 0),
            (.lastState.terminated.reason // "-"),
            (.lastState.terminated.exitCode // "-"),
            (.lastState.terminated.finishedAt // "-"),
            ([$p.spec.containers[]? | select(.name == $c.name) | .resources.limits.memory // empty][0] // "-")
          ] | @tsv' "$1"
}

# BEST EFFORT, AND A DENYLIST: this takes five shapes and nothing else,
# so a command line that passes it is NOT thereby free of credentials.
# Measured in the review of car 7849553a: of 32 credential shapes, 23
# came out intact — `curl -u user:pass`, `Authorization: Basic <b64>`
# (the word Basic goes, the credential stays), a JSON body
# `{"token":"v"}`, `mysql -pVALUE`, `sshpass -p VALUE`, a cookie, `--key`
# `--pat` `--passphrase` `pwd=` `pass=`, hyphenated tokens (sk-ant-,
# glpat-, xoxb-), base64 with a slash, a secret stated in prose. That is
# why a command line is written ONLY to the ring, on a volume no pod but
# this one mounts and whose containers already read each other's /proc —
# and never to a packet (see describe_sample).
#
# What it does take, then anything a line of the ring cannot carry, then
# a bound. The userinfo of a URL; the
# word after `Bearer`; the value of anything NAMED like a credential
# (token, secret, password, api key, authorization, credential —
# `--x-token V`, `X_TOKEN=V`, `x-token: V`); a JWT or a GitHub token by
# its prefix; and any run of 32 or more letters and digits, which takes
# a forge token and a sha. A packet id (hyphens) and a test binary's
# name (underscores) stay readable: they are what a reader of a kill
# needs, and the alphabet of the last rule is why they survive it.
scrub() {
    sed -E \
        -e 's#://[^/@[:space:]]+@#://<redacted>@#g' \
        -e 's/(bearer)[[:space:]]+[^[:space:]]+/\1 <redacted>/Ig' \
        -e 's/((token|secret|passw(or)?d|api[-_]?key|authorization|credential)[A-Za-z0-9_-]*)([=:[:space:]]+)[^[:space:]]+/\1\4<redacted>/Ig' \
        -e 's/(eyJ[A-Za-z0-9_.-]+|gh[pousr]_[A-Za-z0-9]+|github_pat_[A-Za-z0-9_]+)/<redacted>/g' \
        -e 's/[A-Za-z0-9+=]{32,}/<redacted>/g' \
        | tr -cd '\11\12\40-\176' | cut -c1-320
}

# A process's name as a record may carry it. A process sets its own
# comm to any bytes it likes (prctl PR_SET_NAME), tabs, spaces and
# newlines included, so the name is held to a class that cannot be a
# field separator and to the kernel's own fifteen bytes; anything else
# becomes `?`. describe_sample applies the same rule again to what it
# reads, so a ring written by any other hand cannot widen it.
safe_name() {
    local n="${1:0:15}"
    n="${n//[!A-Za-z0-9._:\/+-]/?}"
    printf '%s' "${n:-?}"
}

# The watched cgroup's own counters, read through one of its processes.
# Readable only from inside that container (the header says why).
cgroup_line() {
    local name="$1" pid="$2" base cur peak max kills
    base="$PROC_ROOT/$pid/root/sys/fs/cgroup"
    if read -r cur 2> /dev/null < "$base/memory.current"; then
        peak="-"; max="-"; kills="-"
        read -r peak 2> /dev/null < "$base/memory.peak" || true
        read -r max 2> /dev/null < "$base/memory.max" || true
        kills=$(awk '$1 == "oom_kill" {print $2}' "$base/memory.events" 2>/dev/null)
        printf 'cgroup\t%s\tcurrent=%s\tpeak=%s\tmax=%s\toom_kill=%s\n' "$name" "$cur" "$peak" "$max" "${kills:--}"
    else
        printf 'cgroup\t%s\tunreadable from this container (it holds no capability over that one)\n' "$name"
    fi
    # Kernel accounting categories, not a guess from process RSS. Keep
    # missing, malformed and duplicate counters unknown, never zero.
    { cat "$base/memory.stat" 2>/dev/null || true; } | awk -v name="$name" '
        BEGIN { n = split("anon file kernel kernel_stack pagetables sec_pagetables percpu sock vmalloc shmem zswap zswapped file_mapped file_dirty file_writeback swapcached anon_thp file_thp shmem_thp inactive_anon active_anon inactive_file active_file unevictable slab_reclaimable slab_unreclaimable slab", keys, " ") }
        { seen[$1]++; if (NF == 2 && $2 ~ /^[0-9]+$/) value[$1] = $2 }
        END { printf "memory-stat\t%s", name
              for (i = 1; i <= n; i++) printf "\t%s=%s", keys[i], (seen[keys[i]] == 1 && keys[i] in value) ? value[keys[i]] : "unknown"
              printf "\n" }'
}

# In a private cgroup namespace our processes show only 0::/. Bind
# that root only when this pod's current running IDs and every observed
# external ID leave exactly one candidate. No limit/name heuristic and
# no historical sample is reinterpreted using this answer.
own_binding() {
    printf 'own-binding\tself\tunknown\n' > "$TMP/own-binding"
    OWN_NAME=self
    [ -z "$POD_UNREAD" ] && [ ! -s "$TMP/cgroup-unread" ] || return 0
    jq_doc_file "$TMP/pod.json" || return 0
    jq -er --arg pod "$POD" '
        def valid_running_state:
            (.state | type == "object") and
            (.state.running == null or
                (.state.running | type == "object" and
                    (.startedAt | type == "string" and length > 0)));
        select(.metadata.name == $pod and (.metadata.uid | type == "string" and length > 0))
        | select(([.spec.containers[].name] | sort) == ([.status.containerStatuses[].name] | sort))
        | select(all(.status.containerStatuses[]; valid_running_state))
        | [.status.containerStatuses[] | select(.state.running != null)
            | {name, id: ((.containerID // "") | sub("^containerd://"; ""))}]
        | select(length > 0 and (map(.name) | unique | length) == length
            and (map(.id) | unique | length) == length)
        | .[] | select(.name | test("^[a-z0-9][a-z0-9-]*$"))
        | select(.id | test("^[0-9a-f]{64}$")) | [.name, .id] | @tsv
        ' "$TMP/pod.json" > "$TMP/running" 2>/dev/null || return 0
    # Invalid running rows must not disappear and manufacture uniqueness.
    local count rows binding uid external sandbox
    count=$(jq '[.status.containerStatuses[] | select(.state.running != null)] | length' "$TMP/pod.json")
    rows=$(wc -l < "$TMP/running")
    [ "$count" = "$rows" ] || return 0
    # This reader's own hostname bind mount independently names its pod
    # sandbox. Inspect only that mount metadata, never mounted file contents.
    # An absent attestation exempts nothing; malformed/duplicate rows refuse.
    sandbox=
    if [ -r "$PROC_ROOT/self/mountinfo" ]; then
        sandbox=$(awk '
            $5 == "/etc/hostname" {
                selected++
                separators = 0
                for (i = 7; i <= NF; i++) if ($i == "-") { separators++; sep = i }
                if (NF < 10 || separators != 1 || sep + 3 != NF ||
                    $1 !~ /^[0-9]+$/ || $2 !~ /^[0-9]+$/ || $3 !~ /^[0-9]+:[0-9]+$/ ||
                    $4 !~ /^\/(var\/)?lib\/containerd\/io[.]containerd[.]grpc[.]v1[.]cri\/sandboxes\/[0-9a-f]+\/hostname$/) bad = 1
                n = split($4, path, "/"); id = path[n-1]
                if (length(id) != 64 || id !~ /^[0-9a-f]+$/) bad = 1
            }
            END { if (bad || selected > 1) exit 1; if (selected == 1) print id }
            ' "$PROC_ROOT/self/mountinfo" 2>/dev/null) || return 0
    fi
    binding=$(awk -F'\t' -v sandbox="$sandbox" '
        NR == FNR { names[$2] = $1; next }
        $1 == "root" { root = 1; next }
        $1 == "external" { if (!($2 in names) && $2 != sandbox) bad = 1; observed[$2] = 1; next }
        { bad = 1 }
        END { if (sandbox in names) bad = 1
              for (id in names) if (!(id in observed)) { missing++; own = id }
              if (root && !bad && missing == 1) printf "%s\t%s", names[own], own }' \
        "$TMP/running" "$TMP/cgroups")
    [ -n "$binding" ] || return 0
    OWN_NAME=${binding%%$'\t'*}
    uid=$(jq -r '.metadata.uid' "$TMP/pod.json" | tr -cd 'A-Za-z0-9._-')
    external=$(awk -F'\t' -v sandbox="$sandbox" '$1 == "external" && $2 != sandbox {print $2}' "$TMP/cgroups" | sort -u | paste -sd, -)
    printf 'own-binding\t%s\tmethod=unique-running-minus-external\tid=%s\tpod_uid=%s\texternal_ids=%s' \
        "$OWN_NAME" "${binding#*$'\t'}" "$uid" "$external" > "$TMP/own-binding"
    if [ -n "$sandbox" ]; then
        printf '\tsandbox_id=%s\tsandbox_source=own-hostname-mount' "$sandbox" >> "$TMP/own-binding"
    fi
    printf '\n' >> "$TMP/own-binding"
}

# One sample into the ring. Line kinds, tab-separated:
#   at         <utc> <epoch> <pod> <interval_s>
#   pod-status unread <why>            (only when the API did not answer)
#   own-binding <name> method/id/pod_uid/external_ids | self unknown
#   container  <name> procs=N rss_kib=N anon_kib=N restarts=N limit=X
#   cgroup     <name> current=… peak=… max=… oom_kill=… | unreadable …
#   memory-stat <name> <kernel-category>=<bytes|unknown> …
#   command    <container> <comm> procs=N rss_kib=N anon_kib=N parents=<comm>[<pid>]x<n>,…
#   parent     <container> <pid> <comm> <cmdline>   (see by_command)
#   proc       <rss_kib> <anon_kib> <container> <pid> <comm> <cmdline>
# rss is statm's resident pages, anon is resident minus shared.
git_phase_snapshot() {
    local mode="${1:---snapshot}" result
    result=$(BOSS_RECLAIM_GIT_DIR="${BOSS_RECLAIM_GIT_DIR:-${WORK_MOUNT:-/work}/telemetry/dev-reclaim-git}" \
        bash "$(dirname "$(readlink -f "$SELF")")/dev-reclaim-git.sh" "$mode" 2>/dev/null) || result=""
    if [ -n "$result" ]; then
        printf '%s\n' "$result"
    else
        printf '%s\n' '{"state":"unknown","reason":"phase reader unavailable","phases":[]}'
    fi
}

# EVERY PROCESS, COUNTED BY ITS NAME (backlog 893ce2b0). The proc lines
# keep only the TOP largest, so measured 2026-10-10 07:48:04Z the widest
# sample held at least ten clippy-drivers under one workspace cargo
# check — "at least", because a 32-wide clippy showed at most fifteen.
# From the rows sample() writes, into $TMP/commands one `command` line
# per container and name, largest first within the container: how many
# ran, their summed resident and anonymous memory, and the parents they
# ran under, `<name>[<pid>]x<n>` most first (`?` for a parent the table
# does not hold, `-` for a parent stat would not give), PARENTS_KEEP of
# them and `+<n>` for the rest. Into $TMP/parents, for the commands that
# ran more than once, each listed parent the table holds — the cargo a
# crowd of compilers ran from — PARENT_LINES_KEEP of them, largest crowd
# first; sample() names each with its command line, in the ring only.
PARENTS_KEEP=4
PARENT_LINES_KEEP=8
by_command() {
    # One row per container, name and parent; then folded per name.
    awk -F'\t' '{ k = $3 "\t" $6 "\t" $5; n[k]++; rss[k] += $1; anon[k] += $2 }
        END { for (k in n) printf "%s\t%d\t%d\t%d\n", k, n[k], rss[k], anon[k] }' "$TMP/rows" \
        | sort -t $'\t' -k1,1 -k2,2 -k4,4nr -k3,3n \
        | awk -F'\t' -v keep="$PARENTS_KEEP" '
            function flush() {
                if (cur == "") return
                if (more) parents = parents ",+" more
                printf "%s\t%d\t%d\t%d\t%s\t%s\n", cur, procs, rss, anon, parents, held
            }
            FILENAME == ARGV[1] { who[$4] = $6; where[$4] = $3; next }
            { k = $1 "\t" $2
              if (k != cur) { flush(); cur = k; procs = 0; rss = 0; anon = 0; listed = 0; more = 0; parents = ""; held = "" }
              procs += $4; rss += $5; anon += $6
              if (listed >= keep) { more++; next }
              listed++
              if ($3 == "-") p = "-"
              else p = (($3 in who) ? who[$3] : "?") "[" $3 "]"
              parents = parents (parents == "" ? "" : ",") p "x" $4
              if ($3 in who) held = held " " $3 "," where[$3] "," who[$3] }
            END { flush() }' "$TMP/rows" - \
        | sort -t $'\t' -k1,1 -k4,4nr -k2,2 > "$TMP/by-name"
    awk -F'\t' '{ printf "command\t%s\t%s\tprocs=%d\trss_kib=%d\tanon_kib=%d\tparents=%s\n", $1, $2, $3, $4, $5, $6 }' \
        "$TMP/by-name" > "$TMP/commands"
    awk -F'\t' '$3 >= 2 { n = split($7, h, " "); for (i = 1; i <= n; i++) print $4 "\t" h[i] }' "$TMP/by-name" \
        | sort -t $'\t' -k1,1nr \
        | awk -F'\t' -v keep="$PARENT_LINES_KEEP" '{ split($2, f, ",") }
            !(f[1] in seen) && shown < keep { seen[f[1]] = 1; shown++; print $1 "\t" f[1] "\t" f[2] "\t" f[3] }' \
        > "$TMP/parents"
}

sample() {
    local epoch at page_kb p res shr cg pid c comm cmd out st
    epoch=$(date -u +%s)
    at=$(date -u -d "@$epoch" +%Y-%m-%dT%H:%M:%SZ)
    page_kb=$(( $(getconf PAGESIZE 2>/dev/null || echo 4096) / 1024 ))
    : > "$TMP/cgroups"; : > "$TMP/cgroup-unread"
    for p in "$PROC_ROOT"/[0-9]*; do
        [ -d "$p" ] || continue
        if ! read -r cg 2> /dev/null < "$p/cgroup"; then
            printf 'unread\n' >> "$TMP/cgroup-unread"
            continue
        fi
        case "$cg" in
            '0::/') printf 'root\n' >> "$TMP/cgroups" ;;
            '0::/../'*) printf 'external\t%s\n' "${cg#0::/../}" >> "$TMP/cgroups" ;;
            *) printf 'unrecognized\n' >> "$TMP/cgroups" ;;
        esac
        read -r _ res shr _ 2> /dev/null < "$p/statm" || continue
        case "${res:-0}" in '' | *[!0-9]* | 0) continue ;; esac
        # Every process's name and parent, for the by-name counts
        # (backlog 893ce2b0). The parent is the field after the state in
        # stat, and the state follows the LAST `) `: a process names
        # itself, `) S 1 (` included, so the whole file is read (a name
        # may hold a newline too) and cut at the last one. Unreadable or
        # malformed is `-`, never a guess. Builtins only — this runs for
        # every process every tick.
        st=""
        IFS= read -r -d '' st 2> /dev/null < "$p/stat" || true
        st="${st##*) }"; st="${st#* }"; st="${st%% *}"
        case "${st:-empty}" in empty | *[!0-9]*) st=- ;; esac
        comm=""
        IFS= read -r comm 2> /dev/null < "$p/comm" || true
        comm="${comm:0:15}"; comm="${comm//[!A-Za-z0-9._:\/+-]/?}"
        printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$res" "${shr:-0}" "${p##*/}" "${cg##*/}" "$st" "${comm:-?}"
    done > "$TMP/procs"
    own_binding
    # A process in this script's own container shows an empty root
    # tail; keep self unless the recorded native reconciliation binds
    # it. Unknown or duplicate external IDs keep their twelve-byte ID.
    awk -F'\t' -v kb="$page_kb" -v own="$OWN_NAME" '
        FILENAME == ARGV[1] { name[$2] = $1; count[$2]++; next }
        { c = (count[$4] == 1) ? name[$4] : ($4 == "" ? own : substr($4, 1, 12))
          printf "%d\t%d\t%s\t%s\t%s\t%s\n", $1 * kb, ($1 - $2) * kb, c, $3, $5, $6 }' \
        "$TMP/containers" "$TMP/procs" > "$TMP/rows"
    sort -rn -k1,1 "$TMP/rows" > "$TMP/sorted"
    by_command
    out="$TMP/sample"
    {
        printf 'at\t%s\t%s\t%s\t%s\n' "$at" "$epoch" "$POD" "$INTERVAL"
        cat "$TMP/own-binding"
        # This observer's namespace only; OWN_NAME remains self when the native
        # container reconciliation is unavailable. No PID/argv-only attribution.
        printf 'git-phase\t%s\t%s\n' "$OWN_NAME" "$(git_phase_snapshot)"
        [ -z "$POD_UNREAD" ] || printf 'pod-status\tunread\t%s\n' "$POD_UNREAD"
        awk -F'\t' '
            FILENAME == ARGV[1] { restarts[$1] = $3; limit[$1] = $7; next }
            { n[$3]++; rss[$3] += $1; anon[$3] += $2 }
            END { for (c in n) printf "container\t%s\tprocs=%d\trss_kib=%d\tanon_kib=%d\trestarts=%s\tlimit=%s\n",
                      c, n[c], rss[c], anon[c], (c in restarts) ? restarts[c] : "-", (c in limit) ? limit[c] : "-" }' \
            "$TMP/containers" "$TMP/rows" | sort
        for c in $(printf '%s\n%s\n' "$WATCHED" "$OWN_NAME" | sort -u); do
            pid=$(awk -F'\t' -v c="$c" '$3 == c {print $4; exit}' "$TMP/rows")
            [ -z "$pid" ] || cgroup_line "$c" "$pid"
        done
        cat "$TMP/commands"
        while IFS=$'\t' read -r _ pid c comm; do
            cmd=$(tr '\0\t\n' '   ' 2> /dev/null < "$PROC_ROOT/$pid/cmdline" | cut -c1-600 | scrub)
            printf 'parent\t%s\t%s\t%s\t%s\n' "$c" "$pid" "$comm" "$cmd"
        done < "$TMP/parents"
        head -n "$TOP" "$TMP/sorted" | while IFS=$'\t' read -r res shr c pid _ comm; do
            # The scrub runs over the command line ALONE, and it is the
            # last field. Until the delta review of car 7849553a (N1)
            # the whole record went through it, and its `bearer` rule
            # eats the whitespace after that word: for a process NAMED
            # bearer that whitespace was the tab before the command
            # line, which then joined the name. No rule of the scrub can
            # move a field boundary it is never shown.
            cmd=$(tr '\0\t\n' '   ' 2> /dev/null < "$PROC_ROOT/$pid/cmdline" | cut -c1-600 | scrub)
            printf 'proc\t%s\t%s\t%s\t%s\t%s\t%s\n' "$res" "$shr" "$c" "$pid" "$(safe_name "$comm")" "$cmd"
        done
    } > "$out"
    mkdir -p "$DIR/ring" || return 1
    cp "$out" "$DIR/ring/.writing" && mv "$DIR/ring/.writing" "$DIR/ring/$epoch.tsv" || return 1
    # The bound: newest RING_KEEP stay. Names are epochs of one width,
    # so the lexical order is the time order.
    ls -1 "$DIR/ring" | grep '\.tsv$' | sort | all_but_newest "$RING_KEEP" | while read -r p; do
        rm -f "$DIR/ring/$p"
    done
    return 0
}

# stdin's lines, oldest first, minus the last $1.
all_but_newest() { awk -v keep="$1" '{ a[NR] = $0 } END { for (i = 1; i <= NR - keep; i++) print a[i] }'; }

kib_as_gib() { awk -v k="$1" 'BEGIN { printf "%.1f", k / 1048576 }'; }

# What one sample says of one container, as a sentence fragment: its
# totals, its cgroup line, and its rows among the sample's largest —
# each by NAME, pid and size, never by command line. The packet this
# feeds is an immutable record every reader of the queue sees, and the
# scrub above is a denylist (review of car 7849553a: 23 of 32 credential
# shapes passed it). The command lines stay in the ring.
describe_sample() {
    local f="$1" name="$2"
    awk -F'\t' -v name="$name" '
        $1 == "container" && $2 == name {
            bound = 1
            split($3, a, "="); split($4, b, "="); split($5, c, "=")
            printf "%d processes, resident %.1f GiB, anonymous %.1f GiB", a[2], b[2] / 1048576, c[2] / 1048576
        }
        $1 == "cgroup" && $2 == name { cg = $3; for (i = 4; i <= NF; i++) cg = cg " " $i }
        $1 == "memory-stat" && $2 == name {
            for (i = 3; i <= NF; i++) if ($i ~ /^[a-z_]+=(unknown|[0-9]+)$/) stats = stats " " $i
        }
        $1 == "container" && $2 == "self" { unassigned = 1 }
        $1 == "memory-stat" && $2 == "self" {
            for (i = 3; i <= NF; i++) if ($i ~ /^[a-z_]+=(unknown|[0-9]+)$/) ownstats = ownstats " " $i
        }
        $1 == "cgroup" && $2 == "self" {
            for (i = 3; i <= NF; i++) if ($i ~ /^(current|peak|max|oom_kill)=([0-9]+|max|-)$/) owncg = owncg " " $i
        }
        $1 == "proc" && $4 == name && shown < 8 {
            shown++
            # The name and the pid, each held to its own class HERE as
            # well as where the sample is written: whatever a line of
            # the ring holds, the packet gets at most fifteen safe
            # bytes and a number from it.
            comm = substr($6, 1, 15)
            gsub(/[^A-Za-z0-9._:\/+-]/, "?", comm)
            if (comm == "") comm = "?"
            pid = ($5 ~ /^[0-9]+$/) ? $5 : "?"
            top = top sprintf(" | %d MiB %s[%s]", $2 / 1024, comm, pid)
        }
        # How many ran by name, with the parents they ran under (backlog
        # 893ce2b0): names, counts and pids only, each held to its class
        # here too. The command lines of those parents stay in the ring.
        $1 == "command" && $2 == name && named < 5 {
            named++
            split($4, a, "="); split($5, b, "=")
            comm = substr($3, 1, 15)
            gsub(/[^A-Za-z0-9._:\/+-]/, "?", comm)
            if (comm == "") comm = "?"
            under = $7; sub(/^parents=/, "", under)
            gsub(/[^]A-Za-z0-9._:\/+?,[-]/, "?", under)
            byname = byname sprintf("%s%s %s %.1f GiB under %s", byname == "" ? " " : " | ",
                (a[2] ~ /^[0-9]+$/) ? a[2] : "?", comm, b[2] / 1048576, substr(under, 1, 120))
        }
        END {
            if (!bound) {
                printf "container binding unknown; no bound process measurements for %s", name
                if (unassigned) printf "; unassigned sampler cgroup evidence:%s", owncg == "" ? " counters unknown" : owncg
                if (unassigned && ownstats != "") printf "; unassigned memory.stat%s", ownstats
                exit
            }
            if (cg != "") printf "; cgroup %s", cg
            if (stats != "") printf "; memory.stat%s", stats
            if (top != "") printf "; largest processes in it by resident size:%s", top
            else printf "; none of its processes is among the largest in the pod in this sample"
            if (byname != "") printf "; processes by name, largest first:%s", byname
        }' "$f"
}

# Where one restart's ring is frozen: named for the kubelet's finish
# time, or for the count when it gave none.
frozen_dir() {
    local name="$1" count="$2" finished="$3" stamp
    stamp="restart-$count"
    [ "$finished" = "-" ] || stamp=$(printf '%s' "$finished" | tr -cd 'A-Za-z0-9-')
    printf '%s/kills/%s-%s' "$DIR" "$name" "$stamp"
}

# Freeze the ring, whole: the alert is a reduction, and a reduction made
# before the record is stored discards the only copy. `prune_kills`
# bounds kills/ once the pass has said which rings an item names.
FROZE=""
freeze_ring() {
    local frozen="$1"
    [ ! -d "$frozen" ] || return 0
    mkdir -p "$DIR/kills" "$DIR/ring" || return 1
    rm -rf "$frozen.copying"
    cp -a "$DIR/ring" "$frozen.copying" || return 1
    git_phase_snapshot --archive > "$frozen.copying/git-phases.json" || return 1
    mv "$frozen.copying" "$frozen" || return 1
    FROZE=1
}

# A RING AN OPEN ITEM NAMES IS THAT ITEM'S (backlog 0143612a). Measured
# 2026-10-14 on alert item 89675645: the frozen_ring it names — the
# whole ring from before an Oct 12 kill, the copy its detail is a
# reduction of — was gone while the item was still open, because this
# bound kept the newest five kill directories and removed the rest
# unconditionally, and a container in a restart loop freezes a ring for
# EVERY restart while the window files one item for many. Restarts
# nobody itemised rotated away the one ring an open item named.
#
# So a ring has `<ring>.incident` beside it from the moment an alert
# naming it is kept: `pending` until the alert is filed, then `item <id>
# <epoch last read>` (file_kept_alerts), and `check_held_ring` turns it
# into `<ring>.released` once that item reads closed or cancelled.
# BESIDE the ring, never in it: the rotation orders rings by their
# directory's mtime, which `cp -a` froze, and a marker written inside
# would make a ring read newer every time it was touched. A ring with no
# `.incident` — released, or a restart counted into another item — is
# in the rotation of KILLS_KEEP, as every ring was. A held ring is not,
# and what bounds the volume instead is KILLS_HELD_MAX: past it the
# OLDEST held ring is retired, and a retirement is never silent — a line
# in kills-retired.log naming the ring and what held it, and a count
# that `--state` carries and the reclaim pass raises as a problem until
# a person runs --acknowledge. An item that cannot be read (a dark API,
# a refused read, an alert never filed) keeps its ring held, so the
# bound and its record are the only way one leaves.
kills_oldest_first() {
    local f
    ls -1tr "$DIR/kills" | while read -r f; do
        case "$f" in *.copying | *.incident | *.released) continue ;; esac
        [ -d "$DIR/kills/$f" ] || continue
        if [ -f "$DIR/kills/$f.incident" ]; then
            [ "$1" != held ] || printf '%s\n' "$f"
        else
            [ "$1" = held ] || printf '%s\n' "$f"
        fi
    done
}
prune_kills() {
    local f line m
    [ -d "$DIR/kills" ] || return 0
    kills_oldest_first free | all_but_newest "$KILLS_KEEP" | while read -r f; do
        rm -rf "${DIR:?}/kills/$f" "$DIR/kills/$f.released"
    done
    kills_oldest_first held | all_but_newest "$KILLS_HELD_MAX" | while read -r f; do
        line=""
        read -r line 2> /dev/null < "$DIR/kills/$f.incident" || true
        line=$(printf '%s' "$line" | tr -cd '\40-\176' | cut -c1-200)
        printf '%s retired %s (%s): more than %s frozen rings were held by items not yet read closed\n' \
            "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$f" "${line:-unreadable}" "$KILLS_HELD_MAX" >> "$DIR/kills-retired.log"
        count_one "$DIR/kills-retired"
        rm -rf "${DIR:?}/kills/$f" "$DIR/kills/$f.incident"
        log "RETIRED frozen ring $f, held by ${line:-unreadable}: more than $KILLS_HELD_MAX rings were held" >&2
    done
    # A marker whose ring is gone holds nothing.
    for m in "$DIR"/kills/*.incident "$DIR"/kills/*.released; do
        [ -f "$m" ] || continue
        [ -d "${m%.*}" ] || rm -f "$m"
    done
    if [ -f "$DIR/kills-retired.log" ] && [ "$(wc -l < "$DIR/kills-retired.log")" -gt "$RETIRED_LOG_KEEP" ]; then
        tail -n "$RETIRED_LOG_KEEP" "$DIR/kills-retired.log" > "$DIR/kills-retired.log.writing" \
            && mv "$DIR/kills-retired.log.writing" "$DIR/kills-retired.log"
    fi
    return 0
}

# One held ring whose item is due a read, read: at most one request a
# tick, and each item at most once every HELD_CHECK_S. Only an answer
# that IS the packet and says closed or cancelled releases the ring;
# anything else — no answer, a refusal, a 404 a narrower scope can give,
# a body without a status — is not evidence that the incident ended
# (an unreadable answer is not an empty one), so the ring stays held and
# the item is asked again in an hour.
check_held_ring() {
    local m d kind id checked now url hdr="" code status
    now=$(date -u +%s)
    for m in "$DIR"/kills/*.incident; do
        [ -f "$m" ] || continue
        d="${m%.incident}"
        [ -d "$d" ] || continue
        kind=""; id=""; checked=""
        read -r kind id checked 2> /dev/null < "$m" || true
        [ "$kind" = item ] || continue
        case "${id:-empty}" in empty | *[!A-Za-z0-9-]*) continue ;; esac
        case "${checked:-empty}" in empty | *[!0-9]*) checked=0 ;; esac
        [ $((now - checked)) -ge "$HELD_CHECK_S" ] || continue
        url="${BOSS_JOBS_URL:-$(head -n1 "$HERE/../dev/sor-url" 2> /dev/null || true)}"
        if [ -z "$url" ] || [ ! -r "$HERE/../forge/alert-lib.sh" ]; then return 0; fi
        load_alert_lib "$url"
        if declare -F machine_token_header > /dev/null; then
            machine_token_header hdr "$url" || hdr=""
        fi
        code=$(curl -s -o "$TMP/held-item" -w '%{http_code}' --max-time 5 \
            ${hdr:+-H "$hdr"} -H "x-boss-user: $ALERT_USER" "$url/api/jobs/$id") || code=000
        status=""
        if [ "$code" = 200 ] && jq_doc_file "$TMP/held-item"; then
            status=$(jq -r '.status // empty' "$TMP/held-item" 2> /dev/null) || status=""
        fi
        case "$status" in
            closed | cancelled)
                printf 'item %s %s %s\n' "$id" "$status" "$now" > "$d.released" && rm -f "$m"
                echo "frozen ring ${d##*/} released: item $id is $status" >&2
                ;;
            *)
                printf 'item %s %s\n' "$id" "$now" > "$m"
                ;;
        esac
        return 0
    done
    return 0
}

# The item a 201 answered is the one the filed alert's ring is held by.
# The ring is named by the body's own frozen_ring, and only a directory
# under kills/ is written to.
link_incident() {
    local body="$1" answer="$2" ring id
    ring=$(jq -r '.metadata.frozen_ring // empty' "$body" 2> /dev/null) || ring=""
    case "$ring" in "$DIR/kills/"*) ;; *) return 0 ;; esac
    case "${ring#"$DIR/kills/"}" in '' | *[!A-Za-z0-9._-]*) return 0 ;; esac
    [ -d "$ring" ] || return 0
    id=$(jq -r '.id // empty' "$answer" 2> /dev/null) || id=""
    case "${id:-empty}" in
        empty | *[!A-Za-z0-9-]*)
            echo "alert filed, and its answer named no item: ring ${ring##*/} stays held as pending" >&2
            return 0
            ;;
    esac
    printf 'item %s %s\n' "$id" "$(date -u +%s)" > "$ring.incident"
}

# Keep the alert for one restart in the spool; `file_kept_alerts` sends
# it. Returns non-zero only when nothing could be kept, so the caller
# leaves the count unacted-on and the next tick tries again.
#
# `first` is 1 when no count was ever acted on for this container on
# this pod. A first sight with no sample before the kill is a restart
# that PREDATES THE WATCH — on the pod this was written for, the
# 2026-10-08T10:31:52Z kill the item that asked for this watch already
# records. It is filed, because a restart nobody recorded is the defect,
# but at standard priority and saying what it is in its title, so it
# does not read as a new kill.
keep_restart_alert() {
    local name="$1" prev="$2" count="$3" reason="$4" code="$5" finished="$6" limit="$7" first="$8"
    local frozen fin_epoch f e last="" last_epoch="" peak="" peak_kib=0 kib n=0 oldest="" title detail priority=urgent since out
    local kind spod other=0 other_pod="" other_epoch=""
    frozen=$(frozen_dir "$name" "$count" "$finished")
    # date -d '' answers midnight rather than an error: guard it.
    fin_epoch=""
    [ "$finished" = "-" ] || fin_epoch=$(date -u -d "$finished" +%s 2> /dev/null) || fin_epoch=""
    for f in "$frozen"/*.tsv; do
        [ -f "$f" ] || continue
        e="${f##*/}"; e="${e%.tsv}"
        case "$e" in '' | *[!0-9]*) continue ;; esac
        [ -n "$oldest" ] || oldest="$e"
        # A sample after the kill is the restarted container's, not evidence.
        if [ -n "$fin_epoch" ] && [ "$e" -gt "$fin_epoch" ]; then continue; fi
        # NOR IS A SAMPLE TAKEN IN ANOTHER POD (backlog 36b1aa00, from
        # delta review d617cf4e). The ring is on the PVC and survives a
        # roll, and a new pod's loop starts only with the sidecar's first
        # pass, minutes in; until this asked, a kill in that window was
        # filed with `Last sample, N s before the kill` describing the
        # PREVIOUS pod's container of the same name. The pod is the
        # fourth field of the sample's first line (`sample` writes it);
        # a sample that does not name THIS pod there is counted, and
        # named in the item, and never described.
        kind=""; spod=""
        IFS=$'\t' read -r kind _ _ spod _ 2> /dev/null < "$f" || true
        if [ "$kind" != "at" ] || [ "$spod" != "$POD" ]; then
            other=$((other + 1)); other_epoch="$e"
            other_pod=$(printf '%s' "$spod" | tr -cd 'A-Za-z0-9._-' | cut -c1-80)
            continue
        fi
        n=$((n + 1)); last="$f"; last_epoch="$e"
        kib=$(awk -F'\t' -v name="$name" '$1 == "container" && $2 == name { split($4, b, "="); print b[2] }' "$f")
        case "${kib:-empty}" in empty | *[!0-9]*) continue ;; esac
        if [ "$kib" -gt "$peak_kib" ]; then peak_kib="$kib"; peak="$f"; fi
    done
    since=$((count - prev))
    title="dev pod: container $name restarted — $reason (exit $code) at $finished"
    detail="Container $name of pod $POD restarted: the kubelet recorded $reason, exit code $code, finished $finished; restart count $prev -> $count; memory limit $limit."
    if [ "$since" -gt 1 ] && [ "$first" != 1 ]; then
        title="$title — $since restarts since the last item"
        detail="$detail That is $since restarts since the last item for this container: at most one is filed per container every $((ALERT_WINDOW_S / 60)) minutes, and the restarts in between are counted here; this one describes the newest."
    fi
    if [ -n "$last" ]; then
        if [ -n "$fin_epoch" ]; then
            detail="$detail Last sample, $((fin_epoch - last_epoch)) s before the kill: $(describe_sample "$last" "$name")."
        else
            detail="$detail Newest sample (the kubelet gave no finish time to order it against): $(describe_sample "$last" "$name")."
        fi
        if [ -n "$peak" ] && [ "$peak" != "$last" ]; then
            detail="$detail Largest sample in the ring, $(kib_as_gib "$peak_kib") GiB resident at epoch ${peak##*/}: $(describe_sample "$peak" "$name")."
        fi
        detail="$detail Resident sums count shared pages once per process and see no page cache or tmpfs. Processes are named here without their command lines; those are in the whole ring from before the kill, kept on the dev pod's volume at $frozen ($n samples, one every $INTERVAL s)."
        [ "$other" -eq 0 ] || detail="$detail $other more sample(s) in that ring were taken in another pod before this one (the newest at epoch $other_epoch, in pod ${other_pod:-unnamed}); they are not this container and are not described here."
    elif [ "$other" -gt 0 ]; then
        # Before the `first` arm, and urgent: the ring holding another
        # pod's samples is the evidence that the watch ran before this
        # pod did, so this kill does not predate the watch — it fell in
        # the window before this pod's first sample, and nobody else
        # recorded it.
        detail="$detail No sample of this pod precedes this kill: the $other sample(s) the ring held from before it were taken in another pod (the newest at epoch $other_epoch, in pod ${other_pod:-unnamed}), so the watch had not sampled this pod yet and what filled the container is not recorded. The ring as it stood is kept at $frozen."
    elif [ "$first" = 1 ]; then
        priority=standard
        title="dev pod: container $name restarted before the memory watch ran — $reason (exit $code) at $finished; restart predates this watch, no samples"
        detail="NOT A NEW KILL: this restart predates the memory watch. Container $name of pod $POD shows restart count $count, and the kubelet's record of the last one is $reason, exit code $code, finished $finished; memory limit $limit. The watch found it on its first tick for this pod, so no sample precedes it and what filled the container is not recorded here. If an item already records this restart, that item is the record and this one can be closed against it. The next restart will carry samples."
    else
        detail="$detail No sample precedes this kill: the ring's oldest sample is ${oldest:+epoch }${oldest:-absent}, so the watch was not sampling then and what filled the container is not recorded. The ring as it stood is kept at $frozen."
    fi
    detail=$(printf '%s' "$detail" | tr -cd '\11\12\40-\176' | cut -c1-6000)
    title=$(printf '%s' "$title" | tr -cd '\40-\176\342\200\224' | cut -c1-300)
    mkdir -p "$DIR/alert-spool" || return 1
    out="$DIR/alert-spool/$(date -u +%s)-$$-$RANDOM.json"
    # The same shape infra/forge/alert-lib.sh files — a backlog-item
    # tagged `alert`, the facts in metadata — with the facts ALSO as
    # their own keys, and the owner left for the send to resolve.
    jq -n --arg title "$title" --arg detail "$detail" --arg priority "$priority" \
        --arg at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" --arg actor "$ALERT_ACTOR" \
        --arg container "$name" --arg pod "$POD" --arg reason "$reason" --arg code "$code" \
        --arg finished "$finished" --arg count "$count" --arg since "$since" --arg frozen "$frozen" '
        {kind: "backlog-item", status: "open", owner_id: "", priority: $priority,
         tags: ["alert", "cluster"], subject: {subject_kind: "custom", id: "boss-cluster"},
         title: $title,
         metadata: {area: "alert", filed_by: $actor, raised_at: $at, detail: $detail,
                    container: $container, pod: $pod, reason: $reason, exit_code: $code,
                    finished_at: $finished, restart_count: $count, restarts_in_this_item: $since,
                    frozen_ring: $frozen}}' > "$out.writing" \
        && mv "$out.writing" "$out" || { rm -f "$out.writing"; return 1; }
    # From here the ring is this alert's incident's (see `prune_kills`):
    # pending until the alert is filed and the answer names its item.
    [ ! -d "$frozen" ] || [ -f "$frozen.incident" ] || printf 'pending\n' > "$frozen.incident"
    echo "ALERT kept for sending: $title" >&2
    # The spool's bound. One item per container per window keeps it far
    # under this; past it the OLDEST go, counted, because the newest
    # restart is the one a reader needs.
    ls -1 "$DIR/alert-spool" | grep '\.json$' | sort | all_but_newest "$SPOOL_KEEP" | while read -r f; do
        rm -f "$DIR/alert-spool/$f" "$DIR/alert-spool/$f.tries"
        count_one "$DIR/alerts-dropped"
    done
    return 0
}

# Add one to the whole number a file holds.
count_one() {
    local n=0
    if [ -r "$1" ]; then read -r n < "$1" || n=0; fi
    case "${n:-empty}" in empty | *[!0-9]*) n=0 ;; esac
    printf '%s\n' "$((n + 1))" > "$1"
}

# Who signs, and the two things infra/forge/alert-lib.sh already knows
# how to do for an alert: resolve the platform owner from the people
# registry, and present the machine token on that read. Signed as the
# sidecar's own registered actor — this loop is the sidecar's.
ALERT_ACTOR="automation:dev-scratch-reclaim"
ALERT_LIB_LOADED=""
load_alert_lib() {
    [ -z "$ALERT_LIB_LOADED" ] || return 0
    export JOBS_API="$1"
    # shellcheck source=infra/forge/alert-lib.sh
    . "$HERE/../forge/alert-lib.sh"
    ALERT_LIB_LOADED=1
}

# Send what the spool holds, oldest first. Three answers, three acts:
#   201             filed; the file goes.
#   an answer that  no answer, a 5xx, or a refusal of the CALLER (401,
#   is not about    403, 407, 408, 429): stop, and count nothing against
#   this body       the alert. That the alerts behind it would get the
#                   same answer is an ASSUMPTION, true of a dark API or
#                   a refused signer (every alert here is one signer and
#                   one kind) and not of a 5xx this one body provokes,
#                   which holds every alert behind it (review fb97dd5d,
#                   N2). Neither case is counted, so neither ends by
#                   itself: --state names the oldest kept alert's age,
#                   and past HELD_AFTER_S the reclaim pass raises it as
#                   memory-watch-alerts-held (the same review, F1).
#                   alert-lib.sh decides which answers those are, in
#                   alert_answer_judges_the_body, for this spool and the
#                   forge's alike. Until it did here (backlog 9ecd12ea,
#                   residue named by car 97edf615), a stale machine token
#                   or a failing API moved a kill alert aside at its
#                   fifth answer, half an hour in, and it was never filed.
#   any other code  REFUSED, the API's judgement of the body (review of
#                   car 7849553a, finding 3): it backs off (2, 4, 8, 16
#                   minutes) and the ones behind it are still tried; at
#                   the fifth refusal it is moved aside to alert-refused/
#                   with the answer it got, kept on the volume and
#                   counted on the reclaim's packet.
file_kept_alerts() {
    local url f tries n next now owner code
    ls "$DIR/alert-spool"/*.json > /dev/null 2>&1 || return 0
    url="${BOSS_JOBS_URL:-$(head -n1 "$HERE/../dev/sor-url" 2> /dev/null || true)}"
    if [ -z "$url" ] || [ ! -r "$HERE/../forge/alert-lib.sh" ]; then
        log "alerts are kept and cannot be sent: no system of record named (BOSS_JOBS_URL unset, $HERE/../dev/sor-url absent) or alert-lib.sh missing" >&2
        return 0
    fi
    load_alert_lib "$url"
    alert_machine_headers
    MT_HDR=""
    if declare -F machine_token_header > /dev/null; then
        machine_token_header MT_HDR "$url" || MT_HDR=""
    fi
    owner=$(alert_owner)
    now=$(date -u +%s)
    for f in "$DIR/alert-spool"/*.json; do
        [ -f "$f" ] || continue
        tries="$f.tries"; n=0; next=0
        if [ -r "$tries" ]; then read -r n next < "$tries" || true; fi
        case "${n:-empty}" in empty | *[!0-9]*) n=0 ;; esac
        case "${next:-empty}" in empty | *[!0-9]*) next=0 ;; esac
        [ "$now" -ge "$next" ] || continue
        code=000
        if jq --arg o "$owner" '.owner_id = $o' "$f" > "$TMP/alert-body" 2> /dev/null; then
            code=$(curl -s -o "$TMP/alert-answer" -w '%{http_code}' --max-time 15 -X POST \
                -H 'content-type: application/json' ${MT_HDR:+-H "$MT_HDR"} -H "x-boss-user: $ALERT_USER" \
                --data-binary "@$TMP/alert-body" "$url/api/jobs") || code=000
        else
            code=unreadable
        fi
        case "$code" in
            201)
                link_incident "$f" "$TMP/alert-answer"
                rm -f "$f" "$tries"
                echo "alert filed: ${f##*/}" >&2
                ;;
            000 | '')
                echo "alert: the jobs API did not answer — $(ls -1 "$DIR/alert-spool" | grep -c '\.json$') kept for the next tick" >&2
                return 0
                ;;
            *)
                if ! alert_answer_judges_the_body "$code"; then
                    echo "alert: send stopped at ${f##*/} (answer $code, which judges the API or the caller, not this alert) — $(ls -1 "$DIR/alert-spool" | grep -c '\.json$') kept for the next tick, nothing counted against them" >&2
                    return 0
                fi
                n=$((n + 1))
                if [ "$n" -ge "$REFUSALS_MAX" ]; then
                    mkdir -p "$DIR/alert-refused"
                    { echo "refused $n times, last answer $code:"; head -c 2000 "$TMP/alert-answer" 2> /dev/null; } > "$DIR/alert-refused/${f##*/}.why"
                    mv "$f" "$DIR/alert-refused/" && rm -f "$tries"
                    echo "alert REFUSED $n times (last answer $code): ${f##*/} moved aside to $DIR/alert-refused, so the alerts behind it pass" >&2
                    ls -1tr "$DIR/alert-refused" | grep '\.json$' | all_but_newest "$REFUSED_KEEP" | while read -r old; do
                        rm -f "$DIR/alert-refused/$old" "$DIR/alert-refused/$old.why"
                    done
                else
                    printf '%s %s\n' "$n" "$((now + 60 * (1 << n)))" > "$tries"
                    echo "alert refused (answer $code, refusal $n of $REFUSALS_MAX): ${f##*/} waits $((1 << n)) min; the ones behind it are still tried" >&2
                fi
                ;;
        esac
    done
}

# One container's line of state on the volume: the restart count last
# put in an item, when that item was kept, and the count whose ring is
# frozen. (A file written before this held three numbers holds one.)
check_restarts() {
    local uid name id count reason code finished limit prev alerted frozen_n seenf first now frozen
    [ -s "$TMP/containers" ] || return 0
    uid=$(jq -r '.metadata.uid // "pod"' "$TMP/pod.json" | tr -cd 'A-Za-z0-9-')
    now=$(date -u +%s)
    while IFS=$'\t' read -r name id count reason code finished limit; do
        name=$(printf '%s' "$name" | tr -cd 'A-Za-z0-9._-')
        case "${count:-empty}" in empty | *[!0-9]*) continue ;; esac
        [ -n "$name" ] || continue
        seenf="$DIR/seen/$uid.$name"
        prev=0; alerted=0; frozen_n=0; first=1
        if [ -r "$seenf" ]; then
            first=0
            read -r prev alerted frozen_n < "$seenf" || true
        fi
        case "${prev:-empty}" in empty | *[!0-9]*) prev=0 ;; esac
        case "${alerted:-empty}" in empty | *[!0-9]*) alerted=0 ;; esac
        case "${frozen_n:-empty}" in empty | *[!0-9]*) frozen_n=0 ;; esac
        [ "$count" -gt "$prev" ] || continue
        mkdir -p "$DIR/seen" || continue
        # Every restart's ring is frozen when it is first seen, whether
        # or not it gets an item of its own.
        if [ "$count" -gt "$frozen_n" ]; then
            frozen=$(frozen_dir "$name" "$count" "$finished")
            freeze_ring "$frozen" || continue
            frozen_n="$count"
            # Not for a first sight: the file's existence is what
            # `first` reads, so it is written only with that item.
            [ "$first" = 1 ] || printf '%s %s %s\n' "$prev" "$alerted" "$frozen_n" > "$seenf"
        fi
        # ONE ITEM PER CONTAINER PER WINDOW (review finding 4): a
        # container in CrashLoopBackOff restarts about every five
        # minutes, and twelve urgent items an hour bury the one that
        # matters. Inside the window the count is left unacted-on, so
        # the next item says `prev -> count` across all of them.
        if [ $((now - alerted)) -lt "$ALERT_WINDOW_S" ]; then continue; fi
        if keep_restart_alert "$name" "$prev" "$count" "$reason" "$code" "$finished" "$limit" "$first"; then
            printf '%s %s %s\n' "$count" "$now" "$frozen_n" > "$seenf"
        fi
    done < "$TMP/containers"
    # After the alerts are kept, so a ring frozen for one is already
    # marked held when the bound is applied.
    [ -z "$FROZE" ] || prune_kills
    # State for pods that are gone: a week is longer than any question
    # about one.
    find "$DIR/seen" -type f ! -name "$uid.*" -mtime +7 -delete 2> /dev/null || true
}

# The loop's own lines go to watch.log on the volume, where the dev
# session reads them without kubectl. Trimmed here, on every tick and on
# its SIZE (it used to be trimmed only when a loop started, which a
# healthy loop never does again), and in place, so a loop holding it
# open in append mode keeps writing to the same file.
trim_log() {
    [ -f "$DIR/watch.log" ] || return 0
    if [ "$(wc -c 2> /dev/null < "$DIR/watch.log" || echo 0)" -gt "$LOG_MAX_BYTES" ]; then
        tail -c "$((LOG_MAX_BYTES / 4))" "$DIR/watch.log" > "$TMP/log-tail" && cat "$TMP/log-tail" > "$DIR/watch.log"
    fi
}

tick() {
    mkdir -p "$DIR" || { log "cannot create $DIR — nothing sampled" >&2; return 1; }
    # One tick at a time: a hand-run beside the loop must not raise the
    # same restart twice.
    if command -v flock > /dev/null 2>&1 && { exec 8>> "$DIR/.tick.lock"; } 2> /dev/null; then
        flock -n 8 || return 0
    fi
    trim_log
    : > "$TMP/containers"
    if pod_status "$TMP/pod.json"; then
        containers_of "$TMP/pod.json" > "$TMP/containers" || : > "$TMP/containers"
    fi
    # The restart check runs BEFORE this tick's sample is written, so a
    # frozen ring holds only what was sampled before the restart was seen.
    check_restarts
    # Sample BEFORE sending: a send waits on the system of record (the
    # owner read up to 5 s, each POST up to 15), and a tick that sent
    # first left a healthy loop with no sample for that long — which the
    # starting pass could record as "not sampling" (delta review, N5).
    # check_restarts above only keeps; it asks the network nothing.
    sample || { log "could not write a sample under $DIR/ring" >&2; file_kept_alerts; return 1; }
    file_kept_alerts
    check_held_ring
}

# Is <pid> a running loop of this script? The REAL /proc, whatever
# PROC_ROOT says: a pidfile outlives its process and pids are reused.
loop_alive() {
    local cmd
    case "${1:-empty}" in empty | *[!0-9]*) return 1 ;; esac
    [ -r "/proc/$1/cmdline" ] || return 1
    cmd=$(tr '\0' ' ' 2> /dev/null < "/proc/$1/cmdline") || return 1
    case "$cmd" in *"dev-memory-watch.sh --loop"*) return 0 ;; esac
    return 1
}

# The newest sample's age in seconds; empty when the ring holds none.
newest_sample_age() {
    local newest e
    newest=$(ls -1 "$DIR/ring" 2> /dev/null | grep '\.tsv$' | sort | awk 'END { print }')
    e="${newest%.tsv}"
    case "${e:-empty}" in empty | *[!0-9]*) return 0 ;; esac
    echo "$(($(date -u +%s) - e))"
}

# What --ensure decided, said on stdout AND left on the volume: every
# way it declines returns 0, so without this the only trace of a watch
# that never started was a line on the sidecar's stdout (review finding
# 2). `--state` reads it back for the reclaim's packet.
ensure_says() {
    log "$*"
    printf '%s\n' "$*" > "$DIR/ensure.result"
}

ensure() {
    local pid="" gen="" id p cg outside="" age start wait_s sampled
    mkdir -p "$DIR" || { log "cannot create $DIR — the watch is not running" >&2; return 1; }
    if [ -r "$DIR/watch.pid" ]; then read -r pid gen < "$DIR/watch.pid" || true; fi
    if loop_alive "$pid"; then
        if [ "$gen" = "$LOOP_GENERATION" ]; then
            ensure_says "sampling already (pid $pid, every $INTERVAL s, ring at $DIR/ring)"
            return 0
        fi
        log "replacing the running loop (pid $pid, generation ${gen:-none}) with generation $LOOP_GENERATION"
        kill "$pid" 2> /dev/null || true
    fi
    if ! pod_status "$TMP/pod.json"; then
        ensure_says "not started: $POD_UNREAD — without the pod's status this cannot tell which container it stands in"
        return 0
    fi
    id=$(containers_of "$TMP/pod.json" | awk -F'\t' -v c="$WATCHED" '$1 == c {print $2}')
    if [ -z "$id" ] || [ "$id" = "-" ]; then
        ensure_says "not started: pod $POD has no running container named $WATCHED"
        return 0
    fi
    # From inside the watched container its own processes show the
    # cgroup root, so none carries its id; from any other container
    # they all do.
    for p in "$PROC_ROOT"/[0-9]*; do
        read -r cg 2> /dev/null < "$p/cgroup" || continue
        if [ "${cg##*/}" = "$id" ]; then outside=1; break; fi
    done
    if [ -z "$outside" ]; then
        # No ensure.result here: this is a hand-run or a test INSIDE the
        # watched container, and it must not overwrite what the sidecar
        # last decided.
        log "not started here: no process stands in another cgroup named for container $WATCHED, so this IS that container — a sampler started inside it is killed with the evidence; the reclaim sidecar's hourly pass starts it"
        return 0
    fi
    bash "$SELF" --loop < /dev/null >> "$DIR/watch.log" 2>&1 &
    pid=$!
    printf '%s %s\n' "$pid" "$LOOP_GENERATION" > "$DIR/watch.pid"
    # Started is a claim; a sample is the evidence — one written AFTER
    # this start, not a dead loop's leftover. The only wait a tick has
    # before it samples is its read of the pod's status (the sends come
    # after, N5), so that read's own bound plus a margin is how long the
    # first sample is waited for; and when none comes, this says so
    # rather than saying started.
    start=$(date -u +%s)
    wait_s=$((POD_READ_MAX_S + 4))
    sampled=""
    while [ "$(($(date -u +%s) - start))" -lt "$wait_s" ]; do
        age=$(newest_sample_age)
        if [ -n "$age" ] && [ "$age" -le "$(($(date -u +%s) - start))" ]; then sampled=1; break; fi
        sleep 0.5
    done
    if [ -n "$sampled" ]; then
        ensure_says "started (pid $pid, every $INTERVAL s, newest $RING_KEEP samples at $DIR/ring, its own lines in $DIR/watch.log)"
    else
        ensure_says "started (pid $pid) and no sample was written within $wait_s s — see $DIR/watch.log"
    fi
}

# Seconds since the oldest alert in the spool was kept, empty when the
# spool holds none. Kept alerts are named `<epoch kept>-<pid>-<n>.json`;
# a name that does not start with an epoch falls back to the file's
# mtime, so an odd name is still measured rather than skipped.
oldest_kept_age() {
    local f e oldest="" now
    for f in "$DIR/alert-spool"/*.json; do
        [ -f "$f" ] || continue
        e="${f##*/}"
        e="${e%%-*}"
        case "${e:-empty}" in empty | *[!0-9]*) e=$(stat -c %Y "$f" 2> /dev/null) ;; esac
        case "${e:-empty}" in empty | *[!0-9]*) continue ;; esac
        if [ -z "$oldest" ] || [ "$e" -lt "$oldest" ]; then oldest="$e"; fi
    done
    [ -n "$oldest" ] || return 0
    now=$(date -u +%s)
    if [ "$oldest" -gt "$now" ]; then echo 0; else echo "$((now - oldest))"; fi
}

# The watch's state as `key=value` lines, for the reclaim pass to put on
# the maintenance packet it files — the one path by which a watch that
# is not sampling, or alerts that are not leaving, reach a reader who is
# not looking at this volume. Reads files and /proc; changes nothing.
# `memory_watch` is the verdict the car's proof reads: `sampling` only
# when a loop of this generation's pidfile is alive AND its newest
# sample is younger than four intervals.
state() {
    local pid="" gen="" age kept refused dropped=0 said="" verdict
    if [ -r "$DIR/watch.pid" ]; then read -r pid gen < "$DIR/watch.pid" || true; fi
    age=$(newest_sample_age)
    kept=$(ls -1 "$DIR/alert-spool" 2> /dev/null | grep -c '\.json$')
    refused=$(ls -1 "$DIR/alert-refused" 2> /dev/null | grep -c '\.json$')
    if [ -r "$DIR/alerts-dropped" ]; then read -r dropped < "$DIR/alerts-dropped" || dropped=0; fi
    if [ -r "$DIR/ensure.result" ]; then read -r said < "$DIR/ensure.result" || true; fi
    if ! loop_alive "$pid"; then
        verdict="not sampling: no loop is alive"
        pid=""
    elif [ -z "$age" ]; then
        verdict="not sampling: the loop is alive and the ring holds no sample"
    elif [ "$age" -gt "$((INTERVAL * 4))" ]; then
        verdict="not sampling: the loop is alive and its newest sample is $age s old"
    else
        verdict="sampling"
    fi
    printf 'memory_watch=%s\n' "$verdict"
    printf 'memory_watch_pid=%s\n' "$pid"
    printf 'memory_watch_generation=%s\n' "${gen:-}"
    printf 'memory_watch_sample_age_s=%s\n' "$age"
    printf 'memory_watch_alerts_kept=%s\n' "${kept:-0}"
    printf 'memory_watch_alerts_refused=%s\n' "${refused:-0}"
    printf 'memory_watch_alerts_dropped=%s\n' "${dropped:-0}"
    # How long the oldest kept alert has waited, from when it was kept
    # (its name's leading epoch) — not from its last try — and the bound
    # past which the reclaim pass raises it (HELD_AFTER_S above).
    printf 'memory_watch_alerts_oldest_kept_age_s=%s\n' "$(oldest_kept_age)"
    printf 'memory_watch_alerts_held_after_s=%s\n' "$HELD_AFTER_S"
    # Frozen rings an item not yet read closed holds, and how many were
    # retired past KILLS_HELD_MAX since the last acknowledgement — each
    # named in kills-retired.log (backlog 0143612a, `prune_kills`).
    local held=0 retired=0 m
    for m in "$DIR"/kills/*.incident; do
        [ ! -d "${m%.incident}" ] || held=$((held + 1))
    done
    if [ -r "$DIR/kills-retired" ]; then read -r retired < "$DIR/kills-retired" || retired=0; fi
    case "${retired:-empty}" in empty | *[!0-9]*) retired=0 ;; esac
    printf 'memory_watch_rings_held=%s\n' "$held"
    printf 'memory_watch_rings_retired=%s\n' "$retired"
    printf 'memory_watch_ensure=%s\n' "$(printf '%s' "$said" | tr -cd '\40-\176' | cut -c1-400)"
    # Where all of the above lives, so the packet that counts a refused
    # alert also says where to read it.
    printf 'memory_watch_dir=%s\n' "$DIR"
    # Acknowledgements no packet has carried yet: their names (what
    # --recorded takes back) and what each one says.
    # Only a name --recorded would take back is listed (review 1cd6b7a1,
    # finding 3; backlog 8aff9d6b): a hand-made folder whose name held a
    # newline added a key=value line of its own to the packet.
    local stamps="" said_acks="" d name line
    for d in "$DIR"/acknowledged/*/; do
        [ -f "$d/unrecorded" ] || continue
        name="${d%/}"
        name="${name##*/}"
        case "$name" in '' | *[!A-Za-z0-9-]*) continue ;; esac
        line=""
        read -r line 2> /dev/null < "$d/record" || true
        stamps="${stamps:+$stamps }$name"
        said_acks="${said_acks:+$said_acks; }$name $line"
    done
    printf 'memory_watch_acknowledgements=%s\n' "$stamps"
    printf 'memory_watch_acknowledged=%s\n' "$(printf '%s' "$said_acks" | tr -cd '\40-\176' | cut -c1-400)"
}

# THE ACKNOWLEDGEMENT (backlog 9ecd12ea; the operator call recorded on
# it when car 7849553a was released, 2026-10-08). A refused or dropped
# alert is a kill notice nobody received, so the reclaim pass counts it
# as a problem every hour and nothing ages it out. Until this verb the
# only way to clear it was to empty alert-refused/ and remove
# alerts-dropped by hand: an act no record saw, which also destroyed
# the refused bodies — the one copy of what the kill alert said.
# `--acknowledge` moves the bodies (with each refusal's answer) and the
# dropped count into acknowledged/<when>/, beside one line saying who
# and how many; the counts read zero from then on. The acknowledgement
# stays named on --state until the reclaim pass reports it carried it
# on a packet (`--recorded <name>...`), so the clearing is on the
# record and not only on this volume. Who: BOSS_ACTOR, else the box's
# actor file — the same rule every `boss` verb signs by.
ACK_KEEP=20
acknowledge() {
    local refused dropped=0 dropped_said stamp dest who f when moved=0
    mkdir -p "$DIR" || return 1
    # Under the tick's lock: a tick moves a body INTO alert-refused/.
    if command -v flock > /dev/null 2>&1 && { exec 8>> "$DIR/.tick.lock"; } 2> /dev/null; then
        flock -w 60 8 || { log "a tick held the lock for 60 s — nothing acknowledged" >&2; return 1; }
    fi
    refused=$(ls -1 "$DIR/alert-refused" 2> /dev/null | grep -c '\.json$')
    # The count is read exactly as --state reads it, and anything --state
    # would print as other than zero is something to acknowledge — the
    # reclaim pass's own test (review 1cd6b7a1, finding 1; backlog
    # 8aff9d6b). This verb read a count that is not a number as 0 and
    # said there was nothing to acknowledge, while the reclaim flagged it
    # every hour: the verb could not clear the problem it is named in.
    if [ -r "$DIR/alerts-dropped" ]; then read -r dropped < "$DIR/alerts-dropped" || dropped=0; fi
    dropped="${dropped:-0}"
    case "$dropped" in
        *[!0-9]*) dropped_said="a dropped count that is not a number, kept as it was" ;;
        *) dropped_said="$dropped dropped" ;;
    esac
    # And frozen rings retired while an item held them (backlog
    # 0143612a): the count is cleared, the names move with the record.
    # Read as --state reads it, which prints a garbled count as 0.
    local retired=0
    if [ -r "$DIR/kills-retired" ]; then read -r retired < "$DIR/kills-retired" || retired=0; fi
    case "${retired:-empty}" in empty | *[!0-9]*) retired=0 ;; esac
    case "${refused:-0}$dropped$retired" in
        *[!0]*) ;;
        *)
            log "nothing to acknowledge: no refused alert under $DIR/alert-refused, none dropped and no frozen ring retired"
            return 0
            ;;
    esac
    who="${BOSS_ACTOR:-$(head -n1 "${HOME:-/nonexistent}/.config/boss/actor" 2> /dev/null)}"
    who=$(printf '%s' "${who:-unidentified}" | tr -cd 'A-Za-z0-9@._:-')
    stamp=$(date -u +%Y%m%dT%H%M%SZ)
    when=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    dest="$DIR/acknowledged/$stamp"
    [ ! -e "$dest" ] || dest="$dest-$$"
    mkdir -p "$dest" || return 1
    # The mark BEFORE the first move (review 1cd6b7a1, finding 2): the
    # bodies were moved first, so a move that failed part-way left a
    # folder holding them with neither record nor mark — absent from
    # --state, and deleted by the ACK_KEEP bound below on a later
    # acknowledgement, the one copy of each kill alert with it. Marked,
    # it stays named on --state and the bound never takes it; a stop
    # says in its record what was moved and what was not, and what was
    # not is still counted where it was, so the problem stays raised.
    : > "$dest/unrecorded" || { rmdir "$dest" 2> /dev/null; return 1; }
    for f in "$DIR/alert-refused"/*; do
        [ -e "$f" ] || continue
        if ! mv "$f" "$dest/"; then
            printf 'acknowledgement %s by %s stopped part-way: %s of %s refused moved here, the rest still in alert-refused/, the dropped count not touched\n' \
                "$when" "$who" "$moved" "$refused" > "$dest/record"
            log "acknowledgement by $who stopped part-way: $moved of $refused refused alert(s) moved to $dest, the rest still in $DIR/alert-refused" >&2
            return 1
        fi
        case "$f" in *.json) moved=$((moved + 1)) ;; esac
    done
    # Moved, not retyped: a count that is not a number is kept as it was.
    if [ -e "$DIR/alerts-dropped" ]; then
        if ! mv "$DIR/alerts-dropped" "$dest/dropped"; then
            printf 'acknowledgement %s by %s stopped part-way: %s of %s refused moved here, the dropped count still at alerts-dropped\n' \
                "$when" "$who" "$moved" "$refused" > "$dest/record"
            log "acknowledgement by $who stopped part-way: the dropped count could not be moved to $dest" >&2
            return 1
        fi
    else
        printf '0\n' > "$dest/dropped"
    fi
    # The retired rings' names, under the same mark: a stop here says so
    # in the record, and the retired count stays where --state reads it.
    if [ -f "$DIR/kills-retired.log" ] && ! mv "$DIR/kills-retired.log" "$dest/"; then
        printf 'acknowledgement %s by %s stopped part-way: %s of %s refused and the dropped count moved here, the retired rings still named in kills-retired.log\n' \
            "$when" "$who" "$moved" "$refused" > "$dest/record"
        log "acknowledgement by $who stopped part-way: kills-retired.log could not be moved to $dest" >&2
        return 1
    fi
    printf '%s\n' "$retired" > "$dest/kills-retired" && rm -f "$DIR/kills-retired"
    printf 'acknowledged %s by %s: %s refused kept here, %s, %s frozen ring(s) retired\n' \
        "$when" "$who" "$refused" "$dropped_said" "$retired" > "$dest/record"
    log "acknowledged by $who: $refused refused alert(s) kept at $dest, $dropped_said, $retired frozen ring(s) retired — the next reclaim packet carries it"
    # The bound: the newest ACK_KEEP, never one no packet has carried.
    ls -1 "$DIR/acknowledged" | sort | all_but_newest "$ACK_KEEP" | while read -r f; do
        [ -f "$DIR/acknowledged/$f/unrecorded" ] || rm -rf "${DIR:?}/acknowledged/$f"
    done
}

# The reclaim pass's word that its packet carried these acknowledgements.
# A name that is not one (empty, a path) is skipped, never followed.
recorded() {
    local name
    [ "$#" -gt 0 ] || { echo "usage: dev-memory-watch.sh --recorded <acknowledgement>..." >&2; return 64; }
    for name in "$@"; do
        case "$name" in '' | *[!A-Za-z0-9-]*) continue ;; esac
        rm -f "$DIR/acknowledged/$name/unrecorded"
    done
}

case "${1:-}" in
    --loop)
        while :; do
            bash "$SELF" --tick || true
            sleep "$INTERVAL"
        done
        ;;
    --tick | --ensure)
        TMP=$(mktemp -d) || exit 1
        trap 'rm -rf "$TMP"' EXIT
        if [ "$1" = "--tick" ]; then tick; else ensure; fi
        ;;
    --state)
        state
        ;;
    --acknowledge)
        acknowledge
        ;;
    --recorded)
        shift
        recorded "$@"
        ;;
    *)
        echo "usage: dev-memory-watch.sh --tick | --loop | --ensure | --state | --acknowledge | --recorded <acknowledgement>..." >&2
        exit 64
        ;;
esac
