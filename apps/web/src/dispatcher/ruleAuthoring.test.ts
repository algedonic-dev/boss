import { afterEach, describe, expect, test } from 'bun:test';

import {
  buildRuleSpec,
  createDraft,
  draftRefusal,
  formFromVersion,
  listActiveRules,
  liveSource,
  type RuleForm,
  type RuleVersion,
} from './ruleAuthoring';

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

  test('the handler roster rides along, so the editor can say what a rule causes', async () => {
    // backlog 0034d5ef: nothing on the editor said which kinds a rule's
    // handlers emit, though the list's own read carried it.
    answer(
      200,
      JSON.stringify({
        rules: [],
        handler_emits: { 'jobs.auto-park': ['jobs.car.filed'], 'sensor.poll': [] },
        system_edges: [],
      }),
    );
    expect((await listActiveRules()).handlerEmits).toEqual({
      'jobs.auto-park': ['jobs.car.filed'],
      'sensor.poll': [],
    });
  });

  test('an older dispatcher with no authored_registry block reads as null', async () => {
    answer(200, JSON.stringify({ rules: [], handler_emits: {}, system_edges: [] }));
    expect((await listActiveRules()).authoredRegistry).toBeNull();
  });
});

/** An event-triggered form, overridable per test. */
const eventForm = (over: Partial<RuleForm> = {}): RuleForm => ({
  name: 'r',
  trigger: 'event',
  on_event: 'x',
  cadence: '',
  anchor_date: '',
  business_calendar: '',
  when: '',
  delay: '',
  do: [],
  ...over,
});

/** One stored row, as `GET /api/dispatcher/rules/{name}/versions` serves
 *  it: `on_event` and `schedule` are OMITTED when None
 *  (skip_serializing_if), and so is `source` for a product row. */
const row = (over: Partial<RuleVersion> = {}): RuleVersion => ({
  name: 'r',
  version: 1,
  status: 'active',
  when: null,
  do: [{ handler: 'jobs.spawn', args: { kind: '"restock"' } }],
  delay: null,
  created_by: null,
  published_by: null,
  retired_by: null,
  created_at: '2026-09-27T00:00:00Z',
  ...over,
});

// backlog b38360ed (page-audit bd5018e1 gap 1): 26 of 80 live rules are
// schedule-triggered and serve no `on_event` at all. The form seeded
// `onEvent = undefined`, buildRuleSpec threw on `.trim()`, and a draft
// typed from it carried no `schedule` — publishing it turned a schedule
// rule into an event rule. The form now carries the trigger the way the
// server's RawRule does: an event topic OR a schedule, exactly one.
describe('the trigger is an event OR a schedule, exactly one', () => {
  test('a schedule rule seeds a schedule form, with an empty topic rather than undefined', () => {
    const form = formFromVersion(
      row({
        name: 'cadence-silence-sweep-daily',
        schedule: { cadence: 'daily', anchor_date: '2026-09-01', business_calendar: 'us-banking' },
      }),
    );
    expect(form.trigger).toBe('schedule');
    expect(form.on_event).toBe('');
    expect(form.cadence).toBe('daily');
    expect(form.anchor_date).toBe('2026-09-01');
    expect(form.business_calendar).toBe('us-banking');
    expect(form.do).toEqual([{ handler: 'jobs.spawn', args: [{ key: 'kind', value: '"restock"' }] }]);
  });

  test('an event rule seeds an event form with every schedule field empty', () => {
    const form = formFromVersion(row({ on_event: 'step.done.*', when: 'kind = "x"', delay: '5m' }));
    expect(form.trigger).toBe('event');
    expect(form.on_event).toBe('step.done.*');
    expect([form.cadence, form.anchor_date, form.business_calendar]).toEqual(['', '', '']);
    expect(form.when).toBe('kind = "x"');
    expect(form.delay).toBe('5m');
  });

  test('a seeded schedule form builds a spec that keeps its schedule and names no topic', () => {
    const spec = buildRuleSpec(
      formFromVersion(row({ schedule: { cadence: 'every-5-minutes', anchor_date: '2026-09-17' } })),
    );
    expect(spec.schedule).toEqual({ cadence: 'every-5-minutes', anchor_date: '2026-09-17' });
    expect('on_event' in spec).toBe(false);
    expect(JSON.parse(JSON.stringify(spec))).not.toHaveProperty('on_event');
  });

  test('a topic typed into a schedule form is not sent while the trigger is the schedule', () => {
    const spec = buildRuleSpec(
      eventForm({ trigger: 'schedule', on_event: 'typed.by.mistake', cadence: ' weekly ', anchor_date: '2026-09-21' }),
    );
    expect(spec.schedule).toEqual({ cadence: 'weekly', anchor_date: '2026-09-21' });
    expect('on_event' in spec).toBe(false);
  });

  test('an event form sends its topic and no schedule, whatever the schedule fields hold', () => {
    const spec = buildRuleSpec(eventForm({ on_event: ' a.b ', cadence: 'daily', anchor_date: '2026-01-01' }));
    expect(spec.on_event).toBe('a.b');
    expect('schedule' in spec).toBe(false);
  });

  test('refusals name what is missing for the trigger chosen, and a whole spec has none', () => {
    expect(draftRefusal(buildRuleSpec(eventForm({ name: ' ' })))).toBe('Rule name is required.');
    expect(draftRefusal(buildRuleSpec(eventForm({ on_event: '' })))).toBe('on_event is required.');
    expect(draftRefusal(buildRuleSpec(eventForm({ trigger: 'schedule', anchor_date: '2026-01-01' })))).toBe(
      'A schedule needs a cadence.',
    );
    expect(draftRefusal(buildRuleSpec(eventForm({ trigger: 'schedule', cadence: 'daily' })))).toBe(
      'A schedule needs an anchor date.',
    );
    expect(draftRefusal(buildRuleSpec(eventForm()))).toBeNull();
    expect(draftRefusal(buildRuleSpec(eventForm({ trigger: 'schedule', cadence: 'daily', anchor_date: '2026-01-01' })))).toBeNull();
  });
});

// backlog 636b9868 (page-audit bd5018e1 gap 2): the draft door refuses a
// draft whose source differs from the name's live owner, and the editor
// sent none — so Save was refused for every tenant rule. The draft now
// restates the source of the rule's live row (design e187198f: the
// instance is the truth).
describe('the draft carries the source of the rule it versions', () => {
  test('the live source is the active row’s, else the newest row still in service, else none', () => {
    expect(liveSource([row({ version: 1, status: 'active', source: 'tenant:algedonic' })])).toBe('tenant:algedonic');
    expect(liveSource([row({ version: 1, status: 'active' })])).toBeNull();
    expect(
      liveSource([
        row({ version: 1, status: 'retired', source: 'tenant:old' }),
        row({ version: 2, status: 'draft', source: 'tenant:algedonic' }),
      ]),
    ).toBe('tenant:algedonic');
    // Every row retired: nobody owns the name, so the draft restates nobody.
    expect(liveSource([row({ version: 1, status: 'retired', source: 'tenant:old' })])).toBeNull();
  });

  test('createDraft sends the source beside the spec, and omits the key for a product rule', async () => {
    const bodies: unknown[] = [];
    globalThis.fetch = (async (_input: RequestInfo | URL, init?: RequestInit) => {
      bodies.push(JSON.parse(String(init?.body)));
      return new Response(JSON.stringify(row()), { status: 201 });
    }) as unknown as typeof fetch;
    const spec = buildRuleSpec(eventForm({ on_event: 'a.b' }));
    await createDraft(spec, 'tenant:algedonic');
    await createDraft(spec, null);
    expect(bodies[0]).toEqual({ ...spec, source: 'tenant:algedonic' });
    expect(bodies[1]).toEqual(spec);
    expect(bodies[1]).not.toHaveProperty('source');
  });
});

describe('buildRuleSpec', () => {
  test('trims fields, drops empty handlers + arg keys, nulls empty when/delay', () => {
    const form: RuleForm = {
      ...eventForm(),
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
    const spec = buildRuleSpec(
      eventForm({ on_event: 'step.done.*', when: ' on_hand <= 0 ', delay: ' 5m ' }),
    );
    expect(spec.when).toBe('on_hand <= 0');
    expect(spec.delay).toBe('5m');
    expect(spec.do).toEqual([]);
  });

  test('preserves arg value whitespace (only keys are trimmed)', () => {
    const spec = buildRuleSpec(
      eventForm({ do: [{ handler: 'h', args: [{ key: 'memo', value: '  spaced value  ' }] }] }),
    );
    expect(spec.do[0]!.args).toEqual({ memo: '  spaced value  ' });
  });
});
