import { afterEach, describe, expect, test } from 'bun:test';
import { readdirSync, readFileSync } from 'node:fs';
import {
  budgetText,
  changeText,
  costText,
  effortsRun,
  fetchAgentDetail,
  fetchAgents,
  HELD_LIMIT,
  heldText,
  parseAgents,
  parseRunRecords,
  parseSeats,
  RUN_WINDOW,
  seatAgent,
  seatsText,
  type Agent,
} from './agents';

const realFetch = globalThis.fetch;
afterEach(() => {
  globalThis.fetch = realFetch;
});

// The registry's one row as the system of record served it on
// 2026-09-27 (`boss-api GET /api/agents`, backlog 62988516).
const LIVE = {
  data: [
    {
      aliases: ['claude@algedonic.dev'],
      default_model: 'opus-5[1m]',
      department: 'engineering',
      display_name: 'Claude (engineering)',
      hourly_budget_usd_micros: 40000000,
      id: 'agent-claude',
      max_concurrent_runs: 12,
      role: 'engineering-agent',
    },
  ],
  total: 1,
};

const claude: Agent = {
  id: 'agent-claude',
  name: 'Claude (engineering)',
  aliases: ['claude@algedonic.dev'],
  role: 'engineering-agent',
  department: 'engineering',
  defaultModel: 'opus-5[1m]',
  // A registry that predates the column serves no key: records none.
  modelClass: null,
  hourlyBudgetUsdMicros: 40000000,
  // The served `max_concurrent_runs: 12` is not read: it bounds nothing
  // since backlog 3e927996.
  // A registry that predates the seats column serves no key: seated nowhere.
  stations: [],
};

describe('parseAgents', () => {
  test('reads every field the registry serves', () => {
    expect(parseAgents(LIVE)).toEqual([claude]);
  });

  // Backlog 06d8988b: the class is the row's own column, never the model.
  test('reads the model class the row records, and never derives one from the model', () => {
    const [a] = parseAgents({ data: [{ ...LIVE.data[0], model_class: 'opus' }] });
    expect(a?.modelClass).toBe('opus');
    const [b] = parseAgents({ data: [{ ...LIVE.data[0], model_class: null }] });
    expect(b?.modelClass).toBeNull();
  });

  test('a null the registry serves stays null — never a zero or an empty string', () => {
    const [a] = parseAgents({
      data: [
        {
          id: 'agent-x',
          display_name: 'X',
          default_model: 'opus-5[1m]',
          model_class: null,
          aliases: [],
          role: null,
          department: null,
          hourly_budget_usd_micros: null,
          max_concurrent_runs: null,
        },
      ],
    });
    expect(a).toEqual({
      id: 'agent-x',
      name: 'X',
      aliases: [],
      role: null,
      department: null,
      defaultModel: 'opus-5[1m]',
      modelClass: null,
      hourlyBudgetUsdMicros: null,
      stations: [],
    });
  });

  // Backlog cdbebc22: the seats are the row's own ordered list.
  test('reads the stations the row seats it at, in the order it serves them', () => {
    const [a] = parseAgents({ data: [{ ...LIVE.data[0], stations: ['algedonic', 'build'] }] });
    expect(a?.stations).toEqual(['algedonic', 'build']);
  });

  test('a row without an id is dropped; a bare array parses too', () => {
    expect(parseAgents([{ display_name: 'nobody' }, LIVE.data[0]])).toEqual([claude]);
    expect(parseAgents(null)).toEqual([]);
  });
});

describe('the words for a cap', () => {
  test('a budget is dollars an hour, and an undeclared one says so', () => {
    expect(budgetText(40000000)).toBe('$40.00 an hour');
    expect(budgetText(0)).toBe('$0.00 an hour');
    expect(budgetText(null)).toBe('not declared');
  });
});

// ---------------------------------------------------------------------
// The finish record — `GET /api/agent-runs`, one row per run that
// reached a terminal: what it cost. Trimmed from the live payload
// measured 2026-09-23 (usd_micros null on that row, as on 124 of the
// 200 rows the cost roll-up counted that day). Moved here with the
// reader from the retired Crew Board's tests (backlog 462fb22a).
// ---------------------------------------------------------------------

const RUN_RECORDS_RAW = [
  {
    run_id: 'fe591182-80ba-4391-9ff6-67f5a3eb85b2',
    actor_id: 'agent-claude',
    model: 'opus-5[1m]',
    started_at: '2026-09-23T16:55:19.580182Z',
    finished_at: '2026-09-23T18:13:05.439Z',
    outcome: 'success',
    total_tokens: null,
    job_id: '7f3e871a-da38-4e46-a61d-fd67a3a279f7',
    branch: 'fix/jobs-list-refuses-an-unknown-query-parameter',
    detail: { agent_run: 'fe591182-80ba-4391-9ff6-67f5a3eb85b2', effort: 'high', step: 'build' },
    usd_micros: null,
  },
  {
    run_id: 'a1b2c3d4-0000-4000-8000-000000000001',
    actor_id: 'agent-claude',
    started_at: '2026-09-17T10:00:00Z',
    finished_at: '2026-09-17T10:30:00Z',
    outcome: 'died',
    total_tokens: 1200000,
    job_id: null,
    branch: null,
    detail: {},
    usd_micros: 3456789,
  },
  { actor_id: 'no run id, not a row' },
];

describe('parseRunRecords', () => {
  test('reads what each finished run cost, and absence as null', () => {
    const records = parseRunRecords(RUN_RECORDS_RAW);
    expect(records.length).toBe(2);
    const live = records[0]!;
    const priced = records[1]!;
    expect(live.runId).toBe('fe591182-80ba-4391-9ff6-67f5a3eb85b2');
    expect(live.actor).toBe('agent-claude');
    expect(live.outcome).toBe('success');
    expect(live.branch).toBe('fix/jobs-list-refuses-an-unknown-query-parameter');
    expect(live.effort).toBe('high');
    expect(live.minutes).toBe(78);
    expect(live.usdMicros).toBeNull();
    expect(costText(live)).toBe('not priced');
    // The older era: no branch, no effort recorded — said, not guessed.
    expect(priced.branch).toBeNull();
    expect(priced.effort).toBeNull();
    expect(priced.tokens).toBe(1200000);
    expect(costText(priced)).toBe('$3.46');
  });

  test('accepts a page as readily as a bare array', () => {
    expect(parseRunRecords({ data: RUN_RECORDS_RAW }).length).toBe(2);
    expect(parseRunRecords(null)).toEqual([]);
  });
});

describe('effortsRun', () => {
  test('counts each effort in the window, most-run first, unrecorded named as such', () => {
    const runs = parseRunRecords([
      { run_id: 'a', detail: { effort: 'high' } },
      { run_id: 'b', detail: { effort: 'medium' } },
      { run_id: 'c', detail: { effort: 'high' } },
      { run_id: 'd', detail: {} },
    ]);
    expect(effortsRun(runs)).toBe('high ×2, medium ×1, not recorded ×1');
    expect(effortsRun([])).toBe('');
  });
});

describe('heldText', () => {
  test('a count, and a floor when a read reached its limit', () => {
    expect(heldText({ steps: 436, capped: false })).toBe('436');
    expect(heldText({ steps: 1000, capped: true })).toBe(`at least 1000`);
    expect(heldText({ steps: 0, capped: false })).toBe('0');
  });
});

describe('fetchAgents', () => {
  test('a failed read is failed, naming the status — never an empty registry', async () => {
    globalThis.fetch = (async () => new Response('no', { status: 403 })) as unknown as typeof fetch;
    const r = await fetchAgents();
    expect(r.kind).toBe('failed');
    expect(r.kind === 'failed' ? r.error : '').toContain('403');
  });
});

describe('fetchAgentDetail', () => {
  test('reads its runs by id and its held steps under the id AND every alias, each step once', async () => {
    const asked: string[] = [];
    globalThis.fetch = (async (url: string) => {
      asked.push(url);
      if (url.startsWith('/api/agent-runs')) {
        return Response.json([
          { run_id: 'r1', actor_id: 'agent-claude', outcome: 'success', detail: { effort: 'high' } },
        ]);
      }
      // The same step read under both spellings counts once.
      const both = { id: 's-both' };
      return Response.json({
        data: url.includes('agent-claude')
          ? [{ step: { id: 's1' } }, { step: both }]
          : [{ step: { id: 's2' } }, { step: both }],
      });
    }) as unknown as typeof fetch;
    const d = await fetchAgentDetail(claude);
    expect(asked).toEqual([
      `/api/agent-runs?actor_id=agent-claude&limit=${RUN_WINDOW}`,
      `/api/jobs/assignments?assignee_id=agent-claude&limit=${HELD_LIMIT}`,
      `/api/jobs/assignments?assignee_id=claude%40algedonic.dev&limit=${HELD_LIMIT}`,
    ]);
    expect(d.runs.kind === 'ready' ? d.runs.data.map((r) => r.runId) : null).toEqual(['r1']);
    expect(d.held).toEqual({ kind: 'ready', data: { steps: 3, capped: false } });
  });

  test('a held read that fills its limit is a floor; one that fails fails the count', async () => {
    const full = Array.from({ length: HELD_LIMIT }, (_, i) => ({ step: { id: `s${i}` } }));
    globalThis.fetch = (async (url: string) =>
      url.startsWith('/api/agent-runs')
        ? Response.json([])
        : Response.json({ data: full })) as unknown as typeof fetch;
    const one: Agent = { ...claude, aliases: [] };
    expect((await fetchAgentDetail(one)).held).toEqual({
      kind: 'ready',
      data: { steps: HELD_LIMIT, capped: true },
    });

    globalThis.fetch = (async (url: string) =>
      url.includes('claude%40')
        ? new Response('down', { status: 502 })
        : url.startsWith('/api/agent-runs')
          ? Response.json([])
          : Response.json({ data: [] })) as unknown as typeof fetch;
    const d = await fetchAgentDetail(claude);
    expect(d.held.kind).toBe('failed');
    expect(d.runs).toEqual({ kind: 'ready', data: [] });
  });
});

// ---------------------------------------------------------------------
// Seating an agent from the website (backlog cdbebc22, David
// 2026-11-19: "I should place you at the root station formally").
// ---------------------------------------------------------------------

// The row as `GET /api/agents` serves it now, every key `AgentRow`
// carries (the shape of builder run 07e2e58c's prepared row).
const SERVED = {
  aliases: ['claude@algedonic.dev'],
  default_model: 'opus-5-5',
  department: 'it',
  display_name: 'Claude (engineering)',
  factory_rotation: true,
  hourly_budget_usd_micros: 40000000,
  id: 'agent-claude',
  max_concurrent_runs: 12,
  model_class: 'opus',
  role: 'platform-admin',
  stations: ['build'],
};

type Call = Readonly<{ url: string; method: string; body: unknown }>;

/// A fetch that answers the roster read with `rows` and the seat door
/// with `door`.
function registry(
  rows: ReadonlyArray<Record<string, unknown>>,
  door: () => Response,
): { calls: Call[] } {
  const calls: Call[] = [];
  globalThis.fetch = (async (url: string, init?: RequestInit) => {
    const method = init?.method ?? 'GET';
    calls.push({ url, method, body: init?.body ? JSON.parse(String(init.body)) : null });
    if (url.endsWith('/stations')) return door();
    return Response.json({ data: rows, total: rows.length });
  }) as unknown as typeof fetch;
  return { calls };
}

/// The seat door's receipt for SERVED moved from `from` to `to`: the row
/// before and after, as its own transaction read them, and the change.
const receipt = (from: ReadonlyArray<string>, to: ReadonlyArray<string>) => ({
  id: 'agent-claude',
  before: { ...SERVED, stations: from },
  after: { ...SERVED, stations: to },
  changes: [{ field: 'stations', from, to }],
});

describe('parseSeats', () => {
  test('an ordered list from commas or spaces, blanks dropped, order and repeats kept for the door', () => {
    expect(parseSeats('algedonic, build')).toEqual(['algedonic', 'build']);
    expect(parseSeats('  algedonic\nbuild  build ,, ')).toEqual(['algedonic', 'build', 'build']);
    expect(parseSeats('   ')).toEqual([]);
  });
});

describe('the words for seats and changes', () => {
  test('seats in serving order, and none said in words', () => {
    expect(seatsText(['algedonic', 'build'])).toBe('algedonic → build');
    expect(seatsText([])).toBe('seated nowhere');
  });

  test('a change reads as the publish line spells it', () => {
    expect(changeText({ field: 'stations', from: ['build'], to: ['algedonic'] })).toBe(
      'stations: ["build"] → ["algedonic"]',
    );
    expect(changeText({ field: 'role', from: null, to: 'platform-admin' })).toBe(
      'role: null → platform-admin',
    );
  });
});

describe('seatAgent', () => {
  // Review 1a49c3a7 finding 1: the first cut re-read the row and took the
  // WHOLE row back through the batch, so a role another writer narrowed
  // between that read and the write was restored. The seat door writes
  // the seats alone, and only over the seats this page showed.
  test('sends the seats it showed and the seats asked for to the seat door, and nothing else', async () => {
    const { calls } = registry([SERVED], () => Response.json(receipt(['build'], ['algedonic'])));
    const out = await seatAgent('agent-claude', ['build'], ['algedonic']);
    expect(calls.map((c) => `${c.method} ${c.url}`)).toEqual([
      'PUT /api/agents/agent-claude/stations',
    ]);
    expect(calls[0]?.body).toEqual({ expected: ['build'], stations: ['algedonic'] });
    expect(out).toEqual({
      kind: 'applied',
      before: expect.objectContaining({ id: 'agent-claude', stations: ['build'] }),
      after: expect.objectContaining({ id: 'agent-claude', stations: ['algedonic'] }),
      changes: [{ field: 'stations', from: ['build'], to: ['algedonic'] }],
    });
  });

  test('seats that moved since the page read them are stale: the door’s reason, and the row as held now', async () => {
    const why =
      'agent agent-claude: its seats changed since they were read — read ["build"], the registry holds ["design-review"] now; nothing was written';
    const { calls } = registry([{ ...SERVED, stations: ['design-review'] }], () => new Response(why, { status: 409 }));
    const out = await seatAgent('agent-claude', ['build'], ['algedonic']);
    expect(out.kind).toBe('stale');
    if (out.kind !== 'stale') return;
    expect(out.why).toBe(why);
    expect(out.held).toEqual({
      kind: 'ready',
      data: expect.objectContaining({ id: 'agent-claude', stations: ['design-review'] }),
    });
    expect(calls.map((c) => c.method)).toEqual(['PUT', 'GET']);
  });

  test('a seat the door refuses is refused with the door’s own reason', async () => {
    const why =
      'agent agent-claude: station `nowhere` is not an active station in the station registry';
    const { calls } = registry([SERVED], () => new Response(why, { status: 422 }));
    const out = await seatAgent('agent-claude', ['build'], ['nowhere']);
    expect(out).toEqual({ kind: 'refused', status: 422, why });
    expect(calls.length).toBe(1);
  });

  test('a session below the operator tier is refused, and an empty refusal still says why', async () => {
    registry([SERVED], () => new Response('', { status: 403 }));
    const out = await seatAgent('agent-claude', ['build'], ['algedonic']);
    expect(out.kind).toBe('refused');
    expect(out.kind === 'refused' ? out.status : null).toBe(403);
    expect(out.kind === 'refused' ? out.why : '').toContain('operator tier');
    expect(out.kind === 'refused' ? out.why : '').toContain('passkey');
  });

  test('a write with no answer, or a server failure, is unknown — never refused, never landed — and the row is read again beside it', async () => {
    registry([SERVED], () => {
      throw new TypeError('network down');
    });
    const silent = await seatAgent('agent-claude', ['build'], ['algedonic']);
    expect(silent.kind).toBe('unknown');
    expect(silent.kind === 'unknown' ? silent.why : '').toContain('network down');

    registry([SERVED], () => new Response('boom', { status: 500 }));
    const out = await seatAgent('agent-claude', ['build'], ['algedonic']);
    expect(out).toEqual({
      kind: 'unknown',
      why: 'HTTP 500: boom',
      held: { kind: 'ready', data: expect.objectContaining({ stations: ['build'] }) },
    });
  });

  // Review 1a49c3a7 finding 2: a 2xx whose body is not a receipt used to
  // read as "no changes", which the page drew as "the registry already
  // held these seats". An answer that is not a receipt says nothing.
  test('a success whose body is not a seat receipt is unknown, never a no-op', async () => {
    for (const body of [
      'not json',
      '{}',
      JSON.stringify({ changes: [] }),
      JSON.stringify({ ...receipt(['build'], ['algedonic']), changes: 'none' }),
      JSON.stringify({ ...receipt(['build'], ['algedonic']), after: { ...SERVED, id: 'agent-other' } }),
      JSON.stringify({ ...receipt(['build'], ['algedonic']), after: { id: 'agent-claude' } }),
    ]) {
      registry([SERVED], () => new Response(body, { status: 200 }));
      const out = await seatAgent('agent-claude', ['build'], ['algedonic']);
      expect(out.kind).toBe('unknown');
      if (out.kind !== 'unknown') continue;
      expect(out.why).toContain('HTTP 200');
      expect(out.held.kind).toBe('ready');
    }
  });

  // Review 155c150f P2: the receipt's rows went through the directory's
  // tolerant parser, which DROPS a seat that is not a non-empty string, so
  // `after.stations: [null, 17]` read as "seated nowhere" and the page
  // said the door had changed nothing. A seat list the receipt cannot
  // state exactly is no receipt: unknown, and the row read again.
  test('a receipt whose seats are not all station ids is unknown, never coerced to fewer seats', async () => {
    for (const [before, after] of [
      [['build'], [null, 17]],
      [['build'], ['algedonic', '']],
      [[null], ['algedonic']],
      [['build'], [{ id: 'algedonic' }]],
    ] as const) {
      const { calls } = registry([SERVED], () =>
        Response.json({
          ...receipt(['build'], ['algedonic']),
          before: { ...SERVED, stations: before },
          after: { ...SERVED, stations: after },
          changes: [],
        }),
      );
      const out = await seatAgent('agent-claude', ['build'], ['algedonic']);
      expect(out.kind).toBe('unknown');
      expect(calls.map((c) => c.method)).toEqual(['PUT', 'GET']);
      if (out.kind !== 'unknown') continue;
      expect(out.why).toContain('HTTP 200');
      expect(out.held).toEqual({
        kind: 'ready',
        data: expect.objectContaining({ stations: ['build'] }),
      });
    }
  });

  test('a receipt seating it nowhere is a receipt: an empty list is a claim, not a dropped one', async () => {
    const { calls } = registry([SERVED], () => Response.json(receipt(['build'], [])));
    const out = await seatAgent('agent-claude', ['build'], []);
    expect(out).toEqual({
      kind: 'applied',
      before: expect.objectContaining({ stations: ['build'] }),
      after: expect.objectContaining({ stations: [] }),
      changes: [{ field: 'stations', from: ['build'], to: [] }],
    });
    expect(calls.map((c) => c.method)).toEqual(['PUT']);
  });

  test('a receipt naming no change is the registry already holding the seats', async () => {
    registry([SERVED], () => Response.json({ ...receipt(['build'], ['build']), changes: [] }));
    const out = await seatAgent('agent-claude', ['build'], ['build']);
    expect(out.kind).toBe('applied');
    expect(out.kind === 'applied' ? out.changes : null).toEqual([]);
  });
});

// David, 2026-09-26 (6a123f1f): agents are never People rows and never
// headcount. Measured 2026-09-27: the roster (`/api/people`) held
// emp-audit and emp-david, and no People or HR source read the agents
// registry. This holds the second half: the directory is this page.
describe('the People roster', () => {
  // The HR page and the People pages went with step 1 of design 01fa5bdf;
  // what is left of people/ is still held to it.
  test('no People source reads the agents registry', () => {
    const readers = ['../../people'].flatMap((dir) => {
      const at = new URL(`${dir}/`, import.meta.url);
      return readdirSync(at)
        .filter((f) => /\.(ts|svelte)$/.test(f) && !f.endsWith('.test.ts'))
        .filter((f) => readFileSync(new URL(f, at), 'utf8').includes('/api/agents'))
        .map((f) => `${dir}/${f}`);
    });
    expect(readers).toEqual([]);
  });
});
