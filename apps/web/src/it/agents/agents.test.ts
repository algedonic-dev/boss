import { afterEach, describe, expect, test } from 'bun:test';
import { readdirSync, readFileSync } from 'node:fs';
import {
  budgetText,
  capText,
  effortsRun,
  fetchAgentDetail,
  fetchAgents,
  HELD_LIMIT,
  heldText,
  parseAgents,
  RUN_WINDOW,
  type Agent,
} from './agents';
import { parseRunRecords } from '../crew/crew';

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
  hourlyBudgetUsdMicros: 40000000,
  maxConcurrentRuns: 12,
};

describe('parseAgents', () => {
  test('reads every field the registry serves', () => {
    expect(parseAgents(LIVE)).toEqual([claude]);
  });

  test('a null the registry serves stays null — never a zero or an empty string', () => {
    const [a] = parseAgents({
      data: [
        {
          id: 'agent-x',
          display_name: 'X',
          default_model: 'opus-5[1m]',
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
      hourlyBudgetUsdMicros: null,
      maxConcurrentRuns: null,
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

  test('a concurrency cap is a count, and an undeclared one says so', () => {
    expect(capText(12)).toBe('12');
    expect(capText(0)).toBe('0');
    expect(capText(null)).toBe('not declared');
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
  test('no People or HR surface reads the agents registry', () => {
    const readers = ['../../people', '../../hr'].flatMap((dir) => {
      const at = new URL(`${dir}/`, import.meta.url);
      return readdirSync(at)
        .filter((f) => /\.(ts|svelte)$/.test(f) && !f.endsWith('.test.ts'))
        .filter((f) => readFileSync(new URL(f, at), 'utf8').includes('/api/agents'))
        .map((f) => `${dir}/${f}`);
    });
    expect(readers).toEqual([]);
  });
});
