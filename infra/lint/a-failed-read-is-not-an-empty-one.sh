#!/usr/bin/env bash
# a-failed-read-is-not-an-empty-one.sh — a page does not paint an
# outage as its empty state.
#
# WHY THIS EXISTS (backlog 223ebcd6, after 7a7bfc88). One line of
# punctuation manufactures the one forbidden failure mode, silence:
#
#     const pBody = pResp.ok ? await pResp.json() : [];
#
# A non-2xx becomes an empty list, the page renders "no purchase
# orders" / "$0.00 outstanding" / a hidden column, and nothing on it
# says a read failed. It was found by hand on /ux/support, then in six
# more pages; 7a7bfc88 lifted the fix into apps/web/src/data/readState.ts
# and closed with the adoption unfinished, so on 2026-09-23 an operator's
# grep of origin/main b9ffdc81 found it still live in VendorsList (two
# reads, unchanged since 2026-07-01), VendorPage (four) and ExecPage
# (one). A fix adopted page by page is a fix that stops one page short;
# this lint is the half of that fix a review cannot forget.
#
# THE RULE. A response whose `ok` is false is never parsed into an
# empty value — `[]`, `null` or `{}` — on one or several lines, nor
# answered with one by an early return or a discarded PagedResult
# page arm. Explicit no-reading consumers are counted below. Record the
# outcome instead (`readStateOfResponse` in data/readState.ts, or a
# `failed` flag the template renders with `.load-failed`), then parse
# only the answer that worked.
#
# WHAT IS EXEMPT, each a judgement a reader can check:
#   * apps/web/src/data/readState.ts — the helper that names the class.
#   * `*.test.ts` / `*.spec.ts` — a test stages a response.
#   * prose — a comment quoting the line to tell its story (reads.ts
#     and readState.ts both do).
#   * the ALLOW list below, file => exact count. Each entry carries its
#     reason, and the count must EQUAL the file's matches: fixing one
#     site without lowering the count leaves a hole shaped like it.
#
# NOT COVERED: arbitrary control flow or a catch returning an empty
# value. This is a bounded syntactic check, not a TypeScript proof.
#
# EXIT STATUS (house style, infra/lint/lib/git-answer.sh):
#   0  the tree was read and no failed read is painted as an empty one
#   1  a violation, or an allowance that no longer matches its count
#   3  the tree was never read — a fact about the MACHINE; never `clean`
set -euo pipefail
LINT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$LINT_DIR/../.."
# shellcheck source=infra/lint/lib/pattern-scan.sh
. "$LINT_DIR/lib/pattern-scan.sh" || exit 3
# shellcheck source=infra/lint/lib/allowlist.sh
. "$LINT_DIR/lib/allowlist.sh" || exit 3

NAME=a-failed-read-is-not-an-empty-one

# `<x>.ok ? await <y>.json() : []` — with `null` or `{}` in place of
# `[]`, and the await optionally parenthesised. ERE, no `{n}` intervals.
TERNARY='\.ok[[:space:]]*\?[[:space:]]*\(?[[:space:]]*await[[:space:]]+[A-Za-z_$][A-Za-z0-9_$.]*\.json\(\)[[:space:]]*\)?[[:space:]]*:[[:space:]]*(\[\]|null|\{\})'
# `if (!<x>.ok) return [];` (or `{}`, braced or not) — the early-return
# spelling of the same line (backlog 865d3d51). The additional scanner
# includes null and split lines (98ef10a6); legitimate no-reading
# consumers need the explicit counted judgement below.
EARLY_RETURN='if[[:space:]]*\([[:space:]]*![[:space:]]*[A-Za-z_$][A-Za-z0-9_$.]*\.ok[[:space:]]*\)[[:space:]]*(\{[[:space:]]*)?return[[:space:]]+(\[\]|\{\})'
PATTERN="(${TERNARY})|(${EARLY_RETURN})"

# file => exact number of matches it may carry.
#   classes.svelte.ts / departments.svelte.ts — the `null` is not
#     painted: the next line refuses anything but an array (or a
#     department list) and sets the registry's `error` arm.
# AP aging and ledger render null as unavailable alerts; summary now
# retains its cause. Design's missing fallback opens the real job page.
# stationMap's nullable contracts, the machine/converge unknown arms
# and yard cannot-see lane do not manufacture successful empty reads.
# AccountsList/SupportPage retain a failed ReadState alongside each
# narrowed page; their counts/cells/empty surfaces consume that state.
declare -A ALLOW=(
    ["libs/web-kit/src/session/classes.svelte.ts"]=1
    ["libs/web-kit/src/session/departments.svelte.ts"]=1
    ["apps/web/src/finance/api.ts"]=1
    ["apps/web/src/finance/ledger.ts"]=1
    ["apps/web/src/it/design/DesignReviewPage.svelte"]=1
    ["apps/web/src/it/monitoring/stationMap.ts"]=2
    ["apps/web/src/it/yard/yard-machines.ts"]=1
    ["apps/web/src/it/yard/yard-converge.ts"]=1
    ["apps/web/src/it/yard/yard.ts"]=1
    ["apps/web/src/accounts/AccountsList.svelte"]=3
    ["apps/web/src/support/SupportPage.svelte"]=2
)
allowlist_paths_exist "$NAME" "${!ALLOW[@]}"

hits=$(pattern_scan "$PATTERN" \
    --exclude ':!apps/web/src/data/readState.ts' \
    --exclude ':!*.test.ts' \
    --exclude ':!*.spec.ts' \
    -- 'apps/' 'libs/') || exit $?

# The same shapes across lines, plus a PagedResult whose failure arm
# is discarded (98ef10a6). Keep original line numbers and refuse a
# failed git/read/interpreter rather than certifying an unread tree.
extra=$(python3 - <<'PY'
import re, subprocess, sys
r = subprocess.run(['git', 'ls-files', '-z', '--', 'apps/', 'libs/'], capture_output=True)
if r.returncode:
    sys.stderr.buffer.write(r.stderr)
    sys.exit(3)
ident = r'[A-Za-z_$][A-Za-z0-9_$.]*'
empty = r'(?:\[\]|null|\{\})'
patterns = [
    rf'\.ok\s*\?\s*\(?\s*await\s+{ident}\.json\(\)\s*\)?\s*:\s*{empty}',
    rf'if\s*\(\s*!\s*{ident}\.ok\s*\)\s*(?:\{{\s*)?return\s+{empty}',
    rf'{ident}\.kind\s*===\s*[\"\']ready[\"\']\s*\?\s*{ident}\.page\s*:\s*null',
]
try:
    for path in r.stdout.decode().split('\0'):
        if not path.endswith(('.ts', '.tsx', '.js', '.svelte')) or path == 'apps/web/src/data/readState.ts' or '.test.' in path or '.spec.' in path:
            continue
        source = open(path, encoding='utf-8').read()
        keep = lambda m: '\n' * m.group(0).count('\n')
        code = re.sub(r'<!--[\s\S]*?-->|/\*[\s\S]*?\*/', keep, source)
        code = re.sub(r'(^|[^:])//[^\n]*', r'\1', code, flags=re.M)
        lines = source.splitlines()
        for index, pattern in enumerate(patterns):
            for match in re.finditer(pattern, code):
                # Existing scanner owns single-line response ternaries.
                if index == 0 and '\n' not in match.group():
                    continue
                # Existing scanner owns single-line []/{} early returns.
                if index == 1 and '\n' not in match.group() and not match.group().endswith('null'):
                    continue
                line = code.count('\n', 0, match.start()) + 1
                print(f'{path}:{line}:{lines[line-1]}')
except (OSError, UnicodeError) as error:
    print(f'a-failed-read-is-not-an-empty-one: CANNOT ANSWER — {error}', file=sys.stderr)
    sys.exit(3)
PY
) || exit 3
hits=$(printf '%s\n%s\n' "$hits" "$extra" | sort -u)

declare -A counts=() lines=()
while IFS= read -r hit; do
    [ -n "$hit" ] || continue
    file="${hit%%:*}"
    rest="${hit#*:}"
    code="${rest#*:}"
    # A comment quoting the line is prose, not a read.
    case "$(printf '%s' "$code" | sed 's/^[[:space:]]*//')" in
        //*|\**|/\**) continue ;;
    esac
    counts["$file"]=$(( ${counts["$file"]:-0} + 1 ))
    lines["$file"]="${lines[$file]:-}"$'\n'"    $hit"
done <<EOF
$hits
EOF

fail=0
used=""
for file in "${!counts[@]}"; do
    allowed="${ALLOW[$file]:-0}"
    if [ "${counts[$file]}" -gt "$allowed" ]; then
        echo "$NAME: FAIL $file — ${counts[$file]} swallowed read(s), allowance $allowed:${lines[$file]}" >&2
        fail=1
    elif [ "$allowed" -gt 0 ]; then
        used="$used"$'\n'"$file"
        if [ "${counts[$file]}" -lt "$allowed" ]; then
            echo "$NAME: FAIL $file — allowance $allowed, but only ${counts[$file]} site(s) left; lower it to match" >&2
            fail=1
        fi
    fi
done

if [ "$fail" -ne 0 ]; then
    cat >&2 <<'MSG'

A failed read must not look like an empty one. Record what the read did
and let the page say it (apps/web/src/data/readState.ts):

    xRead = readStateOfResponse('/api/x', resp);   // ok | failed
    const body = xRead.kind === 'ok' ? await resp.json() : [];

and render `.load-failed` (role="alert") when `xRead.kind === 'failed'`,
beside or instead of the rows the read feeds — the empty value is then
never the only record of what happened. An allowance is lowered in
the same change that fixes its site; it is raised only with the reason
written beside it.
MSG
    exit 1
fi
allowlist_entries_used "$NAME" "$used" "${!ALLOW[@]}"
echo "$NAME: ok — no failed read is painted as an empty one"
