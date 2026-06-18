<script lang="ts">
  // Invoices tab — filterable list. Port of InvoicesTab sub-component
  // from apps/web/src/finance/FinancePage.tsx.

  import FilterGroup from '../ui/FilterGroup.svelte';
  import FilterButton from '../ui/FilterButton.svelte';
  import SearchInput from '../ui/SearchInput.svelte';
  import EntityLink from '../ui/EntityLink.svelte';
  import OverflowBanner from '../ui/OverflowBanner.svelte';
  import InvoiceStatusChip from './InvoiceStatusChip.svelte';
  import {
    PAYMENT_METHOD_LABEL,
    type Invoice,
    type InvoiceStatus,
    type PaymentMethod,
  } from './types';
  import type { Account } from '../accounts/types';
  import { formatMoney } from '../ui/money';

  type Props = {
    invoices: ReadonlyArray<Invoice>;
    totalCount: number;
  };
  let { invoices, totalCount }: Props = $props();

  type StatusFilter = InvoiceStatus | 'all' | 'unpaid';
  type MethodFilter = PaymentMethod | 'all';

  const METHOD_FILTERS: ReadonlyArray<MethodFilter> = ['all', 'ach', 'wire', 'check', 'card'];

  let statusFilter = $state<StatusFilter>('all');
  let methodFilter = $state<MethodFilter>('all');
  let query = $state('');
  let accounts = $state<Account[]>([]);

  $effect(() => {
    let cancelled = false;
    (async () => {
      try {
        const r = await fetch('/api/people/accounts');
        if (!r.ok) return;
        const body = await r.json();
        if (!cancelled) {
          accounts = Array.isArray(body) ? body : (body.data ?? []);
        }
      } catch {
        // Ignore — invoices still render without friendly account names.
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  let accountById = $derived.by(() => {
    const m = new Map<string, Account>();
    for (const p of accounts) m.set(p.id, p);
    return m;
  });

  let truncated = $derived(totalCount > invoices.length);
  let unpaid = $derived(invoices.filter((i) => i.status !== 'paid'));
  let pastDue = $derived(invoices.filter((i) => i.status === 'past-due'));

  let methodCounts = $derived.by(() => {
    const counts: Record<PaymentMethod | 'all', number> = {
      all: invoices.length,
      ach: 0,
      wire: 0,
      check: 0,
      card: 0,
    };
    for (const i of invoices) {
      if (i.payment_method) counts[i.payment_method] += 1;
    }
    return counts;
  });

  let visible = $derived(
    invoices.filter((i) => {
      if (statusFilter === 'unpaid' && i.status === 'paid') return false;
      if (
        statusFilter !== 'all' &&
        statusFilter !== 'unpaid' &&
        i.status !== statusFilter
      )
        return false;
      if (methodFilter !== 'all' && i.payment_method !== methodFilter) return false;
      if (query) {
        const q = query.toLowerCase();
        const account = accountById.get(i.account_id);
        const lineText = i.line_items
          .map((l) => `${l.revenue_category} ${l.description}`)
          .join(' ');
        if (!`${i.id} ${account?.name ?? ''} ${lineText}`.toLowerCase().includes(q)) {
          return false;
        }
      }
      return true;
    }),
  );
</script>

<div class="catalog-layout">
  <aside class="catalog-filters">
    <FilterGroup label="Search">
        <SearchInput bind:value={query} placeholder="Invoice, account…" />
    </FilterGroup>
    <FilterGroup label="Status">
        <FilterButton active={statusFilter === 'all'} onclick={() => (statusFilter = 'all')}>
          All ({invoices.length})
        </FilterButton>
        <FilterButton active={statusFilter === 'unpaid'} onclick={() => (statusFilter = 'unpaid')}>
          Unpaid ({unpaid.length})
        </FilterButton>
        <FilterButton active={statusFilter === 'past-due'} onclick={() => (statusFilter = 'past-due')}>
          Past due ({pastDue.length})
        </FilterButton>
        <FilterButton active={statusFilter === 'paid'} onclick={() => (statusFilter = 'paid')}>
          Paid ({invoices.length - unpaid.length})
        </FilterButton>
    </FilterGroup>
    <FilterGroup label="Method">
        {#each METHOD_FILTERS as m (m)}
          <FilterButton active={methodFilter === m} onclick={() => (methodFilter = m)}>
              {m === 'all' ? 'All' : PAYMENT_METHOD_LABEL[m]} ({methodCounts[m]})
          </FilterButton>
        {/each}
    </FilterGroup>
  </aside>

  <section class="list-section">
    {#if truncated}
      <OverflowBanner
        showing={invoices.length}
        total={totalCount}
        noun="invoices"
        hint="Use search or status filters to narrow the list."
      />
    {/if}
    {#if visible.length === 0}
      <p class="empty">No invoices match those filters.</p>
    {:else}
      <table class="data-table data-table-striped">
        <thead>
          <tr>
            <th>Invoice</th>
            <th>Status</th>
            <th>Account</th>
            <th class="num">Lines</th>
            <th class="num">Amount</th>
            <th class="num">Tax</th>
            <th>Method</th>
            <th>Issued</th>
            <th>Due</th>
            <th>Paid</th>
          </tr>
        </thead>
        <tbody>
          {#each visible as i (i.id)}
            {@const account = accountById.get(i.account_id)}
            {@const taxCents = i.tax_cents ?? 0}
            <tr>
              <td class="mono"><EntityLink kind="invoice" id={i.id} /></td>
              <td><InvoiceStatusChip status={i.status} /></td>
              <td>
                <EntityLink
                  kind="account"
                  id={i.account_id}
                  label={account?.name}
                  mono={!account}
                />
              </td>
              <td class="num">{i.line_items.length}</td>
              <td class="num">{formatMoney({ amount_cents: i.amount_cents, currency: i.currency })}</td>
              <td class="num" style={taxCents > 0 ? '' : 'color:#a8a29e'}>
                {#if taxCents > 0}
                  {formatMoney({ amount_cents: taxCents, currency: i.currency })}
                  {#if i.tax_jurisdiction}
                    <span class="mono" style="margin-left:4px; font-size:10px; color:#78716c">
                      {i.tax_jurisdiction.replace(/^US-/, '')}
                    </span>
                  {/if}
                {:else}
                  —
                {/if}
              </td>
              <td>
                {#if i.payment_method}
                  {PAYMENT_METHOD_LABEL[i.payment_method]}
                {:else}
                  <span style="color:#a8a29e">—</span>
                {/if}
              </td>
              <td>{i.issued_on}</td>
              <td>{i.due_on}</td>
              <td>{i.paid_on ?? '—'}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
  </section>
</div>
