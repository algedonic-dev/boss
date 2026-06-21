// Tiny router — same shape as apps/web/src/router.ts.
//
// Phase 1: covers the routes the primitive-touching pages hang off
// (jobs/service/refurb/sales + assets + asset detail + home).
// Phase 2 expands to every route the React app knows about.
// URLs stay identical so deep-links work across the flip.

export type Route =
  | { kind: 'home' }
  | { kind: 'login' }
  | { kind: 'authAdmin' }
  | { kind: 'me' }
  | {
      kind: 'jobs';
      jobKind?: string;
      jobKindPrefix?: string;
      jobStatus?: string;
      // #93: filter by Job.owner_id so "View this employee's
      // assigned jobs" links actually filter the list.
      jobOwnerId?: string;
      // #93: filter by Job.subject_id (with optional
      // subject_kind disambiguator). For "View this account's
      // jobs", "View this vendor's POs", etc.
      jobSubjectKind?: string;
      jobSubjectId?: string;
      // Phase 3 of the create-Job UX work: deep-link from a
      // Subject detail page opens the form pre-filled.
      newJobOpen?: boolean;
      newJobSubjectKind?: string;
      newJobSubjectId?: string;
    }
  | { kind: 'jobDetail'; jobId: string }
  | { kind: 'service' }
  | { kind: 'sales' }
  | { kind: 'refurb' }
  | { kind: 'accounts' }
  | { kind: 'account'; accountId: string }
  | { kind: 'vendors' }
  | { kind: 'vendor'; vendorLookup: string }
  | { kind: 'people' }
  | { kind: 'employee'; empId: string }
  | { kind: 'parts' }
  | { kind: 'part'; partSku: string }
  | { kind: 'products' }
  | { kind: 'product'; productSku: string }
  | { kind: 'finance' }
  | { kind: 'newInvoice' }
  | { kind: 'newJournalEntry' }
  | { kind: 'invoice'; invoiceId: string }
  | { kind: 'vendorInvoice'; vendorInvoiceId: string }
  | { kind: 'shipping' }
  | { kind: 'shipmentDetail'; shipmentId: string }
  | { kind: 'support' }
  | { kind: 'hr' }
  | { kind: 'qa' }
  | { kind: 'ops' }
  // 'itSim' retired 2026-05-03 with boss-sim-api (HumanWorker step 9b).
  | { kind: 'itKb' }
  | { kind: 'itMonitoring' }
  | { kind: 'itMonitoringPerf' }
  | { kind: 'itMonitoringEvents' }
  | { kind: 'itMonitoringAtlas' }
  | { kind: 'policy' }
  | { kind: 'jobKinds' }
  | { kind: 'jobKindNew' }
  | { kind: 'jobKindDesign'; jobId: string }
  | { kind: 'jobKindDetail'; kindSlug: string }
  | { kind: 'itStepPlugins' }
  | { kind: 'itStepPluginDetail'; pluginSlug: string }
  | { kind: 'itDesign' }
  | { kind: 'inbox' }
  | { kind: 'calendar' }
  | { kind: 'myCalendar' }
  | { kind: 'schedule' }
  | { kind: 'exec' }
  | { kind: 'warehouse' }
  | { kind: 'catalog' }
  | { kind: 'device'; sku: string }
  | { kind: 'assets' }
  | { kind: 'asset'; assetId: string }
  | { kind: 'marketingAssets' }
  | { kind: 'marketingAsset'; assetId: string }
  | { kind: 'manual' }
  | { kind: 'manualSection'; slug: string }
  | { kind: 'workflows' }
  | { kind: 'po'; poId: string }
  | { kind: 'watchlist' }
  | { kind: 'shop' }
  | { kind: 'shopProduct'; sku: string };

export function parseRoute(pathname: string): Route {
  const p = pathname.replace(/^\/dashboard/, '').replace(/\/$/, '') || '/';
  if (p === '/login') return { kind: 'login' };
  if (p === '/auth-admin') return { kind: 'authAdmin' };
  if (p === '/me') return { kind: 'me' };
  if (p === '/inbox') return { kind: 'inbox' };
  if (p === '/accounts') return { kind: 'accounts' };
  const cm = p.match(/^\/accounts\/(.+)$/);
  if (cm) return { kind: 'account', accountId: cm[1]! };

  if (p === '/vendors') return { kind: 'vendors' };
  const vm = p.match(/^\/vendors\/(.+)$/);
  if (vm) return { kind: 'vendor', vendorLookup: decodeURIComponent(vm[1]!) };

  if (p === '/people') return { kind: 'people' };
  const em = p.match(/^\/people\/(.+)$/);
  if (em) return { kind: 'employee', empId: em[1]! };

  if (p === '/parts') return { kind: 'parts' };
  const partM = p.match(/^\/parts\/(.+)$/);
  if (partM) return { kind: 'part', partSku: decodeURIComponent(partM[1]!) };

  if (p === '/products') return { kind: 'products' };
  const prodM = p.match(/^\/products\/(.+)$/);
  if (prodM) return { kind: 'product', productSku: decodeURIComponent(prodM[1]!) };

  if (p === '/finance') return { kind: 'finance' };
  if (p === '/finance/new') return { kind: 'newInvoice' };
  if (p === '/finance/journal-entries/new') return { kind: 'newJournalEntry' };
  // Wildcard MUST come after every specific `/finance/X` case above —
  // it eagerly matches any tail and would otherwise eclipse them.
  const invM = p.match(/^\/finance\/(.+)$/);
  if (invM) return { kind: 'invoice', invoiceId: decodeURIComponent(invM[1]!) };

  if (p === '/shipping') return { kind: 'shipping' };
  const shipM = p.match(/^\/shipments\/(.+)$/);
  if (shipM) return { kind: 'shipmentDetail', shipmentId: decodeURIComponent(shipM[1]!) };

  if (p === '/support') return { kind: 'support' };

  if (p === '/calendar/me') return { kind: 'myCalendar' };
  if (p === '/calendar') return { kind: 'calendar' };
  if (p === '/service/schedule') return { kind: 'schedule' };
  if (p === '/exec') return { kind: 'exec' };
  // /cto + /perf retired in favor of the IT-as-department IA.
  // The new locations live under /it/monitoring/*; /cto/atlas
  // similarly migrates below.
  if (p === '/it/monitoring') return { kind: 'itMonitoring' };
  if (p === '/it/monitoring/perf') return { kind: 'itMonitoringPerf' };
  if (p === '/it/monitoring/events') return { kind: 'itMonitoringEvents' };
  if (p === '/it/monitoring/atlas') return { kind: 'itMonitoringAtlas' };
  if (p === '/warehouse') return { kind: 'warehouse' };
  if (p === '/catalog') return { kind: 'catalog' };
  const catM = p.match(/^\/catalog\/(.+)$/);
  if (catM) return { kind: 'device', sku: decodeURIComponent(catM[1]!) };
  if (p === '/assets') return { kind: 'assets' };
  const assetM = p.match(/^\/assets\/(.+)$/);
  if (assetM) return { kind: 'asset', assetId: decodeURIComponent(assetM[1]!) };
  if (p === '/marketing-assets') return { kind: 'marketingAssets' };
  const mktM = p.match(/^\/marketing-assets\/(.+)$/);
  if (mktM) return { kind: 'marketingAsset', assetId: decodeURIComponent(mktM[1]!) };
  if (p === '/manual') return { kind: 'manual' };
  if (p === '/workflows') return { kind: 'workflows' };
  const mManual = p.match(/^\/manual\/(.+)$/);
  if (mManual) return { kind: 'manualSection', slug: decodeURIComponent(mManual[1]!) };
  const poM = p.match(/^\/purchase-orders\/(.+)$/);
  if (poM) return { kind: 'po', poId: decodeURIComponent(poM[1]!) };
  const viM = p.match(/^\/vendor-invoices\/(.+)$/);
  if (viM) return { kind: 'vendorInvoice', vendorInvoiceId: decodeURIComponent(viM[1]!) };
  if (p === '/watchlist') return { kind: 'watchlist' };
  if (p === '/shop') return { kind: 'shop' };
  const shopM = p.match(/^\/shop\/(.+)$/);
  if (shopM) return { kind: 'shopProduct', sku: decodeURIComponent(shopM[1]!) };

  if (p === '/hr') return { kind: 'hr' };
  if (p === '/qa') return { kind: 'qa' };
  if (p.startsWith('/ops')) return { kind: 'ops' };
  // /it/sim retired 2026-05-03 with boss-sim-api (HumanWorker step 9b).
  if (p === '/it/kb') return { kind: 'itKb' };

  if (p === '/policy') return { kind: 'policy' };
  // `/admin/job-kinds` is the README's documented authoring URL
  // (the BOSS framing positions JobKinds as admin-tier authoring).
  // Internally everything lives under /job-kinds/*; the /admin
  // prefix is an alias kept stable for the public-facing docs.
  if (p === '/job-kinds' || p === '/admin/job-kinds') return { kind: 'jobKinds' };
  if (p === '/job-kinds/new' || p === '/admin/job-kinds/new') return { kind: 'jobKindNew' };
  // The authoring-workspace route is keyed by the design Job's id and
  // must match before the catch-all detail pattern, which would
  // otherwise swallow `authoring/<jobId>` as a slug.
  const jkDesignM = p.match(/^\/(?:admin\/)?job-kinds\/authoring\/(.+)$/);
  if (jkDesignM) return { kind: 'jobKindDesign', jobId: decodeURIComponent(jkDesignM[1]!) };
  const jkM = p.match(/^\/(?:admin\/)?job-kinds\/(.+)$/);
  if (jkM) return { kind: 'jobKindDetail', kindSlug: decodeURIComponent(jkM[1]!) };
  if (p === '/it/step-plugins') return { kind: 'itStepPlugins' };
  const spM = p.match(/^\/it\/step-plugins\/(.+)$/);
  if (spM) return { kind: 'itStepPluginDetail', pluginSlug: decodeURIComponent(spM[1]!) };
  if (p === '/it/design') return { kind: 'itDesign' };

  if (p === '/service') return { kind: 'service' };
  const tm = p.match(/^\/service\/(.+)$/);
  if (tm) return { kind: 'jobDetail', jobId: tm[1]! };

  if (p === '/refurb') return { kind: 'refurb' };
  const rm = p.match(/^\/refurb\/(.+)$/);
  if (rm) return { kind: 'jobDetail', jobId: rm[1]! };

  if (p === '/sales') return { kind: 'sales' };
  const sm = p.match(/^\/sales\/(.+)$/);
  if (sm) return { kind: 'jobDetail', jobId: sm[1]! };

  if (p === '/jobs') {
    const sp = new URLSearchParams(window.location.search);
    const jk = sp.get('kind');
    const jkp = sp.get('kind_prefix');
    const js = sp.get('status');
    const newJob = sp.get('new');
    const sk = sp.get('subject_kind');
    const sid = sp.get('subject_id');
    // #93: read list-filter params (separate from new-job params).
    // owner_id filters by Job.owner_id; subject_id filters by
    // Job.subject_id.
    const ownerId = sp.get('owner_id');
    const filterSubjectKind = sp.get('filter_subject_kind');
    const filterSubjectId = sp.get('subject_id');
    const r: Route = { kind: 'jobs' };
    if (jk) (r as { jobKind?: string }).jobKind = jk;
    if (jkp) (r as { jobKindPrefix?: string }).jobKindPrefix = jkp;
    if (js) (r as { jobStatus?: string }).jobStatus = js;
    if (ownerId) (r as { jobOwnerId?: string }).jobOwnerId = ownerId;
    if (filterSubjectId) (r as { jobSubjectId?: string }).jobSubjectId = filterSubjectId;
    if (filterSubjectKind) (r as { jobSubjectKind?: string }).jobSubjectKind = filterSubjectKind;
    if (newJob === '1') (r as { newJobOpen?: boolean }).newJobOpen = true;
    if (sk) (r as { newJobSubjectKind?: string }).newJobSubjectKind = sk;
    if (sid) (r as { newJobSubjectId?: string }).newJobSubjectId = sid;
    return r;
  }
  const jm = p.match(/^\/jobs\/(.+)$/);
  if (jm) return { kind: 'jobDetail', jobId: jm[1]! };

  return { kind: 'home' };
}

/** Absolute path that honors the /dashboard mount point. */
export function href(relative: string): string {
  const base = window.location.pathname.startsWith('/dashboard') ? '/dashboard' : '';
  return base + (relative.startsWith('/') ? relative : `/${relative}`);
}

/** Programmatic navigation without a full page reload. */
export function navigate(path: string): void {
  window.history.pushState({}, '', path);
  window.dispatchEvent(new PopStateEvent('popstate'));
}
