// /it/registry/subjects — WHAT EACH READ SAYS, answered or failed (page
// audit 9f7ba57d, 2026-09-27).
//
// The page makes three reads and, until this spec, no spec asserted a
// word any of them rendered on failure. outage-crawl held the route on
// SILENT ("reads /api/subject-kinds + /api/classes, which HEALTHY keeps
// up"), and mount-refetch-audit counts the kinds read without reading the
// page. So:
//
// - A failed CLASSES read painted "No classes registered for <kind>"
//   directly under the "Failed to load" alert — the alert and the empty
//   line contradicting each other on one screen (backlog d145e41d). The
//   detail pane now says which kind's read failed, in place of the empty
//   line.
// - A failed KINDS read still said "Select a subject kind to see its
//   classes." over an empty tree. The pane now says nothing beyond the
//   alert.
// - The owner line read `owner platform` on 24 of 24 kinds, which
//   distinguishes nothing. It now names the kind's module, and whether
//   that module is off on this instance, and how many active workflows
//   name the kind — a count that says it is unknown when its read fails
//   rather than zero (backlog 92ea2e00).
// - The six node-role Classes read "(unclassified)" although a NULL
//   member_attribute is their declared shape; the group is titled by
//   metadata.membership (backlog 2c7a2d5c).
//
// The outage crawl breaks every read at once, and under it the workflow
// count's own failure line is always there to be counted — so a marker
// count there cannot tell a regressed classes line from a working one.
// Each failure line is pinned HERE, by its words, one read at a time.

import { expect, test, type Page, type Route } from './_test';
import { mountPage } from './_helpers';
import { FAILURE_MARKER } from './_routes';
import {
  LIVE_MANIFEST_RECORDED_AT,
  MODULES_LIVE,
  MODULES_ON,
  installSmokeMocks,
  installTenantManifest,
} from './_smokeMocks';

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

const kind = (
  k: string,
  parent_kind: string | null,
  sort_order: number,
  metadata: Record<string, unknown> = {},
) => ({
  kind: k,
  label: k[0]!.toUpperCase() + k.slice(1),
  parent_kind,
  description: null,
  owning_team: 'platform',
  metadata,
  sort_order,
  retired_at: null,
});

/// person is the first root, so it is the kind selected on mount.
const KINDS = [
  kind('person', null, 1),
  kind('employee', 'person', 1),
  kind('object', null, 3),
  kind('asset', 'object', 1, { module: 'equipment' }),
  kind('node', 'object', 2),
];

const cls = (
  subject_kind: string,
  code: string,
  member_attribute: string | null,
  metadata: Record<string, unknown> = {},
) => ({
  subject_kind,
  code,
  display_name: code,
  parent_code: null,
  member_attribute,
  metadata,
  sort_order: 0,
  retired_at: null,
});

/// The shape 202609120300-a-node-declares-its-roles.sql gives every node
/// role: no member_attribute, membership named in metadata.
const NODE_ROLES = [
  cls('node', 'build', null, { membership: 'node_roles' }),
  cls('node', 'gate', null, { membership: 'node_roles' }),
];

const CLASSES_OF = (k: string): RegExp => new RegExp(`/api/classes\\?subject_kind=${k}$`);

type Reads = Readonly<{
  kinds?: (r: Route) => Promise<void>;
  workflows?: (r: Route) => Promise<void>;
  modules?: Readonly<Record<string, boolean>>;
}>;

/// The smoke mocks, then this page's three reads, each answering unless
/// the test says otherwise. Registered after, so they win.
async function openSubjects(page: Page, reads: Reads = {}): Promise<void> {
  await installSmokeMocks(page);
  if (reads.modules) await installTenantManifest(page, reads.modules);
  await page.route(/\/api\/subject-kinds$/, reads.kinds ?? ((r) => json(r, KINDS)));
  await page.route(CLASSES_OF('person'), (r) => json(r, []));
  await page.route(CLASSES_OF('employee'), (r) => json(r, [cls('employee', 'ceo', 'role')]));
  await page.route(CLASSES_OF('asset'), (r) => json(r, []));
  await page.route(CLASSES_OF('node'), (r) => json(r, NODE_ROLES));
  await page.route(
    /\/api\/workflows$/,
    reads.workflows ??
      ((r) =>
        json(r, [
          { kind: 'kettle-clean', status: 'active', subject_kinds: ['asset'] },
          { kind: 'kettle-audit', status: 'retired', subject_kinds: ['asset'] },
        ])),
  );
  await mountPage(page, '/it/registry/subjects');
}

const detail = (page: Page) => page.locator('.sc-detail');
const meta = (page: Page) => page.locator('.sc-meta');
const pick = (page: Page, k: string) =>
  page.locator('.sc-kind', { has: page.locator('.sc-kind-code', { hasText: new RegExp(`^${k}\\b`) }) });

test.describe('/it/registry/subjects says what each read found, or that it failed', () => {
  test('control: every read answers — an empty kind says so, and no failure line shows', async ({ page }) => {
    await openSubjects(page);
    await expect(detail(page)).toContainText('No classes registered for person');
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
  });

  test('a failed classes read names the kind it failed for, and states no empty', async ({ page }) => {
    await openSubjects(page);
    await page.route(CLASSES_OF('employee'), (r) => json(r, { error: 'classes down' }, 500));
    await pick(page, 'employee').click();

    await expect(page.locator(FAILURE_MARKER)).toHaveCount(1);
    await expect(detail(page).locator(FAILURE_MARKER)).toContainText(
      "Couldn't load the classes of employee — HTTP 500",
    );
    await expect(detail(page)).not.toContainText('No classes registered');
  });

  test('a failed kinds read shows the alert and nothing in the pane beneath it', async ({ page }) => {
    await openSubjects(page, { kinds: (r) => json(r, { error: 'registry down' }, 500) });

    await expect(page.locator(FAILURE_MARKER)).toHaveCount(1);
    await expect(page.locator(FAILURE_MARKER)).toContainText('Failed to load: HTTP 500');
    await expect(page.getByText('Subject-kind count unknown')).toBeVisible();
    await expect(page.getByText('Select a subject kind')).toHaveCount(0);
    await expect(detail(page)).toBeEmpty();
  });

  test('node roles are titled by their declared membership, not "(unclassified)"', async ({ page }) => {
    await openSubjects(page);
    await pick(page, 'node').click();

    await expect(detail(page).locator('h3')).toHaveText(['node_roles · 2']);
    await expect(detail(page)).not.toContainText('(unclassified)');
  });
});

test.describe('/it/registry/subjects names each kind by its module and the workflows that use it', () => {
  test(`as live (manifest recorded ${LIVE_MANIFEST_RECORDED_AT}): asset reads its module, off here`, async ({ page }) => {
    expect(MODULES_LIVE['equipment']).toBe(false);
    await openSubjects(page, { modules: MODULES_LIVE });
    await pick(page, 'asset').click();

    await expect(meta(page)).toContainText('module equipment · off on this instance');
    await expect(meta(page)).toContainText('1 active workflow names this kind');
    await expect(meta(page)).not.toContainText('owner');
  });

  test('a module the instance runs is named without the off mark', async ({ page }) => {
    await openSubjects(page, { modules: MODULES_ON });
    await pick(page, 'asset').click();

    await expect(meta(page)).toContainText('module equipment');
    await expect(meta(page)).not.toContainText('off on this instance');
  });

  test('a platform kind names no module and counts zero workflows as zero', async ({ page }) => {
    await openSubjects(page);
    await pick(page, 'employee').click();

    await expect(meta(page)).toContainText('platform kind');
    await expect(meta(page)).not.toContainText('module');
    await expect(meta(page)).toContainText('no active workflow names this kind');
  });

  test('a failed workflows read says the count is unknown, never zero', async ({ page }) => {
    await openSubjects(page, { workflows: (r) => json(r, { error: 'registry down' }, 500) });
    await pick(page, 'employee').click();

    await expect(page.locator(FAILURE_MARKER)).toHaveCount(1);
    await expect(detail(page).locator(FAILURE_MARKER)).toContainText(
      "Couldn't count the workflows naming employee — HTTP 500",
    );
    await expect(detail(page)).not.toContainText('no active workflow');
  });
});
