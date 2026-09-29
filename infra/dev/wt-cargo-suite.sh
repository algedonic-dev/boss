# wt-cargo-suite.sh — `wt-cargo suite <crate>`: a crate's whole test
# suite (`cargo test -p <crate> --all-features --no-fail-fast`, the
# gate's own flags) run in chunks that each fit a builder's tool call,
# resumable across calls. Sourced by wt-cargo once the per-worktree
# target, its lock and the job bound are set; `runner` is its cargo argv
# (niced for a builder) and `root` its worktree.
#
# WHY (backlog ede6245a, 2026-09-28). Triage run ed6ad65a read twenty
# builder logs from that day: the boss-testing suite spent 410-782 s in
# test execution alone and 411-1128 s with its compile — median 11.2
# min, 12 of 20 past the ten-minute window one tool call gets, after
# which the harness moves the call to the background by itself. Rule 4
# of the builder rules asks for the whole suite before the push and rule
# 9 forbids background tasks, so every boss-testing car broke one of the
# two, and builders split the suite into halves by hand. The suite grew
# from 169 to 206 binaries in two days, so a split typed into the rules
# would cross the window again within days (CLAUDE.md 9a).
#
# No single call can run such a suite however it is chunked, so a call
# runs chunks until its BUDGET is spent and stops; the next call on the
# same tree resumes. The pieces:
#
#   * the targets are cargo's own list, read at run time from `--no-run
#     --message-format json` — never typed, so it cannot drift;
#   * a chunk is sized by each target's MEASURED duration (libtest's
#     `finished in`, kept per crate under the scratch root so a new
#     worktree inherits it), not by count: gate_sh.rs at ~63 s and
#     a_lint_that_scanned_nothing_is_red.rs at 30-180 s end up in chunks
#     of their own. An unmeasured target is estimated at the default;
#   * a pass is recorded against a key of the TREE (HEAD, the diff
#     against it, every untracked file), so any change starts over, and
#     only a call that finds every target passed on this tree exits 0 —
#     a call cut short can never read as green;
#   * every chunk prints ONE result line; a red chunk's whole log is
#     printed, because a verdict someone must re-derive is not a verdict.
#
# Exit: 0 every target passed on this tree; 101 a target failed (named);
# 75 the call stopped at its budget with targets left — run it again;
# 2 usage. Tunables (seconds): WT_SUITE_BUDGET_SECS (360 — a call starts
# no chunk that its estimate says would end past this, and always runs
# at least one, so a cold compile plus one chunk still fits ten
# minutes), WT_SUITE_CHUNK_SECS (60), WT_SUITE_DEFAULT_SECS (5 — the
# measured suites average 2-4 s a binary).

# `--lib`, `--test alpha`, `--doc`… for one target key.
wt_suite_selector() {
    case "$1" in
        lib) printf '%s\n' --lib ;;
        doc) printf '%s\n' --doc ;;
        *:*) printf '%s\n' "--${1%%:*}" "${1#*:}" ;;
    esac
}

# A fingerprint of the tree as the suite sees it. Built AFTER the build,
# so a Cargo.lock the build writes is part of it.
wt_suite_tree_key() {
    {
        printf '%s\n' "$1"
        git -C "$root" rev-parse HEAD 2>/dev/null || echo no-head
        git -C "$root" diff HEAD --binary 2>/dev/null || true
        (cd "$root" && git ls-files -o --exclude-standard -z | xargs -0 -r sha256sum)
    } | sha256sum | cut -c1-16
}

wt_suite() {
    if [ $# -ne 1 ] || [ -z "${1:-}" ] || [ "${1#-}" != "$1" ]; then
        echo "usage: wt-cargo suite <crate> — cargo test -p <crate> --all-features, in chunks that fit a tool call; run it again until it exits 0" >&2
        return 2
    fi
    local crate=$1
    command -v jq >/dev/null 2>&1 || { echo "wt-cargo suite: jq is needed to read cargo's target list" >&2; return 2; }
    local budget=${WT_SUITE_BUDGET_SECS:-360} cap=${WT_SUITE_CHUNK_SECS:-60} dflt=${WT_SUITE_DEFAULT_SECS:-5}
    local v
    for v in "$budget" "$cap" "$dflt"; do
        case ${v:-empty} in empty|*[!0-9]*) echo "wt-cargo suite: WT_SUITE_*_SECS must be whole seconds, got '$v'" >&2; return 2 ;; esac
    done

    local state_dir="$CARGO_TARGET_DIR/wt-cargo-suite"
    local times_dir="${WT_TARGET_ROOT:-/scratch}/wt-cargo-suite-times"
    mkdir -p "$state_dir/logs"
    local state="$state_dir/$crate.state" times="$times_dir/$crate.tsv"

    # 1. Build, and read the target list off cargo's own messages.
    #    Diagnostics render to stderr; the JSON goes to a file.
    local listing="$state_dir/$crate.listing.json" rc=0
    "${runner[@]}" test -p "$crate" --all-features --no-run \
        --message-format json-render-diagnostics > "$listing" || rc=$?
    if [ "$rc" -ne 0 ]; then
        echo "wt-cargo suite: the build failed (cargo exit $rc) — no test ran" >&2
        return "$rc"
    fi
    local -a targets=()
    local -A exe_of=()
    local key_t exe
    while IFS=$'\t' read -r key_t exe; do
        [ -n "$key_t" ] || continue
        targets+=("$key_t")
        [ "$exe" = - ] || exe_of[$key_t]=$(basename "$exe")
    done < <(jq -r '
        select(.reason == "compiler-artifact" and .profile.test == true and .executable != null)
        | .target as $t
        | (if ($t.kind | index("test")) then [2, "test:" + $t.name]
           elif ($t.kind | index("bin")) then [1, "bin:" + $t.name]
           elif ($t.kind | index("bench")) then [3, "bench:" + $t.name]
           elif ($t.kind | index("example")) then [4, "example:" + $t.name]
           else [0, "lib"] end) as $k
        | [$k[0], $k[1], .executable], (if $t.doctest then [5, "doc", "-"] else empty end)
        | @tsv' "$listing" | sort -t$'\t' -k1,1n -k2,2 -u | cut -f2-)
    local total=${#targets[@]}
    if [ "$total" -eq 0 ]; then
        echo "wt-cargo suite: $crate has no test targets — nothing to run"
        return 0
    fi

    # 2. What has already passed on THIS tree.
    local key
    key=$(wt_suite_tree_key "$crate")
    local -A result=()
    local t r
    if [ -f "$state" ] && [ "$(head -n1 "$state")" = "key $key" ]; then
        while IFS=$'\t' read -r t r; do result[$t]=$r; done < <(tail -n +2 "$state")
    else
        printf 'key %s\n' "$key" > "$state"
    fi
    local -A est=()
    if [ -f "$times" ]; then
        while IFS=$'\t' read -r t r; do
            case ${r:-empty} in empty|*[!0-9]*) ;; *) est[$t]=$r ;; esac
        done < "$times"
    fi

    # 3. Chunks, greedily in cargo's run order: a target joins the open
    #    chunk while the chunk's estimate stays under the cap; the
    #    doctests always stand alone, since cargo will not mix `--doc`.
    local -a chunks=() chunk_est=()
    local cur="" cur_est=0 e
    for t in "${targets[@]}"; do
        [ "${result[$t]:-}" = pass ] && continue
        e=${est[$t]:-$dflt}
        if [ -n "$cur" ] && { [ "$t" = doc ] || [ $((cur_est + e)) -gt "$cap" ]; }; then
            chunks+=("$cur"); chunk_est+=("$cur_est"); cur=""; cur_est=0
        fi
        cur="${cur:+$cur }$t"; cur_est=$((cur_est + e))
        if [ "$t" = doc ]; then chunks+=("$cur"); chunk_est+=("$cur_est"); cur=""; cur_est=0; fi
    done
    [ -z "$cur" ] || { chunks+=("$cur"); chunk_est+=("$cur_est"); }

    # 4. Run until the budget. SECONDS counts from wt-cargo's own start,
    #    so the seed, the floor pass and the build are already spent.
    local i n=0 log start_c wall crc args parsed base st secs
    local -a sel failed_here
    local -A seen=() took=()
    for i in "${!chunks[@]}"; do
        if [ "$n" -gt 0 ] && [ $((SECONDS + chunk_est[i])) -gt "$budget" ]; then break; fi
        n=$((n + 1))
        sel=()
        for t in ${chunks[i]}; do mapfile -t -O "${#sel[@]}" sel < <(wt_suite_selector "$t"); done
        log="$state_dir/logs/$crate-$(date -u +%Y%m%dT%H%M%SZ)-$$-$n.log"
        start_c=$SECONDS
        crc=0
        "${runner[@]}" test -p "$crate" --all-features --no-fail-fast "${sel[@]}" > "$log" 2>&1 || crc=$?
        wall=$((SECONDS - start_c))
        # libtest's own words: `Running … (<exe>)` or `Doc-tests`, then
        # `test result: ok.|FAILED. … finished in <s>s`.
        seen=(); took=()
        parsed=$(awk '
            /^[[:space:]]+Running / { n = split($0, a, "("); p = a[n]; sub(/\).*/, "", p); m = split(p, b, "/"); cur = b[m]; next }
            /^[[:space:]]+Doc-tests / { cur = "-doc-"; next }
            /^test result: / {
                s = ($3 == "ok.") ? "ok" : "FAILED"; f = ""
                if (match($0, /finished in [0-9.]+s/)) f = substr($0, RSTART + 12, RLENGTH - 13)
                if (cur != "") print cur "\t" s "\t" f
                cur = ""
            }' "$log")
        while IFS=$'\t' read -r base st secs; do
            [ -n "$base" ] || continue
            [ "$st" = ok ] && [ "${seen[$base]:-}" != FAILED ] && seen[$base]=ok
            [ "$st" = FAILED ] && seen[$base]=FAILED
            # Rounded up, and never below 1 s: a binary that tests
            # nothing still costs its start-up and cargo's fresh check.
            [ -z "$secs" ] || took[$base]=$(awk -v s="$secs" 'BEGIN { c = int(s + 0.999); printf "%d", (c < 1 ? 1 : c) }')
        done <<< "$parsed"
        failed_here=()
        for t in ${chunks[i]}; do
            if [ "$t" = doc ]; then base=-doc-; else base=${exe_of[$t]:-?}; fi
            if [ "$crc" -eq 0 ] || [ "${seen[$base]:-}" = ok ]; then r=pass; else r=fail; failed_here+=("$t"); fi
            result[$t]=$r
            printf '%s\t%s\n' "$t" "$r" >> "$state"
            if [ "$t" = doc ]; then est[$t]=$((wall < 1 ? 1 : wall)); elif [ -n "${took[$base]:-}" ]; then est[$t]=${took[$base]}; fi
        done
        if [ "${#failed_here[@]}" -eq 0 ]; then
            echo "wt-cargo suite: chunk $n ok — $(wc -w <<< "${chunks[i]}") targets in ${wall}s (est ${chunk_est[i]}s): ${chunks[i]}"
        else
            echo "----- $log (cargo exit $crc) -----"
            cat "$log"
            echo "----- end of $log -----"
            echo "wt-cargo suite: chunk $n FAILED (${failed_here[*]}) — $(wc -w <<< "${chunks[i]}") targets in ${wall}s (est ${chunk_est[i]}s): ${chunks[i]}"
        fi
    done

    # 5. Keep what this call measured for the next one, any worktree's.
    #    Written whole and renamed, so a concurrent reader never sees half.
    if [ "$n" -gt 0 ] && mkdir -p "$times_dir" 2>/dev/null; then
        local tmp
        if tmp=$(mktemp "$times.XXXXXX" 2>/dev/null); then
            for t in "${!est[@]}"; do printf '%s\t%s\n' "$t" "${est[$t]}"; done | sort > "$tmp"
            mv -f "$tmp" "$times" || rm -f "$tmp"
        fi
    fi
    if [ "$n" -gt 0 ] && [ "$(wt_suite_tree_key "$crate")" != "$key" ]; then
        echo "wt-cargo suite: WARNING — the tree changed while this call ran (an edit, or a test that writes into the tree); the next call starts the suite over" >&2
    fi

    # 6. The tally.
    local passed=0 left=0 left_est=0
    local -a failed=()
    for t in "${targets[@]}"; do
        case "${result[$t]:-}" in
            pass) passed=$((passed + 1)) ;;
            fail) failed+=("$t") ;;
            *) left=$((left + 1)); left_est=$((left_est + ${est[$t]:-$dflt})) ;;
        esac
    done
    if [ "$left" -gt 0 ]; then
        echo "wt-cargo suite: $left of $total test targets remain on this tree (about ${left_est}s) — this call stopped at its ${budget}s budget so it ends inside the ten-minute window. NOT GREEN YET: run wt-cargo suite $crate again."
    fi
    if [ "${#failed[@]}" -gt 0 ]; then
        echo "suite result: FAILED. $passed of $total test targets passed; failed: ${failed[*]}"
        return 101
    fi
    if [ "$left" -gt 0 ]; then
        return 75
    fi
    echo "suite result: ok. $total of $total test targets passed on this tree ($key)"
    return 0
}
