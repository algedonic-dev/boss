// The SPA's one read of the station tree for page addresses (backlog
// 0bc2eb68): GET /api/stations for the root (the one row naming no
// parent — code never names an address, station-tree.md §8 rule 1), then
// the tree read under it, whose rows carry the address the SERVER
// derived from the parent chain. ./page-address turns that into each
// station's prefix; the router and every link to a hosted page read it
// from here, so a re-parented station moves its pages on the next load.
//
// The same two reads the station floor makes (it/tree/TreeFloor.svelte),
// through the same parsers, rather than a second ancestry walk over the
// flat rows: the address has one author, and it is the server.
//
// Read once per load. A failed read is NOT remembered as an answer: it
// is the failed arm, naming why, and the router says so on a page that
// only the tree could have found (never a guessed prefix).

import { href } from '@boss/web-kit/nav';
import { parseTree, rootIdOf } from '../it/tree/station-tree';
import { ROUTE_CATALOG, type NavItem } from './nav-catalog';
import { ADDRESSES_LOADING, addressesOf, fillPath, pagePath, type StationAddresses } from './page-address';

// Reassigned, never mutated in place, so `$derived` readers re-run.
const tree = $state<{ value: StationAddresses }>({ value: ADDRESSES_LOADING });

// Non-reactive dedupe guard, as departments.svelte.ts keeps its own.
let requested = false;

/** What a read of the tree answers: its addresses, or why not. Pure of
 *  this module's state, so its arms are tested without a component. */
export async function readStationAddresses(
  get: (path: string) => Promise<Response> = (p) => fetch(p),
): Promise<StationAddresses> {
  try {
    const list = await get('/api/stations');
    if (!list.ok) return { kind: 'failed', why: `GET /api/stations answered ${list.status}` };
    const root = rootIdOf(await list.json());
    if (root === null) return { kind: 'failed', why: 'the station rows name no single root' };
    const r = await get(`/api/stations?under=${encodeURIComponent(root)}`);
    if (!r.ok) return { kind: 'failed', why: `the tree read under the root answered ${r.status}` };
    return addressesOf(parseTree(await r.json()));
  } catch (e) {
    return { kind: 'failed', why: e instanceof Error ? e.message : String(e) };
  }
}

/** Load (once) the tree's addresses. Idempotent; a failure may retry. */
export async function loadStationAddresses(): Promise<void> {
  if (requested) return;
  requested = true;
  const answer = await readStationAddresses();
  if (answer.kind === 'failed') requested = false;
  tree.value = answer;
}

/** The tree as the pages read it — loading, failed, or ready. */
export function stationAddresses(): StationAddresses {
  return tree.value;
}

type CatalogId = keyof typeof ROUTE_CATALOG;

/** A catalogued page's path — its parameters filled and `rest` (a
 *  sub-page or a query) appended — or null while the tree has not
 *  answered. A hosted page renders only after the router found it
 *  through a read tree, so inside one this always has an answer. */
export function pageHref(id: CatalogId, params: Readonly<Record<string, string>> = {}, rest = ''): string | null {
  const base = pagePath(ROUTE_CATALOG[id] as NavItem, tree.value);
  const filled = base === null ? null : fillPath(base, params);
  return filled === null ? null : `${filled === '/' && rest !== '' ? '' : filled}${rest}`;
}

/** `pageHref` as a link target for a page's own markup, the /dashboard
 *  mount honoured: '' while the tree has not answered, which the shared
 *  Link and `safeLinkHref` draw as text, never as a link to a guess. */
export function pageLink(id: CatalogId, params: Readonly<Record<string, string>> = {}, rest = ''): string {
  const p = pageHref(id, params, rest);
  return p === null ? '' : href(p);
}
