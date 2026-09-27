#!/usr/bin/env bash
# list-block-devices — every block device on this host, and which of
# them commission-a-disk would accept. READ-ONLY: no params, no writes.
#
#   list-block-devices.sh               the listing, then the candidates
#   list-block-devices.sh --candidates  the candidate lines alone (what
#                                       commission-a-disk's refusal of a
#                                       wrong device name prints)
#
# WHY (backlog 88eda2eb, 2026-09-27). To file plan-a-disk-commission the
# operator needs the new drive's /dev/disk/by-id name, and no read the
# system had could produce it: disk-report lists mounted filesystems
# only, the estate registry holds no block-device facts, and the journal
# door showed no kernel NVMe lines. The one way was a person running ls
# on the host — the hand act the protocol exists to remove (David,
# 2026-09-26: "How can we let the system peer into that question so that
# I don't have to manually ssh in?"). And a by-id name is the ONLY name
# commission-a-disk takes, because on 2026-09-21 the kernel renumbered
# the forge's NVMe devices when the second one went in.
#
# THE CANDIDATE MARK IS commission-a-disk's OWN JUDGEMENT, not a copy.
# The refusals live in commission-a-disk.judge.sh, sourced here exactly
# as the destructive script sources it, and each device's facts are read
# with the same commands that script uses (pinned equal by
# list_block_devices_sh.rs, CLAUDE.md §9a — the reads cannot move to a
# shared file without touching the write path's reviewed body). A name
# counts as commissionable only if it also passes the verb's own
# device_by_id pattern, read out of infra/ops/verbs/commission-a-disk.json
# and tested with jq the way the runner tests it, and does not end in
# -partN, which the script refuses. So a disk marked here is one the
# plan verb would render, and one not marked says which refusal it would
# get.
#
# EVERY FACT FAILS CLOSED. A read that fails is printed as unreadable,
# the device is not a candidate, and the run exits 1 after printing
# everything it could read — an incomplete listing is not a pass. The one
# fact every judgement needs, the chain of devices backing /, failing
# makes every device unjudgeable. commission-a-disk also checks that the
# source of / is a block device; lsblk refuses a path that is not one,
# so the chain read below fails closed on the same state.
#
# BOSS_BY_ID_DIR (default /dev/disk/by-id) exists so a test can hand the
# script a directory of links; the runner never sets it. It moves where
# the links are READ, never the name printed.
set -uo pipefail

ME=list-block-devices
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BY_ID_DIR="${BOSS_BY_ID_DIR:-/dev/disk/by-id}"
VERB="$HERE/../ops/verbs/commission-a-disk.json"

MODE=all
case "${1-}" in
  '') : ;;
  --candidates) MODE=candidates ;;
  *) echo "$ME: usage: list-block-devices.sh [--candidates] — got '$1'" >&2; exit 78 ;;
esac

for t in lsblk wipefs findmnt readlink jq; do
    command -v "$t" >/dev/null 2>&1 || {
        echo "$ME: FAILED — $t is not on this host; the facts cannot be read, and no evidence is not a pass" >&2
        exit 1
    }
done
# No `set -e` here (a failed read is printed, not fatal), so a judge
# that cannot be sourced must stop the run by hand: without it no
# device can be judged, and "no candidates" would be a guess.
# shellcheck source=commission-a-disk.judge.sh
. "$HERE/commission-a-disk.judge.sh" || {
    echo "$ME: FAILED — cannot source $HERE/commission-a-disk.judge.sh, the judgement this listing marks candidates by" >&2
    exit 1
}

say() { [ "$MODE" = all ] && printf '%s\n' "$*"; return 0; }
gaps=""
gap() { gaps="${gaps}${gaps:+; }$1"; }

# ---- the verb's own device_by_id pattern ------------------------------
# Judged by its OUTPUT, not jq's exit code: on jq-1.6 an input with no
# document exits 0 (a_jq_guard_asks_whether_there_is_a_document.rs), so
# an empty read is what says the pattern could not be had.
pattern=$(jq -r '.params[] | select(.name == "device_by_id") | .pattern // empty' "$VERB" 2>/dev/null)
[ -n "$pattern" ] || gap "the device_by_id pattern in $VERB"
# is_commissionable_name <by-id path>: the verb admits it and it does not
# name a partition (commission-a-disk.sh's -partN refusal, same test).
is_commissionable_name() {
    local suffix=${1##*-part}
    if [ "$suffix" != "$1" ]; then
        case "$suffix" in ''|*[!0-9]*) : ;; *) return 1 ;; esac
    fi
    [ -n "$pattern" ] || return 1
    [ "$(jq -n --arg n "$1" --arg p "$pattern" '$n | test($p)' 2>/dev/null)" = true ]
}

# ---- what backs / ------------------------------------------------------
root_chain=""
root_err=""
if root_src=$(findmnt -nvro SOURCE /); then
    root_chain=$(lsblk -nrso KNAME "$root_src") \
        || root_err="lsblk could not trace what backs / ($root_src) — it is not a block device, or the read failed"
else
    root_err="findmnt could not read the source of /"
fi
if [ -n "$root_err" ]; then
    gap "$root_err"
    say "/ is backed by: UNREADABLE — $root_err; no device can be judged"
else
    say "/ is mounted from $root_src, backed by: $(printf '%s\n' "$root_chain" | awk 'NF' | tr '\n' ' ')"
fi

# ---- the by-id names, resolved the way commission-a-disk resolves one --
byid=""
if [ -d "$BY_ID_DIR" ]; then
    for l in "$BY_ID_DIR"/*; do
        [ -L "$l" ] || [ -e "$l" ] || continue
        t=$(readlink -f -- "$l") || { gap "readlink -f $l"; continue; }
        # Printed and judged as the /dev/disk/by-id path, whatever
        # directory it was read from: that is the name commission-a-disk
        # takes, and the one the operator copies into the request.
        byid="${byid}${t##*/} /dev/disk/by-id/${l##*/}"$'\n'
    done
else
    say "$BY_ID_DIR does not exist on this host — no device has a name commission-a-disk takes"
fi

# ---- every block device ------------------------------------------------
all=$(lsblk -nro KNAME) || {
    echo "$ME: FAILED — lsblk could not list the block devices" >&2
    exit 1
}
knames=$(printf '%s\n' "$all" | awk 'NF && !seen[$1]++ {print $1}')
[ -n "$knames" ] || say "lsblk lists no block devices on this host"

candidates=""
one_line() { printf '%s' "$1" | awk 'NF' | tr '\n' ' ' | sed 's/ $//'; }
trim() { local s="$1"; s="${s#"${s%%[![:space:]]*}"}"; printf '%s' "${s%"${s##*[![:space:]]}"}"; }

for k in $knames; do
    DEV="/dev/$k"
    unread=""
    # The facts commission-a-disk.sh judges, read with its commands.
    dtype=$(lsblk -dno TYPE "$DEV") || { dtype=""; unread="${unread} type"; }
    kids=$(lsblk -nro KNAME "$DEV") || { kids=""; unread="${unread} children"; }
    parts=$(( $(printf '%s\n' "$kids" | awk 'NF {n++} END {print n+0}') - 1 ))
    sigs=$(wipefs -n -i -O TYPE "$DEV") || { sigs=""; unread="${unread} signatures(wipefs)"; }
    mps=$(lsblk -nro MOUNTPOINTS "$DEV") || { mps=""; unread="${unread} mountpoints"; }
    mounted=$(printf '%s\n' "$mps" | awk 'NF {n++} END {print n+0}')
    if [ -n "$root_err" ]; then
        backs_root=unknown
    else
        backs_root=$(printf '%s\n' "$root_chain" | awk -v d="$k" '$1 == d {f = 1} END {print f ? "yes" : "no"}')
    fi
    # What an operator needs to tell one drive from another.
    size=$(lsblk -dno SIZE "$DEV") || { size=""; unread="${unread} size"; }
    model=$(lsblk -dno MODEL "$DEV") || { model=""; unread="${unread} model"; }
    serial=$(lsblk -dno SERIAL "$DEV") || { serial=""; unread="${unread} serial"; }
    size=$(trim "$size"); model=$(trim "$model"); serial=$(trim "$serial")
    names=$(printf '%s' "$byid" | awk -v d="$k" '$1 == d {print $2}')
    usable=""
    for n in $names; do
        is_commissionable_name "$n" && usable="${usable}${usable:+ }$n"
    done

    if [ -n "$unread" ]; then
        verdict="no — cannot read:$unread; a fact that cannot be read is a refusal, never a pass"
        gap "$k:$unread"
    elif [ -n "$root_err" ]; then
        verdict="no — $root_err"
    elif ! why=$(disk_refusal "$DEV" "$dtype" "$parts" "$sigs" "$mounted" "$backs_root"); then
        verdict="no — $why"
    elif [ -z "$usable" ]; then
        verdict="no — no /dev/disk/by-id name for it passes commission-a-disk's device_by_id (${pattern:-pattern unreadable}, not ending in -partN), and that verb takes nothing else"
    else
        verdict="YES — commission-a-disk would accept it by: $usable"
        for n in $usable; do
            candidates="${candidates}$n ($k, ${size:-size unread}, ${model:-no model}, serial ${serial:-none})"$'\n'
        done
    fi

    say ""
    say "== $k =="
    say "  type:        ${dtype:-UNREADABLE}"
    say "  size:        ${size:-unknown}"
    say "  model:       ${model:-none}"
    say "  serial:      ${serial:-none}"
    if [ -n "$names" ]; then
        first=1
        for n in $names; do
            if [ "$first" = 1 ]; then say "  by-id:       $n"; first=0; else say "               $n"; fi
        done
    else
        say "  by-id:       none"
    fi
    kid_list=$(printf '%s\n' "$kids" | awk -v d="$k" 'NF && $1 != d' | tr '\n' ' ' | sed 's/ $//')
    say "  below it:    ${kid_list:-none}"
    sig_list=$(one_line "$sigs")
    say "  signatures:  ${sig_list:-none}"
    mp_list=$(one_line "$mps")
    say "  mounted:     ${mp_list:-nothing} (on it or below it)"
    say "  backs /:     $backs_root"
    say "  candidate:   $verdict"
done

if [ "$MODE" = candidates ]; then
    printf '%s' "$candidates"
else
    say ""
    say "== candidates for commission-a-disk (pass the by-id name to plan-a-disk-commission) =="
    if [ -n "$candidates" ]; then
        printf '%s' "$candidates"
    else
        say "none — no whole, raw, unmounted disk with a usable /dev/disk/by-id name"
    fi
fi

if [ -n "$gaps" ]; then
    echo "$ME: INCOMPLETE — could not read: $gaps. A device whose facts could not be read is never marked a candidate." >&2
    exit 1
fi
exit 0
