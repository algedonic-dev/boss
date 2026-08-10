# Design: deployment as a network — generations, confirms, waves

**Status:** draft — open questions tracked at `/system/design`
**Origin:** David, 2026-08-10 (`8b508f95`): "I think we should model
our deployment around networking principles" — following the
discussion of how networks patch at scale.
**Related**: [schema-migrations.md](./schema-migrations.md) (the
N-1 compatibility that makes this safe) ·
[internal-forge.md](./internal-forge.md) (the pipeline this deploys)
· [dev-cluster.md](./dev-cluster.md)

## The three layers, applied to deploys

Networks patch forward because they distinguish what KIND of thing
changes — and deployment maps onto the same split BOSS already runs:

- **Traffic** — requests in flight, Jobs mid-step, the audit log.
  Never rolled back; a delivered response is history.
- **Derived state** — the running binaries, the projections, the
  served SPA. RECONVERGED, not restored: rebuilt from intent, freely
  replaceable, no snapshot nostalgia.
- **Intent** — the repo at a commit + the registries + config. The
  versioned layer. "Rollback" exists here only as rolling FORWARD to
  a prior intent: a new deploy action that reproduces yesterday's
  shape, then convergence.

Two prerequisites are ALREADY policy, which is why this design is
mostly plumbing: **expand/contract migrations are exactly the N-1
compatibility** that lets a reverted binary run on today's schema
(networks call it graceful restart; we legislated it 2026-08-08),
and the SPA's content-hashed dist is make-before-break natively.

## The mechanisms

- **Generations (make-before-break).** Installs land in versioned
  directories (`/usr/local/boss/releases/<sha>/`) with a `current`
  symlink; the previous generation stays on disk. Deploy = install
  beside, flip, restart. Revert = re-point, restart — seconds, not a
  rebuild. The old path exists until the new one carries traffic.
- **Commit-confirmed (the dead-man switch).** After the flip, the
  deploy is UNCONFIRMED for N minutes: the health gate (the existing
  per-service checks + dispatcher readyz + a smoke probe) must
  confirm, or the deployer auto-reverts to the previous generation
  and says so loudly. A bad train costs minutes, unattended — which
  matters more as the box's own network path (forge, IdP, cluster)
  joins the loop.
- **Waves (canary).** Wave 1 is the scratch environment the deploy
  script already serves; prod follows only on scratch's confirm.
  Cluster nodes become waves 2..n when they exist.
- **Drains.** Real drain-patch-undrain arrives with k8s; on the
  single box, restart order + health gates approximate it. Named so
  nobody mistakes the approximation for the thing.

## Open questions

### Q1: What is the generation store, and how many do we keep?

Proposed: `releases/<train-merge-sha>/` holding bin + web dist;
`current` and `previous` symlinks; keep 3. The conductor's deploy
phase stamps which generation a train landed as, so the Job record
names the artifact.

### Q2: What constitutes the confirm, and what is N?

Proposed: every service health 200 + dispatcher readyz + one
end-to-end probe (a jobs-api write round-trip), evaluated at 2 and
8 minutes; N=10. Confirmation is recorded on the train Job
(`deployed` completes only on confirm — an auto-revert reopens it
loudly).

### Q3: What does auto-revert cover?

Proposed: binaries + web dist only. Schema stays forward-only
(expand/contract holds N-1 both directions); registries and data are
intent that rolled forward and stays. A revert is derived-state
reconvergence, never history surgery.

### Q4: Who owns the mechanism?

Proposed: deploy-services.sh grows generations + confirm/revert (it
already owns install + health); the conductor's deploy phase calls
it and interprets the confirm for the train Job. The maintenance
family watches for a standing UNCONFIRMED/reverted state.

### Q5: When do waves become real?

Proposed: scratch-first ordering lands with this design; per-node
waves and true drains wait for the cluster and are Q4 of
dev-cluster's territory — named here so the two docs stay joined.
