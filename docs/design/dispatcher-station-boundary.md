# The dispatcher/station boundary

**Status**: living

Decided by David on packet 194db591 (*Review how the dispatcher and
the queue layer divide matchmaking*), accepted 2026-08-19. This page
makes the boundary citable. It is a governance rule, not a decision
archive: cite it in review when a change puts routing policy on the
wrong side.

## The rule

**The dispatcher owns reactions. Stations own holding. Matchmaking is
station discipline — data, never dispatcher code.**

- **The dispatcher reacts.** It watches the clock (scheduled rules),
  thresholds (cadence and census rules), and the event stream
  (`step.done.<kind>` side-effect rules). Every reaction it runs is a
  registry row; the dispatcher binary knows *how* to run a rule and
  nothing about *why* any packet goes anywhere. A routing decision
  appearing in dispatcher Rust is a leak from the protocol layer into
  the substrate (CLAUDE.md, the three layers).
- **Stations hold.** Queue membership, ordering discipline, and
  capability checks live at the station: the membership predicate is
  the station row's `WHERE`, the discipline is its ordering data, and
  capability is enforced at the claim. A packet waits at a station
  because the station's own data says so — which is what lets an
  operator answer "why is this packet in front of this actor" by
  reading one row.
- **Matchmaking is station discipline.** Which actor a ready step is
  offered to is queue policy, so it belongs in station data alongside
  the rest of the discipline. The dispatcher may *execute* a
  matchmaking rule; it must not *be* one.

## The two current violations

Named here so the boundary is measured against the tree, not against
intent. Each is a packet of ordinary size; neither is licensed by
appearing in this doc.

1. **The assign strategy is dispatcher code.**
   `crates/core/boss-dispatcher/src/dispatcher.rs` —
   `pick_employee_with_role_fallback` (and `pick_employee` under it)
   hardcodes the matchmaking policy: candidate roles are tried in
   order, the first role with an active holder wins, and load spreads
   deterministically by step id across that role's holders. All three
   choices are queue discipline, and none of them is data. A tenant
   that wants round-robin-by-department, or seniority-weighted
   assignment, or no auto-assignment for a given station, has to fork
   the dispatcher. The migration path: an `assign` discipline field on
   the station row, executed by the dispatcher the way it already
   executes rule rows.

2. **My Day's queue partition is client code.**
   `apps/web/src/me/assignments.ts` — `splitQueues`, `needsAPerson`,
   and the `VERDICT_KINDS` roster partition the assignments feed into
   queues in the SPA. The server's `WHERE` defines *membership*, but
   which queue a row lands in (mine / verdicts / up-for-grabs /
   waiting-on-automation) is decided in TypeScript, and the verdict
   roster is a client-held copy of a fact that belongs on the StepType
   registry (a `decision_shaped` flag — the upgrade path is already
   documented at the roster's declaration). A second consumer of the
   same queues (the simulator UX, a CLI docket view) would have to
   re-implement the partition and would drift.

## What this does not say

- It does not say the dispatcher shrinks to zero code — executing
  rules, NAK/redelivery, and dead-lettering are substrate and stay.
- It does not adopt "Q" as an owning domain. David's 1dfed5d6
  question (graduate the dispatcher into a rich actor service?) stays
  open in the design queue; this boundary is compatible with either
  answer and is the yardstick to evaluate it with.
- It does not schedule the two migrations. They are backlog packets;
  this page exists so their reviews have something to cite.
