# Design: the physical layer, virtualized — nodes and instances as data

**Status**: in-review — open questions tracked at `/system/design`.
**Origin**: David, 2026-08-16: *"I have wanted a view on the physical
infrastructure for a while, and it is because I figured we have
confusion in this area about exactly what resources we had under what
considerations. I know we have all the configs and things are actually
running, but it makes it almost impossible to foresee bottlenecks
without having the data available to BossNET."* Immediately preceded
by: *"Let's clean up our network topology and understanding... We want
a unified understanding even if we have physical nodes designated for
specific things."*
**Related**: [deployment-as-network.md](./deployment-as-network.md)
(how intent reaches a node) · [dev-cluster.md](./dev-cluster.md) ·
[the-three-layers.md](./the-three-layers.md) (this doc is about the
substrate layer) · `crates/core/boss-ports` (service → port, the one
piece of this that IS data today)

---

## The claim

BOSS models the brewery's physical world — locations, assets, vendors
— as Subjects, and reasons about them. It models **its own** physical
world nowhere. Every fact in the inventory below took an SSH session
to obtain, and none of it is reachable from inside BossNET.

That is not a documentation gap. A system that cannot see its own
resources cannot anticipate its own limits, which is the cybernetic
argument this repo is named for: the viable system needs a model of
itself, and capacity is part of that model.

## What is actually out there, measured 2026-08-16

Three physical nodes, two of them full BOSS deployments, and one
cluster of three whose pods float.

| node | address | cpu | ram | disk | free | role |
|---|---|---|---|---|---|---|
| boss-gcp | 34.45.110.40 | 4 | 15 GB | **48 GB** | **20 GB (59% used)** | full BOSS stack, 32 services, own Postgres + NATS; the train conductor |
| forge host | 10.20.0.15 | 16 | 30 GB | 228 GB | 136 GB (37% used) | Forgejo, container registry, CI runner |
| Talos cp-1/2/3 | 10.20.0.11–13, VIP .10 | — | — | — | — | the cluster; the `boss` pod, Postgres, NATS |

The cluster's declared shape, from `infra/cluster/manifests/`:

| workload | requests | limits | storage |
|---|---|---|---|
| `boss` (all-in-one pod) | 2 cpu, 4 Gi | 12 Gi mem | — |
| `postgres` | 500m, 1 Gi | — | 20 Gi PVC (RWO) |
| `nats` | 100m, 256 Mi | — | 10 Gi PVC (RWO) |
| `boss-auth` | — | — | 1 Gi PVC (RWO) |

No `nodeSelector` anywhere: pods float across cp-1/2/3, so "which
machine is the database on" has no answer in the tree — only in the
cluster's current state.

## The confusion is real and it has a number

**There are two complete BOSS deployments and they hold different
data.** Not a replica pair — two independent systems:

| | user-feedback packets | employees | roster contains |
|---|---|---|---|
| boss-gcp local Postgres | **66** | 411 | `emp-bootstrap-admin` only |
| cluster (`10.20.0.34:7900`) | **168** | not reachable | unknown |

boss-gcp's `jobs-api` reads `postgres://boss:boss@127.0.0.1/boss` and
holds 3124 wholesale-keg-orders of its own. The cluster holds the
packets David files.

This is the 2026-08-14 split-brain (`protocol-data-agrees-between-record-and-runtime`
in the invariant register) still live, and now visible as a topology
fact rather than a config bug: the conductor and dispatcher run on
boss-gcp against boss-gcp's database, while the packets they are about
live on the cluster.

It cost something concrete within an hour of being noticed: an agent
investigating "David cannot see his own feedback" read the roster from
boss-gcp, found no `emp-david`, and drew a root cause that had to be
retracted. The wrong database was reachable and the right one was not.

## Three bottlenecks nobody could have foreseen

Each was invisible until someone opened a shell, and each is the kind
of thing a capacity model surfaces before it bites.

1. **boss-gcp has 20 GB of disk free on a 48 GB volume** while running
   Postgres, NATS and 32 services. Nothing watches it. This is the
   same shape as the failure below, on a smaller disk.
2. **A cold CI job needs ~74 GB.** Measured on train 49: the forge
   host went 141 GB free → 67 GB during one `cargo test
   --all-features`, and back. On 2026-08-14 a crashed job left a 63 GB
   volume behind, which left 74 GB free — less than the next job
   needed. Two trains died, and the symptom was four unrelated
   `boss-ledger` tests failing on `could not extend file`. Diagnosis
   took an hour of archaeology (`1b63456b`).
3. **The cluster's Postgres PVC is 20 Gi, ReadWriteOnce.** RWO is
   already load-bearing elsewhere — it is why the `boss` Deployment
   must stay `Recreate` (`single-pod-deploy-requires-recreate`). No
   surface reports how full it is.

## The shape of an answer

Model the substrate the way the tenant's world is modelled: as
Subjects with attributes, in registries, readable through the same
API as everything else.

- A **node** Subject — the machine. Address, role, and its *declared*
  capacity: cpu, memory, disk. Physical designation is welcome; David
  asked for a unified understanding *even though* nodes are
  purpose-specific, and a `role` attribute is how a node says what it
  is for without fragmenting the model.
- A **service instance** Subject — the (service, node, environment)
  triple. Which port, which database, and one bit that ends the
  confusion above: **is this instance authoritative for its data?**
  `boss-ports` already answers service → port; this is the missing
  service → node → database → authority.
- **Observations** as events, not columns. Free disk is a
  measurement with a timestamp, so it belongs in the log like every
  other fact, with the current value a projection. That is what makes
  a trend — and a trend is what "foresee bottlenecks" actually needs.

Two things fall out almost free once it exists. A conformance check
can assert that exactly one instance claims authority for a given
dataset, which is the split-brain becoming detectable instead of
folklore. And the locomotive's disk check
(`infra/forge/locomotive.sh`) stops being a hardcoded 70 GB threshold
and starts reading the node's declared headroom.

## What this is not

Not a monitoring system. Prometheus-style time series for CPU
utilisation is a different job with different tools, and BOSS should
not grow one. The claim here is narrower: the **inventory** and the
**capacity commitments** are operational facts of the same kind as a
vendor or a location, and the system that plans work should be able
to read them.

Not an excuse to collapse the topology. Three nodes with distinct
roles is a reasonable shape. The problem is that the shape is only
knowable by SSH.

## Open questions

### Q1: Node and service-instance — new Subject kinds, or Classes of `asset`?

`asset` already exists as a Subject kind with a KB view, and a server
is an asset in the ordinary sense. Reusing it costs no new crate and
inherits the existing surfaces; the Class registry already carries
taxonomies like this one. Against: an `asset` in this tenant means
brewery equipment, and mixing "fermenter FV-3" with "cp-2" in one kind
makes every asset view ask which sort it is looking at — the same
complaint that made `refurb-device` its own thing.

Proposed: **new `node` and `service-instance` Subject kinds.** The
deciding argument is that these are BOSS's own substrate, not the
tenant's inventory: every deployment has them regardless of what it
models, which is the test for whether something belongs in
`crates/core/`. A brewery's fermenters and BOSS's servers are the same
noun only by coincidence of English.

### Q2: Where does the inventory live — the tree, or the database?

The manifests and `boss-ports` are in the tree, which makes them
reviewable and versioned; the running truth is in the cluster, which
makes it current. A node's declared capacity could reasonably live in
either.

Proposed: **declared capacity in the tree, observed state in the log.**
Intent is versioned and reviewable (`deployment-as-network`'s own
split: intent versioned, derived state reconverged), so "cp-2 is
supposed to have 200 GB" is a repo fact that arrives through a car.
"cp-2 has 41 GB free right now" is an observation, and observations
are events. The two disagreeing is then a finding rather than a
mystery — which is exactly the check that would have caught the 63 GB
orphan.

### Q3: What produces the observations, and how often?

Nothing in BOSS reaches the nodes today, and the cluster's services
are not reachable from boss-gcp at all — during this investigation
`10.20.0.34` answered on 7900 and nothing else, no `kubectl` exists on
either reachable host, and the cluster's people API could not be read.
So an agent cannot currently gather what this design wants to store.

Proposed: **a reporter per node, pushing to an endpoint, on a
cadence.** Not a central poller: a poller needs credentials and a
route to every node, which is the thing that does not exist and is
also the thing worth not building. A small unit on each node that
reads `df`, `free`, `nproc` and posts them inverts the dependency —
each node needs one outbound path, and the nodes BOSS cannot reach
are exactly the ones that would otherwise stay invisible. Frequency
should be minutes, not seconds; this is capacity planning, not
alerting.

### Q4: Does this absorb the reachability problem, or just describe it? (resolved)

Resolved 2026-08-16 — override. The question assumed a reachability
problem; measurement found a CREDENTIALS problem, which is a
different and much smaller thing.

The network path is fine. boss-gcp reaches the LAN over WireGuard,
and the Kubernetes API on the VIP `10.20.0.10:6443` answers — a clean
`401 Unauthorized`, so the cluster is listening and simply does not
know the caller. What was missing on boss-gcp was a credential: no
kubeconfig in `~/.kube`, `~/.talos` or `/etc/kubernetes`, and no
`kubectl` binary there or on the forge host.

> **Corrected 2026-08-16.** "No kubeconfig anywhere" was never true,
> and this paragraph carried the error for two days. An admin
> kubeconfig has been sitting at `~/talos-homelab/v2/kubeconfig` on
> David's laptop the whole time, with `kubectl` and `talosctl` both
> installed — the laptop reaches the VIP directly. The survey above
> checked boss-gcp and the forge host, found nothing on either, and
> generalised to "nowhere"; nobody checked the machine the survey was
> being run from.
>
> That error had a cost beyond this file. It is why the dev pod was
> believed unreachable, why verification kept happening on the laptop
> instead of in the CI image, and therefore why train 52 went red on
> `bunx` — a binary present on a Mac and absent from the container.
>
> Since corrected: `kubectl` is installed on boss-gcp and a
> namespace-scoped credential for `boss-dev` lives at
> `/etc/boss-dev/kubeconfig`
> (`infra/cluster/manifests/boss-dev-access.yaml`). The admin config
> stays on the laptop. Of the cluster's
service ports only `7900` is exposed on `10.20.0.34`; 4443, 8080, 443
and 80 are closed, as is 443/4443 on the VIP.

So the fix is one of two small things rather than a networking
project. A read-only kubeconfig plus `kubectl` on boss-gcp is the
general answer and is what Q3's reporters would verify themselves
against. Extending the existing SSH tunnel is the narrow one — it is
already `-L 7900:10.20.0.34:7900`, so another service is one more
`-L` and no new credential exists to leak.

The concrete thing this blocked: whether an `emp-david` employee
row exists on the CLUSTER, which is what `classifyProbe` resolves a
session against and therefore whether My Day renders David's board or
the bare "no matching employee in the roster" line. boss-gcp's local
roster answers that question about the wrong database — 411 employees,
no `emp-david` — and reading it there is exactly the mistake that
produced a retracted root cause on packet e8665893.

### Q5: What is authoritative, and who decides? (resolved)

Resolved 2026-08-16 — accept. David: *"Cluster should be
authoritative."*

The cluster is authoritative. boss-gcp stops being a second
deployment of record.

The consequence is bigger than a config flip and is the reason this
is written down rather than just done. The conductor and the cadence
loop run ON boss-gcp and read boss-gcp's database — that is the
2026-08-14 split-brain, and it is why `cadence_firings` held 244 rows
locally against 0 on the cluster. Pointing them at the cluster is one
`BOSS_POSTGRES_URL` drop-in, and the unit file's own header warns what
happens when that is got wrong: incident c4b4a6b0. Migration 123's
reconcile must be on the cluster first, or the boarding threshold
silently halves.

Two things follow that are worth stating so they are not discovered.
The 66 user-feedback packets in boss-gcp's local Postgres are not
replicated anywhere; deciding the cluster is authoritative does not
migrate them, and somebody has to say whether they matter. And a
second deployment is only a problem when nobody has decided it is
one — if boss-gcp is to survive as a scratch or demo environment,
that is fine, but the model must record it as NOT authoritative and
the conductor must still book trains against the cluster.

## Decision history

_None yet._
