<script lang="ts">
  // Purchase order detail — port of apps/web/src/po/PoPage.tsx.

  import Breadcrumb from '@boss/web-kit/ui/Breadcrumb.svelte';
  import EntityLink from '@boss/web-kit/ui/EntityLink.svelte';
  import Meta from '@boss/web-kit/ui/Meta.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import type { PurchaseOrder } from '../parts/types';
  import type { VendorInvoice } from '../vendors/types';
  import { href } from '../router';

  type Props = { poId: string };
  let { poId }: Props = $props();

  let pos = $state<PurchaseOrder[]>([]);
  let vendorInvoices = $state<VendorInvoice[]>([]);
  let loading = $state(true);

  $effect(() => {
    let cancelled = false;
    loading = true;
    (async () => {
      try {
        const [pResp, vResp] = await Promise.all([
          fetch('/api/inventory/orders'),
          fetch('/api/inventory/vendor-invoices'),
        ]);
        if (pResp.ok) {
          const body = await pResp.json();
          if (!cancelled) pos = Array.isArray(body) ? body : (body.data ?? []);
        }
        if (vResp.ok) {
          const body = await vResp.json();
          if (!cancelled) {
            vendorInvoices = Array.isArray(body) ? body : (body.data ?? []);
          }
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

  let id = $derived(decodeURIComponent(poId));
  let po = $derived<PurchaseOrder | undefined>(pos.find((p) => p.id === id));
</script>

{#if loading && pos.length === 0}
  <div class="catalog theme-exec">
    <p class="empty">Loading PO…</p>
  </div>
{:else if !po}
  <div class="catalog theme-exec">
    <Breadcrumb to={href('/ux/warehouse')}>
      ← Warehouse
    </Breadcrumb>
    <div class="exec-header"><h1 class="exec-title">Purchase order not found</h1></div>
    <p class="empty">No PO record for <code>{id}</code>.</p>
  </div>
{:else}
  {@const lines = po.lines as ReadonlyArray<{
    part_sku: string;
    qty: number;
    unit_cost_cents: number;
    currency: string;
  }>}
  {@const totalCents = lines.reduce((s, l) => s + l.qty * l.unit_cost_cents, 0)}
  {@const currency = lines[0]?.currency ?? 'USD'}
  {@const bills = vendorInvoices.filter((vi) => vi.po_id === po.id)}
  {@const billedCents = bills.reduce((s, vi) => s + vi.amount_cents, 0)}

  <div class="detail-page theme-exec">
    <Breadcrumb to={href('/ux/warehouse')}>
      ← Warehouse
    </Breadcrumb>

    <header class="detail-hero">
      <div>
        <div class="detail-eyebrow">
          <EntityLink kind="po" id={po.id} /> · {po.status.replace(/-/g, ' ')}
        </div>
        <h1 class="detail-title"><EntityLink kind="vendor" id={po.vendor} /></h1>
        <div class="detail-tagline">
          Placed {po.placed_on} · expected {po.expected_on}
          {#if po.received_on} · received {po.received_on}{/if}
        </div>
        <div class="detail-meta">
          <Meta label="Lines">{lines.length}</Meta>
          <Meta label="Total">
            ${(totalCents / 100).toLocaleString()} {currency}
          </Meta>
          <Meta label="Bills">{bills.length}</Meta>
          <Meta label="Billed">
            ${(billedCents / 100).toLocaleString()}
          </Meta>
        </div>
      </div>
    </header>

    <div class="tab-grid">
      <Section title="Summary">
          <dl class="kv">
            <dt>PO</dt><dd><EntityLink kind="po" id={po.id} /></dd>
            <dt>Vendor</dt><dd><EntityLink kind="vendor" id={po.vendor} /></dd>
            <dt>Status</dt><dd>{po.status.replace(/-/g, ' ')}</dd>
            <dt>Placed</dt><dd>{po.placed_on}</dd>
            <dt>Expected</dt><dd>{po.expected_on}</dd>
            <dt>Received</dt><dd>{po.received_on ?? '—'}</dd>
          </dl>
      </Section>

      <Section title={`Lines (${lines.length})`} wide>
          {#if lines.length === 0}
            <p class="empty">No lines on this PO.</p>
          {:else}
            <table class="data-table">
              <thead>
                <tr>
                  <th>Part</th>
                  <th class="num">Qty</th>
                  <th class="num">Unit cost</th>
                  <th class="num">Line total</th>
                </tr>
              </thead>
              <tbody>
                {#each lines as l (l.part_sku)}
                  <tr>
                    <td class="mono"><EntityLink kind="part" id={l.part_sku} /></td>
                    <td class="num">{l.qty}</td>
                    <td class="num">${(l.unit_cost_cents / 100).toLocaleString()}</td>
                    <td class="num">${((l.qty * l.unit_cost_cents) / 100).toLocaleString()}</td>
                  </tr>
                {/each}
              </tbody>
            </table>
          {/if}
      </Section>

      <Section title={`Vendor invoices (${bills.length})`} wide>
          {#if bills.length === 0}
            <p class="empty">No invoices received against this PO.</p>
          {:else}
            <table class="data-table">
              <thead>
                <tr>
                  <th>Invoice #</th>
                  <th>Received</th>
                  <th>Status</th>
                  <th class="num">Amount</th>
                  <th>Discrepancy</th>
                </tr>
              </thead>
              <tbody>
                {#each bills as vi (vi.id)}
                  <tr>
                    <td class="mono"><EntityLink kind="vendor-invoice" id={vi.id} label={vi.vendor_invoice_no} /></td>
                    <td>{vi.received_on}</td>
                    <td>{vi.status}</td>
                    <td class="num">${(vi.amount_cents / 100).toLocaleString()}</td>
                    <td>
                      {#if vi.discrepancy_kind}
                        {vi.discrepancy_kind}{vi.discrepancy_cents ? ` · $${(vi.discrepancy_cents / 100).toLocaleString()}` : ''}
                      {:else}
                        —
                      {/if}
                    </td>
                  </tr>
                {/each}
              </tbody>
            </table>
          {/if}
      </Section>
    </div>
  </div>
{/if}
