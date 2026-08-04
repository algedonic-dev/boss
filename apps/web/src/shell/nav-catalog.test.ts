// The nav catalog is the single source of truth for which app owns a
// surface. These tests hold two lines.
//
// First, the de-duplication that made the app split safe: app
// membership used to live in two sets, in two vocabularies, in two
// files (AppShell's `MODEL_ROUTES: Set<RouteName>` for the sidebar,
// App.svelte's `MODEL_KINDS: Set<Route['kind']>` for the active tab),
// which had to agree for every routed surface or a page rendered under
// the wrong tab with nothing failing. The System Model app's
// membership is still pinned verbatim against the deleted list.
//
// Second, the split itself: every surface belongs to exactly one app,
// every app the tab bar advertises can actually be reached, and no
// surface is stranded in an app with no tab.

import { describe, it, expect } from 'bun:test';
import { APPS, type AppId } from '@boss/web-kit/nav';
import { ROUTE_CATALOG, appForSection, type NavItem } from './nav-catalog';

/// Verbatim copy of AppShell.svelte's deleted `MODEL_ROUTES`. The
/// System Model app is unchanged by the split, so this stays the
/// regression pin — if a future change moves a surface into or out of
/// it, this list is the thing to update, deliberately.
const LEGACY_MODEL_ROUTES: ReadonlyArray<string> = [
  'system-model', 'system-monitoring', 'system-step-plugins', 'system-dispatcher',
  'system-subjects', 'system-dispatcher-rules', 'system-dispatcher-rule',
  'system-kb', 'system-design', 'system-experiments', 'policy', 'job-kinds',
  'workflows', 'auth-admin',
];

const entries = Object.entries(ROUTE_CATALOG) as ReadonlyArray<[string, NavItem]>;

describe('nav catalog — app assignment', () => {
  it('every catalog entry declares an app', () => {
    const missing = entries.filter(([, v]) => v.app === undefined).map(([k]) => k);
    expect(
      missing,
      `these surfaces declare no app and would render under whichever tab ` +
        `they happened to fall back to: ${missing.join(', ')}`,
    ).toEqual([]);
  });

  it('the System Model app still contains exactly what MODEL_ROUTES listed', () => {
    const derived = entries
      .filter(([, v]) => v.app === 'model')
      .map(([k]) => k)
      .sort();
    expect(derived).toEqual([...LEGACY_MODEL_ROUTES].sort());
  });

  it('every surface lands in an app the chrome bar actually offers', () => {
    const tabbed = new Set<AppId>(APPS.map((a) => a.id));
    for (const [name, item] of entries) {
      expect(
        tabbed.has(item.app as AppId),
        `${name} is assigned to app "${item.app}", which has no tab in APPS — ` +
          `it would be unreachable.`,
      ).toBe(true);
    }
  });

  it('no surface is left in the retired catch-all "user" app', () => {
    // `/ux` was one tab holding 24 surfaces. The split exists to end
    // that; a straggler here means a surface nobody re-homed.
    const stragglers = entries
      .filter(([, v]) => (v.app as string) === 'user')
      .map(([k]) => k);
    expect(stragglers).toEqual([]);
  });

  it('every domain app owns at least one surface', () => {
    // A tab that renders an empty sidebar is a dead end. Simulator is
    // exempt: it is a separate SPA with no surfaces in this catalog.
    const owned = new Set(entries.map(([, v]) => v.app));
    for (const app of APPS) {
      if (app.id === 'simulator') continue;
      expect(owned.has(app.id), `app "${app.id}" has a tab but owns no surface`).toBe(true);
    }
  });
});

describe('appForSection — the App.svelte tab derivation', () => {
  it('resolves surfaces to their app', () => {
    expect(appForSection('system-model')).toBe('model');
    expect(appForSection('accounts')).toBe('crm');
    expect(appForSection('finance')).toBe('finance');
    expect(appForSection('jobs')).toBe('operations');
    expect(appForSection('warehouse')).toBe('supply-chain');
    expect(appForSection('people')).toBe('people');
    expect(appForSection('inbox')).toBe('home');
  });

  it('falls back to Home for unknown sections', () => {
    // `me` is App.svelte's terminal fallback in the activeSection
    // ternary and has no catalog entry. Home is where personal
    // surfaces live, so that is the right landing for the fallback.
    expect(appForSection('me')).toBe('home');
    expect(appForSection('definitely-not-a-section')).toBe('home');
  });
});
