<script lang="ts">
  // /it/registry — the Workflow registry's catalog: every active
  // Workflow, grouped by category, each linking to its detail page
  // (/it/registry/<kind>), with how many packets of that kind are in
  // flight now, and the version each runs, when it was published and,
  // where there is one, the packet that authored it.
  //
  // Two reads, judged apart: GET /api/workflows is the catalog, and
  // GET /api/jobs/live is only the in-flight counts. A failed counts
  // read keeps the catalog and says so on its own line — it used to be
  // dropped in silence, which painted zero in flight on every kind
  // (page audit 9da74410, backlog 398913af).

  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import Link from '@boss/web-kit/ui/Link.svelte';
  import StatusChip from '@boss/web-kit/ui/StatusChip.svelte';
  import { formatDate } from '@boss/web-kit/ui/date';
  import type { WorkflowSpec } from '../../workflows/workflowTypes';
  import { href } from '../../router';

  let kinds = $state<ReadonlyArray<WorkflowSpec>>([]);
  let liveCounts = $state<Readonly<Record<string, number>>>({});
  let loading = $state(true);
  let error = $state<string | null>(null);
  /// Why the in-flight counts are missing, or null when they were read.
  let liveError = $state<string | null>(null);
  let query = $state('');

  /// The in-flight read's outcome, as the words its failure line says.
  async function readLive(r: PromiseSettledResult<Response>): Promise<
    { ok: true; counts: Record<string, number> } | { ok: false; why: string }
  > {
    if (r.status === 'rejected') {
      return { ok: false, why: `failed: ${r.reason instanceof Error ? r.reason.message : String(r.reason)}` };
    }
    if (!r.value.ok) return { ok: false, why: `answered ${r.value.status}` };
    try {
      const body = (await r.value.json()) as { counts?: Record<string, number> };
      return { ok: true, counts: body.counts ?? {} };
    } catch {
      return { ok: false, why: `answered ${r.value.status} with a body that is not JSON` };
    }
  }

  $effect(() => {
    let cancelled = false;
    loading = true;
    (async () => {
      const [kindsR, liveR] = await Promise.allSettled([
        fetch('/api/workflows'),
        fetch('/api/jobs/live'),
      ]);
      const live = await readLive(liveR);
      if (!cancelled) {
        liveCounts = live.ok ? live.counts : {};
        liveError = live.ok ? null : `In-flight counts unavailable — GET /api/jobs/live ${live.why}.`;
      }
      try {
        if (kindsR.status === 'rejected') throw kindsR.reason;
        if (!kindsR.value.ok) throw new Error(`HTTP ${kindsR.value.status}: ${await kindsR.value.text()}`);
        const body = (await kindsR.value.json()) as WorkflowSpec[];
        if (!cancelled) {
          kinds = body;
          error = null;
        }
      } catch (e) {
        if (!cancelled) error = e instanceof Error ? e.message : String(e);
      } finally {
        if (!cancelled) loading = false;
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  let filtered = $derived.by(() => {
    if (!query.trim()) return kinds;
    const q = query.trim().toLowerCase();
    return kinds.filter((k) => {
      const hay = `${k.kind} ${k.label} ${k.category} ${k.subject_kinds.join(' ')} ${k.description ?? ''}`.toLowerCase();
      return hay.includes(q);
    });
  });

  let byCategory = $derived.by(() => {
    const m = new Map<string, WorkflowSpec[]>();
    for (const k of filtered) {
      const arr = m.get(k.category) ?? [];
      arr.push(k);
      m.set(k.category, arr);
    }
    for (const [, arr] of m) arr.sort((a, b) => a.kind.localeCompare(b.kind));
    return m;
  });
  let categoryKeys = $derived([...byCategory.keys()].sort());

  function describe(k: WorkflowSpec): string {
    if (k.description) {
      // Strip newlines so the prose flows; keep first paragraph.
      return k.description.replace(/\s+/g, ' ').trim().slice(0, 220);
    }
    return '';
  }
</script>

<div class="catalog theme-exec">
  <PageHeader
    eyebrow="IT · Registry"
    title="Workflows"
    subtitle={loading
      ? 'Loading…'
      : error
        ? // "0 active Workflows" above the failure line read as an empty
          // registry (sweep c3e4edcc). The count is unknown, so say so.
          'Workflow count unknown — the registry read failed'
        : `${kinds.length} active Workflow${kinds.length === 1 ? '' : 's'} across ${categoryKeys.length} categor${categoryKeys.length === 1 ? 'y' : 'ies'} — every kind of work this instance runs`}
  />

  {#if error}
    <!-- The shared failure marker (sweep c3e4edcc); an inline --err
         colour would outrank its troubled ink, so only the margin here. -->
    <p class="empty load-failed" role="alert" style="margin:0 24px">Failed to load: {error}</p>
  {/if}
  {#if liveError && !loading}
    <!-- The counts are their own read: the catalog below stays, and the
         missing chips are said to be missing rather than zero (398913af). -->
    <p class="empty load-failed" role="alert" style="margin:0 24px 12px">{liveError}</p>
  {/if}

  <div class="wf-toolbar">
    <input
      type="search"
      placeholder="Search workflows by kind, label, category…"
      bind:value={query}
      class="wf-search"
    />
    <!-- Authoring entry point. Workflows is the single UI surface for
         Workflows (the "Job kinds" sidebar entry was retired); the
         authoring routes (/it/registry*) are reached from here. -->
    <Link to={href('/it/registry/new')} className="wf-new">+ New workflow</Link>
  </div>

  <div class="tab-grid">
    {#each categoryKeys as cat (cat)}
      {@const rows = byCategory.get(cat) ?? []}
      <Section title={`${cat} (${rows.length})`} wide>
          <ul class="kb-workflow-list">
            {#each rows as k (k.kind)}
              <li class="kb-workflow-row">
                <div class="kb-workflow-header">
                  <Link to={href(`/it/registry/${encodeURIComponent(k.kind)}`)}>
                    <span class="kb-workflow-kind mono">{k.kind}</span>
                  </Link>
                  <span class="kb-workflow-label">{k.label}</span>
                  {#if (liveCounts[k.kind] ?? 0) > 0}
                    <span class="kb-workflow-live">
                      {liveCounts[k.kind]} in flight
                    </span>
                  {/if}
                  <span class="kb-workflow-tiers">
                    {k.steps.length} step{k.steps.length === 1 ? '' : 's'}
                  </span>
                </div>
                <div class="kb-workflow-meta">
                  Subject:
                  {#each k.subject_kinds as s (s)}
                    <span style="margin-right:4px"><StatusChip value={s} tone="muted" /></span>
                  {/each}
                  {#if k.owning_team !== 'platform'}
                    · owning team: <span class="mono">{k.owning_team}</span>
                  {/if}
                  · v{k.version} · published {formatDate(k.created_at)}
                  <!-- The registry's out is its publishes, and the packet
                       that authored one is the record of why (6147420a). -->
                  {#if k.authoring_job_id}
                    · <Link to={href(`/ux/jobs/${encodeURIComponent(k.authoring_job_id)}`)}>authoring packet</Link>
                  {/if}
                </div>
                {#if describe(k)}
                  <p class="kb-workflow-desc">{describe(k)}</p>
                {/if}
              </li>
            {/each}
          </ul>
      </Section>
    {/each}
    <!-- A failed read, an empty registry and a search miss are three
         different facts, and each says only its own (384160e5). -->
    {#if !loading && !error}
      {#if kinds.length === 0}
        <p class="empty" style="padding:24px">The registry holds 0 active workflows.</p>
      {:else if categoryKeys.length === 0}
        <p class="empty" style="padding:24px">No Workflows match "{query}".</p>
      {/if}
    {/if}
  </div>
</div>

<style>
  .wf-toolbar {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 0 24px 16px;
    max-width: 760px;
  }
  .wf-search {
    flex: 1 1 auto;
    max-width: 520px;
    padding: 8px 10px;
    font-size: 14px;
    border: 1px solid var(--hairline);
    border-radius: 6px;
  }
  .catalog :global(a.wf-new) {
    flex: 0 0 auto;
    padding: 8px 14px;
    font-size: 14px;
    font-weight: 600;
    border-radius: 6px;
    background: var(--signal);
    color: var(--on-band);
    text-decoration: none;
    white-space: nowrap;
  }
  .catalog :global(a.wf-new:hover) {
    background: var(--fog);
  }
  .kb-workflow-list {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .kb-workflow-row {
    padding: 12px 0;
    border-bottom: 1px solid var(--hairline);
  }
  .kb-workflow-row:last-child {
    border-bottom: none;
  }
  .kb-workflow-header {
    display: flex;
    align-items: baseline;
    gap: 12px;
    flex-wrap: wrap;
  }
  .kb-workflow-kind {
    font-size: 14px;
    font-weight: 500;
  }
  .kb-workflow-label {
    color: var(--static);
    font-size: 14px;
  }
  .kb-workflow-tiers {
    color: var(--static);
    font-size: 12px;
    margin-left: auto;
  }
  .kb-workflow-live {
    color: var(--warn);
    background: var(--warn-wash);
    padding: 2px 8px;
    border-radius: 999px;
    font-size: 12px;
    font-weight: 500;
  }
  .kb-workflow-meta {
    color: var(--static);
    font-size: 12px;
    margin-top: 4px;
  }
  .kb-workflow-desc {
    color: var(--static);
    font-size: 13px;
    margin: 6px 0 0;
    line-height: 1.45;
  }
</style>
