// THE TWO NAVIGATION SURFACES, EACH WITH ONE JOB (backlog 474e2c1c,
// decided by David 2026-11-19; design 01fa5bdf, the visual surfaces are
// the station tree).
//
//  1. THE TOP BAR MOVES BETWEEN STATIONS. "Home" first — the signed-in
//     person's own station, /me ("Home is the user-specific web UX. The
//     company root is just at /") — then the path through the station
//     tree from the company root to the station being viewed, each
//     segment a link back to that ancestor. When the path does not fit (a
//     phone, or a deep station) the root and the current station stay and
//     the middle folds into one control that lists it in order ("we may
//     need to collapse the middle on Phones or if we get too deep").
//
//  2. THE SIDEBAR IS WHAT THIS STATION HOSTS: the catalog entries whose
//     `host` is the station being viewed, nothing else. It was the
//     department's rows — whatever entries a departments-registry code
//     owned, then a Jobs row — and the top bar was one tab per row of
//     that registry, so both drew departments the company no longer has
//     (David 2026-11-20: "A lot of the options are stale departments
//     rather than the new stations").
//
// BOTH READ THE ONE TREE READ THE ROUTER READS (stationAddresses.svelte.ts)
// and every link is the address `pagePath`/the prefix derives, so a menu
// cannot disagree with a route, and a re-parented station moves its
// trail and its links on the next load with no edit here.
//
// AN UNREAD TREE IS SAID, NEVER DRAWN EMPTY (builder rule 21.2): until it
// answers the trail is `loading`, and a failed read names why. A station
// the route names and the tree does not hold is said too — never drawn as
// the root, which would be a plausible wrong answer.
//
// Pure functions of a route and the tree, pinned in station-trail.test.ts;
// StationTrail.svelte and AppShell.svelte own the strokes.

import type { Route } from '../router';
import { stationHref } from '../it/tree/station-page';
import { ROUTE_CATALOG, type NavItem } from './nav-catalog';
import type { StationAddresses, StationAt } from './page-address';
import { sectionForRoute } from './sections';

/** What a page views: the person's own station (Home), a station of the
 *  company tree by id, the company root (a record view, which belongs to
 *  no one station — design deba3000), or a station the route names that
 *  the tree has not placed (not read yet, or not there). */
export type Viewing =
  | Readonly<{ kind: 'home' }>
  | Readonly<{ kind: 'root' }>
  | Readonly<{ kind: 'station'; id: string }>
  | Readonly<{ kind: 'unplaced'; name: string }>;

/** The route kinds that are the person's own station's pages (backlog
 *  5cb56bb3). `/` renders My Day in place and is one of them until it
 *  renders the company root. */
const PERSONAL_KINDS: ReadonlySet<Route['kind']> = new Set(['me', 'inbox', 'myCalendar']);

/** What `route` views, in the tree as read. */
export function viewedBy(route: Route, at: StationAddresses): Viewing {
  if (PERSONAL_KINDS.has(route.kind)) return { kind: 'home' };
  const named =
    route.kind === 'systemStation' || route.kind === 'systemStationResources'
      ? route.station
      : route.kind === 'systemTree'
        ? (route.under ?? null)
        : null;
  if (named !== null) {
    const s = at.kind === 'ready' ? at.stations.find((x) => x.name === named || x.id === named) : undefined;
    return s === undefined ? { kind: 'unplaced', name: named } : { kind: 'station', id: s.id };
  }
  // A department's jobs view stands at the station of the same name when
  // the tree has one; a page the catalog hosts, at its host.
  if (route.kind === 'department') {
    const s = at.kind === 'ready' ? at.stations.find((x) => x.prefix === `/${route.code}`) : undefined;
    return s === undefined ? { kind: 'root' } : { kind: 'station', id: s.id };
  }
  const entry = (ROUTE_CATALOG as Record<string, NavItem | undefined>)[sectionForRoute(route)];
  return entry?.host !== undefined ? { kind: 'station', id: entry.host } : { kind: 'root' };
}

/** A station's short name for a bar: its title's own name (the words
 *  before the dash, "Engineering — makes the change"), else its name. */
export function stationLabel(s: Readonly<{ name: string; title: string }>): string {
  const head = s.title.split(' — ')[0]?.trim() ?? '';
  return head === '' ? s.name : head;
}

/** One segment of the trail. */
export type TrailStop = Readonly<{
  id: string;
  name: string;
  label: string;
  /** The whole title, the segment's hint. */
  title: string;
  href: string;
  current: boolean;
}>;

/** The trail as the bar draws it. `home`: Home is the page being viewed,
 *  and the stops are the root alone, one link away. */
export type Trail =
  | Readonly<{ kind: 'loading' }>
  | Readonly<{ kind: 'failed'; why: string }>
  | Readonly<{ kind: 'ready'; home: boolean; stops: ReadonlyArray<TrailStop> }>;

/** Where a station's link goes: the address the router places it at — its
 *  prefix, the derivation every route uses. The root's own address is /,
 *  which still renders the person's day in place (router.ts), so its link
 *  is the root's station page by name, the address the router gives that
 *  page today; when / renders the root, this is the prefix too. */
function hrefOf(s: StationAt): string {
  return s.prefix === '' ? stationHref(s.name) : s.prefix;
}

const rootOf = (stations: ReadonlyArray<StationAt>): StationAt | undefined => stations.find((s) => s.parent === null);

/** The parent chain, root first; bounded by the read's size, so a
 *  malformed cycle cannot hang it. */
function chainOf(stations: ReadonlyArray<StationAt>, id: string): ReadonlyArray<StationAt> {
  const chain: StationAt[] = [];
  let at = stations.find((s) => s.id === id);
  for (let i = 0; at !== undefined && i <= stations.length; i += 1) {
    chain.unshift(at);
    const parent: string | null = at.parent;
    at = parent === null ? undefined : stations.find((s) => s.id === parent);
  }
  return chain;
}

/** The trail a view draws: the root to the station being viewed. */
export function trailOf(v: Viewing, at: StationAddresses): Trail {
  if (at.kind !== 'ready') return at.kind === 'loading' ? { kind: 'loading' } : { kind: 'failed', why: at.why };
  if (v.kind === 'unplaced') return { kind: 'failed', why: `the station tree holds no station named \`${v.name}\`` };
  const root = rootOf(at.stations);
  if (root === undefined) return { kind: 'failed', why: 'the station tree read holds no root' };
  const stop = (s: StationAt, current: boolean): TrailStop => ({
    id: s.id,
    name: s.name,
    label: stationLabel(s),
    title: s.title,
    href: hrefOf(s),
    current,
  });
  if (v.kind === 'home') return { kind: 'ready', home: true, stops: [stop(root, false)] };
  const id = v.kind === 'root' ? root.id : v.id;
  const chain = chainOf(at.stations, id);
  if (chain.length === 0) return { kind: 'failed', why: `the station tree holds no station \`${id}\`` };
  return { kind: 'ready', home: false, stops: chain.map((s, i) => stop(s, i === chain.length - 1)) };
}

/** The longest path the bar draws whole on a wide screen: the root and
 *  three stations below it. The shipped tree is three deep at most. */
export const MAX_FULL_TRAIL = 4;

/** The trail split for drawing: `head` and `tail` on the bar, `hidden`
 *  behind the one expander between them, in order. Collapsed, the root
 *  and the current station stay and every station between them folds
 *  (David 2026-11-19, `decided_by_david_20261119_collapse`). */
export type TrailView = Readonly<{
  head: ReadonlyArray<TrailStop>;
  hidden: ReadonlyArray<TrailStop>;
  tail: ReadonlyArray<TrailStop>;
}>;

export function trailView(stops: ReadonlyArray<TrailStop>, opts: Readonly<{ narrow: boolean }>): TrailView {
  const fold = stops.length > 2 && (opts.narrow || stops.length > MAX_FULL_TRAIL);
  return fold
    ? { head: stops.slice(0, 1), hidden: stops.slice(1, -1), tail: stops.slice(-1) }
    : { head: stops, hidden: [], tail: [] };
}

/** The sidebar a view draws: the entries it hosts, in catalog order, under
 *  the station's own label ('' for Home, whose rows need no heading). */
export type Sidebar =
  | Readonly<{ kind: 'loading' }>
  | Readonly<{ kind: 'failed'; why: string }>
  | Readonly<{ kind: 'ready'; label: string; items: ReadonlyArray<NavItem> }>;

const CATALOG: ReadonlyArray<NavItem> = Object.values(ROUTE_CATALOG);

/** Home's rows are the person's own pages, which no tree places, so they
 *  never wait on the tree. A station's need the tree for its label and
 *  its links, and say so while it is unread. Unlisted entries (a tab on a
 *  page, a door) are reachable and never a row, as they always were. */
export function sidebarOf(v: Viewing, at: StationAddresses): Sidebar {
  const listed = CATALOG.filter((e) => e.unlisted !== true);
  if (v.kind === 'home') return { kind: 'ready', label: '', items: listed.filter((e) => e.person === true) };
  const t = trailOf(v, at);
  if (t.kind !== 'ready') return t;
  const here = t.stops[t.stops.length - 1]!;
  return { kind: 'ready', label: here.label, items: listed.filter((e) => e.host === here.id) };
}
