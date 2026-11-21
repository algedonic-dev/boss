// Surface opens — the SPA tells the system which surface it opened
// (backlog 628f182b).
//
// David, 2026-09-16, running the company as one human plus agents: "I
// am mostly concerned about my personal HCI … the UI and good
// transparency into underlying state has been really important for
// driving reliability and trust." Then: "let's measure which surfaces I
// open for a week." Measured that day: nothing recorded a route open
// per actor. The SPA routes client-side, so only hard loads reach the
// gateway at all, and those are login redirects, not a record; the
// audit log carries no per-request events by decision. This module is
// the record's client half: one POST per client-side navigation to
// `/api/surface-opens`, carrying the route PATTERN and the time and
// nothing else. The actor is the session's, signed by the gateway —
// the body never names one, so it cannot lie about one.
//
// THE PATTERN, NEVER THE ID. `/ux/jobs/:jobId`, not the uuid: the
// question is which SURFACES get opened, and a per-id row would count
// a busy day on one packet as a hundred surfaces. The pattern is the
// pathname with every id-valued field of the parsed Route replaced by
// its field name, so the vocabulary is the router's own and a new
// route's id becomes a placeholder without this file learning it.
// Query parameters (a jobs-list filter, a search) are not part of the
// pattern: they are how a surface was opened, not which.
//
// ONCE PER NAVIGATION, DEBOUNCED AGAINST THE SAME PATTERN. A popstate
// that re-parses to the same surface (an in-page filter change, a
// detail page opened for a second id) is the same open, and is not
// counted twice. Navigating away and back is two opens.
//
// FAILURES ARE SILENT TO THE OPERATOR. A fetch that fails changes
// nothing on the page: the count is a measurement, and a measurement
// must never get in the way of the thing it measures.

import { routable, type Route } from '../router';
import { ROUTE_CATALOG, departmentLanding } from './nav-catalog';
import { pagePath, type StationAddresses } from './page-address';

/// Route fields the router reads off the QUERY STRING, not the path.
/// They are how a surface was opened (a list filter, a search, where
/// Back goes) and never appear in the pathname — and their values are
/// free text, so `fromLabel: 'jobs'` must not turn `/ux/jobs/…` into
/// `/ux/:fromLabel/…`. Pinned against router.ts by surface-opens.test.ts:
/// a new `sp.get(…)` field that is not listed here fails the test.
// The jobs list's filter and new-job fields left with that list (step 1
// of design 01fa5bdf).
export const QUERY_FIELDS: ReadonlySet<string> = new Set([
  'q',
  'from',
  'fromLabel',
  // The Department Map's selection (design e765b3fc, car N1): a station
  // name, so `/alarms?at=gates` is the page /alarms, never `/alarms/:at`.
  'at',
]);

/// A route's id-valued fields, longest value first so a value that is a
/// prefix of another is replaced after the longer one.
function idFields(route: Route): ReadonlyArray<readonly [string, string]> {
  return Object.entries(route)
    .filter(
      (e): e is [string, string] =>
        e[0] !== 'kind' && !QUERY_FIELDS.has(e[0]) && typeof e[1] === 'string' && e[1] !== '',
    )
    .sort(([, a], [, b]) => b.length - a.length);
}

/// Decode one path segment; a malformed escape stays as it was rather
/// than throwing on a URL somebody typed.
function decodeSegment(s: string): string {
  try {
    return decodeURIComponent(s);
  } catch {
    return s;
  }
}

/// The route pattern for a parsed Route at a pathname: the pathname
/// (minus the /dashboard mount and a trailing slash, as the router
/// reads it) with each id-valued field's value replaced by `:field`.
/// `/ux/jobs/abc-123` for `{kind:'jobDetail', jobId:'abc-123'}` is
/// `/ux/jobs/:jobId`; `/it/codebase` stays `/it/codebase`.
export function routePattern(route: Route, pathname: string): string {
  const raw = pathname.replace(/^\/dashboard/, '').replace(/\/$/, '') || '/';
  const decoded = raw.split('/').map(decodeSegment).join('/');
  return idFields(route).reduce((path, [field, value]) => replaceSegment(path, value, field), decoded);
}

/// The pattern an open is RECORDED under: the route pattern, except where
/// a department's own address (`/<code>`) renders its landing page in
/// place — then the landing page's own address, because that is the
/// surface that was opened. The IT tab links to /it and /it renders the
/// alarms board, whose catalog address is /alarms since backlog 0bc2eb68;
/// recorded as /it, every tab open read as no open of the board, and the
/// daily never-opened list (infra/surface-usage.sh, catalog minus opened)
/// named the second most-opened surface a deletion candidate (review
/// 7379181c F1: 99 opens of /it in 7 days). Null while the landing page
/// has no address yet (the tree unread): the open is recorded once it
/// has one, never under a guess.
///
/// My Day is the same case: `/` and `/ux` render it in place, and its
/// own address is the person's station, /me (backlog 5cb56bb3), so every
/// open of it is recorded as /me — the catalog path the never-opened
/// list subtracts from. That address needs no tree.
export function openedPattern(route: Route, pathname: string, at: StationAddresses): string | null {
  if (route.kind === 'me') return pagePath(ROUTE_CATALOG.me, at);
  const raw = routable(pathname);
  const code = raw.match(/^\/([^/]+)$/)?.[1];
  const landing = code === undefined ? undefined : departmentLanding(decodeSegment(code));
  if (landing === undefined) return routePattern(route, pathname);
  const own = pagePath(landing, at);
  if (own === null) return null;
  return own === raw ? routePattern(route, pathname) : routePattern(route, own);
}

/// `path` with the LAST segment-bounded occurrence of `/value` replaced
/// by `/:field`. Segment-bounded: `/` + value followed by `/` or the
/// end, so `invoiceId: 'invoice'` rewrites `/finance/invoices/invoice`
/// to `/finance/invoices/:invoiceId` and leaves the `invoices` segment
/// alone; last,
/// because the router puts ids at the tail and a static segment that
/// merely starts with the id (`/jobs` for `jobId: 'j'`) sits earlier.
function replaceSegment(path: string, value: string, field: string): string {
  const needle = `/${value}`;
  for (let at = path.lastIndexOf(needle); at !== -1; at = at === 0 ? -1 : path.lastIndexOf(needle, at - 1)) {
    const after = path.charAt(at + needle.length);
    if (after === '' || after === '/') {
      return `${path.slice(0, at)}/:${field}${path.slice(at + needle.length)}`;
    }
  }
  return path;
}

/// What the recorder posts. `at` is the client's clock: when the
/// operator opened it, as the operator's machine saw it.
export type SurfaceOpen = Readonly<{ route: string; at: string }>;

/// A recorder: call it with the current pattern on every navigation;
/// it posts once per change of pattern and swallows every failure.
/// `post` is injected so the debounce is testable without a network.
export function makeSurfaceOpenRecorder(
  post: (open: SurfaceOpen) => Promise<unknown>,
  now: () => Date = () => new Date(),
): (pattern: string) => void {
  let last: string | null = null;
  return (pattern: string): void => {
    if (pattern === last) return;
    last = pattern;
    void post({ route: pattern, at: now().toISOString() }).catch(() => {
      // A failed record changes nothing the operator sees.
    });
  };
}

/// The one live transport: a fire-and-forget POST through the gateway,
/// which signs the session's actor onto it. `keepalive` so a navigation
/// that unloads the document (a full-page link) still delivers.
export function postSurfaceOpen(open: SurfaceOpen): Promise<unknown> {
  return fetch('/api/surface-opens', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(open),
    keepalive: true,
  });
}
