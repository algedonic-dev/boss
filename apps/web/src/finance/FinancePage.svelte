<script lang="ts">
  // Finance dashboard — port of apps/web/src/finance/FinancePage.tsx.
  //
  // Eight tabs: Overview (AR/AP aging + margins), Invoices (filterable
  // list), PO Approvals (draft POs), and the five ledger-derived
  // financial statements. New Invoice / New JE actions render only when
  // the session is off the read-only floor AND policy allows each one
  // (./readOnly financeOffersWrite; backlog 9dad102c). The commerce and
  // inventory sections draw only for a tenant whose manifest lists that
  // module (./sections; backlog 801b6a7a, design c2820008 D1).

  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import Link from '@boss/web-kit/ui/Link.svelte';
  import { appToday } from '@boss/web-kit/sim-clock';
  import IncomeStatementTab from './IncomeStatementTab.svelte';
  import BalanceSheetTab from './BalanceSheetTab.svelte';
  import CashFlowTab from './CashFlowTab.svelte';
  import TrialBalanceTab from './TrialBalanceTab.svelte';
  import TaxLiabilityTab from './TaxLiabilityTab.svelte';
  import OverviewTab from './OverviewTab.svelte';
  import InvoicesTab from './InvoicesTab.svelte';
  import ApprovalsTab from './ApprovalsTab.svelte';
  import MonthlyClosePackageButton from './MonthlyClosePackageButton.svelte';
  import type { Invoice } from './types';
  import {
    loadInvoices,
    loadCommerceSummary,
    type CommerceSummary,
  } from './api';
  import { headlineText, loadHeadline, type Headline } from './ledger';
  import { trailingYear } from './window';
  import type { Remote } from '../data/remote';
  import type { PagedResult } from '../data/paginated';
  import { loadingRead, readStateOf, type ReadState } from '../data/readState';
  import { href } from '../router';
  import { session } from '@boss/web-kit/session/session.svelte';
  import { financeOffersWrite } from './readOnly';
  import { permission } from '@boss/web-kit/session/permission.svelte';

  import { financeSearch, type FinanceTab, type FinanceView } from './financeQuery';
  import { manifest } from '@boss/web-kit/session/manifest.svelte';
  import { financeModules, financeTabs, tabShown } from './sections';
  import { departmentLabel } from '@boss/web-kit/nav';
  import { departments } from '@boss/web-kit/session/departments.svelte';
  import DepartmentThirds from '../departments/DepartmentThirds.svelte';

  // `department` is the catalog entry's, handed in by App.svelte the
  // way /ux/parts gets the warehouse's (044dffa1). Every read below is
  // a ledger or commerce read; none is a jobs read, so until this prop
  // a receive-a-payout packet waiting at its post step for 2.6 days was
  // on no finance surface (backlog 4d4dc204, page audit 3f964c57 gap
  // 2). Empty draws no panel.
  let { view, department = '' }: { view: FinanceView; department?: string } = $props();
  const departmentName = $derived(departmentLabel(department, departments()));

  // The tab, and the entry or fact a link opened, live in the URL, not
  // only in page state: a posted journal entry landed on Overview, and
  // back or reload lost the tab (backlog 2ab44d55, page audit 3f964c57
  // gap 7). The route seeds them once — App keys this page on the
  // route, so a navigation remounts it — and after mount the page is
  // the truth and the URL follows. replaceState, not a navigation:
  // /ux/jobs's rule (f8027805), a tab is not a place the back button
  // steps through; back from a page this one linked to returns to the
  // URL written here. The write is the inverse of parseRoute's read, so
  // a mount rewrites nothing.
  // svelte-ignore state_referenced_locally
  let tab = $state<FinanceTab>(view.tab);
  // svelte-ignore state_referenced_locally
  let entry = $state(view.entry);
  // svelte-ignore state_referenced_locally
  let fact = $state(view.fact);

  // The company-layer modules this tenant runs. AR aging, gross margin,
  // Invoices and New invoice read boss-commerce; AP aging and PO
  // Approvals read boss-inventory. Algedonic runs neither, so those
  // sections answered with a source no protocol of the tenant writes
  // (backlog 801b6a7a, f6d54f7b). Design c2820008 D1: absent, not zero,
  // and never read. App renders this page only once the manifest is
  // ready, so `off` here is the tenant's answer, not an unread one.
  let modules = $derived(financeModules(manifest.value));
  let tabs = $derived(financeTabs(modules));
  // The tab drawn: the one asked for, or the first the tenant runs when
  // it asked for an absent one (the default Overview, a ?tab=invoices
  // link). The URL follows what is drawn.
  let shown = $derived(tabShown(tab, modules));

  $effect(() => {
    const { pathname, search, hash } = window.location;
    const next = financeSearch(search, { tab: shown, entry, fact });
    if (next !== search) window.history.replaceState(window.history.state, '', pathname + next + hash);
  });

  /// An entry or a fact is a drill-down of the Trial Balance, so
  /// choosing another tab closes it.
  function chooseTab(t: FinanceTab): void {
    if (t === shown) return;
    tab = t;
    entry = '';
    fact = '';
  }

  /// The invoice list as loaded — failed is its own state, rendered
  /// by InvoicesTab, so an outage never reads as "no invoices"
  /// (packet 3fba9c35).
  let invoicesResult = $state<PagedResult<Invoice> | null>(null);
  let summary = $state<CommerceSummary | null>(null);
  let summaryLoading = $state(true);
  let summaryRead = $state<ReadState>(loadingRead);

  let invoices = $derived(
    invoicesResult?.kind === 'ready' ? invoicesResult.page.data : [],
  );
  // Not answered yet is loading, not an empty book (backlog 0ef5e008).
  let invoicesRead = $derived(invoicesResult ? readStateOf(invoicesResult) : loadingRead);

  // The read-only floor (backlog 432f0eb4) AND policy's answer for the
  // session user on each link's own write (9dad102c) — Create on
  // `invoice` (boss-commerce) and Create on `ledger` (the journal-entry
  // door). Hidden until an Allow arrives; the server's 403 stays the
  // authority.
  const mayInvoice = permission('create', 'invoice');
  const mayPost = permission('create', 'ledger');
  let offerInvoice = $derived(modules.commerce && financeOffersWrite(session, mayInvoice.value));
  let offerEntry = $derived(financeOffersWrite(session, mayPost.value));

  $effect(() => {
    if (!modules.commerce) return;
    let cancelled = false;
    summaryLoading = true;
    const load = async () => {
      const [inv, s] = await Promise.all([loadInvoices(), loadCommerceSummary()]);
      if (!cancelled) {
        invoicesResult = inv;
        summary = s.kind === 'ready' ? s.data : null;
        summaryRead = readStateOf(s);
        summaryLoading = false;
      }
    };
    void load();
    const timer = window.setInterval(() => void load(), 30_000);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  });

  // The headline is the ledger's income statement over the trailing
  // year — the window /exec's revenue card reads — not boss-commerce's
  // summary, which no Algedonic protocol writes (backlog 801b6a7a,
  // design c2820008 D1). Read once on mount: the ledger moves when a
  // packet posts, and the Income statement tab does not poll either.
  let headlineRead = $state<Remote<Headline>>({ kind: 'loading' });
  $effect(() => {
    let cancelled = false;
    void loadHeadline(trailingYear(appToday())).then((r) => {
      if (!cancelled) headlineRead = r;
    });
    return () => {
      cancelled = true;
    };
  });
  let headline = $derived(headlineText(headlineRead));
</script>

<div class="catalog theme-exec">
  <PageHeader
    eyebrow="Finance"
    title={headline.title}
    subtitle={headline.subtitle}
  />

  <div class="finance-actions" style="display:flex; gap:8px; align-items:flex-start; flex-wrap:wrap">
    {#if offerInvoice}
      <Link to={href('/finance/invoices/new')} className="fin-new-invoice">
        + New invoice
      </Link>
    {/if}
    {#if offerEntry}
      <Link to={href('/finance/journal-entries/new')} className="fin-new-invoice">
        + New journal entry
      </Link>
    {/if}
    <MonthlyClosePackageButton />
  </div>

  <nav class="tabs" role="tablist">
    {#each tabs as t (t.id)}
      <button
        type="button"
        role="tab"
        aria-selected={shown === t.id}
        class="tab {shown === t.id ? 'tab-active' : ''}"
        onclick={() => chooseTab(t.id)}
      >
        {t.label}
      </button>
    {/each}
  </nav>

  <div class="tab-panel">
    {#if shown === 'overview'}
      <OverviewTab
        {summary}
        {summaryRead}
        loading={summaryLoading}
        commerce={modules.commerce}
        warehouse={modules.warehouse}
      />
    {:else if shown === 'invoices'}
      <InvoicesTab
        {invoices}
        read={invoicesRead}
        totalCount={summary?.total_invoice_count ?? invoices.length}
      />
    {:else if shown === 'approvals'}
      <ApprovalsTab />
    {:else if shown === 'income-statement'}
      <IncomeStatementTab />
    {:else if shown === 'balance-sheet'}
      <BalanceSheetTab />
    {:else if shown === 'cash-flow'}
      <CashFlowTab />
    {:else if shown === 'trial-balance'}
      <TrialBalanceTab
        linkedEntryId={entry}
        linkedFactId={fact}
        onCloseLinked={() => {
          entry = '';
          fact = '';
        }}
      />
    {:else if shown === 'tax-liability'}
      <TaxLiabilityTab />
    {/if}
  </div>

  {#if department}
    <!-- Outside the tab panel, so the department's packets stand under
         every tab and a tab click never remounts (and re-reads) them. -->
    <section class="department-jobs">
      <h2>{departmentName} jobs</h2>
      <DepartmentThirds code={department} />
    </section>
  {/if}
</div>
