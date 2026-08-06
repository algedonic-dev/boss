// The nav catalog is the single source of truth for which app owns a
// surface. These tests hold two lines.
//
// First, the de-duplication that made the app split safe: app
// membership used to live in two sets, in two vocabularies, in two
// files (AppShell's `MODEL_ROUTES: Set<RouteName>` for the sidebar,
// App.svelte's `MODEL_KINDS: Set<Route['kind']>` for the active tab),
// which had to agree for every routed surface or a page rendered under
// the wrong tab with nothing failing. That membership set is still
// pinned verbatim against the deleted list — it now belongs to the IT
// app rather than a top-level System Model tab, but which surfaces
// travel together has not changed.
//
// Second, the split itself: every surface belongs to exactly one app,
// every app the tab bar advertises can actually be reached, and no
// surface is stranded in an app with no tab.

import { describe, it, expect } from 'bun:test';
import { APPS, type AppId } from '@boss/web-kit/nav';
import {
  DEPARTMENT_APP,
  ROUTE_CATALOG,
  appForSection,
  type NavItem,
} from './nav-catalog';
import { readFileSync } from 'node:fs';

/// Verbatim copy of AppShell.svelte's deleted `MODEL_ROUTES`. These
/// surfaces have now moved wholesale from the retired `model` app
/// into `it` — the review resolved that IT is the department and
/// System Model lives inside it, rather than the two being separate
/// tabs (home-workspace-and-department-apps.md, Q2). The MEMBERSHIP
/// is still pinned verbatim: the app they belong to changed, which
/// surfaces belong together did not. If a future change moves a
/// surface into or out of the set, this list is the thing to update,
/// deliberately.
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

  it('the IT app contains exactly what the System Model tab listed', () => {
    const derived = entries
      .filter(([, v]) => v.app === 'it')
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

  it('no surface is left in the retired "model" app', () => {
    // System Model stopped being a top-level app when the review made
    // IT the department that owns it. A straggler here would render
    // under a tab that no longer exists.
    const stragglers = entries
      .filter(([, v]) => (v.app as string) === 'model')
      .map(([k]) => k);
    expect(stragglers).toEqual([]);
  });

  it('IT is a department app, not a second model-facing tab', () => {
    // The decision (Q2) was that IT is a department like Finance or
    // People. Pinning its presence and Simulator's separateness keeps
    // a later reshuffle from quietly recreating the two-model-tabs
    // shape the review rejected.
    const ids = APPS.map((a) => a.id);
    expect(ids).toContain('it');
    expect(ids).not.toContain('model');
    expect(ids.indexOf('it')).toBeGreaterThan(ids.indexOf('simulator'));
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
    expect(appForSection('system-model')).toBe('it');
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

describe('departments map to apps', () => {
  /// Department Classes, read from the files that seed them rather
  /// than restated here — restating is the drift this test exists to
  /// catch.
  ///
  /// They come from TWO places, which is itself worth knowing: the
  /// platform ships twelve (`01-registries.sql`), and the tenant adds
  /// its own (`examples/brewery/seeds/classes.json` adds production,
  /// packaging, taproom, maintenance, distribution, it, admin, audit).
  /// So `apps/web` — which is core — has to map departments a tenant
  /// invented. That works while one tenant ships in-tree; a second
  /// tenant with its own departments needs a real extension point.
  function registryDepartments(): ReadonlyArray<string> {
    const core = readFileSync(
      new URL('../../../../infra/postgres/schema/01-registries.sql', import.meta.url),
      'utf8',
    );
    const coreCodes = [
      ...core.matchAll(/\(\s*'employee',\s*'([a-z-]+)',\s*'[^']*',\s*'department'/g),
    ].map((m) => m[1]!);

    const tenant = JSON.parse(
      readFileSync(
        new URL('../../../../examples/brewery/seeds/classes.json', import.meta.url),
        'utf8',
      ),
    ) as ReadonlyArray<{ member_attribute?: string; code?: string }>;
    const tenantCodes = tenant
      .filter((c) => c.member_attribute === 'department' && c.code)
      .map((c) => c.code!);

    const all = [...new Set([...coreCodes, ...tenantCodes])];
    // A parser that silently matched nothing would make every
    // assertion below vacuous.
    expect(coreCodes.length).toBeGreaterThan(10);
    expect(tenantCodes.length).toBeGreaterThan(0);
    return all;
  }

  it('every department in the registry is served by some app', () => {
    // `audit` had no app at all and nothing failed. A new department
    // row should be a decision about where its people work, not a
    // silent omission.
    const missing = registryDepartments().filter((d) => !(d in DEPARTMENT_APP));
    expect(
      missing,
      `these departments are seeded but map to no app: ${missing.join(', ')}`,
    ).toEqual([]);
  });

  it('every mapping target is an app the chrome bar offers', () => {
    const tabbed = new Set<AppId>(APPS.map((a) => a.id));
    for (const [dept, app] of Object.entries(DEPARTMENT_APP)) {
      expect(
        tabbed.has(app),
        `department "${dept}" maps to app "${app}", which has no tab`,
      ).toBe(true);
    }
  });

  it('maps no department the registry does not have', () => {
    // The other direction: a mapping for a department that was renamed
    // or retired is dead weight that reads as coverage.
    const known = new Set(registryDepartments());
    const stale = Object.keys(DEPARTMENT_APP).filter((d) => !known.has(d));
    expect(stale, `mapped but not in the registry: ${stale.join(', ')}`).toEqual([]);
  });
});
