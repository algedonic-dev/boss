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
  APP_SUBJECT_KINDS,
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

describe('every concrete Subject kind is claimed by an app', () => {
  /// The subject-kind registry is a taxonomy, not a flat list: rows
  /// carry a `parent_kind`, and the roots with children (`person`,
  /// `object`, `intangible`) are abstract — `account` specializes
  /// `person`, and nothing is ever of kind `person` itself. So this
  /// exempts roots-with-children structurally rather than naming them,
  /// which means a future abstract root is exempt automatically and a
  /// future concrete kind is not.
  ///
  /// Rows look like:
  ///   ('account', 'Account', 'desc…', 'platform', 10, 'person'),
  /// with the kind first and parent_kind last. Descriptions contain
  /// commas and parentheses, so this anchors on those two positions
  /// rather than splitting fields.
  function taxonomy(): ReadonlyArray<{ kind: string; parent: string | null }> {
    const sql = readFileSync(
      new URL('../../../../infra/postgres/schema/01-registries.sql', import.meta.url),
      'utf8',
    );
    // Walk lines from the INSERT to the statement terminator. Slicing
    // on the first `;` truncated the block at a semicolon INSIDE a
    // description ("one row per tenant; the subject…"), which silently
    // yielded only the six root rows.
    const lines = sql.slice(sql.indexOf('INSERT INTO subject_kinds')).split('\n');
    const rows: Array<{ kind: string; parent: string | null }> = [];
    for (const line of lines) {
      const isLast = line.trimEnd().endsWith(';');
      // Kind is the first quoted token; parent_kind is the last field.
      // Parsed positionally rather than by one big regex — the
      // descriptions carry commas, parens, quotes and em-dashes, and a
      // regex threading past all of them matched only the rows ending
      // in NULL.
      const kind = /^\s*\('([a-z_-]+)'/.exec(line)?.[1];
      if (!kind) {
        if (isLast) break;
        continue;
      }
      // Strip the row's closing `),` FIRST — otherwise the last comma
      // in the line is the trailing one and the field comes back empty.
      const inner = line.trim().replace(/\),?$/, '');
      const last = inner.slice(inner.lastIndexOf(',') + 1).trim();
      const parent = last.startsWith('NULL')
        ? null
        : (/^'([a-z_-]+)'/.exec(last)?.[1] ?? null);
      rows.push({ kind, parent });
      // Terminator checked AFTER parsing: the final row ends `NULL);`,
      // so breaking first silently dropped it — and it was `custom`,
      // one of the two kinds this whole test exists to catch.
      if (isLast) break;
    }
    // A parser that silently matched nothing — or only some rows —
    // would make every assertion below vacuous. It did exactly that
    // once.
    expect(rows.length).toBeGreaterThan(15);
    expect(rows.filter((r) => r.parent !== null).length).toBeGreaterThan(5);
    return rows;
  }

  it('claims every kind that is not an abstract root', () => {
    const rows = taxonomy();
    const hasChildren = new Set(rows.map((r) => r.parent).filter(Boolean) as string[]);
    const claimed = new Set(Object.values(APP_SUBJECT_KINDS).flat());

    const unclaimed = rows
      .filter((r) => !(r.parent === null && hasChildren.has(r.kind)))
      .map((r) => r.kind)
      .filter((k) => !claimed.has(k));

    expect(
      unclaimed,
      `concrete Subject kinds no app claims, so search never floats them: ` +
        unclaimed.join(', '),
    ).toEqual([]);
  });

  it('claims nothing the registry does not define', () => {
    const known = new Set(taxonomy().map((r) => r.kind));
    const stale = [...new Set(Object.values(APP_SUBJECT_KINDS).flat())].filter(
      (k) => !known.has(k),
    );
    expect(stale, `claimed but not a registered kind: ${stale.join(', ')}`).toEqual([]);
  });

  it('leaves the abstract roots unclaimed', () => {
    // The other direction: claiming `person` would rank a kind that
    // has no instances, which is noise in every result set.
    const claimed = new Set(Object.values(APP_SUBJECT_KINDS).flat());
    for (const root of ['person', 'object', 'intangible']) {
      expect(claimed.has(root), `${root} is abstract and should not be claimed`).toBe(
        false,
      );
    }
  });
});
