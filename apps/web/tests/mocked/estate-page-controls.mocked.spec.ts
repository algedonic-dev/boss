// /it/estate — every control the page renders, pinned (page audit
// 2cff1d6e, step `test`).
//
// estate-page.mocked.spec.ts pins THE LOOPS section (gap 10, 0d9b2960):
// each loop row's cells, its outcome colour, its open packet and a
// failed jobs read per cell. Before this spec the rest of the page — the
// machines, the observed-vs-declared rows, the verdict, the dev door,
// the loading line, the refresh timer and the loops' links as links —
// was reached only by the two crawls, under the mock's `[]` catch-all:
// route-smoke saw a header-only table, outage-crawl saw the failure
// lines, and neither read a word the page says.
//
// The inventory this spec covers, measured on origin/main on 2026-09-25
// (the measure step's controls_md was read on 2026-09-23, before the
// loops section landed with its links):
//   links     4 kinds — a loop's newest finished packet, a loop's open
//                        packet; both land on the job detail surface.
//                        Up to 2 per loop row: 6 declared loops plus one
//                        ops-request row per host declaring `ops-runner`.
//                        Since gap 9 (48ef9961), an open estate alarm in
//                        the list at the head of section 01, and the same
//                        alarm beside the verdict of its own series — both
//                        on the job detail surface too
//   buttons   0         — no manual refresh
//   forms     0, inputs 0
//   snippets  3         — the dev door's copyable command lines
//   reads     4 estate reads (the fourth, the host comparisons scoped
//             since gap 1 — 2d8d983b — and grouped per host since
//             725532ab), plus 2 jobs reads per loop row. Since gap 4
//             (75027a93) the observations are one read PER SERIES — the
//             cluster's, then per host its host and host-units series —
//             and the cluster comparison is read by its scope: 4 + 2 per
//             host. Plus one jobs read of the open estate alarms (48ef9961),
//             and since gap 11 (e0e183fb) two of the DNS zone readings —
//             the newest closed ones, whole, and the open ones — whose
//             zone line, open reading, zone alarm and tunnel-routes line
//             each link to their packet.
//   writes    0
//   timer     1         — every read again each 60 s
//
// THIS STEP PINS WHAT THE PAGE DOES TODAY; it does not fix the gaps.
// Where today's behaviour IS a filed gap, the test says so in its name
// ("CURRENT, gap N (item)") and asserts the current paint, so the car
// that fixes the gap has to flip that assertion — the fix is visible in
// this file rather than passing beside a spec that never noticed.

import { expect, test, type Page, type Route } from './_test';
import { mountPage } from './_helpers';
import { FAILURE_MARKER } from './_routes';
import { installSmokeMocks } from './_smokeMocks';
import { parseRoute } from '../../src/router';
import { ROUTE_CATALOG } from '../../src/shell/nav-catalog';
import { readFileSync } from 'node:fs';

const DEV_DOOR_DECLARATION = JSON.parse(readFileSync(new URL('../../../../infra/estate/dev-door.json', import.meta.url), 'utf8')) as unknown;

const PATH = ROUTE_CATALOG['system-estate'].path;
const TITLE = { titleMatch: /The estate/ };

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

/// The page's estate reads, as estate.ts spells them — the query is part
/// of the match, so a read that changes shape fails here.
const NODES_READ = /\/api\/estate\/nodes$/;
/// Every observation read, answered by one handler as the reader would:
/// by scope and host, newest first, `total` counting the series (gap 4,
/// 75027a93). An UNSCOPED read is answered too — the page must never
/// send one, and the reads test below says so.
const OBS_READ = /\/api\/estate\/observations\?/;
/// The cluster verdict, read by its scope (75027a93).
const CMP_READ = /\/api\/estate\/comparisons\?scope=kubernetes-nodes&limit=1$/;
/// The host series, read on its own (gap 1, 2d8d983b): forge compares
/// every 15 minutes and boss-gcp once a day, so the unscoped page of 20
/// almost never holds boss-gcp's row — and grouped per host on the
/// server (725532ab), because even scoped, 50 of 768 rows were forge's.
const HOST_CMP_READ = /\/api\/estate\/comparisons\?scope=host&latest_per=host&limit=50$/;
/// The instance volumes' judged series (backlog 21ee3b4e).
const VOLUMES_READ = /\/api\/estate\/comparisons\?scope=instance-volumes&limit=10$/;
/// The loops' reads (two per row).
const LOOPS_READ = /\/api\/jobs\?kind=(maintenance-|ops-request)/;
/// The open estate alarms (gap 9, 48ef9961), keyed as the raiser keys them.
const ALARMS_READ = /\/api\/jobs\?kind=backlog-item&status=open&metadata_has=estate_finding&limit=50$/;

/// Stamps relative to the test's own clock, taken when the read is
/// answered (the estate-page spec's reason: a stamp taken at file load
/// drifts under a slow worker).
const ago = (minutes: number): string => new Date(Date.now() - minutes * 60_000).toISOString();

/// Three live machines and one retired. Addresses are TEST-NET-1
/// (RFC 5737), never an estate address. w-1 declares nothing but its
/// role, so every optional cell reads its dash.
const NODES = [
  { id: 'forge', label: 'forge', role: 'forge', roles: ['cluster-operator', 'ops-runner'], address: '192.0.2.15', cpu: 16, memory_gb: 30, disk_gb: 500, notes: 'the forge host', retired: false },
  { id: 'boss-gcp', label: 'boss-gcp', role: 'gateway-host', roles: ['ops-runner'], address: '192.0.2.20', cpu: 4, memory_gb: 15, disk_gb: 60, notes: null, retired: false },
  { id: 'w-1', label: 'w-1', role: 'talos-worker', roles: [], address: null, cpu: null, memory_gb: null, disk_gb: null, notes: null, retired: false },
  { id: 'old-1', label: 'old-1', role: 'talos-worker', roles: [], address: '192.0.2.99', cpu: 2, memory_gb: 4, disk_gb: 20, notes: 'decommissioned', retired: true },
];

/// The reader serves newest-first envelopes `{payload}`.
const envelope = (payload: Record<string, unknown>) => ({ payload });

/// Every series the log holds, each at its own cadence — the live shape
/// measured 2026-09-27, each row naming only the host it was taken on
/// (observe-host.sh, observe-units.sh):
///   door              every 2 min — unrendered, and the fastest, so it
///                     spends an unscoped page (the live one held 6)
///   kubernetes-nodes  every 15 min, newest 3 min old
///   host forge        every 15 min, newest 5 min old (older rows 211G,
///                     212G must not show)
///   host boss-gcp     DAILY, newest 200 min old — 13 G under a 17 G floor
///   host-units forge  every 5 min, newest 1 min old, one unit unhealthy
///   host-units boss-gcp every 5 min, but the newest is an HOUR old:
///                     quiet past 3x its cadence (e1eb34bc)
const series = (
  scope: string, observer: string, minutes: readonly number[], nodes: (i: number) => readonly Record<string, unknown>[],
) => minutes.map((m, i) => envelope({ scope, observer, observed_at: ago(m), nodes: nodes(i) }));

const unit = (name: string, healthy: boolean) => ({ unit: name, healthy });
const FORGE_UNITS = [unit('boss-train.service', false), unit('boss-train.timer', true), unit('estate-observe-host.timer', true)];
const GCP_UNITS = [unit('boss-gcp-converge.timer', true), unit('boss-gcp-converge.service', true)];

const observations = () => [
  ...series('door', 'boss-door-observe', Array.from({ length: 20 }, (_, i) => 0.5 + i * 2), () => [{ id: 'dev-ssh' }]),
  ...series('kubernetes-nodes', 'boss-estate-observe', [3, 18, 33], () => [{ id: 'cp-1', disk_free_gb: 40 }, { id: 'w-1' }]),
  // Each host reading carries what observe-host.sh measures — the live
  // 2026-09-28 shape: one GiB more memory than each host declares.
  ...series('host', 'boss-estate-observe-host', [5, 20, 35], (i) => [
    { id: 'forge', address: '192.0.2.15', cpu: 16, memory_gb: 31, disk_gb: 480, disk_free_gb: 210 + i },
  ]),
  ...series('host', 'boss-estate-observe-host', [200, 200 + 1440, 200 + 2880], () => [
    { id: 'boss-gcp', address: '192.0.2.20', cpu: 4, memory_gb: 16, disk_gb: 60, disk_free_gb: 13 },
  ]),
  ...series('host-units', 'boss-estate-observe-units', [1, 6, 11], () => [{ id: 'forge', healthy: false, units: FORGE_UNITS }]),
  ...series('host-units', 'boss-estate-observe-units', [60, 65, 70], () => [{ id: 'boss-gcp', healthy: true, units: GCP_UNITS }]),
];

type Envelope = Readonly<{ payload: Record<string, unknown> }>;
const stamp = (e: Envelope): number => Date.parse(String(e.payload.observed_at));
const firstNode = (e: Envelope): unknown => (e.payload.nodes as { id?: unknown }[] | undefined)?.[0]?.id;

/// The reader's answer to an observation read: the rows of the asked
/// scope and host (a host series is keyed by its first node's id, as
/// jobs.rs reads it), newest first, `limit` of them, and the total.
function readerAnswer(url: URL, rows: readonly Envelope[]): { data: Envelope[]; total: number } {
  const scope = url.searchParams.get('scope');
  const host = url.searchParams.get('host');
  const limit = Number(url.searchParams.get('limit') ?? '5');
  const matching = rows
    .filter((e) => (scope === null || e.payload.scope === scope) && (host === null || firstNode(e) === host))
    .sort((a, b) => stamp(b) - stamp(a));
  return { data: matching.slice(0, limit), total: matching.length };
}

const ZERO = {
  observed: 5, participating_declared: 5, observed_not_declared: 0,
  declared_not_observed: 0, drift: 0, disk_tight: 0, disk_unmeasured: 0,
};

/// The cluster's newest comparison, as its scoped read answers it.
const comparisons = (cluster: Record<string, unknown> = {}, findings: Record<string, unknown> = {}) => [
  envelope({ scope: 'kubernetes-nodes', observed_at: ago(3), counts: { ...ZERO, ...cluster }, findings }),
];

/// A host comparison as compare_host shapes it (estate_compare.rs):
/// stamped with its host, four counts, no declared total.
const hostCmp = (host: string, minutes: number, counts: Record<string, number> = {}, findings: Record<string, unknown> = {}) =>
  envelope({
    scope: 'host', observed_at: ago(minutes), host,
    counts: { observed: 1, observed_not_declared: 0, drift: 0, disk_tight: 0, ...counts },
    findings,
  });

/// A drift finding in compare_host's own shape: the machine, and each
/// field that differs with both sides.
const memoryDrift = (id: string, declared: number, observed: number) => ({
  drift: [{ id, fields: { memory_gb: { declared, observed } } }],
});

/// The live shape measured on 2026-09-23, as the grouped read serves it
/// (725532ab: `latest_per=host`, one row per host, `total` counting
/// hosts): forge drifted (memory declared 30, observed 31), and
/// boss-gcp's daily row short of disk (13 G free against a 17 G floor)
/// AND drifted (memory declared 15, observed 16 — re-read 2026-09-28).
const hostComparisons = () => ({
  data: [
    hostCmp('forge', 2, { drift: 1 }, memoryDrift('forge', 30, 31)),
    hostCmp('boss-gcp', 200, { drift: 1, disk_tight: 1 }, memoryDrift('boss-gcp', 15, 16)),
  ],
  total: 2,
});

/// The open estate alarms (gap 9, 48ef9961), as GET /api/jobs lists them:
/// the live one of 2026-09-28 — boss-gcp short of disk, on the host
/// series — and a door alarm, which has no verdict line on this page.
const DISK_ALARM = 'd3c7eada-0000-0000-0000-000000000009';
const DOOR_ALARM = 'd00d0000-0000-0000-0000-00000000000a';
const alarm = (id: string, title: string, minutes: number, md: Record<string, unknown>) =>
  ({ id, kind: 'backlog-item', status: 'open', title, opened_at: ago(minutes), metadata: { area: 'estate', ...md } });
const openAlarms = () => ({
  data: [
    alarm(DISK_ALARM, 'ESTATE ALARM: disk_tight:boss-gcp persisted 3 consecutive comparisons', 90,
      { estate_finding: 'disk_tight:boss-gcp', scope: 'host', host: 'boss-gcp' }),
    alarm(DOOR_ALARM, 'ESTATE ALARM: door:dev-ssh:edge — the edge half (dev.algedonic.dev) is dark past its 10-minute band', 12,
      { estate_finding: 'door:dev-ssh:edge', scope: 'door' }),
  ],
  total: 2,
});

/// The instance volumes as compare_volumes records them (backlog
/// 21ee3b4e, incident d3c0a67c): the database volume under its floor,
/// boss-auth with room, and boss-files unread — each row judged, the
/// floor on the row. Three readings fifteen minutes apart.
const GIB = 1024 ** 3;
const VOLUME_ALARM = 'd3c0a67c-0000-0000-0000-00000000000b';
const volumeRows = () => [
  { id: 'boss/pgdata-postgres-0', namespace: 'boss', claim: 'pgdata-postgres-0', volume: 'pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56',
    capacity_bytes: 30 * GIB, used_bytes: 25 * GIB, free_bytes: 5 * GIB, floor_bytes: 6 * GIB, tight: true },
  { id: 'boss/boss-auth', namespace: 'boss', claim: 'boss-auth', volume: 'pvc-auth',
    capacity_bytes: GIB, used_bytes: GIB / 10, free_bytes: GIB - GIB / 10, floor_bytes: GIB / 2, tight: false },
  { id: 'boss/boss-files', namespace: 'boss', claim: 'boss-files', volume: 'pvc-files',
    capacity_bytes: null, used_bytes: null, free_bytes: null, tight: null,
    unread: 'the kubelet stats of w-2 could not be read' },
];
const volumeReadings = () => ({
  data: [4, 19, 34].map((m) => envelope({
    scope: 'instance-volumes', observer: 'boss-estate-observe-volumes', observed_at: ago(m),
    counts: { volumes: 3, disk_tight: 1, disk_unmeasured: 1 }, findings: {}, volumes: volumeRows(),
  })),
  total: 3,
});

/// Two loop packets, so the loops table carries both link kinds.
const WATCHDOG_DONE = 'c0c0c0c0-0000-0000-0000-000000000003';
const UNITS_OPEN = 'e1e1e1e1-0000-0000-0000-000000000008';
const loopAnswer = (url: URL) => {
  const kind = url.searchParams.get('kind');
  const open = url.searchParams.get('status') === 'open';
  if (!open && kind === 'maintenance-cluster-watchdog') {
    return [{ id: WATCHDOG_DONE, status: 'closed', metadata: { outcome: 'completed', closed_at: ago(3) }, steps: [] }];
  }
  if (open && kind === 'maintenance-estate-observe-units') {
    return [{ id: UNITS_OPEN, status: 'open', opened_at: ago(2), metadata: {}, steps: [] }];
  }
  return [];
};

type Answer = 'fixture' | 'empty' | 'down';
type Reads = Readonly<{
  nodes?: Answer; cmp?: Answer; host?: Answer;
  /** The cluster's observation series. */
  cluster?: Answer;
  /** Every host's host and host-units series. */
  series?: Answer;
  /** The open estate alarms. */
  alarms?: Answer;
  /** The instance volumes' series (21ee3b4e). */
  volumes?: Answer;
  cmpBody?: () => unknown; hostBody?: () => unknown; alarmsBody?: () => unknown;
}>;

type Seen = Record<'nodes' | 'cluster' | 'series' | 'cmp' | 'host', number>;

/// Every read the page makes, answered; `down` is a 503, `empty` an
/// empty list (an observation read's empty is the reader's own
/// `{data: [], total: 0}`). Returns the counts of each estate read as it
/// arrives.
async function install(page: Page, reads: Reads = {}): Promise<Seen> {
  const seen: Seen = { nodes: 0, cluster: 0, series: 0, cmp: 0, host: 0 };
  await installSmokeMocks(page);
  await page.route('**/instance-config/dev-door.json', (r) => json(r, DEV_DOOR_DECLARATION));
  const answer = (key: keyof Seen, mode: Answer, body: () => unknown) => (r: Route) => {
    seen[key] += 1;
    if (mode === 'down') return json(r, { error: 'estate upstream unavailable' }, 503);
    return json(r, mode === 'empty' ? [] : body());
  };
  await page.route(NODES_READ, answer('nodes', reads.nodes ?? 'fixture', () => NODES));
  await page.route(OBS_READ, (r) => {
    const url = new URL(r.request().url());
    const key = url.searchParams.get('scope') === 'kubernetes-nodes' ? 'cluster' : 'series';
    seen[key] += 1;
    const mode = reads[key] ?? 'fixture';
    if (mode === 'down') return json(r, { error: 'estate upstream unavailable' }, 503);
    return json(r, readerAnswer(url, mode === 'empty' ? [] : observations()));
  });
  await page.route(CMP_READ, answer('cmp', reads.cmp ?? 'fixture', reads.cmpBody ?? (() => comparisons())));
  await page.route(HOST_CMP_READ, answer('host', reads.host ?? 'fixture', reads.hostBody ?? hostComparisons));
  await page.route(LOOPS_READ, (r) => {
    const data = loopAnswer(new URL(r.request().url()));
    return json(r, { data, total: data.length });
  });
  await page.route(VOLUMES_READ, (r) => {
    const mode = reads.volumes ?? 'fixture';
    if (mode === 'down') return json(r, { error: 'estate upstream unavailable' }, 503);
    return json(r, mode === 'empty' ? { data: [], total: 0 } : volumeReadings());
  });
  await page.route(ALARMS_READ, (r) => {
    const mode = reads.alarms ?? 'fixture';
    if (mode === 'down') return json(r, { error: 'jobs upstream unavailable' }, 503);
    return json(r, mode === 'empty' ? { data: [], total: 0 } : (reads.alarmsBody ?? openAlarms)());
  });
  return seen;
}

const section = (page: Page) => page.locator('.estate-section');
const machines = (page: Page) => page.locator('table.estate-table:not(.estate-loops)');
const obsRows = (page: Page) => page.locator('.estate-obs .estate-obs-row');
const obsRow = (page: Page, scope: string) =>
  obsRows(page).filter({ has: page.locator('.estate-scope', { hasText: new RegExp(`^${scope}$`, 'i') }) });
/// One line per host the host series names (gap 1, 2d8d983b).
const hostRows = (page: Page) => obsRow(page, 'host comparison');
const loops = (page: Page) => page.locator('table.estate-loops');

/// parseRoute reads `window.location.search` for two routes; this is
/// Node, so give it the one field it reads (interaction-crawl and the
/// manual spec do the same).
function route(path: string): ReturnType<typeof parseRoute> {
  (globalThis as { window?: unknown }).window = { location: { search: '', pathname: path } };
  return parseRoute(path);
}

test.describe('/it/estate — the chrome, the loading line and the reads', () => {
  test('the page is the catalogued Estate surface, with its header and five sections in order', async ({ page }) => {
    expect(route(PATH)).toEqual({ kind: 'systemEstate' });
    await install(page);
    await mountPage(page, PATH, TITLE);

    await expect(page.locator('.exec-eyebrow')).toHaveText('IT · Hardware');
    await expect(page.locator('h1').first()).toHaveText('The estate');
    await expect(page.getByText('Declared beside observed — what we meant to have, what a look found, and the difference')).toBeVisible();
    await expect(section(page)).toHaveText([
      '00 — THE MACHINES',
      '01 — OBSERVED vs DECLARED',
      '02 — THE LOOPS',
      '03 — THE EDGE',
      '04 — THE DEV WORKSPACE',
    ]);
  });

  test('the page says it is reading until the registry answers', async ({ page }) => {
    await install(page);
    let release: () => void = () => {};
    const held = new Promise<void>((r) => { release = r; });
    await page.route(NODES_READ, async (r) => { await held; await json(r, NODES); });
    await mountPage(page, PATH, TITLE);

    await expect(page.locator('.estate-root > p.estate-quiet')).toHaveText('Reading the registry…');
    await expect(section(page)).toHaveCount(0);
    release();
    await expect(machines(page).locator('tbody tr')).toHaveCount(3);
    await expect(page.getByText('Reading the registry…')).toHaveCount(0);
  });

  // Gap 4 (75027a93), FIXED. This pin asserted the CURRENT reads: both
  // event reads one unscoped page of 20 rows across every scope. Now each
  // series the page draws is its own read — the cluster's observations
  // and comparison by scope, and per host (the two declared outside the
  // cluster) its host and host-units series by scope and host — beside
  // the host comparisons, scoped (gap 1, 2d8d983b) and grouped per host
  // (725532ab). No read is unscoped.
  test('gap 4: one read per rendered series, every observation read scoped — none unscoped; no write, no button, no form', async ({ page }) => {
    const sent: string[] = [];
    page.on('request', (req) => {
      const u = new URL(req.url());
      if (u.pathname.startsWith('/api/estate') || u.pathname.startsWith('/api/jobs')) {
        sent.push(`${req.method()} ${u.pathname}${u.search}`);
      }
    });
    await install(page);
    await mountPage(page, PATH, TITLE);
    await expect(loops(page).locator('a')).toHaveCount(2);

    const estate = sent.filter((s) => s.includes('/api/estate')).sort();
    expect(estate).toEqual([
      'GET /api/estate/comparisons?scope=host&latest_per=host&limit=50',
      'GET /api/estate/comparisons?scope=instance-volumes&limit=10',
      'GET /api/estate/comparisons?scope=kubernetes-nodes&limit=1',
      'GET /api/estate/nodes',
      'GET /api/estate/observations?scope=host&host=boss-gcp&limit=10',
      'GET /api/estate/observations?scope=host&host=forge&limit=10',
      'GET /api/estate/observations?scope=host-units&host=boss-gcp&limit=10',
      'GET /api/estate/observations?scope=host-units&host=forge&limit=10',
      'GET /api/estate/observations?scope=kubernetes-nodes&limit=10',
    ]);
    // Six declared loops plus the two ops-runner hosts, two reads each,
    // the open estate alarms once (gap 9, 48ef9961), and the zone
    // readings, closed and open (gap 11, e0e183fb) — the tunnel routes
    // ride the cluster converge's loop read, so they cost none.
    expect(sent.filter((s) => s.startsWith('GET /api/jobs?kind=')).length).toBe((6 + 2) * 2 + 1 + 2);
    expect(sent.filter((s) => s.startsWith('GET /api/jobs?kind=backlog-item'))).toEqual([
      'GET /api/jobs?kind=backlog-item&status=open&metadata_has=estate_finding&limit=50',
    ]);
    expect(sent.filter((s) => s.startsWith('GET /api/jobs?kind=dns-zone-observation')).sort()).toEqual([
      'GET /api/jobs?kind=dns-zone-observation&status=closed&limit=7&full=true',
      'GET /api/jobs?kind=dns-zone-observation&status=open',
    ]);
    expect(sent.filter((s) => !s.startsWith('GET ')), 'the page wrote').toEqual([]);

    const root = page.locator('.estate-root');
    await expect(root.locator('button')).toHaveCount(0);
    await expect(root.locator('form')).toHaveCount(0);
    await expect(root.locator('input, select, textarea')).toHaveCount(0);
  });

  test('every 60 s the page reads the estate again and paints the new answer', async ({ page }) => {
    await page.clock.install();
    const seen = await install(page);
    await mountPage(page, PATH, TITLE);
    await expect(machines(page).locator('tbody tr')).toHaveCount(3);
    // Counted from what the mount left, never from one: an installed
    // clock still runs on real time, so on a starved runner the page's
    // own 60 s poll had already read again before the count was taken
    // (61 real seconds held after mount: every count 2, not 1; backlog
    // 3027f808). That the mount reads each exactly once is pinned by
    // the no-clock test above, which counts every request the page sent.
    // -1 while a tick's four page-wide reads are only partly in (the
    // per-host series reads follow them, two per host).
    const sameEach = (s: typeof seen): number =>
      s.nodes === s.cluster && s.cluster === s.cmp && s.cmp === s.host ? s.nodes : -1;
    let before = -1;
    await expect.poll(() => (before = sameEach(seen))).toBeGreaterThan(0);

    // The next answer declares one more machine; the page must show it.
    await page.route(NODES_READ, (r) => {
      seen.nodes += 1;
      return json(r, [...NODES, { id: 'w-2', label: 'w-2', role: 'talos-worker', roles: [], retired: false }]);
    });
    await page.clock.runFor(60_000);
    // The four reads go out together, each once more per tick.
    await expect.poll(() => sameEach(seen)).toBeGreaterThan(before);
    await expect(machines(page).locator('tbody tr')).toHaveCount(4);
    await expect(machines(page).locator('td.estate-id').last()).toHaveText('w-2');
    await expect(page.locator('.estate-machine-count')).toHaveText('5 machines declared · 4 active · 1 retired');
  });
});

test.describe('/it/estate — 00 THE MACHINES', () => {
  // Gap 12 (d6d39f60): the registry total conserves active and retired
  // machines, even though only the active rows are drawn.
  // Gap 7 (ab3c54d7), FIXED: this test pinned the cells verbatim as the
  // declared value alone ('30G'), while the subtitle promised declared
  // beside observed. Each value cell now reads the declared value, then
  // what the newest reading of that machine's own series saw.
  test('gap 12: one row per active machine and the declared, active and retired counts agree', async ({ page }) => {
    await install(page);
    await mountPage(page, PATH, TITLE);

    await expect(machines(page).locator('thead th')).toHaveText(['machine', 'role', 'address', 'cpu', 'mem', 'disk']);
    await expect(page.locator('p.estate-legend')).toHaveText(
      'Each value is what the registry declares, then what the newest observation saw. A field a comparison names as drift reads amber.',
    );
    const rows = machines(page).locator('tbody tr');
    await expect(rows).toHaveCount(3);
    await expect(rows.nth(0).locator('td')).toHaveText([
      'forge', 'forge · cluster-operator · ops-runner',
      '192.0.2.15 seen 192.0.2.15', '16 seen 16', '30G seen 31G · drifted', '500G seen 480G',
    ]);
    await expect(rows.nth(1).locator('td')).toHaveText([
      'boss-gcp', 'gateway-host · ops-runner',
      '192.0.2.20 seen 192.0.2.20', '4 seen 4', '15G seen 16G · drifted', '60G seen 60G',
    ]);
    // A machine that declares nothing reads a dash, never a zero — on
    // both sides: w-1's cluster reading carries no capacity.
    await expect(rows.nth(2).locator('td')).toHaveText(['w-1', 'talos-worker', '— seen —', '— seen —', '— seen —', '— seen —']);
    // The notes ride as the row's tooltip; no notes, an empty one.
    await expect(rows.nth(0)).toHaveAttribute('title', 'the forge host');
    await expect(rows.nth(1)).toHaveAttribute('title', '');

    await expect(page.locator('.estate-root').getByText('old-1')).toHaveCount(0);
    await expect(page.locator('.estate-machine-count')).toHaveText('4 machines declared · 3 active · 1 retired');
  });

  // Gap 7 (ab3c54d7): the pin the finding asked for — a mocked drift
  // finding, and the one field it names marked on the machine it names.
  test('gap 7: the field a drift finding names is marked on its machine, amber, with both sides; no other cell is', async ({ page }) => {
    await install(page);
    await mountPage(page, PATH, TITLE);

    const forge = machines(page).locator('tbody tr').nth(0);
    const marked = machines(page).locator('td[data-drift]');
    await expect(marked).toHaveCount(2);
    await expect(forge.locator('td[data-drift]')).toHaveAttribute('data-drift', 'memory_gb');
    await expect(forge.locator('td[data-drift]')).toHaveAttribute('title', 'drift: declared 30G, observed 31G');
    await expect(forge.locator('td[data-drift] .estate-seen')).toHaveClass(/\bestate-drift\b/);
    // An agreeing field stays grey.
    await expect(forge.locator('td').nth(3).locator('.estate-seen')).not.toHaveClass(/\bestate-drift\b/);
    // And the verdict names the machine and the field, not only a count.
    await expect(hostRows(page).filter({ hasText: 'forge:' }).locator('span:nth-child(2)'))
      .toHaveText('forge: 1 drifted from declaration (forge memory_gb 30 → 31)');
  });

  test('gap 7: a machine whose series failed reads unread, and one missing from its reading reads not seen — never a guess', async ({ page }) => {
    await install(page, {
      series: 'down',
      cmpBody: () => comparisons({ drift: 0 }),
      hostBody: () => ({ data: [], total: 0 }),
    });
    await page.route(/\/api\/estate\/observations\?scope=kubernetes-nodes&/, (r) => json(r, {
      data: [envelope({ scope: 'kubernetes-nodes', observer: 'boss-estate-observe', observed_at: ago(3), nodes: [{ id: 'cp-1' }] })],
      total: 1,
    }));
    await mountPage(page, PATH, TITLE);

    const rows = machines(page).locator('tbody tr');
    await expect(rows.nth(0).locator('td').nth(4)).toHaveText('30G unread');
    await expect(rows.nth(2).locator('td').nth(4)).toHaveText('— not seen');
    await expect(machines(page).locator('td[data-drift]')).toHaveCount(0);
  });

  test('gap 12: an empty registry states its zero counts and remains distinct from failure', async ({ page }) => {
    await install(page, { nodes: 'empty' });
    await mountPage(page, PATH, TITLE);

    await expect(machines(page).locator('thead th')).toHaveCount(6);
    await expect(machines(page).locator('tbody tr')).toHaveCount(0);
    await expect(page.locator('.estate-machine-count')).toHaveText('0 machines declared · 0 active · 0 retired');
    await expect(loops(page).locator('tbody tr')).toHaveCount(6);
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
  });

  test('gap 12: a retired-only registry conserves its one machine while drawing no active row', async ({ page }) => {
    await install(page);
    await page.route(NODES_READ, (r) => json(r, [NODES[3]]));
    await mountPage(page, PATH, TITLE);
    await expect(page.locator('.estate-machine-count')).toHaveText('1 machine declared · 0 active · 1 retired');
    await expect(machines(page).locator('tbody tr')).toHaveCount(0);
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
  });

  test('an unreachable registry says so in the page\'s words, and draws no table of machines', async ({ page }) => {
    await install(page, { nodes: 'down' });
    await mountPage(page, PATH, TITLE);

    const fail = page.locator(`p.estate-fail${FAILURE_MARKER}`).first();
    await expect(fail).toHaveText(
      'The registry did not answer: /api/estate/nodes: HTTP 503. This page refuses to guess — an unreachable registry is not an empty estate.',
    );
    await expect(machines(page)).toHaveCount(0);
    await expect(page.locator('.estate-machine-count')).toHaveCount(0);
    // The loops still read: the runner row kept, unfiltered, with no host.
    await expect(loops(page).locator('tbody tr')).toHaveCount(7);
    await expect(loops(page).locator('tr[data-loop="ops-request"] td').nth(1)).toHaveText('not named on the packet');
  });
});

// THE INSTANCE VOLUMES (backlog 21ee3b4e, incident d3c0a67c): the
// database volume filled on 2026-10-01 and no surface showed any claim.
test.describe('/it/estate — 00 THE MACHINES: the instance volumes', () => {
  const volumes = (page: Page) => page.locator('table.estate-volume-table tbody tr');

  test('every claim sits under the machines with the comparator\'s verdict; a tight one and an unread one read amber', async ({ page }) => {
    await install(page, {
      alarmsBody: () => ({
        data: [
          ...openAlarms().data,
          alarm(VOLUME_ALARM, 'ESTATE ALARM: disk_tight:boss/pgdata-postgres-0 persisted 3 consecutive comparisons', 20,
            { estate_finding: 'disk_tight:boss/pgdata-postgres-0', scope: 'instance-volumes' }),
        ],
        total: 3,
      }),
    });
    await mountPage(page, PATH, TITLE);

    // Inside section 00, after the machines and before section 01.
    await expect(page.locator('.estate-section').nth(1)).toHaveText('01 — OBSERVED vs DECLARED');
    await expect(page.locator('.estate-volumes-head span').first()).toHaveText(
      'Instance volumes — 3 claims read by boss-estate-observe-volumes',
    );
    await expect(page.locator('.estate-volumes-head .estate-when')).toHaveText('4m ago · every 15m');
    await expect(page.locator('table.estate-volume-table thead th')).toHaveText([
      'claim', 'volume', 'free', 'declared capacity · report only',
    ]);
    await expect(volumes(page)).toHaveCount(3);
    await expect(volumes(page).nth(0).locator('td')).toHaveText([
      'boss/pgdata-postgres-0', 'pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56', '5.0G free of 30.0G — under its 6.0G floor alarm',
      'Capacity intent unknown: no declared capacity comparison recorded · desired unknown · requested unread',
    ]);
    await expect(volumes(page).nth(0)).toHaveAttribute('data-state', 'tight');
    await expect(volumes(page).nth(0).locator('td').nth(2)).toHaveClass(/\bestate-drift\b/);
    await expect(volumes(page).nth(0).locator('a.estate-alarm-link')).toHaveAttribute('href', `/ux/jobs/${VOLUME_ALARM}`);
    await expect(volumes(page).nth(1).locator('td').nth(2)).toHaveText('0.9G free of 1.0G · floor 0.5G');
    await expect(volumes(page).nth(1).locator('td').nth(2)).toHaveClass(/\bestate-ok\b/);
    // A claim the forge could not read says unread and why — never fine.
    await expect(volumes(page).nth(2).locator('td').nth(2)).toHaveText('unread: the kubelet stats of w-2 could not be read');
    await expect(volumes(page).nth(2)).toHaveAttribute('data-state', 'unread');
    await expect(volumes(page).nth(2).locator('td').nth(2)).toHaveClass(/\bestate-drift\b/);
    // Legacy filesystem evidence carries no capacity assignment or request.
    // Its healthy floor must not turn the independent intent column green.
    const unknownCapacity = 'Capacity intent unknown: no declared capacity comparison recorded · desired unknown · requested unread';
    await expect(volumes(page).locator('[data-capacity-intent]')).toHaveText([
      unknownCapacity, unknownCapacity, unknownCapacity,
    ]);
    await expect(volumes(page).locator('td[data-capacity-intent="unknown"].estate-drift')).toHaveCount(3);
    await expect(volumes(page).locator('[data-capacity-intent] a')).toHaveCount(0);
  });

  test('a failed volume read says so in the page\'s words, never as no claims', async ({ page }) => {
    await install(page, { volumes: 'down' });
    await mountPage(page, PATH, TITLE);
    await expect(page.locator(`.estate-volumes p.estate-fail${FAILURE_MARKER}`)).toHaveText(
      'Volume readings unavailable: /api/estate/comparisons?scope=instance-volumes&limit=10: HTTP 503',
    );
    await expect(volumes(page)).toHaveCount(0);
    await expect(machines(page).locator('tbody tr')).toHaveCount(3);
  });

  test('a series never recorded says so', async ({ page }) => {
    await install(page, { volumes: 'empty' });
    await mountPage(page, PATH, TITLE);
    await expect(page.locator('p.estate-volumes-none')).toHaveText('Instance volumes: no volume reading recorded yet.');
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
  });
});

test.describe('/it/estate — 01 OBSERVED vs DECLARED', () => {
  // Gap 2 (3d1678ba), FIXED: the newest row per SCOPE won, so this test
  // asserted boss-gcp's older host row — 13G free — never showed. The
  // host series is keyed per host, one line each — and since gap 4
  // (75027a93) each host's series is its own read, so boss-gcp's daily
  // row no longer depends on surviving an unscoped page.
  // Gap 8 (e1eb34bc), FIXED: this test asserted "today" for a 3-hour-old
  // row and a 3-minute-old one alike. Each series' age now reads against
  // its own measured cadence.
  test('the cluster row and one host row per host, free space per machine, each aged against its own cadence', async ({ page }) => {
    await install(page);
    await mountPage(page, PATH, TITLE);

    // The cluster scope, per host its reading and its units, the cluster
    // comparison, and one host comparison per host (gap 1, pinned below).
    await expect(obsRows(page)).toHaveCount(8);
    await expect(obsRow(page, 'kubernetes-nodes').locator('span')).toHaveText([
      'kubernetes-nodes',
      '2 machines seen by boss-estate-observe — cp-1: 40G free — w-1: free space unread',
      '3m ago · every 15m',
    ]);
    // Each host's NEWEST reading, in host order: forge's older 211G row
    // is hidden by its newer one, and boss-gcp's daily row — hours older
    // — keeps a line of its own, FRESH against its own daily cadence.
    const hosts = obsRow(page, 'host');
    await expect(hosts).toHaveCount(2);
    await expect(hosts.nth(0).locator('span')).toHaveText([
      'host',
      'boss-gcp: 13G free — seen by boss-estate-observe-host',
      '3.3h ago · every 24h',
    ]);
    await expect(hosts.nth(1).locator('span')).toHaveText([
      'host',
      'forge: 210G free — seen by boss-estate-observe-host',
      '5m ago · every 15m',
    ]);
    await expect(page.locator('.estate-obs').getByText(/211G/)).toHaveCount(0);
    await expect(page.locator('.estate-obs .estate-when.estate-drift')).toHaveCount(1);
  });

  // Gap 8 (e1eb34bc): the pin the finding asked for — a stale row. The
  // boss-gcp units series ticks every five minutes and its newest
  // reading is an hour old: past three of its own cadences, which is
  // exactly when the estate alarm files it unobserved.
  test('gap 8: a series quiet past 3x its own cadence reads amber and says so; a fresh one stays grey', async ({ page }) => {
    await install(page);
    await mountPage(page, PATH, TITLE);

    const units = obsRow(page, 'host units');
    const stale = units.filter({ hasText: 'boss-gcp:' }).locator('.estate-when');
    await expect(stale).toHaveText('1h ago — quiet past 3× its 5m cadence');
    await expect(stale).toHaveClass(/\bestate-drift\b/);
    const fresh = units.filter({ hasText: 'forge:' }).locator('.estate-when');
    await expect(fresh).toHaveText('1m ago · every 5m');
    await expect(fresh).not.toHaveClass(/\bestate-drift\b/);
  });

  // Gap 3 (d5efb80d), FIXED: this test's parent asserted no "units" text
  // anywhere — the host-units series was fetched and discarded. Each
  // host's newest units reading is a line now, an unhealthy unit named.
  test('gap 3: each host\'s units — how many watched, and an unhealthy one named, in amber', async ({ page }) => {
    await install(page);
    await mountPage(page, PATH, TITLE);

    const units = obsRow(page, 'host units').locator('span:nth-child(2)');
    await expect(units).toHaveText([
      'boss-gcp: 2 units watched, all healthy',
      'forge: 3 units watched, 1 unhealthy: boss-train.service',
    ]);
    await expect(units.nth(0)).toHaveClass(/\bestate-ok\b/);
    await expect(units.nth(1)).toHaveClass(/\bestate-drift\b/);
  });

  // Gap 4 (75027a93): the pin the finding asked for — a slow series that
  // falls off an unscoped page. The fixture's unscoped answer is spent by
  // the two-minute door series and holds no boss-gcp host row; the page
  // never asks for it, and boss-gcp's daily reading still has its line.
  test('gap 4: a daily host reading that an unscoped page of 20 would not hold is still drawn', async ({ page }) => {
    const unscoped = readerAnswer(new URL('http://x/api/estate/observations?limit=20'), observations());
    expect(unscoped.data.filter((e) => e.payload.scope === 'host' && firstNode(e) === 'boss-gcp')).toEqual([]);
    expect(unscoped.data).toHaveLength(20);

    await install(page);
    await mountPage(page, PATH, TITLE);
    await expect(obsRow(page, 'host').filter({ hasText: 'boss-gcp:' }).locator('span').nth(1))
      .toHaveText('boss-gcp: 13G free — seen by boss-estate-observe-host');
  });

  test('gap 4: a series read that holds none of its series says the absence is the read\'s — never "no observation recorded"', async ({ page }) => {
    await install(page);
    // A reader that ignored ?host= answers forge's rows to boss-gcp's read.
    await page.route(/\/api\/estate\/observations\?scope=host&host=boss-gcp&/, (r) => json(r, {
      data: observations().filter((e) => e.payload.scope === 'host' && firstNode(e) === 'forge'),
      total: 989,
    }));
    await mountPage(page, PATH, TITLE);

    await expect(obsRow(page, 'host').filter({ hasText: 'boss-gcp:' }).locator('span').nth(1)).toHaveText(
      'boss-gcp: none of the 3 rows this read returned is this series\' (the read counted 989)',
    );
  });

  test('gap 2: a host whose row carries no free-space reading says so, like a cluster node', async ({ page }) => {
    await install(page);
    await page.route(/\/api\/estate\/observations\?scope=host&host=forge&/, (r) => json(r, {
      data: [envelope({ scope: 'host', observer: 'boss-estate-observe-host', observed_at: ago(5), nodes: [{ id: 'forge' }] })],
      total: 1,
    }));
    await mountPage(page, PATH, TITLE);

    await expect(obsRow(page, 'host').filter({ hasText: 'forge:' }).locator('span')).toHaveText([
      'host',
      'forge: free space unread — seen by boss-estate-observe-host',
      // One reading measures no cadence, and no verdict is invented.
      '5m ago · cadence not yet measured',
    ]);
  });

  test('a clean cluster comparison reads green "no drift"', async ({ page }) => {
    await install(page);
    await mountPage(page, PATH, TITLE);

    const cmp = obsRow(page, 'comparison');
    await expect(cmp).toHaveCount(1);
    await expect(cmp.locator('span').nth(1)).toHaveText('5 observed, 5 declared — no drift');
    await expect(cmp.locator('span').nth(1)).toHaveClass(/\bestate-ok\b/);
    await expect(cmp.locator('span').nth(2)).toHaveText('today');
  });

  // Gap 1 (2d8d983b), FIXED: until this pin the page rendered only the
  // cluster's verdict, and this test asserted that the drifted host row
  // was NOT shown. The fixture is the live shape of 2026-09-23.
  test('gap 1: each host\'s newest comparison reads beside the cluster\'s, a drift row and a disk_tight row in amber', async ({ page }) => {
    await install(page);
    await mountPage(page, PATH, TITLE);

    await expect(hostRows(page)).toHaveCount(2);
    const verdicts = hostRows(page).locator('span:nth-child(2)');
    // Each drift named as its finding names it (ab3c54d7).
    await expect(verdicts).toHaveText([
      'boss-gcp: 1 drifted from declaration (boss-gcp memory_gb 15 → 16); 1 short of disk',
      'forge: 1 drifted from declaration (forge memory_gb 30 → 31)',
    ]);
    for (let i = 0; i < 2; i += 1) {
      await expect(verdicts.nth(i)).toHaveClass(/\bestate-drift\b/);
    }
    // A read that is every host says nothing about its reach.
    await expect(page.locator('.estate-cover')).toHaveCount(0);
  });

  test('gap 1: a host that matches its declaration reads green, with no declared total to print as 0', async ({ page }) => {
    await install(page, {
      hostBody: () => ({ data: [hostCmp('forge', 2), hostCmp('boss-gcp', 600)], total: 2 }),
    });
    await mountPage(page, PATH, TITLE);

    const verdict = hostRows(page).filter({ hasText: 'forge:' }).locator('span:nth-child(2)');
    await expect(verdict).toHaveText('forge: 1 observed — no drift');
    await expect(verdict).toHaveClass(/\bestate-ok\b/);
  });

  // FLIPPED by 725532ab. This pin asserted a coverage line — "The newest
  // 1 of 612 host comparisons, back to …: a host whose last comparison
  // is older has no line here" — because the read was a scoped page of
  // 50 and boss-gcp's daily row fell off it about half of every day
  // (measured 2026-09-25: 50 of 768, all forge's). The read is grouped
  // per host on the server now, so a whole answer draws no coverage
  // line, and a declared host missing from it gets a line of its own.
  test('gap 1: every declared host gets its line — one with no comparison says so in amber, and a whole read draws no coverage line', async ({ page }) => {
    await install(page, { hostBody: () => ({ data: [hostCmp('forge', 2, { drift: 1 })], total: 1 }) });
    await mountPage(page, PATH, TITLE);

    const verdicts = hostRows(page).locator('span:nth-child(2)');
    await expect(verdicts).toHaveText([
      'boss-gcp: no host comparison recorded',
      'forge: 1 drifted from declaration',
    ]);
    await expect(verdicts.nth(0)).toHaveClass(/\bestate-drift\b/);
    // w-1 is a cluster node (the cluster verdict speaks for it) and
    // old-1 is retired: neither is owed a host line.
    await expect(hostRows(page).filter({ hasText: /w-1|old-1/ })).toHaveCount(0);
    await expect(page.locator('.estate-cover')).toHaveCount(0);
  });

  test('gap 1: a host read that is not every host says so, and never tells a declared host it has none', async ({ page }) => {
    // More hosts than the page held: the absence is the read's.
    await install(page, { hostBody: () => ({ data: [hostCmp('forge', 2, { drift: 1 })], total: 3 }) });
    await mountPage(page, PATH, TITLE);

    await expect(hostRows(page).locator('span:nth-child(2)')).toHaveText([
      'boss-gcp: not among the 1 of 3 hosts this read returned',
      'forge: 1 drifted from declaration',
    ]);
    await expect(page.locator('.estate-cover')).toHaveText(
      'The host read returned 1 of 3 hosts: a host past it has no comparison shown here.',
    );
  });

  test('gap 1: an empty host series gives each declared host its line', async ({ page }) => {
    await install(page, { host: 'empty' });
    await mountPage(page, PATH, TITLE);
    await expect(hostRows(page).locator('span:nth-child(2)')).toHaveText([
      'boss-gcp: no host comparison recorded',
      'forge: no host comparison recorded',
    ]);
  });

  test('gap 1: an empty host series with the registry unread says so in its own line', async ({ page }) => {
    await install(page, { host: 'empty', nodes: 'down' });
    await mountPage(page, PATH, TITLE);
    await expect(hostRows(page).locator('span')).toHaveText(['host comparison', 'no host comparison recorded yet']);
    // The host comparisons answered, and named no host: there is no host
    // series to read, and the observation line says that, not "unread".
    await expect(obsRow(page, 'host').locator('span').nth(1)).toHaveText(
      'no host declared or compared, so no host series to read',
    );
  });

  test('gap 1: a failed host read fails in the page\'s words, beside a cluster verdict that answered', async ({ page }) => {
    await install(page, { host: 'down' });
    await mountPage(page, PATH, TITLE);
    await expect(page.locator(`p.estate-fail${FAILURE_MARKER}`)).toHaveText([
      'Host comparisons unavailable: /api/estate/comparisons?scope=host&latest_per=host&limit=50: HTTP 503',
    ]);
    await expect(hostRows(page)).toHaveCount(0);
    await expect(obsRow(page, 'comparison').locator('span').nth(1)).toHaveText('5 observed, 5 declared — no drift');
  });

  test('a comparison that disagrees names every count that does, in amber', async ({ page }) => {
    await install(page, {
      cmpBody: () => comparisons({ observed_not_declared: 1, declared_not_observed: 2, drift: 3, disk_tight: 1, disk_unmeasured: 2 }),
    });
    await mountPage(page, PATH, TITLE);

    const verdict = obsRow(page, 'comparison').locator('span').nth(1);
    await expect(verdict).toHaveText(
      '1 in the cluster but undeclared; 2 declared but not seen; 3 drifted from declaration; 1 short of disk; 2 with no free-space reading',
    );
    await expect(verdict).toHaveClass(/\bestate-drift\b/);
  });

  // Gap 5 (ea5e0e8b): not_ready was a finding with no count, and the
  // verdict reads counts. Gap 6 (c2373cc4): dead_letters_unrecorded and
  // dispatcher_unread were dropped by the parser. Both read green
  // "no drift" until 2026-09-27. Fixtures in compare()'s own shapes
  // (estate_compare.rs): not_ready ids bare, dispatcher_unread a reason.
  test('gaps 5 and 6: a NotReady node and unrecorded dead letters are named, in amber', async ({ page }) => {
    await install(page, {
      cmpBody: () => comparisons(
        { not_ready: 1, dead_letters_unrecorded: 1 },
        {
          not_ready: ['w-1'],
          dead_letters_unrecorded: [{ id: 'boss-dispatcher', dead_letters_unrecorded: 4, age_s: 600 }],
          dispatcher_unread: null,
        },
      ),
    });
    await mountPage(page, PATH, TITLE);

    const verdict = obsRow(page, 'comparison').locator('span').nth(1);
    await expect(verdict).toHaveText('1 not ready (w-1); 1 dispatcher with unrecorded dead letters');
    await expect(verdict).toHaveClass(/\bestate-drift\b/);
  });

  test('gap 6: a dispatcher whose counters went unread is not "no drift"', async ({ page }) => {
    await install(page, {
      cmpBody: () => comparisons({}, { not_ready: [], dead_letters_unrecorded: [], dispatcher_unread: 'curl: (7) Failed to connect' }),
    });
    await mountPage(page, PATH, TITLE);

    const verdict = obsRow(page, 'comparison').locator('span').nth(1);
    await expect(verdict).toHaveText('dispatcher dead-letter counters unread: curl: (7) Failed to connect');
    await expect(verdict).toHaveClass(/\bestate-drift\b/);
  });

  test('gap 12: empty observation series and a zero cluster-comparison read each state what they hold', async ({ page }) => {
    await install(page, { cluster: 'empty', series: 'empty', cmp: 'empty', host: 'empty' });
    await mountPage(page, PATH, TITLE);

    // The cluster series, per declared host (forge and boss-gcp) its
    // two series, and its host-comparison line (gap 1; per declared host
    // since 725532ab). Each says "no observation recorded yet" because
    // ITS OWN read counted zero (75027a93).
    await expect(obsRows(page)).toHaveCount(8);
    await expect(obsRow(page, 'host comparison').locator('span:nth-child(2)')).toHaveText([
      'boss-gcp: no host comparison recorded',
      'forge: no host comparison recorded',
    ]);
    await expect(obsRow(page, 'kubernetes-nodes').locator('span').nth(1)).toHaveText('no observation recorded yet');
    await expect(obsRow(page, 'host').locator('span:nth-child(2)')).toHaveText([
      'boss-gcp: no observation recorded yet',
      'forge: no observation recorded yet',
    ]);
    await expect(obsRow(page, 'host units').locator('span:nth-child(2)')).toHaveText([
      'boss-gcp: no observation recorded yet',
      'forge: no observation recorded yet',
    ]);
    await expect(obsRow(page, 'comparison').locator('span').nth(1)).toHaveText('0 cluster comparisons in this read');
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
  });

  // Gap 12 (d6d39f60), IN PART, by 75027a93: the comparison line sat
  // inside the one observations read's ready branch, so this test
  // asserted that a comparison read that ANSWERED was hidden whenever
  // observations failed. There is no one observations read now: each
  // series fails on its own line, and hides nothing that answered.
  test('a failed cluster series says so on its own line, and hides nothing that answered', async ({ page }) => {
    await install(page, { cluster: 'down' });
    await mountPage(page, PATH, TITLE);

    await expect(page.locator(`p.estate-fail${FAILURE_MARKER}`)).toHaveText([
      'Observations unavailable: /api/estate/observations?scope=kubernetes-nodes&limit=10: HTTP 503',
    ]);
    await expect(obsRow(page, 'kubernetes-nodes')).toHaveCount(0);
    await expect(obsRow(page, 'comparison').locator('span').nth(1)).toHaveText('5 observed, 5 declared — no drift');
    await expect(obsRow(page, 'host')).toHaveCount(2);
    await expect(hostRows(page)).toHaveCount(2);
  });

  test('a failed host series says so, naming the series, and never as "no observation recorded"', async ({ page }) => {
    await install(page, { series: 'down' });
    await mountPage(page, PATH, TITLE);

    await expect(page.locator(`p.estate-fail${FAILURE_MARKER}`)).toHaveText([
      'Observations unavailable: /api/estate/observations?scope=host&host=boss-gcp&limit=10: HTTP 503',
      'Observations unavailable: /api/estate/observations?scope=host&host=forge&limit=10: HTTP 503',
      'Observations unavailable: /api/estate/observations?scope=host-units&host=boss-gcp&limit=10: HTTP 503',
      'Observations unavailable: /api/estate/observations?scope=host-units&host=forge&limit=10: HTTP 503',
    ]);
    await expect(page.locator('.estate-obs').getByText(/no observation recorded/)).toHaveCount(0);
    await expect(obsRow(page, 'kubernetes-nodes').locator('span').nth(1)).toContainText('2 machines seen');
  });

  test('failed comparisons say so beside observations that answered, never as "no drift"', async ({ page }) => {
    await install(page, { cmp: 'down' });
    await mountPage(page, PATH, TITLE);

    await expect(page.locator(`p.estate-fail${FAILURE_MARKER}`)).toHaveText([
      'Comparisons unavailable: /api/estate/comparisons?scope=kubernetes-nodes&limit=1: HTTP 503',
    ]);
    // The cluster scope, per host its reading and its units (3d1678ba,
    // d5efb80d), plus the host comparison lines: their series is its own
    // read, and it answered.
    await expect(obsRows(page)).toHaveCount(7);
    await expect(hostRows(page)).toHaveCount(2);
    await expect(page.getByText(/no drift/)).toHaveCount(0);
    await expect(page.getByText('0 cluster comparisons in this read')).toHaveCount(0);
  });
});

// Gap 9 (48ef9961), FIXED: the audit pinned this indirectly — the page
// had exactly two link kinds, both on loop packets, and no way to reach
// the alarm behind an amber line. The open ESTATE ALARM packets are
// listed at the head of section 01, each linked, and each also sits
// beside the verdict of the series it is about.
test.describe('/it/estate — 01 the open estate alarms', () => {
  const alarmRows = (page: Page) => page.locator('.estate-alarms .estate-alarm-row');

  test('gap 9: every open alarm is listed, linked to its packet on the job detail surface', async ({ page }) => {
    await install(page);
    await mountPage(page, PATH, TITLE);

    await expect(alarmRows(page)).toHaveCount(2);
    await expect(alarmRows(page).locator('.estate-scope')).toHaveText(['open alarm', 'open alarm']);
    const links = alarmRows(page).locator('a');
    await expect(links).toHaveText([
      'ESTATE ALARM: disk_tight:boss-gcp persisted 3 consecutive comparisons',
      'ESTATE ALARM: door:dev-ssh:edge — the edge half (dev.algedonic.dev) is dark past its 10-minute band',
    ]);
    const hrefs = await links.evaluateAll((as) => as.map((a) => a.getAttribute('href')));
    expect(hrefs).toEqual([`${ROUTE_CATALOG.jobs.path}/${DISK_ALARM}`, `${ROUTE_CATALOG.jobs.path}/${DOOR_ALARM}`]);
    for (const [href, jobId] of [[hrefs[0]!, DISK_ALARM], [hrefs[1]!, DOOR_ALARM]] as const) {
      expect(route(href)).toEqual({ kind: 'jobDetail', jobId });
    }
    await expect(alarmRows(page).locator('.estate-when')).toHaveText([/ ago$/, / ago$/]);
    await expect(page.locator('.estate-alarm-none')).toHaveCount(0);
    await expect(page.locator('.estate-alarm-cover')).toHaveCount(0);
  });

  test('gap 9: an alarm sits beside the verdict of its own series, and nowhere else', async ({ page }) => {
    await install(page);
    await mountPage(page, PATH, TITLE);

    const gcp = hostRows(page).filter({ hasText: 'boss-gcp:' }).locator('a.estate-alarm-link');
    await expect(gcp).toHaveText('alarm');
    await expect(gcp).toHaveAttribute('href', `/ux/jobs/${DISK_ALARM}`);
    await expect(gcp).toHaveAttribute('title', 'ESTATE ALARM: disk_tight:boss-gcp persisted 3 consecutive comparisons');
    // One inline link: the door alarm has no verdict line on this page,
    // so it is in the list only.
    await expect(page.locator('.estate-obs a.estate-alarm-link')).toHaveCount(1);
  });

  test('gap 9: an alarm link opens the packet, and back returns to the estate', async ({ page }) => {
    await install(page);
    await mountPage(page, PATH, TITLE);

    await hostRows(page).filter({ hasText: 'boss-gcp:' }).getByRole('link', { name: 'alarm', exact: true }).click();
    await expect(page).toHaveURL(new RegExp(`/ux/jobs/${DISK_ALARM}$`));
    await page.goBack();
    await expect(page).toHaveURL(new RegExp(`${PATH}$`));
    await expect(alarmRows(page)).toHaveCount(2);
  });

  test('gap 9: with none open the page says so, and puts no alarm beside any verdict', async ({ page }) => {
    await install(page, { alarms: 'empty' });
    await mountPage(page, PATH, TITLE);

    await expect(page.locator('p.estate-alarm-none')).toHaveText('No estate alarm is open.');
    await expect(alarmRows(page)).toHaveCount(0);
    await expect(page.locator('a.estate-alarm-link')).toHaveCount(0);
  });

  test('gap 9: a failed alarm read says so in the page\'s words — never "no alarm" — and hides no verdict', async ({ page }) => {
    await install(page, { alarms: 'down' });
    await mountPage(page, PATH, TITLE);

    await expect(page.locator(`.estate-alarms p.estate-fail${FAILURE_MARKER}`)).toHaveText(
      'Estate alarms unavailable: /api/jobs?kind=backlog-item&status=open&metadata_has=estate_finding&limit=50: HTTP 503',
    );
    await expect(page.locator('.estate-alarm-none')).toHaveCount(0);
    await expect(hostRows(page)).toHaveCount(2);
  });

  test('gap 9: a read holding fewer alarms than it counted says how many it holds', async ({ page }) => {
    await install(page, { alarmsBody: () => ({ ...openAlarms(), total: 61 }) });
    await mountPage(page, PATH, TITLE);

    await expect(page.locator('p.estate-alarm-cover')).toHaveText(
      'The alarm read returned 2 of 61 open estate alarms: the rest are not listed here.',
    );
  });
});

test.describe('/it/estate — 02 THE LOOPS, as links', () => {
  test('the hint reads verbatim, and every link lands on the catalogued job detail surface', async ({ page }) => {
    await install(page);
    await mountPage(page, PATH, TITLE);

    await expect(page.locator('p.estate-hint').first()).toHaveText(
      'Did each loop run: its newest finished packet (outcome and age) and any packet still open, each linked. The host is the one the packet names; where a packet names none, the page says so rather than guess.',
    );
    await expect(loops(page).locator('thead th')).toHaveText(['loop', 'host', 'newest finished', 'open']);
    const links = loops(page).locator('a');
    await expect(links).toHaveCount(2);
    const hrefs = await links.evaluateAll((as) => as.map((a) => a.getAttribute('href')));
    expect(hrefs).toEqual([
      `${ROUTE_CATALOG.jobs.path}/${WATCHDOG_DONE}`,
      `${ROUTE_CATALOG.jobs.path}/${UNITS_OPEN}`,
    ]);
    for (const [href, jobId] of [[hrefs[0]!, WATCHDOG_DONE], [hrefs[1]!, UNITS_OPEN]] as const) {
      expect(route(href)).toEqual({ kind: 'jobDetail', jobId });
    }
    await expect(links).toHaveText(['completed', 'open']);
  });

  for (const [what, name, id] of [
    ['newest finished', 'completed', WATCHDOG_DONE],
    ['open', 'open', UNITS_OPEN],
  ] as const) {
    test(`a loop's ${what} link opens the packet, and back returns to the estate`, async ({ page }) => {
      await install(page);
      await mountPage(page, PATH, TITLE);

      await loops(page).getByRole('link', { name, exact: true }).click();
      await expect(page).toHaveURL(new RegExp(`/ux/jobs/${id}$`));
      await page.goBack();
      await expect(page).toHaveURL(new RegExp(`${PATH}$`));
      await expect(page.locator('h1').first()).toHaveText('The estate');
      await expect(loops(page).getByRole('link', { name, exact: true })).toHaveAttribute('href', `/ux/jobs/${id}`);
    });
  }
});

// GAP 11 (e0e183fb), FIXED: the page rendered only machines, while the
// DNS zone, the Access applications in front of it and the tunnel routes
// behind it are declared in the tree and read back daily — so the one
// estate alarm about the zone had no line to stand beside. The shapes
// are the live ones of 2026-10-01 (zone reading d6ecf14f, converge
// 05ec4e89), names moved to example.org.
const ZONES_READ = /\/api\/jobs\?kind=dns-zone-observation&status=closed&limit=7&full=true$/;
const ZONES_OPEN_READ = /\/api\/jobs\?kind=dns-zone-observation&status=open$/;
const ZONE_READING = 'd6ecf14f-0000-0000-0000-00000000000b';
const ZONE_OPEN = 'd6ecf14f-0000-0000-0000-00000000000e';
const ZONE_ALARM = 'd3470342-0000-0000-0000-00000000000c';
const CONVERGE_DONE = '05ec4e89-0000-0000-0000-00000000000d';
const TUNNEL = 'd8a8ef3b-0000-0000-0000-000000000000.cfargotunnel.com';

/// The newest reading of example.org: one record matches behind Access,
/// one has drifted off the tunnel, two live records nobody declared, one
/// Access application that matches.
const zoneReading = () => ({
  id: ZONE_READING, kind: 'dns-zone-observation', status: 'closed',
  subject: { id: 'example.org', subject_kind: 'custom' },
  metadata: { zone: 'example.org', outcome: 'findings', closed_at: ago(600) },
  steps: [{
    spec_slug: 'observe',
    metadata: {
      result: 'findings',
      verdicts: [
        { record: 'boss.example.org CNAME', verdict: 'MATCH', interlock: 'access', access: 'present', why: 'the operating site',
          declared: { content: TUNNEL, target: 'tunnel:cloudflare-tunnel-credentials' }, live: { content: TUNNEL } },
        { record: 'www.example.org CNAME', verdict: 'DRIFT',
          declared: { content: TUNNEL, target: 'tunnel:cloudflare-tunnel-credentials' }, live: { content: 'old.example.net' } },
        { record: 'example.org MX', verdict: 'UNDECLARED', live: { content: 'mail.example.net' } },
        { record: 'example.org TXT', verdict: 'UNDECLARED', live: { content: '"v=spf1 ~all"' } },
      ],
      access: [{
        domain: 'boss.example.org', verdict: 'MATCH',
        declared: { name: 'BOSS', type: 'self_hosted', policies: [{ name: 'operators', decision: 'allow', emails: ['op@example.org'] }] },
        live: { name: 'BOSS', type: 'self_hosted', policies: [{ name: 'operators', decision: 'allow', include: [{ email: { email: 'op@example.org' } }] }] },
      }],
    },
  }],
});

/// The alarm the drift raised, keyed as dns_observe.rs keys it.
const zoneAlarm = () => alarm(ZONE_ALARM, 'ESTATE ALARM: DNS zone example.org disagrees with its declaration (1 finding(s))', 590,
  { estate_finding: 'dns_drift:example.org', scope: 'dns-zone', zone: 'example.org' });

async function installEdge(page: Page, zones: 'fixture' | 'down' = 'fixture', open: readonly unknown[] = []): Promise<void> {
  await install(page, { alarmsBody: () => ({ data: [...openAlarms().data, zoneAlarm()], total: 3 }) });
  await page.route(ZONES_READ, (r) =>
    zones === 'down' ? json(r, { error: 'jobs upstream unavailable' }, 503) : json(r, { data: [zoneReading()], total: 1 }));
  await page.route(ZONES_OPEN_READ, (r) => json(r, { data: open, total: open.length }));
}

const edge = (page: Page) => page.locator('.estate-edge');

test.describe('/it/estate — 03 THE EDGE', () => {
  test('gap 11: the zone line reads its verdict, linked to its reading, with the alarm its drift raised beside it', async ({ page }) => {
    await installEdge(page);
    await mountPage(page, PATH, TITLE);

    const line = edge(page).locator('.estate-zone[data-zone="example.org"]');
    const verdict = line.locator('a').first();
    await expect(verdict).toHaveText('example.org: 1 of 2 declared records match · 1 of 1 Access applications match');
    await expect(verdict).toHaveClass(/\bestate-drift\b/);
    await expect(verdict).toHaveAttribute('href', `/ux/jobs/${ZONE_READING}`);
    await expect(line.locator('a.estate-alarm-link')).toHaveAttribute('href', `/ux/jobs/${ZONE_ALARM}`);
    await expect(line.locator('.estate-when')).toHaveText(/^\d+h ago$/);
  });

  test('gap 11: each declared record reads the tree\'s spelling, what the zone holds, what fronts it and its verdict', async ({ page }) => {
    await installEdge(page);
    await mountPage(page, PATH, TITLE);

    const records = edge(page).locator('table.estate-records');
    await expect(records.locator('thead th')).toHaveText(['record', 'declared', 'in front', 'verdict']);
    await expect(records.locator('tbody tr').nth(0).locator('td')).toHaveText([
      'boss.example.org CNAME',
      `tunnel:cloudflare-tunnel-credentials zone holds ${TUNNEL}`,
      'access: present',
      'MATCH',
    ]);
    await expect(records.locator('tbody tr').nth(1).locator('td')).toHaveText([
      'www.example.org CNAME',
      'tunnel:cloudflare-tunnel-credentials zone holds old.example.net',
      '—',
      'DRIFT',
    ]);
    await expect(records.locator('tbody tr').nth(1).locator('td').last()).toHaveClass(/\bestate-drift\b/);
    await expect(records.locator('tbody tr')).toHaveCount(2);
    await expect(edge(page).locator('p.estate-edge-note')).toHaveText([
      '2 live records the declaration names nowhere — reported on the reading, not a finding.',
    ]);
  });

  test('gap 11: each Access application reads its type and policies by name and decision — never the people admitted', async ({ page }) => {
    await installEdge(page);
    await mountPage(page, PATH, TITLE);

    const apps = edge(page).locator('table.estate-access');
    await expect(apps.locator('thead th')).toHaveText(['access application', 'type', 'policies', 'verdict']);
    await expect(apps.locator('tbody tr td')).toHaveText(['boss.example.org', 'self_hosted', 'operators (allow)', 'MATCH']);
    await expect(edge(page).getByText('op@example.org')).toHaveCount(0);
  });

  test('gap 11: the tunnel routes are the newest cluster converge\'s, with its connector, linked to it', async ({ page }) => {
    await installEdge(page);
    await page.route(LOOPS_READ, (r) => {
      const url = new URL(r.request().url());
      const data = url.searchParams.get('kind') === 'maintenance-cluster-converge' && url.searchParams.get('status') === 'closed'
        ? [{ id: CONVERGE_DONE, status: 'closed', metadata: { outcome: 'completed', closed_at: ago(4) }, steps: [{ spec_slug: 'run', metadata: {
          cloudflared: 'connected',
          tunnel_ingress: 'boss.example.org → boss; www.example.org → boss (site); dev.example.org → ssh://boss-dev-ssh:22 (origin)',
        } }] }]
        : loopAnswer(url);
      return json(r, { data, total: data.length });
    });
    await mountPage(page, PATH, TITLE);

    const line = edge(page).locator('.estate-tunnel');
    await expect(line.locator('a')).toHaveText('connector connected · 3 routes');
    await expect(line.locator('a')).toHaveClass(/\bestate-ok\b/);
    await expect(line.locator('a')).toHaveAttribute('href', `/ux/jobs/${CONVERGE_DONE}`);
    await expect(edge(page).locator('ul.estate-routes li')).toHaveText([
      'boss.example.org → boss',
      'www.example.org → boss (site)',
      'dev.example.org → ssh://boss-dev-ssh:22 (origin)',
    ]);
  });

  test('gap 11: with nothing recorded the edge says so — no zone reading, no converge — and fails nothing', async ({ page }) => {
    await install(page);
    await mountPage(page, PATH, TITLE);

    await expect(edge(page).locator('.estate-obs-row').first().locator('span').nth(1)).toHaveText('no zone reading recorded yet');
    await expect(edge(page).locator('.estate-tunnel span').nth(1)).toHaveText('no cluster converge has finished, so no route is recorded');
    await expect(edge(page).locator(FAILURE_MARKER)).toHaveCount(0);
  });

  test('gap 11: a failed zone read says so in the page\'s words — never "no zone reading" — and an open reading reads amber, linked', async ({ page }) => {
    await installEdge(page, 'down', [
      { id: ZONE_OPEN, kind: 'dns-zone-observation', status: 'open', opened_at: ago(30), subject: { id: 'example.org' }, metadata: { zone: 'example.org' } },
    ]);
    await mountPage(page, PATH, TITLE);

    await expect(edge(page).locator(`p.estate-fail${FAILURE_MARKER}`)).toHaveText([
      'Zone readings unavailable: /api/jobs?kind=dns-zone-observation&status=closed&limit=7&full=true: HTTP 503',
    ]);
    await expect(edge(page).getByText('no zone reading recorded yet')).toHaveCount(0);
    const open = edge(page).locator('[data-zone-open="example.org"] a');
    await expect(open).toHaveText('example.org: a reading is still open — the zone could not be read or compared');
    await expect(open).toHaveClass(/\bestate-drift\b/);
    await expect(open).toHaveAttribute('href', `/ux/jobs/${ZONE_OPEN}`);
  });
});

test.describe('/it/estate — 04 THE DEV WORKSPACE', () => {
  test('three numbered steps, each with its reason and one copyable command, verbatim', async ({ page }) => {
    await install(page);
    await mountPage(page, PATH, TITLE);

    const door = page.locator('.estate-door');
    await expect(door.locator('p.estate-hint').first()).toHaveText(
      'The workspace answers on dev.algedonic.dev, from anywhere, behind Cloudflare Access. There is no VPN to join and no key to install: the edge asks who you are and issues a certificate that lasts the session. Three lines, the first two once per machine.',
    );
    await expect(door.locator('p.estate-hint strong')).toHaveText([
      '1. Install cloudflared, once per machine',
      '2. Teach ssh the route, once per machine',
      '3. Open the workspace',
    ]);
    const snippets = door.locator('pre.estate-snippet');
    await expect(snippets).toHaveText([
      'cloudflared --version',
      "grep -qsF 'Match host dev.algedonic.dev ' ~/.ssh/config || cloudflared access ssh-config --hostname dev.algedonic.dev --short-lived-cert | sed '/^Add to your/d' >> ~/.ssh/config",
      'ssh root@dev.algedonic.dev',
    ]);
    // Copyable without a button: one click selects the whole line.
    for (let i = 0; i < 3; i += 1) {
      await expect(snippets.nth(i)).toHaveCSS('user-select', 'all');
    }
    await expect(door.locator('p.estate-hint').last()).toHaveText(
      'Inside: the durable tmux session is dev — attach with /work/dev-session.sh, detach with ctrl-b d. For the browser instead, run claude remote-control inside the session and drive it from claude.ai.',
    );
  });

  test('the declared door remains visible when the estate reads fail', async ({ page }) => {
    await install(page, { nodes: 'down', cluster: 'down', series: 'down', cmp: 'down', host: 'down' });
    await mountPage(page, PATH, TITLE);

    // The registry, the cluster series, the cluster comparison and the
    // host comparisons — each its own failure line since 75027a93, which
    // no longer hides the comparison reads behind the observations one.
    // No host series is read: neither source could name a host, and the
    // page says so rather than "no host".
    await expect(page.locator(`p.estate-fail${FAILURE_MARKER}`)).toHaveCount(4);
    await expect(obsRow(page, 'host').locator('span').nth(1)).toHaveText(
      'no host series read — neither the registry nor the host comparisons answered',
    );
    await expect(page.locator('pre.estate-snippet')).toHaveCount(3);
  });
});
