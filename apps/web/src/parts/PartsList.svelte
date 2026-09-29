<script lang="ts">
  // Parts list — port of apps/web/src/parts/PartsList.tsx.

  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import { entityHref } from '@boss/web-kit/ui/entity-href';
  import FilterGroup from '@boss/web-kit/ui/FilterGroup.svelte';
  import FilterButton from '@boss/web-kit/ui/FilterButton.svelte';
  import SearchInput from '@boss/web-kit/ui/SearchInput.svelte';
  import Link from '@boss/web-kit/ui/Link.svelte';
  import StatusChip from '@boss/web-kit/ui/StatusChip.svelte';
  import {
    collectParts,
    kindFromSku,
    stockStatus,
    stockTone,
    type CatalogPart,
    type DeviceModel,
    type InventoryItem,
    type PurchaseOrder,
    type StockStatus,
  } from './types';
  import { partsHeader, unstockedSkus } from './stock-counts';
  import { countLabel, emptyState, readStateOfLoad } from '../data/readState';
  import { readRows } from '../data/shape';
  import ListEmpty from '../data/ListEmpty.svelte';
  import { rowLink } from '@boss/web-kit/ui/RowLink';
  import { href, navigate } from '../router';
  import { getLabel } from '@boss/web-kit/session/manifest.svelte';
  import { departmentLabel } from '@boss/web-kit/nav';
  import { departments } from '@boss/web-kit/session/departments.svelte';
  import DepartmentThirds from '../departments/DepartmentThirds.svelte';

  // The department whose packets this page lists beside its parts —
  // the catalog entry's `department`, handed in by App.svelte the way
  // the two queues get theirs. The four reads below are stock; none is
  // a jobs read, so until this prop a warehouse packet could never
  // appear on the warehouse's page (backlog 044dffa1, page audit
  // 63d810aa). Empty draws no panel.
  let { department = '' } = $props<{ department?: string }>();
  const departmentName = $derived(departmentLabel(department, departments()));

  type RowKind = 'ingredient' | 'packaging' | 'spare' | 'consumable';
  // A catalogued part with no inventory row has no stock to judge, so
  // it is not out, low or healthy: it is never stocked (backlog
  // 4cb8c06a). It needs no attention by the reorder arithmetic — there
  // is no reorder point — so it has its own button and sorts last.
  type RowStatus = StockStatus | 'never-stocked';
  type Filter = 'all' | 'needs-attention' | RowKind | RowStatus;

  let models = $state<DeviceModel[]>([]);
  let inventory = $state<InventoryItem[]>([]);
  let pos = $state<PurchaseOrder[]>([]);
  // The brewery seeds parts directly into the `parts` table —
  // no system_models.spare_parts/consumables linkage. Pull the
  // canonical /api/catalog/parts list and fall back to the
  // device-asset shape (collectParts) for tenants that DO use
  // satellite linkage.
  let catalogParts = $state<CatalogPart[]>([]);

  const MODELS_URL = '/api/catalog/models';
  const ITEMS_URL = '/api/inventory/items';
  const CATALOG_PARTS_URL = '/api/catalog/parts';
  const ORDERS_URL = '/api/inventory/orders';

  /// A row-source failure names its read: three reads stand behind the
  /// list, and "Failed to fetch" alone cannot say which (0ef5e008).
  function namedAs(url: string): (e: unknown) => never {
    return (e) => {
      throw new Error(`${url}: ${e instanceof Error ? e.message : String(e)}`);
    };
  }
  const fetchNamed = (url: string): Promise<Response> => fetch(url).catch(namedAs(url));
  /// Non-null when a row-source load failed — rendered instead of the
  /// empty state, so an outage never reads as "no parts" (packet
  /// 3fba9c35, the false-empty sweep).
  let loadFailed = $state<string | null>(null);
  let loading = $state(true);
  // Defaults to "all" so the playground shows the SKUs on first
  // load. Operators rebrowsing for stockouts click "Needs
  // attention" themselves; first-impression empty-state was
  // confusing.
  let filter = $state<Filter>('all');
  let query = $state('');

  $effect(() => {
    let cancelled = false;
    loading = true;
    (async () => {
      try {
        const [mResp, iResp, pResp, cpResp] = await Promise.all([
          fetchNamed(MODELS_URL),
          fetchNamed(ITEMS_URL),
          fetch(ORDERS_URL),
          fetchNamed(CATALOG_PARTS_URL),
        ]);
        // The row sources (models, inventory, catalog parts) are the
        // page's primary data — any of them failing fails the list, and
        // the line names WHICH (backlog 0ef5e008: it said "HTTP 503" with
        // three reads behind it). A 200 that is not a list shape fails it
        // too: each was coerced to no rows, which read as no parts.
        // The PO list only feeds "on order" counts and degrades.
        const primary = [
          [MODELS_URL, mResp],
          [ITEMS_URL, iResp],
          [CATALOG_PARTS_URL, cpResp],
        ] as const;
        const down = primary.find(([, r]) => !r.ok);
        if (down) throw new Error(`${down[0]}: HTTP ${down[1].status}`);
        const [mRows, iRows, cpRows] = await Promise.all(
          primary.map(async ([url, r]) => readRows(url, await r.json().catch(namedAs(url)))),
        );
        const pBody = pResp.ok ? await pResp.json() : [];
        // A refused PO read degrades to no "on order" counts (gap 4,
        // 61c16b17); a 200 that is not a list shape is a contract break,
        // not an outage, and fails the page by name like the three above
        // (backlog b6b74115). `[]` from the refused arm reads as itself.
        const pRows = readRows(ORDERS_URL, pBody);
        if (!cancelled) {
          models = mRows as DeviceModel[];
          inventory = iRows as InventoryItem[];
          pos = pRows as PurchaseOrder[];
          catalogParts = cpRows as CatalogPart[];
          loadFailed = null;
          loading = false;
        }
      } catch (e) {
        if (!cancelled) {
          loadFailed = e instanceof Error ? e.message : String(e);
          loading = false;
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  let catalogSkuSet = $derived(new Set(models.map((m) => m.sku)));
  let parts = $derived(collectParts(models));
  let catalogPartBySku = $derived(
    new Map(catalogParts.map((p) => [p.part_sku, p])),
  );

  let onOrder = $derived.by(() => {
    const m = new Map<string, number>();
    for (const po of pos) {
      if (po.status === 'received' || po.status === 'closed') continue;
      for (const line of po.lines) {
        m.set(line.part_sku, (m.get(line.part_sku) ?? 0) + line.qty);
      }
    }
    return m;
  });

  type Row = {
    sku: string;
    /// Null for a catalogued part that was never stocked (4cb8c06a).
    item: InventoryItem | null;
    name: string;
    description: string;
    kind: RowKind;
    used_by: number;
    status: RowStatus;
    on_order: number;
  };

  // Every stocked part, then every catalogued part with no inventory
  // row. Until backlog 4cb8c06a the rows were `inventory.map(...)`, so
  // a part that exists and was never stocked never appeared while the
  // title counted the rest as the page's parts.
  let rows = $derived<Row[]>(
    [
      ...inventory.map((item) => ({ sku: item.part_sku, item })),
      ...unstockedSkus(
        inventory,
        catalogParts.map((p) => p.part_sku),
        parts.map((p) => p.sku),
      ).map((sku) => ({ sku, item: null })),
    ].map(({ sku, item }) => {
      // Prefer the device-catalog satellite linkage when it
      // exists (used-device-shop shape — gives the "used by N
      // models" count). Fall back to /api/catalog/parts when
      // the part isn't linked to a system_model (brewery
      // shape — ingredients + packaging).
      const meta = parts.find((p) => p.sku === sku);
      const flat = catalogPartBySku.get(sku);
      return {
        sku,
        item,
        name: meta?.part.name ?? flat?.name ?? sku,
        description: meta?.part.description ?? flat?.description ?? '',
        kind: meta?.kind ?? kindFromSku(sku),
        used_by: meta?.used_by.length ?? 0,
        status: item ? stockStatus(item) : 'never-stocked',
        on_order: onOrder.get(sku) ?? 0,
      };
    }),
  );

  let counts = $derived({
    total: rows.length,
    out: rows.filter((r) => r.status === 'out').length,
    critical: rows.filter((r) => r.status === 'critical').length,
    low: rows.filter((r) => r.status === 'low').length,
    healthy: rows.filter((r) => r.status === 'healthy').length,
    never: rows.filter((r) => r.status === 'never-stocked').length,
    spare: rows.filter((r) => r.kind === 'spare').length,
    consumable: rows.filter((r) => r.kind === 'consumable').length,
    ingredient: rows.filter((r) => r.kind === 'ingredient').length,
    packaging: rows.filter((r) => r.kind === 'packaging').length,
  });
  let attention = $derived(counts.out + counts.critical + counts.low);

  // The header and the filter buttons count only a list that was read —
  // backlog f867d71c: they counted the `[]` the inventory starts as, so
  // a loading or failed read printed "0 need attention · 0 out · 0
  // critical" and All (0) above an honest "Couldn't load parts".
  let read = $derived(readStateOfLoad(loading, loadFailed));
  let header = $derived(
    partsHeader(read, getLabel('parts.page_title', 'parts'), {
      total: counts.total,
      attention,
      out: counts.out,
      critical: counts.critical,
    }),
  );

  let visible = $derived(
    rows.filter((r) => {
      if (filter === 'needs-attention' && (r.status === 'healthy' || r.status === 'never-stocked')) {
        return false;
      }
      if (
        (filter === 'spare' ||
          filter === 'consumable' ||
          filter === 'ingredient' ||
          filter === 'packaging') &&
        r.kind !== filter
      ) return false;
      if (
        (filter === 'out' ||
          filter === 'critical' ||
          filter === 'low' ||
          filter === 'healthy' ||
          filter === 'never-stocked') &&
        r.status !== filter
      ) return false;
      if (query) {
        const q = query.toLowerCase();
        if (!`${r.sku} ${r.name} ${r.description}`.toLowerCase().includes(q)) {
          return false;
        }
      }
      return true;
    }),
  );

  // Read failed, no parts, or the filters hid them — an empty inventory
  // is not an over-narrow filter (backlogs 0ef5e008, bc38daa8).
  let listState = $derived(
    emptyState([{ source: 'the parts reads', state: read }], rows.length, visible.length),
  );

  let sortedVisible = $derived.by(() => {
    const rank: Record<RowStatus, number> = {
      out: 0, critical: 1, low: 2, healthy: 3, 'never-stocked': 4,
    };
    return [...visible].sort((a, b) => rank[a.status] - rank[b.status]);
  });
</script>

<div class="catalog theme-exec">
  <PageHeader
    eyebrow="Inventory"
    title={header.title}
    subtitle={header.subtitle}
  />

  <div class="catalog-layout">
    <aside class="catalog-filters">
      <FilterGroup label="Search">
          <SearchInput bind:value={query} placeholder="SKU, name…" label="Search" />
      </FilterGroup>

      <FilterGroup label="Stock status">
          <FilterButton active={filter === 'needs-attention'} onclick={() => (filter = 'needs-attention')}>
            {countLabel('Needs attention', read, attention)}
          </FilterButton>
          <FilterButton active={filter === 'all'} onclick={() => (filter = 'all')}>
            {countLabel('All', read, counts.total)}
          </FilterButton>
          <FilterButton active={filter === 'out'} onclick={() => (filter = 'out')}>
            {countLabel('Out of stock', read, counts.out)}
          </FilterButton>
          <FilterButton active={filter === 'critical'} onclick={() => (filter = 'critical')}>
            {countLabel('Critical', read, counts.critical)}
          </FilterButton>
          <FilterButton active={filter === 'low'} onclick={() => (filter = 'low')}>
            {countLabel('Low', read, counts.low)}
          </FilterButton>
          <FilterButton active={filter === 'healthy'} onclick={() => (filter = 'healthy')}>
            {countLabel('Healthy', read, counts.healthy)}
          </FilterButton>
          {#if counts.never > 0}
            <FilterButton active={filter === 'never-stocked'} onclick={() => (filter = 'never-stocked')}>
              {countLabel('Never stocked', read, counts.never)}
            </FilterButton>
          {/if}
      </FilterGroup>

      <FilterGroup label="Kind">
          {#if counts.ingredient > 0}
            <FilterButton active={filter === 'ingredient'} onclick={() => (filter = 'ingredient')}>
              {countLabel('Ingredients', read, counts.ingredient)}
            </FilterButton>
          {/if}
          {#if counts.packaging > 0}
            <FilterButton active={filter === 'packaging'} onclick={() => (filter = 'packaging')}>
              {countLabel('Packaging', read, counts.packaging)}
            </FilterButton>
          {/if}
          {#if counts.spare > 0}
            <FilterButton active={filter === 'spare'} onclick={() => (filter = 'spare')}>
              {countLabel('Spare parts', read, counts.spare)}
            </FilterButton>
          {/if}
          {#if counts.consumable > 0}
            <FilterButton active={filter === 'consumable'} onclick={() => (filter = 'consumable')}>
              {countLabel('Consumables', read, counts.consumable)}
            </FilterButton>
          {/if}
      </FilterGroup>
    </aside>

    <section class="list-section">
      {#if listState.kind !== 'rows'}
        <ListEmpty view={listState} words={{ what: 'parts', noun: 'parts' }} />
      {:else}
        <table class="data-table data-table-striped">
          <thead>
            <tr>
              <th>Part SKU</th>
              <th>Name</th>
              <th>Kind</th>
              <th class="num">On hand</th>
              <th class="num">Allocated</th>
              <th class="num">Reorder pt</th>
              <th class="num">On order</th>
              <th>Status</th>
              <th class="num">Used by</th>
              <th>Bin</th>
            </tr>
          </thead>
          <tbody>
            {#each sortedVisible as r (r.sku)}
              <tr
                use:rowLink={{
                  onActivate: () => navigate(entityHref('part', r.sku)),
                  label: `${r.name} (${r.sku})`,
                }}
              >
                <td class="mono">
                  <Link to={entityHref('part', r.sku)}>
                    {r.sku}
                  </Link>
                </td>
                <td>{r.name}</td>
                <td>{r.kind}</td>
                <!-- A never-stocked part has no inventory row: "—", not a
                     0 the record does not hold (4cb8c06a). -->
                <td class="num">{r.item ? r.item.on_hand : '—'}</td>
                <td class="num">{r.item ? r.item.allocated : '—'}</td>
                <td class="num">{r.item ? r.item.reorder_point : '—'}</td>
                <td class="num">{r.on_order > 0 ? r.on_order : '—'}</td>
                <td>
                  <StatusChip
                    value={r.status}
                    tone={r.status === 'never-stocked' ? 'muted' : stockTone(r.status)}
                  />
                </td>
                <td class="num">
                  {catalogSkuSet.has(r.sku) ? '—' : r.used_by}
                </td>
                <td class="mono">{r.item ? r.item.bin : '—'}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    </section>
  </div>

  {#if department}
    <!-- Apart from the parts table (its own class, not list-section),
         so nothing that reads the stock list reads a packet. -->
    <section class="department-jobs">
      <h2>{departmentName} jobs</h2>
      <DepartmentThirds code={department} />
    </section>
  {/if}
</div>
