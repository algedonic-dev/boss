# The factory taxonomy, v2: eight primitives and a station tree

**Status**: decided. David proposed the eight primitives on 2026-11-06,
accepted the handoff as the direction at 18:50Z, approved the station-tree
bundle at 19:44Z, and accepted every open question's proposal at 20:24Z
(design 4b27a65a). This supersedes the table, the mechanisms M1–M6 and the
cars T0–T5 of [factory-taxonomy.md](factory-taxonomy.md) (v1, decided
2026-11-02). v1's census and its diagnosis ("the vocabulary is unsorted")
still hold, and v1 stays in the tree until the retirement cars fold it.

**Reading frame**: [the-three-layers.md](the-three-layers.md) stands. The
network is the substrate, the fat protocols dictate the operating model,
the actors run it. v2 changes the set of things that live in those layers.

**The tenant's tree**: [station-tree.md](station-tree.md) applies this
document to Algedonic: which stations exist, where they sit, what they
reserve, who owns which protocol.

---

## The eight primitives

A software factory is a system that produces software. These eight things
make it up. Everything else is a rule, a derived state or a UX label.

### 1. Resource pool
A pool represents a constraint. Capacity in a pool is either **reserved**,
which is held by default (the VM running the front door, a department's
standing share), or **floating**, which is drawn on demand (a coding
agent's build space). At its smallest a pool is a budget number. A real
pool is the provisioned hardware as virtualized to BOSS, and each piece of
hardware is one of the pools.

A pool has one of three shapes. Shape is an attribute of the pool, not a
kind of its own:
- **capacity**: vCPU, RAM, slots. Released when the holder releases.
- **stock**: disk by consumer, credentials. Consumed, and recovered by a
  protocol (reclaim), not on release.
- **rate**: dollars per hour, tokens per minute. Spent, never released.

Executors are pool capacity too (R3): people by headcount within a role,
robots by token use or budget.

### 2. Job packet
The unit of production. A packet is defined only by the protocol it was
created under, and is made of job steps. It stays pinned to that protocol
version for its whole life.

### 3. Job step
The unit of work. A step is defined only by the packet's protocol, and a
packet is worked one step at a time. A step names what it needs, a
**station spec** (executor kind, machines, resources, where in the tree),
and ends with its completion test.

### 4. Station
Where an executor works a job step. **A station is defined by its
allocation**: the pools it holds, drawn from its parent, plus the machines
provisioned into it and the executors it can seat. The root station (the
company) holds every constraint in the system; for Algedonic that is the
hardware estate and one human. Any station can create a station beneath
it with a given allocation, provided no constraint above it is violated.

A station is either **standing**, instantiated at seed and kept (the forge,
the gates, a department), or **ad hoc**, instantiated for the work and
released afterwards (a build workspace). Every station has a stable id and
an **address**, its path in the tree. The record names the station by id,
and the address is derived from the tree as it stood at that sequence.
When no allocation is free, the work waits.

### 5. Executor
Something that can do the work of a job step: a **human**, a **robot**
(an LLM agent), or an **automaton** (deterministic software). An executor
claims a step at a station. There is no fourth kind.

### 6. Machine
Software provisioned into a station to support the work done there. A
machine can require resources, which raises its station's allocation. The
gate runner in a build station, the registry in the company station and
the dispatcher in any containing station are all machines. A machine never
claims a step. It serves its station and everything beneath it (R5).

### 7. Protocol
The exact instructions for the work: what is done, in what order, under
what conditions. A protocol is executable only when every step validates
(R10). Its defining feature is the **objective, unambiguous completion
test** at the end of every step, which holds the output inside tolerance
and certifies that the next step is ready. Protocols are versioned data,
and each is owned by a station.

### 8. Audit record
Every change, written down and never changed. v1 said "every job step
change". v2 widens that to every change to a packet, a step, a station or
a pool (R9), because replay needs routing, provisioning and reclaim. Every
screen is a projection of the record at a sequence.

---

## The rules

Each rule follows from the primitives, and each one is meant to become
something a lint or a test can refuse. The register entries that track
them are listed under each car below.

**R1. Stations form a tree, and the root holds the constraints.** Every
station except the root has exactly one parent. The root's pools are the
system's constraints (the estate, the roster, the budget). Every other
station's pools are allocations from its parent. A department is a
station and so is a workspace. The factory could itself be one station
inside a larger factory. The console's three screens are three altitudes
of one tree.

**A department is a tier, not a thing** (David, 2026-11-08, backlog
53abeb8d). "Department" is the name for the tier of stations directly
under the root, and nothing more: a department has no identity of its
own beside its station row. What it rolls up, what it is allocated and
the line its protocols run are that station's, read off `parent`,
`allocation` and its stations' specs. The departments registry
(`GET /api/departments`), the employee row's `department` field and the
`department` key on station and workflow rows are readers of that tier
until they retire. A car that touches one of those rows moves it to the
station's words in the same car, keeps the old key only where a live
reader still reads it (and names that reader in its excludes), and reads
a department as a tier-1 station. `infra/lint/rows-still-in-v1-words.sh`
counts the rows still in v1 words, so the retirement is a number that
falls.

**R2. An allocation never violates an ancestor.** A child's reserved
allocation is subtracted from its parent's floating capacity when the
child is created. A floating draw at claim time passes up to the nearest
ancestor that can satisfy it, and is bounded at every level it passes.
There is no double counting, because every allocation has exactly one
parent. When nothing can satisfy a draw, the step waits, and the record
names the constraint it is waiting on. Waiting is the only outcome of an
allocation that cannot be met; the system never over-allocates.

**R3. Executors are pool capacity.** `people:<role>` has a capacity read
from the roster and never set by hand. Today there is one human at the
root, so every human step on every floor waits on that one person.
`robots:<model>` has a rate (tokens per minute or dollars) and may have a
concurrency. Automata are bounded by the station that hosts them. A claim
is therefore one predicate: the step's station spec (executor, machines
and resources) can be satisfied at or under the named ancestor.

**R4. A pool's shape is an attribute.** capacity, stock or rate. Release
rules differ by shape; the primitive does not.

**R5. Machines serve their station and everything beneath it.** The
dispatcher at the root routes for every floor, and a department may
provision its own dispatcher to run its own discipline. Queue policy (v1's
"discipline") is the program of the containing station's dispatcher, not a
primitive. The dispatcher, the conductor, the sweeper and reclaim are
machines. v1's "automation" group no longer exists.

**R6. The step decides whether software is a machine or an automaton; the
binary does not.** If the step is "run the gates", the gate runner is the
automaton executing it. If the step is "build the change" and the gates
are its completion test, the same runner is a machine in the robot's
station. The taxonomy records the role, not the binary.

**R7. Any station with an executor can host a step.** A department
sign-off done by a person at the department altitude is a step at that
station. Containment is about allocation and machines, not about being a
leaf.

**R8. A step may be fulfilled by packets one altitude down.** A
company-level protocol's step "Engineering: build it" is received by the
Engineering station, which creates packets under whichever Engineering
protocols it chooses. The parent step's completion test is the contract
across the boundary, and the child packets stay pinned to their own
protocol versions. A train works this way too: it is a packet whose step
consumes other packets. R8 is also how protocols are organized: each one
lives at the altitude of the station that runs it.

**R9. The record covers packets, steps, stations and pools.** Every
claim, release, route, allocation, station release, reclaim and wait is an
event, and each names its station by id. Every screen is a projection of
the record at a sequence, and no client adds anything up.

**R10. A protocol is executable only when it validates.** Every step's
station spec must name an executor kind the factory knows, machines it can
provision, pools that exist in the tree at or above the named ancestor,
and a completion test it can run. Validation checks that things exist, not
that capacity is free; capacity is checked at claim (R2). The lint that
refuses an undeclared pool also refuses an unknown executor kind, an
unknown machine, or a step with no test.

**R11. Events rise and reactions descend.** A step completing at a station
is written to the record. Machines in the containing stations react by
routing, instantiating an ad hoc station, forming a train or reclaiming,
and each reaction is itself recorded (R9).

**R12. There are two kinds of failure.** A robot fails as rework of the
product. A station with machines fails either as a **product defect** (a
red gate) or as a **machine fault** (a dead runner, a disk floor), and the
record never writes one as the other.

---

## Station specs, addresses and resolution

A step's requirement is a **station spec**: `{ under: <station>, executor:
<kind|role|model>, machines: [...], resources: {...} }`. A standing station
is a spec instantiated at seed and kept. An ad hoc station is the same
spec instantiated on demand. The line is the sequence of station specs in
the protocol, instantiated against the tree as the packet moves.

**Addresses** are paths derived from the tree: the company's root, a
department beneath it, a standing station beneath that, an ad hoc
workspace beneath that. Rows hold the tree (`parent`, `owner`,
`under`/`at`, `station`) and the record names stations by their stable
id. **Code never names an address**: the vocabulary lint refuses a literal
tenant address in `crates/` and `apps/` (T4), so a reorganization stays a
registry edit.

**Resolution** is done by the dispatcher of the containing station, in
this order:
1. An existing station at or under `under` that matches the spec and has
   free capacity: claim there.
2. If there is none, but the parent can allocate the spec without
   violating R2: instantiate an ad hoc station, then claim.
3. If neither works, the step waits, and the wait names the specific
   constraint (a pool, a machine, an executor kind). The constraint waited
   on most at each altitude is the **binding constraint** the console
   shows.

Resolution never creates a standing station; standing stations are
declared. An ad hoc station is released when its last claim releases (Q4).

---

## What v1 became

| v1 | v2 |
|---|---|
| Packet, Step | Job packet, Job step (unchanged) |
| Protocol | Protocol (unchanged); a completion test per step is its defining feature |
| Check, inline | the step's completion test, run by a machine in the station or by the executor |
| Check, equipment-hosted | the step's completion test, run by a machine in a standing station |
| Contract | the completion test of the previous step (R8 makes it the cross-boundary contract too) |
| Discipline | the program of the containing station's dispatcher (R5) |
| Person, Robot | Human, Robot |
| Machine (executor kind) | **Automaton** |
| Generic station | a station spec without machines (ad hoc, or standing) |
| Equipment station | a standing station with machines |
| Pool (flow) | pool, shape = capacity |
| Stock | pool, shape = stock |
| Bays | a standing station's capacity (how many steps it seats at once) |
| Estate | the root station's pools (the hardware, virtualized) |
| Record | Audit record, widened (R9) |
| Admission | the event that creates a packet under a protocol at a station |
| Automation (conveyance, maintenance, yard) | machines of the containing stations (R5) |
| Department, floor | a station in the tree (R1); the UX draws an altitude |
| Eligibility (capability + reservation + bay) | resolution of a station spec (one predicate) |

## BOSS words in v2

| today | becomes |
|---|---|
| actor, executor (umbrella) | **executor**; `actor` survives in the three-layers sentence and in the policy vocabulary |
| agent (registry row) | **robot** executor; `agents` stays the table name |
| person, human, employee-as-executor | **human** |
| machine, runner, ops-runner, gate-runner (as executors) | **automaton** when claiming a step; **machine** when provisioned into a station |
| machinery, equipment | the **machines** of a standing station |
| crew | the robots on a floor; a UX label only |
| region, territory | **stations** in the tree; `region` stays the API word until the floor reads station rows |
| batch station (dock) | a standing station whose machine is the conductor; `batch` is already deleted (item 0a78f8f8) |
| gate (region / gate-run / gate.sh / verdict) | standing station / packet on it / machine program / product fact |
| capacity, bound, bays | station capacity; pool capacity |
| mover | retired |
| automation (regions family) | machines of the root station |
| department | a station |
| dispatcher, conductor, sweeper, reclaim | machines |
| `waiting_on` (console D3) | the constraint named by resolution step 3 |

---

## Where main stands (reconciled 2026-11-06)

The proposal's repo facts were older than it said. The conductor checked
them against main 18d8525c and the estate registry on 2026-11-06
(`conductor_check_20261106_1855Z` on design 4b27a65a), and these are the
facts the cars build on:

- `batch` is already deleted. The dock is an equipment row
  (`automation:train-conductor`, one bay), and the old batch rows are
  `group` rows (item 0a78f8f8).
- `infra/platform/stations/` holds 12 rows, and the live registry serves
  about 25 once generated rows are counted. The proposal's count of 7 was
  out of date.
- The gate capacity is **2**, the `bays` of the `gates` row, and David
  keeps it at 2 until the new drive is measured. `gate_max_concurrent` is
  gone from the delivery policy; only `ci_host_floor_gb` (40) remains
  there.
- `infra/platform/pools/` already holds 9 rows under v1 names
  (`vcpu`, `ram_gb`, `disk:*`, `model-slot:*`). The tree's names follow
  in T2+T5. `model-slot:<class>` publishes first and becomes
  `robots:<model>` in T5 (David, 19:28Z).
- The estate registry (`GET /api/estate/nodes`) names the boxes:
  `boss-gcp`, `forge`, `cp-1`, `cp-2`, `cp-3`, `w-1` and `w-2`. A host
  pool takes the registry id (`host:w-1`, never a name the proposal made
  up), and the Dell joins as its own pool when it arrives (11-09, holding
  PostgreSQL).
- Gates and builder workers already run as Kubernetes Jobs on `w-1`, a
  cluster worker. "Until the cluster builder image lands" is already true.
- These still hold: `REGIONS` is a const of 11, `transit.ts` lays out the
  yard by hand, 74 of 78 workflows carry `department = "it"`, `pr-train`
  is unsplit, `roll-a-train` and `keep-the-estate` do not exist, and there
  are about 46 automations.
- `ExecutorKind` already exists in `boss-core` from v1 (backlog 3ff2ef60),
  as `person | robot | automaton`. `agents.executor_kind` exists with
  `robot` as its default. Every claim through the per-step door and
  through claim-next records `_executor_kind` on its fact.
- The two failure facts already exist from v1 (item c02fc3b3):
  `equipment_fault` and `product_defect` on every gate-run's
  `record-verdict` step, read by the merge, the alert and auto-cancel
  through `boss_jobs::failure_facts::recorded`.

---

## The cars

David's bundle decision (20261106_1944Z) carries T0, T1, T2+T5, T3, T4 and
T7's first drawing as **one integration car**, with one rule: **additive
only**. New fields and rows only. Nothing old is removed, the old map,
station kind and bays keep working, and an old build reads every new row
(the rollback pin). Retirement is a later car. Not in the bundle: T6
(resolution, live routing), the `pr-train` split, `roll-a-train`, timers,
automations, and the rename to `robots:<model>`.

**Expand now, contract later.** Where v2 renames a word the record
already carries (`person` to `human`, `equipment_fault` to
`machine_fault`), the bundle teaches every reader the new word and keeps
writing the old one. The last-converged build that a watchdog rollback
lands on refuses a word it does not know, so the writer flips only in the
retirement car, after a build that reads both has converged.

**T0. This document and [station-tree.md](station-tree.md)**, with
`CLAUDE.md` §Primitives pointing at them. Five register entries are filed
in `docs/invariants/` as `unenforced` and flip as each car lands:
`one-parent-one-root`, `child-allocation-never-exceeds-parent`,
`a-protocol-names-only-its-owners-subtree`,
`a-machine-fault-is-not-a-product-defect` and
`every-executor-is-human-robot-or-automaton`.

**T1. `ExecutorKind` = `human | robot | automaton`** in `boss-core`.
`agents.executor_kind` is backfilled `robot` (by the column's default),
the employee row is `human`, and every machine identity that claims a step
signs as `automaton` (`automation:<name>`). The claim records the kind.
Test: every executor id in the record resolves to exactly one kind.

**T2 + T5. Stations become a tree, and the pools registry follows it.**
Station rows gain a stable `id`, `parent`, `mode` (standing or ad hoc),
`capacity`, `machines` and `allocation`; the address is derived, never
stored. Pools gain `shape` (capacity, stock or rate) and an owner, with
`host:<registry id>` from the estate observer, `people:<role>` from the
roster and `robots:<model>` from the budget.

**T3. Two failure facts** on any run at a station with machines, a
machine fault and a product defect as separate fields, read by the merge,
the alert and auto-cancel. Register entry:
`a-machine-fault-is-not-a-product-defect`.

**T4. A vocabulary lint with a cutover stamp.** New files may not use
`mover`, `territory`, `machinery`, `equipment` as a station kind, `bay`,
`automation` as a group, or a literal tenant address in `crates/` or
`apps/`. Old files are renamed as they are touched, never in a sweep. The
mechanism extends `infra/lint/factory-vocabulary.sh`.

**T6. Station specs on steps, and resolution in the dispatcher.** The
step's `resources` block (console D1) becomes a `station` spec (`under`,
`executor`, `machines`, `resources`). The dispatcher implements the
three-step resolution, and the claim event records the resolved station
or, when the step waits, the named constraint (replacing console D3's
`waiting_on`). A step may carry `fulfilled_by: <protocol>` (R8). Trains
are re-expressed this way.

**T7. The floor speaks v2.** One station card, drawn recursively for
stations and constraints, AND jobs visibly moving between stations (David,
19:37Z: the prototype captures the first and not the second). The
existing motion (LiveMotion, RouteLayer, TransitMap) moves onto tree
positions, and the transit map is not retired until the new floor shows
movement.

Order: T0, T1, T2+T5, T6, T3, T4, automations, T7, timers (design
4b27a65a, `station_tree_cars_20261106_1928Z`). The bundle ships the first
six of these together, except T6.

---

## Decided questions

David accepted every proposal on 2026-11-06 at 20:24Z.

**Q3. What is the completion test of a step fulfilled by packets?** The
parent step declares its own test over the children's outputs, not "all
children done". The boundary contract is explicit, and the receiving
station can choose or change its protocols without the parent noticing.

**Q4. Does an ad hoc station live for one claim or one packet?** For one
claim, by default. A protocol may mark a spec `keep: packet` so that
consecutive steps reuse a workspace. The allocation is then held across
the gap and shown on the floor.

**Q5. When does a rate pool charge?** An estimate is charged at claim, so
that a budget can refuse the claim, and it is trued up at release from the
record. The HUD shows both.

**Q6. Which ancestor satisfies a floating draw?** The nearest one that
can (R2), and the record names it, so the floor can show a department
running on the company's slack.

**Q7. Can a standing station's allocation change while it has claims?**
Upward, or downward to no less than what its open claims hold. The change
is an event like any other.

## Decision history

_v1 taxonomy decided 2026-11-02. On 2026-11-06 David proposed the v2
primitives: eight of them, stations at any altitude, departments that
receive high-level steps and create their own protocols' packets, and
stations defined by allocation, addressed, defined dynamically, with
waiting as the only outcome of a spec that cannot be met. The rules,
resolution and the mapping were drafted the same day. v1 Q1 and Q2 are
resolved by v2. Bundle approved at 19:44Z; Q3–Q7 decided at 20:24Z._
