#!/usr/bin/env bash
# a-lint-sources-a-header-lib-only-hermetic — a lint that SOURCES a lib
# which makes the machine-token header must first hand itself an
# environment with no token in reach: the token directory named and
# pointed at an empty one, the system of record's address and the host
# list unset, and sor.env pointed at a file that does not exist.
#
# WHY THIS EXISTS (backlog 920524dc, 844b936e and 2e1f609e, 2026-10-01).
# A lint is a question about a TREE; its answer must not depend on what
# credentials the asker holds. ci-images-are-pruned-by-age.sh sourced
# infra/forge/landed-train-shas.lib.sh inside `rc=$(resolve …)`, whose
# body is a `( … )` subshell — and that lib makes its token header WHEN
# SOURCED, for the BOSS_JOBS_URL it finds, through infra/lib/secret-header.sh,
# which refuses a first call in a subshell. In the conductor pod, where
# a token is mounted and BOSS_JOBS_URL names the estate, the lint went
# red on 3 FAILs and held every train from 00:04Z to 01:15Z over a
# credential event that changed no code. That lint was made hermetic by
# hand (#859), and the runners that call lints now hand them an empty
# token directory (crates/orchestrators/boss-cli/src/door_env.rs). This
# lint is the third half: the NEXT lint written in that shape is refused
# at its own pre-flight, wherever it is run from — by hand, by a test
# harness, by a runner nobody has taught yet.
#
# WHY EVERY SOURCING AND NOT ONLY THE SUBSHELL. `$( ( . lib ) )` is the
# shape that went red, because the subshell is where secret-header.sh
# refuses. The same sourcing at the top of a lint does not refuse — it
# writes the host's token into a header file on behalf of a fixture that
# never needed it, which is the accident door_env.rs exists to keep off
# the token. Telling the two apart would need a shell parser this lint
# should not grow; the hermetic block costs four lines, so it is asked
# of both.
#
# WHAT IT CHECKS. The header-making libs are DERIVED, never listed:
# infra/lib/secret-header.sh, plus every infra/lib/*.sh, infra/forge/*.sh
# and infra/estate/*.sh that itself sources secret-header.sh. Until
# 2026-10-06 the forge glob was `*.lib.sh` and infra/estate was not read,
# which found landed-train-shas.lib.sh alone; the day the host senders
# were stamped (backlog 2710c8fc) alert-lib.sh, cluster-deploy-lib.sh,
# cluster-node-lib.sh, node-roles.sh and observe-lib.sh became header
# libs, five lints sourced one of them with a token in reach, and the
# narrow glob saw none of the five. For every infra/lint/*.sh
# but this one, a line that sources (`.` or `source`) one of them — the
# lib's name, or a variable assigned a path naming it, ANYWHERE in the
# sourcing command up to its `||`, `&&`, `;` or the end of the line, so
# `. "$(dirname "${BASH_SOURCE[0]}")/../lib/secret-header.sh"` counts
# (review 19ecf37f F1) — must come AFTER all four of:
#
#     export BOSS_MACHINE_TOKEN_DIR=<an empty dir the lint made>
#     export BOSS_SOR_ENV=<a path that does not exist>
#     unset BOSS_JOBS_URL BOSS_MACHINE_TOKEN_HOSTS
#
# THE ONE EXCEPTION (2710c8fc). A file whose HEADER declares
# `# consist: skip — ... against a LIVE deployment; ...` is a live
# reader, not a tree lint: the roster leaves it out, its chore mounts
# the token, and its request is the one that must be stamped
# (conservation-invariants.sh, the hourly sweep). The header is read as
# gate.sh reads it — the first line that is not a comment ends it — so
# a lint cannot stay in the roster and claim the exception from its
# body. Build-only skips and mentions in prose do not qualify.
#
# (`NAME=value` and a separate `export NAME` count as the export, from
# the later of the two lines.) Comment lines and heredoc bodies are
# prose and are not read: a lint's refusal message may name
# `source infra/lib/secret-header.sh`.
#
# OUT OF SCOPE: RUNNING a script that makes the header, rather than
# sourcing a lib into the lint's own shell — the-executor-never-waits-on-
# its-visibility.sh runs boss-maintenance-wrap.sh, which sources
# secret-header.sh. That child inherits the lint's environment, and the
# runners that call lints (the consist check, the brief, the gate) hand
# every lint no token in reach (crates/orchestrators/boss-cli/src/door_env.rs),
# so the child does too. This lint reads only what it can see in one
# file: a sourcing line.
#
# EXIT STATUS: 0 clean, 1 a finding (or the self-test failed, or nothing
# was scanned). Reads the working tree with `find`, never git.
#
# Usage:  infra/lint/a-lint-sources-a-header-lib-only-hermetic.sh [--self-test]

set -uo pipefail

NAME="a-lint-sources-a-header-lib-only-hermetic"
cd "$(dirname "$0")/../.." || exit 1
# shellcheck source=infra/lint/lib/scanned.sh
. infra/lint/lib/scanned.sh || exit 3

# The awk below skips prose: a comment line, and the body of a heredoc
# (classified off its opener, up to its terminator). mawk is the gate
# image's awk — no `\s`, no interval, no IGNORECASE.
PROSE_AWK='
function heredoc_open(s,    t) {
    if (s ~ /<<</) return ""
    if (s !~ /<<-?[ \t]*["\047]?[A-Za-z_][A-Za-z0-9_]*/) return ""
    t = s
    sub(/.*<<-?[ \t]*["\047]?/, "", t)
    match(t, /^[A-Za-z_][A-Za-z0-9_]*/)
    return substr(t, 1, RLENGTH)
}
function prose(s,    t) {
    if (term != "") {
        t = s
        sub(/^[ \t]+/, "", t)
        if (t == term) term = ""
        return 1
    }
    if (s ~ /^[ \t]*#/) return 1
    return 0
}
'

# The libs that make a token header, one basename per line: secret-header.sh
# and every script under $1/infra/{lib,forge,estate} that sources it.
header_libs() { # root
    echo "secret-header.sh"
    local f
    for f in "$1"/infra/lib/*.sh "$1"/infra/forge/*.sh "$1"/infra/estate/*.sh; do
        [ -f "$f" ] || continue
        [ "${f##*/}" = secret-header.sh ] && continue
        LC_ALL=C awk "$PROSE_AWK"'
            { if (prose($0)) next; h = heredoc_open($0); if (h != "") term = h }
            /(^|[ \t;&|({])(\.|source)[ \t]/ && /secret-header\.sh/ { found = 1 }
            END { exit found ? 0 : 1 }
        ' "$f" && echo "${f##*/}"
    done
}

# One finding per file that sources a header lib unhermetically:
# `<file>:<line>: <what is missing>`. $1 = the libs, space-separated;
# the rest = the lint files.
scan() { # libs file...
    local libs="$1"; shift
    local f
    for f in "$@"; do
        LC_ALL=C awk -v libs="$libs" "$PROSE_AWK"'
            BEGIN { n = split(libs, L, " ") }
            {
                # The header as gate.sh header_declarations reads it: an
                # optional shebang, then comments and blank lines; the
                # first other line ends it. A declaration below that
                # leaves the lint in the roster and excuses nothing.
                if (FNR == 1) hdr = 1
                if (hdr && $0 !~ /^#/ && $0 !~ /^[ \t]*$/) hdr = 0
                if (hdr && !live && $0 ~ /^# consist: skip — [^;]* against a LIVE deployment;/) live = FNR
                if (prose($0)) next
                line = $0
                h = heredoc_open(line)
                if (h != "") term = h
                # `export NAME=value`, or the two-step `NAME=value` then a
                # bare `export NAME` (either order of lines; the later one
                # is when the child first sees it).
                if (!tokdir && line ~ /(^|[ \t;&|({])export[ \t]+BOSS_MACHINE_TOKEN_DIR=[^ \t;]/) tokdir = FNR
                if (!sorenv && line ~ /(^|[ \t;&|({])export[ \t]+BOSS_SOR_ENV=[^ \t;]/) sorenv = FNR
                if (!tdset && line ~ /(^|[ \t;&|({])BOSS_MACHINE_TOKEN_DIR=[^ \t;]/) tdset = FNR
                if (!tdexp && line ~ /(^|[ \t;&|({])export[ \t]+([A-Za-z_][A-Za-z0-9_]*[ \t]+)*BOSS_MACHINE_TOKEN_DIR([ \t;]|$)/) tdexp = FNR
                if (!seset && line ~ /(^|[ \t;&|({])BOSS_SOR_ENV=[^ \t;]/) seset = FNR
                if (!seexp && line ~ /(^|[ \t;&|({])export[ \t]+([A-Za-z_][A-Za-z0-9_]*[ \t]+)*BOSS_SOR_ENV([ \t;]|$)/) seexp = FNR
                if (!tokdir && tdset && tdexp) tokdir = (tdset > tdexp ? tdset : tdexp)
                if (!sorenv && seset && seexp) sorenv = (seset > seexp ? seset : seexp)
                if (line ~ /(^|[ \t;&|({])unset[ \t]/) {
                    if (!nourl && line ~ /[ \t]BOSS_JOBS_URL([ \t;]|$)/) nourl = FNR
                    if (!nohosts && line ~ /[ \t]BOSS_MACHINE_TOKEN_HOSTS([ \t;]|$)/) nohosts = FNR
                }
                # A variable assigned a path naming a lib.
                for (i = 1; i <= n; i++) {
                    if (!index(line, L[i])) continue
                    if (match(line, /(^|[ \t;(])(local[ \t]+|export[ \t]+|readonly[ \t]+)?[A-Za-z_][A-Za-z0-9_]*=/)) {
                        v = substr(line, RSTART, RLENGTH - 1)
                        sub(/^[ \t;(]/, "", v)
                        sub(/^(local|export|readonly)[ \t]+/, "", v)
                        V[v] = 1
                    }
                }
                # A sourcing of one: the WHOLE sourcing command, up to its
                # terminator (||, &&, ; or the end of the line), names a
                # lib or such a variable ANYWHERE — so the repo idiom
                # `. "$(dirname "${BASH_SOURCE[0]}")/../lib/x.sh"`, whose
                # first word is `$(dirname`, is read (review 19ecf37f F1).
                if (!src && match(line, /(^|[ \t;&|({])(\.|source)[ \t]+/)) {
                    cmd = substr(line, RSTART + RLENGTH)
                    sub(/(\|\||&&|;).*/, "", cmd)
                    hit = 0
                    for (i = 1; i <= n; i++) if (index(cmd, L[i])) hit = 1
                    if (!hit) for (v in V) {
                        if (index(cmd, "${" v "}")) hit = 1
                        rest = cmd
                        while (!hit && (k = index(rest, "$" v)) > 0) {
                            after = substr(rest, k + 1 + length(v), 1)
                            if (after !~ /[A-Za-z0-9_]/) hit = 1
                            rest = substr(rest, k + 1)
                        }
                    }
                    if (hit) src = FNR
                }
            }
            END {
                if (!src) exit 0
                if (live && live < src) exit 0
                miss = ""
                if (!tokdir || tokdir > src) miss = miss " export BOSS_MACHINE_TOKEN_DIR"
                if (!sorenv || sorenv > src) miss = miss " export BOSS_SOR_ENV"
                if (!nourl || nourl > src) miss = miss " unset BOSS_JOBS_URL"
                if (!nohosts || nohosts > src) miss = miss " unset BOSS_MACHINE_TOKEN_HOSTS"
                if (miss != "") print FILENAME ":" src ":" miss
            }
        ' "$f"
    done | LC_ALL=C sort -u
}

# ---------------------------------------------------------------------------
# Self-test — fixtures in a temp directory this run owns, never under
# infra/, where a file is judged as real. Runs on every invocation: a
# scanner whose regex stopped matching passes every tree.
# ---------------------------------------------------------------------------
self_test() {
    local tmp hits want libs
    tmp="$(mktemp -d)" || { echo "$NAME: cannot make a temp dir for the self-test" >&2; return 1; }

    # The derivation: a forge lib that sources secret-header.sh is a
    # header lib; one that only names it in prose or a heredoc is not.
    mkdir -p "$tmp/root/infra/lib" "$tmp/root/infra/forge"
    : > "$tmp/root/infra/lib/secret-header.sh"
    printf '%s\n' 'if . "$(dirname "${BASH_SOURCE[0]}")/../lib/secret-header.sh" 2>/dev/null; then :; fi' \
        > "$tmp/root/infra/forge/makes-a-header.lib.sh"
    printf '%s\n' '# . infra/lib/secret-header.sh is what this lib does NOT do' 'cat <<EOF' \
        '. infra/lib/secret-header.sh' 'EOF' > "$tmp/root/infra/forge/only-prose.lib.sh"
    libs="$(header_libs "$tmp/root" | LC_ALL=C sort | tr '\n' ' ')"
    if [ "$libs" != "makes-a-header.lib.sh secret-header.sh " ]; then
        echo "$NAME: self-test FAILED — the header libs must be derived from what sources secret-header.sh; got: [$libs]" >&2
        rm -rf "$tmp"; return 1
    fi
    libs="secret-header.sh landed-train-shas.lib.sh"

    # The production sweep's declaration needs the mounted credential;
    # neither a build-only skip nor prose grants that classification.
    cat > "$tmp/live.sh" <<'SH'
# consist: skip — psql + curl against a LIVE deployment; an invariant on the running system, not on a tree
. infra/lib/secret-header.sh || exit 3
SH
    cat > "$tmp/build-only.sh" <<'SH'
# consist: skip — reads a built binary; the gate runs it after build
. infra/lib/secret-header.sh || exit 3
SH
    cat > "$tmp/misdeclared.sh" <<'SH'
# prose: consist: skip — psql + curl against a LIVE deployment; this is not a declaration
. infra/lib/secret-header.sh || exit 3
SH
    cat > "$tmp/late-live.sh" <<'SH'
. infra/lib/secret-header.sh || exit 3
# consist: skip — psql + curl against a LIVE deployment; declared too late
SH
    # The roster (gate.sh header_declarations) reads the declaration in
    # the HEADER only — the first line that is not a comment ends it. One
    # written below a line of code leaves the lint IN the roster, so it
    # must not also excuse it from the hermetic block (rescue review of
    # 759721e7, 2026-10-06: the first draft accepted it anywhere above
    # the sourcing).
    cat > "$tmp/body-live.sh" <<'SH'
set -uo pipefail
# consist: skip — psql + curl against a LIVE deployment; declared in the body, where the roster does not read it
. infra/lib/secret-header.sh || exit 3
SH

    # Refused 1 — the shape that held the trains on 2026-10-01: a lib
    # named by a variable, sourced inside $( ( … ) ), no hermetic block.
    cat > "$tmp/subshell.sh" <<'SH'
resolver="$here/../forge/landed-train-shas.lib.sh"
keys=$( ( set -uo pipefail
      . "$resolver"
      landed_train_shas curl "$u" 120 selftest ) )
SH
    # Refused 2 — sourced at the top, by name, no hermetic block.
    cat > "$tmp/toplevel.sh" <<'SH'
set -uo pipefail
source infra/lib/secret-header.sh
SH
    # Refused 3 — the token dir emptied, the address left in reach.
    cat > "$tmp/partial.sh" <<'SH'
export BOSS_MACHINE_TOKEN_DIR="$tmp/no-machine-token"
export BOSS_SOR_ENV="$tmp/no-sor.env"
lib=infra/forge/landed-train-shas.lib.sh
out=$( ( . "${lib}" ) )
SH
    # Refused 4 — the whole block, but AFTER the sourcing it was for.
    cat > "$tmp/late.sh" <<'SH'
out=$( ( . infra/forge/landed-train-shas.lib.sh; landed_train_shas ) )
export BOSS_MACHINE_TOKEN_DIR="$tmp/no-machine-token"
export BOSS_SOR_ENV="$tmp/no-sor.env"
unset BOSS_JOBS_URL BOSS_MACHINE_TOKEN_HOSTS
SH
    # Refused 5 — the repo's idiom for a lib beside the script, whose
    # first word is a command substitution (review 19ecf37f F1: 11
    # sourcing lines under infra/lint have this shape).
    cat > "$tmp/dirname.sh" <<'SH'
set -uo pipefail
out=$( ( . "$(dirname "${BASH_SOURCE[0]}")/../lib/secret-header.sh" || exit 3; secret_header H x ) )
SH
    # Refused 6 — the same through git, by `source`, after a fallback.
    cat > "$tmp/toplevel-git.sh" <<'SH'
true
source "$(git rev-parse --show-toplevel)/infra/forge/landed-train-shas.lib.sh" 2>/dev/null && landed_train_shas
SH
    # Accepted 1 — ci-images-are-pruned-by-age.sh's shape since #859.
    cat > "$tmp/hermetic.sh" <<'SH'
resolver="$here/../forge/landed-train-shas.lib.sh"
mkdir -p "$tmp/no-machine-token"
export BOSS_MACHINE_TOKEN_DIR="$tmp/no-machine-token"
export BOSS_SOR_ENV="$tmp/no-sor.env"
unset BOSS_JOBS_URL BOSS_MACHINE_TOKEN_HOSTS
resolve() { ( set -uo pipefail; . "$resolver"; landed_train_shas curl "$u" 120 x ) >"$tmp/keys"; echo $?; }
rc=$(resolve)
SH
    # Accepted 3 — the two-step export (review 19ecf37f), each half on
    # its own line, before the idiom's sourcing.
    cat > "$tmp/two-step.sh" <<'SH'
e="$tmp/no-machine-token"; mkdir -p "$e"
BOSS_MACHINE_TOKEN_DIR=$e; export BOSS_MACHINE_TOKEN_DIR
BOSS_SOR_ENV="$tmp/no-sor.env"
export BOSS_SOR_ENV
unset BOSS_JOBS_URL BOSS_MACHINE_TOKEN_HOSTS
out=$( ( . "$(dirname "${BASH_SOURCE[0]}")/../lib/secret-header.sh" || exit 3 ) )
SH
    # Accepted 2 — prose and unrelated libs: a comment and a heredoc that
    # name the lib, a copy of it, and a sourcing of another lib.
    cat > "$tmp/prose.sh" <<'SH'
# . infra/lib/secret-header.sh would make a header here
. infra/lint/lib/scanned.sh || exit 3
cp "$here/../lib/secret-header.sh" "$tmp/lib/secret-header.sh"
cat >&2 <<'MSG'
  in a file and hand curl the file — source infra/lib/secret-header.sh
MSG
SH
    hits="$(scan "$libs" "$tmp"/*.sh)"
    want="$(printf '%s\n' \
        "$tmp/body-live.sh:3: export BOSS_MACHINE_TOKEN_DIR export BOSS_SOR_ENV unset BOSS_JOBS_URL unset BOSS_MACHINE_TOKEN_HOSTS" \
        "$tmp/build-only.sh:2: export BOSS_MACHINE_TOKEN_DIR export BOSS_SOR_ENV unset BOSS_JOBS_URL unset BOSS_MACHINE_TOKEN_HOSTS" \
        "$tmp/misdeclared.sh:2: export BOSS_MACHINE_TOKEN_DIR export BOSS_SOR_ENV unset BOSS_JOBS_URL unset BOSS_MACHINE_TOKEN_HOSTS" \
        "$tmp/late-live.sh:1: export BOSS_MACHINE_TOKEN_DIR export BOSS_SOR_ENV unset BOSS_JOBS_URL unset BOSS_MACHINE_TOKEN_HOSTS" \
        "$tmp/dirname.sh:2: export BOSS_MACHINE_TOKEN_DIR export BOSS_SOR_ENV unset BOSS_JOBS_URL unset BOSS_MACHINE_TOKEN_HOSTS" \
        "$tmp/toplevel-git.sh:2: export BOSS_MACHINE_TOKEN_DIR export BOSS_SOR_ENV unset BOSS_JOBS_URL unset BOSS_MACHINE_TOKEN_HOSTS" \
        "$tmp/late.sh:1: export BOSS_MACHINE_TOKEN_DIR export BOSS_SOR_ENV unset BOSS_JOBS_URL unset BOSS_MACHINE_TOKEN_HOSTS" \
        "$tmp/partial.sh:4: unset BOSS_JOBS_URL unset BOSS_MACHINE_TOKEN_HOSTS" \
        "$tmp/subshell.sh:3: export BOSS_MACHINE_TOKEN_DIR export BOSS_SOR_ENV unset BOSS_JOBS_URL unset BOSS_MACHINE_TOKEN_HOSTS" \
        "$tmp/toplevel.sh:2: export BOSS_MACHINE_TOKEN_DIR export BOSS_SOR_ENV unset BOSS_JOBS_URL unset BOSS_MACHINE_TOKEN_HOSTS" \
        | LC_ALL=C sort -u)"
    if [ "$hits" != "$want" ]; then
        echo "$NAME: self-test FAILED — tree sourcing must be hermetic, including build-only skips and misdeclared live readers; got:" >&2
        printf '%s\n' "$hits" >&2
        rm -rf "$tmp"; return 1
    fi
    rm -rf "$tmp"
    echo "$NAME: self-test ok — header libs are derived; ten unhermetic or misdeclared shapes are refused; hermetic readers and an explicit preceding live-deployment declaration pass"
}

self_test || exit 1
if [ "${1:-}" = "--self-test" ]; then exit 0; fi

# ---------------------------------------------------------------------------
# The tree.
# ---------------------------------------------------------------------------
[ -d infra/lint ] || { echo "$NAME: infra/lint/ does not exist" >&2; exit 1; }
libs="$(header_libs . | tr '\n' ' ')"
lints=()
for f in infra/lint/*.sh; do
    [ "$f" = "infra/lint/$NAME.sh" ] && continue
    lints+=("$f")
done
hits="$(scan "$libs" "${lints[@]}")"
lint_scanned "$NAME" "${#lints[@]}" "lint(s) against header lib(s): ${libs% }"
if [ -n "$hits" ]; then
    while IFS= read -r site; do
        [ -n "$site" ] || continue
        echo "$NAME: ${site%%: *} — sources a lib that makes the machine-token header without, before it:${site#*:*:}" >&2
    done <<EOF
$hits
EOF
    cat >&2 <<'MSG'

  A lib that makes the token header reads the token and the address
  from the environment it is sourced in, so a lint that sources one
  answers differently on a host that holds the token (backlog 920524dc:
  every train held for 71 minutes by a credential event). Before the
  first sourcing, give the lint no token in reach:

    mkdir -p "$tmp/no-machine-token"
    export BOSS_MACHINE_TOKEN_DIR="$tmp/no-machine-token"
    export BOSS_SOR_ENV="$tmp/no-sor.env"
    unset BOSS_JOBS_URL BOSS_MACHINE_TOKEN_HOSTS

  (infra/lint/ci-images-are-pruned-by-age.sh is the worked example.)
MSG
    exit 1
fi

echo "$NAME: ok — header-making readers are hermetic or explicitly declared against a LIVE deployment"
exit 0
