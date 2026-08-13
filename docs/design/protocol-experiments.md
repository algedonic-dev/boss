# Design: protocol experiments — variants as data, verdicts from the log

**Status:** in-review — open questions tracked at `/system/design`
**Origin:** David, 2026-08-12 (verbatim): "We can definitely take
empirical data from our train deployment protocol and use that to
propose changes. We haven't really exercised our capabilities for
supporting experimentation with new protocols, but it will be
important."
**Related**: [protocol-cadence.md](./protocol-cadence.md) ·
[job-packet-network.md](./job-packet-network.md) — fixed protocol
set at creation is what makes cohorts clean ·
[protocol-policy-publish.md](./protocol-policy-publish.md) ·
[queue-visibility.md](./queue-visibility.md)

## The experiment that already ran

The forge cutover day doubles as the first measured protocol
comparison, and its numbers motivate this design:

- **Old protocol** (GitHub-gated): train #228 sat **~10 hours
  all-green** on a merge permission — the gate cost was the whole
  cycle time.
- **New protocol** (forge-native): train 1801's clean run went
  **CI-green → merged in 1 second → playground deployed in 8
  minutes → cluster converged within the runner's next tick**. The
  landing leg is now measured in minutes.
- **The repair tax moved**: five red rounds on train 1650, and
  **every one was environment, not car code** — missing
  interpreter, runner-cached image, container-root semantics, and a
  timing flake twice. Each attribution cost a manual log
  excavation (docker-cp + zstd + grep) of a 15–25-minute test run.

Three protocol changes fall straight out of that data:

1. **A locomotive check before boarding** — a seconds-long canary
   validating the environment against the suite's declared needs
   (toolchain present, runner image digest == registry digest, uid
   expectations), so environment drift fails before a 25-minute
   test run, not after. Every red tonight would have been caught by
   it.
2. **Attribution lands on the Job** — the `forge.*` ingress
   (already resolved) writes the failing check's log excerpt into
   the train Job's ci step metadata; blame becomes a step read, not
   an excavation.
3. **Re-signal becomes a verb and a metric** — `boss train
   resignal` stamps a counter on the train Job; retry cost turns
   into protocol telemetry instead of anecdote. (Likewise the
   flush-window practice: boarding when the dock is deep is a
   cadence trigger — `basis: queue-depth` joins wall|clock in
   protocol-cadence.md's row.)

## The capability

Everything an experiment needs already exists as data; what is
missing is the harness that ties it together:

- **A variant is a workflow version.** The registry is append-only
  and versioned; registry writes are now log events. An experiment
  arm is a draft published alongside the incumbent, not a fork of
  anything.
- **Assignment is at admission.** The packet model fixes the
  protocol set at creation — so cohort membership is decided once,
  recorded on the envelope, and never ambiguous mid-flight.
  Arm selection per packet (hash-spread like the dispatcher's
  assignment) or per window (alternating cadence firings) are both
  deterministic and replayable.
- **Measurement is the log.** Marker events are correlatable
  (`27341d5d`); per-version traffic has an index waiting
  (`jobs_kind_version`); the flow-strip machinery computes
  depth/latency per queue. An experiment's verdict is a query
  filtered by workflow_version — no bespoke instrumentation.
- **Conclusion is publish or retire.** Adopting the winner is the
  registry operation that already exists, and the log shows exactly
  which packets ran under which arm forever.

## Open questions

### Q1: What declares an experiment?

Proposed: an `experiments` registry row — name, the kind, the arm
versions, the assignment rule (`per-packet-hash | per-window`), the
split, the metrics (named queries over the log), and a review Job
that owns the verdict. Registry data like everything else; the
admission edge reads it when fixing a packet's protocol set.

### Q2: What guards a bad arm?

Proposed: arms are subject to the same lint/viability proofs at
publish; a kill = retiring the arm version (packets in flight finish
under their pinned version per the packet model); and the algedonic
default — an arm whose queue depth or failure rate exceeds the
incumbent's by a declared margin raises to the experiment's owner.

### Q3: Where do verdicts render?

Proposed: the experiment is a lens (views-as-queue-lenses): its two
arms are two queue predicates over the same stations, its flow
strips are the comparison, and the verdict review lands in the same
Design Review queue as everything else. The yard shows a train's
arm the way it shows its consist — cohorts visible, never hidden.

### Q4: First experiment?

Proposed: the locomotive-check change itself — run windows with and
without the canary for a week and measure red-round rate and
time-to-green. The protocol that measures protocols should be the
first thing it measures.
