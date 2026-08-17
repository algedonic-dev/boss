# Design: checking out a dev node

**Status**: approved — every question answered in packet `2d43cbcb`; carried to a file 2026-08-17.

**Origin**: David, 2026-08-16: *"I am really hoping for an eventual
experience where I literally checkout a dev node from the website to do
my work."* Immediately after a laptop ran out of disk mid-build and
wedged its own shell — the second time in two days that development
happening on a personal machine cost a session.

**Related**: [durable-session](./durable-session.md) (the pod this
generalises) · `bossnet-physical-topology` Q1 (the Subject kinds,
accepted) · [presence](./presence.md) (who may claim one)

---

## What already works

Not a proposal — measured in `boss-dev` this evening:

| | |
|---|---|
| cores | 12 |
| `/scratch` (node-local, `CARGO_TARGET_DIR`) | 188 GB free; **22 GB** of build output during one gate run |
| `/work` (Longhorn PVC, the clone + CARGO_HOME) | 40 GB, **39 GB free** after that same run |
| Postgres | 16.15 sidecar on `127.0.0.1:5432` |
| toolchain | cargo, rustc, node 20, bun, tmux, psql |
| durability | a tmux session survived its `kubectl exec` exiting and a second, separate exec attaching |

The PVC barely moved while `/scratch` took 22 GB, which is the
manifest's central bet paying off: the reproducible thing persists, the
rebuildable thing sits on disposable node-local disk.

Two properties are worth naming because they are structural rather than
careful. `127.0.0.1:5432` inside the pod **cannot** be the production
database, which is the 2026-08-14 incident (582 scratch databases on
the production volume) removed by construction. And the image is the
one CI runs, so "works on the dev box" and "passes the gate" cannot
drift.

## What is missing

**One credential.** The pod cannot clone a private repo —
`could not read Username for 'http://10.20.0.15:3000'`. It was
bootstrapped tonight with a `git bundle` over `kubectl cp`, which
proves the environment and leaves it unable to fetch.

**It is a singleton.** One Deployment, one PVC, one workspace. There is
no allocation, no lease, and nothing to check out.

**Access is `kubectl exec`.** Which is a command, not a website.

## The shape

```
   [dev-node pool]              a StatefulSet: boss-dev-0, boss-dev-1, …
        │                       ordinals give stable identity + per-replica PVC
        ▼
   checkout packet  ──claim──►  one ordinal, leased to one actor
        │                       the Job IS the lease; its terminal releases
        ▼
   ready session                tmux inside the pod, workspace cloned
        │
        ▼
   idle sweep       ──────────► reclaims what was abandoned
```

The allocation is a Job, which means it is already queued, already
audited, already policy-gated, and already visible — none of that needs
building. What needs building is the pool and the lease.

## What this is not

Not a replacement for the agent's own session. An agent working the
queue needs a workspace too, and it is the same object; "checkout" is
not a human-only verb.

Not a VM per person. The pod shares nodes with the system of record,
which is why the resource limits in `boss-dev.yaml` exist and why a
pool needs a cap.

## Open questions

### Q1: Is a dev node a new Subject kind, or a `service-instance`? (resolved)

Resolved 2026-08-16 — accept.

A service-instance, with no new kind. The topology doc's Q1 is
accepted, so `node` and `service-instance` become Subject kinds; a dev
node is plausibly just a service-instance whose service is boss-dev
and whose node is cp-2. The checkout is a Job ABOUT that Subject,
which is exactly the relationship the packet model already has.
Inventing a `dev-node` kind would duplicate the fields that make a
service-instance one - which host, which port, which database - and
split the estate view in two. What distinguishes a dev node is not its
shape; it is that something can hold a lease on it.


The topology doc's Q1 is accepted: `node` and `service-instance` become
Subject kinds. A dev node is plausibly just a `service-instance` whose
service is `boss-dev` and whose node is cp-2 — no third kind.

Proposed: **`service-instance`, with no new kind.** The checkout is a
Job *about* that Subject, which is exactly the relationship the packet
model already has. Inventing `dev-node` would duplicate the fields that
make a service-instance one (which host, which port, which database)
and split the estate view in two. The thing that distinguishes a dev
node is not its shape, it is that something can hold a lease on it.

### Q2: Pre-provisioned pool, or provision on demand? (resolved)

Resolved 2026-08-16 — accept.

A small warm pool, as a StatefulSet. Ordinals give stable identity and
a per-replica PVC without writing an allocator, and scaling the
StatefulSet IS provisioning. Start at 2 - one for you, one for an
agent - which is honest about current demand. The real cost of on-
demand is a cold target/: tonight's single gate run produced 22 GB on
/scratch and took minutes, and that is paid on every checkout. A warm
pool amortises it, and the ordinal's PVC keeps CARGO_HOME and the
clone between checkouts. The cap matters because these pods share
nodes with the system of record.


A pool is instant and costs idle CPU/RAM/volumes on the same three
nodes that hold the system of record. On demand costs a cold start:
image pull (cached), clone, and a cold `target/` — the run measured
tonight took 22 GB and several minutes.

Proposed: **a small warm pool, as a StatefulSet.** Ordinals give stable
identity and a per-replica PVC without writing an allocator, and
scaling the StatefulSet IS provisioning. Start at 2: one for David, one
for an agent, which is honest about current demand. A cold `target/` is
the real cost of on-demand and it is paid on every checkout; a warm
pool amortises it, and the ordinal's PVC keeps `CARGO_HOME` and the
clone between checkouts.

### Q3: What does "from the website" actually hand you? (resolved)

Resolved 2026-08-16 — override.

I was thinking we could use a mime type to launch the terminal with
the necessary command to load up a running tmux session. Or something
like that. Let's just facilitate the leasing and connection making,
but I don't think we need to provide an interface in the ideal. The
dev can pick their own terminal.


Three readings, and they are not the same product. A page that shows
you a `kubectl exec` command (copy-paste — which David has already
ruled out as "not a protocol"). A browser terminal (xterm.js over a
websocket through the gateway). Or a page where you talk to the agent
already running in that pod, and never see a shell.

Proposed: **the checkout hands you a ready session, and the terminal
is a separate decision.** The Job's terminal state should carry
everything needed to attach — pod, namespace, tmux session name — so a
CLI attaches today and a browser terminal can attach later without
changing the protocol. Shipping a shell in a browser is a real piece of
work with real auth surface, and it should not be the thing that blocks
allocation from existing.

### Q4: What ends a checkout, and what reclaims a leak? (resolved)

Resolved 2026-08-16 — accept.

The Job's terminal releases it, and a maintenance sweep reclaims the
abandoned. A lease nobody ends is a pool that fills, and the failure
is silent - the third person to ask just gets nothing. Both halves
already exist: the packet model gives the explicit release, and the
sweep family (five targets, daily, each leaving findings on a packet)
gives the reclaim. An idle node with no live tmux session and no
commits for N days is a finding, and the sweep's own fork decides
whether to reclaim or leave it.


A lease nobody ends is a pool that fills. This is the failure mode that
matters, because it is silent: the third person to ask for a node just
gets nothing.

Proposed: **the Job's terminal releases it, and a maintenance sweep
reclaims the abandoned.** Both halves already exist — the packet model
gives the explicit release, and the sweep family (five targets, daily,
each leaving findings on a packet) gives the reclaim. An idle node with
no live tmux session and no commits for N days is a finding, and the
sweep's own fork decides whether to reclaim or leave it.

### Q5: What credential does a checked-out node hold? (resolved)

Resolved 2026-08-16 — accept.

Read-only, and never a push credential. The conductor now publishes
car branches itself from its own clone, so a workspace has no reason
to hold write access to the forge - the thing it would have been for
is already solved. A read-scoped token is the smallest grant that
makes the pod useful at all (it currently cannot clone: 'could not
read Username for http://10.20.0.15:3000'), and it is the last blocker
to development leaving personal machines. If a checkout ever needs
more, that is a presence-gated step on the checkout packet rather than
a standing capability of the pool.


It must clone. Must it push?

Proposed: **read-only, and never a push credential.** The conductor now
publishes car branches itself from its own clone, so a workspace has no
reason to hold write access to the forge — the thing it would have been
for is already solved. A read-scoped token is the smallest grant that
makes the pod useful, and it is the last blocker to development leaving
personal machines. If a checkout ever needs more, that is a presence-
gated step on the checkout packet rather than a standing capability of
the pool.

## Decision history

Reviewed as packet `2d43cbcb` on 2026-08-16; the packet carried the
prose and the questions, so this file is the residue rather than the
precondition. Answers verbatim:

**Q1 — Is a dev node a new Subject kind, or a service-instance?**

A service-instance, with no new kind. The topology doc's Q1 is
accepted, so `node` and `service-instance` become Subject kinds; a dev
node is plausibly just a service-instance whose service is boss-dev
and whose node is cp-2. The checkout is a Job ABOUT that Subject,
which is exactly the relationship the packet model already has.
Inventing a `dev-node` kind would duplicate the fields that make a
service-instance one - which host, which port, which database - and
split the estate view in two. What distinguishes a dev node is not its
shape; it is that something can hold a lease on it.

**Q2 — Pre-provisioned pool, or provision on demand?**

A small warm pool, as a StatefulSet. Ordinals give stable identity and
a per-replica PVC without writing an allocator, and scaling the
StatefulSet IS provisioning. Start at 2 - one for you, one for an
agent - which is honest about current demand. The real cost of on-
demand is a cold target/: tonight's single gate run produced 22 GB on
/scratch and took minutes, and that is paid on every checkout. A warm
pool amortises it, and the ordinal's PVC keeps CARGO_HOME and the
clone between checkouts. The cap matters because these pods share
nodes with the system of record.

**Q3 — What does 'from the website' actually hand you?**

I was thinking we could use a mime type to launch the terminal with
the necessary command to load up a running tmux session. Or something
like that. Let's just facilitate the leasing and connection making,
but I don't think we need to provide an interface in the ideal. The
dev can pick their own terminal.

**Q4 — What ends a checkout, and what reclaims a leak?**

The Job's terminal releases it, and a maintenance sweep reclaims the
abandoned. A lease nobody ends is a pool that fills, and the failure
is silent - the third person to ask just gets nothing. Both halves
already exist: the packet model gives the explicit release, and the
sweep family (five targets, daily, each leaving findings on a packet)
gives the reclaim. An idle node with no live tmux session and no
commits for N days is a finding, and the sweep's own fork decides
whether to reclaim or leave it.

**Q5 — What credential does a checked-out node hold?**

Read-only, and never a push credential. The conductor now publishes
car branches itself from its own clone, so a workspace has no reason
to hold write access to the forge - the thing it would have been for
is already solved. A read-scoped token is the smallest grant that
makes the pod useful at all (it currently cannot clone: 'could not
read Username for http://10.20.0.15:3000'), and it is the last blocker
to development leaving personal machines. If a checkout ever needs
more, that is a presence-gated step on the checkout packet rather than
a standing capability of the pool.

