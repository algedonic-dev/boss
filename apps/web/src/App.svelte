<script lang="ts">
  // Root component — parses the URL, dispatches to the matched
  // page inside AppShell.
  //
  // Phase 1 wires /me, /jobs, /jobs/:id, /service, /sales, /refurb,
  // /assets, /assets/:id. Unmatched URLs fall back to My Day
  // (same as the React app's default).

  import { onMount } from 'svelte';
  import { parseRoute, type Route } from './router';
  import { DEMO_MODE, loadSession } from './session/session.svelte';
  import { loadManifest } from './session/manifest.svelte';
  import { loadStepTypeRegistry } from './steps/surfaceRegistry.svelte';
  import { loadClasses } from './session/classes.svelte';
  import AppShell from './shell/AppShell.svelte';
  import DebugGear from './debug/DebugGear.svelte';
  import MePage from './me/MePage.svelte';
  import JobsListPage from './jobs/JobsListPage.svelte';
  import JobDetailPage from './jobs/JobDetailPage.svelte';
  import MarketingAssetsList from './marketing-assets/MarketingAssetsList.svelte';
  import MarketingAssetPage from './marketing-assets/MarketingAssetPage.svelte';
  import AccountsList from './accounts/AccountsList.svelte';
  import AccountPage from './accounts/AccountPage.svelte';
  import VendorsList from './vendors/VendorsList.svelte';
  import VendorPage from './vendors/VendorPage.svelte';
  import PeopleList from './people/PeopleList.svelte';
  import EmployeePage from './people/EmployeePage.svelte';
  import PartsList from './parts/PartsList.svelte';
  import PartPage from './parts/PartPage.svelte';
  import ProductsList from './products/ProductsList.svelte';
  import ProductPage from './products/ProductPage.svelte';
  import ShippingPage from './shipping/ShippingPage.svelte';
  import ShipmentPage from './shipping/ShipmentPage.svelte';
  import SupportPage from './support/SupportPage.svelte';
  import FinancePage from './finance/FinancePage.svelte';
  import InvoicePage from './finance/InvoicePage.svelte';
  import NewInvoicePage from './finance/NewInvoicePage.svelte';
  import NewJournalEntryPage from './finance/NewJournalEntryPage.svelte';
  import HrPage from './hr/HrPage.svelte';
  import QaPage from './qa/QaPage.svelte';
  import OpsDashboard from './ops/OpsDashboard.svelte';
  // SimPage retired 2026-05-03 — boss-sim-api is gone (HumanWorker
  // generator retirement step 9b). Tenant runners are CLI tools now.
  import ItKnowledgeBasePage from './it/ItKnowledgeBasePage.svelte';
  import AtlasPage from './it/monitoring/AtlasPage.svelte';
  import PolicyPage from './policy/PolicyPage.svelte';
  import JobKindsPage from './job-kinds/JobKindsPage.svelte';
  import JobKindNewPage from './job-kinds/JobKindNewPage.svelte';
  import JobKindDesignWorkspace from './job-kinds/JobKindDesignWorkspace.svelte';
  import JobKindDetailPage from './job-kinds/JobKindDetailPage.svelte';
  import StepPluginsPage from './it/step-plugins/StepPluginsPage.svelte';
  import StepPluginDetailPage from './it/step-plugins/StepPluginDetailPage.svelte';
  import DispatcherCascadePage from './dispatcher/DispatcherCascadePage.svelte';
  import DesignReviewPage from './it/design/DesignReviewPage.svelte';
  import InboxPage from './inbox/InboxPage.svelte';
  import CalendarPage from './calendar/CalendarPage.svelte';
  import MyCalendarPage from './calendar/MyCalendarPage.svelte';
  import SchedulePage from './schedule/SchedulePage.svelte';
  import ExecPage from './exec/ExecPage.svelte';
  import WarehousePage from './warehouse/WarehousePage.svelte';
  import CatalogBrowser from './catalog/CatalogBrowser.svelte';
  import DevicePage from './catalog/DevicePage.svelte';
  import AssetsList from './assets/AssetsList.svelte';
  import AssetPage from './assets/AssetPage.svelte';
  import ManualPage from './content/ManualPage.svelte';
  import WorkflowsPage from './kb/WorkflowsPage.svelte';
  import MonitoringPage from './it/monitoring/MonitoringPage.svelte';
  import PerfPage from './it/monitoring/PerfPage.svelte';
  import EventsPage from './it/monitoring/EventsPage.svelte';
  import PoPage from './po/PoPage.svelte';
  import VendorInvoicePage from './po/VendorInvoicePage.svelte';
  import WatchlistPage from './accounts/WatchlistPage.svelte';
  import ShopHome from './shop/ShopHome.svelte';
  import ShopProductPage from './shop/ShopProductPage.svelte';
  import LandingPage from './landing/LandingPage.svelte';
  import LoginPage from './auth/LoginPage.svelte';
  import AuthAdminPage from './auth/AuthAdminPage.svelte';
  import ModuleDisabled from './shell/ModuleDisabled.svelte';
  import { moduleEnabled } from './session/manifest.svelte';

  let route = $state<Route>(parseRoute(window.location.pathname));

  // Map route.kind → tenant module-id. Routes whose module is
  // flagged false in tenant.toml render a "not enabled" notice
  // instead of an empty/broken page. Routes not listed here are
  // always-on (jobs, people, finance, etc. — never gated).
  function routeRequiredModule(kind: Route['kind']): { id: string; label: string } | null {
    switch (kind) {
      case 'support':
      case 'service':
      case 'refurb':          return { id: 'support',   label: 'Support / Service' };
      case 'shipping':
      case 'shipmentDetail':  return { id: 'shipping',  label: 'Shipments' };
      case 'calendar':        return { id: 'calendar',  label: 'Release calendar' };
      case 'marketingAssets':
      case 'marketingAsset':  return { id: 'marketing-assets', label: 'Marketing assets' };
      case 'catalog':
      case 'device':
      case 'assets':
      case 'asset':           return { id: 'equipment', label: 'Equipment' };
      case 'shop':
      case 'shopProduct':     return { id: 'shop',      label: 'Shop' };
      case 'exec':            return { id: 'exec',      label: 'Exec' };
      default:                return null;
    }
  }

  let blockedModule = $derived.by(() => {
    const req = routeRequiredModule(route.kind);
    if (req && !moduleEnabled(req.id)) return req;
    return null;
  });

  // 401-redirect interceptor. Wraps window.fetch so any
  // /api/* response that comes back unauthenticated kicks the
  // operator to /login with the current path captured as ?next=.
  //
  // Skipped in demo mode: anonymous demo visitors HAVE a valid
  // (audit-readonly) session and most 401s in their flow are
  // intentional — e.g., /api/auth/me returns 401 for demo
  // sessions (that's what tells the AppShell to render
  // "Sign in" instead of "Sign out"). Auto-redirecting on every
  // such 401 breaks anonymous browsing entirely. In demo mode
  // the visitor reaches /login via the explicit "Sign in" link,
  // never via an interceptor.
  if (!DEMO_MODE) {
    const _origFetch = window.fetch;
    window.fetch = (async (
      input: RequestInfo | URL,
      init?: RequestInit,
    ): Promise<Response> => {
      const resp = await _origFetch(input, init);
      if (resp.status === 401 && window.location.pathname !== '/login') {
        const url = typeof input === 'string'
          ? input
          : input instanceof URL ? input.href : input.url;
        // Only redirect on /api/* — let app-internal 401 handling
        // for non-API resources stay where the call was made.
        if (url.startsWith('/api/')) {
          const next = encodeURIComponent(window.location.pathname + window.location.search);
          window.location.href = `/login?next=${next}`;
        }
      }
      return resp;
    }) as typeof window.fetch;
  }

  onMount(() => {
    loadSession();
    loadManifest();
    loadStepTypeRegistry();
    loadClasses('employee');
    const onPop = () => {
      route = parseRoute(window.location.pathname);
    };
    window.addEventListener('popstate', onPop);
    return () => window.removeEventListener('popstate', onPop);
  });

  // Map each route to the sidebar id that should highlight.
  let activeSection = $derived(
    route.kind === 'me' ? 'me'
      : route.kind === 'inbox' ? 'inbox'
      : route.kind === 'jobs' || route.kind === 'jobDetail' ? 'jobs'
      : route.kind === 'service' ? 'service'
      : route.kind === 'sales' ? 'sales'
      : route.kind === 'refurb' ? 'refurb'
      : route.kind === 'assets' || route.kind === 'asset' ? 'assets'
      : route.kind === 'accounts' || route.kind === 'account' ? 'accounts'
      : route.kind === 'vendors' || route.kind === 'vendor' ? 'vendors'
      : route.kind === 'people' || route.kind === 'employee' ? 'people'
      : route.kind === 'parts' || route.kind === 'part' ? 'parts'
      : route.kind === 'finance' || route.kind === 'invoice' ? 'finance'
      : route.kind === 'shipping' || route.kind === 'shipmentDetail' ? 'shipping'
      : route.kind === 'support' ? 'support'
      : route.kind === 'hr' ? 'hr'
      : route.kind === 'qa' ? 'qa'
      : route.kind === 'ops' ? 'ops'
      : route.kind === 'calendar' ? 'calendar'
      : route.kind === 'schedule' ? 'schedule'
      : route.kind === 'exec' ? 'exec'
      : route.kind === 'itMonitoring' || route.kind === 'itMonitoringPerf' || route.kind === 'itMonitoringEvents' || route.kind === 'itMonitoringAtlas' ? 'itMonitoring'
      : route.kind === 'warehouse' ? 'warehouse'
      : route.kind === 'catalog' || route.kind === 'device' ? 'catalog'
      : route.kind === 'marketingAssets' || route.kind === 'marketingAsset' ? 'marketing-assets'
      : route.kind === 'manual' || route.kind === 'manualSection' ? 'manual'
      : route.kind === 'policy' ? 'policy'
      : route.kind === 'jobKinds' || route.kind === 'jobKindNew' || route.kind === 'jobKindDesign' || route.kind === 'jobKindDetail' ? 'jobKinds'
      : route.kind === 'itStepPlugins' || route.kind === 'itStepPluginDetail' ? 'itStepPlugins'
      : route.kind === 'workflows' ? 'workflows'
      : 'me',
  );
</script>

{#if route.kind === 'login'}
  <LoginPage />
{:else}
<AppShell {activeSection}>
  {#if blockedModule}
    <ModuleDisabled module={blockedModule.id} label={blockedModule.label} />
  {:else if route.kind === 'home'}
      <LandingPage />
    {:else if route.kind === 'authAdmin'}
      <AuthAdminPage />
    {:else if route.kind === 'me'}
      <MePage />
    {:else if route.kind === 'jobs'}
      <JobsListPage
        initialKind={route.jobKind ?? ''}
        initialKindPrefix={route.jobKindPrefix ?? ''}
        initialStatus={route.jobStatus ?? 'open'}
        initialOwnerId={route.jobOwnerId ?? ''}
        initialSubjectKind={route.jobSubjectKind ?? ''}
        initialSubjectId={route.jobSubjectId ?? ''}
        initialNewJobOpen={route.newJobOpen ?? false}
        initialNewJobSubjectKind={route.newJobSubjectKind ?? ''}
        initialNewJobSubjectId={route.newJobSubjectId ?? ''}
      />
    {:else if route.kind === 'jobDetail'}
      <JobDetailPage jobId={route.jobId} />
    {:else if route.kind === 'service'}
      <JobsListPage
        initialKind="field-service"
        initialStatus="open"
        pageTitle="Service queue"
      />
    {:else if route.kind === 'sales'}
      <JobsListPage
        initialKind="sale"
        initialStatus="open"
        pageTitle="Sales pipeline"
      />
    {:else if route.kind === 'refurb'}
      <!-- /refurb is the device-shop tenant's service queue.
           Page title generalizes — tenants that don't run a
           refurb pipeline (brewery) just see an empty list. -->
      <JobsListPage
        initialKindPrefix="refurb"
        initialStatus="open"
        pageTitle="Service queue"
      />
    {:else if route.kind === 'assets'}
      <AssetsList />
    {:else if route.kind === 'asset'}
      <AssetPage assetId={route.assetId} />
    {:else if route.kind === 'accounts'}
      <AccountsList />
    {:else if route.kind === 'account'}
      <AccountPage accountId={route.accountId} />
    {:else if route.kind === 'vendors'}
      <VendorsList />
    {:else if route.kind === 'vendor'}
      <VendorPage vendorLookup={route.vendorLookup} />
    {:else if route.kind === 'people'}
      <PeopleList />
    {:else if route.kind === 'employee'}
      <EmployeePage empId={route.empId} />
    {:else if route.kind === 'parts'}
      <PartsList />
    {:else if route.kind === 'part'}
      <PartPage partSku={route.partSku} />
    {:else if route.kind === 'products'}
      <ProductsList />
    {:else if route.kind === 'product'}
      <ProductPage sku={route.productSku} />
    {:else if route.kind === 'shipping'}
      <ShippingPage />
    {:else if route.kind === 'shipmentDetail'}
      <ShipmentPage shipmentId={route.shipmentId} />
    {:else if route.kind === 'support'}
      <SupportPage />
    {:else if route.kind === 'finance'}
      <FinancePage />
    {:else if route.kind === 'newInvoice'}
      <NewInvoicePage />
    {:else if route.kind === 'newJournalEntry'}
      <NewJournalEntryPage />
    {:else if route.kind === 'invoice'}
      <InvoicePage invoiceId={route.invoiceId} />
    {:else if route.kind === 'hr'}
      <HrPage />
    {:else if route.kind === 'qa'}
      <QaPage />
    {:else if route.kind === 'ops'}
      <OpsDashboard />
    {:else if route.kind === 'itKb'}
      <ItKnowledgeBasePage />
    {:else if route.kind === 'policy'}
      <PolicyPage />
    {:else if route.kind === 'jobKinds'}
      <JobKindsPage />
    {:else if route.kind === 'jobKindNew'}
      <JobKindNewPage />
    {:else if route.kind === 'jobKindDesign'}
      <JobKindDesignWorkspace jobId={route.jobId} />
    {:else if route.kind === 'jobKindDetail'}
      <JobKindDetailPage kindSlug={route.kindSlug} />
    {:else if route.kind === 'itStepPlugins'}
      <StepPluginsPage />
    {:else if route.kind === 'itStepPluginDetail'}
      <StepPluginDetailPage pluginSlug={route.pluginSlug} />
    {:else if route.kind === 'itDesign'}
      <DesignReviewPage />
    {:else if route.kind === 'dispatcherRules'}
      <DispatcherCascadePage />
    {:else if route.kind === 'inbox'}
      <InboxPage />
    {:else if route.kind === 'calendar'}
      <CalendarPage />
    {:else if route.kind === 'myCalendar'}
      <MyCalendarPage />
    {:else if route.kind === 'schedule'}
      <SchedulePage />
    {:else if route.kind === 'exec'}
      <ExecPage />
    {:else if route.kind === 'warehouse'}
      <WarehousePage />
    {:else if route.kind === 'catalog'}
      <CatalogBrowser />
    {:else if route.kind === 'device'}
      <DevicePage sku={route.sku} />
    {:else if route.kind === 'marketingAssets'}
      <MarketingAssetsList />
    {:else if route.kind === 'marketingAsset'}
      <MarketingAssetPage assetId={route.assetId} />
    {:else if route.kind === 'manual'}
      <ManualPage slug={null} />
    {:else if route.kind === 'manualSection'}
      <ManualPage slug={route.slug} />
    {:else if route.kind === 'workflows'}
      <WorkflowsPage />
    {:else if route.kind === 'itMonitoring'}
      <MonitoringPage />
    {:else if route.kind === 'itMonitoringPerf'}
      <PerfPage />
    {:else if route.kind === 'itMonitoringEvents'}
      <EventsPage />
    {:else if route.kind === 'itMonitoringAtlas'}
      <AtlasPage />
    {:else if route.kind === 'po'}
      <PoPage poId={route.poId} />
    {:else if route.kind === 'vendorInvoice'}
      <VendorInvoicePage vendorInvoiceId={route.vendorInvoiceId} />
    {:else if route.kind === 'watchlist'}
      <WatchlistPage />
    {:else if route.kind === 'shop'}
      <ShopHome />
    {:else if route.kind === 'shopProduct'}
      <ShopProductPage sku={route.sku} />
    {:else}
      <MePage />
    {/if}
</AppShell>
{/if}

<DebugGear />
