# Formal specs — TLA+

Auditor-grade specifications of the load-bearing BOSS state
machines. The specs document the invariants that the live code
relies on and let the [TLC](https://lamport.azurewebsites.net/tla/tla.html)
model checker exhaustively confirm them on a bounded model.

This is the TLA+ track of BOSS's formal-methods work. The other
tracks are independent and live in the Rust tree: proptest property
tests across the Rust crates and Kani bounded proofs
(`crates/modules/boss-ledger/src/kani_proofs.rs`).

## Specs

| Spec                                          | Models                                                                                  | Run cost (TLC) |
| --------------------------------------------- | --------------------------------------------------------------------------------------- | -------------- |
| [`LedgerPeriodLock.tla`](LedgerPeriodLock.tla) | Journal-entry posting + period lock/unlock. DoubleEntry, AccountingEquation, LockedPeriodImmutable. | ~5s, ~144k states |
| [`StepStatus.tla`](StepStatus.tla)            | Step lifecycle state machine. TerminalAbsorbing, NoSkippingReadyToBegin, PrereqsBeforeReady, CompletedOnlyFromInProgress, BlockedRequiresPriorInProgress. | ~1s, ~1.7k states |

## Running

```bash
./infra/tla/run-tlc.sh LedgerPeriodLock
```

The wrapper invokes `java -jar infra/tla/tla2tools.jar` with the
spec's `.cfg` and the `-deadlock` flag (our specs allow stuttering
as a no-op terminal state). Pass extra TLC flags through:

```bash
./infra/tla/run-tlc.sh LedgerPeriodLock -workers 4
```

A clean run prints `Model checking completed. No error has been
found.` plus a state-count summary. A violated invariant prints
the action trace from `Init` to the bad state — TLC's own
counterexample, ready to step through.

## Tooling

`tla2tools.jar` is downloaded into `infra/tla/` (gitignored). To
refresh:

```bash
curl -sL -o infra/tla/tla2tools.jar \
  https://github.com/tlaplus/tlaplus/releases/latest/download/tla2tools.jar
```

Java 21 (any JRE) is the only host dependency. On the dev VMs:
`sudo apt-get install -y openjdk-21-jre-headless`.

## Why TLA+ here

`boss-ledger` is the auditor-facing surface with the highest blast
radius — a balance bug or a period-lock bypass would be a
correctness incident, not a UX bug. Three properties matter:

1. **DoubleEntry**: every committed journal entry has
   `sum(debits) == sum(credits)`. Encoded today as a DB-level
   trigger + the rule pipeline that builds `JournalEntryDraft`s.
   The spec confirms there is no action sequence that produces an
   unbalanced entry under any rule combination.

2. **AccountingEquation**: `assets = liabilities + equity +
   (revenue - expense)` at every reachable state. Falls out of
   DoubleEntry plus the kind→side mapping. The spec proves the
   two together so a future change that breaks the invariant
   (e.g. a new account kind, or a posting rule that touches an
   unknown side) is caught by TLC, not by an external auditor.

3. **LockedPeriodImmutable**: once `periodStatus[p] = "locked"`,
   the count of entries in that period never increases until an
   explicit `UnlockPeriod(p)` step. Mirrors the Rust precondition
   on `crates/modules/boss-ledger/src/postgres.rs::post_fact_in_tx` that
   rejects with `LedgerError::LockedPeriod`.

The bounded TLC model isn't a proof for arbitrary inputs — it's a
proof that **no action sequence within the bounds** breaks an
invariant. That's still extremely strong: the relevant invariants
are linear-arithmetic + state-machine properties, and TLC will
find any counterexample that exists below the bound. Bumping the
bound (more accounts, more periods, more entries) is one config
edit away.

## Spec-authoring gotcha — set comprehensions deduplicate

TLA+ set comprehensions (`{ entries[i].amount : i \in S }`)
deduplicate by value: two entries each crediting `a1000` by 1
collapse to the singleton `{1}` and sum to 1, not 2. Summing
amounts that way understates totals and produces phantom invariant
violations. `LedgerPeriodLock.tla` walks the entries sequence
directly via a recursive operator instead (the inline comment marks
it); the live ledger uses multiset `SUM(...)` queries, so the
hazard is the spec's alone. Mirror the sequence-walk when adding a
spec that aggregates over per-entry values.
