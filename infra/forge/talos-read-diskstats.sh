#!/usr/bin/env bash
# talos-read-diskstats.sh — READ-ONLY: per-device write and flush latency
# of one cluster node, from the kernel's own counters, read twice through
# the Talos API. The ops verb `talos-read-diskstats`.
#
#   talos-read-diskstats.sh <node>
#
# WHY IT EXISTS (backlog ba0b7308; design f29798c0 §7, car 1). On
# 2026-10-29 PostgreSQL's WAL writes took about 8 s each in stalls
# (d9b39685 measured_20261029T1933Z), and the cause was placed on the
# control-plane system disks — SAMSUNG MZALQ256 OEM client NVMe on cp-1
# and cp-2 — from the drive CLASS alone: "INFERRED from the drive class,
# not measured per device: no BOSS door records per-device latency"
# (d9b39685 disks_20261029T1957Z, gap 803e10ed). The kernel already keeps
# the figures. Each /proc/diskstats line counts, per device, the writes
# completed and the milliseconds spent on them, and since kernel 5.5 the
# flushes completed and the milliseconds spent flushing — a flush is what
# an fsync becomes at the device. Two reads a fixed interval apart give,
# per device, the interval's writes, flushes and time spent, and from
# them the MEAN write latency and the MEAN flush latency. The Talos
# `disks` resource names the drive (model, serial, size, transport), as
# `talos-get <node> disks` does, so the figure lands beside the drive it
# describes.
#
# WHAT A FIGURE MEANS, said here so a reader does not over-read it. The
# kernel's "time spent" is the sum of each request's own time, so
# Δms ÷ Δcount is the mean per request over the interval — a mean, never
# a percentile: one 8 s flush among 400 fast ones reads as a 20 ms mean.
# A device that completed nothing in the interval has NO mean, and says
# so, rather than a 0.00 that reads as a fast disk. Rates (per second,
# MB/s, busy %) divide by the measured gap between the two reads, which
# includes a talosctl call's own time; the means do not depend on it.
# iSCSI `VIRTUAL-DISK` devices on this cluster are Longhorn volumes as the
# node that attached them sees them, so a database volume's flush latency
# reads here beside the physical disk under it.
#
# A WRAP IS NOT A RESET (review c129255c, N1). The kernel prints the time
# fields — 7, 11, 13, 14, 18 and 20 — as a 32-bit `%u` (block/genhd.c
# diskstats_show), while the I/O counts are 64-bit. Summed per-request
# time on a busy device with a deep queue passes 2^32 ms in days, so an
# interval can span a wrap and read as a negative delta. A time field
# that fell is taken modulo 2^32, and the line says so, only when both
# reads fit 32 bits and the requests it accrues with advanced (time
# accrues only as requests finish; a second wrap inside 30 s would need
# ~140,000 requests in flight). Any count that fell, or a fallen time
# with nothing completed, is a reset or a re-created device, and gets no
# figure.
#
# THE BOUND. The node is one DNS label the estate registry declares, not
# retired, with a `talos-*` role (a control plane may be READ); its
# address is the registry's, one IPv4 literal (cluster-node-lib.sh). The
# three talosctl calls are literals — `get disks -o json` and `read
# /proc/diskstats` twice — through the ops_talosctl door, each bounded
# there; no path, resource or flag comes from the packet. The interval is
# fixed at 30 s; BOSS_DISKSTATS_INTERVAL_S exists only so a test does not
# sleep, and is refused outside 1..60 (the ops runner passes a verb no
# environment from the packet). Nothing is written.
#
# THE RECORD. The two reads of every reported device are printed after
# the figures, as the kernel wrote them: a mean is a reduction, and the
# counters are what a later reader recomputes it from.
#
# Exit 78 is a refusal (the request was wrong); exit 1 a read that could
# not look — talosctl's own words, or an answer that is not
# /proc/diskstats. No evidence is not a pass.
set -uo pipefail

ME=talos-read-diskstats
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=infra/forge/cluster-node-lib.sh
. "$HERE/cluster-node-lib.sh"

[ $# -eq 1 ] || node_refuse "usage: talos-read-diskstats.sh <node> — one node, nothing else"
NODE="$1"
node_check_id "$NODE"
INTERVAL="${BOSS_DISKSTATS_INTERVAL_S-30}"
[[ "$INTERVAL" =~ ^[0-9]{1,2}$ ]] && [ "$INTERVAL" -ge 1 ] && [ "$INTERVAL" -le 60 ] \
    || node_refuse "BOSS_DISKSTATS_INTERVAL_S must be whole seconds from 1 to 60, got '$INTERVAL'"
command -v jq >/dev/null 2>&1 || node_fail "jq is not on PATH — the registry cannot be read"

WORK="$(mktemp -d)" || node_fail "cannot make a scratch directory"
trap 'rm -rf "$WORK"' EXIT

node_read_registry "$NODE"
case "$NODE_ROLE" in
    talos-*) ;;
    *) node_refuse "the estate registry says $NODE is a ${NODE_ROLE:-node with no role}, not a Talos node — there is no Talos API to read" ;;
esac

node_door
T="$(ops_talosctl)"

# talos_call <out> <talosctl words…> — one bounded call, its words kept.
talos_call() {
    local out="$1"
    shift
    # shellcheck disable=SC2086 # T is one line the door prints to be word-split
    if ! $T -n "$NODE_ADDRESS" "$@" < /dev/null > "$out" 2> "$WORK/err"; then
        sed 's/^/    /' "$WORK/err" >&2
        node_fail "talosctl -n $NODE_ADDRESS $* could not read (its words above)"
    fi
}

# The drives, by kernel name: id, model, serial, size as Talos prints it,
# transport. A disk Talos does not list is still reported, named so.
talos_call "$WORK/disks.json" get disks -o json
jq -rs '.[] | select((.metadata.id // "") != "")
        | [.metadata.id, (.spec.model // ""), (.spec.serial // ""), (.spec.pretty_size // ""), (.spec.transport // "")]
        | map(tostring | gsub("[\t\n]"; " ")) | @tsv' "$WORK/disks.json" > "$WORK/disks.tsv" 2> "$WORK/disks.jq" \
    || node_fail "talosctl -n $NODE_ADDRESS get disks -o json did not answer the disks resource: $(tr '\n' ' ' < "$WORK/disks.jq" | cut -c1-300)"

talos_call "$WORK/read-1" read /proc/diskstats
T1="$(date +%s%N)"
sleep "$INTERVAL"
talos_call "$WORK/read-2" read /proc/diskstats
T2="$(date +%s%N)"
ELAPSED_MS=$(( (T2 - T1) / 1000000 ))
[ "$ELAPSED_MS" -gt 0 ] || node_fail "the two reads were not apart in time (${ELAPSED_MS} ms) — the clock cannot be trusted for rates"

printf '%s: /proc/diskstats on %s (%s, %s), read twice %s s apart through the Talos API\n' \
    "$ME" "$NODE" "$NODE_ADDRESS" "$NODE_ROLE" "$(awk -v ms="$ELAPSED_MS" 'BEGIN { printf "%.1f", ms / 1000 }')"

awk -v ms="$ELAPSED_MS" -v dfile="$WORK/disks.tsv" '
function skipped(name) { return name ~ /^(loop|ram|zram)[0-9]+$/ }
function describe(name,    disk, rest, s) {
    if (name in isdisk) {
        s = (model[name] != "" ? model[name] : "no model in the disks resource")
        if (serial[name] != "") s = s ", serial " serial[name]
        if (psize[name] != "") s = s ", " psize[name]
        if (transport[name] != "") s = s ", " transport[name]
        return s
    }
    for (disk in isdisk) {
        if (index(name, disk) == 1) {
            rest = substr(name, length(disk) + 1)
            if (rest ~ /^p?[0-9]+$/) return "partition of " disk (model[disk] != "" ? " (" model[disk] ")" : "")
        }
    }
    return "not in the Talos disks resource"
}
BEGIN {
    # The time fields the kernel prints as a 32-bit %u, each with the
    # count it accrues alongside (0: any completion; see the header).
    TWO32 = 4294967296
    timeof[7] = 4;  timename[7] = "read ms (field 7)"
    timeof[11] = 8; timename[11] = "write ms (field 11)"
    timeof[13] = 0; timename[13] = "io ms (field 13)"
    timeof[14] = 0; timename[14] = "weighted io ms (field 14)"
    timeof[18] = 15; timename[18] = "discard ms (field 18)"
    timeof[20] = 19; timename[20] = "flush ms (field 20)"
    while ((getline line < dfile) > 0) {
        split(line, f, "\t")
        isdisk[f[1]] = 1; model[f[1]] = f[2]; serial[f[1]] = f[3]; psize[f[1]] = f[4]; transport[f[1]] = f[5]
    }
    close(dfile)
}
{
    # A /proc/diskstats line: major, minor, name, then at least 11
    # counters, every one a whole number. Anything else is not one.
    if (NF < 14 || $1 !~ /^[0-9]+$/ || $2 !~ /^[0-9]+$/) next
    for (i = 4; i <= NF; i++) if ($i !~ /^[0-9]+$/) next
    w = (FILENAME == ARGV[1]) ? 1 : 2
    name = $3
    lines[w]++
    if (skipped(name)) next
    if (!(name in seen)) { seen[name] = 1; order[++nd] = name }
    nf[w, name] = NF
    raw = $1
    for (i = 2; i <= NF; i++) { v[w, name, i] = $i; raw = raw " " $i }
    rawline[w, name] = raw
}
END {
    if (lines[1] == 0 || lines[2] == 0) exit 3
    secs = ms / 1000
    worst_f = -1; worst_w = -1
    for (k = 1; k <= nd; k++) {
        name = order[k]
        print name "  " describe(name)
        if (!((1, name) in nf) || !((2, name) in nf)) {
            print "    in only one of the two reads (the device appeared or went away between them), so no figure is given"
            continue
        }
        n = nf[1, name] < nf[2, name] ? nf[1, name] : nf[2, name]
        back = 0; wrapped = ""
        split("", d)
        for (i = 4; i <= n; i++) d[i] = v[2, name, i] - v[1, name, i]
        # A count that fell is a reset; field 12 is the I/O in flight NOW,
        # a gauge that may fall, and the 32-bit time fields are judged next.
        for (i = 4; i <= n; i++) if (i != 12 && !(i in timeof) && d[i] < 0) back = 1
        done = d[4] > 0 || d[8] > 0 || (n >= 15 && d[15] > 0) || (n >= 19 && d[19] > 0)
        for (i = 4; i <= n && !back; i++) {
            if (!(i in timeof) || d[i] >= 0) continue
            # A wrap, only when both reads fit 32 bits and the requests the
            # time accrues with advanced (field 13 and 14 accrue with any).
            if (v[1, name, i] < TWO32 && v[2, name, i] < TWO32 \
                && (timeof[i] ? (d[timeof[i]] > 0) : done)) {
                d[i] += TWO32
                # A small backwards jump is not evidence of a wrap: a
                # re-created device can have larger counts but lower times.
                if (d[i] >= TWO32 / 2) back = 1
                else wrapped = wrapped (wrapped == "" ? "" : ", ") timename[i]
            } else back = 1
        }
        if (back) {
            print "    counters went backwards between the two reads (the device was reset or re-created), so no figure is given"
            continue
        }
        if (wrapped != "")
            print "    " wrapped " wrapped past 2^32 between the reads: the kernel prints time as a 32-bit count, so each is taken modulo 2^32"
        reads = d[4]; rms = d[7]; writes = d[8]; wsect = d[10]; wms = d[11]; ioms = d[13]; wq = d[14]
        # Busy time is wall time, unlike summed request and weighted time.
        if (ioms > ms) ioms = ms
        if (writes > 0) {
            printf "    writes %.0f (%.1f/s, %.1f MB/s), %.0f ms spent, mean write %.2f ms\n", writes, writes / secs, wsect * 512 / 1e6 / secs, wms, wms / writes
            if (wms / writes > worst_w) { worst_w = wms / writes; worst_wn = name; worst_wc = writes }
        } else {
            print "    writes 0 in the interval"
        }
        if (n < 20) {
            printf "    flushes unread: this kernel'"'"'s /proc/diskstats has %d fields, and the flush counters are fields 19 and 20 (kernel 5.5 and later)\n", n
        } else if (d[19] > 0) {
            printf "    flushes %.0f (%.1f/s), %.0f ms spent, mean flush %.2f ms\n", d[19], d[19] / secs, d[20], d[20] / d[19]
            if (d[20] / d[19] > worst_f) { worst_f = d[20] / d[19]; worst_fn = name; worst_fc = d[19] }
        } else {
            print "    flushes 0 in the interval"
        }
        if (reads > 0) printf "    reads %.0f, mean read %.2f ms; ", reads, rms / reads
        else printf "    reads 0; "
        printf "busy %.1f%% of the interval, mean queue %.2f\n", ioms / ms * 100, wq / ms
    }
    if (worst_f >= 0) printf "worst mean flush: %s %.2f ms over %.0f flushes\n", worst_fn, worst_f, worst_fc
    else print "worst mean flush: none — no device reported a flush figure for the interval"
    if (worst_w >= 0) printf "worst mean write: %s %.2f ms over %.0f writes\n", worst_wn, worst_w, worst_wc
    else print "worst mean write: none — no device reported a write figure for the interval"
    print "== the two reads, as the kernel printed them (reported devices; loop and ram devices left out) =="
    for (k = 1; k <= nd; k++) {
        name = order[k]
        for (w = 1; w <= 2; w++) if ((w, name) in rawline) print "read " w ": " rawline[w, name]
    }
}' "$WORK/read-1" "$WORK/read-2"
rc=$?
case "$rc" in
    0) ;;
    3) node_fail "talosctl -n $NODE_ADDRESS read /proc/diskstats did not read as /proc/diskstats (no line of major, minor, name and at least 11 counters in one of the two reads): $(head -c 300 "$WORK/read-1" | tr '\n' ' ')" ;;
    *) node_fail "the figures could not be computed from the two reads (awk exit $rc)" ;;
esac
