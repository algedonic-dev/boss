# JobKind authoring & editing UX — graphical, trigger→outcome-guided

**Status:** design / open questions. Living guidance until the settled
parts fold into [architecture-decisions.md](../architecture-decisions.md)
§Jobs, JobKinds, Steps. Realizes the "Workflow modeling UX
improvements" item in [TODO.md](../../TODO.md).

## Frame

The model and the lint already enforce well-formed trigger→outcome
DAGs. `crates/core/boss-jobs/src/job_kind_lint.rs` (`validate_all`),
run at publish, boot, and seed-load, guarantees: **≥1 trigger** (a step
with `ready_when = "true"`), **≥1 terminal** (a step with a
`StepSpec.terminal`), **full reachability both ways** (every step
forward-reachable from a trigger AND backward-reachable to a terminal —
no dead steps), **acyclicity** (the DAG is implicit in `ready_when`;
`blocked_by` is derived, never authored), and **fork-coverage** (at any
fork, every enum `outcome` value is handled by a successor, or a
free-text fork has an explicit fallback).

So the system already produces correct trigger→outcome graphs. **The
gap is the authoring UX, not the rules.** Today (`apps/web/src/job-kinds/`):

- Steps are authored in a flat form; `ready_when` is typed as **raw
  boss-expr text** and `metadata_defaults` as **raw JSON** per step.
- There is **no edit-after-create** — only fork-to-a-new-slug. The
  `PUT /api/jobs/kinds/{kind}` endpoint exists but the SPA never calls it.
- `metadata_schema`, `entitlements`, and per-step `fields` are
  **unauthorable** (hardcoded `{}` on create).
- The authoritative lint runs **only at publish** and surfaces as a raw
  `HTTP 400: <text>` string. A DAG is rendered, but on the read-only
  *detail* page, not in the editor.

Authors fly blind until publish fails.

**Goal:** make the trigger→outcome DAG *the editing surface*, pull the
authoritative lint forward to author-time, and guardrail the structure
so an invalid graph is hard to build.

**Non-goal:** changing the model or the lint — both are correct. This
is a UX effort plus a thin API surface. The DAG stays implicit in
`ready_when`; `blocked_by` stays derived; the editor *generates*
`ready_when`, it does not add a second edge store.

## Decided direction

1. **Interactive DAG editor — the canvas is the surface.** Steps are
   nodes, auto-laid-out left→right from trigger(s) to outcome(s)
   (Sugiyama layered — the long-standing `StepDagEditor` TODO).
   Node types are visually distinct and first-class:
   - **trigger** (entry; `Completion::AutoOnMaterialize` / `ready_when
     = "true"`) — an entry chip;
   - **outcome / terminal** (`StepSpec.terminal`) — an end/flag shape
     labelled with the outcome;
   - **fork** (an `agent`/gate step whose StepType has an enum
     `outcome` field, e.g. `demand-gate`) — a branch node, one outbound
     branch per enum value;
   - **work step** — a card showing kind, executor (human / agent /
     child-job / external), and authority role.

   A **step palette** sourced from `GET /api/jobs/step-types` (grouped
   by `StepCategory`) is dragged onto the canvas; selecting a node opens
   a right-side inspector for its fields.

2. **Edges = `ready_when`, authored by connecting + a structured
   builder.** Drawing an edge A→B adds `steps.A.done` to B's
   `ready_when`. A condition builder refines it as AND/OR of clauses —
   *a step is done*, *`job.metadata.<field> <op> <value>`*, *this fork's
   outcome = `<enum>`* — and emits the boss-expr string. Authors never
   type the grammar; the raw predicate is viewable read-only behind an
   "advanced" reveal. Connecting an edge *from* a fork prompts which
   outcome value the branch handles, which is what drives fork-coverage.

3. **Live lint — the authoritative validator at author-time.** A new
   backend endpoint runs `validate_all` on a draft spec **without
   persisting** and returns the structured offender list (phase, step,
   message). The editor debounces a dry-run on every change and renders
   results **on the graph**: unreachable steps dimmed with a badge;
   steps that cannot reach a terminal flagged; an uncovered fork outcome
   drawn as a dangling red branch stub on the fork; missing
   trigger/terminal as canvas banners; predicate parse errors on the
   offending node. Because it reuses `validate_all`, **editor-green ==
   publishes**.

4. **Guardrails.** A new JobKind scaffolds with a trigger node and one
   outcome node pre-placed and connected. The editor refuses to delete
   the last trigger or the last terminal. Publish is disabled while the
   live lint has errors (it would 400 anyway). Drafts may still be
   *saved* in an invalid state so work-in-progress persists — only
   *publish* is gated.

5. **Close the gaps (table-stakes that ride along).** Wire `PUT
   /api/jobs/kinds/{kind}` so a draft can be edited (not only forked);
   add structured editors for `metadata_schema`, `entitlements`, and
   per-step `fields` (the typed-fields half of a JobKind is currently
   inaccessible); source subject-kinds from the registry (drop the seven
   hardcoded core kinds in `JobKindNewPage`).

### Enforced vs. informed

- **Enforced** (hard — guardrails + publish gated on `validate_all`):
  ≥1 trigger, ≥1 terminal, reachability both ways, acyclic,
  fork-coverage.
- **Informed** (live, on-graph): which node is the entry, which are
  terminal outcomes, where a fork's branches miss an outcome, dead
  steps. The author *sees* the trigger→outcome flow as the literal shape
  being built.

## Implementation shape (incremental slices)

- **Slice 1 — backend, small, zero model change.** `POST
  /api/jobs/kinds/_validate` dry-run (reuse `validate_all`, no persist)
  + wire the existing `PUT` for edit-draft. Unblocks live lint + editing.
- **Slice 2 — canvas.** Promote `jobs/StepDag.svelte` (read-only render)
  to an interactive editor; node inspector; step palette; live-lint
  overlay.
- **Slice 3 — edges.** Structured predicate/edge builder; fork-branch
  authoring.
- **Slice 4 — completeness + guardrails.** `metadata_schema` /
  `entitlements` / `fields` editors; scaffold trigger+outcome;
  publish-gated-on-live-lint.
- **Slice 5 — polish (from TODO).** Version diff by stable step id;
  live "what would a new Job of this kind look like" preview.

## Decision history

All five open questions resolved 2026-06-19.

- **D1 — step identity (was Q1): keep slug-as-identity; no per-step id.**
  The `ready_when` DSL references steps by slug, so a rename must
  rewrite referencing predicates regardless of any id — an id would only
  help the version diff. Not worth touching `StepSpec` + the lint's
  slug-ref model + every seed + a backfill. The editor does rename as an
  **atomic refactor** (rewrite all `steps.<old>` references); the diff's
  rename-as-remove+add is acceptable (or fixed later with a
  position+kind heuristic). Revisit only if a data-layer feature needs
  rename-stable references across versions.

- **D2 — graph editor (was Q2): adopt `@xyflow/svelte` (Svelte Flow) +
  a layout lib (dagre/elk), lazy-loaded on the editor route.** Interactive
  drag/connect/layout is Svelte Flow's sweet spot; hand-rolling it on the
  bespoke SVG is months of work. Both deps are MIT; dependabot tracks
  npm; the editor chunk is code-split (same pattern as the React plugin
  runtime) so the main bundle isn't hit. Read-only `StepDag` stays for
  the detail page for now.

- **D3 — predicate builder (was Q3): curated structured builder + a
  live-validated raw "advanced" escape hatch.** The builder covers the
  common shapes (step-done, `job.metadata` comparisons, fork-outcome
  equality, AND/OR); the long tail uses raw text but — unlike today —
  with live boss-expr parse + reference validation. Must round-trip: a
  predicate the builder can't represent shows in advanced mode rather
  than being dropped.

- **D4 — save semantics (was Q4): edits always target a DRAFT version.**
  `save` = upsert the working draft; `publish` = promote it to active and
  demote the prior active to retired. The active row stays immutable
  (required by append-only versioning + the version-pin invariant). This
  is the existing `create_draft`/`publish` model; the editor's Save wires
  the (currently-unused) `PUT /api/jobs/kinds/{kind}`.

- **D5 — dry-run registry (was Q5): reuse the process-resident StepType
  registry.** `StepRegistry::v1()` is loaded once from `step_types.toml`
  into a `OnceLock` (`'static`); `validate_all` of a single draft needs
  only that registry. The dry-run endpoint runs in-process, so
  editor-green == publishes by construction.

- **D6 — authoring host (was "can drafts be local state for the authoring
  step?", resolved 2026-06-21): a JobKind is authored *through* a
  `job-kind-design` Job; the working spec lives in that Job's publish-step
  `metadata.job_kind_spec`, and the registry write + `jobs.kind.published`
  audit fact happen exactly once, when the terminal `job-kind-publish` step
  completes.** The Slice-1/2 SPA took the direct `POST/PUT /api/jobs/kinds`
  path, which writes a throwaway `job_kinds` draft row per Save *and* — via
  the direct `POST /kinds/{kind}/publish` — emits **no** `jobs.kind.published`,
  so SPA-published kinds were never recorded as published facts (a provenance
  gap vs the five-property protocol). The Job-based machinery already exists
  (`job_kind_design_spec`; the `job-kind-publish` StepType +
  `dispatch_job_kind_publish` → `publish_authored`) but nothing ever drove it.
  Realization, **zero backend change** (the runtime contract was verified end
  to end against the stack):
  - **New** collects the slug + spec fields and POSTs a `job-kind-design` Job
    with `subject = {custom, <slug>}`. The slug is the Job's immutable subject
    id — which *is* D1's slug-as-identity. (`custom` is a seeded subject kind;
    custom subject ids bypass existence checks, so a brand-new slug is
    accepted.) Steps materialize on create.
  - The graph surface edits the full spec and persists it by **debounced PATCH
    onto the publish step's `job_kind_spec`** (legal: step metadata is
    required-at-done, not at-create). These are normal `STEP_UPDATED` events on
    the design Job — durable, resumable authoring history, **no `job_kinds`
    rows**. (Metadata PATCH replaces the field wholesale, so the SPA always
    sends the publish step's complete metadata.)
  - author → validate → approve (platform-admin sign-off: POST `/sign-offs`
    then complete) → publish are the gates; the SPA gates "advance past
    validate" on the live dry-run being clean. Completing the terminal publish
    step fires the single registry write + event (returns 204; the SPA then
    routes to the published kind).
  - **Editing** a published kind opens a fresh design Job seeded from the
    active spec (optionally stamping `previous_kind_version`).
  - The direct `/api/jobs/kinds` create/update/publish handlers stay for
    bootstrap + tests; the **SPA stops calling them**.

## Build status

- **Slice 1 (done):** `POST /api/jobs/kinds/_validate` dry-run (reuse
  `validate_all`, no persist) — the keystone the live lint reads. Lean
  `{kind, steps}` body so a per-keystroke call never 422s on the
  heavyweight registry fields. Tests in `job_kind_registry.rs`.
- **Slice 2 (done):** the graphical authoring surface.
  - `JobKindGraphEditor.svelte` — interactive Svelte Flow canvas (nodes
    classified trigger/outcome/fork/work; edges derived from `ready_when`
    references; dagre LR layout; live-lint problems badge the nodes),
    lazy-loaded into its own ~216 KB chunk (D2).
  - `StepPalette.svelte` — add a step by picking a StepType (the
    `/api/jobs/step-types` registry vocabulary).
  - `StepInspector.svelte` — edit the selected node; slug renames route
    through `renameSlug`, rewriting every `ready_when` reference (D1).
  - `StepAuthoringSurface.svelte` — composes palette + canvas + inspector
    + the full list editor; owns the one step-types fetch and the
    debounced dry-run lint. Reused by every authoring host.
  - Pure, unit-tested step transforms in `stepEdits.ts`
    (`makeStep`/`freshSlug`/`patchStep`/`removeStep`/`renameSlug`).
- **Authoring-as-a-Job (D6, done):** the persistence + publish flow re-based
  on a `job-kind-design` Job (the graph surface above is reused unchanged).
  - `designJob.ts` — the design-Job client (create / load / find-step /
    persist-spec-to-publish-step / complete-step / sign-off) + the pure
    `initialSpec` seed (unit-tested).
  - `JobKindDesignWorkspace.svelte` + `/admin/job-kinds/authoring/:jobId` —
    loads the design Job, binds the surface to the publish step's
    `job_kind_spec` (debounced PATCH), drives author → validate (gated on the
    live dry-run being clean) → approve (sign-off) → publish.
  - `JobKindNewPage` reduced to a name-it entry that creates the design Job;
    detail-page **Edit…**/**Fork…** open a design Job seeded from the active
    spec. The direct-API `JobKindEditPage` + `:slug/edit` route and the
    detail-page **Publish draft** button were removed — the SPA no longer
    touches `POST/PUT /api/jobs/kinds` (D6).
- **Slices 3–5:** not started (structured predicate/edge builder;
  metadata/entitlements/fields editors; version diff by stable step id +
  live new-Job preview).
