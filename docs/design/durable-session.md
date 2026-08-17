# Design: the durable session — development inside BossInfra

**Status**: approved — every question answered in packet `9abd4ad5`; carried to a file 2026-08-17.

**Origin**: David, 2026-08-16: *"We want to move development into the
cluster exactly for this reason. But we keep getting stuck while
thinking we are setup properly."* And: *"My laptop is really meant to
just be a thin terminal to interact with a durable session somewhere
in the BossInfra layer."*

**Related**: `infra/cluster/manifests/boss-dev.yaml` (the pod, already
written and merged) · `bossnet-physical-topology` (the substrate this
runs on) · `the-three-layers.md`

---

## The claim

**The dev environment was designed, reviewed, and merged on 2026-08-15.
It has never run.**

`infra/cluster/manifests/boss-dev.yaml` landed on train 36. It is a
careful piece of work: the CI image itself rather than a copy of its
tool list, a Postgres 16 sidecar so `127.0.0.1:5432` cannot be
production by construction, a single-replica disposable storage class,
`CARGO_TARGET_DIR` on node-local disk instead of Longhorn.

Nothing applies it. No script in `infra/` references it, no CI job
applies it, and there is no `kubectl` on boss-gcp or the forge host to
apply it with. The file has been in the tree for a day and reads, to
anyone browsing the repo, as "we have a dev environment."

That gap **is** the thing David named. Not a missing design — a
missing apply, and no mechanism that would notice.

## What it cost today, measured

Every failure in this session is one defect wearing four coats: *the
environment I verified in is not the environment that runs.*

| verified in | actually ran in | cost |
|---|---|---|
| `boss train board` over ssh | the systemd unit, with drop-ins | wrong root cause — reported the conductor was pointed at the wrong database when it never was |
| `bunx` on the laptop | `boss-ci`, which copies `bun` and not the `bunx` symlink | train 52 red, cancelled and reboarded |
| gate fixture on `:5432` | this laptop's scratch Postgres on `:15432` | ~15 min |
| "the mocked specs run in CI" | GitHub Actions — the mirror, pushed by hand | 13 specs unrun; 2 sat red across the `/it/*` rename |

Four environments each claiming to be the target: the laptop,
boss-gcp, the `boss-ci` container on the forge host, and the cluster.
`boss-dev.yaml` collapses three of them into one by running the CI
image against a sidecar of the production Postgres major. It cannot
collapse anything while it is a file.

A fifth cost, ongoing: **three finished cars are sitting at the dock
because the session cannot push to the forge.** Every push today
needed David. That is not a permissions accident; it is a consequence
of the session living on a laptop that holds no infrastructure
credential.

## What is already decided

Not reopening these — `boss-dev.yaml` settled them and the reasoning
holds:

- The dev workspace runs on cluster hardware, not the laptop.
- It runs the **CI image itself**, so "works on the dev box" and
  "passes the gate" cannot drift.
- Postgres is a sidecar at `127.0.0.1:5432`, which removes the
  2026-08-14 incident class by construction rather than by memory.
- The workspace PVC is disposable and single-replica; `target/` is
  node-local `emptyDir`.

The manifest also names its own deferrals, and one of them is exactly
what David is now asking for: *"Running the agent itself in-cluster.
That needs credentials in the cluster, which is David's call, not a
manifest change."*

That is the decision in front of us.

## Open questions

### Q1: Does the cluster have the disk for a session's build? (resolved)

Resolved 2026-08-16 — accept.

Measure before applying, and pin the pod. A cold `cargo test --all-
features` needs ~74 GB of target/ (measured on train 49: forge host
went 141 GB free to 67 GB). That lands in /scratch, an emptyDir on
whichever control-plane node the scheduler picks — the 40 Gi workspace
PVC does not cover it, and no nodeSelector pins it. cp-1/2/3's free
space has never been measured, and those are the same three nodes
holding the system of record's Longhorn replicas. Read the three nodes
first; if the headroom is there, add a nodeSelector so build churn
lands on one known node. If it is not, the forge host (16 cores, 136
GB free, already running the CI image) is the fallback and this
becomes a systemd unit instead of a manifest. A one-measurement
question that should not be answered by trying it.


`CARGO_TARGET_DIR` is `/scratch/target`, an `emptyDir` — node-local
disk on whichever control-plane node the pod lands on. Measured on
train 49, one `cargo test --all-features` takes the forge host from
141 GB free to 67 GB: a cold build needs **~74 GB**. The workspace PVC
is 40 Gi and does not cover it; `emptyDir` draws on the node's own
filesystem, and no `nodeSelector` pins the pod, so it lands anywhere.

Nobody has measured cp-1/2/3's disk. The topology doc exists because
this class of thing is invisible until it bites, and this is the same
shape: a 74 GB requirement against an unmeasured floor, on the same
three nodes that hold the system of record's Longhorn replicas.

Proposed: **measure before applying, and pin the pod.** Read the three
nodes' free space, and if the headroom is there, add a `nodeSelector`
so the dev pod's build churn lands on one known node rather than
wherever the scheduler puts it. If it is not there, the forge host —
16 cores, 136 GB free, already running the CI image — is the fallback
and the manifest becomes a systemd unit instead. This is a
one-measurement question and it should not be answered by trying it.

### Q2: What is the access path, and what credential does it use? (resolved)

Resolved 2026-08-16 — accept.

kubectl on boss-gcp plus a kubeconfig scoped to the boss-dev namespace
only. Today there is no kubectl on boss-gcp or the forge host, the
Kubernetes API on the VIP answers a clean 401, only port 7900 is
exposed on 10.20.0.34, and boss-dev deliberately has no Service — it
is kubectl exec only. So even applied, nothing could reach it.
Namespace-scoping is the point: a development credential that can also
reach the `boss` namespace recreates the blast radius the Postgres
sidecar just removed by construction. boss-gcp is the right host
because it already holds the WireGuard path and is the box David
reaches through.


There is no `kubectl` on boss-gcp or the forge host. The Kubernetes
API on the VIP answers a clean `401` — reachable, no credential. Of
the cluster's ports only `7900` is exposed on `10.20.0.34`. And
`boss-dev` deliberately has no Service: it is `kubectl exec` only.

So even applied, today nothing could reach it.

Proposed: **`kubectl` on boss-gcp plus a kubeconfig scoped to the
`boss-dev` namespace only.** Namespace-scoped is the whole point — a
development credential that can also reach the `boss` namespace
recreates the blast radius the sidecar just removed. boss-gcp is the
right host because it already holds the WireGuard path to the LAN and
is already the box David reaches through.

### Q3: What makes the session durable across a disconnect? (resolved)

Resolved 2026-08-16 — accept.

The durable unit is a multiplexed session inside the pod. `kubectl
exec -it` dies when the laptop closes, so the pod being Recreate +
PVC-backed is not enough — the workspace persists but the process does
not, and a long agent run is exactly what must survive a lid closing.
Start tmux in the pod and have exec attach and detach from it. Worth
stating explicitly because it is the whole difference between 'a dev
box in the cluster' and the thin-terminal-to-a-durable-session David
asked for.


"Thin terminal to a durable session" requires the session to outlive
the terminal. `kubectl exec -it` does not: close the laptop and the
exec dies with it. The **pod** is durable — `Recreate`, PVC-backed
workspace — but the shell inside it is not, and a long agent run is
exactly the thing that must survive a lid closing.

Proposed: **the durable unit is a multiplexed session inside the pod**
— tmux, started by the pod, that `kubectl exec` attaches to and
detaches from. The workspace already persists on the PVC; this makes
the *process* persist too. Worth stating explicitly because it is the
difference between "a dev box in the cluster" and what David actually
asked for.

### Q4: Does the durable session hold the forge credential? (resolved)

Resolved 2026-08-16 — accept.

Yes — a repo-scoped forge token mounted as a Secret. Three finished
cars are parked right now because the session cannot git push to the
forge; every push today needed David to run a one-liner on boss-gcp.
Note this is a much smaller grant than Q2's cluster credential and can
be decided separately: scoped to david/boss, push only, no admin. The
conductor already proves the pattern with /etc/boss-train/forge.token,
and the blast radius is a branch — every car still passes CI and the
gate before merging, so a bad push costs a red train, not a bad
deploy. Explicitly NOT the GitHub mirror credential, which stays
manual with your sign-off.


Three cars are parked right now because the session cannot
`git push` to the forge. Every push today required David to run a
one-liner on boss-gcp. If the session runs in-cluster it can hold a
repo-scoped forge token the way the conductor already does
(`/etc/boss-train/forge.token`), and cars publish themselves.

This is the highest-leverage question in the doc and also the one with
real risk, so it is worth separating what is being asked. A
**repo-scoped push token** is a much smaller grant than the cluster
credential in Q2, and it is the one that unblocks the pipeline.

Proposed: **yes, a repo-scoped forge token, mounted as a Secret.**
Scoped to `david/boss`, push only, no admin. The conductor already
proves the pattern and the blast radius is a branch — every car still
goes through CI and the gate before it merges, so a bad push costs a
red train, not a bad deploy. Explicitly NOT the GitHub mirror
credential, which stays manual with David's sign-off.

### Q5: What stops the next manifest from sitting unapplied? (resolved)

Resolved 2026-08-16 — accept.

A conformance check that every manifest in infra/cluster/manifests/ is
applied and current, in the same family as check-cluster-freshness.sh.
boss-dev.yaml is the evidence that 'merged' and 'running' are
different states with nothing watching the gap — it landed on train 36
on 2026-08-15, no script applies it, and to anyone browsing the repo
it reads as 'we have a dev environment'. That is the general form of
this doc's complaint: the tree is not the system, and the only honest
way to keep them together is a check that fails when they part.
Unanswerable until Q2 lands, since the check needs the same credential
a human would.


`boss-dev.yaml` is the evidence that "merged" and "running" are
different states with nothing watching the gap. `deploy-services.sh`
installs systemd units on boss-gcp; nothing owns
`infra/cluster/manifests/`. The deploy step of every train reports
`services: prod; web: deployed` and says nothing about the cluster.

Proposed: **a conformance check that every manifest in
`infra/cluster/manifests/` is applied and current**, in the same
family as `check-cluster-freshness.sh`. This is the general form of
the whole doc's complaint: the tree is not the system, and the only
honest way to keep them together is a check that fails when they
part. Note this question is unanswerable until Q2 lands — a check
needs the same credential a human would.

## What this is not

Not a second CI runner. `boss-dev.yaml` already declines that and the
reasoning stands: share the image, not the job.

Not a migration of the conductor or the cadence loop. Those run on
boss-gcp against the cluster's jobs API and, as of today, correctly.
This doc is about where *development* happens, not where the train
runs.

## Decision history

Reviewed as packet `9abd4ad5` on 2026-08-16; the packet carried the
prose and the questions, so this file is the residue rather than the
precondition. Answers verbatim:

**Q1 — Does the cluster have the disk for a session's build?**

Measure before applying, and pin the pod. A cold `cargo test --all-
features` needs ~74 GB of target/ (measured on train 49: forge host
went 141 GB free to 67 GB). That lands in /scratch, an emptyDir on
whichever control-plane node the scheduler picks — the 40 Gi workspace
PVC does not cover it, and no nodeSelector pins it. cp-1/2/3's free
space has never been measured, and those are the same three nodes
holding the system of record's Longhorn replicas. Read the three nodes
first; if the headroom is there, add a nodeSelector so build churn
lands on one known node. If it is not, the forge host (16 cores, 136
GB free, already running the CI image) is the fallback and this
becomes a systemd unit instead of a manifest. A one-measurement
question that should not be answered by trying it.

**Q2 — What is the access path, and what credential does it use?**

kubectl on boss-gcp plus a kubeconfig scoped to the boss-dev namespace
only. Today there is no kubectl on boss-gcp or the forge host, the
Kubernetes API on the VIP answers a clean 401, only port 7900 is
exposed on 10.20.0.34, and boss-dev deliberately has no Service — it
is kubectl exec only. So even applied, nothing could reach it.
Namespace-scoping is the point: a development credential that can also
reach the `boss` namespace recreates the blast radius the Postgres
sidecar just removed by construction. boss-gcp is the right host
because it already holds the WireGuard path and is the box David
reaches through.

**Q3 — What makes the session durable across a disconnect?**

The durable unit is a multiplexed session inside the pod. `kubectl
exec -it` dies when the laptop closes, so the pod being Recreate +
PVC-backed is not enough — the workspace persists but the process does
not, and a long agent run is exactly what must survive a lid closing.
Start tmux in the pod and have exec attach and detach from it. Worth
stating explicitly because it is the whole difference between 'a dev
box in the cluster' and the thin-terminal-to-a-durable-session David
asked for.

**Q4 — Does the durable session hold the forge credential?**

Yes — a repo-scoped forge token mounted as a Secret. Three finished
cars are parked right now because the session cannot git push to the
forge; every push today needed David to run a one-liner on boss-gcp.
Note this is a much smaller grant than Q2's cluster credential and can
be decided separately: scoped to david/boss, push only, no admin. The
conductor already proves the pattern with /etc/boss-train/forge.token,
and the blast radius is a branch — every car still passes CI and the
gate before merging, so a bad push costs a red train, not a bad
deploy. Explicitly NOT the GitHub mirror credential, which stays
manual with your sign-off.

**Q5 — What stops the next manifest from sitting unapplied?**

A conformance check that every manifest in infra/cluster/manifests/ is
applied and current, in the same family as check-cluster-freshness.sh.
boss-dev.yaml is the evidence that 'merged' and 'running' are
different states with nothing watching the gap — it landed on train 36
on 2026-08-15, no script applies it, and to anyone browsing the repo
it reads as 'we have a dev environment'. That is the general form of
this doc's complaint: the tree is not the system, and the only honest
way to keep them together is a check that fails when they part.
Unanswerable until Q2 lands, since the check needs the same credential
a human would.

