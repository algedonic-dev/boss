# Design: the system took its own record-keeper down, and did not notice

**Status**: in-review — protocol proposals need David's call
**Origin:** David, 2026-08-13 (verbatim): "Losing the communication
because of a bad push but not even realizing we were the cause is a
big problem." And: "I want to understand thoroughly what happened here
so that we can implement a protocol to avoid it."
**Related**: [packet-loss.md](./packet-loss.md) ·
[deployment-as-network.md](./deployment-as-network.md) ·
[design-conformance.md](./design-conformance.md) ·
[schema-migrations.md](./schema-migrations.md)

---

## What happened, in one line

A three-line comment edit in an already-applied migration was merged,
deployed, and — because the Deployment's strategy is `Recreate` —
terminated the only pod serving the system of record before
discovering it could not start its replacement. BOSS deleted its own
ability to record work, and then the operator spent an hour looking
for the cause in the wrong building.

## The chain

Each link is necessary; removing any one of them prevents the outage.

1. **The flatten car (`7806c07`) edited `111-gateway-audit-events.sql`.**
   Three lines of comment, repointing a doc reference. No SQL changed.
2. **Nothing caught it before merge.** The gate has no rule about
   editing an applied migration. `schema-converge.sh` checks that the
   deploy paths converge the schema *from the tree* — not that the
   tree's history is append-only.
3. **CI could not have caught it as written.** The test suite applies
   migrations to a scratch database created per test. A scratch
   database has no history, so "this file changed after it was
   applied" is unreachable by construction. **CI tests migration from
   empty; production applies them incrementally. Those are different
   programs, and only one of them was ever run.**
4. **Train #21 merged and deployed** image `boss:0b6f35c`.
5. **The Deployment's strategy is `Recreate`.** Kubernetes terminated
   the healthy pod *first*, then created the replacement.
6. **`boss-init` refused to start.** `migrate.sh` compared the file's
   hash against `schema_migrations.checksum`, found
   `18902fd80ceb…` where `f7cf9c874953…` was recorded, and exited
   nonzero rather than run services against a database it could not
   converge. **This guard behaved perfectly.** It is the only
   component in this story that did its job.
7. **CrashLoopBackOff.** No pod served. `10.20.0.34:7900` went dark.
8. **The system of record was the casualty.** Job bookkeeping stopped,
   so the incident could not be filed, the repair could not be
   tracked, and the audit log gained no record of the event that
   mattered most that day.

## The second failure: we did not suspect ourselves

This is the part David named, and it is worse than the outage.

The SoR went unreachable minutes after a deploy that this system
performed on itself. The correct first hypothesis was *"what did we
just ship?"* The actual first hypothesis was *"what broke in the
world?"* — and an hour went into switches, boot order, PXE loops and
standby power, on hardware that was healthy the entire time.

Three things made that possible, in ascending order of importance:

- **Stale infrastructure truth.** The node addresses were read from
  `~/talos-homelab/final-cp-*.yaml` — the v1 configs, on
  `192.168.1.x`. The live cluster is v2 on `10.20.0.x` with its own
  `v2/talosconfig`. Nothing in either file says which is live. This is
  backlog item `3752b277` ("cluster manifests are not under version
  control") pricing itself: not merely untracked, but *ambiguous*, and
  the ambiguity was resolved wrongly under time pressure.
- **Contradicting evidence was explained away, twice.** UniFi reported
  4 days of uptime; that was dismissed as an unreliable wired-client
  counter. David reported real upload/download throughput; that was
  reinterpreted as PXE broadcast noise. Both readings were correct.
  Each was reconciled with the failing model instead of being treated
  as evidence against it.
- **The system could not tell us.** With the SoR down there was no
  packet, no station, and no audit entry describing the deploy that
  had just happened. The one artifact that would have collapsed the
  diagnosis in seconds — "a deploy landed at 18:16 and its init
  container is failing" — existed only inside the cluster nobody
  thought to look at, precisely because the thing that would have
  pointed there was the thing that was down.

## The shape both failures share

A fact lived in two places and nothing compared them.

- The migration's **content** in the tree, and its **checksum**
  recorded in `schema_migrations`. Compared for the first time at
  deploy, in production, after merge.
- The cluster's **addresses** in the v1 configs, and its **real**
  addresses in v2. Never compared at all.

CLAUDE.md §9a says a fact that lives twice gets an equality test.
Neither pair had one. The outage is what it costs when the comparison
happens at the last possible moment; the misdiagnosis is what it costs
when it never happens.

## Proposed protocol

Five changes, cheapest first. (1) and (2) each independently prevent
this exact outage; (4) and (5) address the class David named.

### 1. Migrations are append-only in the repo — a gate lint

Any file under `infra/postgres/schema/` that already exists on `main`
may not be modified; only new files may be added. Pure git, no
production knowledge, runs at gate time, catches this class before
merge rather than after.

Cost: one lint. Caveat: a genuinely wrong migration that has never
been applied anywhere becomes harder to fix in place — the escape
hatch is an explicit allow-list entry naming the file and the reason,
in the ratchet style the repo already uses.

### 2. Never terminate the old pod for an unvalidated new one

`strategy: Recreate` is why an init failure became an outage instead
of a stalled rollout. Under `RollingUpdate` with `maxUnavailable: 0`,
the old pod keeps serving, the new pod fails its init, and the deploy
simply does not progress — visible, recoverable, and nobody notices at
2am.

`Recreate` is not obviously wrong here: with one database and a
schema-changing init container, it avoids two code versions against
one schema. So this is a real trade and it is David's call. The middle
path: keep `Recreate` for the app, but move migration into a
**pre-deploy job that must succeed before the rollout starts**, so a
migration that cannot converge stops the deploy rather than the
service.

### 3. Test migrations the way production runs them

CI applies migrations to an empty scratch database; production applies
them to a database with history. The failure mode that bit us is
invisible to the first and routine in the second. A CI step that
applies the manifest, then re-applies it against the same database,
would catch checksum drift the way production does. This is the
deeper fix and the most work.

### 4. On a control-plane outage, suspect the last deploy first

A diagnostic protocol, not code. When a BOSS service becomes
unreachable, the ordered first questions are: *what did we deploy most
recently, when, and did it succeed?* — before any question about
hardware, network, or power. This system deploys itself; it is
therefore the most likely cause of its own outages, and it was.

### 5. The incident record must survive the SoR

Today an outage of the jobs door erases the ability to record that
outage. Whatever the mechanism — an append-only local file on
boss-gcp, a packet queued for replay, a bulletin written to the forge
— **a self-inflicted outage must leave a trace that does not depend on
the component it took down.** Without this, the system's worst days
are precisely the days with no audit trail, which inverts the point of
having one.

## Open questions for David

1. **Is `Recreate` deliberate?** If it is there to prevent two app
   versions against one schema, the pre-deploy-job variant in (2)
   preserves that property while removing the outage. If it is a
   default nobody chose, `RollingUpdate` with `maxUnavailable: 0` is
   strictly better.
2. **How far to go on (3)?** Re-applying the manifest twice in CI is
   cheap and catches this class. Reproducing production's *actual*
   applied-set is expensive and catches more. The cheap version may be
   enough.
3. **Where does the out-of-band incident record live** (5)? boss-gcp
   is the obvious host, but it is also the box the offsite-backup job
   (`ba0e54e2`) exists to stop being a single point of failure.
4. **Does (1) need an escape hatch on day one**, or should the first
   version be an absolute ban, with the allow-list added when
   something legitimately needs it?

## Decision history

- **2026-08-13 — the guard was not the problem.** `migrate.sh`
  refusing to run services against an unconvergeable database is
  correct and stays. Every proposal here moves the *detection* earlier
  or makes the *failure* cheaper; none weakens the check. Recorded
  because the tempting fix — teach `migrate.sh` to ignore comments —
  removes a total, dumb, reliable guard to buy back three lines of
  prose, and would leave the `Recreate` outage path untouched.
