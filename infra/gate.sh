#!/usr/bin/env bash
# gate.sh — THE definition of the rust gate. CI's rust job invokes this
# script; anyone gating a car locally invokes this script. There is no
# second list of checks to drift from this one (CLAUDE.md §9a — on the
# 2026-08-10 train the gate's definition lived twice and drifted twice
# in one day; boss-testing/tests/gate_sh.rs pins this collapse).
#
# Usage:
#   infra/gate.sh                 # full gate — exactly what CI runs
#   infra/gate.sh --auto          # car mode, scope DERIVED from the
#                                 # tree. Skips cargo entirely when
#                                 # nothing changed implies a crate —
#                                 # 74 of 164 live branches are in that
#                                 # class. Never used by CI.
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
AUTO=0
while [ $# -gt 0 ]; do
    case "$1" in
        -p) shift; SCOPE+=(-p "${1:?-p needs a crate name}"); NAMED+=("$1"); shift ;;
        --auto) AUTO=1; shift ;;
        *) echo "gate.sh: unknown arg: $1 (accepts -p <crate> and --auto)" >&2; exit 2 ;;
    esac
done
# Alternatives, not companions: --auto derives exactly what -p states,
# so accepting both would mean silently preferring one belief over the
# other — and the whole point of the refusal below is that a stated
# belief gets checked, never quietly overridden.
if [ "$AUTO" -eq 1 ] && [ ${#NAMED[@]} -gt 0 ]; then
    echo "gate.sh: --auto and -p are alternatives; --auto derives what -p would state" >&2
    exit 2
fi

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
#   The same reasoning, four more times — each one a file some crate's
#   test READS, so changing it can redden that crate without touching
#   a line of its source:
#     infra/gate.sh, infra/lint/*, .forgejo/workflows/ci.yml
#         -> boss-testing, which owns gate_sh.rs. That test pins that
#            ci.yml invokes this script, that this script runs every
#            check, and that every executable in infra/lint/ appears
#            here. Omitting these would let `--auto` skip the only
#            test guarding the file being edited — which this very car
#            would have done to itself.
#     infra/dispatcher/rules.toml -> boss-dispatcher, which owns
#            dispatcher_rules_seed.rs. It compares the seeded registry
#            against that file in BOTH directions, and skipping the
#            toml half is what reddened the 13-car train
#            20260815-0621.
#
#   Anything else (infra/, apps/, .forgejo/) maps to no crate and is
#       REPORTED rather than ignored. The lints already run repo-wide,
#       so there is nothing to scope — but the author should see the
#       list, because a file they did not expect is the whole warning.
changed_paths() {
    # THE QUESTION IS "what will this car land", and that has three
    # answers depending on where the author is in the loop. Asking only
    # the first two is a bug I shipped: `--auto` derived from the
    # WORKING TREE alone, so gating after a commit — or after a rebase,
    # which is when you most want to re-check — found a clean tree,
    # scoped to nothing, skipped every cargo phase and reported
    # "all checks green". A gate that runs nothing must never say that.
    #
    # Staged first: that is what a commit will actually carry.
    local staged
    staged=$(git diff --cached --name-only 2>/dev/null)
    if [ -n "$staged" ]; then
        printf '%s\n' "$staged"
        return
    fi
    # Then the working tree, for the common case of gating before
    # `git add`.
    local dirty
    dirty=$({ git diff --name-only 2>/dev/null
              git ls-files --others --exclude-standard 2>/dev/null; })
    if [ -n "$dirty" ]; then
        printf '%s\n' "$dirty"
        return
    fi
    # Finally the commits this branch adds over the trunk. A clean tree
    # on a branch with commits is not "no change" — it is a car that is
    # ready, which is exactly when it gets gated.
    local base
    base=$(git merge-base "$AUTO_TRUNK" HEAD 2>/dev/null) || return 0
    [ -n "$base" ] || return 0
    git diff --name-only "$base" HEAD 2>/dev/null
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
           -e 's|^docs/design/.*|boss-docs|p' \
           -e 's|^infra/gate\.sh$|boss-testing|p' \
           -e 's|^infra/lint/.*|boss-testing|p' \
           -e 's|^\.forgejo/workflows/ci\.yml$|boss-testing|p' \
           -e 's|^infra/dispatcher/rules\.toml$|boss-dispatcher|p' \
           -e 's|^infra/platform/workflows\.toml$|boss-jobs|p' \
           | sort -u | tr '\n' ' '
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
    # The platform bundle is DATA, but boss-jobs compiles a test that
    # parses and lints it (`the_platform_bundle_matches_the_specs_it
    # _replaced`). Without this line a protocol-only car scoped to
    # "lints + fmt only" and never ran the one test that can reject it
    # — which is how correct-the-record's second defect nearly shipped:
    # the bundle lint caught a free-text fork with no fallback, and the
    # gate would not have run that lint at all.
    _case "a protocol-only car still has a crate" "boss-jobs" \
        "infra/platform/workflows.toml"
    _case "two files, one crate" "boss-cli" \
        "crates/orchestrators/boss-cli/src/train.rs" \
        "crates/orchestrators/boss-cli/src/docs.rs"
    # The tier segment must not be mistaken for the crate name.
    _case "tier is not the crate" "boss-people" "crates/modules/boss-people/src/http.rs"
    _case "a crate's root files count" "boss-jobs" "crates/core/boss-jobs/Cargo.toml"
    # Everything outside those two trees implies nothing to scope —
    # the lints already run repo-wide.
    # gate.sh and ci.yml are READ by boss-testing's gate_sh.rs, so a
    # change to either must compile and run that crate.
    _case "the gate's own files imply boss-testing" "boss-testing" \
        "infra/gate.sh" ".forgejo/workflows/ci.yml" "infra/lint/no-secrets.sh"
    _case "the dispatcher rule file implies boss-dispatcher" "boss-dispatcher" \
        "infra/dispatcher/rules.toml"
    _case "other infra implies no crate" "" \
        "infra/forge/locomotive.sh" "infra/deploy-services.sh"
    _case "docs outside design/ imply no crate" "" "docs/invariants/x.toml" "README.md"
    _case "the web app implies no crate" "" "apps/web/src/me/MePage.svelte"
    # Schema files imply no CRATE, which is why --auto asks
    # `schema_touched` separately rather than reading it off this map.
    # Get this wrong in the other direction — map schema to some crate
    # — and every migration would compile a crate for no reason.
    _case "a migration implies no crate" "" "infra/postgres/schema/141-x.sql"
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

# ---------------------------------------------------------------------
# `--auto`: derive the scope instead of stating it
# ---------------------------------------------------------------------
# The refusal above is the SAFETY half of scoping — it stops a `-p`
# that misses a crate. This is the efficiency half, and it is worth
# having on a measured basis: of 164 live branches, 74 touch no Rust
# at all, and two of the fourteen cars shipped on 2026-08-16 were in
# that class. For those, everything cargo does is dead weight — the
# lint roster and fmt are the entire useful gate, thirty seconds
# against eight to fifteen minutes.
#
# A FLAG, not the default, because bare `infra/gate.sh` is what CI
# invokes and must keep meaning "the whole workspace,
# unconditionally". A gate that quietly narrowed itself in CI would be
# the same hole as the mis-scoped `-p` that reddened a three-car train
# (a6ffcb7c), pointed the other way.
#
# THE FIXTURE IS THE SUBTLE PART. `infra/postgres/schema/**` maps to no
# crate, but the shared fixture LOADS the schema — so a schema-only
# change has no crate to compile and can still break every DB-backed
# test in the workspace. Skipping cargo entirely there would scope away
# the exact break the fixture check exists to catch, which is what the
# comment above `check "fixture"` warns about. So the derivation
# answers two questions: which crates, and whether the fixture is
# implicated.
schema_touched() {
    if changed_paths | grep -qE '^infra/postgres/schema/'; then echo yes; else echo no; fi
}

# Which ref is "the trunk" for deriving a branch's own commits. The
# remote-tracking main this repo actually uses, with the local branch
# and an override as fallbacks — a box whose remote is named
# differently must not silently fall through to gating nothing.
AUTO_TRUNK="${BOSS_GATE_TRUNK:-}"
if [ -z "$AUTO_TRUNK" ]; then
    for candidate in gcp/forge-main origin/main main; do
        if git rev-parse --verify --quiet "$candidate" >/dev/null 2>&1; then
            AUTO_TRUNK="$candidate"
            break
        fi
    done
fi

AUTO_LINTS_ONLY=0
AUTO_SKIP_FIXTURE=0
if [ "$AUTO" -eq 1 ]; then
    scope_self_test
    DERIVED=$(crates_from_paths)
    if [ -n "$DERIVED" ]; then
        for c in $DERIVED; do SCOPE+=(-p "$c"); NAMED+=("$c"); done
        echo "gate: --auto scoping to $(echo "$DERIVED" | tr '\n' ' ')"
    elif [ "$(schema_touched)" = "yes" ]; then
        # No crate, but the schema moved: the fixture is the one check
        # that can see that, so it runs and nothing else cargo-shaped.
        AUTO_LINTS_ONLY=1
        echo "gate: --auto — no crate changed, but infra/postgres/schema/ did; fixture + lints only"
    else
        AUTO_LINTS_ONLY=1
        AUTO_SKIP_FIXTURE=1
        local_changed=$(changed_paths | tr '\n' ' ')
        if [ -z "${local_changed// /}" ]; then
            # Nothing staged, nothing dirty, and nothing this branch
            # adds over the trunk. Refuse rather than report green:
            # "the gate passed" and "the gate had nothing to check"
            # must not look the same, and they did.
            echo "GATE REFUSED: --auto found no change at all against ${AUTO_TRUNK:-<no trunk>}." >&2
            echo "" >&2
            echo "Nothing is staged, the tree is clean, and this branch adds no commit" >&2
            echo "over the trunk — so there is nothing to scope and nothing to check." >&2
            echo "If that is wrong, the trunk ref is: ${AUTO_TRUNK:-<none found>}." >&2
            echo "Set BOSS_GATE_TRUNK to the right one, or run the full gate." >&2
            exit 2
        fi
        echo "gate: --auto — nothing changed implies a crate; lints + fmt only"
        echo "gate: (changed: ${local_changed})"
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
if [ "$AUTO_SKIP_FIXTURE" -eq 1 ]; then
    echo "gate: skipping fixture — no crate and no schema change to break it"
else
    check "fixture" cargo test -p boss-testing --features postgres --test fixture_smoke
fi

if [ "$AUTO_LINTS_ONLY" -eq 1 ]; then
    echo "gate: skipping clippy / build / test — nothing changed implies a crate"
elif [ "${#SCOPE[@]}" -eq 0 ]; then
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
check "ci-tools-declared"        infra/lint/ci-tools-declared.sh
check "timers-leave-a-packet"    infra/lint/timers-leave-a-packet.sh

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
