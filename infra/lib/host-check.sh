# host-check.sh — WHICH MACHINE IS THIS, asked before a script installs
# or converges a managed host as root. Sourced; the ONE definition every
# such script uses (backlog 62b09c57, N7).
#
# WHY IT EXISTS. On 2026-10-07 23:12Z a reviewer timed the forge's
# converge launcher in a fixture tree on the DEV POD, with a stub for one
# script. The launcher chose the fixture generation's REAL
# forge-converge.sh and ran it as root for 1.5 s: install.sh wrote
# /etc/boss/sor.env, /var/lib/boss/.converge-hold.carried and two files
# under /usr/local/libexec/boss, downloaded /usr/local/bin/kubectl, and
# probe-account.sh ran `useradd boss-probe` (review aa901496, the
# reviewer's own account of it). Nothing asked which host it was on: the
# only thing between a fixture and the machine under it was every seam
# being remembered, every time.
#
# THE RULE. A gated script names each place it would touch this
# machine's own system — one line per seam, below — and then calls
# host_check with the estate node it installs. Three outcomes:
#
#   nothing real   every seam is redirected, so the run can only write
#                  where its caller pointed it: a test. Proceeds, and no
#                  identity is asked — which is why a fixture needs no
#                  override, and why an override never means "install
#                  here" to a test.
#   this host      at least one seam is at its real default, and this
#                  machine HOLDS an address the estate declares for the
#                  node. Proceeds; HOST_CHECK_VERDICT says which address
#                  matched which file.
#   refused        at least one seam is real and this machine is not the
#                  node. Exit 78, one line naming what it found, what it
#                  expected and which seams were real, BEFORE the script
#                  has written anything. A forgotten seam is therefore a
#                  refusal on every machine but the host itself.
#
# THE SIGNAL, AND WHY THIS ONE. "This machine holds the address" is read
# from the kernel's own table of local addresses (/proc/net/fib_trie,
# plus `hostname -I` where the host has it — the same read
# infra/estate/observe-host.sh has posted from the forge every fifteen
# minutes, 10.20.0.15 on 2026-10-07 23:47Z). What it is compared with is
# infra/estate/estate.toml: the node's own `address`, and for a node
# whose declared address is not on any of its interfaces — boss-gcp's is
# a NAT'd public one — the `[host_identity]` table there.
#   * It cannot be satisfied by accident: an environment variable, a
#     file under /etc, a hostname and a fixture tree are all things a
#     test or a mistaken shell can carry to another machine; an address
#     on an interface is not, and loopback is never counted.
#   * It needs no hand act: no marker is placed, on either host.
#   * /etc/boss/sor.env is NOT a signal — the installer under guard is
#     what writes it — and neither is BOSS_NODE_ID, which the unit sets.
#
# A FIRST INSTALL ON A NEW HOST still works. A rebuilt forge takes the
# address the estate declares and passes as it is. One brought up at
# another address before the estate says so is installed with
# BOSS_FIRST_INSTALL_AS=<node> set on purpose: honoured only for the
# node the script installs, never silently — the line goes to stderr and
# onto the run's packet — and the unattended ticks after it refuse until
# estate.toml declares the machine, which is the edit that makes it the
# host.
#
# SEAMS OF ITS OWN: BOSS_HOST_CHECK_ESTATE names another estate file (a
# test declares an address its own machine holds, to drive the
# this-host arm). There is no seam on what the machine holds.
#
# Pinned by crates/core/boss-testing/tests/host_check_sh.rs.

_host_check_infra="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
host_real=()
HOST_CHECK_VERDICT=""

# host_seam NAME [REAL_DEFAULT] — the environment seam NAME redirects a
# write (or a command that writes). It is REAL when unset, empty, or set
# to the default the script would have used anyway. The default is given
# only where it is the calling file's OWN (it already spells it, lines
# away); a seam that belongs to a script the caller runs is named without
# one and is real when unset, so no path is spelled in a second file
# (CLAUDE.md §9a — converge_hold_sh.rs refused the hold's, which is how
# this rule was found).
host_seam() {
    local v="${!1:-}"
    if [ -z "$v" ] || { [ "$#" -ge 2 ] && [ "$v" = "$2" ]; }; then
        host_real+=("$1")
    fi
}

# host_path LABEL VALUE REAL_DEFAULT — the same judgement for a value
# that arrived another way (an argument, a derived path).
host_path() {
    [ "$2" != "$3" ] || host_real+=("$1")
}

# host_command NAME — a command that changes this machine's accounts or
# packages and whose only seam is PATH (useradd, usermod). REAL when it
# resolves into a system directory; a test puts its stub first on PATH,
# and a machine that has no such command cannot run it.
host_command() {
    local at
    at="$(command -v -- "$1" 2>/dev/null)" || return 0
    case "$at" in
        /usr/sbin/* | /sbin/* | /usr/bin/* | /bin/* | /usr/local/sbin/* | /usr/local/bin/*)
            host_real+=("$1")
            ;;
    esac
}

# Every address this machine holds, one per line, loopback excluded.
host_addresses_held() {
    {
        if [ -r /proc/net/fib_trie ]; then
            awk '/\/32 host LOCAL/ { print prev } { prev = $2 }' /proc/net/fib_trie 2>/dev/null || true
        fi
        # A reader that is not there (no hostname(1), an older one with no
        # -I) is one reader fewer, never the other's answer lost: under
        # pipefail its status would otherwise empty the whole list.
        { hostname -I 2>/dev/null || true; } | tr ' ' '\n'
    } | awk '/^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$/ && $0 !~ /^127\./ && !seen[$0]++'
}

# Every address the estate file declares for a node: its `[[node]]`
# row's address, and its line under `[host_identity]`.
host_addresses_declared() { # <estate file> <node id>
    awk -v want="$2" '
        function val(s) { sub(/^[^=]*=[[:space:]]*"/, "", s); sub(/".*$/, "", s); return s }
        function flush() { if (innode && id == want && addr != "") print addr; id = ""; addr = "" }
        /^\[\[node\]\]/ { flush(); innode = 1; inid = 0; next }
        /^\[/ { flush(); innode = 0; inid = ($0 ~ /^\[host_identity\][[:space:]]*$/); next }
        innode && /^id[[:space:]]*=/ { id = val($0) }
        innode && /^address[[:space:]]*=/ { addr = val($0) }
        inid && /^[A-Za-z0-9_-]+[[:space:]]*=/ {
            k = $0; sub(/[[:space:]]*=.*$/, "", k)
            if (k == want) print val($0)
        }
        END { flush() }
    ' "$1" 2>/dev/null | awk '/^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$/ && $0 !~ /^127\./ && !seen[$0]++'
}

# host_check NODE ME — judge the seams named so far. Returns 0 to
# proceed; EXITS 78 when the run would write this machine's own system
# and the machine is not NODE.
host_check() {
    local node="$1" me="$2" estate held declared a real
    HOST_CHECK_VERDICT=""
    if [ "${#host_real[@]}" -eq 0 ]; then
        return 0
    fi
    real="${host_real[*]}"
    estate="${BOSS_HOST_CHECK_ESTATE:-$_host_check_infra/estate/estate.toml}"
    held="$(host_addresses_held | tr '\n' ' ')" || held=""
    held="${held% }"
    declared="$(host_addresses_declared "$estate" "$node" | tr '\n' ' ')" || declared=""
    declared="${declared% }"
    for a in $declared; do
        case " $held " in
            *" $a "*)
                HOST_CHECK_VERDICT="$node: this machine holds $a, which $estate declares for it"
                if declare -F run_summary_field >/dev/null; then run_summary_field host_check "$HOST_CHECK_VERDICT"; fi
                return 0
                ;;
        esac
    done
    if [ -n "${BOSS_FIRST_INSTALL_AS:-}" ] && [ "$BOSS_FIRST_INSTALL_AS" = "$node" ]; then
        HOST_CHECK_VERDICT="OVERRIDDEN: BOSS_FIRST_INSTALL_AS=$node — installing as the estate's $node on a machine that holds [${held:-no address}], where $estate declares [${declared:-nothing}] for it"
        echo "$me: HOST CHECK $HOST_CHECK_VERDICT. A first install, said on purpose; every later run refuses until the estate declares this machine." >&2
        if declare -F run_summary_field >/dev/null; then run_summary_field host_check "$HOST_CHECK_VERDICT"; fi
        return 0
    fi
    HOST_CHECK_VERDICT="REFUSED: this machine is not the estate's $node — it holds [${held:-no address}] and $estate declares [${declared:-nothing}] for $node"
    echo "$me: $HOST_CHECK_VERDICT; nothing was written. It would have written this machine's own system through: $real. A test redirects every one of those; a first install on a new $node sets BOSS_FIRST_INSTALL_AS=$node on purpose." >&2
    if declare -F run_summary_field >/dev/null; then run_summary_field host_check "$HOST_CHECK_VERDICT"; fi
    exit 78
}
