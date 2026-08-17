# Design: the infrastructure view — subjects, and the packets about them

**Status**: approved — every question answered in packet `3ddf33ed`; carried to a file 2026-08-17.

**Origin**: David, 2026-08-16: *"Can we get a visual of how the
Infrastructure rollout works and the current monitoring of it as a new
IT page?"* then, refining: *"it is just visualizing the subjects that
compose infrastructure along with the network activity in job packets
flowing across them"* and *"Or rather, when the subject is about them?
Maybe this is a slightly new visual."*

**Related**: [bossnet-physical-topology.md](./bossnet-physical-topology.md)
(Q1 proposes the Subject kinds this page would render; still open) ·
`/it/map` (the station map — the LOGICAL network)

---

## What the data says today

Across 351 open packets of infrastructure-ish kinds
(`maintenance-*`, `ship-a-change`, `pr-train`, `ad-hoc`, `incident`,
`repair-a-train`, `regenerate-deployment`) there are **204 distinct
subjects**, and the shape is not what the page needs:

```
 32  custom/main                 8  custom/infra
 19  custom/crates/core/boss-jobs 6  custom/infra/train
 16  custom/docs/design           5  custom/crates/core/boss-docs
 10  custom/crates/orchestrators/boss-cli
  4  custom/bossinfra             3  custom/infra/gate.sh
```

They are **repo paths**, near-universally, with a long tail of 204.
"The subject is about infrastructure" is already true and already
recorded — it just means *a place in the tree*, not a machine, a
service, or a deploy target. A page keyed on it today would draw a
treemap of the codebase.

The kinds that would make it draw the estate — `node`,
`service-instance` — are proposed in the topology doc's Q1 and have
never been answered. This page is that question's payoff, which is
worth saying plainly: **it cannot be built well before Q1 is
decided.**

## What IS available now

The rollout half of the original ask needs no new Subject kinds,
because the pipeline already keeps its own record:

- `pr-train` packets carry `collect` / `assemble` / `pr` / `ci` /
  `merged` / `deployed` steps, each with metadata — the boarded cars,
  the train ref, the PR url, the CI verdict, the deployed sha.
- `ship-a-change` cars carry their branch and the train that took them.
- **`maintenance-*` packets, as of this evening, carry timer health.**
  Eleven chores on boss-gcp now open a Job on each run and complete it
  on success; a failure leaves the packet open. That is a live
  monitoring signal that did not exist this morning.

So "how the rollout works, and is it healthy" is renderable from
packets today. "What machines exist and what runs on them" is not.

## The thing tonight taught us that the page must show

**One commit takes two deploy paths, and nothing displays that.**

`main@bebef409` reached boss-gcp through the conductor's `deployed`
step, and reaches the cluster separately through
`cluster-deploy-runner.timer` on the forge host, which polls forge main
every ten minutes and builds. Tonight I read `/api/stations` against
the cluster, saw the pre-deploy number, and nearly reported a shipped
feature as broken — because the train said "deployed" and the
deployment I was querying had not rolled.

A rollout view whose only claim is "the train deployed" would have told
me the same wrong thing. Whatever this page becomes, it has to show
**per target**: which commit, how it got there, and how stale it is.

## Open questions

### Q1: Do the infrastructure Subject kinds finally get created? (resolved)

Resolved 2026-08-16 — accept.

Yes, and let this page be the forcing function. `node` and `service-
instance` were proposed in the topology doc's Q1 and the question has
sat open; meanwhile 204 distinct 'infrastructure' subjects exist and
they are almost all repo paths, so a page keyed on them draws the
codebase rather than the estate. A design that stays open produces
nothing; a design with a surface waiting on it gets answered.
Concretely: `node` (address, role, declared cpu/mem/disk) and
`service-instance` (the service/node/environment triple, its port, its
database, and whether it is AUTHORITATIVE for that data). That last
bit is what would have made tonight's two-deployments confusion a fact
on a page rather than folklore.


The topology doc proposed `node` and `service-instance` and the
question has sat open since. Without them this page draws code paths;
with them it draws the estate and the code paths become a different
(also useful) view.

Proposed: **yes, and let this page be the forcing function.** A design
that stays open produces nothing; a design with a surface waiting on it
gets answered. Concretely: `node` (address, role, declared cpu/mem/disk)
and `service-instance` (the service/node/environment triple, its port,
its database, and whether it is authoritative for that data). The last
bit is what would have made tonight's two-deployments confusion a fact
on a page rather than folklore.

### Q2: Is this one page or two? (resolved)

Resolved 2026-08-16 — accept.

Two, and build the rollout first. The ask contains two visuals that
share a topic and almost no layout: the ROLLOUT (car, train, CI,
merge, two deploy paths, targets) and the ESTATE (nodes and instances,
with the packets about each). The rollout needs no new Subject kinds -
pr-train packets already carry collect/assemble/pr/ci/merged/deployed
with metadata, and as of tonight eleven maintenance chores carry timer
health - so it can ship while Q1 is decided. The estate view then
lands on real `node` Subjects instead of a hardcoded diagram, which is
the difference between a page and a picture.


The ask contains two visuals: the ROLLOUT (a pipeline — car, train, CI,
merge, two deploy paths, targets) and the ESTATE (nodes and instances,
with the packets about each). They share a topic and share almost no
layout.

Proposed: **two, and build the rollout first.** It needs no new Subject
kinds, it answers the operational question that actually bit us tonight
("is what I am looking at current?"), and it can ship while Q1 is
decided. The estate view then lands on top of real `node` Subjects
instead of a hardcoded diagram — which is the difference between a page
and a picture.

### Q3: Where does live state come from — packets only, or observations? (resolved)

Resolved 2026-08-16 — accept.

Packets for history, a thin per-target probe for current state.
Packets say what the system DID (this train deployed this sha); they
do not say what a target IS right now, and that gap is exactly
tonight's confusion - I read /api/stations against the cluster, saw
the pre-deploy number, and nearly reported a shipped feature as broken
because the train said 'deployed' and the deployment I queried had not
rolled. The topology doc's Q3 already proposed per-node reporters
pushing observations rather than a central poller. This page consumes
those. Until they exist the honest v1 renders packet history and
labels current state 'unknown' rather than inferring it: a stale
number presented as live is worse than a blank.


Packets say what the system *did*: this train deployed this sha. They
do not say what a target *is* right now, and the gap between the two is
exactly tonight's confusion.

Proposed: **packets for history, a thin per-target probe for current
state.** The topology doc's Q3 already proposed reporters pushing
observations rather than a central poller, for good reasons (no
credential to every node). This page consumes those. Until they exist,
the honest v1 renders the packet history and labels current state
"unknown" rather than inferring it — a stale number presented as live
is worse than a blank.

### Q4: Does this replace `/it/map`? (resolved)

Resolved 2026-08-16 — accept.

No - new page, and cross-link. /it/map is the station map: the logical
network, packets across queues. This is the physical substrate. They
are the two halves the three-layers reading names, and conflating them
loses both. /it/map answers 'where is work queued'; this answers 'what
is it running on, and is that current'. A station and a node are
different objects and the architecture reads clearer for keeping them
apart on screen.


`/it/map` is the station map: the logical network, packets across
queues. This is the physical substrate. They are the two halves the
three-layers reading names, and conflating them would lose both.

Proposed: **new page, and cross-link.** `/it/map` answers "where is
work queued"; this answers "what is it running on, and is that
current". A station and a node are different objects and the
architecture is clearer for keeping them apart on screen.

## Decision history

Reviewed as packet `3ddf33ed` on 2026-08-16; the packet carried the
prose and the questions, so this file is the residue rather than the
precondition. Answers verbatim:

**Q1 — Do the infrastructure Subject kinds finally get created?**

Yes, and let this page be the forcing function. `node` and `service-
instance` were proposed in the topology doc's Q1 and the question has
sat open; meanwhile 204 distinct 'infrastructure' subjects exist and
they are almost all repo paths, so a page keyed on them draws the
codebase rather than the estate. A design that stays open produces
nothing; a design with a surface waiting on it gets answered.
Concretely: `node` (address, role, declared cpu/mem/disk) and
`service-instance` (the service/node/environment triple, its port, its
database, and whether it is AUTHORITATIVE for that data). That last
bit is what would have made tonight's two-deployments confusion a fact
on a page rather than folklore.

**Q2 — Is this one page or two?**

Two, and build the rollout first. The ask contains two visuals that
share a topic and almost no layout: the ROLLOUT (car, train, CI,
merge, two deploy paths, targets) and the ESTATE (nodes and instances,
with the packets about each). The rollout needs no new Subject kinds -
pr-train packets already carry collect/assemble/pr/ci/merged/deployed
with metadata, and as of tonight eleven maintenance chores carry timer
health - so it can ship while Q1 is decided. The estate view then
lands on real `node` Subjects instead of a hardcoded diagram, which is
the difference between a page and a picture.

**Q3 — Where does live state come from - packets only, or observations?**

Packets for history, a thin per-target probe for current state.
Packets say what the system DID (this train deployed this sha); they
do not say what a target IS right now, and that gap is exactly
tonight's confusion - I read /api/stations against the cluster, saw
the pre-deploy number, and nearly reported a shipped feature as broken
because the train said 'deployed' and the deployment I queried had not
rolled. The topology doc's Q3 already proposed per-node reporters
pushing observations rather than a central poller. This page consumes
those. Until they exist the honest v1 renders packet history and
labels current state 'unknown' rather than inferring it: a stale
number presented as live is worse than a blank.

**Q4 — Does this replace /it/map?**

No - new page, and cross-link. /it/map is the station map: the logical
network, packets across queues. This is the physical substrate. They
are the two halves the three-layers reading names, and conflating them
loses both. /it/map answers 'where is work queued'; this answers 'what
is it running on, and is that current'. A station and a node are
different objects and the architecture reads clearer for keeping them
apart on screen.

