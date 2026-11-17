# The factory taxonomy — one vocabulary for the code and the floor

**Status**: decided — the taxonomy is David's (2026-11-02, "I really like this"; "let's definitely put it in place and then start consolidating both our code and factory UX in BOSS towards this taxonomy"). The deterministic executor kind is **automaton**, not machine (David, 2026-11-02: "Let's go with automaton then"): `machine` already means a state machine, a machine token and the machine door in about a thousand files of this tree, and `automaton` was in none. Q1 and Q2 below are decided (2026-11-02). The consolidation cars are filed after the operations-console cars.
**Origin**: David, 2026-11-02: *"There is something about the 'deterministic machinery' of the factory too, like stations that host testing gates, deployment pipelines. We are missing something about the factory here. A lot of that machinery shares a resource pool, but they aren't quite fungible like how a station for scoping and a station for reviewing don't really require different configurations. Are we missing some abstraction or do we need to start being more precise with the taxonomy of our factory elements so we can look at them more consistently."* Both.
**Related**: docs/design/the-three-layers.md (the reading frame this refines) · the operations-console design packet (a236103a, Q7) · docs/design/stations.md · docs/design/design-conformance.md · `docs/invariants/` · `CLAUDE.md` §Supporting concepts.

---

## The taxonomy

The three layers stand: the network is the substrate, the fat protocols dictate the current operating model, the actors run it. What was imprecise is *what kinds of thing* live in each layer, and the imprecision had a shape: the deterministic machinery (gates, the deploy pipeline, the probe shed, the dock) was filed as "a station" in some places and "automation" in others, and neither fit, because it is **equipment**: shared, scheduled, capacity-bound process machinery that is not interchangeable with a bench. Two axes sort everything: **determinism** (same input, same output?) and **fungibility** (can any instance do it?).

| Group | Element | What it is | Fungible | Capacity |
|---|---|---|---|---|
| **Work** | Packet | One piece of IP in progress, with its history | — | — |
| | Step | One operation on a packet, from the protocol | — | — |
| **Specification** (versioned data) | Protocol | Route + per-step contract, check, bill of resources, executor kind | — | — |
| | Check, inline | A verifier small enough to inject into any station's slot | — | — |
| | Check, equipment-hosted | A verifier heavy or reused enough to own machinery (the gate); its program is IP the factory produces (`infra/gate.sh`) | — | — |
| | Discipline | A station's queue policy: priority, WIP, cadence | — | — |
| **Executor** | Person | Judgment; stateful (attention) | within role | roster |
| | Robot | Judgment; stochastic; stateless between claims | within model class | slots |
| | Automaton | Mechanical; deterministic; operates equipment; has health | no | bays |
| **Place** | Generic station | Holds steps for a capability; check injected; any count | yes | pool-limited |
| | Equipment station | Bound to machinery (gates, deploy, probe shed, dock); operated by an automaton; shared by many protocols | no | bays |
| **Resource** | Pool (flow) | Slots, vCPU, RAM, people; released on step end | — | capacity |
| | Stock | Disk by consumer, credentials; recovered by a protocol | — | capacity |
| | Bays | Per equipment station; the non-fungible constraint on top of the pools | — | fixed |
| **Substrate** | Estate, record, admission | Hosts and network; the log; the one edge in | — | — |
| **Automation** | Conveyance, maintenance, yard | Automata whose work is a reaction to the record, not a step on a packet | — | — |

Three rules follow, and each is a mechanism below, not a sentence:

1. **Eligibility is capability + reservation + bay.** A step names the executor kind it needs (a role, a model class, an equipment kind); a claim clears on that, on the pool reservation, and — for an equipment step — on a free bay.
2. **Equipment has two kinds of failure; a bench has one.** A robot's failure is rework of the product. Equipment fails as a product defect (a red gate) or an equipment fault (a dead runner, a disk floor), and the record must never write one as the other (`CLAUDE.md` §Diagnosis: "an infrastructure refusal is not a consist failure").
3. **Automation is automata with a reaction trigger.** Executors are people, robots and automata; there is no fourth kind. The dispatcher, the conductor and the sweeper are automata whose "step" is a reaction to the record.

The words, so that the code and the floor say the same thing: an executor is a **person**, a **robot** or an **automaton**, and `executor_kind` carries exactly one of the three. Equipment is *operated by an automaton*; there is no separate word for equipment's machine. `machine` keeps only the meanings it already has in this tree — the state machine, the machine token, the machine door — and names no executor.

## The census (measured 2026-11-02)

Files in `crates`, `apps`, `infra`, `docs`, `CLAUDE.md` using each word as a whole word, case-insensitive:

| word | files | | word | files |
|---|---|---|---|---|
| actor | 784 | | region | 163 |
| machine | 688 | | executor | 201 |
| agent | 590 | | territory | 108 |
| automation | 538 | | machinery | 102 |
| dispatcher | 691 | | equipment | 64 |
| conductor | 337 | | crew | 49 |
| station | 320 | | mover | 20 |
| gate | 1070 | | bay | 78 |
| pool | 535 | | slot | 186 |
| reservation | 63 | | lease | 28 |

`automaton` was in 0 files when the word was chosen, which is why it was chosen.

The vocabulary is not wrong so much as **unsorted**: "actor" and "executor" both name the umbrella; "agent" names the robot in the registry and "crew" names it on the floor; "machine" names an executor in `shop-floor.ts` and a region's machinery in `world-machines.ts`; "region" and "territory" name IT's stations on the map while "station" names them in the registry; "gate" is a region, a step, a script, a run kind and a verdict. None of that is a defect in any one file. It is what happens when the taxonomy lives in heads, which is the design-conformance diagnosis: *prose asserts, and nothing checks*.

## The mapping (old name → taxonomy)

| today | becomes | where it lives |
|---|---|---|
| actor, executor (umbrella) | **executor** in code; "actor" survives only in the three-layers sentence and the policy vocabulary (`actor_station`) | `boss-core` enum |
| agent (registry row) | **robot** as the executor kind; `agents` stays the registry's table name (a rename of a table is a migration with no value) | `agents.executor_kind = 'robot'` |
| person, human, employee-as-executor | **person** | `executor_kind = 'person'` |
| machine (as an executor), machinery, runner, ops-runner, gate-runner | **automaton** (executor kind) when it does a step or reacts; **equipment** (station kind) when it is the place with bays; `machine` keeps its other meanings (state machine, machine token, machine door) | `executor_kind = 'automaton'`; `stations.kind = 'equipment'` |
| crew (`it/crew`, `shop-floor.ts`) | the **robots** on a floor, grouped by identity; the word may stay as the panel's name | UX label only |
| region, territory (`regions.rs`, `world.ts`) | **stations** on IT's floor, drawn by rule; `region` stays the API's word until the floor reads station rows (console car 4), then retires | `GET /api/yard/regions` → station rows |
| batch station (dock) | **equipment station** whose automaton is the conductor | `stations.kind` |
| gate (region) / gate-run (packet) / gate.sh (program) / verdict | **gates**: an equipment station; **gate-run**: a packet on it; **the program**: an equipment-hosted check; **verdict**: the product fact, kept apart from the equipment's health | unchanged names, sorted kinds |
| capacity, bound (`bound_kind: capacity`) | **bays** on an equipment station; **pool** capacity elsewhere | `pools` registry (console D2) |
| mover | retired | — |
| automation (regions family) | **automation**: automata with a reaction trigger | unchanged |

## The mechanisms

Each is a car, and each turns a row of the table above into something a lint or a test can refuse.

**M1. `ExecutorKind` in `boss-core`.** An enum, `person | robot | automaton`, read by the agents registry (a new `executor_kind` column, backfilled `robot`), by the employee row (`person`), and by every automaton identity the dispatcher, conductor, ops-runner and gate-runner sign as (`automaton`). The claim door records the kind on the claim. Pinned by a test that every executor id the record names resolves to exactly one kind (the false-fourth-kind check).

**M2. `stations.kind` gains `equipment`, and `bays`.** `generic | equipment` (the dock's `batch` kind becomes `equipment` with the conductor as its automaton, and `batch` retires with the seed that references it — Q1). An equipment station row names its automaton and its bays; the delivery policy's gate capacity becomes `bays:gates`. Pinned by the register entry `an-equipment-station-declares-its-bays`.

**M3. A step names its executor kind.** The WorkflowSpec step gains `executor` (a role, a model class, or an equipment kind) beside the `resources` block of console D1; the station-eligibility predicate reads it. The lint that refuses a `resources` block naming an undeclared pool also refuses an `executor` naming an undeclared kind.
*Built so far (car T2b, item 0a78f8f8):* the field already named a credential principal (`runner:ops`, design f623e425), so a kind is three more prefixes of it — `role:<slug>`, `model:<class>`, `equipment:<station>` — implying person, robot and automaton (`crates/core/boss-jobs/src/step_executor.rs`). `pools::validate_step_declarations` refuses an undeclared one at the platform seed (roles read from the live `classes` table) and in the tree-wide pin (pools and stations). The claim-time eligibility predicate is the next car; until it lands no protocol row declares a kind.

**M4. Two failure facts at equipment.** A gate-run's record carries `equipment_fault` and `product_defect` as separate fields, and the verdict readers (the merge, the alert, auto-cancel) take both halves (the 6f18390b rule, generalised). Pinned by the register entry `an-equipment-fault-is-not-a-product-defect`.

**M5. Retired words are refused in new files.** `infra/lint/factory-vocabulary.sh`: a file newer than the cutover stamp that uses `mover`, `territory`, or `machinery` as an executor, or `machine` as an executor kind, fails the gate, the same shape as `migrations-declare-schema-only.sh`'s cutover. `machine token`, `state machine` and the machine door stay legal. Older files are history; they are renamed as they are touched, never in a sweep (a rename sweep is the merge-conflict shape §9a warns of).

**M6. The floor speaks the taxonomy.** Every station panel labels its kind (generic: capability + reservations; equipment: automaton health + bays + what is in them), every lamp labels its executor kind, and the HUD's AUTOMATA row is the automata. This is console cars 4–5 with the labels fixed by M1–M2 rather than chosen per panel.

## Cars

T0. **This doc, and `CLAUDE.md` §Supporting concepts points at it** (one paragraph: executors, places, resources, with the table's link). Ships alone, with the register entries below filed `unenforced`.
T1. **M1**: `ExecutorKind`, `agents.executor_kind`, the claim records it, the one-kind test — with a backfill of the claims already recorded (console car 2a records claims), so the one-kind test covers history.
T2. **M2 + M3**: station kinds and bays; the step's `executor`; the eligibility predicate. Rides with console cars 1–2, which it shares a schema with.
T3. **M4**: the two failure facts on gate-run and its readers.
T4. **M5**: the vocabulary lint with its cutover stamp.
T5. **M6**: with console cars 4–5; cars 4 and 5 land as they are and T5 relabels.

Sequencing (2026-11-02): T0 and T4 first; T1 after console cars 2c/2d and before any further car that records a claim; T2, T3 and T5 after the console lands. Every car names what it DELETES (the `batch` kind, the regions API after console car 4, `mover`, the crew model). The register entries are filed with T0 as `unenforced`, `DOES NOT HOLD TODAY`, and flip to `enforced` as each car lands — the same path every drift entry has walked:

- `an-equipment-station-declares-its-bays` (M2)
- `an-equipment-fault-is-not-a-product-defect` (M4)

## Decided questions

### Q1: Does the dock stay `batch`, or become `equipment` now?

Decided 2026-11-02, as proposed: **equipment now, with the conductor as its automaton**, and the `batch` kind retired with the seed that references it. A batch station is an equipment station whose automaton's program is a cadence; keeping a third kind for it is the false-fourth-kind error one level down.

### Q2: Is a person's capacity a pool or a roster read?

Decided 2026-11-02, as proposed: **a pool whose capacity is a roster read** — `people:<role>` rows derived at seed from the roster, re-derived when the roster changes, never hand-set. The console's Q3 reserves a person through the same event as a vCPU; the pool is how the claim reasons, the roster is where the number comes from.

## Decision history

_The taxonomy itself: decided 2026-11-02 (David). The executor word: automaton, decided 2026-11-02 (David), in place of machine. Q1–Q2: accepted as proposed 2026-11-02 (the conductor recommended both; David accepted the plan), recorded on the design packet 4348b857 as decisions_20261102. Filed as a design-doc packet by agent-claude-b from David's markdown, pasted 2026-11-02; this file is that text with the decisions folded in (car T0, item 2bfb1aba)._
