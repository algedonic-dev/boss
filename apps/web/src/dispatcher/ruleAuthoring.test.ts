import { afterEach, describe, expect, test } from 'bun:test';

import { buildRuleSpec, listActiveRules, type RuleForm } from './ruleAuthoring';

const realFetch = globalThis.fetch;
afterEach(() => {
  globalThis.fetch = realFetch;
});

/** Answer every fetch with one canned response, recording the URLs. */
function answer(status: number, body: string): string[] {
  const urls: string[] = [];
  globalThis.fetch = (async (input: RequestInfo | URL) => {
    urls.push(String(input));
    return new Response(body, { status });
  }) as unknown as typeof fetch;
  return urls;
}

// Both throw branches of the rules page's one read, pinned where they
// live (backlog a9c4ad40): the page's mocked spec reaches each through
// the DOM, and these name which branch spelled the message.
describe('listActiveRules', () => {
  test('a non-2xx throws the status and the body', async () => {
    answer(503, 'dispatcher down');
    await expect(listActiveRules()).rejects.toThrow('HTTP 503: dispatcher down');
  });

  test('a 200 carrying error throws that error, never an empty registry', async () => {
    // What boss-dispatcher http.rs `rules` answers when dispatcher_rules
    // will not load: HTTP 200, no rules, and the load error.
    answer(
      200,
      JSON.stringify({
        rules: [],
        handler_emits: {},
        system_edges: [],
        authored_registry: null,
        error: 'load dispatcher_rules: relation "dispatcher_rules" does not exist',
      }),
    );
    await expect(listActiveRules()).rejects.toThrow(
      'load dispatcher_rules: relation "dispatcher_rules" does not exist',
    );
  });

  test('a clean 200 returns the rules AND where their whys were read from', async () => {
    // backlog f9e34a2c: the authored_registry block is what tells "no
    // rule records a why" from "the whys could not be read", so the read
    // hands it to the page rather than dropping it.
    const urls = answer(
      200,
      JSON.stringify({
        rules: [{ name: 'r', on_event: 'x.y', when: null, do: [], version: 1 }],
        handler_emits: {},
        system_edges: [],
        authored_registry: { dir: null, rules: 0, error: 'BOSS_DISPATCHER_RULES is not set' },
      }),
    );
    const read = await listActiveRules();
    expect(urls).toEqual(['/api/dispatcher/rules']);
    expect(read.rules.map((r) => r.name)).toEqual(['r']);
    expect(read.authoredRegistry).toEqual({
      dir: null,
      rules: 0,
      error: 'BOSS_DISPATCHER_RULES is not set',
    });
  });

  test('an older dispatcher with no authored_registry block reads as null', async () => {
    answer(200, JSON.stringify({ rules: [], handler_emits: {}, system_edges: [] }));
    expect((await listActiveRules()).authoredRegistry).toBeNull();
  });
});

describe('buildRuleSpec', () => {
  test('trims fields, drops empty handlers + arg keys, nulls empty when/delay', () => {
    const form: RuleForm = {
      name: '  spawn-restock ',
      on_event: ' inventory.parts.consumed ',
      when: '  ',
      delay: '',
      do: [
        {
          handler: ' jobs.spawn ',
          args: [
            { key: ' kind ', value: '"restock"' },
            { key: '', value: 'dropped' }, // empty key → dropped
          ],
        },
        { handler: '   ', args: [{ key: 'k', value: 'v' }] }, // empty handler → step dropped
      ],
    };
    const spec = buildRuleSpec(form);
    expect(spec.name).toBe('spawn-restock');
    expect(spec.on_event).toBe('inventory.parts.consumed');
    expect(spec.when).toBeNull();
    expect(spec.delay).toBeNull();
    expect(spec.do).toEqual([{ handler: 'jobs.spawn', args: { kind: '"restock"' } }]);
  });

  test('keeps non-empty when/delay (trimmed) and an empty do list', () => {
    const spec = buildRuleSpec({
      name: 'r',
      on_event: 'step.done.*',
      when: ' on_hand <= 0 ',
      delay: ' 5m ',
      do: [],
    });
    expect(spec.when).toBe('on_hand <= 0');
    expect(spec.delay).toBe('5m');
    expect(spec.do).toEqual([]);
  });

  test('preserves arg value whitespace (only keys are trimmed)', () => {
    const spec = buildRuleSpec({
      name: 'r',
      on_event: 'x',
      when: '',
      delay: '',
      do: [{ handler: 'h', args: [{ key: 'memo', value: '  spaced value  ' }] }],
    });
    expect(spec.do[0]!.args).toEqual({ memo: '  spaced value  ' });
  });
});
