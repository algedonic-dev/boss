<script lang="ts">
  // Finished-product catalog list. Sibling to PartsList (input parts)
  // but reads from /api/products instead of /api/inventory/items —
  // products are countable on-hand-by-location output the tenant
  // produces, with `total_on_hand` rolled up across locations.

  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import SearchInput from '@boss/web-kit/ui/SearchInput.svelte';
  import Link from '@boss/web-kit/ui/Link.svelte';
  import { href } from '../router';
  import { entityHref } from '@boss/web-kit/ui/entity-href';
  import type { Product, ProductDetail } from './types';

  let products = $state<Product[]>([]);
  let totals = $state<Record<string, number>>({});
  let loading = $state(true);
  let query = $state('');

  $effect(() => {
    let cancelled = false;
    loading = true;
    (async () => {
      try {
        const resp = await fetch('/api/products');
        const body = resp.ok ? ((await resp.json()) as Product[]) : [];
        if (cancelled) return;
        products = body;
        // Roll up total_on_hand per SKU via the detail endpoint —
        // the list endpoint omits inventory to keep payloads small.
        const detailEntries = await Promise.all(
          body.map(async (p) => {
            try {
              const r = await fetch(`/api/products/${encodeURIComponent(p.sku)}`);
              if (!r.ok) return [p.sku, 0] as const;
              const d = (await r.json()) as ProductDetail;
              return [p.sku, d.total_on_hand] as const;
            } catch {
              return [p.sku, 0] as const;
            }
          }),
        );
        if (cancelled) return;
        totals = Object.fromEntries(detailEntries);
        loading = false;
      } catch {
        if (!cancelled) loading = false;
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  function fmtMoney(cents: number | null | undefined): string {
    if (cents == null) return '';
    return `$${(cents / 100).toFixed(2)}`;
  }

  function metaStr(p: Product, key: string): string {
    const v = p.metadata?.[key];
    if (typeof v === 'string') return v;
    if (typeof v === 'number') return v.toString();
    return '';
  }

  let filteredRows = $derived.by(() => {
    const q = query.trim().toLowerCase();
    const rows = products.filter((p) => {
      if (!q) return true;
      return (
        p.sku.toLowerCase().includes(q) ||
        p.name.toLowerCase().includes(q) ||
        p.product_kind.toLowerCase().includes(q)
      );
    });
    rows.sort((a, b) => a.sku.localeCompare(b.sku));
    return rows;
  });
</script>

<div class="page">
  <PageHeader title="Products" subtitle="Finished-product catalog with on-hand inventory across all locations." />

  <div class="toolbar">
    <SearchInput bind:value={query} placeholder="Search SKU, name, or kind…" />
  </div>

  {#if loading}
    <p class="empty">Loading products…</p>
  {:else if filteredRows.length === 0}
    <p class="empty">
      {#if query}
        No products match <strong>{query}</strong>.
      {:else}
        No products yet. The brewery's finished-product catalog is seeded via
        <code>examples/brewery/seeds/products.toml</code>.
      {/if}
    </p>
  {:else}
    <table class="prod-table">
      <thead>
        <tr>
          <th>SKU</th>
          <th>Name</th>
          <th>Kind</th>
          <th>Package</th>
          <th class="num">Total on hand</th>
          <th class="num">MSRP</th>
          <th>Style</th>
        </tr>
      </thead>
      <tbody>
        {#each filteredRows as p (p.sku)}
          <tr class:retired={!p.active}>
            <td><Link to={entityHref('product', p.sku)} className="sku">{p.sku}</Link></td>
            <td>{p.name}</td>
            <td>{p.product_kind}</td>
            <td>{p.package_unit}</td>
            <td class="num">{totals[p.sku] ?? 0}</td>
            <td class="num">{fmtMoney(p.metadata?.msrp_cents as number | undefined)}</td>
            <td class="muted">{metaStr(p, 'style')}</td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}
</div>

<style>
  .page {
    padding: 24px;
    max-width: 1200px;
  }
  .toolbar {
    margin-bottom: 16px;
  }
  .empty {
    color: #78716c;
    font-style: italic;
  }
  .prod-table {
    width: 100%;
    border-collapse: collapse;
    background: #fafaf9;
    border: 1px solid #e7e5e4;
    border-radius: 6px;
    overflow: hidden;
  }
  .prod-table th,
  .prod-table td {
    padding: 8px 12px;
    text-align: left;
    border-bottom: 1px solid #e7e5e4;
    font-size: 13px;
  }
  .prod-table th {
    background: #f5f5f4;
    font-weight: 600;
    color: #44403c;
    font-size: 11px;
    letter-spacing: 0.4px;
    text-transform: uppercase;
  }
  .prod-table tr:last-child td {
    border-bottom: none;
  }
  .num {
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  .muted {
    color: #78716c;
  }
  .retired {
    opacity: 0.5;
  }
  :global(.prod-table .sku) {
    font-family: ui-monospace, monospace;
    font-size: 12px;
  }
</style>
