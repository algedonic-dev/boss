import { afterEach, describe, expect, test } from 'bun:test';
import { readdirSync, readFileSync } from 'node:fs';
import {
  budgetText,
  costText,
  effortsRun,
  fetchAgentDetail,
  fetchAgents,
  HELD_LIMIT,
  heldText,
  parseAgents,
  parseRunRecords,
  RUN_WINDOW,
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
    });
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
