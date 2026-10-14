#!/usr/bin/env bash
# cargo-test-wide.sh — the gate's `cargo test`, in two halves, the
# second run several test binaries at a time.
#
#   infra/cargo-test-wide.sh build <state-dir> cargo test <args…>
#   infra/cargo-test-wide.sh run   <state-dir> cargo test <args…>
#
# `build` is `<cargo test …> --no-run`: it compiles every test target and
# keeps cargo's own JSON list of them in <state-dir>. `run` executes that
# list — each binary in its own process, exactly as cargo would start it
# (cwd = its package root, cargo's runtime environment) — WIDTH at a
# time, then the doctests through cargo itself. Both halves take the
# same argv so the doctest pass selects exactly what the build did.
#
# WHY (backlog 2d234222, measured 2026-10-09 by run c4a8df09 over every
# gate-run). The gate's `test` check had become the delivery bottleneck:
# a median 2041 s at the fixed scope boss-testing, on a median 2.25 of
# the 20 cores the gate container may use, IO wait 9%. `cargo test` runs
# its test binaries ONE AT A TIME, each 2 threads wide (RUST_TEST_THREADS,
# infra/gate-runner/run.sh), and boss-testing grew from 122 files / 905
# tests (09-21) to 305 files / 3063 tests — so the check was bounded by
# serial execution of a tripled suite, not by the disk and not by CPU.
# And nobody could say how much of the 2041 s was compiling: one check
# timed both. Splitting it into two checks puts each half on the receipt
# (`test build` and `test`), and `test_binaries` on the receipt carries
# the run's width, its wall time and the sum of every binary's own time
# — the serial cost — so the gain is read off one receipt, not inferred.
#
# WHAT IS KEPT, ON PURPOSE:
#   * RUST_TEST_THREADS is untouched. The width multiplies PROCESSES; a
#     binary's tests still run as they did, so a log-capturing test that
#     owns its process (a934e9a0) still owns it, and the suites that
#     share the Postgres sidecar each still open what they opened.
#   * Every binary runs, failed or not (`--no-fail-fast`, backlog
#     3bef4198), and its whole output is printed in one block with the
#     `error: test failed, to rerun pass …` line cargo prints, so the
#     runner's failure-detail parser (infra/gate-runner/run.sh) reads it
#     exactly as before.
#   * A list that names nothing, or a build that did not finish, is a
#     FAILED run, never an empty pass — no evidence is not a pass.
#
# THE WIDTH. BOSS_TEST_BINARIES_WIDE when set; otherwise a quarter of
# CARGO_BUILD_JOBS (which infra/gate-runner/run.sh sets from the
# container's CGROUP CPU quota — the one measured CPU fact, not read
# twice), between 1 and 4; and 1 — cargo's own serial order — wherever
# CARGO_BUILD_JOBS is unset (the public mirror's runner, a hand run).
# Four, at most: four binaries x RUST_TEST_THREADS=2 is 8 test threads,
# a quarter of w-1's 32 cores beside three other gates, inside the gate
# container's 20-CPU limit; and the Postgres sidecar keeps its default
# max_connections of 100 (gate-runner.yaml), where one DB-backed binary
# holds two tests' pools (5 each) plus its reaper's 8 drops at worst.
#
# Exit: 0 every binary and the doctests passed; 101 any failed (cargo's
# own code); 2 usage or a missing listing.
set -uo pipefail

usage() {
    echo "usage: $0 build|run <state-dir> cargo test <args…>" >&2
    exit 2
}
[ $# -ge 4 ] || usage
mode=$1 state=$2
shift 2
case "$mode" in build|run) ;; *) usage ;; esac
for a in "$@"; do
    # A `--` hands what follows to libtest; `--no-run` would land there.
    [ "$a" != -- ] || { echo "$0: harness arguments (after --) are not supported" >&2; exit 2; }
done
listing="$state/listing.json"

if [ "$mode" = build ]; then
    mkdir -p "$state" || exit 2
    rm -f "$listing" "$state/summary.json"
    # Diagnostics are rendered to stderr as usual; only the JSON is kept.
    "$@" --no-run --message-format json-render-diagnostics > "$listing.part"
    rc=$?
    [ "$rc" -eq 0 ] && mv "$listing.part" "$listing"
    exit "$rc"
fi

# ---- run ------------------------------------------------------------
command -v jq >/dev/null 2>&1 || { echo "$0: jq is needed to read cargo's target list" >&2; exit 2; }
if [ ! -s "$listing" ]; then
    echo "$0: no target list at $listing — the test build did not finish, so no test ran" >&2
    exit 2
fi

width=${BOSS_TEST_BINARIES_WIDE:-}
if [ -z "$width" ]; then
    case ${CARGO_BUILD_JOBS:-empty} in
        empty|*[!0-9]*) width=1 ;;
        *) width=$((CARGO_BUILD_JOBS / 4)) ;;
    esac
    [ "$width" -le 4 ] || width=4
fi
case ${width:-empty} in empty|*[!0-9]*) echo "$0: BOSS_TEST_BINARIES_WIDE must be a whole number, got '$width'" >&2; exit 2 ;; esac
[ "$width" -ge 1 ] || width=1

# One row per test binary: kind, target, executable, manifest, package,
# version, source. The package's name and version are read off cargo's
# package id, in both of its spellings (`name ver (source)` before cargo
# 1.77, `source#name@ver` or `source#ver` after).
rows=$(jq -r '
    select(.reason == "compiler-artifact" and .profile.test == true and .executable != null)
    | .package_id as $id
    | (if ($id | test("^[^ ]+ [^ ]+ \\(")) then ($id | split(" ") | [.[0], .[1]])
       else ($id | split("#")) as $p
         | if ($p[1] | contains("@")) then ($p[1] | split("@"))
           else [($p[0] | split("/") | last), $p[1]] end
       end) as $pkg
    | [.target.kind[0], .target.name, .executable, .manifest_path, $pkg[0], $pkg[1], .target.src_path]
    | @tsv' "$listing" | sort -t$'\t' -k3,3 -u | sort -t$'\t' -k1,1 -k5,5 -k2,2)
# Ordered by kind first — `bin` and `lib` sort ahead of `test` — so each
# crate's unit-test harness, which usually holds most of its tests and
# is its longest binary (boss-cli's: 2142 tests, 126 s on 2026-10-09),
# starts early instead of last, where it would be the run's tail.
# Doctests run when a library under test declares them; cargo refuses
# `--doc` over a selection with no library at all.
has_doc=$(jq -n -r 'first(inputs | select(.reason == "compiler-artifact" and .profile.test == true and .target.doctest == true) | .target.name) // empty' "$listing")

kinds=() names=() exes=() dirs=() pkgs=() vers=() srcs=()
while IFS=$'\t' read -r k t e m p v s; do
    [ -n "$e" ] || continue
    kinds+=("$k") names+=("$t") exes+=("$e") dirs+=("$(dirname "$m")") pkgs+=("$p") vers+=("$v") srcs+=("$s")
done <<< "$rows"
n=${#exes[@]}
if [ "$n" -eq 0 ] && [ -z "$has_doc" ]; then
    echo "$0: cargo's list at $listing names no test binary and no doctest — a run that ran nothing is not a pass" >&2
    exit 2
fi

logs="$state/logs"
rm -rf "$logs"
mkdir -p "$logs" || exit 2
real_cargo=$(command -v cargo || echo cargo)

now_ms() { local t; t=$(date +%s%N); echo $((t / 1000000)); }

# Job 0 is the doctest pass when there is one: it compiles, so it is the
# longest single job and starts first. Jobs 1..n are the binaries.
job() { # <job> <cargo test argv…>
    local i=$1 rc t0
    shift
    t0=$(now_ms)
    if [ "$i" -eq 0 ]; then
        "$@" --doc > "$logs/0.log" 2>&1 < /dev/null
        rc=$?
    else
        local j=$((i - 1)) ver=${vers[$((i - 1))]}
        local minor=${ver#*.}
        local patch=${minor#*.}
        # The braces' 2>/dev/null takes bash's own job notice for a child
        # killed by a signal ("Killed ( cd … exec … )"), which named the
        # whole subshell rather than the binary and landed on stderr out
        # of order; the signal now rides on the binary's rerun line
        # (report, below), and the binary's own stderr is in its log.
        {
            (
                cd "${dirs[j]}" || exit 2
                export CARGO="$real_cargo" \
                    CARGO_MANIFEST_DIR="${dirs[j]}" CARGO_MANIFEST_PATH="${dirs[j]}/Cargo.toml" \
                    CARGO_PKG_NAME="${pkgs[j]}" CARGO_PKG_VERSION="$ver" \
                    CARGO_PKG_VERSION_MAJOR="${ver%%.*}" CARGO_PKG_VERSION_MINOR="${minor%%.*}" \
                    CARGO_PKG_VERSION_PATCH="${patch%%[-+]*}" \
                    CARGO_CRATE_NAME="${names[j]//-/_}" CARGO_PRIMARY_PACKAGE=1 \
                    LD_LIBRARY_PATH="$(dirname "${exes[j]}"):$(dirname "$(dirname "${exes[j]}")")${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
                [ "${kinds[j]}" != bin ] || export CARGO_BIN_NAME="${names[j]}"
                exec "${exes[j]}"
            ) > "$logs/$i.log" 2>&1 < /dev/null
        } 2>/dev/null
        rc=$?
    fi
    printf '%s %s\n' "$rc" $(( $(now_ms) - t0 )) > "$logs/$i.status.part" && mv "$logs/$i.status.part" "$logs/$i.status" \
        || echo "$0: job $i ended with exit status $rc, but its status could not be recorded in $logs" >&2
}

# How a job ended, in cargo's own terms (`exit status: 101`, or
# `signal: 9, SIGKILL` for a status above 128), for the rerun line
# (review 465e723f F2, backlog c0259ecc): a binary the kernel OOM-killed
# or that crashed otherwise reads as a test failure with no failing test.
ended() { # <exit status>
    local rc=$1 sig
    if [ "$rc" -gt 128 ] && sig=$(kill -l "$((rc - 128))" 2>/dev/null); then
        printf '(exit status: %s, signal: %s, SIG%s)' "$rc" "$((rc - 128))" "$sig"
    else
        printf '(exit status: %s)' "$rc"
    fi
}

selector() { # <index into the binary arrays>
    case "${kinds[$1]}" in
        lib|rlib|proc-macro) printf -- '-p %s --lib' "${pkgs[$1]}" ;;
        *) printf -- '-p %s --%s %s' "${pkgs[$1]}" "${kinds[$1]}" "${names[$1]}" ;;
    esac
}

failed=() took=()
sum_ms=0
declare -A lost=()
report() { # <job> <cargo test argv…>
    local i=$1 rc ms how
    shift
    if [ -n "${lost[$i]:-}" ]; then
        rc=125 ms=0 how="(no exit status: ${lost[$i]})"
    else
        read -r rc ms < "$logs/$i.status"
        how=$(ended "$rc")
    fi
    sum_ms=$((sum_ms + ms))
    if [ "$i" -eq 0 ]; then
        echo "   Doc-tests ($* --doc)"
        cat "$logs/0.log" 2>/dev/null || echo "(its output, $logs/0.log, is gone)"
        [ "$rc" -eq 0 ] || { failed+=("doctests"); echo "error: doctests failed $how"; }
        took+=("$ms doctests")
        return
    fi
    local j=$((i - 1))
    echo "     Running ${srcs[j]#"$PWD"/} (${exes[j]})"
    cat "$logs/$i.log" 2>/dev/null || echo "(its output, $logs/$i.log, is gone)"
    if [ "$rc" -ne 0 ]; then
        failed+=("$(selector "$j")")
        echo "error: test failed, to rerun pass \`$(selector "$j")\` $how"
    fi
    took+=("$ms ${pkgs[j]}:${names[j]}")
}

first=1
[ -n "$has_doc" ] && first=0
total=$(( n + 1 - first ))
echo "cargo-test-wide: ${n} test binaries$([ "$first" -eq 0 ] && echo ' and the doctests'), ${width} at a time (RUST_TEST_THREADS=${RUST_TEST_THREADS:-unset} each)"
t_run=$(now_ms)
next=$first
running=0
reported=0
declare -A done_job=()
while [ "$reported" -lt "$total" ]; do
    while [ "$running" -lt "$width" ] && [ "$next" -le "$n" ]; do
        job "$next" "$@" &
        next=$((next + 1))
        running=$((running + 1))
    done
    wait -n 2>/dev/null || true
    # A job that left no status still ends: once no child is left, every
    # unreported one is recorded as failed, with why, rather than waited
    # on forever. HELD IN MEMORY, never written (review 465e723f F1,
    # backlog c0259ecc): this fallback used to write a 125 status into
    # the same logs dir, so when that dir was the thing that failed —
    # removed, or the disk refusing writes — the fallback failed too and
    # this loop spun, 42,937 error lines in 20 s, until a gate's deadline
    # took the verdict with it.
    if [ -z "$(jobs -rp)" ]; then
        for ((i = first; i < next; i++)); do
            [ -n "${done_job[$i]:-}" ] || [ -f "$logs/$i.status" ] || [ -n "${lost[$i]:-}" ] && continue
            if [ -d "$logs" ]; then
                lost[$i]="it left no status in $logs: killed before it could write one, or the write failed"
            else
                lost[$i]="the runner's log directory $logs is gone, so no status could be written"
            fi
        done
    fi
    for ((i = first; i < next; i++)); do
        [ -z "${done_job[$i]:-}" ] && { [ -f "$logs/$i.status" ] || [ -n "${lost[$i]:-}" ]; } || continue
        done_job[$i]=1
        running=$((running - 1))
        reported=$((reported + 1))
        report "$i" "$@"
    done
done
wall_ms=$(( $(now_ms) - t_run ))

slowest=$(printf '%s\n' "${took[@]}" | sort -rn | awk 'NR <= 5')
echo ""
echo "cargo-test-wide: ${total} job(s) ${width} at a time in $((wall_ms / 1000))s; their own times sum to $((sum_ms / 1000))s (what one at a time would cost); ${#failed[@]} failed"
echo "cargo-test-wide: slowest: $(printf '%s\n' "$slowest" | awk '{ printf "%s%s %.0fs", (NR > 1 ? ", " : ""), $2, $1 / 1000 }')"
slow_json=$(printf '%s\n' "$slowest" | jq -R -s -c 'split("\n") | map(select(length > 0) | split(" ") | {target: .[1], seconds: ((.[0] | tonumber) / 1000 | . * 10 | round / 10)})')
fail_json=$(printf '%s\n' "${failed[@]+"${failed[@]}"}" | jq -R -s -c 'split("\n") | map(select(length > 0))')
printf '{"width":%s,"rust_test_threads":"%s","binaries":%s,"doctests":%s,"wall_s":%s,"own_time_sum_s":%s,"failed":%s,"slowest":%s}\n' \
    "$width" "${RUST_TEST_THREADS:-}" "$n" "$([ "$first" -eq 0 ] && echo true || echo false)" \
    "$((wall_ms / 1000))" "$((sum_ms / 1000))" "$fail_json" "$slow_json" > "$state/summary.json"
if [ "${#failed[@]}" -gt 0 ]; then
    echo ""
    echo "failures (rerun each with cargo test <selector>):"
    printf '    %s\n' "${failed[@]}"
    exit 101
fi
exit 0
