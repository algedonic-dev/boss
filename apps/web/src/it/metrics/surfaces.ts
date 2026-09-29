// Surface usage — which surfaces each operator opened, read off the
// `surface_opens` roll-up (backlog 628f182b).
//
// David, 2026-09-16: "let's measure which surfaces I open for a week."
// The recording half is apps/web/src/shell/surface-opens.ts (one POST
// per navigation, the pattern and the time, the actor signed by the
// gateway); the daily chore (infra/surface-usage.sh) files a 24-hour
// roll-up onto a `maintenance-surface-usage` packet. This is the
// reading half: the last seven days per actor, opens per route
// sorted, distinct routes, and the catalogued surfaces NOBODY opened —
// the deletion candidates.
//
// ONE READ, SHARED WITH THE CHORE. `GET /api/surface-opens/rollup`
// does the GROUP BY server-side, so the page and the packet cannot
// disagree by summing differently; everything here is a pure function
// of the rows it returns. The never-opened list compares those rows
// against the nav catalog's paths — the catalog is the roster, so a
// path it lists that no row names is a surface with no reader in the
// window. The roster is NOT every route the router serves, and the
// section says so and lists the opened routes outside it
// (`uncatalogued`, 13ded76c).
//
// EVERY NUMBER IS A FUNCTION OF THE ROWS. A failed read is a failure
// (rendered as such, never as "nobody opened anything"); an empty
// roll-up is an empty week, which after seven days of recording is a
// real finding and before that is stated as "nothing recorded yet".

import { fetchRemote, type Remote } from '../../data/remote';
import { ROUTE_CATALOG } from '../../shell/nav-catalog';
import { envelopeShape } from './trend';

export type RouteCount = Readonly<{ actor_id: string; route: string; opens: number; last_at: string }>;

export type Rollup = Readonly<{
  since: string | null;
  until: string | null;
  rows: ReadonlyArray<RouteCount>;
}>;

const rec = (v: unknown): Record<string, unknown> | null =>
  v !== null && typeof v === 'object' && !Array.isArray(v) ? (v as Record<string, unknown>) : null;
const str = (v: unknown): string | null => (typeof v === 'string' && v !== '' ? v : null);
const num = (v: unknown): number | null => (typeof v === 'number' && Number.isFinite(v) ? v : null);

function parseRow(v: unknown): RouteCount | null {
  const r = rec(v);
  const actor_id = r ? str(r.actor_id) : null;
  const route = r ? str(r.route) : null;
  const opens = r ? num(r.opens) : null;
  if (!r || !actor_id || !route || opens === null) return null;
  return { actor_id, route, opens, last_at: str(r.last_at) ?? '' };
}

/// The roll-up envelope `{since, until, rows}`. A bare array (a mock's
/// catch-all) is its rows with no window stated — `[]` an empty
/// roll-up. Anything else THROWS (b64b3c04, page audit f82b05a9): an
/// object without `rows` used to read as zero rows, which the section
/// draws as "No surface open is recorded", and fetchRemote turns the
/// throw into the failed arm instead.
export function parseRollup(raw: unknown): Rollup {
  const env = rec(raw);
  if (!Array.isArray(raw) && !(env && Array.isArray(env.rows))) {
    throw new Error(`unrecognised surface roll-up envelope: expected {rows: [...]} or a list, got ${envelopeShape(raw)}`);
  }
  const list: unknown[] = Array.isArray(raw) ? raw : (env?.rows as unknown[]);
  return {
    since: env ? str(env.since) : null,
    until: env ? str(env.until) : null,
    rows: list.flatMap((r) => parseRow(r) ?? []),
  };
}

/// The window's two edges for the last `days` days ending at `now`.
export function windowFor(days: number, now: Date): Readonly<{ since: string; until: string }> {
  return { since: new Date(now.getTime() - days * 86_400_000).toISOString(), until: now.toISOString() };
}

export function loadSurfaceRollup(
  days: number,
  now: Date = new Date(),
): Promise<Exclude<Remote<Rollup>, { kind: 'loading' }>> {
  const w = windowFor(days, now);
  return fetchRemote(
    `/api/surface-opens/rollup?since=${encodeURIComponent(w.since)}&until=${encodeURIComponent(w.until)}`,
    parseRollup,
  );
}

export type ActorUsage = Readonly<{
  actor_id: string;
  opens: number;
  distinct: number;
  /** Most-opened first, then by route so equal counts are stable. */
  routes: ReadonlyArray<Readonly<{ route: string; opens: number; last_at: string }>>;
}>;

/// Rows folded per actor: total opens, distinct routes, and the routes
/// sorted most-opened first. Actors are ordered by opens, busiest first.
export function perActor(rows: ReadonlyArray<RouteCount>): ReadonlyArray<ActorUsage> {
  const byActor = rows.reduce<Map<string, RouteCount[]>>((m, r) => {
    m.set(r.actor_id, [...(m.get(r.actor_id) ?? []), r]);
    return m;
  }, new Map());
  return [...byActor.entries()]
    .map(([actor_id, rs]) => ({
      actor_id,
      opens: rs.reduce((s, r) => s + r.opens, 0),
      distinct: new Set(rs.map((r) => r.route)).size,
      routes: [...rs]
        .sort((a, b) => b.opens - a.opens || (a.route < b.route ? -1 : a.route > b.route ? 1 : 0))
        .map((r) => ({ route: r.route, opens: r.opens, last_at: r.last_at })),
    }))
    .sort((a, b) => b.opens - a.opens || (a.actor_id < b.actor_id ? -1 : a.actor_id > b.actor_id ? 1 : 0));
}

/// Every path the nav catalog registers, once each, in catalog order.
/// A parameterised path (`/it/registry/rules/:ruleName`) is compared as
/// the pattern surface-opens records, which is the same spelling.
export function catalogPaths(): ReadonlyArray<string> {
  return [...new Set(Object.values(ROUTE_CATALOG).map((e) => e.path))];
}

/// The catalogued surfaces no row names — nobody opened them in the
/// window. `paths` is injectable so the fold is testable against a
/// small roster; the page passes `catalogPaths()`.
export function neverOpened(rows: ReadonlyArray<RouteCount>, paths: ReadonlyArray<string>): ReadonlyArray<string> {
  const opened = new Set(rows.map((r) => r.route));
  return paths.filter((p) => !opened.has(p));
}

/// The routes some row names that the roster does not hold, once each,
/// sorted — surfaces in use that `neverOpened` cannot see, because its
/// roster is the nav catalog and the router serves more than the
/// catalog lists (13ded76c part a; measured at the audit, 20 of the 45
/// patterns opened in a week). Comparing against every pattern the
/// router serves needs the router to export its pattern table, which is
/// its own car (13ded76c part b), so the page states this scope instead.
export function uncatalogued(rows: ReadonlyArray<RouteCount>, paths: ReadonlyArray<string>): ReadonlyArray<string> {
  const roster = new Set(paths);
  return [...new Set(rows.map((r) => r.route))].filter((r) => !roster.has(r)).sort();
}
