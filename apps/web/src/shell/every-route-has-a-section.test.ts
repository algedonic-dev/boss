// Every route kind resolves to a section, or is listed as one that
// deliberately has none.
//
// App.svelte maps `route.kind` to a sidebar section id through a long
// ternary, and `appForSection` turns that into the active app tab. A
// kind missing from the ternary falls through to `'me'`, so the right
// page renders inside the HOME chrome — right content, wrong app, no
// error.
//
// That shipped for 21 of 74 kinds. It was reported as "clicking
// Feedback triage in the IT app took me to the Home app for some
// reason", which was one symptom of twenty-one, and the ternary
// carried a comment claiming it "already resolves every route.kind
// down to" a section. Nothing checked the claim.
//
// This is the third instance in one day of a surface registered in
// some places and not others — the OS map had a route, a permKey and a
// catalog entry but was absent from IT_GROUPS; Flow was absent from
// the route-smoke crawl. The pattern is that registration is spread
// across several lists and none of them knows about the others, so
// each new list needs the equality test rather than the good
// intentions of whoever adds the next surface (CLAUDE.md §9a).

import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';

const router = readFileSync(new URL('../router.ts', import.meta.url), 'utf8');
const app = readFileSync(new URL('../App.svelte', import.meta.url), 'utf8');

/// Kinds with no section by design, each with the reason. The bar for
/// adding one is "this route does not render inside AppShell, or has
/// no sidebar row" — not "I could not find where it goes".
const NO_SECTION: ReadonlyMap<string, string> = new Map([
  ['login', 'renders outside AppShell — full viewport, no chrome'],
  ['stepFocus', 'renders outside AppShell — the full-page step surface'],
  ['home', 'the unauthenticated landing page, outside AppShell'],
  ['search', 'cross-cutting results page with no sidebar row of its own'],
]);

const routeKinds = new Set(
  [...router.matchAll(/\{\s*kind:\s*'([A-Za-z]+)'/g)].map((m) => m[1]!),
);

const chain = app.slice(
  app.indexOf('let activeSection = $derived('),
  app.indexOf(": 'me',\n  );"),
);
const handled = new Set(
  [...chain.matchAll(/route\.kind === '([A-Za-z]+)'/g)].map((m) => m[1]!),
);

describe('every route kind resolves to a section', () => {
  test('no kind silently falls through to the home chrome', () => {
    const unmapped = [...routeKinds]
      .filter((k) => !handled.has(k) && !NO_SECTION.has(k))
      .sort();
    expect(
      unmapped,
      'these route kinds fall through to the `me` section, so their page renders ' +
        'inside the Home app chrome — add an arm to the activeSection ternary in ' +
        'App.svelte, or list the kind in NO_SECTION with the reason it has none',
    ).toEqual([]);
  });

  test('no exemption names a kind that no longer exists', () => {
    // An exemption for a deleted route reads as "handled elsewhere"
    // while covering nothing, and quietly widens the hole the next
    // time a kind is renamed onto it.
    const ghosts = [...NO_SECTION.keys()].filter((k) => !routeKinds.has(k)).sort();
    expect(ghosts).toEqual([]);
  });

  test('the router actually defines routes, so an empty scrape fails loudly', () => {
    // Both sets above are scraped from source. If a refactor changes
    // the shape they match, every other assertion here would pass
    // vacuously against two empty sets.
    expect(routeKinds.size).toBeGreaterThan(50);
    expect(handled.size).toBeGreaterThan(50);
  });
});
