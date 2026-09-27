<script lang="ts">
  // /it/registry/new — name a new kind, then open the authoring
  // workspace. Under D6 a Workflow is authored *through* a
  // `workflow-design` Job: this page collects the identity + headline
  // fields, creates the design Job (the slug becomes its immutable
  // subject id), seeds the publish step with an initial spec, and hands
  // off to WorkflowDesignWorkspace. No `workflows` row is written until
  // the author drives that Job to its publish step.

  import Breadcrumb from '@boss/web-kit/ui/Breadcrumb.svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import type { WorkflowSpec, StepSpec } from './workflowTypes';
  import { initialSpec, startDesignJob } from './designJob';
  import { session } from '@boss/web-kit/session/session.svelte';
  import { appToday } from '@boss/web-kit/sim-clock';
  import { href, navigate } from '../router';
  import {
    failedRead,
    loadingRead,
    readStateOfResponse,
    type ReadState,
  } from '../data/readState';

  let ownerId = $derived(
    session.value.kind === 'ready' ? session.value.user.id : '',
  );

  // The two suggestion reads and their outcomes (backlog aaeb02d6). Each
  // used to end in `if (!r.ok) return` and a bare catch, so a failure
  // kept the defaults and the form looked like a registry with no
  // categories and nothing beyond the built-in subject kinds. The form
  // still works without either — both only fill suggestions — so each
  // failure is a named line, not a blocked page.
  const WORKFLOWS_URL = '/api/workflows';
  const SUBJECT_KINDS_URL = '/api/subject-kinds';

  /// Fetch one suggestion read and parse it, or say why not.
  async function suggestionRead<T>(url: string): Promise<{ read: ReadState; body: T | null }> {
    try {
      const r = await fetch(url);
      const read = readStateOfResponse(url, r);
      return read.kind === 'ok' ? { read, body: (await r.json()) as T } : { read, body: null };
    } catch (e) {
      return { read: failedRead(`${url}: ${e instanceof Error ? e.message : String(e)}`), body: null };
    }
  }

  let categoryOptions = $state<string[]>([]);
  let workflowsRead = $state<ReadState>(loadingRead);
  $effect(() => {
    let cancelled = false;
    void (async () => {
      const { read, body } = await suggestionRead<Array<{ category?: string }>>(WORKFLOWS_URL);
      if (cancelled) return;
      workflowsRead = read;
      if (body) {
        categoryOptions = [...new Set(body.map((k) => k.category).filter((c): c is string => !!c))].sort();
      }
    })();
    return () => { cancelled = true; };
  });

  let subjectKindOptions = $state<string[]>([
    'asset', 'account', 'employee', 'vendor', 'campaign', 'purchase_order', 'custom',
  ]);
  let subjectKindsRead = $state<ReadState>(loadingRead);
  $effect(() => {
    let cancelled = false;
    void (async () => {
      const { read, body } = await suggestionRead<Array<{ kind: string }>>(SUBJECT_KINDS_URL);
      if (cancelled) return;
      subjectKindsRead = read;
      if (body) {
        subjectKindOptions = [...new Set([
          ...body.map((x) => x.kind),
          'asset', 'account', 'employee', 'vendor', 'campaign', 'purchase_order', 'custom',
        ])].sort();
      }
    })();
    return () => { cancelled = true; };
  });

  let kindSlug = $state('');
  let label = $state('');
  let category = $state('');
  let description = $state('');
  let subjectKinds = $state<string[]>(['asset']);
  // When forking, the source kind's steps seed the new draft instead of
  // the default single open-and-close step.
  let forkSource = $state<string | null>(null);
  let forkSteps = $state<ReadonlyArray<StepSpec> | null>(null);
  let starting = $state(false);
  let error = $state<string | null>(null);

  $effect(() => {
    const sp = new URLSearchParams(window.location.search);
    const fork = sp.get('fork');
    if (!fork) return;
    forkSource = fork;
    let cancelled = false;
    void (async () => {
      try {
        const r = await fetch(`/api/workflows/${encodeURIComponent(fork)}`);
        if (!r.ok) throw new Error(`HTTP ${r.status}`);
        const src = (await r.json()) as WorkflowSpec;
        if (cancelled) return;
        label = `${src.label} (fork)`;
        category = src.category;
        description = src.description ?? '';
        subjectKinds = [...src.subject_kinds];
        forkSteps = src.steps;
      } catch (e) {
        error = `Could not load fork source ${fork}: ${e instanceof Error ? e.message : String(e)}`;
      }
    })();
    return () => { cancelled = true; };
  });

  function toggleSubject(s: string): void {
    subjectKinds = subjectKinds.includes(s)
      ? subjectKinds.filter((x) => x !== s)
      : [...subjectKinds, s];
  }

  async function start(): Promise<void> {
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
    starting = true;
    try {
      // Guardrail: a brand-new kind shouldn't collide with an existing
      // one — that path is "new version" (Edit on the detail page).
      const existing = await fetch(`/api/workflows/${encodeURIComponent(kindSlug)}`);
      if (existing.ok) {
        error = `Kind "${kindSlug}" already exists — use Edit on its detail page to author a new version.`;
        starting = false;
        return;
      }
      const seed: WorkflowSpec = {
        ...initialSpec(kindSlug, label, category, subjectKinds, description || undefined),
        ...(forkSteps ? { steps: forkSteps } : {}),
      };
      const jobId = await startDesignJob(seed, ownerId, appToday(), {
        title: forkSource ? `Fork ${forkSource} → ${kindSlug}` : `Design ${kindSlug}`,
      });
      navigate(href(`/it/registry/authoring/${encodeURIComponent(jobId)}`));
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
      starting = false;
    }
  }
</script>

<div class="catalog theme-exec">
  <Breadcrumb to={href('/it/registry')}>← All job kinds</Breadcrumb>
  <PageHeader
    eyebrow="Platform · Job kind"
    title={forkSource ? `New job kind (forked from ${forkSource})` : 'New job kind'}
    subtitle="Name it, then build the trigger→outcome graph in the authoring workspace. Nothing publishes until you drive the design Job to its publish step."
  />

  {#if workflowsRead.kind === 'failed'}
    <p class="load-failed" role="alert">
      Couldn't load the existing job kinds — {workflowsRead.error}. Category has no suggestions; type one.
    </p>
  {/if}
  {#if subjectKindsRead.kind === 'failed'}
    <p class="load-failed" role="alert">
      Couldn't load subject kinds — {subjectKindsRead.error}. Only the built-in subject kinds are listed below.
    </p>
  {/if}

  <Section title="Identity">
    <div style="display:grid; gap:12px; max-width:800px">
      <div>
        <div style="font-size:12px; color:var(--static); margin-bottom:2px">
          Kind slug
          <span style="color:var(--static)"> — lowercase, hyphen-separated; becomes the kind's permanent identity</span>
        </div>
        <input bind:value={kindSlug} placeholder="seasonal-release" class="mono" style="padding:6px; font-size:13px; width:100%" />
      </div>
      <div>
        <div style="font-size:12px; color:var(--static); margin-bottom:2px">Label</div>
        <input bind:value={label} placeholder="Seasonal Release" style="padding:6px; font-size:13px; width:100%" />
      </div>
      <div>
        <div style="font-size:12px; color:var(--static); margin-bottom:2px">Category</div>
        <input list="category-options" bind:value={category} placeholder="production / sales / procurement / …" style="padding:6px; font-size:13px" />
        <datalist id="category-options">
          {#each categoryOptions as c (c)}<option value={c}></option>{/each}
        </datalist>
      </div>
      <div>
        <div style="font-size:12px; color:var(--static); margin-bottom:2px">
          Subject kinds <span style="color:var(--static)"> — what each Job of this kind is about</span>
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
        <div style="font-size:12px; color:var(--static); margin-bottom:2px">Description <span style="color:var(--static)"> — optional</span></div>
        <textarea bind:value={description} rows="3" placeholder="What this kind of Job accomplishes" style="padding:6px; font-size:13px; width:100%"></textarea>
      </div>
    </div>
  </Section>

  <div style="padding:0 24px 24px; display:flex; gap:12px; align-items:center">
    <button type="button" class="btn btn-primary" onclick={start} disabled={starting}>
      {starting ? 'Creating…' : 'Create & author →'}
    </button>
    {#if error}<span style="color:var(--err); font-size:13px">{error}</span>{/if}
  </div>
</div>
