<script lang="ts">
  // /admin/job-kinds/:slug/edit — graphical editor for an existing
  // kind. Editing always targets a DRAFT (D4): if a draft version
  // exists we continue it; otherwise we seed from the latest version
  // and the PUT creates a fresh draft (boss-jobs `create_draft` is
  // append-only and assigns the next version server-side). The active
  // row and any in-flight Jobs are never touched — only Publish
  // promotes a draft.

  import Breadcrumb from '../ui/Breadcrumb.svelte';
  import PageHeader from '../ui/PageHeader.svelte';
  import Section from '../ui/Section.svelte';
  import StepAuthoringSurface from './StepAuthoringSurface.svelte';
  import type { JobKindSpec, StepSpec } from './jobKindTypes';
  import { lintSteps } from './stepValidation';
  import { href, navigate } from '../router';

  type Props = { kindSlug: string };
  let { kindSlug }: Props = $props();

  type LoadState =
    | { kind: 'loading' }
    | { kind: 'error'; message: string }
    | { kind: 'ready' };
  let loadState = $state<LoadState>({ kind: 'loading' });

  // The full spec we're editing from, kept verbatim so the PUT
  // preserves fields the form doesn't expose (metadata, entitlements,
  // on_complete_create, …). Editable fields are mirrored into the
  // `$state` below and overlaid back at save.
  let baseSpec = $state<JobKindSpec | null>(null);
  // Whether `baseSpec` is itself a draft we're continuing, vs. a
  // published version we're branching a new draft from.
  let seededFromDraft = $state(false);
  let seedVersion = $state(0);

  let label = $state('');
  let category = $state('');
  let description = $state('');
  let subjectKinds = $state<string[]>([]);
  let steps = $state<ReadonlyArray<StepSpec>>([]);
  let saving = $state(false);
  let error = $state<string | null>(null);

  let subjectKindOptions = $state<string[]>([
    'asset', 'account', 'employee', 'vendor', 'campaign',
    'purchase_order', 'custom',
  ]);
  let categoryOptions = $state<string[]>([]);

  $effect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const [r, cats] = await Promise.all([
          fetch('/api/subject-kinds'),
          fetch('/api/jobs/kinds'),
        ]);
        if (cancelled) return;
        if (r.ok) {
          const rows = (await r.json()) as Array<{ kind: string }>;
          const merged = new Set([
            ...rows.map((x) => x.kind),
            'asset', 'account', 'employee', 'vendor', 'campaign',
            'purchase_order', 'custom',
          ]);
          subjectKindOptions = [...merged].sort();
        }
        if (cats.ok) {
          const kinds = (await cats.json()) as Array<{ category?: string }>;
          const set = new Set<string>();
          for (const k of kinds) if (k.category) set.add(k.category);
          categoryOptions = [...set].sort();
        }
      } catch {
        // Silent: fall back to the seeded defaults.
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  $effect(() => {
    const slug = kindSlug;
    let cancelled = false;
    void (async () => {
      try {
        const r = await fetch(
          `/api/jobs/kinds/${encodeURIComponent(slug)}/versions`,
        );
        if (!r.ok) throw new Error(`HTTP ${r.status}`);
        const versions = (await r.json()) as JobKindSpec[];
        if (cancelled) return;
        if (versions.length === 0) {
          loadState = { kind: 'error', message: `No versions found for ${slug}.` };
          return;
        }
        const sorted = [...versions].sort((a, b) => a.version - b.version);
        const draft = [...sorted].reverse().find((v) => v.status === 'draft');
        const seed = draft ?? sorted[sorted.length - 1]!;
        baseSpec = seed;
        seededFromDraft = draft != null;
        seedVersion = seed.version;
        label = seed.label;
        category = seed.category;
        description = seed.description ?? '';
        subjectKinds = [...seed.subject_kinds];
        steps = seed.steps;
        loadState = { kind: 'ready' };
      } catch (e) {
        loadState = {
          kind: 'error',
          message: e instanceof Error ? e.message : String(e),
        };
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

  let stepWarningCount = $derived(lintSteps(steps).length);

  async function save(): Promise<void> {
    error = null;
    if (!baseSpec) return;
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
      // Spread the loaded spec so unexposed fields survive; overlay the
      // edited ones and force `status: draft`. The server assigns the
      // next version + created_at and forces the kind to match the URL.
      const body = {
        ...baseSpec,
        label,
        description: description || null,
        category,
        subject_kinds: subjectKinds,
        steps,
        status: 'draft',
      };
      const r = await fetch(`/api/jobs/kinds/${encodeURIComponent(kindSlug)}`, {
        method: 'PUT',
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
  <Breadcrumb to={href(`/job-kinds/${encodeURIComponent(kindSlug)}`)}>
    ← Back to {kindSlug}
  </Breadcrumb>

  {#if loadState.kind === 'loading'}
    <p class="empty">Loading…</p>
  {:else if loadState.kind === 'error'}
    <PageHeader eyebrow="Platform · Job kind" title={kindSlug} subtitle={loadState.message} />
  {:else}
    <PageHeader
      eyebrow="Platform · Job kind"
      title={`Edit ${kindSlug}`}
      subtitle={seededFromDraft
        ? `Continuing draft v${seedVersion}. Saving records a new draft version.`
        : `Branching from v${seedVersion}. Saving creates a new draft — the active version and in-flight Jobs are untouched.`}
    />

    <Section title="Spec">
      <div style="display:grid; gap:12px; max-width:800px">
        <div>
          <div style="font-size:12px; color:#666; margin-bottom:2px">
            Kind slug
            <span style="color:#888"> — identity; fixed once created</span>
          </div>
          <input value={kindSlug} class="mono" disabled style="padding:6px; font-size:13px; width:100%; background:#f3f4f6; color:#666" />
        </div>

        <div>
          <div style="font-size:12px; color:#666; margin-bottom:2px">Label</div>
          <input bind:value={label} placeholder="Warranty Rework" style="padding:6px; font-size:13px; width:100%" />
        </div>

        <div>
          <div style="font-size:12px; color:#666; margin-bottom:2px">Category</div>
          <input
            list="category-options-edit"
            bind:value={category}
            placeholder="production / sales / procurement / …"
            style="padding:6px; font-size:13px"
          />
          <datalist id="category-options-edit">
            {#each categoryOptions as c (c)}
              <option value={c}></option>
            {/each}
          </datalist>
        </div>

        <div>
          <div style="font-size:12px; color:#666; margin-bottom:2px">
            Subject kinds
            <span style="color:#888"> — every Job of this kind carries one of these as its subject.</span>
          </div>
          <div style="display:flex; gap:8px; flex-wrap:wrap">
            {#each subjectKindOptions as s (s)}
              <label style="font-size:13px; display:flex; gap:4px; align-items:center">
                <input type="checkbox" checked={subjectKinds.includes(s)} onchange={() => toggleSubject(s)} />
                {s}
              </label>
            {/each}
          </div>
        </div>

        <div>
          <div style="font-size:12px; color:#666; margin-bottom:2px">Description <span style="color:#888"> — optional</span></div>
          <textarea bind:value={description} rows="3" style="padding:6px; font-size:13px; width:100%"></textarea>
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
      <button type="button" class="wb-btn wb-btn-primary" onclick={save} disabled={saving}>
        {saving ? 'Saving draft…' : 'Save draft'}
      </button>
      {#if stepWarningCount > 0}
        <span style="color:#92400e; font-size:13px">
          {stepWarningCount} step warning{stepWarningCount === 1 ? '' : 's'} — review above (publish runs a stricter server-side check)
        </span>
      {/if}
      {#if error}<span style="color:#dc2626; font-size:13px">{error}</span>{/if}
    </div>
  {/if}
</div>
