<script lang="ts">
  // /workflows — Knowledge Base view of active JobKinds.
  //
  // In the three-axis IA, Workflows is the *what* —
  // every active JobKind in the registry, browseable like a
  // catalog. Editing JobKinds happens at /admin/job-kinds (the
  // Surface for the platform-eng role). This page is the
  // read-only KB view non-admin operators land on when they
  // want to understand "what kinds of work does this brewery
  // run?"
  //
  // Reads the same `/api/jobs/kinds` endpoint as the admin
  // page; renders categories grouped + step-graph summary
  // per kind. Detail click drops the operator into the
  // canonical detail page (which is shared with admin since
  // the data is the same).
  //
  // When the JobKind-as-Subject promotion lands (the
  // `custom_kind = "job-kind"` Subject discriminator from
  // the job-kind-self-bootstrap design), this page
  // becomes the "Equipment KB"-shaped sidebar entry that
  // every Subject kind owns — same shape as /catalog,
  // /accounts, /vendors, etc.

  import PageHeader from '../ui/PageHeader.svelte';
  import Section from '../ui/Section.svelte';
  import Link from '../ui/Link.svelte';
  import type { JobKindSpec } from '../job-kinds/jobKindTypes';
  import { href } from '../router';

  let kinds = $state<ReadonlyArray<JobKindSpec>>([]);
  let liveCounts = $state<Readonly<Record<string, number>>>({});
  let loading = $state(true);
  let error = $state<string | null>(null);
  let query = $state('');

  $effect(() => {
    let cancelled = false;
    loading = true;
    (async () => {
      try {
        // Two parallel fetches: the JobKind catalog (static-ish)
        // + the live in-flight count per kind. The catalog
        // grounds the page; the counts make it operational
        // ("12 wholesale-keg-orders in flight right now").
        const [kindsR, liveR] = await Promise.allSettled([
          fetch('/api/jobs/kinds'),
          fetch('/api/jobs/live'),
        ]);
        if (kindsR.status === 'fulfilled' && kindsR.value.ok) {
          const body = (await kindsR.value.json()) as JobKindSpec[];
          if (!cancelled) kinds = body;
        } else if (kindsR.status === 'fulfilled') {
          throw new Error(`HTTP ${kindsR.value.status}: ${await kindsR.value.text()}`);
        } else {
          throw kindsR.reason;
        }
        if (liveR.status === 'fulfilled' && liveR.value.ok) {
          const body = (await liveR.value.json()) as { counts?: Record<string, number> };
          if (!cancelled) liveCounts = body.counts ?? {};
        }
        if (!cancelled) error = null;
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
    const m = new Map<string, JobKindSpec[]>();
    for (const k of filtered) {
      const arr = m.get(k.category) ?? [];
      arr.push(k);
      m.set(k.category, arr);
    }
    for (const [, arr] of m) arr.sort((a, b) => a.kind.localeCompare(b.kind));
    return m;
  });
  let categoryKeys = $derived([...byCategory.keys()].sort());

  function describe(k: JobKindSpec): string {
    if (k.description) {
      // Strip newlines so the prose flows; keep first paragraph.
      return k.description.replace(/\s+/g, ' ').trim().slice(0, 220);
    }
    return '';
  }
</script>

<div class="catalog theme-exec">
  <PageHeader
    eyebrow="Knowledge Base · Workflows"
    title="Workflows"
    subtitle={loading
      ? 'Loading…'
      : `${kinds.length} active JobKind${kinds.length === 1 ? '' : 's'} across ${categoryKeys.length} categor${categoryKeys.length === 1 ? 'y' : 'ies'} — every kind of work the brewery runs`}
  />

  {#if error}
    <p class="empty" style="color:#dc2626; padding:0 24px">Failed to load: {error}</p>
  {/if}

  <div style="padding:0 24px 16px; max-width:520px">
    <input
      type="search"
      placeholder="Search workflows by kind, label, category…"
      bind:value={query}
      style="width:100%; padding:8px 10px; font-size:14px; border:1px solid #e5e5e5; border-radius:6px"
    />
  </div>

  <div class="tab-grid">
    {#each categoryKeys as cat (cat)}
      {@const rows = byCategory.get(cat) ?? []}
      <Section title={`${cat} (${rows.length})`} wide>
          <ul class="kb-workflow-list">
            {#each rows as k (k.kind)}
              <li class="kb-workflow-row">
                <div class="kb-workflow-header">
                  <Link to={href(`/admin/job-kinds/${encodeURIComponent(k.kind)}`)}>
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
                    <span class="chip chip-stage chip-stage-muted" style="margin-right:4px">{s}</span>
                  {/each}
                  {#if k.owning_team !== 'platform'}
                    · owning team: <span class="mono">{k.owning_team}</span>
                  {/if}
                  · v{k.version}
                </div>
                {#if describe(k)}
                  <p class="kb-workflow-desc">{describe(k)}</p>
                {/if}
              </li>
            {/each}
          </ul>
      </Section>
    {/each}
    {#if !loading && categoryKeys.length === 0}
      <p class="empty" style="padding:24px">No JobKinds match "{query}".</p>
    {/if}
  </div>
</div>

<style>
  .kb-workflow-list {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .kb-workflow-row {
    padding: 12px 0;
    border-bottom: 1px solid #f3f4f6;
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
    color: #374151;
    font-size: 14px;
  }
  .kb-workflow-tiers {
    color: #78716c;
    font-size: 12px;
    margin-left: auto;
  }
  .kb-workflow-live {
    color: #b45309;
    background: #fef3c7;
    padding: 2px 8px;
    border-radius: 999px;
    font-size: 12px;
    font-weight: 500;
  }
  .kb-workflow-meta {
    color: #6b7280;
    font-size: 12px;
    margin-top: 4px;
  }
  .kb-workflow-desc {
    color: #4b5563;
    font-size: 13px;
    margin: 6px 0 0;
    line-height: 1.45;
  }
</style>
