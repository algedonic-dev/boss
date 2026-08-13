#!/usr/bin/env bash
#
# dispatcher-rules-ratchet — the shrink-only guard on reactive rules
# (protocol-policy-publish.md, the rule census).
#
# THE PRINCIPLE
# -------------
# Under the 3P admission edge, a dispatcher rule is a reaction the
# protocol definition could not express. The census classed the 38
# rules of 2026-08-12: seven jobs-internal consequences that move
# into WorkflowSpec `on` blocks whole, ~22 domain effects that become
# admission-staged obligations, and nine external-glue reactions that
# stay. Every migration deletes its rule; nothing should quietly add
# one. So the roster is a ratchet: the count may fall, and any rise
# fails CI with this explanation in the output.
#
# THE CHECKED PROPERTY
# --------------------
# The number of `[[rule]]` entries in infra/dispatcher/rules.toml is
# <= the baseline recorded below. When a migration lands, lower the
# baseline in the same car — the same one-definition discipline the
# outbox ratchet used, and like it, this line is the entire state.
#
# A genuinely NEW reaction is still possible — timers, external
# ingress, cross-protocol reactors are legitimate residents — but it
# costs raising the baseline here, in a diff a reviewer sees, with a
# sentence in rules.toml saying why it cannot be a protocol
# consequence.
#
# Usage:  infra/lint/dispatcher-rules-ratchet.sh

set -euo pipefail

# 38 -> 40 (2026-08-13). Train #20 landed the two feedback-obligation
# reactors — `complete-feedback-branch-on-car-merged` and
# `notify-filer-on-feedback-terminal` — without raising this line,
# which nothing caught because forge CI did not yet run the gate. Both
# qualify under the cross-protocol-reactor exemption above and both
# carry their "why" in rules.toml: one advances a `user-feedback` job
# from a `ship-a-change` close, the other notifies the filer on ANY
# terminal. Neither can be declared as a consequence inside a single
# Workflow definition, because each spans two protocols by
# construction. Raised here rather than on that train because the
# violation only became visible when the gate was wired in.
BASELINE=40
RULES_FILE="infra/dispatcher/rules.toml"

count=$(grep -c '^\[\[rule\]\]' "$RULES_FILE")

if (( count > BASELINE )); then
    echo "dispatcher-rules-ratchet: $RULES_FILE has $count rules; baseline is $BASELINE" >&2
    echo "" >&2
    echo "  A new dispatcher rule is a reaction the protocol definition could" >&2
    echo "  not express (protocol-policy-publish.md). If this reaction truly" >&2
    echo "  cannot be a protocol consequence (timer / external ingress /" >&2
    echo "  cross-protocol reactor), raise BASELINE in this script in the same" >&2
    echo "  change and say why next to the rule. Otherwise: declare it in the" >&2
    echo "  Workflow definition instead." >&2
    exit 1
fi

if (( count < BASELINE )); then
    echo "dispatcher-rules-ratchet: $count rules (baseline $BASELINE) — a migration landed; lower BASELINE to $count in the same car"
    # Advisory, not fatal: the migration car that forgets to tighten
    # the ratchet gets told, loudly, without blocking the migration.
fi

echo "dispatcher-rules-ratchet: OK ($count rules <= baseline $BASELINE)"
