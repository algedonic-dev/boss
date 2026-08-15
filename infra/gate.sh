#!/usr/bin/env bash
# gate.sh — THE definition of the rust gate. CI's rust job invokes this
# script; anyone gating a car locally invokes this script. There is no
# second list of checks to drift from this one (CLAUDE.md §9a — on the
# 2026-08-10 train the gate's definition lived twice and drifted twice
# in one day; boss-testing/tests/gate_sh.rs pins this collapse).
#
# Usage:
#   infra/gate.sh                 # full gate — exactly what CI runs
#   infra/gate.sh -p crate [...]  # car mode — cargo phases scoped to
#                                 # the named crates (FULL suites, all
#                                 # features); lints + fmt always run
#                                 # repo-wide, they are cheap
#
# Environment setup (toolchain, dependency cache, schema apply for
# DB-backed tests) is the caller's job — CI does it in ci.yml steps,
# a dev box has it standing. The gate is the checks, nothing else.

set -u

cd "$(dirname "$0")/.."

SCOPE=()
while [ $# -gt 0 ]; do
    case "$1" in
        -p) shift; SCOPE+=(-p "${1:?-p needs a crate name}"); shift ;;
        *) echo "gate.sh: unknown arg: $1 (only -p <crate> is accepted)" >&2; exit 2 ;;
    esac
done

FAILED=()

# Each check runs even if an earlier one failed — a red gate should
# report every failure it can see, not make the author fix serially.
check() {
    local name="$1"; shift
    echo "::group::gate: ${name}"
    if "$@"; then
        echo "::endgroup::"
    else
        echo "::endgroup::"
        echo "GATE FAIL: ${name}" >&2
        FAILED+=("${name}")
    fi
}

# The shared fixture, checked in BOTH modes and named before anything
# else. Measured across the forge's CI history on 2026-08-15 (106 runs,
# 36 trains): 79% of train reds surfaced only in `test`, the slowest
# stage, and the expensive ones were not a crate's logic failing. They
# were the shared fixture failing — the schema directory, or the TestDb
# harness itself — which reds every DB-backed crate at once.
#
# Those are exactly the breaks car mode could not see. `-p <crate>`
# answers "did I break my crate"; a fixture break belongs to everyone,
# so scoping the gate to the changed crate scoped the check away and the
# first thing to notice was a train. Running it unscoped here puts a
# fixture break in front of the agent who caused it.
check "fixture" cargo test -p boss-testing --features postgres --test fixture_smoke

if [ "${#SCOPE[@]}" -eq 0 ]; then
    # Full gate — the CI shape.
    check "clippy"  cargo clippy --workspace --all-features --tests -- -D warnings
    # Default-feature build: a dangling `#[cfg(feature = ...)]` rebinds
    # onto the next item and is invisible to every --all-features step
    # (see #180). One cheap build closes the class.
    check "build (default features)" cargo build --workspace
    check "test"    cargo test --all-features
else
    check "clippy"  cargo clippy "${SCOPE[@]}" --all-features --tests -- -D warnings
    check "build (default features)" cargo build "${SCOPE[@]}"
    check "test"    cargo test "${SCOPE[@]}" --all-features
fi

check "fmt" cargo fmt -- --check

# The lint roster. These are repo-wide greps and audits — fast in car
# mode too, and a car's diff can trip any of them (both #226 red runs
# were exactly this class).
check "seed-bypass-smell"        infra/lint/seed-bypass-smell.sh
check "no-todo-citation"         infra/lint/no-todo-citation.sh
check "no-step-kind-match"       infra/lint/no-step-kind-match.sh
check "api-path-bypass-smell"    infra/lint/api-path-bypass-smell.sh
check "dispatcher-actor-stamp"   infra/lint/dispatcher-actor-stamp.sh
check "sim-boundary-audit"       infra/lint/sim-boundary-audit.sh
check "tier-import-audit"        infra/lint/tier-import-audit.sh
check "layer-order-audit"        infra/lint/layer-order-audit.sh
check "no-wallclock"             infra/lint/no-wallclock.sh
check "outbox-migration-ratchet" infra/lint/outbox-migration-ratchet.sh
check "idempotence-ratchet"      infra/lint/idempotence-ratchet.sh
check "dispatcher-rules-ratchet" infra/lint/dispatcher-rules-ratchet.sh
check "schema-converge"          infra/lint/schema-converge.sh
check "migrations-append-only"   infra/lint/migrations-append-only.sh
check "no-secrets"               infra/lint/no-secrets.sh
check "invariant-register"       infra/lint/invariant-register.sh
check "crate-counts-fresh"       infra/lint/crate-counts-fresh.sh
check "registry-bump-order"      infra/lint/registry-bump-retires-first.sh

# The frontend type gate. Last, because it is the only check that
# installs anything, and a Rust-only car should learn about its Rust
# failures before waiting on a package install.
check "svelte-check"             infra/lint/svelte-check.sh

if [ "${#FAILED[@]}" -gt 0 ]; then
    echo "" >&2
    echo "gate: ${#FAILED[@]} check(s) failed: ${FAILED[*]}" >&2
    exit 1
fi
echo "gate: all checks green"
