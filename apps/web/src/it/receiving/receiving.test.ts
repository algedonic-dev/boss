import { describe, expect, test } from 'bun:test';
import {
  ageBand,
  ageDays,
  arrivalsByDay,
  daysEndingOn,
  failedStep,
  holderOf,
  inboundKinds,
  kindNoun,
  kindSentence,
  laneColor,
  lanesOf,
  laneOf,
  loadEveryPage,
  loadKind,
  parseJobsPage,
  provenance,
  readings,
  takenIn,
  UNCLASSIFIED,
  waiting,
  type InboundRow,
} from './receiving';

const WORKFLOWS = [
  { kind: 'backlog-item', category: 'platform', status: 'active' },
  { kind: 'user-feedback', category: 'platform', status: 'active' },
  { kind: 'design-doc', category: 'platform', status: 'active' },
  { kind: 'maintenance-backup', category: 'platform', status: 'active' },
  { kind: 'pr-train', category: 'platform', status: 'active' },
  { kind: 'gate-run', category: 'platform', status: 'active' },
  { kind: 'ship-a-change', category: 'platform', status: 'active' },
  { kind: 'ops-request', category: 'platform', status: 'active' },
  { kind: 'payroll-run', category: 'finance', status: 'active' },
  { kind: 'learning', category: 'platform', status: 'retired' },
];

function job(over: Record<string, unknown>): Record<string, unknown> {
  return {
    id: '0123456789abcdef',
    kind: 'backlog-item',
    title: 'A thing',
    status: 'open',
    opened_on: '2026-09-11',
    closed_on: null,
    priority: 'standard',
    metadata: {},
    steps: [
      { kind: 'trigger', status: 'completed', assignee_id: null },
      { kind: 'task', status: 'ready', assignee_id: 'claude@algedonic.dev' },
    ],
    ...over,
  };
}

describe('which kinds are inbound', () => {
  test('platform kinds that are neither chores nor the delivery pipeline, active only', () => {
    expect(inboundKinds(WORKFLOWS)).toEqual(['backlog-item', 'design-doc', 'user-feedback']);
  });
});

// THE LANE IS THE SERVER'S (backlog 1eea4554). This board read
// `metadata.channel` — a key no filer writes — in a six-lane vocabulary
// of its own, so 0 of 1,626 arrivals read as recorded while 492 carried
// `input_channel`. It now draws the `lane` the list put on each row.
describe('lane — what the server read, drawn as it came', () => {
  test('a recorded lane is drawn in the server\'s spelling', () => {
    expect(laneOf(job({ lane: { lane: 'review-finding', basis: 'recorded' } }))).toEqual({
      lane: 'review-finding',
      basis: 'recorded',
    });
    expect(laneOf(job({ lane: { lane: 'telemetry/monitoring', basis: 'recorded' } })).lane).toBe(
      'telemetry/monitoring',
    );
  });
  test('the server\'s unclassified is unclassified', () => {
    expect(laneOf(job({ lane: { lane: 'unclassified', basis: 'unclassified' } }))).toEqual(UNCLASSIFIED);
  });
  test('the board classifies nothing itself — not a kind, a reporter, or the old key', () => {
    for (const over of [
      { kind: 'user-feedback' },
      { kind: 'incident' },
      { kind: 'design-doc' },
      { metadata: { reporter: 'automation:cluster-watchdog' } },
      { metadata: { channel: 'monitoring' } },
      { metadata: { input_channel: 'review-finding' } },
    ]) {
      expect(laneOf(job(over))).toEqual(UNCLASSIFIED);
    }
  });
  test('the lanes to draw: every recorded lane the rows carry, then unclassified, always', () => {
    const rows = parseJobsPage({
      data: [
        job({ id: 'a', lane: { lane: 'roadmap', basis: 'recorded' } }),
        job({ id: 'b', lane: { lane: 'discovery-while-working', basis: 'recorded' } }),
        job({ id: 'c', lane: { lane: 'roadmap', basis: 'recorded' } }),
      ],
      total: 3,
    }).rows;
    expect(lanesOf(rows)).toEqual(['discovery-while-working', 'roadmap', 'unclassified']);
    expect(lanesOf([])).toEqual(['unclassified']);
  });
  test('each lane a reading draws has its own hue, and unclassified is the faint one', () => {
    const lanes = [
      'design-resolution',
      'dependency/external',
      'discovery-while-working',
      'pipeline-failure',
      'post-mortem',
      'review-finding',
      'roadmap',
      'scheduled',
      'telemetry/monitoring',
      'user-feedback',
      'unclassified',
    ];
    const recorded = lanes.slice(0, -1).map((l) => laneColor(l, lanes));
    expect(new Set(recorded).size).toBe(recorded.length);
    expect(laneColor('unclassified', lanes)).toBe('var(--text-faint)');
    expect(recorded).not.toContain('var(--text-faint)');
  });
});

describe('the page is parsed once', () => {
  test('rows carry what the tracks and the manifest need, and total is read off the envelope', () => {
    const page = parseJobsPage({ data: [job({})], total: 41 });
    expect(page.total).toBe(41);
    expect(page.rows).toHaveLength(1);
    const r = page.rows[0];
    expect(r?.id).toBe('0123456789abcdef');
    expect(r?.openedOn).toBe('2026-09-11');
    expect(r?.ready).toEqual([{ kind: 'task', who: 'claude@algedonic.dev', failed: null }]);
    expect(r?.lane).toBe('unclassified');
    expect(r?.laneBasis).toBe('unclassified');
  });
  // A failed verb answer (backlog 074e1287): jobs.complete_linked_step
  // leaves the step OPEN and writes the verb's last FAILED line on it
  // as `failed` with the alert it filed. Publish 254177e2's open-pr sat
  // ready for five hours on 2026-09-19 and this yard drew it like any
  // ready step — the packet was troubled and did not look it.
  test('a ready step a failed verb annotated carries the FAILED line and its alert', () => {
    const line = 'publish-github-pr: FAILED — pushing publish/2026-09-18: dubious ownership';
    const page = parseJobsPage({
      data: [
        job({
          kind: 'publish-to-github',
          steps: [
            { kind: 'approval', status: 'completed', assignee_id: 'emp-david' },
            {
              kind: 'task',
              status: 'ready',
              assignee_id: null,
              metadata: {
                ops_verb: 'publish-github-pr',
                failed: line,
                failed_exit: '1',
                failed_source: 'c98a782f-0000-4000-8000-000000000000',
                alert: 'a1e57000-0000-4000-8000-000000000000',
              },
            },
          ],
        }),
      ],
      total: 1,
    });
    const r = page.rows[0];
    expect(r?.ready).toEqual([
      {
        kind: 'task',
        who: null,
        failed: {
          line,
          exit: '1',
          source: 'c98a782f-0000-4000-8000-000000000000',
          alert: 'a1e57000-0000-4000-8000-000000000000',
        },
      },
    ]);
    if (!r) throw new Error('fixture parsed to nothing');
    expect(failedStep(r)?.failed?.line).toBe(line);
    // A healthy packet has no failed step — the yard says nothing.
    const healthy = parseJobsPage({ data: [job({})], total: 1 }).rows[0];
    if (!healthy) throw new Error('fixture parsed to nothing');
    expect(failedStep(healthy)).toBeNull();
  });
  test('a malformed envelope is an empty page with total 0, not a throw', () => {
    expect(parseJobsPage(null)).toEqual({ rows: [], total: 0 });
  });
});

// THE PARTITION (design 62de32ae decision 4): a packet stands in
// receiving until its intake step completes, and is marshalling's after
// — the rule `regions.rs::taken_in` draws the server's line with.
describe('receiving holds a packet until its intake step completes', () => {
  const parsed = (steps: ReadonlyArray<Record<string, unknown>>): InboundRow => {
    const r = parseJobsPage({ data: [job({ steps })], total: 1 }).rows[0];
    if (!r) throw new Error('fixture parsed to nothing');
    return r;
  };
  const untriaged = parsed([
    { kind: 'trigger', status: 'completed' },
    { kind: 'task', status: 'ready' },
  ]);
  const triaged = parsed([
    { kind: 'trigger', status: 'completed' },
    { kind: 'task', status: 'completed' },
    { kind: 'task', status: 'ready' },
  ]);

  test('a trigger takes nothing in; any other completed step does', () => {
    expect(untriaged.takenIn).toBe(false);
    expect(triaged.takenIn).toBe(true);
    expect(takenIn([{ kind: 'task', status: 'skipped' }])).toBe(false);
    expect(takenIn([])).toBe(false);
  });

  test('only what nothing has taken in is standing', () => {
    const standing = waiting([{ ...untriaged, id: 'u' }, { ...triaged, id: 't' }], '2026-09-20');
    expect(standing.map((r) => r.id)).toEqual(['u']);
  });
});

// A LIMIT IS NOT A FILTER (design 62de32ae decision 4): the board read
// one page of 500 and, on 2026-09-24, backlog-item had 822 in the window.
describe('every page of a kind is read, to its total', () => {
  const rows = (from: number, n: number): ReadonlyArray<InboundRow> =>
    parseJobsPage({
      data: Array.from({ length: n }, (_, i) => job({ id: `row-${from + i}` })),
      total: n,
    }).rows;
  const TOTAL = 1201;

  test('past the page, every row — and a row a shifting page repeats is kept once', async () => {
    const asked: number[] = [];
    const load: typeof loadKind = async (_kind, _days, limit, offset = 0) => {
      asked.push(offset);
      // The table moved under the read: the second page starts one row
      // early, repeating the first page's last row.
      const start = offset === 0 ? 0 : offset - 1;
      return { kind: 'ready', data: { rows: rows(start, Math.min(limit, TOTAL - start)), total: TOTAL } };
    };
    const got = await loadEveryPage('backlog-item', 8, 500, load);
    if (got.kind !== 'ready') throw new Error(`the read failed: ${JSON.stringify(got)}`);
    expect(got.data.total).toBe(TOTAL);
    expect(new Set(got.data.rows.map((r) => r.id)).size).toBe(got.data.rows.length);
    expect(got.data.rows.length).toBe(TOTAL);
    expect(asked.slice(0, 2)).toEqual([0, 500]);
  });

  test('a page that fails fails the read — half a kind is not a kind', async () => {
    const load: typeof loadKind = async (_kind, _days, limit, offset = 0) =>
      offset === 0
        ? { kind: 'ready', data: { rows: rows(0, limit), total: TOTAL } }
        : { kind: 'failed', error: 'HTTP 500' };
    const got = await loadEveryPage('backlog-item', 8, 500, load);
    expect(got).toEqual({ kind: 'failed', error: 'HTTP 500' });
  });
});

describe('who holds a standing packet', () => {
  const base = parseJobsPage({ data: [job({})], total: 1 }).rows[0];
  const row = (ready: InboundRow['ready']): InboundRow => {
    if (!base) throw new Error('fixture parsed to nothing');
    return { ...base, ready };
  };
  test('an agent, a human, nobody', () => {
    expect(holderOf(row([{ kind: 'task', who: 'claude@algedonic.dev', failed: null }]))).toEqual({
      who: 'agent',
      label: 'the agent',
    });
    expect(holderOf(row([{ kind: 'answer-question', who: 'emp-david', failed: null }]))).toEqual({
      who: 'human',
      label: 'emp-david',
    });
    expect(holderOf(row([{ kind: 'task', who: null, failed: null }]))).toEqual({
      who: 'nobody',
      label: 'unassigned',
    });
    expect(holderOf(row([]))).toEqual({ who: 'nobody', label: 'no ready step' });
  });
  test('a human on any ready step outranks the agent — the packet waits on the person', () => {
    expect(
      holderOf(
        row([
          { kind: 'task', who: 'claude@algedonic.dev', failed: null },
          { kind: 'sign-off', who: 'emp-david', failed: null },
        ]),
      ).who,
    ).toBe('human');
  });
});

describe('age', () => {
  test('days from opened_on to today, and the band it falls in', () => {
    expect(ageDays('2026-09-05', '2026-09-12')).toBe(7);
    expect(ageBand(0)).toBe('fresh');
    expect(ageBand(3)).toBe('fresh');
    expect(ageBand(4)).toBe('aging');
    expect(ageBand(14)).toBe('aging');
    expect(ageBand(15)).toBe('stale');
  });
  test('the window is the last n days ending today, oldest first', () => {
    expect(daysEndingOn('2026-09-12', 3)).toEqual(['2026-09-10', '2026-09-11', '2026-09-12']);
  });
});

describe('arrivals and departures per day', () => {
  const rows = parseJobsPage(
    {
      data: [
        job({ id: 'a', opened_on: '2026-09-11', lane: { lane: 'telemetry/monitoring', basis: 'recorded' } }),
        job({ id: 'b', opened_on: '2026-09-11', kind: 'user-feedback', lane: { lane: 'user-feedback', basis: 'recorded' } }),
        job({ id: 'c', opened_on: '2026-09-12', status: 'closed', closed_on: '2026-09-12' }),
        job({ id: 'd', opened_on: '2026-09-01', status: 'closed', closed_on: '2026-09-11' }),
      ],
      total: 4,
    },
  ).rows;
  test('a bar per day by lane, and the departures beside it', () => {
    const days = arrivalsByDay(rows, ['2026-09-11', '2026-09-12']);
    expect(days[0]).toEqual({
      day: '2026-09-11',
      arrived: 2,
      byLane: { 'telemetry/monitoring': 1, 'user-feedback': 1 },
      left: 1,
    });
    expect(days[1]).toEqual({
      day: '2026-09-12',
      arrived: 1,
      byLane: { unclassified: 1 },
      left: 1,
    });
  });
  test('the standing packets, oldest first, with age, band and holder', () => {
    const w = waiting(rows, '2026-09-12');
    expect(w.map((r) => r.id)).toEqual(['a', 'b']);
    expect(w[0]?.age).toBe(1);
    expect(w[0]?.band).toBe('fresh');
    expect(w[0]?.holder.who).toBe('agent');
  });
});

describe('what the snapshot says', () => {
  test('the readings name the feedback that stands, the one-actor share and the unrecorded share', () => {
    const rows = parseJobsPage(
      {
        data: [
          job({ id: 'f1', kind: 'user-feedback', opened_on: '2026-08-22' }),
          job({ id: 'f2', kind: 'user-feedback', opened_on: '2026-09-10' }),
          job({ id: 'b1', opened_on: '2026-09-11' }),
          job({
            id: 'b2',
            opened_on: '2026-09-11',
            steps: [{ kind: 'task', status: 'ready', assignee_id: 'emp-david' }],
          }),
          job({ id: 'b3', opened_on: '2026-09-01', status: 'closed', closed_on: '2026-09-11' }),
          // A recorded arrival is not unrecorded; a backlog item in the
          // user-feedback LANE is not a feedback packet — the reading
          // counts the kind a person files through the feedback door.
          job({
            id: 'b4',
            opened_on: '2026-09-12',
            status: 'closed',
            closed_on: '2026-09-12',
            lane: { lane: 'user-feedback', basis: 'recorded' },
          }),
        ],
        total: 6,
      },
    ).rows;
    const r = readings(rows, '2026-09-12', ['2026-09-11', '2026-09-12']);
    expect(r.feedbackStanding).toEqual({ count: 2, oldestDays: 21 });
    expect(r.onOneActor).toEqual({ count: 3, of: 4, actor: 'the agent' });
    expect(r.unrecorded).toEqual({ count: 2, of: 3 });
  });
});

// WHERE THE UNTRIAGED ITEMS CAME FROM (design 3036296f mechanism D,
// backlog 9b473d4a). The server reads each row's source and area
// (`origin=true`, `boss_jobs::origin::origin_of`); this board groups by
// the KEY it read and keeps no rule of its own. Every unrecorded shape
// stands in one visible group — never dropped.
describe('origin — grouped by what the server read', () => {
  const recorded = (kind: string, key: string, extra: Record<string, unknown> = {}) => ({
    source: { basis: 'recorded', kind, key, id: null, branch: null, ...extra },
    area: null,
  });
  const unrecorded = (basis: string, area: string | null = null) => ({
    source: { basis, kind: null, key: null, id: null, branch: null },
    area,
  });

  test('the read asks the list for the origin reading', () => {
    let url = '';
    const origFetch = globalThis.fetch;
    globalThis.fetch = (async (u: string) => {
      url = u;
      return new Response(JSON.stringify({ data: [], total: 0 }));
    }) as unknown as typeof fetch;
    return loadKind('backlog-item', 8, 500).finally(() => {
      globalThis.fetch = origFetch;
      expect(url).toContain('&origin=true');
      expect(url).toContain('&lane=true');
    });
  });

  test('a row carries the source and area the server read; a row without one is unrecorded, not dropped', () => {
    const { rows } = parseJobsPage({
      data: [
        job({ id: 'a', origin: { ...recorded('review', 'review:fix/x', { branch: 'fix/x' }), area: 'jobs' } }),
        job({ id: 'b', origin: unrecorded('prose') }),
        job({ id: 'c' }),
        // A basis this board does not know is drawn unrecorded under the
        // word the server sent, never promoted to a source.
        job({ id: 'd', origin: { source: { basis: 'vibes', key: 'x:y', kind: 'x' }, area: 7 } }),
      ],
      total: 4,
    });
    expect(rows.map((r) => [r.id, r.source.basis, r.source.key, r.area])).toEqual([
      ['a', 'recorded', 'review:fix/x', 'jobs'],
      ['b', 'prose', null, null],
      ['c', 'missing', null, null],
      ['d', 'vibes', null, null],
    ]);
    expect(rows[0]?.source.branch).toBe('fix/x');
  });

  test('untriaged backlog items group by source key and by area; the unrecorded are one named group', () => {
    const { rows } = parseJobsPage({
      data: [
        job({ id: 'r1', priority: 'urgent', origin: { ...recorded('review', 'review:fix/a'), area: 'jobs' } }),
        job({ id: 'r2', origin: { ...recorded('review', 'review:fix/a'), area: 'jobs' } }),
        job({ id: 'r3', priority: 'urgent', origin: { ...recorded('review', 'review:fix/b'), area: 'web' } }),
        job({ id: 'g1', origin: recorded('agent-run', 'agent-run:665c7419', { id: '665c7419' }) }),
        job({ id: 'p1', origin: unrecorded('prose', 'jobs') }),
        job({ id: 'h1', origin: unrecorded('ad-hoc') }),
        job({ id: 'h2', origin: unrecorded('ad-hoc') }),
        job({ id: 'm1' }),
        // Taken in: marshalling's, not standing here.
        job({
          id: 't1',
          origin: recorded('review', 'review:fix/a'),
          steps: [{ kind: 'triage', status: 'completed', assignee_id: null }],
        }),
        // Not a backlog item: it stands, but is not filed with a source.
        job({ id: 'u1', kind: 'user-feedback', origin: unrecorded('missing', 'web') }),
      ],
      total: 10,
    });
    const p = provenance(waiting(rows, '2026-09-12'));
    expect(p.items).toBe(8);
    expect(p.kinds).toEqual([
      { kind: 'review', items: 3, sources: 2, urgent: 2 },
      { kind: 'agent-run', items: 1, sources: 1, urgent: 0 },
    ]);
    expect(p.kinds.map(kindSentence)).toEqual([
      '3 items from 2 car reviews, 2 urgent',
      '1 item from 1 agent run',
    ]);
    expect(p.groups.map((g) => [g.key, g.rows.map((r) => r.id), g.urgent])).toEqual([
      ['review:fix/a', ['r1', 'r2'], 1],
      ['agent-run:665c7419', ['g1'], 0],
      ['review:fix/b', ['r3'], 1],
    ]);
    // The group links to the packet it names when the reading carries an id.
    expect(p.groups.find((g) => g.key === 'agent-run:665c7419')?.id).toBe('665c7419');
    expect(p.unrecorded.rows.map((r) => r.id).sort()).toEqual(['h1', 'h2', 'm1', 'p1']);
    expect(p.unrecorded.byBasis).toEqual({ 'ad-hoc': 2, missing: 1, prose: 1 });
    expect(p.areas.map((a) => [a.area, a.rows.length, a.urgent])).toEqual([
      ['jobs', 3, 1],
      ['web', 1, 1],
    ]);
    expect(p.noArea.map((r) => r.id).sort()).toEqual(['g1', 'h1', 'h2', 'm1']);
  });

  test('nothing standing is an empty reading, not a missing one', () => {
    const p = provenance([]);
    expect(p.items).toBe(0);
    expect(p.kinds).toEqual([]);
    expect(p.unrecorded.rows).toEqual([]);
  });

  test('every source kind the door records has its nouns', () => {
    for (const kind of ['car', 'gate-run', 'agent-run', 'packet', 'review']) {
      expect(kindNoun(kind, 1)).not.toBe(kindNoun(kind, 2));
    }
    expect(kindNoun('review', 14)).toBe('car reviews');
  });
});
