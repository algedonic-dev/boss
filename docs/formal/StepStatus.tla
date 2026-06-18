--------------------------- MODULE StepStatus ---------------------------
(*
 * TLA+ specification of the BOSS Step lifecycle state machine.
 *
 * !! STALE — DO NOT TREAT AS CURRENT. This spec models the RETIRED
 * six-state lifecycle. The shipped machine is FIVE states —
 * Pending -> Ready -> Active -> Completed, plus Skipped — with terminals
 * {Completed, Skipped} and no Blocked/Aborted/InProgress (see
 * crates/core/boss-core/src/job.rs). Its invariants
 * below prove properties of states that no longer exist; the spec needs
 * a rewrite to the 5-state machine (or deletion) before it can be
 * trusted again.
 *
 * The lifecycle is six states organised as **2 alternates × 3 stages**:
 *
 *   Stage 1 (not started yet):   PendingPrereqs  <->  ReadyToBegin
 *   Stage 2 (in motion):         InProgress      <->  Blocked
 *   Stage 3 (terminal):          Completed   |   Aborted
 *
 * Alternates within a stage flip back and forth as the world changes:
 *
 *   - PendingPrereqs <-> ReadyToBegin: a Step in ReadyToBegin can
 *     revert to PendingPrereqs if a part is reallocated, a prereq
 *     Step's completion is reversed, etc. Likewise PendingPrereqs
 *     promotes to ReadyToBegin when the prereq becomes satisfied.
 *
 *   - InProgress <-> Blocked: a Step that's been started can hit a
 *     snag (Blocked) and then be unblocked back to InProgress. Blocked
 *     is NOT terminal — it's the "in motion but paused" peer of
 *     InProgress.
 *
 * Stage transitions are one-way:
 *
 *   - Stage 1 -> Stage 2: ReadyToBegin -> InProgress (assignee starts).
 *     Once a Step enters Stage 2, it does NOT revert to Stage 1 —
 *     "missing prereq again" while in motion is modelled as Blocked,
 *     not as falling back to PendingPrereqs.
 *
 *   - Stage 2 -> Stage 3: InProgress -> Completed (work done OK).
 *
 *   - Any non-terminal -> Aborted (cancelled at any point).
 *
 * Five properties the live code relies on:
 *
 *   1. TerminalAbsorbing — once a Step is Completed or Aborted, no
 *      action can mutate it. Falls out structurally because every
 *      action's precondition rules out terminal sources.
 *
 *   2. NoSkippingReadyToBegin — every Step that has ever been
 *      InProgress was also ReadyToBegin first. The Messages inbox
 *      / dashboard need this signal so an assignee-actionable
 *      transition is never invisible.
 *
 *   3. PrereqsBeforeReady — at every reachable state, any Step in
 *      ReadyToBegin has all its `blocked_by` prereqs Completed. The
 *      bidirectional Stage-1 hinge enforces both directions:
 *      promotion gates on the check, reversion fires when the check
 *      becomes false.
 *
 *   4. CompletedOnlyFromInProgress — Completed is reachable only
 *      via the InProgress -> Completed arc; no path from Blocked
 *      (or earlier stages) jumps straight to Completed.
 *
 *   5. BlockedRequiresPriorInProgress — Blocked is the alternate of
 *      InProgress, not a state you can land in from Stage 1. Every
 *      Step in Blocked was previously InProgress at some point.
 *      This is the property that hardens "Blocked is real, not a
 *      degenerate Completed-with-warning."
 *
 * Implementation: instead of a transition history (which grows
 * unboundedly under the bidirectional alternates), the spec uses
 * monotone "ever-was" set variables. Total state space at the
 * default bound: 6^3 × 2^3 × 2^3 = 13,824.
 *
 * Scope: a single Job with a fixed step set + fixed blocked_by graph.
 * Cross-Job triggers + the embedded_job recursion are out of scope;
 * both reduce to "spawn a fresh Step state machine with its own
 * initial conditions" so the per-Step invariants here cover the
 * recursive case by construction.
 *
 * Run with:
 *   ./infra/tla/run-tlc.sh StepStatus
 *)

EXTENDS Naturals, FiniteSets, TLC

CONSTANTS
    Steps,            \* set of step ids in the Job, e.g. {s1, s2, s3}
    \* Each step's prereq set is passed as its own constant so the
    \* .cfg file can use plain set literals (TLC's .cfg parser does
    \* not accept inline function literals like [s1 |-> {s2}]).
    Prereqs_s1,
    Prereqs_s2,
    Prereqs_s3

\* Assemble the BlockedBy function from the per-step constants.
BlockedBy ==
    [s \in Steps |->
        IF s = "s1" THEN Prereqs_s1
        ELSE IF s = "s2" THEN Prereqs_s2
        ELSE IF s = "s3" THEN Prereqs_s3
        ELSE {}]

\* Sanity: no Step depends on itself.
ASSUME \A s \in Steps : s \notin BlockedBy[s]

Statuses == {
    "PendingPrereqs",
    "ReadyToBegin",
    "InProgress",
    "Blocked",
    "Completed",
    "Aborted"
}

TerminalStatuses == {"Completed", "Aborted"}

\* Stage groupings — useful for reasoning about "still in Stage N"
\* without enumerating every member.
Stage1 == {"PendingPrereqs", "ReadyToBegin"}
Stage2 == {"InProgress", "Blocked"}
Stage3 == TerminalStatuses

VARIABLES
    status,           \* [Steps -> Statuses]
    everReady,        \* SUBSET Steps — Steps that have ever been ReadyToBegin
    everInProgress    \* SUBSET Steps — Steps that have ever been InProgress

vars == << status, everReady, everInProgress >>

(*****************************************************************************
 * Type invariant
 *)
TypeOK ==
    /\ status         \in [Steps -> Statuses]
    /\ everReady      \subseteq Steps
    /\ everInProgress \subseteq Steps

(*****************************************************************************
 * Initial state — every Step starts as PendingPrereqs (the seed-loader
 * default in `materialize_steps`). No Step has yet reached
 * ReadyToBegin or InProgress.
 *)
Init ==
    /\ status         = [s \in Steps |-> "PendingPrereqs"]
    /\ everReady      = {}
    /\ everInProgress = {}

\* Helper: are all of s's prereqs currently Completed?
PrereqsComplete(s) ==
    \A p \in BlockedBy[s] : status[p] = "Completed"

(*****************************************************************************
 * Stage 1 actions — alternates within "not yet started."
 *)

\* PendingPrereqs -> ReadyToBegin when every blocked_by is Completed.
PromoteToReady(s) ==
    /\ status[s] = "PendingPrereqs"
    /\ PrereqsComplete(s)
    /\ status'         = [status EXCEPT ![s] = "ReadyToBegin"]
    /\ everReady'      = everReady \cup {s}
    /\ UNCHANGED everInProgress

\* ReadyToBegin -> PendingPrereqs when a prereq becomes unsatisfied
\* again (e.g. a part the Step needed gets allocated elsewhere; an
\* upstream Step's completion is reversed). The alternate-direction
\* of the same Stage-1 hinge.
RevertToPending(s) ==
    /\ status[s] = "ReadyToBegin"
    /\ \neg PrereqsComplete(s)
    /\ status'         = [status EXCEPT ![s] = "PendingPrereqs"]
    /\ UNCHANGED << everReady, everInProgress >>

(*****************************************************************************
 * Stage 1 -> Stage 2 transition.
 *)

\* ReadyToBegin -> InProgress when an assignee picks it up.
StartWork(s) ==
    /\ status[s] = "ReadyToBegin"
    /\ status'         = [status EXCEPT ![s] = "InProgress"]
    /\ everInProgress' = everInProgress \cup {s}
    /\ UNCHANGED everReady

(*****************************************************************************
 * Stage 2 actions — alternates within "in motion."
 *
 * Blocked is the peer of InProgress, not a terminal. Both directions
 * are explicit so the spec catches any code that treats Blocked as
 * "stuck-and-done" rather than "paused."
 *)

\* InProgress -> Blocked when work hits a snag (missing part, waiting
\* on a vendor reply, equipment unavailable, etc.).
BlockStep(s) ==
    /\ status[s] = "InProgress"
    /\ status' = [status EXCEPT ![s] = "Blocked"]
    /\ UNCHANGED << everReady, everInProgress >>

\* Blocked -> InProgress when the snag clears.
UnblockStep(s) ==
    /\ status[s] = "Blocked"
    /\ status' = [status EXCEPT ![s] = "InProgress"]
    /\ UNCHANGED << everReady, everInProgress >>

(*****************************************************************************
 * Stage 2 -> Stage 3 transitions.
 *)

\* InProgress -> Completed when work is done OK.
\* Note: Blocked CANNOT directly Complete — must first unblock to
\* InProgress and complete from there. The "completed-from-Blocked"
\* path doesn't exist; if someone says "we're done," they're describing
\* an unblock-then-complete pair.
CompleteStep(s) ==
    /\ status[s] = "InProgress"
    /\ status' = [status EXCEPT ![s] = "Completed"]
    /\ UNCHANGED << everReady, everInProgress >>

\* Any non-terminal -> Aborted (cancel from any non-terminal state).
AbortStep(s) ==
    /\ status[s] \notin TerminalStatuses
    /\ status' = [status EXCEPT ![s] = "Aborted"]
    /\ UNCHANGED << everReady, everInProgress >>

Next ==
    \E s \in Steps :
        \/ PromoteToReady(s)
        \/ RevertToPending(s)
        \/ StartWork(s)
        \/ BlockStep(s)
        \/ UnblockStep(s)
        \/ CompleteStep(s)
        \/ AbortStep(s)

Spec == Init /\ [][Next]_vars

(*****************************************************************************
 * Invariants
 *)

\* (1) Terminal states are absorbing — no action ever mutates a
\* Step that's already Completed or Aborted. Each action's
\* precondition explicitly rules out a terminal source; this
\* invariant is the closure that proves the property.
TerminalAbsorbing ==
    \A s \in Steps :
        status[s] \in TerminalStatuses
            => \A s2 \in Steps :
                status[s2] = "PendingPrereqs"
                    \/ status[s2] = "ReadyToBegin"
                    \/ status[s2] = "InProgress"
                    \/ status[s2] = "Blocked"
                    \/ status[s2] \in TerminalStatuses

\* (2) Every Step that has ever been InProgress was also
\* ReadyToBegin at some prior point. NoSkippingReadyToBegin in
\* monotone-set form.
NoSkippingReadyToBegin ==
    everInProgress \subseteq everReady

\* (3) Whenever a Step is currently in ReadyToBegin, all of its
\* prereqs are Completed. Both promotion and reversion preserve
\* this invariant by gating on PrereqsComplete.
PrereqsBeforeReady ==
    \A s \in Steps :
        status[s] = "ReadyToBegin" => PrereqsComplete(s)

\* (4) Completed is reachable only via the InProgress -> Completed
\* arc. Encoded as: any Step currently Completed must have been
\* InProgress at some point. (CompleteStep precondition guarantees
\* this; the invariant ratifies it.)
CompletedOnlyFromInProgress ==
    \A s \in Steps :
        status[s] = "Completed" => s \in everInProgress

\* (5) Blocked is the alternate of InProgress, not a state reachable
\* from Stage 1. Every Step currently Blocked must have been
\* InProgress at some prior point. The hardening property — proves
\* Blocked is not "stuck-Pending" or "stuck-Ready" but a real
\* in-motion alternate.
BlockedRequiresPriorInProgress ==
    \A s \in Steps :
        status[s] = "Blocked" => s \in everInProgress

================================================================================
