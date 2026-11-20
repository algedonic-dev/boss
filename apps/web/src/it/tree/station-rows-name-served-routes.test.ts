import { describe, expect, test } from 'bun:test';
import { readdirSync, readFileSync } from 'node:fs';
import { parseRoute } from '../../router';
import { ADDRESSES } from '../../../tests/fixtures/station-tree';

// NO STATION ROW NAMES A ROUTE THE ROUTER DOES NOT SERVE (backlog
// a1c0a0ff, design deba3000). Measured on origin/main 2026-11-10: seven
// rows under infra/platform/stations carried `[station.upstream] href =
// "/system/yard"` and the dock carried `/system/feedback`, both under a
// prefix the IT consolidation retired on 2026-08-31, so the walk upstream
// they declared led to a not-found page. Nothing read the rows against the
// router: the row lives in TOML, the router in TypeScript, and the only
// test that named the hrefs read them back from the database. This pin is
// the reading across that seam.
//
// The fix derives a station's upstream from the tree (its parent's page,
// `upstreamOf` in station-page.ts) and the seed refuses a declared one, so
// today no row names a route at all. The pin holds the general rule for
// any route-like value a row may carry later: the router must answer it
// with a page. A one-segment `/<code>` path is a department's address the
// router cannot vouch for (the registry decides it, sections.ts
// `withRoster`), so a row naming one is refused too.

const STATIONS = new URL('../../../../../infra/platform/stations/', import.meta.url);

/** Every quoted value in a row's non-comment lines that reads as an app
 *  route: a string starting with `/` and a letter. */
function routesNamedIn(text: string): ReadonlyArray<string> {
  return text
    .split('\n')
    .filter((line) => !line.trimStart().startsWith('#'))
    .flatMap((line) => [...line.matchAll(/"(\/[A-Za-z][^"]*)"/g)].map((m) => m[1]!));
}

/** The routes `text` names that the router does not serve with a page. */
function unservedRoutes(text: string): ReadonlyArray<string> {
  return routesNamedIn(text).filter((r) => {
    const [path, query = ''] = r.split('?');
    const kind = parseRoute(path!, query === '' ? '' : `?${query}`, ADDRESSES).kind;
    return kind === 'notFound' || kind === 'department';
  });
}

describe('no station row names a route the router does not serve', () => {
  test('the reader catches the shape that shipped (control)', () => {
    const shipped = '[station.upstream]\nlabel = "THE YARD"\nhref = "/system/yard"\n';
    expect(unservedRoutes(shipped)).toEqual(['/system/yard']);
    expect(unservedRoutes('href = "/system/feedback"')).toEqual(['/system/feedback']);
    // A served page passes; a comment is prose, not a link.
    expect(unservedRoutes('href = "/it/tree/engineering"')).toEqual([]);
    expect(unservedRoutes('# the old pointer read "/system/yard"')).toEqual([]);
  });

  test('every row under infra/platform/stations', () => {
    const files = readdirSync(STATIONS).filter((f) => f.endsWith('.toml'));
    expect(files.length).toBeGreaterThan(0);
    const offenders = files.flatMap((f) =>
      unservedRoutes(readFileSync(new URL(f, STATIONS), 'utf8')).map((r) => `${f}: ${r}`),
    );
    expect(offenders).toEqual([]);
  });
});
