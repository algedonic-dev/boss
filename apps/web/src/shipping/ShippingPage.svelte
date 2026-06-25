<script lang="ts">
  // Shipping dashboard — port of apps/web/src/shipping/ShippingPage.tsx.

  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import { entityHref } from '@boss/web-kit/ui/entity-href';
  import FilterGroup from '@boss/web-kit/ui/FilterGroup.svelte';
  import FilterButton from '@boss/web-kit/ui/FilterButton.svelte';
  import SearchInput from '@boss/web-kit/ui/SearchInput.svelte';
  import EntityLink from '@boss/web-kit/ui/EntityLink.svelte';
  import Link from '@boss/web-kit/ui/Link.svelte';
  import OverflowBanner from '@boss/web-kit/ui/OverflowBanner.svelte';
  import { fetchPaged, isCapped, type Paged } from '../data/paginated';
  import {
    CARRIER_LABEL,
    DIRECTION_LABEL,
    STATUS_LABEL,
    type Shipment,
    type ShipmentDirection,
    type ShipmentStatus,
  } from './types';
  import { href } from '../router';

  type Tab = 'inbound' | 'outbound' | 'all';
  type StatusFilter = ShipmentStatus | 'all' | 'undelivered';

  const TAB_LIST: ReadonlyArray<{ id: Tab; label: string }> = [
    { id: 'all', label: 'All' },
    { id: 'inbound', label: 'Inbound' },
    { id: 'outbound', label: 'Outbound' },
  ];

  const STATUSES: ReadonlyArray<ShipmentStatus> = [
    'label-created', 'picked-up', 'in-transit', 'delivered', 'exception',
  ];

  let shipmentsPage = $state<Paged<Shipment> | null>(null);
  let loading = $state(true);
  let tab = $state<Tab>('all');
  // Default to undelivered — the working queue (label-created,
  // picked-up, in-transit, exception). Delivered shipments
  // remain accessible behind the "Delivered" chip and the "All"
  // chip; they just don't dominate the default view.
  let statusFilter = $state<StatusFilter>('undelivered');
  let query = $state('');

  let shipments = $derived(shipmentsPage?.data ?? []);

  $effect(() => {
    let cancelled = false;
    loading = true;
    (async () => {
      const paged = await fetchPaged<Shipment>('/api/shipping/shipments?limit=1000');
      if (!cancelled) {
        shipmentsPage = paged;
        loading = false;
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  let inboundCount = $derived(shipments.filter((s) => s.direction === 'inbound').length);
  let outboundCount = $derived(shipments.filter((s) => s.direction === 'outbound').length);
  let inTransit = $derived(shipments.filter((s) => s.status === 'in-transit').length);
  let exceptions = $derived(shipments.filter((s) => s.status === 'exception').length);

  let visible = $derived(
    shipments.filter((s) => {
      if (tab !== 'all' && s.direction !== tab) return false;
      if (statusFilter === 'undelivered') {
        if (s.status === 'delivered') return false;
      } else if (statusFilter !== 'all' && s.status !== statusFilter) {
        return false;
      }
      if (query) {
        const q = query.toLowerCase();
        const hay = `${s.id} ${s.origin} ${s.destination} ${s.tracking_number ?? ''} ${s.carrier ?? ''}`.toLowerCase();
        if (!hay.includes(q)) return false;
      }
      return true;
    }),
  );

  function directionClass(d: ShipmentDirection): string {
    return d === 'inbound' ? 'ok' : 'muted';
  }
  function statusClass(s: ShipmentStatus): string {
    if (s === 'delivered') return 'ok';
    if (s === 'exception') return 'warn';
    if (s === 'in-transit') return 'ok';
    return 'muted';
  }

  function countForStatus(st: ShipmentStatus): number {
    return shipments.filter(
      (s) => s.status === st && (tab === 'all' || s.direction === tab),
    ).length;
  }
</script>

<div class="catalog theme-exec">
  <PageHeader
    eyebrow="Shipping"
    title={`${(shipmentsPage?.total ?? shipments.length).toLocaleString()} shipments`}
    subtitle={isCapped(shipmentsPage)
      ? `Loaded window of ${shipments.length.toLocaleString()} — per-status counts below cover this window only`
      : `${inboundCount} inbound · ${outboundCount} outbound · ${inTransit} in transit · ${exceptions} exception${exceptions !== 1 ? 's' : ''}`}
  />

  <nav class="tabs" role="tablist">
    {#each TAB_LIST as t (t.id)}
      <button
        type="button"
        role="tab"
        aria-selected={tab === t.id}
        class="tab {tab === t.id ? 'tab-active' : ''}"
        onclick={() => (tab = t.id)}
      >
        {t.label}
      </button>
    {/each}
  </nav>

  {#if isCapped(shipmentsPage)}
    <OverflowBanner
      showing={shipments.length}
      total={shipmentsPage!.total}
      noun="shipments loaded"
      hint="Tabs and per-status counts below only consider this window. Use the search or status filter to drill into the rest."
    />
  {/if}

  <div class="catalog-layout" style="margin-top:16px">
    <aside class="catalog-filters">
      <FilterGroup label="Search">
          <SearchInput bind:value={query} placeholder="ID, tracking, origin…" />
      </FilterGroup>
      <FilterGroup label="Status">
          {@const undelivered = shipments.filter((s) =>
            (tab === 'all' || s.direction === tab) && s.status !== 'delivered'
          ).length}
          <FilterButton active={statusFilter === 'undelivered'} onclick={() => (statusFilter = 'undelivered')}>
              Undelivered ({undelivered})
          </FilterButton>
          <FilterButton active={statusFilter === 'all'} onclick={() => (statusFilter = 'all')}>
              All ({shipments.filter((s) => tab === 'all' || s.direction === tab).length})
          </FilterButton>
          {#each STATUSES as st (st)}
            {@const count = countForStatus(st)}
            {#if count > 0}
              <FilterButton active={statusFilter === st} onclick={() => (statusFilter = st)}>
                {STATUS_LABEL[st]} ({count})
              </FilterButton>
            {/if}
          {/each}
      </FilterGroup>
    </aside>

    <section class="list-section">
      {#if loading}
        <p class="empty">Loading…</p>
      {:else if visible.length === 0}
        <p class="empty">No shipments match those filters.</p>
      {:else}
        <table class="data-table data-table-striped">
          <thead>
            <tr>
              <th>Shipment</th>
              <th>Direction</th>
              <th>Status</th>
              <th>Carrier</th>
              <th>Tracking</th>
              <th>Origin</th>
              <th>Destination</th>
              <th>Items</th>
              <th>Created</th>
              <th>ETA</th>
            </tr>
          </thead>
          <tbody>
            {#each visible as s (s.id)}
              <tr id={`shipment-${s.id}`}>
                <td class="mono"><EntityLink kind="shipment" id={s.id} /></td>
                <td>
                  <span class="chip chip-stage chip-stage-{directionClass(s.direction)}">
                    {DIRECTION_LABEL[s.direction]}
                  </span>
                </td>
                <td>
                  <span class="chip chip-stage chip-stage-{statusClass(s.status)}">
                    {STATUS_LABEL[s.status]}
                  </span>
                </td>
                <td>{s.carrier ? CARRIER_LABEL[s.carrier] : '—'}</td>
                <td class="mono">{s.tracking_number ?? '—'}</td>
                <td>
                  {#if s.account_id && s.direction === 'inbound'}
                    <Link to={entityHref('account', s.account_id)}>
                      {s.origin}
                    </Link>
                  {:else}
                    {s.origin}
                  {/if}
                </td>
                <td>
                  {#if s.account_id && s.direction === 'outbound'}
                    <Link to={entityHref('account', s.account_id)}>
                      {s.destination}
                    </Link>
                  {:else}
                    {s.destination}
                  {/if}
                </td>
                <td class="num">{s.asset_ids.length || '—'}</td>
                <td>{s.created_on}</td>
                <td>{s.estimated_delivery ?? '—'}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    </section>
  </div>
</div>
