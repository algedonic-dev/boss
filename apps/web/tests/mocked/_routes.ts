// The roster of crawled surfaces, and the ONE marker a surface uses to
// say a read failed. Both are read by two specs, so both live here.
//
// WHY THIS FILE EXISTS (CLAUDE.md §9a). `ROUTES` used to live inside
// route-smoke.mocked.spec.ts. outage-crawl.mocked.spec.ts needs the same
// roster, and a second copy of it would reproduce — exactly — the defect
// route-smoke's own drift test was written to stop: a crawl that
// silently omits a surface reports success for a page it never loaded.
// A route added to one list and not the other would be crash-checked and
// never outage-checked, and nothing would say so. So the list is
// collapsed to one definition rather than pinned by an equality test.

/// Every top-level surface a ceo persona reaches, from the router's
/// exact-match routes. Pure-action / form-submit routes (/login,
/// /finance/new, /finance/journal-entries/new) are excluded — the crawls
/// assert surfaces RENDER, not that forms submit. Detail routes are
/// included (a Workflow, a marketing asset, an employee) because the mock seeds them,
/// and that is where the omitted-field crashes live — and one per
/// parameterised catalog path (the rule editor), because a pattern is
/// not a URL a crawl can open.
///
/// Pinned against the route catalog by route-smoke.mocked.spec.ts, so a
/// newly registered surface is crawled by default — by BOTH crawls.
import { VENDOR_CRAWL_PATH } from '../fixtures/vendor';

export const ROUTES: ReadonlyArray<string> = [
  // User Experiences perspective — bare / is the public home alias; the
  // operator surfaces are re-rooted under /ux/*.
  // The /ux business pages (accounts, people, parts, products, shipping,
  // assets, catalog, marketing-assets, support, service, qa, sales, shop,
  // exec, warehouse) and the jobs list went with step 1 of design
  // 01fa5bdf; RETIRED_ROUTES below holds each to not-found.
  '/', '/me', '/me/inbox', '/finance/vendors',
  // e9c8e28d: focused specs already open VendorPage; shared crawls need
  // their own answered representative of the same native detail route.
  VENDOR_CRAWL_PATH,
  //6d6f88bd separates Finance detail availability from Vendors. These
  // concrete addresses open the catalog's native route-field patterns.
  '/finance/purchase-orders/po-review', '/finance/vendor-invoices/bill-review',
  '/me/calendar',
  '/engineering/manual',
  // Finance — DEFERRED until 2026-09-25 for want of object-shaped
  // statement fixtures, which _smokeMocks.ts now carries (page audit
  // 3f964c57, backlog e0732f75). Under the empty leg every statement
  // answers its well-formed empty body; under the outage, Overview's
  // two failure lines wear the marker.
  '/finance',
  // The IT department — six surfaces, families as tabs (1f6d55e0).
  // /system is GONE (David's Q1/Q4: no legacy users, no redirects), so
  // this list crawls exactly what the catalog declares and nothing
  // else answers.
  // Since backlog 0bc2eb68 each page answers at its host station's
  // address in the tree plus its slug (the fixture tree's, which the
  // floor serves), and the alarms board, hosted at the root, at /alarms;
  // /it renders it too, as IT's landing, because the IT tab links there,
  // until the departments registry retires the row (car 2 of design
  // df0ecd8b). The station tree floor is the root's page at /tree since
  // backlog 92bfcb04 (car 3 of that design).
  '/it', '/alarms', '/tree', '/engineering/registry/subjects', '/engineering/registry/dispatcher', '/engineering/registry/rules',
  // The rule editor — a detail route, because its catalog path is the
  // pattern /engineering/registry/rules/:ruleName (car 3071e235) and a pattern
  // is not a URL. route-smoke's drift test holds every parameterised
  // catalog path to a row here that routes to it (backlog d7732e88).
  // Under the mock's `[]` catch-all the versions read answers empty, so
  // the editor paints "No versions found"; under the outage it paints
  // `load-failed`.
  '/engineering/registry/rules/auto-park-on-gate-green',
  '/operations/perf',
  '/engineering/registry/step-plugins', '/engineering/kb', '/engineering/design',
  // Modeling + admin surfaces (System Model).
  '/engineering/registry', '/engineering/registry/new',
  '/engineering/registry/seasonal-release', '/operations/vault/auth-admin',
  // '/it/design/feedback' and '/it/design/backlog' were crawled here
  // until car N3 of design e765b3fc (2026-09-25) made each a station's
  // panel on the Department Map; both panels went with the map in step 1
  // of design 01fa5bdf, as did Incidents, Atlas, Bottlenecks,
  // Experiments and Policy.
  // The Audit Log. Deferred until 2026-09-23
  // for want of an object-shaped /api/events/stats fixture, which
  // _smokeMocks.ts now carries (EVENTS_STATS); its live stream answers
  // the floor's 204, so the page falls to its snapshot poll. Its own
  // controls are pinned in audit-log-page.mocked.spec.ts (65a273d5).
  '/operations/audit',
  // THE ALARMS BOARD AND THE STATIONS THAT KEEP A BOARD (design
  // 01fa5bdf step 1): what stays of the Department Map. Only the
  // track, the dock and the garage still carry boards — the release and
  // cancel controls (panel.ts `STATION_BOARDS`) — so the crawl opens
  // those three selections: each board renders its empty states under
  // the mocks and its `load-failed` line under the outage. The other
  // stations' boards went with the map. The board itself is crawled
  // above; its selections ride its own address since backlog 92bfcb04.
  '/alarms?at=dock',
  '/alarms?at=track',
  '/alarms?at=garage',
  // The codebase — its own sidebar row since feedback 9827c699
  // (2026-09-14; a Design tab before, backlog 06048ade). Its one read is
  // `/api/jobs?kind=maintenance-codebase-metrics`; under the mock's `[]`
  // catch-all it renders "No codebase-metrics packet exists yet — the
  // 05:10 measurement has not filed" as a bordered notice, and under the
  // outage it renders `load-failed`. The row's path is what the catalog
  // registers (the older tab path is in RETIRED_ROUTES below).
  '/engineering/codebase',
  // Protocol drift — a Registry tab (4ae9969e). Its one read is
  // `/api/jobs?kind=maintenance-protocol-drift`; under the mock's `[]`
  // catch-all it renders "the daily 05:20Z measurement on boss-gcp has
  // not filed a packet" as a bordered notice, and under the outage it
  // renders `load-failed`.
  '/engineering/registry/drift',
  // Agents — a Registry tab (62988516). Its roster read is `/api/agents`;
  // under the mock's `[]` catch-all it renders "the registry holds no
  // agent", and under the outage it renders `load-failed`. The per-agent
  // reads (runs, held steps) fire only for a listed agent.
  '/engineering/registry/agents',
  // Credentials — a Registry tab (851259b9). Its registry read is
  // `/api/credentials`; under the mock's `[]` catch-all it renders "the
  // registry holds no credential" beside the declare form, and under the
  // outage it renders `load-failed`.
  '/operations/vault/credentials',
  // The risk watchlist. Since CAR-6 it HAS a catalog entry, so the
  // drift test in route-smoke.mocked.spec.ts now enforces its presence
  // here instead of this line being the whole of its coverage.
  '/watchlist',
  // The operator manual joined the catalog with the watchlist (CAR-6):
  // it renders its docs chrome with the fetch failing honestly — chrome
  // + no-crash, same bar as every other row. HR, which joined with it,
  // went with step 1 of design 01fa5bdf. Hosted at Engineering since
  // backlog 0bc2eb68, so it is crawled once, at /engineering/manual above.
  // The estate page under the mock's catch-all: every /api/estate/*
  // fetch fails or reads empty, and the page's whole design is that
  // absence renders as bordered failure notices, never an empty
  // estate - chrome + three honest failure states, no crash. Its own
  // unit suite pins the failed-never-empty arms; this crawl pins that
  // the route actually mounts.
  '/operations/estate',
  // A department's jobs view — in / working / out (cc76f755). One
  // surface for every department the registry declares, so it has no
  // catalog entry and is crawled here by one code; the mock's `[]`
  // catch-all for `/api/jobs?department=sales` renders the "no jobs in
  // Sales" state, and the outage renders `load-failed`. At /<code>/jobs
  // since car 2 of design 8c3e9599; /ux/departments/<code> retired with
  // no alias.
  '/sales/jobs',
  // A department with no catalog surface, by its own address: the tab's
  // /<code> lands on its jobs view (design 8c3e9599 §5). The smoke
  // roster carries `refurb`, which owns no page.
  '/refurb',
  // The landing page (the System Model live view). It was reachable only
  // through the router's catch-all, crawled here as '/ux/unknown-path',
  // until design ee3a3a2f gave it this door.
  '/ux/system-model',
  // The router's catch-all — see NOT_FOUND_ROW.
  '/ux/accounts/agreements/x',
];

/// The one ROUTES entry the router does NOT serve, on purpose: an
/// unmatched path renders the not-found page, naming the path (design
/// ee3a3a2f). It is spelled as one of the dead links that motivated that
/// page — a greedy `/accounts/(.+)` used to render it as the account page
/// for a missing account "agreements/x". Before that the catch-all
/// rendered the landing page, and until 2026-09-18 this row was spelled
/// '/ux/refurb' while both crawls believed they were crawling a refurb
/// page. interaction-crawl pins every OTHER row to the router (f2b8a01c).
export const NOT_FOUND_ROW = '/ux/accounts/agreements/x';

/// THE PAGES THE DEPARTMENT MAP REPLACED — design e765b3fc, car N3
/// (2026-09-25). Each path answered until then and is not found now, like
/// any path nothing serves: its content is a selection's panel on the
/// map (the floors and the Crew Board are their stations', yard status
/// the track's, dock's and garage's, the conductor's feed the track's,
/// the feedback and backlog boards receiving's and marshalling's —
/// src/it/yard/panel.ts `STATION_BOARDS`), and David's word was no
/// aliases and no redirects before 1.0.0. So none of these may creep
/// back into ROUTES, and each must render the not-found page with its
/// one door, the Department Map: router.test.ts pins the route and the
/// door, and it-department-map-stations.mocked.spec.ts opens each one.
export const RETIRED_ROUTES: ReadonlyArray<string> = [
  '/it/yard', '/it/yard/dock', '/it/yard/gates', '/it/yard/track', '/it/yard/shed',
  '/it/yard/arrivals', '/it/yard/garage', '/it/yard/publish', '/it/yard/shop-floor',
  '/it/yard/receiving', '/it/yard/marshalling', '/it/crew',
  '/it/operate/receiving', '/it/operate/marshalling', '/it/operate/yard-status',
  '/it/operate/conductor', '/it/design/feedback', '/it/design/backlog', '/it/design/codebase',
  // The department floor (backlog 858e62aa, 2026-11-08): the recursive
  // station view (/tree since backlog 92bfcb04) is the surface it was a
  // flat copy of.
  '/it/floor/hosting',
  // THE /it/tree SPELLINGS (backlog 92bfcb04, car 3 of design df0ecd8b):
  // the floor, a station's page and its Resources view are the root's
  // pages at /tree, /tree/<name> and /tree/<name>/resources, with no
  // alias (no shims before 1.0.0).
  '/it/tree', '/it/tree/engineering', '/it/tree/engineering/resources',
  // STEP 1 OF DESIGN 01fa5bdf (David, 2026-11-17: "IT disappears, the
  // old map is pulled out, and the /ux business pages are removed").
  // Each answered until then and is not found now; a page comes back as
  // a station's page when a protocol runs there. (/it/registry/policy is
  // not here: /it/registry/<kind> is a workflow's page, so it now asks
  // the registry for a workflow named `policy`.)
  '/it/operate', '/it/operate/atlas', '/it/operate/bottlenecks', '/it/design/experiments',
  '/people/hr',
  '/ux/accounts', '/ux/accounts/acct-1', '/ux/assets', '/ux/assets/ast-1',
  '/ux/catalog', '/ux/catalog/FV-30', '/ux/exec', '/ux/marketing-assets', '/ux/marketing-assets/ma-1',
  '/ux/parts', '/ux/parts/part-1', '/ux/people', '/ux/people/emp-001', '/ux/products', '/ux/products/sku-1',
  '/ux/qa', '/ux/sales', '/ux/sales/job-1', '/ux/service', '/ux/service/job-1', '/ux/service/schedule',
  '/ux/shipping', '/ux/shipments/ship-1', '/ux/shop', '/ux/shop/sku-1', '/ux/support', '/ux/warehouse',
  // The jobs list, which the first build of that car kept (review
  // df7aeb9c). A packet's own page, /ux/jobs/<id>, and its step pages
  // stay: src/jobs/the-jobs-list-is-retired.test.ts holds both halves.
  '/ux/jobs',
];

/// Routes the crawls cannot cover yet, each with why. Shrinking this
/// list is the work; adding to it is a decision.
///
/// It lives beside ROUTES for the same reason ROUTES lives here: two
/// specs walk the catalog (route-smoke.mocked.spec.ts and
/// interaction-crawl.mocked.spec.ts, since f2b8a01c) and a route
/// deferred in one and not the other would be crash-checked and never
/// interaction-checked, or the reverse, with nothing to say so. One
/// definition cannot disagree with itself (CLAUDE.md §9a).
export const DEFERRED: ReadonlyMap<string, string> = new Map([
  // '/operations/audit' left on 2026-09-23 (page audit 65a273d5, gap
  // 0398c4d0): the fixture it waited for is EVENTS_STATS in
  // _smokeMocks.ts, and audit-log-page.mocked.spec.ts pins the page.
  // '/ux/finance' left on 2026-09-25 (page audit 3f964c57, backlog
  // e0732f75): its statements' object-shaped fixtures are in
  // _smokeMocks.ts (COMMERCE_SUMMARY, AP_AGING, LEDGER_STATEMENTS), and
  // finance-page.mocked.spec.ts pins the page's own controls.
  // '/ux/warehouse' was deferred here until its page went with step 1 of
  // design 01fa5bdf.
  // '/ux/exec' left on 2026-09-27 (page audit a1d62870, backlog
  // 1495cf68): its reads parse their shapes, and exec-page.mocked.spec.ts
  // pins the page's own lines and links.
  // '/system/os-map' deferral dropped: the page retired with the
  // pre-network framing and its catalog entry is gone.
]);

/// The ONE class a surface puts on a line that says "this read failed".
///
/// It has to be one name, and this is why: on 2026-09-11 a census of
/// every failure line in apps/web found `load-failed` on 37 elements and
/// roughly thirty-five OTHER bespoke names beside it — `.estate-fail`,
/// `.my-fail`, `.ys-fail`, `.crew-fail`, `.tb-err` among them. Four of
/// those pages render a failed read perfectly honestly and NOTHING could
/// assert that they do, because there was no name to assert on. A
/// vocabulary of thirty-five synonyms is not a marker; it is the reason
/// the false-empty class had to be found four times by hand.
///
/// So: one name, asserted by outage-crawl.mocked.spec.ts. Per-page
/// styling keeps its own class alongside — `class="estate-fail
/// load-failed"` — because the crawl reads the shared name and the
/// stylesheet reads the local one. A new surface that invents a
/// thirty-sixth synonym and no `load-failed` does not get a lint; it
/// fails the outage crawl, which is the same mechanism that catches a
/// surface with no failure line at all.
export const FAILURE_MARKER = '.load-failed';
