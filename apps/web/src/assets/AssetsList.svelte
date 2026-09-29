<script lang="ts">
  // Assets list (kanban + table). Scope-reduced:
  // kanban rail + filterable list, same endpoints (/api/assets/summary,
  // /api/assets). The full page has a warranty-expiring + KB
  // panel that we're deferring to phase 2.

  import ClassesReadFailed from '@boss/web-kit/ui/ClassesReadFailed.svelte';
  import { navigate, href } from '../router';
  import Link from '@boss/web-kit/ui/Link.svelte';
  import { rowLink } from '@boss/web-kit/ui/RowLink';
  import { entityHref } from '@boss/web-kit/ui/entity-href';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import OverflowBanner from '@boss/web-kit/ui/OverflowBanner.svelte';
  import { fetchPaged, isCapped, type Paged } from '../data/paginated';
  import {
    failedRead, loadingRead, okRead, readStateOf, type ReadState,
  } from '../data/readState';
  import { getLabel } from '@boss/web-kit/session/manifest.svelte';
  import { loadClasses, classesFor } from '@boss/web-kit/session/classes.svelte';
  import type { Asset, AssetsSummary } from './types';
  import {
    assetsHeader, listLine, matchesQuery, overflowHint, phaseLabel, phaseTiles, summaryOf,
  } from './view';

  // Phase labels are the Class registry's `(asset, phase)` rows, and the
  // tiles are the phases the summary counted — no PHASE_ORDER here
  // (backlog 53fecfc9; its copy left `registered` out, 3da7e008).
  $effect(() => {
    void loadClasses('asset');
  });
  let phaseLabels = $derived(
    new Map(classesFor('asset', 'phase').map((c): [string, string] => [c.code, c.display_name])),
  );

  // One outcome PER READ, each starting in flight (backlog e1cb1ef3):
  // one try around a Promise.all threw away the list when the summary
  // failed, and the other way round.
  let devicesPage = $state<Paged<Asset> | null>(null);
  let listRead = $state<ReadState>(loadingRead);
  let summary = $state<AssetsSummary | null>(null);
  let summaryRead = $state<ReadState>(loadingRead);

  let phaseFilter = $state<string>('all');
  let query = $state('');

  let devices = $derived(devicesPage?.data ?? []);

  async function readSummary(): Promise<{ read: ReadState; body: AssetsSummary | null }> {
    try {
      const r = await fetch('/api/assets/summary');
      if (!r.ok) return { read: failedRead(`summary HTTP ${r.status}`), body: null };
      const body = summaryOf(await r.json());
      return body
        ? { read: okRead, body }
        : { read: failedRead('the summary is not in the shape the page reads'), body: null };
    } catch (e) {
      return { read: failedRead(e instanceof Error ? e.message : String(e)), body: null };
    }
  }

  $effect(() => {
    let cancelled = false;
    void fetchPaged<Asset>('/api/assets?limit=500').then((paged) => {
      if (cancelled) return;
      listRead = readStateOf(paged);
      if (paged.kind === 'ready') devicesPage = paged.page;
    });
    void readSummary().then((s) => {
      if (cancelled) return;
      summaryRead = s.read;
      summary = s.body;
    });
    return () => {
      cancelled = true;
    };
  });

  let pageTitle = $derived(getLabel('assets.page_title', 'tracked assets'));
  let header = $derived(assetsHeader({ read: summaryRead, body: summary }, pageTitle));
  let tiles = $derived(summary ? phaseTiles(summary.phase_counts, phaseLabels) : []);

  let visible = $derived(
    devices.filter((d) => (phaseFilter === 'all' || d.phase === phaseFilter) && matchesQuery(d, query)),
  );
  let line = $derived(listLine(listRead, devices.length, visible.length));
</script>

<div class="catalog theme-exec">
  <PageHeader
    eyebrow={getLabel('nav.assets_label', 'Assets')}
    title={header.title}
    subtitle={header.subtitle}
  />
  <ClassesReadFailed subjectKind="asset" what="asset phases" fallback="Phases show by code, not by their registry names." />

  {#if isCapped(devicesPage)}
    <OverflowBanner
      showing={devices.length}
      total={devicesPage!.total}
      noun="assets in the list below"
      hint={overflowHint(summaryRead)}
    />
  {/if}

  <!-- The tiles draw only what the summary answered: in flight they are
       absent, and a failed summary says so here instead of five zeros
       (backlog 93c1e5f1). -->
  {#if summaryRead.kind === 'failed'}
    <p class="empty load-failed" role="alert">Couldn't load the asset summary: {summaryRead.error}</p>
  {:else if tiles.length > 0}
    <section
      class="kanban"
      style="grid-template-columns: repeat({tiles.length}, 1fr)"
    >
      {#each tiles as t (t.phase)}
        <div class="kanban-col" data-stage={t.phase}>
          <div class="kanban-head">
            <div class="kanban-label">{t.label}</div>
            <div class="kanban-count">{t.count.toLocaleString()}</div>
          </div>
        </div>
      {/each}
    </section>
  {/if}

  <div class="catalog-layout" style="margin-top: 24px">
    <aside class="catalog-filters">
      <div class="filter-group">
        <div class="filter-label">Search</div>
        <input
          type="search"
          bind:value={query}
          placeholder="BOSS ID, SKU or serial…"
          class="search-input"
        />
      </div>
      <div class="filter-group">
        <div class="filter-label">Phase</div>
        <button
          type="button"
          class="filter-button {phaseFilter === 'all' ? 'filter-button-active' : ''}"
          aria-pressed={phaseFilter === 'all'}
          onclick={() => (phaseFilter = 'all')}
        >
          <!-- The count only when the summary gave it: "All (0)" sat
               beside a failure and under "Loading…" (93c1e5f1). -->
          {summary ? `All (${summary.total_systems.toLocaleString()})` : 'All'}
        </button>
        {#each tiles as t (t.phase)}
          {#if t.count > 0}
            <button
              type="button"
              class="filter-button {phaseFilter === t.phase ? 'filter-button-active' : ''}"
              aria-pressed={phaseFilter === t.phase}
              onclick={() => (phaseFilter = t.phase)}
            >
              {t.label} ({t.count.toLocaleString()})
            </button>
          {/if}
        {/each}
      </div>
    </aside>

    <section class="list-section">
      {#if line.kind === 'loading'}
        <p class="empty">Loading…</p>
      {:else if line.kind === 'failed'}
        <p class="empty load-failed" role="alert">Couldn't load assets: {line.error}</p>
      {:else if line.kind === 'none-tracked'}
        <p class="empty">No {pageTitle} yet.</p>
      {:else if line.kind === 'none-match'}
        <p class="empty">{getLabel('assets.empty_state', 'No asset matches the search and phase filter.')}</p>
      {:else}
        <table class="data-table data-table-striped">
          <thead>
            <tr>
              <th>BOSS ID</th>
              <th>SKU</th>
              <th>Phase</th>
              <th>Holder</th>
              <th>Warranty</th>
              <th class="num">{getLabel('assets.tickets_label', 'Open SRs')}</th>
              <th>Last event</th>
            </tr>
          </thead>
          <tbody>
            {#each visible as d (d.asset_id)}
              <tr
                use:rowLink={{
                  onActivate: () => navigate(entityHref('asset', d.asset_id)),
                  label: `Asset ${d.asset_id}`,
                }}
              >
                <td class="mono">
                  <Link to={entityHref('asset', d.asset_id)}>{d.asset_id}</Link>
                </td>
                <td class="mono">{d.sku ?? '—'}</td>
                <td>{phaseLabel(phaseLabels, d.phase)}</td>
                <td class="mono">{d.holder_id ?? '—'}</td>
                <!-- No date on record is unknown, not expired: it printed
                     "out" (backlog 8429a34a). -->
                <td>{d.warranty_through ?? '—'}</td>
                <td class="num">{d.open_ticket_count}</td>
                <td>{d.last_event_at}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    </section>
  </div>
</div>
