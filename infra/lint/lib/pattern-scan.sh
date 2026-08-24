# pattern-scan.sh — the shared body of every "this pattern must not
# appear" lint, so none of them has to remember the two things they
# all forget.
#
# WHY THIS EXISTS. A lint that forbids a pattern has to NAME that
# pattern, and so does the test that proves the lint works. Both are
# then hits. `no-session-paths.sh` remembered to exclude itself.
# `one-palette.sh` remembered too — and still went red on a train,
# because the mocked spec that documents its rule also names
# `prefers-color-scheme`, and the lint and the spec only met each
# other during assembly (2026-08-24: four checks green, one red, nine
# cars held). Each lint got this right or wrong on its own; a rule
# every author must re-derive is a rule that will be got wrong again.
#
# THE CONVENTION, so the exemption needs no list:
#   a lint at infra/lint/<name>.sh is proven by files whose path
#   contains <name> and which live under a tests/ directory or carry
#   a .spec./.test. segment.
# Both are excluded by construction. Nothing else is — a lint that
# wants a domain exemption (no-session-paths lets docs/ name paths,
# because runbooks legitimately tell that story) states it as an extra
# argument, in its own file, where a reader can see the judgement.
#
# USAGE
#   . "$(dirname "$0")/lib/pattern-scan.sh"
#   hits=$(pattern_scan 'prefers-color-scheme' -- 'apps/' 'libs/')
#   hits=$(pattern_scan 'X' --exclude ':!docs/' -- 'crates/')
#
# Returns hits on stdout (empty when clean); the caller decides the
# message, because the remediation text is the part that has to be
# written by someone who understands the rule.

pattern_scan() {
    local pattern="$1"; shift
    local name; name="$(basename "${BASH_SOURCE[1]:-$0}" .sh)"
    local excludes=(":!infra/lint/${name}.sh")

    # Caller-declared exemptions, before the -- separator.
    while [ "$#" -gt 0 ] && [ "$1" != "--" ]; do
        if [ "$1" = "--exclude" ]; then
            shift; excludes+=("$1")
        fi
        shift
    done
    [ "${1:-}" = "--" ] && shift

    # The proof-exempts-itself rule. Discovered, not listed: a file is
    # a proof of THIS lint if its path names the lint and it lives
    # where tests live. `git ls-files` so it matches what git grep
    # searches, and so a file nobody tracked cannot buy an exemption.
    local f
    while IFS= read -r f; do
        case "$f" in
            *"$name"*)
                case "$f" in
                    */tests/*|*.spec.*|*.test.*|*_test.*|*/testdata/*)
                        excludes+=(":!${f}") ;;
                esac ;;
        esac
    done < <(git ls-files)

    git grep -nE "$pattern" -- "$@" "${excludes[@]}" || true
}
