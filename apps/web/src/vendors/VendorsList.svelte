<script lang="ts">
  // Vendors list — port of apps/web/src/vendors/VendorsList.tsx.
  //
  // Aggregates POs + vendor invoices by vendor name and shows open PO
  // counts + outstanding balances. Sits under the Know bucket.

  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import FilterGroup from '@boss/web-kit/ui/FilterGroup.svelte';
  import FilterButton from '@boss/web-kit/ui/FilterButton.svelte';
  import SearchInput from '@boss/web-kit/ui/SearchInput.svelte';
  import EntityLink from '@boss/web-kit/ui/EntityLink.svelte';
  import { formatMoney } from '@boss/web-kit/ui/money';
  import type { PurchaseOrder, Vendor, VendorInvoice } from './types';
  import { failedRead, okRead, readStateOfResponse, type ReadState } from '../data/readState';

  let vendors = $state<Vendor[]>([]);
  let pos = $state<PurchaseOrder[]>([]);
  let bills = $state<VendorInvoice[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  // The two reads the per-vendor counts are built from. Until backlog
  // 223ebcd6 a refused one was parsed as `[]`, so an inventory outage
  // painted every vendor as owing nothing with no line saying why; the
  // outcome now survives beside the rows and the page says it.
  let ordersRead = $state<ReadState>(okRead);
  let billsRead = $state<ReadState>(okRead);

  let category = $state<string>('all');
  let stateFilter = $state<string>('all');
  let query = $state('');

  /// One side read, fetched AND parsed in its own try (backlog aaeb02d6).
  /// Both used to share the vendors read's catch, so a network rejection
  /// or an unparseable body on orders or invoices threw away a vendor
  /// list that had answered and reported "Couldn't load vendors" — the
  /// wrong read. Settling here, a side failure is its own line and the
  /// vendor rows stand, the same as a refused one already did.
  async function sideRead<T>(url: string): Promise<{ read: ReadState; rows: T[] }> {
    try {
      const resp = await fetch(url);
      const read = readStateOfResponse(url, resp);
      if (read.kind !== 'ok') return { read, rows: [] };
      const body = await resp.json();
      return { read, rows: Array.isArray(body) ? body : (body.data ?? []) };
    } catch (e) {
      return {
        read: failedRead(`${url}: ${e instanceof Error ? e.message : String(e)}`),
        rows: [],
      };
    }
  }

  $effect(() => {
    let cancelled = false;
    loading = true;
    (async () => {
      try {
        const [vResp, orders, invoices] = await Promise.all([
          fetch('/api/inventory/vendors'),
          sideRead<PurchaseOrder>('/api/inventory/orders'),
          sideRead<VendorInvoice>('/api/inventory/vendor-invoices'),
        ]);
        if (!vResp.ok) throw new Error(`vendors HTTP ${vResp.status}`);
        const vBody = await vResp.json();
        if (!cancelled) {
          vendors = Array.isArray(vBody) ? vBody : (vBody.data ?? []);
          pos = orders.rows;
          bills = invoices.rows;
          ordersRead = orders.read;
          billsRead = invoices.read;
          loading = false;
        }
      } catch (e) {
        if (!cancelled) {
          error = e instanceof Error ? e.message : String(e);
          loading = false;
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  let categories = $derived(
    [...new Set(vendors.map((v) => v.category).filter((c): c is string => c !== null))].sort(),
  );
  let states = $derived(
    [...new Set(vendors.map((v) => v.state).filter((s): s is string => s !== null))].sort(),
  );

  let rows = $derived(
    vendors.map((v) => {
      const vendorPos = pos.filter((po) => po.vendor === v.name);
      const openPos = vendorPos.filter(
        (po) => po.status !== 'received' && po.status !== 'closed',
      );
      const vendorBills = bills.filter((b) => b.vendor === v.name);
      const unpaidBills = vendorBills.filter((b) => b.status !== 'paid');
      const outstandingCents = unpaidBills.reduce((s, b) => s + b.amount_cents, 0);
      return {
        vendor: v,
        totalPos: vendorPos.length,
        openPos: openPos.length,
        unpaidBills: unpaidBills.length,
        outstandingCents,
      };
    }),
  );

  let visible = $derived(
    rows.filter((r) => {
      if (category !== 'all' && r.vendor.category !== category) return false;
      if (stateFilter !== 'all' && r.vendor.state !== stateFilter) return false;
      if (query) {
        const q = query.toLowerCase();
        const hay = `${r.vendor.id} ${r.vendor.name} ${r.vendor.contact_name}`.toLowerCase();
        if (!hay.includes(q)) return false;
      }
      return true;
    }),
  );

  let totalOpenPos = $derived(rows.reduce((s, r) => s + r.openPos, 0));
  let totalOutstandingCents = $derived(
    rows.reduce((s, r) => s + r.outstandingCents, 0),
  );

  let ordersUnknown = $derived(ordersRead.kind === 'failed');
  let billsUnknown = $derived(billsRead.kind === 'failed');

  // Each failed count read, with the columns it leaves unknown — the
  // columns stay, and read `?` rather than a zero nobody measured.
  let failedCounts = $derived(
    [
      { what: 'purchase orders', read: ordersRead, columns: 'Open POs' },
      { what: 'vendor invoices', read: billsRead, columns: 'Unpaid bills and Outstanding' },
    ].flatMap((f) =>
      f.read.kind === 'failed' ? [{ ...f, error: f.read.error }] : [],
    ),
  );

  // Under a failed list read every figure in the header is unknown, and
  // "0 vendors · 0 open POs · $0.00 outstanding" above the failure line
  // read as an answer (sweep c3e4edcc, vendors gap 2).
  let subtitle = $derived(
    error
      ? 'Vendor count unknown — the read failed'
      : [
          ordersUnknown
            ? 'open POs unknown'
            : `${totalOpenPos} open PO${totalOpenPos === 1 ? '' : 's'}`,
          billsUnknown
            ? 'outstanding unknown'
            : `${formatMoney({ amount_cents: totalOutstandingCents, currency: 'USD' })} outstanding across all vendors`,
        ].join(' · '),
  );
</script>

<div class="catalog theme-exec">
  <PageHeader
    eyebrow="Know"
    title={error ? 'Vendors' : `${vendors.length} vendors`}
    {subtitle}
  />

  <div class="catalog-layout">
    <aside class="catalog-filters">
      <FilterGroup label="Search">
          <SearchInput bind:value={query} placeholder="Vendor, contact…" label="Search vendors" />
      </FilterGroup>
      <FilterGroup label="Category">
          <FilterButton active={category === 'all'} onclick={() => (category = 'all')}>
            All ({vendors.length})
          </FilterButton>
          {#each categories as c (c)}
            <FilterButton active={category === c} onclick={() => (category = c)}>
                {c.replace(/-/g, ' ')} ({vendors.filter((v) => v.category === c).length})
            </FilterButton>
          {/each}
      </FilterGroup>
      <FilterGroup label="State">
          <FilterButton active={stateFilter === 'all'} onclick={() => (stateFilter = 'all')}>
            All
          </FilterButton>
          {#each states as s (s)}
            <FilterButton active={stateFilter === s} onclick={() => (stateFilter = s)}>
              {s}
            </FilterButton>
          {/each}
      </FilterGroup>
    </aside>

    <section class="list-section">
      {#if !loading && !error}
        {#each failedCounts as f (f.what)}
          <p class="empty load-failed" role="alert">
            Couldn't load {f.what} — {f.error}. {f.columns} below are unknown, not zero.
          </p>
        {/each}
      {/if}
      {#if loading}
        <p class="empty">Loading…</p>
      {:else if error}
        <p class="empty load-failed" role="alert">Couldn't load vendors: {error}</p>
      {:else if visible.length === 0}
        <p class="empty">No vendors match those filters.</p>
      {:else}
        <table class="data-table data-table-striped">
          <thead>
            <tr>
              <th>Vendor</th>
              <th>Category</th>
              <th>Location</th>
              <th>Terms</th>
              <th class="num">Lead time</th>
              <th class="num">Open POs</th>
              <th class="num">Unpaid bills</th>
              <th class="num">Outstanding</th>
            </tr>
          </thead>
          <tbody>
            {#each visible as r (r.vendor.id)}
              <tr>
                <td>
                  <EntityLink kind="vendor" id={r.vendor.id} label={r.vendor.name} />
                </td>
                <td>{r.vendor.category?.replace(/-/g, ' ') ?? '—'}</td>
                <td>{r.vendor.city ?? '—'}, {r.vendor.state ?? '—'}</td>
                <td>{r.vendor.payment_terms}</td>
                <td class="num">{r.vendor.lead_time_days}d</td>
                <td class="num">
                  {#if ordersUnknown}
                    <span title="purchase orders did not load">?</span>
                  {:else if r.openPos > 0}
                    {r.openPos}<span style="color:var(--static); margin-left:4px">/ {r.totalPos}</span>
                  {:else}
                    <span style="color:var(--static)">0</span>
                  {/if}
                </td>
                <td class="num">
                  {#if billsUnknown}
                    <span title="vendor invoices did not load">?</span>
                  {:else if r.unpaidBills > 0}
                    {r.unpaidBills}
                  {:else}
                    <span style="color:var(--static)">0</span>
                  {/if}
                </td>
                <td class="num">
                  {#if billsUnknown}
                    <span title="vendor invoices did not load">?</span>
                  {:else if r.outstandingCents > 0}
                    {formatMoney({ amount_cents: r.outstandingCents, currency: 'USD' })}
                  {:else}
                    <span style="color:var(--static)">—</span>
                  {/if}
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    </section>
  </div>
</div>
