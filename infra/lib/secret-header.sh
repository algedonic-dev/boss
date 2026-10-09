# secret-header.sh — a secret HTTP header reaches curl in a 0600 FILE,
# never in curl's argv. Source it, then:
#
#   . "$(dirname "$0")/../lib/secret-header.sh"
#   secret_header MT_HDR ${BOSS_MACHINE_TOKEN:+"x-boss-machine-token: $BOSS_MACHINE_TOKEN"}
#   curl -fsS ${MT_HDR:+-H "$MT_HDR"} "$url"
#
# `secret_header VAR LINE` writes LINE into a 0600 file and sets VAR to
# `@<that file>`, the argument curl's `-H` reads a header FILE from
# (curl 7.55+). With no LINE — the token is unset, as it is on a host
# whose system of record needs none — VAR is set EMPTY and nothing is
# written, so `${VAR:+-H "$VAR"}` sends no header, exactly as the
# `${BOSS_MACHINE_TOKEN:+-H "x-boss-machine-token: …"}` it replaces did.
#
# WHY (backlog 5f3ad356, 2026-09-27; the sweep on cfa678e9, 2026-09-28).
# `curl -H "x-boss-machine-token: $BOSS_MACHINE_TOKEN"` puts the token in
# curl's command line, and a command line is world-readable while the
# process runs: `ps -eo args`, /proc/<pid>/cmdline, to every local user
# of the host. The sweep counted 28 such sites in 13 files under infra/,
# plus the pod door's `$TOK`, a registry bearer in `$auth_header`, the CI
# locomotive's `$FORGE_TOKEN` and the CLI installer's `$TOKEN`. The line
# handed to THIS function is not argv: a shell function and `printf` are
# both run inside the shell, and nothing is exec'd with the value. The
# only process that sees the file's path is curl, and the path is not the
# secret. infra/lint/a-secret-header-rides-in-a-file.sh keeps the argv
# shape from coming back, by the HEADER NAME, so a variable called `TOK`
# or `auth_header` cannot hide it.
#
# WHERE THE FILE LIVES. Under a systemd unit that declares
# `RuntimeDirectory=`, in a private directory inside $RUNTIME_DIRECTORY:
# tmpfs under /run, which systemd removes when the unit stops — after a
# SIGKILL or an OOM kill too — and a power loss clears (the shape car
# 5f77b205 gave github-act.sh). Anywhere else, in a `mktemp -d` directory
# (0700) under ${TMPDIR:-/tmp}. Every unit that starts a sender declares
# one (a_killed_sender_leaves_no_header_behind.rs walks the unit files),
# so the second place is a hand run's, the pod door's and a pod's. Either
# way the directory is removed by an
# EXIT trap, CHAINED IN FRONT OF any EXIT trap the script already set:
# most of these scripts `trap 'rm -rf "$workdir"' EXIT`, and a helper
# that replaced a caller's trap would leak the caller's own scratch. The
# caller's trap still sees the exit status it would have seen.
#
# ORDER, AND WHICH SHELL. The cleanup is chained once, when the first
# call makes the directory. So: set your own `trap … EXIT` FIRST, and
# make the first call from the script's own shell, not from inside a
# `$(…)` or `( … )`. A `trap … EXIT` set later replaces the cleanup,
# and under /tmp the 0600 file then outlives the run. A first call in a
# bash SUBSHELL is REFUSED (return 1): bash lists the PARENT's traps
# there although none of them will run, so chaining would hand the
# subshell's exit the parent's `rm -rf "$workdir"` — measured, not
# guessed (bash 5.2; dash lists nothing in a subshell). A later call in
# a subshell only rewrites its file in the directory the first one made.
#
# One file per VAR, rewritten on each call, so a loop that calls this
# once per request holds one file, not one per request. A LINE holding a
# newline or a carriage return is REFUSED (return 2): curl reads a
# header file line by line, and a second line is a second header.
#
# POSIX sh, because infra/ops/ops-runner.sh is `#!/bin/sh` (dash on the
# hosts) and the other callers are bash: nothing here may be a bashism
# (infra/lint/a-sh-script-parses-under-sh.sh). Every name it sets begins
# `_secret_header_`, except the VAR you name and the four machine-token
# functions at the end of this file.
#
# THE MACHINE TOKEN (design 6805c764 car 4; backlog 1876bbdb INFO-6 and
# INFO-7). Every shell sender of the estate machine token takes it here:
#
#   machine_token_header MT_HDR "$BASE"
#   curl -fsS ${MT_HDR:+-H "$MT_HDR"} "$BASE/api/…"
#
# See machine_token_header below for where it reads the token and which
# hosts it stamps.

# Sourced twice (a lib that sources this, inside a script that did too),
# the second source must not forget the directory the first one opened.
if [ -z "${_secret_header_sourced:-}" ]; then
    _secret_header_dir=""
    _secret_header_sourced=1
fi

# _secret_header_close — the EXIT trap's half: remove the directory.
_secret_header_close() {
    if [ -n "$_secret_header_dir" ]; then
        rm -rf "$_secret_header_dir"
        _secret_header_dir=""
    fi
}

# secret_header_close — remove the header directory NOW, without waiting
# for the EXIT trap. For the one shape the trap cannot serve: a caller
# that makes its header INSIDE its own EXIT trap. The cleanup chained at
# the first call runs in front of the caller's trap, so by the time that
# trap sends, the directory is gone; the call it makes then opens a new
# one, and a trap set while the EXIT trap is running never fires —
# measured in bash 5.2, 2026-10-06: the 0600 file outlived the run. Such
# a caller calls this after its last request
# (infra/forge/cluster-deploy-lib.sh answer_converge_requests, backlog
# 2710c8fc). Every header made before it is gone afterwards: a VAR still
# holding `@<file>` names a file that no longer exists, so make the
# header again before the next request. Always returns 0.
secret_header_close() {
    _secret_header_close
    return 0
}

# _secret_header_capture -- CMD SIGNAL — one entry of `trap`'s listing,
# re-read: the command of the EXIT entry is kept, every other entry is
# ignored. The listing is `trap -- 'cmd' EXIT` in bash and in dash, and
# is read in THIS shell (a redirect, never `$(trap)`, which dash answers
# empty inside a command substitution).
_secret_header_capture() {
    [ "${3:-}" = EXIT ] && _secret_header_prev=$2
    return 0
}

# _secret_header_arm — make sure the EXIT trap removes the directory,
# chained in front of whatever EXIT trap stands now.
_secret_header_arm() {
    _secret_header_prev=""
    trap > "$_secret_header_dir/.traps" || return 1
    # A multi-line trap command continues on lines that do not begin
    # `trap -- `, so only entry lines are renamed; eval then re-reads the
    # shell's own quoting of every entry.
    _secret_header_list=$(sed 's/^trap -- /_secret_header_capture -- /' "$_secret_header_dir/.traps") || return 1
    rm -f "$_secret_header_dir/.traps"
    eval "$_secret_header_list" || return 1
    case "$_secret_header_prev" in
        *_secret_header_close*) return 0 ;;
    esac
    if [ -n "$_secret_header_prev" ]; then
        # `set +e` BEFORE `(exit rc)`: under errexit a non-zero `(exit rc)`
        # ends the trap right there, and the caller's own trap below never
        # ran — measured in bash and dash by the adversarial review of car
        # 676f4cdd (HOLD F1): workdirs leaked on every refusal, and the
        # maintenance wrap's on_exit never folded its claimed ledger rows
        # back. The shell is exiting, so errexit has nothing left to guard.
        # shellcheck disable=SC2064  # the caller's trap text, expanded NOW, is the point
        trap "_secret_header_rc=\$?; _secret_header_close; set +e; (exit \$_secret_header_rc)
$_secret_header_prev" EXIT
    else
        trap '_secret_header_close' EXIT
    fi
}

# _secret_header_born PID — when PID started, in the kernel's clock ticks
# since boot (field 22 of /proc/PID/stat, read after the last `) ` so a
# process name holding spaces or brackets cannot shift it). Empty where
# there is no /proc or no such process.
_secret_header_born() {
    [ -r "/proc/$1/stat" ] || return 0
    sed -e 's/^.*) //' "/proc/$1/stat" 2>/dev/null | cut -d' ' -f20
}

# _secret_header_sweep BASE — remove the header directories that DEAD runs
# of this account left under BASE, and say how many, by path.
#
# WHY (backlog df38075a, F1 of review a92c0b94). The EXIT trap is the
# only thing that removes the directory, and a SIGKILL runs no trap in
# any shell, a SIGTERM none under dash. Measured 2026-10-06: the dev
# pod's /tmp held four such directories dated 10-03 to 10-06, three with
# the door's TOKEN_HDR file in them, left by killed boss-api calls. A
# unit's RuntimeDirectory= is the mechanism where there is a unit; this
# is what is left for a sender with none, and for a unit's first run
# after one that had none — which is why _secret_header_open calls it on
# the temp directory as well as on the directory it writes in.
#
# WHAT IT MAY JUDGE. Only a real directory (never a link) of this name
# that THIS account owns. Each one carries `.owner`, written when it is
# made: the owner's pid, when that pid was born, and its pid namespace.
#   * same namespace, and no such pid, or one born at another time (a
#     recycled pid): the owner is dead — removed.
#   * same namespace, same pid, same birth: a live sender mid-request —
#     left alone, whatever its age. Removing it would send that request
#     unstamped, a would-refuse fact made by the cleanup.
#   * anything else — another namespace sharing this directory (a second
#     container on one volume), no record (a reader older than the
#     record, or a run killed before it was written), a record that is
#     not three plain fields, a host with no /proc: its pids mean nothing
#     here, so only AGE judges it, and only past a day.
# Never a refusal and never a reason to stop: a caller that cannot sweep
# sends exactly as it would have.
_secret_header_sweep() {
    _secret_header_sw_ns=$(readlink /proc/self/ns/pid 2>/dev/null) || _secret_header_sw_ns=""
    _secret_header_sw_n=0
    _secret_header_sw_said=""
    _secret_header_sw_flags="$-"
    set +f
    for _secret_header_sw_d in "$1"/boss-secret-header.*; do
        [ -d "$_secret_header_sw_d" ] && [ ! -L "$_secret_header_sw_d" ] && [ -O "$_secret_header_sw_d" ] || continue
        _secret_header_sw_pid="" _secret_header_sw_born="" _secret_header_sw_ons="" _secret_header_sw_more=""
        if [ -f "$_secret_header_sw_d/.owner" ] && [ ! -L "$_secret_header_sw_d/.owner" ]; then
            read -r _secret_header_sw_pid _secret_header_sw_born _secret_header_sw_ons _secret_header_sw_more \
                < "$_secret_header_sw_d/.owner" 2>/dev/null || :
        fi
        # PLACED: a record of two plain numbers from THIS pid namespace —
        # the only kind whose pid can be looked up here.
        _secret_header_sw_placed=""
        if [ -z "$_secret_header_sw_more" ] && [ -n "$_secret_header_sw_ns" ] \
            && [ "$_secret_header_sw_ons" = "$_secret_header_sw_ns" ]; then
            case "$_secret_header_sw_pid:$_secret_header_sw_born" in
                *[!0-9:]* | :* | *:) ;;
                *) _secret_header_sw_placed=1 ;;
            esac
        fi
        if [ -n "$_secret_header_sw_placed" ]; then
            [ "$(_secret_header_born "$_secret_header_sw_pid")" != "$_secret_header_sw_born" ] || continue
        else
            [ -n "$(find "$_secret_header_sw_d" -maxdepth 0 -mmin +1440 2>/dev/null)" ] || continue
        fi
        rm -rf "$_secret_header_sw_d" 2>/dev/null || continue
        _secret_header_sw_n=$((_secret_header_sw_n + 1))
        _secret_header_sw_said="$_secret_header_sw_said $_secret_header_sw_d"
    done
    case "$_secret_header_sw_flags" in *f*) set -f ;; esac
    if [ "$_secret_header_sw_n" -eq 1 ]; then
        echo "secret_header: removed 1 header directory a killed earlier run left:$_secret_header_sw_said" >&2
    elif [ "$_secret_header_sw_n" -gt 1 ]; then
        echo "secret_header: removed $_secret_header_sw_n header directories killed earlier runs left:$_secret_header_sw_said" >&2
    fi
    return 0
}

# _secret_header_open — the private directory, made once per shell.
_secret_header_open() {
    if [ -z "$_secret_header_dir" ] || [ ! -d "$_secret_header_dir" ]; then
        if [ "${BASH_SUBSHELL:-0}" -gt 0 ]; then
            echo "secret_header: first call inside a subshell — refused; make it from the script's own shell, after its trap (infra/lib/secret-header.sh, ORDER)" >&2
            return 1
        fi
        _secret_header_base="${RUNTIME_DIRECTORY:-}"
        _secret_header_base="${_secret_header_base%%:*}"
        if [ -z "$_secret_header_base" ] || [ ! -d "$_secret_header_base" ]; then
            _secret_header_base="${TMPDIR:-/tmp}"
        fi
        _secret_header_sweep "$_secret_header_base" || :
        # UNDER A RuntimeDirectory, THE TEMP DIRECTORY TOO (backlog
        # df38075a, F1, second car). A unit that gains RuntimeDirectory=
        # writes in /run from then on, and a sweep of only the directory
        # it writes in finds a fresh tmpfs: what the unit's killed runs
        # had left under /tmp before had no remover but a hand run of the
        # same account. Same rules, same account, one glob.
        _secret_header_tmp="${TMPDIR:-/tmp}"
        if [ "$_secret_header_base" != "$_secret_header_tmp" ] && [ -d "$_secret_header_tmp" ]; then
            _secret_header_sweep "$_secret_header_tmp" || :
        fi
        _secret_header_dir=$(mktemp -d "$_secret_header_base/boss-secret-header.XXXXXX") || {
            _secret_header_dir=""
            echo "secret_header: cannot make a private directory under $_secret_header_base — nothing was sent" >&2
            return 1
        }
        chmod 700 "$_secret_header_dir" || return 1
        # Who owns it, for the next run's sweep (see _secret_header_sweep).
        # Written before any header is: a run killed between the two lines
        # leaves an empty directory, which a day's age removes.
        (umask 077 && printf '%s %s %s\n' "$$" "$(_secret_header_born "$$")" \
            "$(readlink /proc/self/ns/pid 2>/dev/null)" > "$_secret_header_dir/.owner") || :
        _secret_header_arm || {
            echo "secret_header: could not chain the cleanup onto the EXIT trap — nothing was sent" >&2
            _secret_header_close
            return 1
        }
    fi
    return 0
}

# secret_header VAR [LINE] — see the header of this file.
secret_header() {
    case "${1:-}" in
        '' | [0-9]* | *[!A-Za-z0-9_]*)
            echo "secret_header: '${1:-}' is not a variable name" >&2
            return 2
            ;;
    esac
    if [ "$#" -lt 2 ] || [ -z "$2" ]; then
        eval "$1="
        return 0
    fi
    case "$2" in
        *"
"* | *"$(printf '\r')"*)
            echo "secret_header: the line for $1 holds a line break — refused, a second line would be a second header" >&2
            return 2
            ;;
    esac
    _secret_header_open || return 1
    (umask 077 && printf '%s\n' "$2" > "$_secret_header_dir/$1") || {
        echo "secret_header: cannot write $_secret_header_dir/$1 — nothing was sent" >&2
        return 1
    }
    eval "$1=\"@\$_secret_header_dir/\$1\""
}

# ---------------------------------------------------------------------
# machine_token_header VAR URL [LIST] — the estate machine token's header
# for a request to URL, handed over as secret_header does (VAR is
# `@<0600 file>`), or VAR empty when this request carries none.
#
# WHERE THE TOKEN IS READ. The `current` file of the directory the
# `boss-machine-token` Secret is mounted at: $BOSS_MACHINE_TOKEN_DIR,
# default /etc/boss/machine-token — the one definition boss-core's
# callers and every service's gate read (machine_token.rs TOKEN_DIR_ENV,
# DEFAULT_TOKEN_DIR). Until car 4 every script here stamped the env var
# BOSS_MACHINE_TOKEN, which a `secretKeyRef` fixed at process start and
# which the mount replaces; it is NOT read any longer, because two
# sources for one fact drift (design choice 2, CLAUDE.md §9a). And
# infra/dev/boss-api read /etc/boss/machine-token as a FILE while core
# reads it as this directory, so the day it was mounted every door write
# would have gone out unstamped (backlog 1876bbdb, INFO-6).
#
# WHICH HOSTS. Loopback (localhost, ::1, a full 127.x.y.z quad), and the
# hosts LIST names — commas or whitespace; an entry with a leading dot is
# a suffix, `.boss.svc.cluster.local` = every Service in that one
# namespace and no other. LIST is $BOSS_MACHINE_TOKEN_HOSTS when it is
# SET (even empty); else the third argument, when given; else the
# `BOSS_MACHINE_TOKEN_HOSTS=` line of the rendered sor.env
# ($BOSS_SOR_ENV, default /etc/boss/sor.env) — the order boss-core's
# `Hosts::from_env` takes, held equal to it on the rows of the table in
# crates/core/boss-testing/tests/secret_header_sh.rs. Off those rows the
# two can differ (a backslash before `@`, malformed IPv6, the short and
# octal IPv4 spellings Url::parse expands, a scheme that is not http or
# https); review ef2da426 F4 measured each against where curl actually
# connects, and ON A URL THAT BEGINS WITH `http://` OR `https://` every
# difference either withholds, stamps a host curl reaches as the one the
# rule read, or stamps a URL curl refuses to send (exit 3). That sentence
# was written without the qualifier and was false for a URL with no
# leading scheme — six rows of review 927f8602 read ADMIT while curl
# delivered the header elsewhere (backlog 50708d76, F1). Such a URL is
# now never read for a host at all: machine_token_host, below. Until car 4 a
# script sent the token to whatever $BASE held, so a hand run with
# BOSS_JOBS_URL pointed at the public edge sent it there (INFO-7, the
# shell half of 2ee29275 F1).
#
# NEVER A REFUSAL. No mount, an empty mount, or a blank slot is no token:
# VAR is empty and nothing is said — every host is in that state until
# the broker's first mint. A slot boss-core would refuse (the directory
# is a file, `current` is not a regular file, is larger than 4096 bytes
# — MAX_SLOT_BYTES — or holds a line break) and a host that is not the
# estate's are both said on stderr and sent WITHOUT the token, exit 0:
# a gate in `report` admits the request and tallies it, which is how a
# missed caller is found (design 6805c764, the report phase). The only
# non-zero return is secret_header's own: the header file could not be
# written, and the caller decides what that stops.
machine_token_header() {
    case "${1:-}" in
        '' | [0-9]* | *[!A-Za-z0-9_]*)
            echo "machine_token_header: '${1:-}' is not a variable name" >&2
            return 2
            ;;
    esac
    eval "$1="
    _secret_header_mt_dir="${BOSS_MACHINE_TOKEN_DIR:-/etc/boss/machine-token}"
    _secret_header_mt_me="${0##*/}"
    if [ -e "$_secret_header_mt_dir" ] && [ ! -d "$_secret_header_mt_dir" ]; then
        echo "$_secret_header_mt_me: the machine token's mount $_secret_header_mt_dir is not a directory — the token is the \`current\` file of the mounted Secret (design 6805c764); this request goes out without it" >&2
        return 0
    fi
    _secret_header_mt_slot="$_secret_header_mt_dir/current"
    if [ ! -e "$_secret_header_mt_slot" ] && [ ! -L "$_secret_header_mt_slot" ]; then
        return 0
    fi
    # -f is false for a FIFO, a device and a directory: never opened, so
    # a FIFO at the path cannot hang the caller (core's review S7).
    if [ ! -f "$_secret_header_mt_slot" ] || [ ! -r "$_secret_header_mt_slot" ]; then
        echo "$_secret_header_mt_me: the machine token's slot $_secret_header_mt_slot is not a readable regular file; this request goes out without it" >&2
        return 0
    fi
    _secret_header_mt_size=$(wc -c < "$_secret_header_mt_slot" | tr -d ' ') || _secret_header_mt_size=""
    case "$_secret_header_mt_size" in
        '' | *[!0-9]*)
            echo "$_secret_header_mt_me: the machine token's slot $_secret_header_mt_slot could not be measured; this request goes out without it" >&2
            return 0
            ;;
    esac
    if [ "$_secret_header_mt_size" -gt 4096 ]; then
        echo "$_secret_header_mt_me: the machine token's slot $_secret_header_mt_slot is larger than 4096 bytes, which is not a token; this request goes out without it" >&2
        return 0
    fi
    _secret_header_mt_tok=$(sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' "$_secret_header_mt_slot") || _secret_header_mt_tok=""
    # Leading and trailing blank lines, as core's trim drops them.
    _secret_header_mt_tok=$(printf '%s\n' "$_secret_header_mt_tok" | sed -e '/./,$!d')
    case "$_secret_header_mt_tok" in
        '') return 0 ;;
        *"
"* | *"$(printf '\r')"*)
            echo "$_secret_header_mt_me: the machine token's slot $_secret_header_mt_slot holds a line break, which no header can carry; this request goes out without it" >&2
            _secret_header_mt_tok=""
            return 0
            ;;
    esac
    if [ "$#" -ge 3 ]; then
        _secret_header_mt_hosts "$3"
    else
        _secret_header_mt_hosts
    fi
    _secret_header_mt_host=$(machine_token_host "${2:-}")
    if ! _secret_header_mt_allowed "$_secret_header_mt_host" "$_secret_header_mt_list"; then
        # The scheme and host, never the path or query (a login's state
        # rides there) and never the userinfo. A URL with no leading
        # http(s) scheme has no host to name, and NOTHING of it is
        # printed: what stands before its first `://`, if it has one, may
        # be a path, a query or a userinfo (backlog 50708d76, F1).
        case "${2:-}" in
            [Hh][Tt][Tt][Pp]://* | [Hh][Tt][Tt][Pp][Ss]://*)
                echo "$_secret_header_mt_me: machine token withheld from ${2%%://*}://$_secret_header_mt_host — not loopback and not in BOSS_MACHINE_TOKEN_HOSTS, so this request goes out without it" >&2
                ;;
            *)
                echo "$_secret_header_mt_me: machine token withheld — the URL does not begin with http:// or https://, so no host is read from it (curl would guess one); this request goes out without it" >&2
                ;;
        esac
        _secret_header_mt_tok=""
        return 0
    fi
    secret_header "$1" "x-boss-machine-token: $_secret_header_mt_tok"
    _secret_header_mt_rc=$?
    _secret_header_mt_tok=""
    return "$_secret_header_mt_rc"
}

# ---------------------------------------------------------------------
# machine_gate_value_header VAR URL VALUE — the same header for a value
# the CALLER holds, not the mount: VAR is `@<0600 file>`, or empty when
# this request must not carry it. machine_token_header above still reads
# the mount and is still the only way the ESTATE token is sent.
#
# WHY A SECOND DOOR (backlog d26515c5, unit 7). The forge's probe-reader
# deposit must ask every gate what it makes of the Secret's `current`
# slot BEFORE that value is written anywhere a reader could find it, so
# it has no mount to read: the value is in a variable. Writing the header
# by hand in that script would be a second place the header is spelled
# (every_shell_sender_reads_the_machine_token_from_its_mount.rs refuses
# one) and a second copy of the host rule.
#
# ONE CALLER. crates/core/boss-testing/tests/
# every_shell_sender_reads_the_machine_token_from_its_mount.rs refuses
# every live line under infra/ that names this function outside a roster
# of one (review 4d39f4dc, B1): a door for "a value the caller holds" is
# a door for the estate token read by hand, unless who may use it is
# held somewhere a reviewer can read.
#
# THE SAME HOST RULE ON THE SAME LIST, by the same lines:
# _secret_header_mt_hosts resolves the list for both functions and
# _secret_header_mt_allowed judges the host for both (held row for row
# by machine_gate_value_header_sh.rs). A host outside the rule is said
# on stderr and VAR is empty: the caller sends nothing, since unlike a
# stamped request this one has no purpose without the value. An empty
# VALUE is VAR empty and nothing said. Non-zero only as secret_header's
# own.
machine_gate_value_header() {
    case "${1:-}" in
        '' | [0-9]* | *[!A-Za-z0-9_]*)
            echo "machine_gate_value_header: '${1:-}' is not a variable name" >&2
            return 2
            ;;
    esac
    eval "$1="
    if [ -z "${3:-}" ]; then
        return 0
    fi
    _secret_header_mt_hosts
    _secret_header_gv_host=$(machine_token_host "${2:-}")
    if ! _secret_header_mt_allowed "$_secret_header_gv_host" "$_secret_header_mt_list"; then
        echo "${0##*/}: a machine-gate value was withheld from host '$_secret_header_gv_host' — not loopback and not in BOSS_MACHINE_TOKEN_HOSTS, so nothing is sent" >&2
        return 0
    fi
    secret_header "$1" "x-boss-machine-token: $3"
}

# ---------------------------------------------------------------------
# machine_token_admits URL — the host rule's JUDGEMENT alone: return 0
# when a request to URL may carry the machine token, 1 when it may not.
# Nothing is read from the mount, written, or said; the caller says what
# it withheld, in its own words.
#
# WHY (backlog 7369b078, F1 of review 9c484ca8). The ops runner presents
# a SECOND secret to the system of record, its own credential in
# x-boss-runner-credential, and made that header with plain
# secret_header — which judges no host. So with BOSS_JOBS_URL pointed off
# the estate the machine token stayed home and the runner's credential
# went, on a queue read sent every minute. A secret that goes where the
# machine token goes is held to the machine token's rule by asking THIS,
# not by a second copy of the rule: it is the two calls the header
# writers above make, on the same list, and
# machine_gate_value_header_sh.rs holds all three equal row for row.
machine_token_admits() {
    _secret_header_mt_hosts
    _secret_header_mt_allowed "$(machine_token_host "${1:-}")" "$_secret_header_mt_list"
}

# _secret_header_mt_hosts [LIST] — the hosts list of the rule above into
# _secret_header_mt_list, by ONE resolution for both header writers
# (review 4d39f4dc, F2: it lived in each, and the copies had already
# drifted on four rows). $BOSS_MACHINE_TOKEN_HOSTS when it is SET (even
# empty); else LIST, when one is given (even empty); else the first
# `BOSS_MACHINE_TOKEN_HOSTS=` line of the rendered sor.env ($BOSS_SOR_ENV,
# default /etc/boss/sor.env). A sor.env that is absent or cannot be read
# is NO list: loopback is the rule's own and needs none, and no other
# host is named.
_secret_header_mt_hosts() {
    if [ -n "${BOSS_MACHINE_TOKEN_HOSTS+set}" ]; then
        _secret_header_mt_list="$BOSS_MACHINE_TOKEN_HOSTS"
    elif [ "$#" -ge 1 ]; then
        _secret_header_mt_list="$1"
    else
        _secret_header_mt_list=$(sed -n 's/^BOSS_MACHINE_TOKEN_HOSTS=//p' "${BOSS_SOR_ENV:-/etc/boss/sor.env}" 2>/dev/null | sed -n '1p') || _secret_header_mt_list=""
    fi
}

# machine_token_host URL — the host a request to URL goes to, as the
# decision above reads it: lowercased, no port, no userinfo, no IPv6
# brackets, one trailing dot dropped. The authority ends at the first
# `/`, `?` or `#`, cut BEFORE the userinfo is, or `http://evil.com#@127.0.0.1`
# reads as loopback while curl and boss-core both send it to evil.com
# (review of 54d9a23a, MEDIUM-2). Printed without a newline.
#
# ONLY AFTER A LEADING `http://` OR `https://` (backlog 50708d76, F1 of
# review 927f8602). Anything else prints NOTHING, and an empty host is
# withheld by _secret_header_mt_allowed. Until 2026-10-08 this stripped
# `${1#*://}` — up to the first `://` ANYWHERE — so a URL with no scheme
# that held `://` further on was judged by the host after it, while curl
# guesses http and connects to the FIRST: with the list naming
# sor.invalid, `evil.invalid/x://sor.invalid`, `evil.invalid?x=://sor.invalid`,
# `evil.invalid#://sor.invalid`, `evil.invalid:80/x://sor.invalid:7900`,
# `evil.invalid/x://127.0.0.1` and `user@evil.invalid/://localhost` each
# read ADMIT and curl 7.88.1 delivered the header to evil.invalid
# (measured through a loopback stub, dash and bash alike). WITHHELD
# RATHER THAN JUDGED BY THE FIRST HOST: reading it as curl would means
# keeping a second copy of curl's guess (it also guesses ftp, dict and
# others from the host's first label), and boss-core withholds every such
# URL already — Url::parse refuses a relative one. No sender here builds
# a URL without a scheme (render-sor-env.sh writes sor_url whole), so the
# only caller this refuses is the mistyped BOSS_JOBS_URL it is for. Not
# every scheme either: curl speaks twenty and the estate two.
machine_token_host() {
    case "${1:-}" in
        [Hh][Tt][Tt][Pp]://* | [Hh][Tt][Tt][Pp][Ss]://*) ;;
        *) return 0 ;;
    esac
    _secret_header_h="${1#*://}"
    _secret_header_h="${_secret_header_h%%[/?#]*}"
    _secret_header_h="${_secret_header_h##*@}"
    case "$_secret_header_h" in
        \[*)
            _secret_header_h="${_secret_header_h#\[}"
            _secret_header_h="${_secret_header_h%%\]*}"
            ;;
        *) _secret_header_h="${_secret_header_h%%:*}" ;;
    esac
    _secret_header_h="${_secret_header_h%.}"
    printf '%s' "$_secret_header_h" | tr '[:upper:]' '[:lower:]'
}

# _secret_header_octet N — 0-255 with no leading zero, what Rust's
# Ipv4Addr parses. A looser pattern stamped 127.0.0.999, which
# Url::parse refuses and curl hands to DNS (delta review of ddfa1032, N1).
_secret_header_octet() {
    case "$1" in
        [0-9] | [1-9][0-9] | 1[0-9][0-9] | 2[0-4][0-9] | 25[0-5]) return 0 ;;
    esac
    return 1
}

# _secret_header_mt_allowed HOST LIST — may a request to HOST carry the
# token? The rule of machine_token_header's header.
_secret_header_mt_allowed() {
    [ -n "$1" ] || return 1
    case "$1" in
        localhost | ::1) return 0 ;;
        *.*.*.*.*) ;;
        127.*.*.*)
            _secret_header_q="${1#127.}"
            _secret_header_q1="${_secret_header_q%%.*}"
            _secret_header_q="${_secret_header_q#*.}"
            _secret_header_q2="${_secret_header_q%%.*}"
            _secret_header_q3="${_secret_header_q#*.}"
            if _secret_header_octet "$_secret_header_q1" \
                && _secret_header_octet "$_secret_header_q2" \
                && _secret_header_octet "$_secret_header_q3"; then
                return 0
            fi
            ;;
    esac
    _secret_header_hit=1
    _secret_header_flags="$-"
    set -f
    # Split on the space the `tr` below leaves, whatever IFS the caller
    # set for itself (review ef2da426 F8: a comma or newline IFS read a
    # two-host list as one word and withheld every host). POSIX has no
    # `local`, so the caller's IFS — set, empty or unset — is put back.
    if [ -n "${IFS+set}" ]; then _secret_header_ifs="$IFS"; else _secret_header_ifs="(unset)"; fi
    IFS=' '
    _secret_header_words=$(printf '%s' "$2" | tr ',\n\t' '   ')
    for _secret_header_l in $_secret_header_words; do
        _secret_header_l=$(printf '%s' "${_secret_header_l%.}" | tr '[:upper:]' '[:lower:]')
        case "$_secret_header_l" in
            '') ;;
            .*) case "$1" in *"$_secret_header_l") _secret_header_hit=0 ;; esac ;;
            *) if [ "$_secret_header_l" = "$1" ]; then _secret_header_hit=0; fi ;;
        esac
    done
    if [ "$_secret_header_ifs" = "(unset)" ]; then unset IFS; else IFS="$_secret_header_ifs"; fi
    case "$_secret_header_flags" in *f*) ;; *) set +f ;; esac
    return "$_secret_header_hit"
}
