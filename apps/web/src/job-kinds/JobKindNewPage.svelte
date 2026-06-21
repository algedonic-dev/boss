<script lang="ts">
  // /admin/job-kinds/new — port of
  // apps/web/src/admin/JobKindNewPage.tsx.

  import Breadcrumb from '../ui/Breadcrumb.svelte';
  import PageHeader from '../ui/PageHeader.svelte';
  import Section from '../ui/Section.svelte';
  import StepAuthoringSurface from './StepAuthoringSurface.svelte';
  import type { JobKindSpec, StepSpec } from './jobKindTypes';
  import { lintSteps } from './stepValidation';
  import { appNow } from '../shell/sim-clock.svelte';
  import { href, navigate } from '../router';

  // A fresh draft starts with a single trigger step (ready_when =
  // "true") that is also terminal, so the minimal valid JobKind is
  // one open-and-close step. Authors edit from there.
  const DEFAULT_STEPS: ReadonlyArray<StepSpec> = [
    {
      title: 'first-step',
      kind: 'generic',
      ready_when: 'true',
      terminal: { outcome: 'completed' },
      title_template: '',
      sign_offs_required: [],
      authority_role: null,
      metadata_defaults: {},
    },
  ];

  // Existing categories come from the union of `category` fields
  // on every active JobKind — derived data, no hardcoded list.
  // Free-text input via <datalist> so authors can both pick a
  // common existing category and coin new ones.
  let categoryOptions = $state<string[]>([]);
  $effect(() => {
    let cancelled = false;
    (async () => {
      try {
        const r = await fetch('/api/jobs/kinds');
        if (!r.ok) return;
        const kinds = (await r.json()) as Array<{ category?: string }>;
        if (cancelled) return;
        const set = new Set<string>();
        for (const k of kinds) {
          if (k.category) set.add(k.category);
        }
        categoryOptions = [...set].sort();
      } catch {
        // Silent: empty datalist → free-text only, still functional.
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  // Subject kinds come from the SubjectKind registry. The
  // brewery's `recipe` and `equipment` Custom kinds appear here
  // alongside the four core kinds (account / vendor / employee /
  // location) the system ships.
  let subjectKindOptions = $state<string[]>([
    // Defaults rendered while the registry fetch is in flight.
    // Match the platform-shipped kinds; tenant Custom kinds slot
    // in once the API responds.
    'asset', 'account', 'employee', 'vendor', 'campaign',
    'purchase_order', 'custom',
  ]);
  $effect(() => {
    let cancelled = false;
    (async () => {
      try {
        const r = await fetch('/api/subject-kinds');
        if (!r.ok) return;
        const rows = (await r.json()) as Array<{ kind: string }>;
        if (cancelled) return;
        const fromRegistry = rows.map((x) => x.kind);
        // Union of registry kinds + the always-on core kinds the
        // SubjectKind registry doesn't enumerate (they're modeled as
        // closed enum branches on the Subject type).
        const merged = new Set([...fromRegistry, 'asset', 'account',
          'employee', 'vendor', 'campaign', 'purchase_order', 'custom']);
        subjectKindOptions = [...merged].sort();
      } catch {
        // Silent fallback to the seeded defaults above.
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  let kindSlug = $state('');
  let label = $state('');
  let category = $state('');
  let description = $state('');
  let subjectKinds = $state<string[]>(['asset']);
  let steps = $state<ReadonlyArray<StepSpec>>(DEFAULT_STEPS);
  let saving = $state(false);
  let error = $state<string | null>(null);
  let forkSource = $state<string | null>(null);

  $effect(() => {
    const sp = new URLSearchParams(window.location.search);
    const forkSlug = sp.get('fork');
    if (!forkSlug) return;
    forkSource = forkSlug;
    let cancelled = false;
    (async () => {
      try {
        const r = await fetch(`/api/jobs/kinds/${encodeURIComponent(forkSlug)}`);
        if (!r.ok) throw new Error(`HTTP ${r.status}`);
        const spec = (await r.json()) as JobKindSpec;
        if (cancelled) return;
        label = `${spec.label} (fork)`;
        category = spec.category;
        description = spec.description ?? '';
        subjectKinds = [...spec.subject_kinds];
        steps = spec.steps;
      } catch (e) {
        error = `Could not load source kind ${forkSlug}: ${e instanceof Error ? e.message : String(e)}`;
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  function toggleSubject(s: string): void {
    subjectKinds = subjectKinds.includes(s)
      ? subjectKinds.filter((x) => x !== s)
      : [...subjectKinds, s];
  }

  // Advisory lint surfaced next to the Create button. Non-blocking:
  // the authoritative viability check runs server-side at publish (and
  // live, on the graph, via StepAuthoringSurface's dry-run lint).
  let stepWarningCount = $derived(lintSteps(steps).length);

  async function submit(): Promise<void> {
    error = null;
    if (!/^[a-z][a-z0-9-]*$/.test(kindSlug)) {
      error = 'Kind slug must be lowercase alphanumeric with dashes (no leading digit).';
      return;
    }
    if (label.trim().length === 0) {
      error = 'Label is required.';
      return;
    }
    if (subjectKinds.length === 0) {
      error = 'Pick at least one subject kind.';
      return;
    }
    if (steps.length === 0) {
      error = 'A JobKind must have at least one step.';
      return;
    }
    saving = true;
    try {
      const body = {
        kind: kindSlug,
        version: 1,
        status: 'draft',
        label,
        description: description || null,
        category,
        subject_kinds: subjectKinds,
        steps,
        metadata_schema: {},
        entitlements: {},
        owning_team: 'authoring',
        authoring_job_id: null,
        created_at: appNow().toISOString(),
      };
      const r = await fetch('/api/jobs/kinds', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify(body),
      });
      if (!r.ok) throw new Error(`HTTP ${r.status}: ${await r.text()}`);
      navigate(href(`/admin/job-kinds/${encodeURIComponent(kindSlug)}`));
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
      saving = false;
    }
  }
</script>

<div class="catalog theme-exec">
  <Breadcrumb to={href('/job-kinds')}>
    ← All job kinds
  </Breadcrumb>
  <PageHeader
    eyebrow="Platform · Job kind"
    title={forkSource ? `New job kind (forked from ${forkSource})` : 'New job kind'}
    subtitle={forkSource
      ? `Pre-filled from ${forkSource}. Pick a fresh slug to create the new draft.`
      : 'Draft a new kind. Publish from the detail page to make it active.'}
  />

  <Section title="Spec">
      <div style="display:grid; gap:12px; max-width:800px">
        <div>
          <div style="font-size:12px; color:#666; margin-bottom:2px">
            Kind slug
            <span style="color:#888"> — Lowercase, hyphen-separated. e.g. seasonal-release</span>
          </div>
          <input
            bind:value={kindSlug}
            placeholder="seasonal-release"
            class="mono"
            style="padding:6px; font-size:13px; width:100%"
          />
        </div>

        <div>
          <div style="font-size:12px; color:#666; margin-bottom:2px">
            Label
            <span style="color:#888"> — Human-readable name shown in lists + detail views</span>
          </div>
          <input
            bind:value={label}
            placeholder="Warranty Rework"
            style="padding:6px; font-size:13px; width:100%"
          />
        </div>

        <div>
          <div style="font-size:12px; color:#666; margin-bottom:2px">Category</div>
          <input
            list="category-options"
            bind:value={category}
            placeholder="production / sales / procurement / …"
            style="padding:6px; font-size:13px"
          />
          <datalist id="category-options">
            {#each categoryOptions as c (c)}
              <option value={c}></option>
            {/each}
          </datalist>
        </div>

        <div>
          <div style="font-size:12px; color:#666; margin-bottom:2px">
            Subject kinds
            <span style="color:#888"> — Every Job of this kind will carry one of these as its subject.</span>
          </div>
          <div style="display:flex; gap:8px; flex-wrap:wrap">
            {#each subjectKindOptions as s (s)}
              <label style="font-size:13px; display:flex; gap:4px; align-items:center">
                <input
                  type="checkbox"
                  checked={subjectKinds.includes(s)}
                  onchange={() => toggleSubject(s)}
                />
                {s}
              </label>
            {/each}
          </div>
        </div>

        <div>
          <div style="font-size:12px; color:#666; margin-bottom:2px">
            Description
            <span style="color:#888"> — Optional. Shown on the detail view.</span>
          </div>
          <textarea
            bind:value={description}
            rows="3"
            style="padding:6px; font-size:13px; width:100%"
          ></textarea>
        </div>
      </div>
  </Section>

  <Section title="Steps" wide>
      <StepAuthoringSurface
        {steps}
        kindSlug={kindSlug}
        onChange={(s) => (steps = s)}
      />
  </Section>

  <div style="padding:0 24px 24px; display:flex; gap:12px; align-items:center">
    <button
      type="button"
      class="wb-btn wb-btn-primary"
      onclick={submit}
      disabled={saving}
    >
      {saving ? 'Creating draft…' : 'Create draft'}
    </button>
    {#if stepWarningCount > 0}
      <span style="color:#92400e; font-size:13px">
        {stepWarningCount} step warning{stepWarningCount === 1 ? '' : 's'} — review above (publish runs a stricter server-side check)
      </span>
    {/if}
    {#if error}<span style="color:#dc2626; font-size:13px">{error}</span>{/if}
  </div>
</div>
