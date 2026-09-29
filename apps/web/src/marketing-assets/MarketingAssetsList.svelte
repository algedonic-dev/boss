<script lang="ts">
  // Marketing Asset KB list.

  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import { formatDate } from '@boss/web-kit/ui/date';
  import FilterGroup from '@boss/web-kit/ui/FilterGroup.svelte';
  import FilterButton from '@boss/web-kit/ui/FilterButton.svelte';
  import SearchInput from '@boss/web-kit/ui/SearchInput.svelte';
  import EntityLink from '@boss/web-kit/ui/EntityLink.svelte';
  import SortHeader from '@boss/web-kit/ui/SortHeader.svelte';
  import { createSortState } from '@boss/web-kit/ui/sort-state.svelte';
  import { type MarketingAsset } from './types';
  import { loadClasses, classesFor } from '@boss/web-kit/session/classes.svelte';
  import ClassesReadFailed from '@boss/web-kit/ui/ClassesReadFailed.svelte';
  import { href, navigate } from '../router';
  import Link from '@boss/web-kit/ui/Link.svelte';
  import { rowLink } from '@boss/web-kit/ui/RowLink';
  import { loadOwnerNames, ownerIdsOf } from '../data/ownerNames';
  import { emptyState, okRead, readStateOfLoad, type ReadState } from '../data/readState';
  import { readList } from '../data/shape';
  import ListEmpty from '../data/ListEmpty.svelte';

  // Kind labels + the filter rail come from the Class registry
  // (subject_kind='marketing-asset', member_attribute='kind') — no
  // hardcoded option list. `ClassesReadFailed` says when that read FAILED: the
  // rail used to fall to "All" alone and kinds to raw codes with no line
  // saying why (backlog e520c794).
  $effect(() => {
    void loadClasses('marketing-asset');
  });
  let kindRows = $derived(classesFor('marketing-asset', 'kind'));
  let kindLabel = $derived(
    new Map(kindRows.map((c): [string, string] => [c.code, c.display_name])),
  );
  let kindOptions = $derived<ReadonlyArray<string>>([
    'all',
    ...kindRows.map((c) => c.code),
  ]);

  let kind = $state<string>('all');
  let includeRetired = $state(false);
  let query = $state('');
  let assets = $state<MarketingAsset[]>([]);
  /// Non-null when the load failed — rendered instead of the empty
  /// state, so an outage never reads as "no marketing assets yet"
  /// (packet 3fba9c35, the false-empty sweep).
  let loadFailed = $state<string | null>(null);
  let loading = $state(true);

  /// The one page the list reads. The answer carries no `total`, so an
  /// answer holding exactly this many rows may have more behind it, and
  /// the page says so rather than titling a truncated list as the whole
  /// (backlog a14de49e; a server total is more machinery than the 43
  /// seeded rows justify).
  const LIMIT = 500;
  const ASSETS_URL = '/api/catalog/marketing-assets';
  let maybeMore = $derived(!loading && loadFailed === null && assets.length >= LIMIT);

  $effect(() => {
    const k = kind;
    const r = includeRetired;
    let cancelled = false;
    loading = true;
    (async () => {
      const qs = new URLSearchParams();
      if (k !== 'all') qs.set('kind', k);
      if (r) qs.set('include_retired', 'true');
      qs.set('limit', String(LIMIT));
      try {
        const resp = await fetch(`${ASSETS_URL}?${qs.toString()}`);
        if (resp.ok) {
          // Only a list is the assets: any other 200 was coerced to `[]`
          // and read "No marketing assets yet." (backlog 0ef5e008).
          const body = readList(ASSETS_URL, await resp.json()) as MarketingAsset[];
          if (!cancelled) {
            assets = [...body];
            loadFailed = null;
          }
        } else {
          if (!cancelled) loadFailed = `${ASSETS_URL}: HTTP ${resp.status}`;
        }
      } catch (e) {
        if (!cancelled) loadFailed = e instanceof Error ? e.message : String(e);
      }
      if (!cancelled) loading = false;
    })();
    return () => {
      cancelled = true;
    };
  });

  // Owners by name, one read per distinct owner, with the detail page's
  // failure line — the Owner column used to print raw employee ids while
  // the detail page named the same people (backlog 7ea34901).
  let empNames = $state<ReadonlyMap<string, string>>(new Map());
  let namesRead = $state<ReadState>(okRead);
  let ownerKey = $derived(ownerIdsOf(assets).join('\n'));
  $effect(() => {
    const ids = ownerKey ? ownerKey.split('\n') : [];
    let cancelled = false;
    (async () => {
      const out = await loadOwnerNames(ids);
      if (!cancelled) {
        empNames = out.names;
        namesRead = out.read;
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  let visible = $derived.by(() => {
    if (!query) return assets;
    const q = query.toLowerCase();
    return assets.filter((a) => {
      const hay = [
        a.id,
        a.title,
        a.description ?? '',
        ...a.tags,
        ...a.linked_skus,
        ...a.linked_campaign_ids,
      ]
        .join(' ')
        .toLowerCase();
      return hay.includes(q);
    });
  });

  // Read failed, none, or the search hid them (backlog 0ef5e008). The
  // Kind filter is the SERVER's (`?kind=`), so an empty answer under it
  // says no asset of that kind exists — a none, in that kind's name.
  let listState = $derived(
    emptyState(
      [{ source: ASSETS_URL, state: readStateOfLoad(loading && assets.length === 0, loadFailed) }],
      assets.length,
      visible.length,
    ),
  );
  let noneLine = $derived(
    kind === 'all' ? 'No marketing assets yet.' : `No ${kindLabel.get(kind) ?? kind} assets yet.`,
  );

  // CAR-4: previously rendered in API arrival order. Freshest-updated
  // first is the new landing order; the count columns default
  // descending.
  type SortKey = 'title' | 'kind' | 'tags' | 'linked' | 'owner' | 'updated';
  const DESC_FIRST: ReadonlyArray<SortKey> = ['tags', 'linked', 'updated'];
  const sort = createSortState<SortKey>({ key: 'updated', dir: 'desc' }, (k) =>
    DESC_FIRST.includes(k) ? 'desc' : 'asc',
  );
  let visibleSorted = $derived(
    sort.sorted(visible, {
      title: (a) => a.title,
      kind: (a) => a.kind,
      tags: (a) => a.tags.length,
      linked: (a) =>
        a.linked_skus.length + a.linked_account_ids.length + a.linked_campaign_ids.length,
      owner: (a) => (a.owner_id ? (empNames.get(a.owner_id) ?? a.owner_id) : null),
      updated: (a) => a.updated_at,
    }),
  );
</script>

<div class="catalog theme-exec">
  <PageHeader
    eyebrow="Know"
    title={`Marketing assets (${assets.length}${maybeMore ? '+' : ''}${loading ? '…' : ''})`}
  />
  <!-- The subtitle enumerated six kinds by hand while the kinds are
       Class registry rows (nine live on 2026-09-28); the Kind rail lists
       them from the registry, so the copy went (backlog e2b6a801). -->

  <div class="catalog-layout">
    <aside class="catalog-filters">
      <FilterGroup label="Search">
          <SearchInput bind:value={query} placeholder="Title, tag, SKU, campaign…" label="Search marketing assets" />
      </FilterGroup>
      <FilterGroup label="Kind">
          {#each kindOptions as k (k)}
            <FilterButton active={kind === k} onclick={() => (kind = k)}>
              {k === 'all' ? 'All' : (kindLabel.get(k) ?? k)}
            </FilterButton>
          {/each}
          <ClassesReadFailed subjectKind="marketing-asset" what="asset kinds" fallback="Kinds show as codes." />
      </FilterGroup>
      <FilterGroup label="Status">
          <FilterButton active={!includeRetired} onclick={() => (includeRetired = false)}>
            Active only
          </FilterButton>
          <FilterButton active={includeRetired} onclick={() => (includeRetired = true)}>
            Include retired
          </FilterButton>
      </FilterGroup>
    </aside>

    <section class="list-section">
      {#if listState.kind !== 'rows'}
        <ListEmpty
          view={listState}
          words={{ what: 'marketing assets', noun: 'assets', none: noneLine }}
        />
      {:else}
        {#if maybeMore}
          <p class="list-truncated" style="color:var(--static); font-size:13px; margin:0 0 8px">
            This list shows the first {LIMIT} assets, and there may be more — narrow it by kind.
          </p>
        {/if}
        {#if namesRead.kind === 'failed'}
          <p class="load-failed" role="alert">
            Couldn't load the owners' names — {namesRead.error}. People show as ids.
          </p>
        {/if}
        <table class="data-table data-table-striped">
          <thead>
            <tr>
              <SortHeader {sort} key="title">Asset</SortHeader>
              <SortHeader {sort} key="kind">Kind</SortHeader>
              <SortHeader {sort} key="tags">Tags</SortHeader>
              <SortHeader {sort} key="linked">Linked</SortHeader>
              <SortHeader {sort} key="owner">Owner</SortHeader>
              <SortHeader {sort} key="updated">Updated</SortHeader>
            </tr>
          </thead>
          <tbody>
            {#each visibleSorted as a (a.id)}
              {@const retired = Boolean(a.retired_at)}
              {@const linkedCount = a.linked_skus.length + a.linked_account_ids.length + a.linked_campaign_ids.length}
              {@const to = href(`/ux/marketing-assets/${encodeURIComponent(a.id)}`)}
              <tr
                style={`opacity:${retired ? 0.55 : 1}`}
                use:rowLink={{ onActivate: () => navigate(to), label: `${a.title} (${a.id})` }}
              >
                <td>
                  <Link to={to}>{a.title}</Link>
                  {#if retired}
                    <span class="chip" style="margin-left:6px">RETIRED</span>
                  {/if}
                </td>
                <td>{a.kind ? (kindLabel.get(a.kind) ?? a.kind) : '—'}</td>
                <td style="color:var(--static); font-size:12px">
                  {a.tags.length > 0 ? a.tags.slice(0, 4).join(', ') : '—'}
                  {#if a.tags.length > 4} +{a.tags.length - 4}{/if}
                </td>
                <td style="color:var(--static); font-size:12px">
                  {linkedCount > 0 ? `${linkedCount} ${linkedCount === 1 ? 'link' : 'links'}` : '—'}
                </td>
                <td>
                  {#if a.owner_id}
                    <EntityLink kind="employee" id={a.owner_id} label={empNames.get(a.owner_id)} />
                  {:else}
                    <span style="color:var(--static)">—</span>
                  {/if}
                </td>
                <td style="color:var(--static); font-size:12px">{formatDate(a.updated_at)}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    </section>
  </div>
</div>
