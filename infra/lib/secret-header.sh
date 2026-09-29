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
# (0700) under ${TMPDIR:-/tmp}. Either way the directory is removed by an
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
# `_secret_header_`, except the VAR you name.

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
        _secret_header_dir=$(mktemp -d "$_secret_header_base/boss-secret-header.XXXXXX") || {
            _secret_header_dir=""
            echo "secret_header: cannot make a private directory under $_secret_header_base — nothing was sent" >&2
            return 1
        }
        chmod 700 "$_secret_header_dir" || return 1
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
