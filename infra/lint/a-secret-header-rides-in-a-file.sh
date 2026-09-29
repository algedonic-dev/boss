#!/usr/bin/env bash
# a-secret-header-rides-in-a-file — no file under infra/ hands a secret
# HTTP header to a process in its ARGUMENTS. The machine token and every
# `Authorization:` reach curl as `-H @<file>`, a file written by
# infra/lib/secret-header.sh (0600, in a private directory removed at
# exit) or by a `printf … > file` of the same shape.
#
# WHY THIS EXISTS (backlog 5f3ad356, 2026-09-27; the sweep on cfa678e9,
# 2026-09-28). `curl -H "x-boss-machine-token: $BOSS_MACHINE_TOKEN"`
# puts the token in curl's command line, which any local user reads in
# `ps -eo args` or /proc/<pid>/cmdline for as long as curl runs. The
# publish review of 8d7a3507 found it in two forge scripts; the sweep
# found 28 sites in 13 files, and four more secrets that a grep for a
# variable NAMED *TOKEN* misses: the pod door read the token into `TOK`,
# the CI image report built `auth_header="Authorization: Bearer $token"`
# and passed `-H "$auth_header"`, and two more passed `$FORGE_TOKEN` and
# `$TOKEN` as `Authorization:`. So this lint pins the HEADER NAME, not
# any variable's name: a secret is whatever rides as
# `x-boss-machine-token:`, `x-boss-runner-credential:` or `Authorization:`.
# The runner credential joined on 2026-09-29 (design f623e425 option A;
# backlog 6c9183de): the jobs API believes a declared writer only on it,
# so a copy of it in a `ps` line is the forgery that design refuses.
#
# WHAT IT CHECKS. Every file under infra/ except prose (*.md) and this
# lint; lines whose first word is a `#` comment are prose too. Header
# names are matched case-insensitively (HTTP's rule). A line is refused
# when:
#
#   argv    a `-H` / `--header` argument spells the header name — the
#           value, literal or expanded, is then in the argument; or
#   built   the header name and its colon are followed on the line by
#           an expansion (`$…`) or a printf `%s`: a header line built
#           in the shell, which is how `auth_header=…` then `-H
#           "$auth_header"` hid it — UNLESS the line is one of the two
#           shapes that put it in a file:
#             * a call to `secret_header` (infra/lib/secret-header.sh);
#             * a `printf` redirected to a file (`printf … > "$f"`) and
#               not captured by `$(…)` or backticks — a captured printf
#               is an argument, whatever the line redirects after it —
#               which github-act.sh, forge-converge.sh,
#               move-forgejo-data.sh and prune-registry-versions.sh use.
#
# NO ALLOWLIST. A site that needs the header uses the lib.
#
# EXIT STATUS: 0 clean, 1 a site in argv (or the self-test failed, or
# nothing was scanned). Reads the working tree with `find`, never git.
#
# Usage:  infra/lint/a-secret-header-rides-in-a-file.sh [--self-test]

set -uo pipefail

NAME="a-secret-header-rides-in-a-file"
cd "$(dirname "$0")/../.." || exit 1
# shellcheck source=infra/lint/lib/scanned.sh
. infra/lint/lib/scanned.sh || exit 3

# The files under judgement below $1: every file but prose and this lint.
judged_files() { # dir
    find "$1" -type f ! -name '*.md' ! -path "*/lint/$NAME.sh" -print0
}

# Every finding under $1, `<file>:<line>: <argv|built>` per line, sorted.
# Empty output = clean. mawk is the gate image's awk: no `\s`, no
# interval, no IGNORECASE — `[ \t]` and tolower() instead.
scan() { # dir
    judged_files "$1" | xargs -0 -r env LC_ALL=C awk '
        {
            line = " " tolower($0)
            if (line ~ /^[ \t]*#/) next
            if (line ~ /[ \t;&|(=+{]--?(h|header)[= \t]*["\047]?[ \t]*(x-boss-machine-token|x-boss-runner-credential|authorization)[ \t]*:/) {
                print FILENAME ":" FNR ": argv"
                next
            }
            if (line ~ /(x-boss-machine-token|x-boss-runner-credential|authorization)[ \t]*:.*(\$|%s)/) {
                if (line ~ /[ \t;&|(]secret_header[ \t]/) next
                # A printf CAPTURED by $(…) or backticks is not written to
                # a file, whatever the line redirects after it (HOLD F2 of
                # the review of car 676f4cdd: -H "$(printf …)" > "$out").
                if (line !~ /\$\([ \t]*printf/ && line !~ /`[ \t]*printf/ \
                    && line ~ /[ \t;&|(]printf[ \t].*[ \t]>>?[ \t]*["\047]?[$\/]/) next
                print FILENAME ":" FNR ": built"
            }
        }
    ' | LC_ALL=C sort -u
}

# ---------------------------------------------------------------------------
# Self-test — fixtures in a temp directory this run owns, never under
# infra/, where a file is judged as real. Runs on every invocation: a
# scanner whose regex stopped matching passes every tree.
# ---------------------------------------------------------------------------
self_test() {
    local tmp hits want
    tmp="$(mktemp -d)" || { echo "$NAME: cannot make a temp dir for the self-test" >&2; return 1; }

    # Accepted: the lib's call and its use, a -H @file, the printf-to-file
    # shapes in the tree today, a header that is no secret, prose that
    # names a header with no colon, and a comment that names the defect.
    cat > "$tmp/accepted.sh" <<'SH'
# curl -H "x-boss-machine-token: $BOSS_MACHINE_TOKEN" is what this replaced
secret_header MT_HDR ${BOSS_MACHINE_TOKEN:+"x-boss-machine-token: $BOSS_MACHINE_TOKEN"}
curl -fsS ${MT_HDR:+-H "$MT_HDR"} "$url"
    secret_header AUTH_HDR "Authorization: Bearer $token"
curl -H "@$AUTH" -H 'Content-Type: application/json' "$u"
    (umask 077 && printf 'Authorization: Bearer %s\n' "$token" >"$WORK/auth" \
( umask 077; printf 'header = "Authorization: Basic %s"\n' "$B64" > "$CFG" )
curl -H "x-boss-user: $BOSS_USER" -H "Accept: application/json" "$u"
echo "the Authorization header rides in a file"
SH
    hits="$(scan "$tmp")"
    if [ -n "$hits" ]; then
        echo "$NAME: self-test FAILED — every accepted shape must pass; got:" >&2
        printf '%s\n' "$hits" >&2
        rm -rf "$tmp"; return 1
    fi

    # Refused, each on a known line: the 28 sites' shape; the pod door's
    # $TOK; the header built in a variable; a forge token; wget's
    # --header=; a printf captured, not written; a literal glued to -H;
    # and a printf captured INTO -H on a line that also redirects to a
    # file, by $(…) and by backticks (HOLD F2, review of car 676f4cdd).
    cat > "$tmp/refused.sh" <<'SH'
curl ${BOSS_MACHINE_TOKEN:+-H "x-boss-machine-token: $BOSS_MACHINE_TOKEN"} "$u"
[ -n "$TOK" ] && args+=( -H "X-Boss-Machine-Token: $TOK" )
                    auth_header="Authorization: Bearer $token"
curl -H "Authorization: token $FORGE_TOKEN" "$u"
wget --header="Authorization: token ${FORGE_TOKEN}" "$u"
hdr=$(printf 'Authorization: Bearer %s' "$t" 2>/dev/null)
curl -H'Authorization: Bearer literal' "$u"
curl -H "$(printf 'Authorization: Bearer %s' "$t")" "$u" > "$out"
curl -H "`printf 'x-boss-machine-token: %s' "$t"`" "$u" > "$out"
curl -fsS -H "x-boss-runner-credential: $rc_value" "$u"
SH
    hits="$(scan "$tmp")"
    want="$(printf '%s\n' "$tmp/refused.sh:1: argv" "$tmp/refused.sh:2: argv" "$tmp/refused.sh:3: built" \
        "$tmp/refused.sh:4: argv" "$tmp/refused.sh:5: argv" "$tmp/refused.sh:6: built" \
        "$tmp/refused.sh:7: argv" "$tmp/refused.sh:8: built" "$tmp/refused.sh:9: built" \
        "$tmp/refused.sh:10: argv" | LC_ALL=C sort -u)"
    if [ "$hits" != "$want" ]; then
        echo "$NAME: self-test FAILED — ten refused shapes must be named by file:line; got:" >&2
        printf '%s\n' "$hits" >&2
        rm -rf "$tmp"; return 1
    fi
    rm -rf "$tmp"
    echo "$NAME: self-test ok — the lib's call, -H @file, printf to a file, a non-secret header and prose pass; -H with the token, \$TOK, a header built in a variable, a forge token, wget --header=, a captured printf, a glued literal, a printf captured into -H beside a redirect (\$(…) and backticks) and the runner credential in -H are each named by file:line"
}

self_test || exit 1
if [ "${1:-}" = "--self-test" ]; then exit 0; fi

# ---------------------------------------------------------------------------
# The tree.
# ---------------------------------------------------------------------------
[ -d infra ] || { echo "$NAME: infra/ does not exist" >&2; exit 1; }
hits="$(scan infra)"
lint_scanned "$NAME" "$(judged_files infra | tr -cd '\0' | wc -c | tr -d ' ')" "file(s) under infra/"
if [ -n "$hits" ]; then
    while IFS= read -r site; do
        [ -n "$site" ] || continue
        echo "$NAME: $site — a secret header in a process's arguments" >&2
    done <<EOF
$hits
EOF
    cat >&2 <<'MSG'

  A header in argv is readable by every local user in ps and
  /proc/<pid>/cmdline while the process runs (backlog 5f3ad356). Put it
  in a file and hand curl the file — source infra/lib/secret-header.sh
  in the script's own shell, after its trap, then:

    secret_header MT_HDR ${BOSS_MACHINE_TOKEN:+"x-boss-machine-token: $BOSS_MACHINE_TOKEN"}
    curl ${MT_HDR:+-H "$MT_HDR"} "$url"

  `argv`: a -H/--header argument spells the header. `built`: a header
  line is assembled in the shell (then passed as -H "$var") — build it
  with secret_header instead, which writes the 0600 file.
MSG
    exit 1
fi

echo "$NAME: ok — no file under infra/ puts x-boss-machine-token, x-boss-runner-credential or Authorization in a process's arguments"
exit 0
