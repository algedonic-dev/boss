// The nav catalog is now the single source of truth for which app a
// surface belongs to. These tests pin that it answers exactly what the
// two hand-maintained sets it replaced used to answer, so the
// de-duplication is provably behaviour-preserving rather than
// approximately so.
//
// The sets were:
//   AppShell.svelte  MODEL_ROUTES: Set<RouteName>      (drove the sidebar)
//   App.svelte       MODEL_KINDS:  Set<Route['kind']>  (drove the active tab)
//
// Two vocabularies, two files, one question. Either could drift and a
// page would render under the wrong tab with nothing failing.

import { describe, it, expect } from 'bun:test';
import { ROUTE_CATALOG, appForSection, type NavItem } from './nav-catalog';

/// Verbatim copy of AppShell.svelte's deleted `MODEL_ROUTES`. This is
/// the behaviour being preserved — if a future change moves a surface
/// between apps this list is the thing to update, deliberately.
const LEGACY_MODEL_ROUTES: ReadonlyArray<string> = [
  'system-model', 'system-monitoring', 'system-step-plugins', 'system-dispatcher',
  'system-subjects', 'system-dispatcher-rules', 'system-dispatcher-rule',
  'system-kb', 'system-design', 'system-experiments', 'policy', 'job-kinds',
  'workflows', 'auth-admin',
];

describe('nav catalog — app assignment', () => {
  it('every catalog entry declares an app', () => {
    const missing = Object.entries(ROUTE_CATALOG)
      .filter(([, v]) => (v as NavItem).app === undefined)
      .map(([k]) => k);
    expect(
      missing,
      `these surfaces declare no app and would render under whichever tab ` +
        `they happened to fall back to: ${missing.join(', ')}`,
    ).toEqual([]);
  });

  it('the model app contains exactly the surfaces MODEL_ROUTES listed', () => {
    const derived = Object.entries(ROUTE_CATALOG)
      .filter(([, v]) => (v as NavItem).app === 'model')
      .map(([k]) => k)
      .sort();
    expect(derived).toEqual([...LEGACY_MODEL_ROUTES].sort());
  });

  it('everything else is the user app — no surface is silently unassigned', () => {
    const model = new Set(LEGACY_MODEL_ROUTES);
    const others = Object.entries(ROUTE_CATALOG).filter(([k]) => !model.has(k));
    expect(others.length).toBeGreaterThan(0);
    for (const [name, item] of others) {
      expect((item as NavItem).app, `${name} should be in the user app`).toBe('user');
    }
  });
});

describe('appForSection — the App.svelte tab derivation', () => {
  it('resolves model surfaces to the model app', () => {
    expect(appForSection('system-model')).toBe('model');
    expect(appForSection('system-dispatcher')).toBe('model');
    expect(appForSection('workflows')).toBe('model');
    expect(appForSection('policy')).toBe('model');
  });

  it('resolves operator surfaces to the user app', () => {
    expect(appForSection('finance')).toBe('user');
    expect(appForSection('accounts')).toBe('user');
    expect(appForSection('jobs')).toBe('user');
  });

  it('falls back to the user app for unknown sections', () => {
    // `me` is App.svelte's terminal fallback in the activeSection
    // ternary and is not a catalog entry; the old MODEL_KINDS check
    // likewise answered "not model" for it.
    expect(appForSection('me')).toBe('user');
    expect(appForSection('definitely-not-a-section')).toBe('user');
  });
});
