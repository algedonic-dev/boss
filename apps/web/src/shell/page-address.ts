// A PAGE'S ADDRESS IS ITS HOST STATION'S, PLUS ITS SLUG (backlog
// 0bc2eb68; the routes decision of design deba3000, David 2026-11-10:
// "We need to move away from /ux which isn't a station").
//
// The nav catalog names each page's host station by ID and the page's
// slug below it (nav-catalog.ts `host` and `slug`), and never a prefix.
// The prefix is the station's address in the tree, which the SERVER
// derives from the parent chain on every read (GET /api/stations?under=,
// crates/core/boss-jobs/src/station_tree.rs `subtree`) and nothing
// stores — so a re-parented station moves every page it hosts, with no
// edit here or in the catalog. The company root's own segment is
// dropped, so the root's pages sit at the top level (/alarms) and a
// department's under its name (/finance/vendors, /engineering/registry,
// /operations/estate).
//
// THIS MODULE IS PURE: it holds no tree, it is handed one. The SPA's
// one read of it is stationAddresses.svelte.ts; the router and every
// link take the result as an argument, so `bun test` pins the whole
// derivation without a server — and re-parenting is a fixture edit.
//
// AN UNREAD TREE IS SAID, NEVER GUESSED. Until the read answers, a
// hosted page has no address (`pagePath` is null) and a path that only
// the tree could resolve is `resolving`; a read that failed names why.
// The alternative — falling back to a typed prefix — is the hand-typed
// path this module exists to retire.

import type { StationTree } from '../it/tree/station-tree';
import type { NavItem } from './nav-catalog';

/** One station of the tree as a page address needs it. */
export type StationAt = Readonly<{
  id: string;
  name: string;
  /** Its address with the root's own segment dropped: '' for the root,
   *  `/engineering` for a department, `/engineering/design` below it. */
  prefix: string;
}>;

/** The tree as the pages read it: not answered yet, failed (and why),
 *  or every station's prefix. */
export type StationAddresses =
  | Readonly<{ kind: 'loading' }>
  | Readonly<{ kind: 'failed'; why: string }>
  | Readonly<{ kind: 'ready'; stations: ReadonlyArray<StationAt> }>;

export const ADDRESSES_LOADING: StationAddresses = { kind: 'loading' };

/** The prefixes of a tree read under the ROOT. A read under any other
 *  station is refused rather than read as the company: its first row's
 *  address would be dropped as if it were the root's, and every page
 *  would answer one level too high. */
export function addressesOf(tree: StationTree): StationAddresses {
  const root = tree.nodes[0];
  if (root === undefined || root.parent !== null) {
    return { kind: 'failed', why: `the tree read starts at \`${root?.name ?? 'nothing'}\`, not at the root` };
  }
  const stations = tree.nodes.flatMap((n): StationAt[] =>
    n.id === null ? [] : [{ id: n.id, name: n.name, prefix: n.address.slice(root.address.length) }],
  );
  return { kind: 'ready', stations };
}

/** A station's prefix by id, or null when the tree is not read or does
 *  not hold it. */
export function stationPrefix(at: StationAddresses, id: string): string | null {
  return at.kind === 'ready' ? (at.stations.find((s) => s.id === id)?.prefix ?? null) : null;
}

/** The station whose prefix is exactly `path`, if the tree is read. */
export function stationAtPath(at: StationAddresses, path: string): StationAt | null {
  return at.kind === 'ready' ? (at.stations.find((s) => s.prefix !== '' && s.prefix === path) ?? null) : null;
}

/** Where a catalog entry is: a hosted page at its host's prefix plus its
 *  slug (a `:param` segment left as the pattern), an unhosted one at the
 *  path it still spells, a person's page at its /me path (backlog
 *  5cb56bb3: the signed-in person's station, which no tree places), and
 *  null for a hosted page whose host the tree has not answered — never a
 *  guess. */
export function pagePath(entry: NavItem, at: StationAddresses): string | null {
  if (entry.host === undefined) return entry.path;
  const prefix = stationPrefix(at, entry.host);
  if (prefix === null) return null;
  return entry.slug === '' ? prefix || '/' : `${prefix}/${entry.slug}`;
}

/** A pattern with its `:name` segments filled, each value encoded as
 *  one segment; null when a value it names is missing. */
export function fillPath(pattern: string, params: Readonly<Record<string, string>>): string | null {
  const parts = pattern.split('/').map((seg) => {
    if (!seg.startsWith(':')) return seg;
    const v = params[seg.slice(1)];
    return v === undefined ? null : encodeURIComponent(v);
  });
  return parts.includes(null) ? null : parts.join('/');
}

/** The values a path holds for a pattern's `:name` segments (one decoded
 *  segment each) and a trailing `*name` (the rest, at least one segment,
 *  decoded whole), or null when it does not match. */
export function matchPath(pattern: string, path: string): Readonly<Record<string, string>> | null {
  const want = pattern.split('/');
  const got = path.split('/');
  const out: Record<string, string> = {};
  for (let i = 0; i < want.length; i += 1) {
    const w = want[i]!;
    if (w.startsWith('*')) {
      const rest = got.slice(i).join('/');
      if (rest === '') return null;
      out[w.slice(1)] = decode(rest);
      return out;
    }
    const g = got[i];
    if (g === undefined) return null;
    if (w.startsWith(':')) {
      if (g === '') return null;
      out[w.slice(1)] = decode(g);
    } else if (w !== g) {
      return null;
    }
  }
  return got.length === want.length ? out : null;
}

/** A segment decoded, or as typed when its escape is malformed — a
 *  mistyped URL renders a page instead of throwing out of the router. */
function decode(s: string): string {
  try {
    return decodeURIComponent(s);
  } catch {
    return s;
  }
}
