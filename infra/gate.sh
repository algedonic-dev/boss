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
# Car mode REFUSES a `-p` set that does not cover the crates the tree
# actually changes — see "`-p` states a belief" below.
#
# Environment setup (toolchain, dependency cache, schema apply for
# DB-backed tests) is the caller's job — CI does it in ci.yml steps,
# a dev box has it standing. The gate is the checks, nothing else.

set -u

cd "$(dirname "$0")/.."

SCOPE=()
NAMED=()
while [ $# -gt 0 ]; do
    case "$1" in
        -p) shift; SCOPE+=(-p "${1:?-p needs a crate name}"); NAMED+=("$1"); shift ;;
        *) echo "gate.sh: unknown arg: $1 (only -p <crate> is accepted)" >&2; exit 2 ;;
    esac
done

# ---------------------------------------------------------------------
# `-p` states a belief; the tree states a fact
# ---------------------------------------------------------------------
# Car mode asks the author which crates they changed, and on 2026-08-16
# the answer was wrong in the way that matters. A docs branch was gated
# `-p boss-docs` while `git add -A` had also swept an uncommitted
# crates/core/boss-jobs change into the commit — so the gate compiled
# the crate the author believed they touched, missed two independent
# defects in the one they had, and a three-car train went red on
# clippy (a6ffcb7c).
#
# The fix is not a new flag to remember. A flag you have to remember is
# the folklore this repo keeps paying for; the check has to fire
# exactly when `-p` is used, which is when the belief is being stated.
# So: derive the crate set from the tree and refuse a `-p` that does
# not cover it.
#
# Derivation is deliberately dumb — a path under crates/<tier>/<name>/
# means <name>. Two extras earn their place:
#
#   docs/design/  -> boss-docs. Those markdown files are INPUT to the
#       corpus gate (boss-docs/tests/docs_corpus_presents.rs parses
#       every one), so a docs-only change really can fail a crate's
#       tests. Path-to-crate is not the same question as which SOURCE
#       files a crate compiles.
#
#   Anything else (infra/, apps/, .forgejo/) maps to no crate and is
#       REPORTED rather than ignored. The lints already run repo-wide,
#       so there is nothing to scope — but the author should see the
#       list, because a file they did not expect is the whole warning.
changed_paths() {
    # Staged first: that is what a commit will actually carry. Fall
    # back to the full working tree so this works before `git add`,
    # which is when an author is most likely to run the gate.
    local staged
    staged=$(git diff --cached --name-only 2>/dev/null)
    if [ -n "$staged" ]; then
        printf '%s\n' "$staged"
    else
        { git diff --name-only 2>/dev/null
          git ls-files --others --exclude-standard 2>/dev/null; }
    fi
}

crates_from_paths() {
    changed_paths | path_map | tr ' ' '\n' | sed '/^$/d'
}

# Following invariant-register.sh and no-secrets.sh: a check that
# cannot demonstrate itself is a check nobody can trust. This one is
# pure string work, so it runs every time car mode does — a rule that
# only self-tests when asked is a rule that stops working quietly.
#
# The fixtures are path lists rather than real trees on purpose. The
# rule under test is paths -> crates; staging files would test git.
path_map() {
    sed -n -e 's|^crates/[^/]*/\([^/]*\)/.*|\1|p' \
           -e 's|^docs/design/.*|boss-docs|p' | sort -u | tr '\n' ' '
}

scope_self_test() {
    local fails=0 label want got
    _case() {
        label="$1"; want="$2"; shift 2
        got=$(printf '%s\n' "$@" | path_map); got="${got% }"
        if [ "$got" != "$want" ]; then
            echo "gate.sh scope self-test FAIL: ${label} -> [${got}], wanted [${want}]" >&2
            fails=1
        fi
    }
    # The commit this rule was written for: a docs title over a
    # boss-jobs change (a6ffcb7c).
    _case "the commit that earned this rule" "boss-docs boss-jobs" \
        "docs/design/queue-visibility.md" \
        "crates/core/boss-jobs/src/registry.rs" \
        "crates/core/boss-jobs/tests/platform_bundle.rs"
    # Design docs are INPUT to boss-docs' corpus gate, so a docs-only
    # car really does have a crate to compile.
    _case "a genuinely docs-only car" "boss-docs" "docs/design/payload-encryption.md"
    _case "two files, one crate" "boss-cli" \
        "crates/orchestrators/boss-cli/src/train.rs" \
        "crates/orchestrators/boss-cli/src/docs.rs"
    # The tier segment must not be mistaken for the crate name.
    _case "tier is not the crate" "boss-people" "crates/modules/boss-people/src/http.rs"
    _case "a crate's root files count" "boss-jobs" "crates/core/boss-jobs/Cargo.toml"
    # Everything outside those two trees implies nothing to scope —
    # the lints already run repo-wide.
    _case "infra implies no crate" "" "infra/gate.sh" ".forgejo/workflows/ci.yml"
    _case "docs outside design/ imply no crate" "" "docs/invariants/x.toml" "README.md"
    _case "the web app implies no crate" "" "apps/web/src/me/MePage.svelte"
    if [ "$fails" -ne 0 ]; then
        echo "gate.sh: the scope check cannot be trusted — fix it before relying on -p" >&2
        exit 2
    fi
}

if [ ${#NAMED[@]} -gt 0 ]; then
    scope_self_test
    IMPLIED=$(crates_from_paths)
    if [ -n "$IMPLIED" ]; then
        echo "gate: tree implies $(echo "$IMPLIED" | tr '\n' ' ')"
        MISSING=""
        for c in $IMPLIED; do
            covered=0
            for n in "${NAMED[@]}"; do [ "$n" = "$c" ] && covered=1; done
            [ "$covered" -eq 0 ] && MISSING="${MISSING} ${c}"
        done
        if [ -n "$MISSING" ]; then
            echo "" >&2
            echo "GATE REFUSED: -p names [${NAMED[*]}] but the tree also changes:${MISSING}" >&2
            echo "" >&2
            echo "Those crates would not be compiled or tested by this run. Either add" >&2
            echo "them (-p ${MISSING# }) or run the full gate. If a change is there by" >&2
            echo "accident — \`git add -A\` sweeping an unrelated edit into a car is how" >&2
            echo "this rule was earned — this is the moment to notice." >&2
            exit 2
        fi
    fi
fi

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
