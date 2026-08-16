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

### Q4: Does this absorb the reachability problem, or just describe it?

Modelling the topology does not make the cluster's people API
reachable. There is a real operational gap underneath — no kubeconfig
on any host an agent can reach — and a model that records "unreachable"
is honest but not useful.

Proposed: **fix the reachability first, and let the model record what
it finds.** Taking them in the other order builds an inventory whose
most important rows read "unknown", and an inventory with holes in
exactly the places that matter teaches people not to trust it.

The concrete gap is small: no host an agent can reach has a
kubeconfig, and the cluster exposes only `10.20.0.34:7900`. Either a
read-only kubeconfig on boss-gcp, or the same SSH tunnel treatment
7900 already gets for the handful of service ports worth reading,
closes it. The second is narrower and needs no new credential.

Worth stating plainly: this is the only question here that blocks the
others. Q3's reporters run ON each node and push outward, so they work
without it — but nothing can verify their claims against the cluster
until an agent can read the cluster.

### Q5: What is authoritative, and who decides?

The two-deployment split needs an answer before the model can record
one. Today the evidence says the cluster is authoritative for jobs
(David's packets land there) while boss-gcp runs the conductor and
dispatcher against its own database. That is not a configuration
anyone chose; it accumulated.

Proposed, and flagged as a recommendation on a call that is David's:
**the cluster is authoritative, and boss-gcp stops being a second
deployment.** Three reasons, in order of weight. The packets David
files land on the cluster, so it is already where the real work is.
The cluster has the durable shape — PVCs, a Recreate strategy that has
been reasoned about, backups. And boss-gcp is the smaller machine by
every measure that matters here: 4 cores against 16, and 20 GB of free
disk against 136 GB.

What that implies is bigger than a config flip and should be said out
loud rather than discovered: the conductor and the cadence loop run on
boss-gcp today and read boss-gcp's database. Pointing them at the
cluster is one drop-in — `BOSS_POSTGRES_URL` — but the unit file's own
header already warns that getting this wrong on a cutover box caused
split-brain incident c4b4a6b0, and that migration 123's reconcile must
land first or the boarding threshold silently halves.

The alternative worth naming: keep boss-gcp as a deliberate
second environment — a scratch or demo tenant — in which case nothing
moves, but the model must record it as NOT authoritative and the
conductor must still be pointed at the cluster it books trains for. A
second deployment is only a problem when nobody has decided it is one.

## Decision history

_None yet._
