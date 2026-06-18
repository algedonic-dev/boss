<script lang="ts">
  // Marketing Asset KB list.

  import PageHeader from '../ui/PageHeader.svelte';
  import FilterGroup from '../ui/FilterGroup.svelte';
  import FilterButton from '../ui/FilterButton.svelte';
  import SearchInput from '../ui/SearchInput.svelte';
  import EntityLink from '../ui/EntityLink.svelte';
  import { type MarketingAsset } from './types';
  import { loadClasses, classesFor } from '../session/classes.svelte';
  import { href, navigate } from '../router';

  // Kind labels + the filter rail come from the Class registry
  // (subject_kind='marketing-asset', member_attribute='kind') — no
  // hardcoded option list.
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
  let loading = $state(true);

  $effect(() => {
    const k = kind;
    const r = includeRetired;
    let cancelled = false;
    loading = true;
    (async () => {
      const qs = new URLSearchParams();
      if (k !== 'all') qs.set('kind', k);
      if (r) qs.set('include_retired', 'true');
      qs.set('limit', '500');
      try {
        const resp = await fetch(`/api/catalog/marketing-assets?${qs.toString()}`);
        if (resp.ok) {
          const body = (await resp.json()) as MarketingAsset[];
          if (!cancelled) assets = Array.isArray(body) ? body : [];
        }
      } catch {
        // ignore
      }
      if (!cancelled) loading = false;
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
        ...a.linked_device_skus,
        ...a.linked_campaign_ids,
      ]
        .join(' ')
        .toLowerCase();
      return hay.includes(q);
    });
  });
</script>

<div class="catalog theme-exec">
  <PageHeader
    eyebrow="Know"
    title={`Marketing assets (${assets.length}${loading ? '…' : ''})`}
    subtitle="Photos, videos, decks, one-pagers, templates, and brand files."
  />

  <div class="catalog-layout">
    <aside class="catalog-filters">
      <FilterGroup label="Search">
          <SearchInput bind:value={query} placeholder="Title, tag, SKU, campaign…" />
      </FilterGroup>
      <FilterGroup label="Kind">
          {#each kindOptions as k (k)}
            <FilterButton active={kind === k} onclick={() => (kind = k)}>
              {k === 'all' ? 'All' : (kindLabel.get(k) ?? k)}
            </FilterButton>
          {/each}
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
      {#if loading && assets.length === 0}
        <p class="empty">Loading…</p>
      {:else if visible.length === 0}
        <p class="empty">
          {assets.length === 0 ? 'No marketing assets yet.' : 'No assets match those filters.'}
        </p>
      {:else}
        <table class="data-table data-table-striped">
          <thead>
            <tr>
              <th>Asset</th>
              <th>Kind</th>
              <th>Tags</th>
              <th>Linked</th>
              <th>Owner</th>
              <th>Updated</th>
            </tr>
          </thead>
          <tbody>
            {#each visible as a (a.id)}
              {@const retired = Boolean(a.retired_at)}
              {@const linkedCount = a.linked_device_skus.length + a.linked_account_ids.length + a.linked_campaign_ids.length}
              <tr
                style={`cursor:pointer; opacity:${retired ? 0.55 : 1}`}
                onclick={() => navigate(href(`/marketing-assets/${encodeURIComponent(a.id)}`))}
              >
                <td>
                  <a
                    href={href(`/marketing-assets/${encodeURIComponent(a.id)}`)}
                    onclick={(e) => e.stopPropagation()}
                  >
                    {a.title}
                  </a>
                  {#if retired}
                    <span style="margin-left:6px; font-size:10px; padding:1px 6px; border-radius:3px; background:#e7e5e4; color:#57534e">
                      RETIRED
                    </span>
                  {/if}
                </td>
                <td>{a.kind ? (kindLabel.get(a.kind) ?? a.kind) : '—'}</td>
                <td style="color:#78716c; font-size:12px">
                  {a.tags.length > 0 ? a.tags.slice(0, 4).join(', ') : '—'}
                  {#if a.tags.length > 4} +{a.tags.length - 4}{/if}
                </td>
                <td style="color:#78716c; font-size:12px">
                  {linkedCount > 0 ? `${linkedCount} ${linkedCount === 1 ? 'link' : 'links'}` : '—'}
                </td>
                <td>
                  {#if a.owner_id}
                    <EntityLink kind="employee" id={a.owner_id} />
                  {:else}
                    <span style="color:#a8a29e">—</span>
                  {/if}
                </td>
                <td style="color:#78716c; font-size:12px">{a.updated_at.slice(0, 10)}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    </section>
  </div>
</div>
