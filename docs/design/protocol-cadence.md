# Design: protocol cadence — the clock coordinates, systemd supervises

**Status:** in-review — open questions tracked at `/system/design`
**Origin:** David, 2026-08-12 (verbatim, `bacca14e`): "We should be
using dispatcher to coordinate the conductor as well rather than
systemd. We want every protocol internalized so we can measure,
experiment, and update."
**Related**: [protocol-policy-publish.md](./protocol-policy-publish.md) —
revises its "timers never migrate" boundary ·
[internal-forge.md](./internal-forge.md) — supersedes half of Q6's
resolution, with its objection answered ·
[job-packet-network.md](./job-packet-network.md) ·
the clock-as-service rule, documented in the header of
`infra/lint/no-wallclock.sh` (no doc was ever written for it)

## The claim

A protocol's cadence — when its windows open, how often its
reconciliation runs — is part of the protocol, and today it is the
one part living outside the system: in systemd timer units, on a
box, invisible to the log, changeable only by an operator with sudo.
Internalizing it means three things his sentence names exactly:

- **measure** — every window-open and every reconcile tick is an
  event in the log, so "how often does the train actually run" and
  "what did the cadence cost" are queries, not folklore;
- **experiment** — cadence is a dispatcher rule row, edited through
  the rules API and hot-reloaded by the existing 30-second
  supervision (`1e576baf`), so trying a 3×-daily train is a data
  change with an audit trail;
- **update** — no unit files, no daemon-reload, no drift between
  boxes; the cadence deploys with the registry like every other
  protocol change (and under 3P, a protocol edit already is a
  network configuration change).

systemd is not deleted; it is demoted to what an OS is for —
**keeping processes alive**. Coordination of work belongs to BOSS.

## The maintenance-family objection, answered

internal-forge Q6 chose raw timers deliberately: "the dispatcher's
schedule runner fires on SIM-day boundaries, and at warp a daily
rule fires every couple of wall-minutes. Maintenance is wall-clock
work." That was an objection to the runner's *time basis*, not to
internalized cadence. The answer is to make the basis explicit:
a cadence rule declares `basis: wall | clock`. Wall-basis rules fire
from wall time regardless of warp (backups, certificate renewals,
the train's twice-daily); clock-basis rules keep today's sim-day
semantics (the brewery's daily cycles). With the basis field, the
maintenance family's timers migrate too — same guarantee they moved
to systemd for, now as measurable data. This doc supersedes the
timer-as-spawner half of that resolution once the basis lands.

## The conductor as a subscribed executor

The dispatcher cannot exec a binary on another box, and should not
learn to. The conductor becomes what every other actor already is:
**an executor with a queue**. A cadence rule emits the window packet
(`train.window.opened`, payload naming the window); the conductor —
`boss train serve`, a durable consumer exactly like the dispatcher's
own JetStream loop — claims it and runs the phases it already owns.
systemd keeps `boss-train.service` alive (a simple long-running
unit, no timers); the OS supervises the process, BOSS coordinates
the work. Reconcile ticks ride the same shape at their own cadence.
Every phase the conductor completes is already Job/step data; with
the trigger internalized, the train protocol is measurable
end-to-end: cadence → window → board → CI → merge → arrivals.

## Sequencing

Strictly after the Rust conductor lands (`26d61c97`) and never under
a moving train: (1) the cadence-rule schema + basis field + runner
support; (2) `boss train serve` consuming window packets, proven in
shadow against the timers; (3) the timers delete, systemd unit goes
long-running; (4) the maintenance family migrates onto wall-basis
rows, retiring its ExecStartPre wrapper pattern.

## Open questions

### Q1: What is the cadence row's shape?

Proposed: dispatcher rules grow a `[[cadence]]` sibling: `name`,
`basis: wall|clock`, `every` (interval) or `at` (times-of-day),
`emit` (topic + payload template). Same table, same hot-reload, same
ratchet posture as `[[rule]]` — and the departure board's "next
departure" line reads it as data.

### Q2: Where does the wall-basis tick come from?

The schedule runner drives off the clock service's tick stream,
which under warp compresses days. Proposed: the clock service
already knows both times (`ClockNow` carries wall and sim); the
cadence runner evaluates wall-basis rows against wall time from the
same feed — one clock service remains authoritative for both bases,
and no component grows a second time source.

### Q3: Exactly-once windows across restarts?

A cadence firing must not double-emit after a dispatcher restart nor
skip a window that elapsed while down. Proposed: each firing records
its event with a deterministic id (`cadence:<name>:<window-stamp>`),
the outbox dedupes on it, and catch-up on start emits at most the
single most-recent missed window per rule — a deliberate "no
thundering backfill" choice matching the conductor's own
one-window-at-a-time cadence.

### Q4: Does the conductor's queue use the claim CAS?

Proposed: yes — window packets are ordinary packets; `boss train
serve` claims via the same CAS the human queues use, which gives the
board's live dot its data and makes a second conductor instance safe
by construction rather than by deployment discipline.
