# Step plugin bundles

This directory holds the JavaScript bundles served at `/plugins/*`
by `boss-gateway`. A bundle adds a custom UX surface for one
StepType (or for a Workflow step that doesn't have an inline render
in the SPA today). Authoring one is the **second-level extension
point** in BOSS:

| Want to add… | Build… | Code? |
|---|---|---|
| A new workflow that composes existing steps | A Workflow row at `/system/workflows` | No |
| A new step kind with its own UX surface | A StepPlugin (this directory) | JS only |
| A new domain entity (Subject kind) | A new crate | Rust |

Both first-tier extensions (Workflow authoring + StepPlugin
authoring) are **data + JS**, no Rust changes, no core PR. New
core code lands only when you're introducing a new domain.

---

## What a plugin is

A plain JavaScript bundle (IIFE) that calls
`window.__boss_register_step_plugin(kind, mount)` on load. When
the SPA renders a step whose `kind` matches your `kind`, the host
fetches the bundle, calls your `mount(container, props)`, and
hands you a DOM element to render into. You bring whatever
rendering tech you like (vanilla DOM, lit-html, a bundled
micro-library, even a tiny React if you want — bundle it). The
host ships **zero framework runtime**; everything is your call.

A bundle exposes a single contract:

```js
(function () {
  function mount(container, props) {
    // container: HTMLElement — render here
    // props: { step, jobId, onUpdate, currentUser? }
    //   step       — the full step row, including step.metadata
    //   jobId      — owning Job id (you build the PUT URL from it)
    //   onUpdate() — call after a successful write to make the host refetch
    //   currentUser — { id, role } when a user is signed in
    const title = document.createElement('h3');
    title.textContent = props.step.title; // text, never markup
    container.append(title);
    return function cleanup() {
      // optional — called when the host unmounts your surface
    };
  }
  window.__boss_register_step_plugin('your-kind', mount);
})();
```

That's the whole API. There is no `save`/`done`/`cancel` helper:
to persist, you `fetch` yourself, through two doors, then call
`onUpdate()`. The keys your surface owns go to the step merge door,
`PATCH /api/jobs/{jobId}/steps/{step.id}/metadata` — it merges them
against the row as it stands, keeps every key you do not name, and
deletes a key sent as `null`. The status goes ALONE to
`PUT /api/jobs/{jobId}/steps/{step.id}` as `{ status }` (`"completed"`
to complete the step — a status is one of `pending`, `ready`, `active`,
`completed`, `skipped`, and any other word is refused with 400; the pin
`crates/core/boss-testing/tests/a_step_plugin_writes_only_step_statuses.rs`
holds every bundle here to that list). The PUT keeps every field its
body omits, and it is closing to any metadata body (backlog e39a9d2a):
`apps/web/src/steps/a-step-plugin-put-carries-no-metadata.test.ts`
refuses a step PUT here whose body is anything but the status. Send
the PUT only after the merge answered ok, so a failed save moves no
status. The props type is `StepPluginProps` in
`apps/web/src/steps/pluginHost.ts`; `StepPluginMount.svelte` calls
your `mount`. The full decision record is in
`docs/architecture-decisions.md` §Step UX & frontend.

**Build the surface as nodes, never as an HTML string.** A step's
title and metadata are written by whoever may write the step, and your
bundle runs in the reviewer's signed-in session — on sign-off steps,
the one a passkey signs. `createElement`, `append` (a string handed to
it is always a Text node) and `textContent` draw text as text;
`innerHTML`, `insertAdjacentHTML` and an `href` copied from input run
it. GitHub CodeQL failed a publish on exactly that in two bundles here
(backlog 4a359b51, 2026-09-27), and
`apps/web/src/steps/stepPluginHtmlSinks.test.ts` now refuses an HTML
sink in any bundle in this directory outside the few sites it names
with their reasons (an escape-first markdown renderer, a sandboxed
exhibit frame). It refuses a URL sink the same way (backlog 4f1f7698):
a `setAttribute('href' | 'src', …)`, a `.href =` / `.src =` write, or
an `href:` / `src:` prop to `h()` passes only when its value is a
literal starting with one `/`, or when the site is named — as the Join
button is, because `meetingHref` writes the http(s) scheme itself.

---

## Build your first plugin — a worked example

Suppose the brewery wants a "pour quality check" step: at the end
of a wholesale-keg-order, a sales rep visits the bar and walks
through a per-tap quality form (foam height, head retention,
clarity, off-flavor flags) for each keg they delivered. None of
the existing StepTypes capture that shape, so we author a new
StepType + a plugin to render it.

### 1. Add the StepType

Append a `[[step_type]]` block to
`crates/core/boss-jobs/seeds/step_types.toml` — the catalog
ships as data (D1, 2026-05-27); no Rust recompile needed beyond
re-running the boss-jobs-api so the embedded `include_str!` picks
up the new TOML. Mirror the shape of the existing entries:

```toml
[[step_type]]
kind = "pour-quality-check"
label = "Pour Quality Check"
category = "operations"
ux = "expanded"
version = 1
description = "On-site per-tap quality walkthrough …"
typical_duration_hours = 0.75
typical_duration_jitter = 0.5
required_roles = ["sales-rep"]
block_probability = 0.0
unblock_probability = 0.0
side_effects = []

[[step_type.fields]]
name = "visited_at"
field_type = "date-time"
required = false
description = "When the visit happened"

[[step_type.fields]]
name = "rep_id"
field_type = "string"
required = false
description = "Sales rep ID"

[[step_type.fields]]
name = "checks"
field_type = "array"
required = false
description = "Per-tap rows: {sku, foam_cm, retention_s, clarity, off_flavors}"
```

After appending, bump the count asserted by
`step_registry::tests::toml_parses_at_compile_time` (the fail-loud
guard that catches typos at `cargo test`).

### 2. Write the plugin bundle

Create `infra/step-plugins/pour-quality-check.js`. Self-contained,
no build step:

```js
(function () {
  function mount(container, props) {
    const { step, jobId, onUpdate } = props;
    const readOnly = step.status === 'completed';
    const checks = (step.metadata && step.metadata.checks) || [];

    // Persist our own keys through the step merge door, then (on
    // Complete) the status alone through the step PUT, then ask the
    // host to refetch. Set status='completed' to complete.
    async function save(status) {
      const merged = await fetch(`/api/jobs/${jobId}/steps/${step.id}/metadata`, {
        method: 'PATCH',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ checks, visited_at: new Date().toISOString() }),
      });
      if (merged.ok && status) {
        await fetch(`/api/jobs/${jobId}/steps/${step.id}`, {
          method: 'PUT',
          headers: { 'content-type': 'application/json' },
          body: JSON.stringify({ status }),
        });
      }
      onUpdate();
    }

    // Built as nodes, never as an HTML string: a SKU or a clarity note
    // is metadata anyone may write, and markup would run it. A string
    // handed to append() is always a Text node.
    function el(tag, props, ...kids) {
      const e = document.createElement(tag);
      Object.entries(props || {}).forEach(([k, v]) => {
        if (k.startsWith('on')) e.addEventListener(k.slice(2), v);
        else if (v != null && v !== false) e[k] = v;
      });
      e.append(...kids.flat().filter((k) => k != null));
      return e;
    }

    function cell(i, k, type, value) {
      return el('td', null, el('input', {
        type, value: value ?? '', disabled: readOnly,
        oninput: (e) => {
          checks[i] = checks[i] || {};
          checks[i][k] = k === 'off_flavors'
            ? e.target.value.split(',').map((s) => s.trim()).filter(Boolean)
            : (type === 'number' ? +e.target.value : e.target.value);
        },
      }));
    }

    function render() {
      container.replaceChildren(
        el('h2', null, 'Pour quality check'),
        el('table', null,
          el('tr', null, ['SKU', 'Foam cm', 'Retention s', 'Clarity', 'Off-flavors']
            .map((t) => el('th', null, t))),
          checks.map((row, i) => el('tr', null,
            el('td', null, row.sku || ''),
            cell(i, 'foam_cm', 'number', row.foam_cm),
            cell(i, 'retention_s', 'number', row.retention_s),
            cell(i, 'clarity', 'text', row.clarity),
            cell(i, 'off_flavors', 'text', (row.off_flavors || []).join(',')),
          )),
        ),
        ...(readOnly ? [] : [
          el('button', { onclick: () => save() }, 'Save progress'),
          el('button', { onclick: () => save('completed') }, 'Mark done'),
        ]),
      );
    }

    render();

    // Optional cleanup — host calls this if your step unmounts.
    return function cleanup() {};
  }

  window.__boss_register_step_plugin('pour-quality-check', mount);
})();
```

That's the whole plugin. ~70 lines, no build, no framework
runtime, no host-side changes.

### 3. Declare the row

A registry row needs `kind`, `version`, `status = "active"`, `label`,
`category`, `owning_team`, a `metadata_schema`, and `frontend_url` —
the bundle filename, e.g. `pour-quality-check.js`, which the gateway
resolves under `/var/lib/boss/step-plugins/`. Since 2026-09-18
(backlog 393d3234, consolidation H4) the row is DATA in the platform
bundle: create `infra/platform/step-plugins/pour-quality-check.toml`
holding one `[[step_plugin]]` named for the file, mirroring
`checklist.toml` there. `boss-platform-workflow-seed` publishes the
directory insert-if-missing by (kind, version) at every start, so a
fresh instance gets the row without replaying history; editing a row
is bumping its `version` (an unbumped edit is refused, naming the
field). Two lints hold the row to its JS:
`infra/lint/step-plugin-bundle-exists.sh` and the
`platform_step_plugins_bundle` test. Before that day the row's only
home was an `INSERT INTO step_plugins` in a migration; the seven that
did so are history and
`infra/lint/migrations-declare-schema-only.sh` refuses a new one.

A row can also be published live — `POST /api/jobs/step-plugins`
then `.../{kind}/publish` — which is how `sign-off` v2 arrived on
2026-08-19. The seed never rewrites a live row: an operator's later
version supersedes the file's, and the seed reports it.
`/system/step-plugins` lists the active rows.

### 4. Deploy

Land the car. The converge runner rebuilds the `step-plugins`
ConfigMap from this directory's `*.js` on every deploy
(`infra/forge/cluster-deploy-runner.sh`), and the seed publishes the
row on the next start. The SPA picks up the new plugin on the next
load (the plugin registry is fetched at boot; a hard refresh forces
it). Off-cluster, the gateway serves whatever is under
`/var/lib/boss/step-plugins/` (override via `BOSS_PLUGINS_DIR`) and
re-reads a file on the next request.

### 5. Use it

Author or update a Workflow that includes a step of
`kind=pour-quality-check` (via `/system/workflows`), open a Job of
that kind, and the step renders your plugin's surface instead of
the generic typed-fields form.

---

## Where each half lives

| Artefact | Home | Reaches a deployment by |
|---|---|---|
| the JS bundle | `infra/step-plugins/<name>.js` (this directory) | the `step-plugins` ConfigMap the converge runner builds from `*.js` |
| the registry row | `infra/platform/step-plugins/<kind>.toml` | `boss-platform-workflow-seed`, insert-if-missing by (kind, version) |

The two directories differ because the launchers already read them
differently: the image copies `infra/platform` whole and the seed runs
inside it, while the JS is mounted, not copied. `step-plugin-bundle-
exists.sh` refuses a row whose JS is absent from this directory.

---

## Current bundles

| File | Kind | What it does |
|---|---|---|
| `checklist.js` | `checklist` | Generic per-item-checked walkthrough; first v1 surface to land via the plugin path. |
| `sr-triage.js` | `sr-triage` | Mandatory intake fields (account, device, failure, priority) + optional Jira key + triage decision (dispatch / remote / parts-only). |
| `diagnostic-call.js` | `diagnostic-call` | Call log: schedule, channel, join URL, attendees, notes, optional recording URL. |
| `review-design.js` | `review-design` | Design-doc-review surface: per-`### Qn:` resolution textareas; gates completion on every open question having a recorded resolution, saved onto the step. |
| `answer-question.js` | `answer-question` | Question-and-response decision surface: the brief from the step's own metadata (falling back to the Job's filed message), then the answer. |
| `sign-off.js` | `sign-off` | The case being decided, the roster of roles that must stamp, and the stamp ceremony — so a sign-off is a choice rather than a button. |
| `correction-verdict.js` | `correction-verdict` | The correct-the-record gate: the false claim beside the measurement that contradicts it, each verdict labelled with what it causes. |
| `scope-declaration.js` | `scope-declaration` | The ship-a-change boundary declaration: asks what the car DOES and what it deliberately does NOT do (and why), with the branch, the packet it answers, and the gate receipt when there is one. |

---

## Anti-patterns

- **Don't bake business rules into the plugin.** The plugin
  renders a surface and reads/writes step metadata. Validation,
  authority gating, and state-machine rules live on the StepType
  + StepStatus / authority-role primitives. A plugin that
  hand-rolls "you can't move past this step until X" is fighting
  the platform.
- **Don't fetch the world on mount.** `props.step` (with its
  `metadata`) and `props.currentUser` arrive already loaded. Hit
  `/api/*` only for the step's own writes (the merge door and the
  status `PUT`) or a peer
  resource the host didn't hand you.
- **Don't keep state outside `step.metadata`.** The audit log is
  the system of record; metadata is its surface. Anything you
  hold in JS-only state disappears on refresh, can't be replayed,
  and breaks the correctness-protocol provenance property.
