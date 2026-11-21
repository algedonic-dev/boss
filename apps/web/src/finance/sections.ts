// Which of /finance's sections a tenant runs (backlog 801b6a7a, page
// audit 3f964c57 gap 3; f6d54f7b; design c2820008 D1).
//
// Two of the page's sources are company-layer modules, not the ledger:
// boss-commerce (AR aging, gross margin by product line, the Invoices
// tab, the New invoice offer) and boss-inventory (AP aging, PO
// Approvals). Algedonic runs neither — its manifest lists no `commerce`
// and `warehouse = false`, and the event log holds no event from source
// commerce — so those sections answered from a source no protocol of
// the tenant writes, as zeros beside an Income statement that read
// otherwise. D1: a section whose module the tenant does not run is
// ABSENT, not zero, and a failed read stays a load-failed line (the
// tabs' own read states). The headline reads the ledger for every
// tenant since car 80145201, so it needs no module.
//
// `commerce` is its own key, not `shop`: the storefront is one writer
// of the invoice book, not the book (the 2026-11-01 conductor decision
// on 801b6a7a refused a shop=commerce inference).
//
// Pure, so bun test exercises it; FinancePage hands it the manifest the
// route gate has already read (App renders /finance only once it is
// `ready`, so an unread manifest never reaches here as "off").

import { moduleOn, type ManifestState } from '@boss/web-kit/session/manifest-inline';
import { FINANCE_TABS, type FinanceTab } from './financeQuery';

export type FinanceModules = Readonly<{ commerce: boolean; warehouse: boolean }>;

export function financeModules(state: ManifestState): FinanceModules {
  return { commerce: moduleOn(state, 'commerce'), warehouse: moduleOn(state, 'warehouse') };
}

/// The Overview is AR aging + gross margin (commerce) and AP aging
/// (warehouse); with neither it would be an empty tab, so it goes too.
export function overviewShown(m: FinanceModules): boolean {
  return m.commerce || m.warehouse;
}

function runs(tab: FinanceTab, m: FinanceModules): boolean {
  switch (tab) {
    case 'overview':
      return overviewShown(m);
    case 'invoices':
      return m.commerce;
    case 'approvals':
      return m.warehouse;
    default:
      // The five statements read the ledger, which every tenant runs.
      return true;
  }
}

/// The tabs this tenant's page draws, in FINANCE_TABS order.
export function financeTabs(m: FinanceModules): ReadonlyArray<(typeof FINANCE_TABS)[number]> {
  return FINANCE_TABS.filter((t) => runs(t.id, m));
}

/// The tab to show for `asked`: itself when the tenant runs it, else the
/// first tab the tenant does run — so a deep link to ?tab=invoices, or
/// the default Overview, on a tenant without commerce opens a page that
/// exists instead of an empty one. The income statement is always there.
export function tabShown(asked: FinanceTab, m: FinanceModules): FinanceTab {
  return runs(asked, m) ? asked : (financeTabs(m)[0]?.id ?? 'income-statement');
}
