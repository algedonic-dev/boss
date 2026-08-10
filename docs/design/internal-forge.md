# Design: the internal forge — Git, CI, and maintenance come inside

**Status:** draft — open questions tracked at `/system/design`
**Origin:** David, 2026-08-10 (`4bff901a`): "We are going to
internalize Git and CI" — plus, in the same breath: "develop a
better sense of the building up of commits into PRs pushed onto
trains, deployed periodically … I think we also have maintenance
missing here."
**Related**: [dev-cluster.md](./dev-cluster.md) (declared this as
the after-runners direction) · [idm-kanidm.md](./idm-kanidm.md) ·
[it-activity-network.md](./it-activity-network.md)

## What internalizing buys

- **The merge wall becomes policy.** Today merge is a GitHub admin
  bit no BOSS rule can see (the no-oversight test hit it
  structurally). Internal: the ship-a-change `review` step IS the
  review — the operator's sign-off completes it, the conductor
  executes the merge through the forge API, and the review budget
  becomes a policy row instead of a foreign platform's permission.
- **The external station comes inside.** On the activity network,
  GitHub/CI is the dashed station where packets vanish into
  off-department time. The forge and runners become instrumented
  stations; trains never leave the map.
- **Kanidm pays twice**: the forge fronts with the same OIDC door;
  agent service accounts (idm Q3, phase 2) are what let an agent
  merge under BOSS policy rather than a borrowed human credential.

## The pipeline, grained

David's "building up" ask, modeled: **commits accrete into a
change; changes board a train; trains deploy periodically.** Each
grain is already half-present — ship-a-change Jobs carry branches,
pr-train Jobs carry consists, the reconcile stamps deploys — but
the accretion itself is invisible: nothing shows a change GROWING
(commits landing on its branch), a train FILLING (candidates
parking), a deploy WINDOW approaching. Internal CI gives us the
events to model all three (the forge emits push/check/merge webhooks
we can land on the outbox), so the canvas can show the dev pipeline
as nested packets: commit-dots accreting onto a change-packet,
change-packets boarding the train-packet, the train departing on
schedule.

## Maintenance is missing

The department's recurring labor — backup, audit-integrity,
ledger-replay checks, views catchup, files GC, message purge, the
reconcile itself, certbot renewals, and soon the flush worker — runs
as systemd timers OUTSIDE the Job model. Invisible work, in a system
whose thesis is that work is visible. The dispatcher's schedule
runner already fires clock-driven rules; a `maintenance` Workflow
family (spawned on cadence, auto-executed, self-closing, loud on
failure) would put every chore in the log, on the canvas, and in the
stage-duration numbers — and a failed backup becomes an algedonic
signal instead of a quiet journal line.

## Open questions

### Q1: Forgejo, and where does it live?

dev-cluster named Forgejo. Propose: confirm it; it runs on the
cluster (repos are exactly the always-rebuildable-from-mirrors
state the cluster may hold), with its data in the backup set and
GitHub as the safety mirror.

### Q2: CI engine?

Propose Forgejo Actions — GitHub-Actions-compatible, so the six
checks port near-verbatim onto the cluster runners (builder image +
sccache per dev-cluster).

### Q3: What exactly is the internal review protocol?

Propose: the ship-a-change `review` step gains a sign-off gated on
the operator role; completing it (with the sign-off) is the merge
authorization; the conductor merges via forge API. The PR page
becomes a diff viewer; the decision lives in BOSS.

### Q4: Mirror shape?

Propose: Forgejo → GitHub push-mirror on every main update; the
public OSS presence is unchanged; inbound contributions arrive via
GitHub and are pulled in deliberately.

### Q5: Do forge events land on the outbox?

Propose yes — push/check/merge webhooks → a small ingress (the
webhook handler pattern) → `forge.*` kinds in the registry → the
pipeline grains above become queryable and animatable like
everything else.

### Q6: What is the maintenance Workflow family?

Propose: one `maintenance` kind per chore, spawned by schedule
rules (the clock runner exists), steps auto-executed by handlers,
failure leaving the Job OPEN and loud on the canvas. The systemd
timers remain the executors initially — the Job is the visibility
layer — and migrate into handlers where it pays.

### Q7: Sequencing — what preps before the cluster is up?

Propose: (a) the conductor's gh calls behind a thin forge seam
(adapter swap at cutover); (b) the maintenance family needs no
forge at all and could ship next; (c) forge install + runner
config staged like infra/idm; actual cutover waits on Talos + a
rehearsal like the migration's.
