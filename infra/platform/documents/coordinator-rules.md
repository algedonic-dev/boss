---
profile: coordinator
lane: step
---

# Queue coordinator rules (BOSS) — read fully before the first cycle

These rules apply when you are explicitly assigned to coordinate the
BOSS queues. Read the current packet, its pinned workflow, the supported
door and the rules for each executor you assign. Sign as yourself and
stay within existing authorization. This document defines an operating
cycle; it grants no authority and supplies no timer, daemon or wake path.

1. **Observe before acting.** At startup/resume, sync source decisions
   to `origin/main` and run `boss orient`. Read current native execution
   capacity and worker status, ready/active claims, recent gate verdicts
   INCLUDING closed runs, parked/held cars, in-flight trains and proof
   owners. Record when and where each observation was made. Check an
   empty system-of-record read against a known packet on the same door,
   and compare list rows with totals before concluding absence. Open
   workflow jobs are not running executors. Count native execution slots
   separately from registered jobs and gate lanes; an unavailable native
   observation is unknown capacity, never an invented free-slot count.

2. **Advance completed work before refill.** A worker handback prompts
   immediate receipt, exact-head and claim readback. Advance eligible
   independent review, own-run reporting, authorized operator release,
   observed merge/convergence, required registry publication and recorded
   machine-run proof through their existing doors. A reviewer records a
   verdict; the authorized operator releases the car. Preserve complete
   receipts and failure diagnostics rather than storing an excerpt as
   their replacement. A source GREEN is not convergence or runtime proof.
   When a registry change needs publication after convergence, retain its
   owner and actual active-row readback obligation until the probe passes.

3. **Arrange distinct review promptly.** An early-review request is
   coordinator work: assign a distinct available reviewer while the
   builder's focused feedback is available, or record why it waits and
   what next observation would free it. Early review does not replace a
   fresh formal verdict after GREEN. Reuse an executor after its previous
   registered run closes; admit a fresh run for new registered work and
   never borrow a closed run marker or let a builder formally self-review.

4. **Refill actual available slots by urgency then age.** First verify
   each packet's current source or live premise and existing branches,
   gates, cars and trains. Admit eligible work through registered claims
   before child execution, choose disjoint work or explicitly coordinate
   shared files, and retain the observed native concurrency limit.
   Distinguish reservation, building, gating, reviewing and waiting for
   external evidence. Do not rebuild landed or in-flight work or spend a
   gate on a historical verdict already carried by a merged car. Free
   native capacity with eligible work is an explicit finding with a next
   action; quiet is not evidence that the queues are healthy.

5. **Keep gate submissions ready.** Builders retain useful targeted
   TDD and mandatory preflight; comprehensive validation belongs in the
   gates. Require a coherent frozen source checkpoint and distinct early
   review before publishing/gating. Preserve the actual failing check,
   full log and receipt, distinguishing branch defects from infrastructure
   refusal. Repair the measured cause and improve the relevant protocol
   or testing infrastructure. Bay occupancy alone does not justify an
   unready or duplicate submission.

6. **Keep holds packet-local.** A passkey, design decision, rollout
   condition or external event holds that packet while other authorized
   work continues. Every waiting item names its owner, reason, evidence,
   condition and next observable check. A global goal becomes blocked
   only under its own external blocking rule when meaningful authorized
   progress actually requires external input. Do not bypass a hold, infer
   live-operation permission from source preparation, or claim a blocked
   goal resumed without an actual controller transition.

7. **Record a cycle receipt and its next intended check.** Retain the
   actual check time and native observation source; execution slots
   used/free/unknown; separate workflow and gate counts; completed work
   advanced; newly admitted run ids; held owners and conditions; failed
   or unknown reads; evidence links; and the next intended check with its
   trigger. Write working files in the working directory THE RUN names.
   Without a registered run, use the coordinator assignment's own named
   scratch directory. Never the shared scratchpad root: dd747b4c measured
   two simultaneous runs overwriting each other's drafts and filing the
   other's evidence. Store the receipt through an existing authorized
   record door, using file flags for long prose, and verify the readback.

Run a cycle immediately on worker completion, gate verdict, approval
completion, train arrival or detected free capacity. While the executor
is active, a five-minute maximum quiet recheck interval is an initial
operating target; a ready handback warrants an earlier check. Avoid long
blocking waits when independent work exists, and use existing gate/ops
wait doors rather than invented pollers.

That target is not an implemented wake source. Inspect the actual goal,
controller and harness capabilities and record machine evidence for
completion notifications, queue events or supervised timers. A deadline
passing proves no wake or heartbeat. Hooks for another harness prove no
wake capability for the current executor. If only active-turn polling is
available, retain that limit and any overdue intended check. A stopped
executor or blocked goal needs its supported continuation path; prose
does not reactivate it. New wake adapters remain separate work with
their own evidence and any applicable authority review.

This first instruction/linkage car is partial. Keep its parent open
until two actual cycles are evidenced: one advancing a completed review
or early-review request, and one refilling a freed native slot. Preserve
native observations, registered admission, copied review/gate receipts
and subsequent source readback. Measure completion-to-next-action delay,
idle slot time while eligible work exists, first useful gate feedback,
repeat gate visits and proof age. Record actual wake capability as a
separate claim. Text checks cannot prove those cycles, unattended wake
or continuous operation.
