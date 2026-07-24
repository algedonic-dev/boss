#!/usr/bin/env bash
#
# outbox-migration-ratchet — the kind-classification guard for the
# transactional-audit-log migration (outbox phase 2).
#
# THE PRINCIPLE
# -------------
# A domain event is the rebuild source for state. If it publishes
# post-commit (`DomainPublisher::emit_*`), there is a crash window
# where the state commits and the event is lost — the swallowed-write
# class behind the 2026-07-13 replay-divergence incident. The fix is
# structural: writers record the event ON THE TRANSACTIONAL OUTBOX,
# inside the same transaction as the state
# (`boss_events::outbox::record_event_in_tx` via an `EventStamp`), and
# boss-event-relay delivers it to audit_log + NATS after commit.
#
# THE CHECKED PROPERTY
# --------------------
# Every crate is in exactly one class:
#
#   MIGRATED — records in-tx; post-commit publishing is BANNED.
#     Any `emit_*` call site here fails CI.
#
#   PENDING  — not yet migrated; its post-commit emit count is PINNED.
#     Count goes UP   → fail (new events must be born on the outbox —
#                       even in a pending crate, add new writes via
#                       record_event_in_tx, not the publisher).
#     Count goes DOWN → fail with a "ratchet down" instruction (update
#                       the baseline in the same PR, so this table
#                       stays the migration scoreboard).
#
#   Everything else — zero emit sites. A brand-new crate that starts
#     publishing post-commit fails CI immediately.
#
# When the last PENDING baseline hits zero, delete the PENDING table
# and this lint becomes a flat ban.
#
# WHAT COUNTS AS AN EMIT SITE
# ---------------------------
# A method call to any of the four DomainPublisher publish APIs
# (`.emit_at(` / `.emit_with_actor_at(` / `.emit_simulated_at(` /
# `.emit_with_actor_simulated_at(`) in a crate's src/ tree.
# `publisher.rs` (the API definition) and `tests/` are excluded.
# Comment-only mentions are excluded.
#
# Usage:  infra/lint/outbox-migration-ratchet.sh
# The CI hook lives in .github/workflows/ci.yml alongside the other lints.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

# Crates that completed the migration — post-commit publishing banned.
MIGRATED=(
  boss-commerce
  boss-products
  boss-inventory
  boss-ledger
)

# Not-yet-migrated crates and their pinned post-commit emit counts.
# Migrating a crate = move its sites to record_event_in_tx and ratchet
# its row down (to the MIGRATED list at zero). Baselines 2026-07-24.
declare -A PENDING=(
  [boss-jobs]=28
  [boss-accounts]=15
  [boss-people]=8
  [boss-content]=7
  [boss-shipping]=5
  [boss-messages]=5
  [boss-catalog]=3
  [boss-calendar]=3
  [boss-cybernetics]=1
)

EMIT_PATTERN='\.emit_at\(|\.emit_with_actor_at\(|\.emit_simulated_at\(|\.emit_with_actor_simulated_at\('

# Count emit call sites in one crate's src/ tree (0 when the crate has
# none). Excludes the publisher's own definition file, tests/, and
# comment lines.
count_sites() {
  local crate_dir="$1"
  # `|| true` swallows grep's exit-1-on-no-match under pipefail; the
  # wc output is what we want either way.
  { grep -rn --include='*.rs' -E "$EMIT_PATTERN" "$crate_dir" 2>/dev/null \
      | grep -v '/publisher\.rs:' \
      | grep -v '/tests/' \
      | grep -vE '^[^:]+:[0-9]+: *//' \
      | wc -l; } || true
}

list_sites() {
  local crate_dir="$1"
  { grep -rn --include='*.rs' -E "$EMIT_PATTERN" "$crate_dir" 2>/dev/null \
      | grep -v '/publisher\.rs:' \
      | grep -v '/tests/' \
      | grep -vE '^[^:]+:[0-9]+: *//' \
      | sed 's/^/    /'; } || true
}

fail=0

echo "outbox-migration-ratchet: post-commit publisher emits per crate"
echo

# 1. MIGRATED crates: zero tolerance.
for crate in "${MIGRATED[@]}"; do
  dir=$(find crates -maxdepth 2 -type d -name "$crate" | head -1)
  [ -z "$dir" ] && { echo "  [ERROR] migrated crate $crate not found"; fail=1; continue; }
  n=$(count_sites "$dir/src")
  if [ "$n" -ne 0 ]; then
    echo "  [FAIL] $crate is MIGRATED but has $n post-commit emit site(s):"
    list_sites "$dir/src"
    echo "         → record the event in the domain transaction instead:"
    echo "           boss_events::outbox::record_event_in_tx(&mut tx, &stamp.event(kind, payload))"
    fail=1
  else
    echo "  [ok]   $crate: 0 (migrated)"
  fi
done

# 2. PENDING crates: pinned baselines, both directions checked.
for crate in "${!PENDING[@]}"; do
  dir=$(find crates -maxdepth 2 -type d -name "$crate" | head -1)
  [ -z "$dir" ] && { echo "  [ERROR] pending crate $crate not found"; fail=1; continue; }
  n=$(count_sites "$dir/src")
  base=${PENDING[$crate]}
  if [ "$n" -gt "$base" ]; then
    echo "  [FAIL] $crate: $n emit sites (baseline $base) — new events must record"
    echo "         on the transactional outbox, not publish post-commit."
    list_sites "$dir/src"
    fail=1
  elif [ "$n" -lt "$base" ]; then
    echo "  [FAIL] $crate: $n emit sites (baseline $base) — nice, it went DOWN."
    echo "         Ratchet the baseline in infra/lint/outbox-migration-ratchet.sh"
    echo "         to $n in this PR (move the crate to MIGRATED when it hits 0)."
    fail=1
  else
    echo "  [ok]   $crate: $n (pinned)"
  fi
done

# 3. Every other crate: zero emit sites (new crates are born on the outbox).
known=" ${MIGRATED[*]} ${!PENDING[*]} "
while IFS= read -r cargo_toml; do
  dir=$(dirname "$cargo_toml")
  crate=$(basename "$dir")
  case "$known" in *" $crate "*) continue ;; esac
  [ -d "$dir/src" ] || continue
  n=$(count_sites "$dir/src")
  if [ "$n" -ne 0 ]; then
    echo "  [FAIL] $crate is not on the migration lists but has $n emit site(s)"
    echo "         — new crates record events in-tx via the outbox, they don't"
    echo "         publish post-commit. (If this is genuinely ephemeral,"
    echo "         non-state signaling, add it to PENDING with a reason.)"
    list_sites "$dir/src"
    fail=1
  fi
done < <(find crates -mindepth 3 -maxdepth 3 -name Cargo.toml)

echo
if [ "$fail" -ne 0 ]; then
  echo "outbox-migration-ratchet: FAIL"
  exit 1
fi
echo "outbox-migration-ratchet: OK"
