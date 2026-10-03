// THE TOP BOARD — design ea906603, car 4 (backlog 74569e94). The HUD
// frame above the Department Map (design 00774ca8) draws four rows of
// things to act on in place of the three rows of flow arithmetic, which
// David removed (Q5, 2026-09-27):
//
//   OUTRANKS REGULAR ORDER  /api/yard/regions `outranks` (car 1)
//   NEEDS YOU               /api/jobs/assignments?for=me (car 1)
//   NEXT UP                 /api/yard/regions `next_up` (car 3)
//   MACHINES                /api/yard/regions `machines`, unchanged
//
// Every row is pinned in its three pictures: POPULATED (each packet a
// link, by protocol and title, never a bare uuid), EMPTY IN WORDS, and
// UNREAD with its reason — an older server's absent field included,
// which must never read as "nothing". And 8c7c2f4b's intent: a row that
// cannot be drawn fails alone, inside its own boundary, while the frame
// and the other rows keep their shape — and it recovers on the next read.

import { expect, test, type Page, type Route } from './_test';
import { TERRITORIES } from '../../src/it/yard/world';
import { YARD_REGIONS, installSmokeMocks } from './_smokeMocks';
import { FAILURE_MARKER } from './_routes';

// NEXT UP prints times in the VIEWER's zone: pin the viewer to David's.
test.use({ timezoneId: 'America/Los_Angeles', locale: 'en-US' });

const NOW = '2026-09-27T12:00:00Z';
const ASSIGNMENTS = /\/api\/jobs\/assignments(\?|$)/;
const HUD = 'section[data-hud]';
const row = (page: Page, name: string) => page.locator(`${HUD} .hud-row[data-row="${name}"]`);

const json = (r: Route, b: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(b) });

const after = (minutes: number): string => new Date(Date.parse(NOW) + minutes * 60_000).toISOString();

const URGENT = {
  id: '4a56c797-1111-4000-8000-000000000001',
  kind: 'backlog-item',
  title: 'Conductor merges on a red gate',
  priority: 'urgent',
  opened_at: '2026-09-24T09:00:00Z',
  age_minutes: 3 * 24 * 60 + 3 * 60,
  at: { slug: 'triage', title: 'Measure the claim, choose a route', status: 'ready', assignee_id: null },
  stations: ['backlog'],
};
const EMERGENCY = {
  ...URGENT,
  id: 'df6251c8-2222-4000-8000-000000000002',
  title: 'The system of record is dark',
  priority: 'emergency',
  age_minutes: 45,
  at: null,
  stations: null,
};

/** Car 3's wire shape, `boss_jobs::next_up::NextEvent`: timed rows
 *  soonest first, then the untimed, then the unread. */
const event = (kind: string, title: string, at: string | null, over: Record<string, unknown> = {}) => ({
  kind, title, at, estimate: false, basis: '', source: 'cadence registry + loading dock', unread: null, ...over,
});
const NEXT_UP = [
  event('gates', 'next bay free', after(7), { estimate: true, basis: 'median gate 11m, 2 queued', source: 'gate-runs + measured gate durations' }),
  event('train-board', 'next board, 3 cars waiting', after(30), { basis: 'cooldown ends' }),
  event('scheduled', 'daily publish-to-github', after(6 * 60 + 52), { basis: 'daily at the day boundary', source: 'dispatcher schedule' }),
  event('rotation-due', 'forge token (dev pod)', after(5 * 24 * 60), { basis: 'rotation policy', source: 'credentials registry' }),
  event('in-transit', 'train #724', null, { basis: 'no measured arrivals to estimate from', source: 'open trains + measured arrivals' }),
];

const MACHINES = {
  running: 4, idle: 3, failed: 1, unknown: 1, total: 9,
  failed_or_unknown: [
    { region: 'gates', id: 'bay:2', name: 'bay 2', state: 'failed', why: 'its gate-run is past its own deadline' },
    { region: 'marshalling', id: 'station:design-review', name: 'design-review', state: 'unknown', why: 'blind' },
  ],
};

/** The regions payload, every territory clear, with the three board
 *  fields — and the `thirds` block the server still sends, which the
 *  board no longer draws. */
const regions = (over: Record<string, unknown> = {}) => ({
  window_hours: 24,
  now: NOW,
  regions: TERRITORIES.map(({ name }) => ({
    name, count: 1, state: 'clear', why: `${name} is quiet`,
    trend: { metric: 'm', unit: 'per day', current: 1, previous: 1, samples: 1, previous_samples: 1 },
  })),
  thirds: [
    { third: 'queue-management', regions: ['receiving'],
      balance: { unit: 'packets', in_means: 'opened', out_means: 'taken', in: 5, out: 4, net: 1, in_count: 5, out_count: 4 },
      stuck: { third: 'queue-management', stuck: 0, waiting: 0, unknown: [], oldest_hours: null, regions: [] } },
  ],
  machines: MACHINES,
  outranks: [URGENT, EMERGENCY],
  next_up: NEXT_UP,
  ...over,
});

const step = (over: Record<string, unknown>) => ({
  job_id: 'b1ed7200-3333-4000-8000-000000000003',
  job_title: 'Merge the tenant main',
  opened_on: '2026-09-25',
  workflow: 'ship-a-change',
  subject_kind: 'custom',
  subject_id: 's',
  priority: 'standard',
  ...over,
  step: {
    id: 'c0ffee00-4444-4000-8000-000000000004', job_id: 'b1ed7200-3333-4000-8000-000000000003',
    kind: 'sign-off', title: 'Approve the merge', status: 'ready', assignee_id: 'emp-david',
    ...((over.step as Record<string, unknown> | undefined) ?? {}),
  },
});

const FOR = { ids: ['emp-david'], roles: ['platform-admin'] };
const NEEDS = {
  data: [
    step({}),
    step({
      job_id: 'ea906603-5555-4000-8000-000000000005', job_title: 'The top board', workflow: 'design-doc',
      opened_on: '2026-09-27', priority: 'urgent',
      step: { id: 'b1ed7200-6666-4000-8000-000000000006', job_id: 'ea906603-5555-4000-8000-000000000005',
        kind: 'review-design', title: 'Decide the design', assignee_id: null },
    }),
  ],
  total: 2,
  for: FOR,
};

async function board(
  page: Page,
  payload: unknown = regions(),
  needs: (r: Route) => Promise<void> = (r) => json(r, NEEDS),
): Promise<void> {
  await page.clock.install({ time: new Date(NOW) });
  await installSmokeMocks(page);
  await page.route(YARD_REGIONS, (r) => json(r, payload));
  await page.route(ASSIGNMENTS, needs);
}

test('the board stands above the map with four rows, and the three-thirds rows are gone', async ({ page }) => {
  await board(page);
  await page.goto('/it');
  const hud = page.locator(HUD);
  await expect(hud).toHaveAttribute('data-read', 'ok');
  await expect(hud.locator('.hud-age')).toHaveText(/^read \d+s ago$/);
  await expect(hud.locator('.hud-label')).toHaveText(['Outranks regular order', 'Needs you', 'Next up', 'Machines']);
  // Q5: removed, not moved — no third, no balance, no stuck figure,
  // though the payload still carries its `thirds` block.
  await expect(hud.locator('[data-third]')).toHaveCount(0);
  await expect(hud).not.toContainText('balance');
  await expect(hud).not.toContainText('Queue management');
  // Above the map.
  const hudTop = (await hud.boundingBox())!.y;
  const mapTop = (await page.locator('section.yard svg').boundingBox())!.y;
  expect(hudTop).toBeLessThan(mapTop);
  await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
});

test('POPULATED: each row draws the server’s field, each packet a link by protocol and title', async ({ page }) => {
  await board(page);
  await page.goto('/it');

  // OUTRANKS, oldest first as sent, the count on the troubled plate.
  const out = row(page, 'outranks');
  await expect(out.locator('[data-state="rows"]')).toHaveCount(1);
  await expect(out.locator('.hud-count [data-fig="value"]')).toHaveText('2');
  await expect(out.locator('.hud-count .plate-troubled')).toHaveCount(1);
  const outLinks = out.locator('li[data-outrank] a');
  await expect(outLinks).toHaveCount(2);
  await expect(outLinks.nth(0)).toHaveAttribute('href', `/ux/jobs/${URGENT.id}`);
  await expect(outLinks.nth(0)).toHaveText(/urgent\s*backlog-item\s*Conductor merges on a red gate\s*3d\s*triage · backlog/);
  await expect(outLinks.nth(1)).toHaveText(/emergency\s*backlog-item\s*The system of record is dark\s*45m\s*between steps/);

  // NEEDS YOU, oldest first, each a link to its step with Back to here.
  const needs = row(page, 'needs-you');
  await expect(needs.locator('.hud-count')).toHaveText(/2\s*for emp-david · platform-admin/);
  const needLinks = needs.locator('li[data-need] a');
  await expect(needLinks).toHaveCount(2);
  await expect(needLinks.nth(0)).toHaveAttribute(
    'href',
    '/ux/jobs/b1ed7200-3333-4000-8000-000000000003/steps/c0ffee00-4444-4000-8000-000000000004?from=%2Fit&from_label=Department+Map',
  );
  await expect(needLinks.nth(0)).toHaveText(/sign-off\s*ship-a-change\s*Merge the tenant main\s*Approve the merge\s*2d/);
  await expect(needLinks.nth(1)).toHaveText(/review-design\s*design-doc\s*The top board\s*Decide the design\s*urgent\s*today/);

  // NEXT UP, a departures board: the viewer's zone in the head, each
  // time in it, and the time left; an estimate wears "~", the basis is
  // the small print, and a row with no time says so beside its basis.
  const next = row(page, 'next-up');
  await expect(next.locator('.dep-head .dep-time')).toHaveText('time (PDT)');
  const deps = next.locator('.dep[data-next]');
  await expect(deps).toHaveCount(5);
  await expect(deps.locator('.dep-source')).toHaveText(['gates', 'train board', 'scheduled', 'rotation due', 'in transit']);
  await expect(deps.locator('.dep-title')).toHaveText([
    'next bay free', 'next board, 3 cars waiting', 'daily publish-to-github', 'forge token (dev pod)', 'train #724',
  ]);
  await expect(deps.locator('.dep-time')).toHaveText(['~05:07', '05:30', '11:52', 'Oct 02', 'no time']);
  await expect(deps.locator('.dep-in')).toHaveText(['~7m', '30m', '6h52m', '5d']);
  await expect(deps.locator('.dep-basis')).toHaveText([
    'median gate 11m, 2 queued', 'cooldown ends', 'daily at the day boundary', 'rotation policy',
    'no measured arrivals to estimate from',
  ]);
  await expect(next.locator('.dep[data-state="untimed"]')).toHaveAttribute('data-next', 'in transit');
  await expect(next.locator('[data-fig="unread"]')).toHaveCount(0);

  // MACHINES, unchanged: failed and unjudged against a total, each listed.
  const machines = row(page, 'machines');
  await expect(machines.locator('.hud-machine-line [data-fig]')).toHaveText(['1', '1', '9']);
  await expect(machines.locator('a[data-machine]')).toHaveCount(2);

  // NEVER A BARE UUID where a title belongs: every id is in an href only.
  for (const id of [URGENT.id, EMERGENCY.id, 'b1ed7200-3333-4000-8000-000000000003']) {
    await expect(page.locator(HUD)).not.toContainText(id);
    await expect(page.locator(HUD)).not.toContainText(id.slice(0, 8));
  }

  // A packet link opens the packet.
  await outLinks.nth(0).click();
  await expect(page).toHaveURL(new RegExp(`/ux/jobs/${URGENT.id}$`));
});

test('EMPTY: each row says so in words — an answer, not a blank', async ({ page }) => {
  await board(
    page,
    regions({ outranks: [], next_up: [], machines: { running: 3, idle: 2, failed: 0, unknown: 0, total: 5, failed_or_unknown: [] } }),
    (r) => json(r, { data: [], total: 0, for: FOR }),
  );
  await page.goto('/it');
  await expect(row(page, 'outranks').locator('[data-state="empty"] .hud-words')).toHaveText('Nothing outranks regular order.');
  await expect(row(page, 'outranks').locator('.hud-count [data-fig="zero"]')).toHaveText('0');
  await expect(row(page, 'needs-you').locator('[data-state="empty"] .hud-words')).toHaveText('Nothing needs you.');
  await expect(row(page, 'needs-you').locator('.hud-count')).toContainText('for emp-david · platform-admin');
  await expect(row(page, 'next-up').locator('[data-state="empty"] .hud-words')).toHaveText('Nothing is due ahead.');
  await expect(row(page, 'machines').locator('.hud-machine-line [data-fig]')).toHaveText(['0', '0', '5']);
  await expect(page.locator(`${HUD} [data-fig="unread"]`)).toHaveCount(0);
});

test('UNREAD, an older server: absent fields and an ignored for=me are unread with the reason — never empty', async ({ page }) => {
  // What a server without cars 1 and 3 answers: no `outranks`, no
  // `next_up`, and an assignments list with no `for` block, because it
  // dropped the parameter it did not know and answered the empty
  // no-selector list.
  const older = regions();
  delete (older as Record<string, unknown>).outranks;
  delete (older as Record<string, unknown>).next_up;
  await board(page, older, (r) => json(r, { data: [], total: 0 }));
  await page.goto('/it');
  for (const [name, reason] of [
    ['outranks', 'this server does not send what outranks regular order'],
    ['needs-you', 'this server does not answer for=me'],
    ['next-up', 'this server does not send what is next up'],
  ] as const) {
    const r = row(page, name);
    await expect(r.locator('[data-state="unread"] [data-fig="unread"]')).toHaveText('?');
    await expect(r.locator('[data-state="unread"] .hud-why')).toContainText(reason);
    await expect(r.locator('.hud-words')).toHaveCount(0);
    await expect(r.locator('[data-fig="zero"]')).toHaveCount(0);
  }
  // The rows it could read still read.
  await expect(row(page, 'machines').locator('.hud-machine-line [data-fig]')).toHaveText(['1', '1', '9']);
});

test('UNREAD, a failed read: the server’s own null, a refused queue, and one dark source each say why', async ({ page }) => {
  await board(
    page,
    regions({
      outranks: null,
      next_up: [
        event('gates', 'a bay frees', after(10)),
        event('scheduled', 'the dispatcher schedule', null, { unread: 'the dispatcher did not answer', source: 'dispatcher schedule' }),
      ],
    }),
    (r) => json(r, 'the agents registry did not answer', 503),
  );
  await page.goto('/it');
  await expect(row(page, 'outranks').locator('[data-state="unread"] .hud-why')).toHaveText(
    'the server could not read what outranks regular order',
  );
  await expect(row(page, 'needs-you').locator('[data-state="unread"] .hud-why')).toHaveText(
    'the read failed — /api/jobs/assignments?for=me&limit=500: HTTP 503',
  );
  // A dark source is its own line; the rest of the board still reads.
  const next = row(page, 'next-up');
  await expect(next.locator('.dep[data-next="scheduled"] [data-fig="unread"]')).toHaveText('?');
  await expect(next.locator('.dep[data-next="scheduled"] .hud-why')).toHaveText('the dispatcher did not answer');
  await expect(next.locator('.dep[data-next="gates"] .dep-in')).toHaveText('10m');
});

// Backlog bd506215: a narrowed scope is served `withheld_rows` for the
// sources not scoped by packet (next_up.rs, 0964ba80) — a refusal by
// policy, drawn as the row and the scope's words, never the unread `?`.
test('WITHHELD by policy scope: the row stands, says not in your policy scope, and wears no ?', async ({ page }) => {
  const why = "withheld: this caller's policy scope does not read every packet, and this source is not scoped by packet";
  await board(
    page,
    regions({
      next_up: [
        event('gates', 'a bay frees', after(10)),
        event('scheduled', 'scheduled rules', null, { unread: why, withheld: true, basis: 'the dispatcher schedule is withheld from this caller', source: 'dispatcher schedule' }),
        event('rotation-due', 'credential rotations', null, { unread: why, withheld: true, basis: 'the credentials registry is withheld from this caller', source: 'credentials registry' }),
      ],
    }),
  );
  await page.goto('/it');
  const next = row(page, 'next-up');
  for (const kind of ['scheduled', 'rotation due']) {
    const dep = next.locator(`.dep[data-next="${kind}"]`);
    await expect(dep).toHaveAttribute('data-state', 'withheld');
    await expect(dep.locator('.dep-basis')).toHaveText('not in your policy scope');
    await expect(dep.locator('[data-fig="unread"]')).toHaveCount(0);
  }
  await expect(next.locator('.dep[data-next="scheduled"] .dep-title')).toHaveText('scheduled rules');
  await expect(next.locator('.dep[data-next="gates"] .dep-in')).toHaveText('10m');
});

test('UNREAD, the regions read failed: every regions row is ? with the failure, and no stale value is kept', async ({ page }) => {
  await board(page, regions());
  await page.unroute(YARD_REGIONS);
  await page.route(YARD_REGIONS, (r) => json(r, 'the backend is down', 500));
  await page.goto('/it');
  const hud = page.locator(HUD);
  await expect(hud).toHaveAttribute('data-read', 'failed');
  await expect(hud.locator('.hud-age')).toHaveText(/^read failed \d\d:\d\dZ · no good read yet$/);
  for (const name of ['outranks', 'next-up']) {
    await expect(row(page, name).locator('.hud-why')).toHaveText('the read failed — /api/yard/regions: HTTP 500');
  }
  await expect(row(page, 'machines').locator('[data-fig="unread"]')).toHaveCount(3);
  // NEEDS YOU is its own read, and it landed.
  await expect(row(page, 'needs-you').locator('li[data-need]')).toHaveCount(2);
  await expect(hud.locator('.hud-label')).toHaveText(['Outranks regular order', 'Needs you', 'Next up', 'Machines']);
});

test('A ROW THAT CANNOT BE DRAWN FAILS ALONE (8c7c2f4b), and draws again on the next read', async ({ page }) => {
  // The shape that blanked the shop floor on 2026-09-24 (846ab934): two
  // machines under ONE key, which the machine list is keyed by. The
  // first read carries it; every later read is HELD until the test
  // answers it, so the recovery lands where the test says (3027f808).
  const twin = { region: 'gates', id: 'bay:2', name: 'bay 2', state: 'failed', why: 'past its deadline' };
  const broken = regions({ machines: { ...MACHINES, failed_or_unknown: [twin, { ...twin }] } });
  let n = 0;
  const held: Route[] = [];
  await board(page);
  await page.unroute(YARD_REGIONS);
  await page.route(YARD_REGIONS, (r) => (++n === 1 ? json(r, broken) : void held.push(r)));
  await page.goto('/it');

  const machines = row(page, 'machines');
  await expect(machines.locator('[data-state="failed"] [data-fig="unread"]')).toHaveText('?');
  await expect(machines.locator(`[data-state="failed"] .hud-why${FAILURE_MARKER}`)).toContainText('This row cannot be drawn — ');
  // The frame keeps its shape and every other row still reads.
  await expect(page.locator(`${HUD} .hud-label`)).toHaveText(['Outranks regular order', 'Needs you', 'Next up', 'Machines']);
  await expect(row(page, 'outranks').locator('li[data-outrank]')).toHaveCount(2);
  await expect(row(page, 'needs-you').locator('li[data-need]')).toHaveCount(2);
  await expect(row(page, 'next-up').locator('.dep[data-next]')).toHaveCount(5);

  // The next read is well-formed, and the row draws again.
  await page.clock.runFor(10_000);
  await expect.poll(() => held.length).toBeGreaterThan(0);
  await Promise.all(held.splice(0).map((r) => json(r, regions())));
  await expect(machines.locator('[data-state="failed"]')).toHaveCount(0);
  await expect(machines.locator('a[data-machine]')).toHaveCount(2);
});
