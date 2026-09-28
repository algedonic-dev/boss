// /it/registry/drift — the measurement header and the three honest
// behaviours, rendered (page audit 93b508fd).
//
// Until this spec the route was reached only by the crawls, which see
// the empty `[]` answer and nothing else. The audit's decisions, each
// pinned here as the page draws it:
//   0646ac7d — the section-00 header links the measured packet, so the
//              record the page is drawn from is one click away;
//   d83886c6 — the header names the measurement's age and marks it warn
//              past 26 h (the daily period plus two hours' slack): a
//              stopped cadence would otherwise keep drawing a clean
//              streak as current;
//   90a24e28 — one line under the strip says which tenant seeds the
//              "authored" count includes, so 96 authored against 64
//              admitted does not read as 32 missing kinds;
//   fb5f1c2f — the compared fields are the packet's own
//              `measured.method.fields`, and the subtitle no longer
//              keeps a second, drifted list;
//   3a8ab64c — the no-packet notice states the cadence, with no date;
//   cece5515 — the authority-role failure arm, the write refusal, and
//              the 10 s re-read that runs only while a request is open.
//
// The fixture is the live packet b35a096b as measured 2026-09-27
// 05:21:58Z (its method.fields abridged), plus one invented adrift row
// where a test needs the approve section to render.

import { expect, test, type Page, type Route } from './_test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';
import { FAILURE_MARKER } from './_routes';

const PAGE = '/it/registry/drift';
const TITLE = 'Protocol drift';
const PACKET = 'b35a096b-a314-486f-98db-5f9c1b85d910';
const MEASURED_AT = '2026-09-27T05:21:58Z';
const HOURS = 3_600_000;
const at = (hoursAfter: number): Date => new Date(Date.parse(MEASURED_AT) + hoursAfter * HOURS);

const METHOD_FIELDS =
  'label, description, category — the scalar strings an operator reads — and, since 2026-09-15, four step facets: steps.count, steps.titles (ordered), steps.<title>.required and steps.<title>.title_template';

const ADRIFT = {
  kind: 'ship-a-change',
  field: 'description',
  live_version: 31,
  at: 0,
  tree_len: 640,
  live_len: 601,
  tree_window: 'A change to the tree, from branch to converged',
  live_window: 'One change to the tree from branch to live',
};

function packet(fields: ReadonlyArray<unknown> = []): Record<string, unknown> {
  return {
    id: PACKET,
    kind: 'maintenance-protocol-drift',
    status: 'closed',
    title: 'Protocol drift — 2026-09-27',
    metadata: {
      measured: {
        at: MEASURED_AT,
        head: '756da39353512dd89ff80ac8d5e455233e1ee14e',
        head_at: '2026-09-27T05:00:31Z',
        head_why: null,
        target: 'http://10.20.0.34:7900/api/workflows',
        lint_exit: fields.length > 0 ? 2 : 0,
        live_admitted: 64,
        authored: 96,
        fields_parsed: 56,
        fields_compared: 56,
        exempt: [],
        method: { comparator: 'infra/lint/the-live-protocols-are-the-authored-protocols.sh', fields: METHOD_FIELDS },
      },
      drift: {
        counts: { unauthored: 0, fields: fields.length, absent: 0, pending: 0 },
        unauthored: [],
        fields,
        absent: [],
        pending: [],
        tenants: [{ file: 'examples/brewery/seeds/workflows.toml', not_admitted: 37, total: 37 }],
      },
    },
  };
}

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

const jobsOfKind =
  (kind: string) =>
  (u: URL): boolean =>
    u.pathname === '/api/jobs' && u.searchParams.get('kind') === kind;

/// The ops-request row: its execute step names the role that may
/// approve — the persona's, so the control is admitted.
const OPS_REQUEST_ROW = { kind: 'ops-request', steps: [{ title: 'execute', authority_role: 'ceo' }] };

type Install = Readonly<{
  drift?: unknown;
  requests?: (r: Route) => Promise<void>;
  authority?: (r: Route) => Promise<void>;
  create?: (r: Route) => Promise<void>;
}>;

async function install(page: Page, opts: Install = {}): Promise<void> {
  await installSmokeMocks(page);
  // Signed in as the smoke persona (emp-001, role ceo), whose people row
  // installSmokeMocks serves — the approve admits a viewer, never nobody.
  await page.route(/\/api\/session$/, (r) => json(r, { username: 'ceo@demo', employee_id: 'emp-001', role: 'ceo' }));
  await page.route(jobsOfKind('maintenance-protocol-drift'), (r) => json(r, opts.drift ?? { data: [packet()], total: 1 }));
  await page.route(jobsOfKind('ops-request'), opts.requests ?? ((r) => json(r, { data: [], total: 0 })));
  await page.route(/\/api\/workflows\/ops-request$/, opts.authority ?? ((r) => json(r, OPS_REQUEST_ROW)));
  await page.route(
    (u) => u.pathname === '/api/jobs',
    (r) => (r.request().method() === 'POST' && opts.create ? opts.create(r) : r.fallback()),
  );
}

const header = (page: Page) => page.locator('.pd-section').first();

test.describe('/it/registry/drift — the measurement header', () => {
  test('links the measured packet, names its age, the compared fields, and the tenant seeds', async ({ page }) => {
    await page.clock.setFixedTime(at(14));
    await install(page);
    await mountPage(page, PAGE, { titleMatch: new RegExp(TITLE) });

    // 0646ac7d: the record is one click away.
    const link = header(page).getByRole('link', { name: 'packet b35a096b' });
    await expect(link).toHaveAttribute('href', `/jobs/${PACKET}`);

    // d83886c6: inside the daily period the age is drawn, unmarked.
    const age = header(page).locator('.pd-age');
    await expect(age).toHaveText('14h ago');
    await expect(age).not.toHaveClass(/\bwarn\b/);

    // fb5f1c2f: the script's own statement, and no second list above it.
    await expect(page.locator('.pd-method')).toContainText(METHOD_FIELDS);
    const subtitle = page.locator('header.exec-header p');
    await expect(subtitle).toContainText('the measurement names which fields it compares');
    await expect(subtitle).not.toContainText('label, description, category');

    // 90a24e28: the 96 authored is read with the tenant seeds it includes.
    const tenants = page.locator('.pd-tenants');
    await expect(tenants).toContainText('examples/brewery/seeds/workflows.toml, 37 kinds, 37 not admitted here');
    await expect(tenants).toContainText('This instance does not run the demo tenant, by design.');
  });

  test('past 26 h the age is marked warn and says a run was missed', async ({ page }) => {
    await page.clock.setFixedTime(at(27));
    await install(page);
    await mountPage(page, PAGE, { titleMatch: new RegExp(TITLE) });

    const age = header(page).locator('.pd-age');
    await expect(age).toHaveClass(/\bwarn\b/);
    await expect(age).toHaveText('27h ago — past the daily 26h, so a 05:20Z run was missed');
  });

  test('no packet states the cadence that has not filed, with no date', async ({ page }) => {
    await install(page, { drift: { data: [], total: 0 } });
    await mountPage(page, PAGE, { titleMatch: new RegExp(TITLE) });

    const notice = page.locator('.pd-notice');
    await expect(notice).toContainText('the daily 05:20Z measurement on boss-gcp has not filed a packet');
    await expect(notice).not.toContainText(/20\d\d-\d\d-\d\d|train \d/);
    await expect(page.locator('.pd-section')).toHaveCount(0);
  });
});

test.describe('/it/registry/drift — the approve section fails honestly (cece5515)', () => {
  test('a failed authority-role read withholds every control, with the failure marker', async ({ page }) => {
    await install(page, {
      drift: { data: [packet([ADRIFT])], total: 1 },
      authority: (r) => json(r, 'upstream down', 503),
    });
    await mountPage(page, PAGE, { titleMatch: new RegExp(TITLE) });

    const fail = page.locator(FAILURE_MARKER).filter({ hasText: 'The ops-request protocol did not answer' });
    await expect(fail).toHaveCount(1);
    await expect(fail).toContainText('HTTP 503');
    await expect(page.locator('.ap-kind')).toHaveCount(0);
  });

  test('a refused create says the request was not filed, with the server\'s words', async ({ page }) => {
    await install(page, {
      drift: { data: [packet([ADRIFT])], total: 1 },
      create: (r) => r.fulfill({ status: 403, contentType: 'text/plain', body: 'policy: create ops-request denied' }),
    });
    await mountPage(page, PAGE, { titleMatch: new RegExp(TITLE) });

    await page.getByRole('button', { name: "Approve: publish the tree's row" }).click();
    await expect(page.locator('.ap-error')).toHaveText(
      'The request was not filed: file ops-request: HTTP 403: policy: create ops-request denied',
    );
    await expect(page.locator(`.ap-error${FAILURE_MARKER}`)).toHaveCount(1);
  });

  test('a create that names no id is not called filed', async ({ page }) => {
    await install(page, {
      drift: { data: [packet([ADRIFT])], total: 1 },
      create: (r) => json(r, { status: 'open' }, 201),
    });
    await mountPage(page, PAGE, { titleMatch: new RegExp(TITLE) });

    await page.getByRole('button', { name: "Approve: publish the tree's row" }).click();
    await expect(page.locator('.ap-error')).toContainText('the create returned no id');
  });
});

test.describe('/it/registry/drift — the re-read runs only while a request is open (cece5515)', () => {
  const request = (status: 'open' | 'closed') => ({
    id: 'c0ffee00-0000-4000-8000-000000000001',
    title: 'publish-workflow ship-a-change on boss-gcp',
    status,
    metadata: { verb: 'publish-workflow', args: ['ship-a-change'], opened_at: '2026-09-27T12:00:00Z' },
    steps: [],
  });

  test('polls every 10 s while one is open, and stops once none is', async ({ page }) => {
    await page.clock.install({ time: at(7) });
    let reads = 0;
    let status: 'open' | 'closed' = 'open';
    await install(page, {
      drift: { data: [packet([ADRIFT])], total: 1 },
      requests: (r) => {
        reads += 1;
        return json(r, { data: [request(status)], total: 1 });
      },
    });
    await mountPage(page, PAGE, { titleMatch: new RegExp(TITLE) });
    // The page has drawn the open request, so the effect has seen it.
    await expect(page.locator('.ap-pending')).toHaveCount(1);
    expect(reads).toBe(1);

    // Open: the next read lands on the 10 s tick.
    await page.clock.runFor(10_000);
    await expect.poll(() => reads).toBe(2);

    // The answer lands: the tick after reads it closed, and then no more.
    status = 'closed';
    await page.clock.runFor(10_000);
    await expect.poll(() => reads).toBe(3);
    await expect(page.locator('.ap-pending')).toHaveCount(0);
    await page.clock.runFor(60_000);
    expect(reads).toBe(3);
  });

  test('with nothing open, nothing polls', async ({ page }) => {
    await page.clock.install({ time: at(7) });
    let reads = 0;
    await install(page, {
      drift: { data: [packet([ADRIFT])], total: 1 },
      requests: (r) => {
        reads += 1;
        return json(r, { data: [request('closed')], total: 1 });
      },
    });
    await mountPage(page, PAGE, { titleMatch: new RegExp(TITLE) });
    await expect(page.locator('.ap-answer')).toHaveCount(1);
    await expect(page.locator('.ap-pending')).toHaveCount(0);
    await page.clock.runFor(60_000);
    expect(reads).toBe(1);
  });
});
