# Design: packet loss

**Status**: decided — all questions answered by David in review `9fb9904f`, 2026-08-19.
**Origin**: David, 2026-08-13: "I am also afraid that 'packet loss' is
going to be a real issue for us."
**Related**: [job-packet-network.md](./job-packet-network.md) ·
[stations.md](./stations.md) ·
[correctness-protocol.md](./correctness-protocol.md) — this is
**conservation**, the second of the five properties, applied to
packets rather than to money.

---

## It is already real. Five instances, one day.

| # | what happened | loss mode |
|---|---|---|
| 1 | Pipeline bookkeeping wrote to the legacy instance for ~14h; the system of record never saw it (incident `c4b4a6b0`) | **misrouted** — delivered to the wrong fabric |
| 2 | The branch sweep deleted a branch carrying two unmerged commits: the job record said "landed" and was right about the boarded ref, wrong about the branch | **destroyed** — content gone, record clean |
| 3 | Train #7 sat CI-green and unmerged for 95 minutes; the conductor was killing its own verb before the merge, and nothing detected the stall | **stalled** — in flight, no motion, no signal |
| 4 | 16 feedback packets sat at `submitted` while the work they authorized shipped; the queue said "pending", production said "done" | **stalled + unacknowledged** |
| 5 | Cars conflict-skipped at boarding sat mute in the dock: correct behavior, invisible cause (fixed by `skip_reason`) | **stalled, undiagnosable** |

None of these were noticed by the system. Four were noticed by David
looking at a page; one by an agent verifying something else.

## The taxonomy

- **Misrouted** — the packet exists, on the wrong fabric. Invisible
  to every lens that matters.
- **Destroyed** — content lost while the record reads clean. The
  worst kind: the log is *wrong* rather than incomplete.
- **Stalled** — admitted, never terminal, nothing raising it.
- **Orphaned** — exists, matches no station predicate, so no queue
  will ever present it to anyone. Structurally unworkable.
- **Unacknowledged** — terminal reached, the party who cares never
  learns. (David's ruling of 2026-08-13 makes notification
  mandatory, which closes this one by protocol.)
- **Unsent** — the packet never enters, because the sender stopped
  believing it would move. David, 2026-08-13, after being shown 16
  untriaged items: *"I have hesitated to add more feedback"*, and
  then: *"it shows the importance of keeping the network flowing and
  transparent to the actors."*

**Unsent is the worst mode and the only one no census can see.** The
other five leave a record — a row on the wrong fabric, a stalled
step, an orphaned packet, a dangling edge. An unsent packet leaves
nothing at all, so the instrument that finds it is not a query but a
person's confidence, and the only way to measure it is to notice that
signal volume fell while the system reported itself healthy.

This is why flow and transparency are load-bearing rather than
polish. A queue that visibly moves recruits its own senders; a queue
that silently absorbs teaches people to stop sending, and an
algedonic system whose pain signals have been trained out of it
reports perfect health right up until it fails. Beer's whole point is
that the signal must reach the level that can act — and a sender who
has learned not to bother has severed that path more completely than
any dropped packet.

## What makes them detectable now

Two things landed today that were not available before:

1. **The system of record is one place.** Misrouting is detectable
   because there is a canonical fabric to be absent from.
2. **Stations are data.** A packet's queue membership is now
   computable: for each open packet, ask which station predicates it
   satisfies. **A packet matching zero stations is orphaned by
   definition** — nobody's lens will ever show it, so no actor will
   ever work it. This is the sharpest available definition of "lost"
   and it did not exist a day ago.

## Open questions

None — every question was answered in review `9fb9904f` on 2026-08-19; see Decision history.

## Decision history

**Q1 — What is the conservation invariant, exactly (decided by David in review `9fb9904f`, 2026-08-19).**
**every admitted packet reaches a terminal, and every non-terminal packet is visible at ≥1 station.** The first half is conservation over time; the second is conservation over space, and it is the one stations make checkable. A census can compute both without new state.

**Q2 — What does the network do when it finds loss (decided by David in review `9fb9904f`, 2026-08-19).**
**(a) then (b)**. Report first, because we do not yet know the base rate and a noisy raiser trains people to ignore it; then raise once the thresholds are calibrated against real numbers. (c) is tempting and wrong for now: a catch-all station that silently absorbs orphans converts a visible defect into a tidy queue nobody reads.

**Q3 — Where does the census run (decided by David in review `9fb9904f`, 2026-08-19).**
a **cadence rule** writing its counts to the log, so loss becomes a measured series rather than a spot check — the same move that turned train timings into the retro's evidence. The lens then reads the series instead of recomputing it.

**Q4 — Is destroyed-content detectable at all (decided by David in review `9fb9904f`, 2026-08-19).**
out of scope for the census, and worth its own answer; naming it here so it is not mistaken for covered.

