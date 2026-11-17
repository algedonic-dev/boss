# The Algedonic station tree and its allocations

**Status**: decided. David accepted the tree as the direction on 2026-11-06
at 18:50Z and every open question's proposal at 20:24Z (design 4b27a65a).
This applies [factory-taxonomy-v2.md](factory-taxonomy-v2.md) (eight
primitives, station tree, addresses, resolution) to the one tenant BOSS
runs: Algedonic, with one human and one estate. The structure is
deliberately arbitrary, and §8 says what keeps it cheap to change.

Reconciled with main and the estate registry on 2026-11-06: host pools
take the registry's ids (`GET /api/estate/nodes`: `boss-gcp`, `forge`,
`cp-1`, `cp-2`, `cp-3`, `w-1`, `w-2`), and the gates seat 2, the `bays`
of today's `gates` row. The proposal's `build-1` is `w-1` and its
`forge-1` is `forge` (`conductor_reading_20261106_2025Z`).

---

## 1. The principle, for this tenant

There are two producing departments with one boundary between them.

- **Engineering makes the change.** It takes work from scope to a merged,
  green, image-built train. It owns the forge as a *product* (the repo,
  the gates, the dock) and the workspaces where robots build.
- **Operations runs the software and the servers.** It owns the hosts,
  the cluster, the deploys and converges, the observers and sweeps, the
  credentials, the public edge, and the running BOSS instance itself. It
  supports the whole company: when any department needs a machine, a
  credential, a diagnosis or a repair, that request is an Operations
  packet.
- **The company holds the constraints and the mandates.** It holds the
  pools, the record, the registry, a dispatcher, and the protocols that
  cross departments. No infrastructure machine lives at the root.

The boundary is a contract: **Engineering hands Operations a train that
is merged and green with its image built, and Operations rolls it.** The
last step of `pr-train` is a `fulfilled_by` hand-off to Operations'
`roll-a-train`. Nothing in Engineering names a cluster node, and nothing
in Operations edits a branch.

A protocol names only stations in its owner's subtree. Work that crosses
a boundary is a hand-off, never a reference.

**A department is the tier of stations directly under `algedonic`**
(David, 2026-11-08, backlog 53abeb8d: "departments are just the name now
for the tier of stations under the Company station"). Engineering,
Operations and the executive are departments only because their rows
name the root as `parent`; a department has no separate identity, and
its roll-up, allocation and line are its station row's. The departments
registry (`GET /api/departments`) and the employee row's `department`
field are readers of that tier until they retire, as is the
`department = "it"` key that station and workflow rows still carry. A car
that touches such a row moves it to the station's words in the same car
and keeps the old key only where a live reader is named in its excludes;
`infra/lint/rows-still-in-v1-words.sh` counts what is left.

This is one cut, not the only possible cut. §8 is what makes it cheap to
be wrong about.

## 2. The tree

```
<root>                                       standing · the company
  pools      host:boss-gcp, host:forge, host:cp-1, host:cp-2, host:cp-3,
             host:w-1, host:w-2 (one per box, named by its estate id),
             people:platform-admin, robots:<model>
  machines   record, registry, dispatcher (company-altitude packets only),
             sweeper (lease expiry)
  protocols  backlog-item, approval, incident, learning, department-retro,
             protocol-retro, protocol-experiment, user-feedback,
             workflow-design, keep-the-estate (new)

  executive                                  standing · David's desk
    executor  people:platform-admin; every human step in the factory is
              hosted here by hand-off
    hosts     approval, scope sign-off, design-decided, landed-review,
              passkey-promotion, break-glass-enrolment, publish-request,
              operator confirmations

  engineering                                standing · makes the change
    alloc     most of host:w-1 (workspaces, gates); the repo services'
              footprint on host:forge
    machines  dispatcher (Engineering discipline: dock WIP 24, train
              window, dock depth)
    protocols ship-a-change, pr-train (through image-built, then the
              hand-off), gate-run, flight-a-change, emergency-merge,
              rerail-a-car, park-a-job, correct-the-record, design-doc,
              design-doc-review, doc-flatten, page-audit,
              publish-to-github, agent-run, agent-execution, work-session,
              workflow-design (fulfilment), maintenance-codebase-metrics,
              maintenance-protocol-drift

    forge                                    standing · the repo as a product
      machines  forgejo, oci-registry, ci-runner (host:forge itself is
                Operations' to keep)
      capacity  ci runners (measure)
    gates                                    standing · where completion tests run
      machines  gate-runner
      capacity  2 (today's gates row; stays 2 until the new drive is measured)
      alloc     from host:w-1: per-seat vcpu/ram × 2; disk above
                ci_host_floor_gb (measure)
    dock                                     standing · the loading dock
      machines  conductor, cadence timers (train-window,
                board-on-dock-depth, dock-refresh, reconcile)
      capacity  24 (advisory WIP, unchanged)
    ad hoc under engineering, by resolution:
      build/<ws>    ship-a-change.build: robot, vcpu/ram/disk, machines [gate-runner]
      scope/<ws>    robot, small
      proof/<ws>    agent-run / agent-execution: robot, model + budget_usd from the step
      session/<ws>  work-session

  operations                                 standing · runs the software and the servers
    alloc     host:cp-1..3, host:boss-gcp, host:w-2, and the observers'
              footprint on every box
    machines  dispatcher (Operations discipline: one converge at a time;
              the cadence of sweeps)
    protocols roll-a-train (new: image, converge, verified, rolled; today
              the back half of pr-train), regenerate-deployment, every
              maintenance-* except the two Engineering keeps, join-a-node,
              replace-a-disk, dns-zone-observation, ops-request,
              diagnostic-request, diagnostic-recovery-assessment,
              enroll-a-managed-agent, inspect-a-github-installation,
              runtime-roster-capture, rotate-a-credential,
              prepare-*-deposit-key, reprint-recovery-sheet,
              confirm-recovery-sheet-kept, maintenance-break-glass-deposit

    deploy                                   standing · the far end of the belt
      machines  cluster-deploy-runner, forge-converge, cluster-converge,
                boss-gcp-converge
      capacity  1 (a converge holds the floor)
    estate                                   standing · the hosts, observed and kept
      machines  estate-observer (host, units), cluster-watchdog,
                disk-floor-sweep, dev-scratch-reclaim, conservation-sweep,
                maintenance-timer, files-gc, prune-registry-versions,
                views-catchup, search-reindex, messages-purge,
                ledger-recognize, ledger-replay, audit-integrity, backups
      alloc     a small standing footprint on each host (measure)
    vault                                    standing · credentials and deposits
      machines  credential-broker, credential-deposit,
                machine-token-deposit/effect, forge-token-audit
    edge                                     standing · host:boss-gcp
      machines  gateway, bastion, public site, playground-crawl
    repair                                   standing · the garage
      hosts     incident fulfilment, diagnostics, the hands-on half of
                rerail-a-car
    ad hoc under operations, by resolution:
      ops/<run>     ops-runner workspaces for maintenance steps: automaton, small
```

The tree is drawn by station name. The address of each station is derived
from its `parent` chain and is never stored on a row or spelled in code
(§8.1).

Future producing departments (finance, people, a tenant's production
floor) are siblings of engineering and operations. Each has its own
dispatcher and protocols, hands "change the software" to Engineering, and
hands "give me a machine, a credential or a fix" to Operations.

## 3. Resource allocation

### Root pools (the constraints)

| pool | shape | capacity | where the number comes from |
|---|---|---|---|
| `host:w-1` (the build node) | capacity; disk is **stock** with the floor `ci_host_floor_gb` | *measure* | estate observer facts; floor from the delivery policy today |
| `host:forge` | capacity | *measure* | same |
| `host:cp-1`, `host:cp-2`, `host:cp-3` | capacity | *measure* | same |
| `host:w-2` (the utility worker) | capacity | *measure* | same |
| `host:boss-gcp` | capacity | *measure* | same |
| `people:platform-admin` | capacity | 1 | the roster (`employees.json`); never set by hand |
| `robots:<model>` | **rate** ($/day) and concurrency | a daily ceiling (set); per-step `budget_usd` is already in the workflows | tenant seed; `model-slot:<class>` until T5 renamed it (backlog 84f570a1, 2026-11-07: `robots:opus`, `robots:sol`, the <model> being the family an agent row's `model_class` names). The rows carry concurrency today; the rate draws on `budget:robots` |

There is one pool per box rather than one summed vCPU pool, because a
reservation is only real on one box. The sum is a HUD number, not a
constraint. A pool is named by the estate registry id of its host and
never by the station that happens to sit on it, so nothing at the root
reads like a station. The Dell joins as its own `host:` pool when it
arrives (11-09, holding PostgreSQL).

Operations **maintains** every box: it observes, reclaims and converges,
and `join-a-node` and `replace-a-disk` take a `host:` pool as their
subject. It does not own the capacity Engineering reserves on a box.
Owning capacity is an allocation; keeping the box is a mandate.

### Standing allocations (reserved, drawn from the root)

| station | from | reserved | note |
|---|---|---|---|
| gates | host:w-1 | 2 seats × per-seat (measure); disk above the floor | |
| forge | host:forge | the services' footprint (measure) | the box is Operations' to keep |
| engineering (floating, for ad hoc) | host:w-1 | the remainder after gates and observers | workspaces draw here first |
| deploy | host:cp-1..3 | per converge (measure) | |
| estate | every box | the observers' footprint (small, measure) | |
| edge | host:boss-gcp | all | |
| operations (floating, for ad hoc) | host:cp-1..3, host:w-2 | a bounded share for ops-runner workspaces | |
| executive | people:platform-admin | 0 reserved; draws 1 floating per human step | |

A child's reserved allocation is subtracted from its parent's floating
capacity at seed. An ad hoc station draws floating capacity from its
department first, then from the root's remaining slack, and the record
names which one satisfied it (the console's "on root slack" edge).

### Executors

- The one human is a root pool of capacity 1, and the executive station
  hosts his steps. Every human-gated queue in Engineering *and*
  Operations converges on that one station. That is the truth, and it is
  what the floor should show.
- Robots are `robots:<model>` at the root with a rate ceiling. A claim
  reserves concurrency 1 and an estimate of spend, trued up at release
  (Q5 of the taxonomy). Engineering's robots build; Operations' robots
  diagnose and run the maintenance steps that need judgement.
- Automata are bounded by the standing station that hosts them (seats).
  Most of Operations' work is done by automata.

## 4. Protocol ownership and hand-offs

Every workflow gets an `owner` station, replacing `metadata.department =
"it"`; §2 was the first assignment. **Design c0ae096f §1 (David,
2026-11-10) superseded it**, and with it this table's owner column and
§7's answer to Q5: `incident` and `break-glass-enrolment` are
Operations'; `workflow-design`, `protocol-experiment`, `protocol-retro`,
`enroll-a-managed-agent` and `publish-a-protocol` are Engineering's;
`correct-the-record` and `work-session` are the company's; the
`maintenance-*` rows are all Operations' but the ledger two, which are
Finance's. The rows' `metadata.owner` and the pin
`every_platform_protocol_names_a_standing_owner_station`
(crates/core/boss-jobs/tests/station_tree.rs) are the record; §2's tree
drawing predates c0ae096f's stations too. The hand-offs:

| protocol | owner | step | fulfilled_by |
|---|---|---|---|
| `backlog-item` | the company | "Engineering: ship it" | engineering's `ship-a-change` |
| `pr-train` | engineering | "Operations: roll it" (after image-built) | operations' `roll-a-train` |
| `incident` | the company | "Operations: repair", then "Engineering: fix" | operations first, then engineering's `emergency-merge` |
| `keep-the-estate` (new; what the systemd timers become) | the company | one step per maintenance family, on cadence | estate's `maintenance-*` |
| `ops-request`, `diagnostic-request` | operations | any department may open one | none |
| `approval`, `publish-request`, scope sign-off, landed-review | various | the human step | hosted at executive |
| `protocol-experiment`, `protocol-retro`, `learning` | the company | "Engineering: change the protocol" | engineering's `workflow-design`, then `ship-a-change` |

Inside Engineering, `ship-a-change.gate` becomes a `gate-run` at gates,
and `pr-train` consumes `ship-a-change` packets at the dock. Inside
Operations, `roll-a-train` runs at deploy, and every `maintenance-*` runs
at an `ops/<run>` ad hoc station or a seat at estate.

`pr-train` splits where the image is built. Boarded, gate-run, merged and
image are Engineering's steps; converge, verified and rolled are
`roll-a-train`'s. The cadence timers that board trains stay at the dock;
the converge discipline ("one at a time, holds the floor") is
Operations'.

## 5. The eleven regions, mapped

| region | becomes |
|---|---|
| dock | engineering's dock |
| gates | engineering's gates |
| track | a view: trains between engineering's dock and operations' deploy, the one view that crosses the boundary |
| shed | ad hoc `proof/<ws>` under engineering |
| arrivals | a view: admission events at engineering |
| garage | operations' repair |
| receiving | a view: `backlog-item` packets at the company, waiting for the "ship it" hand-off |
| marshalling | a view: ship-a-change packets at scope or sign-off, which is to say waiting on executive |
| shop-floor | the ad hoc stations under engineering, as a group |
| publish | operations' edge, plus `publish-to-github` |
| sensors | operations' estate |

Four of these are views over the record, not places. Under v2 a view is a
projection filtered by station and step, and needs no row.

## 6. What changes in the repo

The repo facts below are reconciled with main as of 2026-11-06; see
"Where main stands" in [factory-taxonomy-v2.md](factory-taxonomy-v2.md).

1. **The stations registry gains the tree (T2).** Each
   `infra/platform/stations/*.toml` row gets a stable `id`, a `parent`, a
   `mode`, a `capacity`, `machines` and an `allocation`. The address is
   derived from the parent chain and never stored. The 12 existing rows
   get parents: `loading-dock` goes under engineering as the dock,
   `gates` under engineering, `repair` under operations,
   `design-review`, `design-decided`, `landed-review`,
   `q.platform-admin.task` and `my-watchlist` under executive, and
   `converge`, `deploy`, `forge-ci` and `probe-shed` under the department
   that runs them. New rows declare the root, executive, engineering,
   operations and their standing children. Under the bundle's
   additive rule the v1 `kind` and `bays` stay on every row until the
   retirement car.
2. **The pools registry follows the tree (T5).** `infra/platform/pools/`
   already holds 9 rows under v1 names. It gains one `host:<estate id>`
   row per box from the estate observer, `people:*` from the roster and
   `robots:*` from the budget. `ci_host_floor_gb` becomes the disk floor
   of `host:w-1`.
3. **Workflows get an `owner` (T6)**, and steps get a station spec
   (`under` / `at`, `executor`, `machines`, `resources`) or
   `fulfilled_by`. `pr-train` splits, and `roll-a-train` and
   `keep-the-estate` are new. `metadata.department = "it"` retires with
   the cutover stamp.
4. **Automations get a host station** (`station = <its station id>`).
   The automation row is the machine, and the field says where it is
   provisioned. The dispatcher becomes three rows of one binary, each in
   a different station.
5. **`REGIONS` and `transit.ts STATIONS` retire (T7)** once the yard
   reads `GET /api/stations?under=…` AND the new floor shows jobs moving
   between stations. The four view-regions become filters on the claims
   read.
6. **systemd timers become `keep-the-estate` steps on cadence.** They run
   the same scripts, but as steps on packets at operations, so every
   maintenance run is in the record (R9). This is the car that most
   changes how the floor looks: Operations will be the busiest department
   by packet count, almost all of it automata.

## 7. Decided questions

David accepted every proposal on 2026-11-06 at 20:24Z.

**Q1. Is executive a station or a role?** A station, so the one real
bottleneck is one place on the floor rather than spread across every
protocol that needs him.

**Q2. Who owns the forge box?** Operations keeps `host:forge`, maintained
like every box. Engineering owns the repo services on it as its standing
forge station, with a reserved footprint. Moving forge under Operations
later is one `parent` edit.

**Q3. Which hosts carry Engineering's ad hoc workspaces?** The builder
hosts. Today that is `w-1`, the build node, where gates and builder
workers already run as Kubernetes Jobs; when another host carries
workspaces, engineering's floating pool spans both and the record names
the host each workspace landed on.

**Q4. One dispatcher binary, three rows?** Yes: the root (company-altitude
routing), engineering (dock discipline) and operations (converge
discipline, sweep cadence). The registry row says which station each is
provisioned into.

**Q5. Does `incident` start at the company or at Operations?** At the
company, because an incident may need both departments and the executive.
Operations diagnoses first, and a fix is a hand-off to Engineering.

**Q6. Where do the four view-regions live in the UX?** As named filters
on the floors, not as boxes. The allocation map draws stations only.
`track` is the one view that spans two departments, and it is worth
drawing as the belt between them.

The taxonomy's own questions are decided in the same answer: a step
fulfilled by packets declares its own completion test over the children's
outputs; an ad hoc station lives for one claim by default, and `keep:
packet` reuses it across steps with the allocation held and visible; a
rate pool is estimated at claim and trued up at release; a floating draw
goes to the nearest ancestor that can satisfy it, and the record names
it.

## 8. Changing our minds

The tree in §2 is a first cut, and being wrong about it has to stay
cheap. The design makes a reorganization a registry edit, not a car, by
holding to six rules.

1. **Code never names an address.** Code knows executor kinds, pool
   shapes, station modes and the resolution algorithm. Every address
   lives in a registry row: a station's `parent`, a workflow's `owner`, a
   step's `under`/`at`, an automation's `station`. The vocabulary lint
   (T4) refuses a literal tenant address in `crates/` or `apps/`.
2. **Stations have a stable id, and the address is derived.** The record
   and every claim name the station by id. The address at any sequence is
   a projection of the tree as it stood then. Re-parenting a station is
   one field on one row with a version bump. Nothing in history is
   rewritten, and Review shows the tree as it stood.
3. **Moving a protocol is changing its `owner`.** Its steps are
   re-validated against the new subtree (R10). The lint names any step
   whose `under`/`at` now falls outside it, and the fix is either to move
   the station too or to turn the step into a `fulfilled_by` hand-off.
   Packets in flight stay pinned to their protocol version and to the
   stations they claimed.
4. **Moving capacity is editing an `allocation`.** R2 is checked at seed:
   the register refuses a tree whose children over-draw a parent.
   Shrinking a standing allocation below what its open claims hold is
   refused until they release (Q7 of the taxonomy).
5. **Splitting or merging a department is a sequence of 2 to 4.** Add a
   station row, re-parent its children, re-own its protocols, move its
   allocation. The record carries each step as an event, so the floor can
   replay the reorganization.
6. **Only structure is invariant.** The register pins: one parent and one
   root; a child's allocation never exceeds its parent's; a protocol names
   only its owner's subtree; every `fulfilled_by` target exists; every
   executor id resolves to one kind; a machine fault is not a product
   defect. It does not pin which departments exist, what they are called,
   or where any station sits. Those are the tenant's to change.

The test of the design is the reorganization done on paper on 2026-11-06:
Documentation removed, Operations added, the back half of `pr-train`
moved. Under these rules that is one station row deleted, four added,
`parent` changed on three, `owner` changed on about 40 workflow rows, and
`pr-train` split at one step with a `fulfilled_by`. No code change. If any
of it turns out to need one, that is the defect to hunt. The bundle's
acceptance demonstrates the re-parent half of this in a test.

## Decision history

_Proposed 2026-11-06 after the v2 taxonomy session. David: "software
changes should be running through Engineering, so we don't need a lot of
the infra machinery at the company level". Revision 2 the same morning:
"we need Operations, in addition to Engineering, that is actually running
all the software and servers. Operations is going to support the whole
company." No Documentation department. "It should all be arbitrary
though, so let's make sure we can easily change our minds", hence §8.
Accepted as the direction at 18:50Z; questions decided at 20:24Z; hosts
reconciled to the estate registry the same evening._
