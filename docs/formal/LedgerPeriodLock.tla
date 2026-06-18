--------------------------- MODULE LedgerPeriodLock ---------------------------
(*
 * TLA+ specification of the Boss ledger's load-bearing invariants.
 *
 * Models the journal-entry posting + period-lock state machine and
 * proves three properties that the live `boss-ledger` crate relies on:
 *
 *   1. DoubleEntry — every committed entry has sum(debits) = sum(credits).
 *      Enforced today by the `gl_journal_lines` row constraints + the
 *      `evaluate` rule pipeline that builds `JournalEntryDraft`s. The
 *      spec confirms there is no action sequence that produces an
 *      unbalanced entry.
 *
 *   2. AccountingEquation — at every reachable state, total asset
 *      balance equals total liability + equity balance. Falls out of
 *      DoubleEntry plus the kind→side rule (assets normal-debit;
 *      liabilities + equity normal-credit). The spec proves the two
 *      together so a future change that breaks the invariant (e.g. a
 *      new account kind, or a rule that posts to an unknown side) is
 *      caught by TLC, not by an auditor.
 *
 *   3. LockedPeriodImmutable — once `periodStatus[p] = "locked"`,
 *      the count of entries in that period never increases until an
 *      explicit `UnlockPeriod(p)` step. Mirrors the precondition on
 *      `crates/boss-ledger/src/postgres.rs::post_fact_in_tx` (rejects
 *      with `LedgerError::PeriodLocked`).
 *
 * Scope deliberately narrow: a single rule-version, no closing
 * entries, no manual journal entries, no FX. The state machine is
 * the same one the live ledger exposes via `POST /api/ledger/...`.
 * Bounded constants in `LedgerPeriodLock.cfg` keep TLC's exhaustive
 * search tractable (under one second on a small machine).
 *
 * Run with:
 *   java -jar infra/tla/tla2tools.jar -config docs/formal/LedgerPeriodLock.cfg \
 *        docs/formal/LedgerPeriodLock.tla
 *
 * Or via the wrapper: ./infra/tla/run-tlc.sh LedgerPeriodLock
 *)

EXTENDS Naturals, Sequences, FiniteSets, TLC

CONSTANTS
    AssetAccounts,        \* model values for asset accounts
    LiabilityAccounts,    \* ditto liability
    EquityAccounts,       \* equity
    RevenueAccounts,      \* revenue
    ExpenseAccounts,      \* expense
    Periods,              \* period ids, e.g. {p1, p2}
    MaxAmount,            \* per-line cap on debit/credit (TLC bound); e.g. 2
    MaxEntries            \* total entries cap (TLC bound); e.g. 3

\* Derived: full account set = union of the kind partitions.
Accounts ==
    AssetAccounts \cup LiabilityAccounts \cup EquityAccounts
        \cup RevenueAccounts \cup ExpenseAccounts

\* Kind lookup as a definition (instead of a function constant) so
\* the TLC config file doesn't need to encode a function literal —
\* `[a1000 |-> "asset", ...]` is illegal in .cfg.
AccountKind(a) ==
    IF a \in AssetAccounts THEN "asset"
    ELSE IF a \in LiabilityAccounts THEN "liability"
    ELSE IF a \in EquityAccounts THEN "equity"
    ELSE IF a \in RevenueAccounts THEN "revenue"
    ELSE "expense"

VARIABLES
    periodStatus,   \* [Periods -> {"open", "locked"}]
    entries         \* sequence of [period: Periods, debit_account: Accounts,
                    \*              credit_account: Accounts, amount: 0..MaxAmount]
                    \*
                    \* Each entry is a single two-line journal entry: one debit
                    \* line + one credit line of the same amount. This keeps the
                    \* state space small while still exercising the
                    \* AccountingEquation invariant — a 5-line entry doesn't add
                    \* new failure modes that the 2-line case doesn't already
                    \* expose, since balance is line-summing arithmetic.

vars == << periodStatus, entries >>

(*****************************************************************************
 * Type invariant
 *)

Amounts == 0..MaxAmount

EntryShape == [
    period         : Periods,
    debit_account  : Accounts,
    credit_account : Accounts,
    amount         : Amounts
]

TypeOK ==
    /\ periodStatus \in [Periods -> {"open", "locked"}]
    /\ entries \in Seq(EntryShape)
    /\ Len(entries) <= MaxEntries

(*****************************************************************************
 * Initial state — every period open, no entries posted.
 *)
Init ==
    /\ periodStatus = [p \in Periods |-> "open"]
    /\ entries      = << >>

(*****************************************************************************
 * Helpers — sum-over-sequence and set-to-sequence. Defined first so
 * the forward references in DebitTotal/CreditTotal resolve cleanly.
 *
 * Note: TLA+ tolerates forward refs in many positions, but TLC's
 * semantic-analysis pass can be picky about RECURSIVE declarations
 * appearing after their use sites — keeping the helper block at the
 * top sidesteps that.
 *)

\* Convert a set to a sequence in some deterministic order. TLC is
\* fine with this for finite sets; the ordering doesn't affect the
\* sum.
RECURSIVE SetToSeq(_)
SetToSeq(set) ==
    IF set = {} THEN << >>
    ELSE LET x == CHOOSE y \in set : TRUE
         IN  << x >> \o SetToSeq(set \ {x})

\* Sum a sequence of naturals.
RECURSIVE SumSeq(_)
SumSeq(s) ==
    IF s = << >> THEN 0
    ELSE Head(s) + SumSeq(Tail(s))

\* Sum a finite set of naturals — sequence-fold via SetToSeq.
Sum(set) == SumSeq(SetToSeq(set))

(*****************************************************************************
 * Per-account, per-side balances — sum across all entries.
 *
 * Each entry contributes `amount` to its debit_account's debit total
 * and the same `amount` to its credit_account's credit total.
 *
 * Implemented via a recursive walk over the entries sequence rather
 * than a set comprehension. TLA+ set comprehensions deduplicate by
 * value — `{ entries[i].amount : i \in S }` collapses two entries
 * each carrying `amount=1` to the singleton `{1}`, which sums to 1
 * not 2. (TLC found exactly this bug on first run; keeping the
 * note as a warning to future spec hackers.)
 *)

RECURSIVE SumDebitsForAccount(_, _)
SumDebitsForAccount(acct, es) ==
    IF es = << >> THEN 0
    ELSE LET h == Head(es) IN
         (IF h.debit_account = acct THEN h.amount ELSE 0)
         + SumDebitsForAccount(acct, Tail(es))

RECURSIVE SumCreditsForAccount(_, _)
SumCreditsForAccount(acct, es) ==
    IF es = << >> THEN 0
    ELSE LET h == Head(es) IN
         (IF h.credit_account = acct THEN h.amount ELSE 0)
         + SumCreditsForAccount(acct, Tail(es))

DebitTotal(acct)  == SumDebitsForAccount(acct, entries)
CreditTotal(acct) == SumCreditsForAccount(acct, entries)

\* Account-kind rollups — sum across all accounts of a given kind.
\* Walk the per-account totals via SetToSeq → SumSeq mapping rather
\* than a set comprehension, for the same dedup-avoidance reason as
\* the per-account sums above.

RECURSIVE SumDebitsOver(_)
SumDebitsOver(s) ==
    IF s = << >> THEN 0
    ELSE DebitTotal(Head(s)) + SumDebitsOver(Tail(s))

RECURSIVE SumCreditsOver(_)
SumCreditsOver(s) ==
    IF s = << >> THEN 0
    ELSE CreditTotal(Head(s)) + SumCreditsOver(Tail(s))

TotalDebitsForKind(k) ==
    SumDebitsOver(SetToSeq({ a \in Accounts : AccountKind(a) = k }))

TotalCreditsForKind(k) ==
    SumCreditsOver(SetToSeq({ a \in Accounts : AccountKind(a) = k }))

\* Net balance per side. Assets are normal-debit (debit-credit);
\* liabilities + equity are normal-credit (credit-debit).
AssetBalance     == TotalDebitsForKind("asset")     - TotalCreditsForKind("asset")
LiabilityBalance == TotalCreditsForKind("liability") - TotalDebitsForKind("liability")
EquityBalance    == TotalCreditsForKind("equity")    - TotalDebitsForKind("equity")
RevenueBalance   == TotalCreditsForKind("revenue")   - TotalDebitsForKind("revenue")
ExpenseBalance   == TotalDebitsForKind("expense")    - TotalCreditsForKind("expense")

(*****************************************************************************
 * Actions
 *)

\* PostEntry — submit a balanced two-line entry to an open period.
\* Mirrors `post_fact_in_tx` in boss-ledger/src/postgres.rs: balance
\* check + period-lock check happen in the same transaction.
PostEntry(p, da, ca, amt) ==
    /\ p \in Periods
    /\ periodStatus[p] = "open"        \* period-lock precondition
    /\ da \in Accounts /\ ca \in Accounts
    /\ da # ca                          \* a real entry touches two accounts
    /\ amt \in 1..MaxAmount             \* zero-amount entries are spec noise
    /\ Len(entries) < MaxEntries
    /\ entries' = Append(entries, [
            period         |-> p,
            debit_account  |-> da,
            credit_account |-> ca,
            amount         |-> amt
       ])
    /\ UNCHANGED periodStatus

\* LockPeriod — flip an open period to locked. After this point no
\* further entries can land until UnlockPeriod fires.
LockPeriod(p) ==
    /\ p \in Periods
    /\ periodStatus[p] = "open"
    /\ periodStatus' = [periodStatus EXCEPT ![p] = "locked"]
    /\ UNCHANGED entries

\* UnlockPeriod — operator-tier escape hatch. The live API gates
\* this on operator role; the spec doesn't model auth.
UnlockPeriod(p) ==
    /\ p \in Periods
    /\ periodStatus[p] = "locked"
    /\ periodStatus' = [periodStatus EXCEPT ![p] = "open"]
    /\ UNCHANGED entries

Next ==
    \/ \E p \in Periods, da \in Accounts, ca \in Accounts, amt \in 1..MaxAmount :
            PostEntry(p, da, ca, amt)
    \/ \E p \in Periods : LockPeriod(p)
    \/ \E p \in Periods : UnlockPeriod(p)

Spec == Init /\ [][Next]_vars

(*****************************************************************************
 * Invariants
 *)

\* INV1: DoubleEntry on every entry. With our two-line entry shape
\* this reduces to "amount >= 0" (the debit and credit lines each
\* carry the same `amount`), but TLC still checks it on every state
\* and would catch a future schema change (e.g. independent
\* debit/credit fields) that breaks the symmetry.
EveryEntryBalanced ==
    \A i \in 1..Len(entries) : entries[i].amount >= 0

\* INV2: AccountingEquation. The fundamental ledger identity:
\* assets = liabilities + equity + (revenue - expense). The (rev-exp)
\* term is the period's net income — in real life it rolls up to
\* equity at year-end via closing entries; before close, the BS
\* already balances if we include it on the equity side, which is
\* what the live `balance_sheet` handler does (synthesises a
\* "Current-period net income" equity row).
AccountingEquation ==
    AssetBalance = LiabilityBalance + EquityBalance + RevenueBalance - ExpenseBalance

\* INV3: PeriodStatusOK — sanity check on the type-level enum.
PeriodStatusOK ==
    \A p \in Periods : periodStatus[p] \in {"open", "locked"}

(*****************************************************************************
 * Temporal properties — locked periods are immutable.
 *
 * For every period p, every step preserves the count of entries
 * targeting p UNLESS the step transitioned p from locked → open
 * (UnlockPeriod) or kept it open (PostEntry / LockPeriod from open).
 * The cleanest formulation: from any state where p is locked, the
 * NEXT state's entry-count for p is the same as this state's, OR p
 * was unlocked in the transition.
 *)

EntriesIn(p, es) == { i \in 1..Len(es) : es[i].period = p }
EntryCountIn(p)  == Cardinality(EntriesIn(p, entries))

LockedPeriodImmutable ==
    [][\A p \in Periods :
            (periodStatus[p] = "locked")
            => (Cardinality(EntriesIn(p, entries')) = Cardinality(EntriesIn(p, entries))
                \/ periodStatus'[p] = "open")
      ]_vars

================================================================================
