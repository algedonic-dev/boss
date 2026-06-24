import { describe, expect, test } from 'bun:test';
import { buildCascade, filterCascadeFromEvents, topicMatch } from './cascadeToGraph';
import type { DispatcherRules } from './types';

describe('topicMatch', () => {
  test('exact, single-wildcard, trailing-wildcard', () => {
    expect(topicMatch('step.done.billing', 'step.done.billing')).toBe(true);
    expect(topicMatch('step.ready.*', 'step.ready.delegate-subjob')).toBe(true);
    expect(topicMatch('step.done.*', 'step.done.billing')).toBe(true);
    expect(topicMatch('step.done.*', 'step.done.billing.detail')).toBe(false);
    expect(topicMatch('step.>', 'step.done.billing.detail')).toBe(true);
    expect(topicMatch('inventory.item.consumed', 'inventory.item.received')).toBe(false);
  });
});

describe('buildCascade', () => {
  test('a self-feeding rule forms a highlighted cycle', () => {
    const data: DispatcherRules = {
      rules: [
        { name: 'loop', on_event: 'ev.x', when: null, do: [{ handler: 'h1', args: {} }], version: 1 },
      ],
      handler_emits: { h1: ['ev.x'] },
      system_edges: [],
    };
    const byId = new Map(buildCascade(data).nodes.map((n) => [n.id, n]));
    expect(byId.get('evt:ev.x')?.inCycle).toBe(true);
    expect(byId.get('rule:loop')?.inCycle).toBe(true);
    expect(byId.get('hdl:h1')?.inCycle).toBe(true);
  });

  test('system + wildcard edges close the DAG-advance loop', () => {
    // jobs.complete_step --emit--> jobs.step.completed --system--> step.ready.*
    //   --trigger--> marker --do--> jobs.complete_step   (a cycle)
    const data: DispatcherRules = {
      rules: [
        {
          name: 'marker',
          on_event: 'step.ready.*',
          when: null,
          do: [{ handler: 'jobs.complete_step', args: {} }],
          version: 1,
        },
      ],
      handler_emits: { 'jobs.complete_step': ['jobs.step.completed'] },
      system_edges: [
        { from: 'jobs.step.completed', to: 'step.ready.*', kind: 'jobs-api', label: 'readies dependents' },
      ],
    };
    const byId = new Map(buildCascade(data).nodes.map((n) => [n.id, n]));
    expect(byId.get('rule:marker')?.inCycle).toBe(true);
    expect(byId.get('hdl:jobs.complete_step')?.inCycle).toBe(true);
  });

  test('linear chain has no cycle; a wildcard match edge bridges topics', () => {
    const data: DispatcherRules = {
      rules: [
        { name: 'agg', on_event: 'metric.*', when: null, do: [{ handler: 'sink', args: {} }], version: 1 },
        { name: 'src', on_event: 'tick', when: null, do: [{ handler: 'emit_cpu', args: {} }], version: 1 },
      ],
      handler_emits: { emit_cpu: ['metric.cpu'], sink: [] },
      system_edges: [],
    };
    const g = buildCascade(data);
    const byId = new Map(g.nodes.map((n) => [n.id, n]));
    expect(byId.get('rule:agg')?.inCycle).toBe(false);
    // emit_cpu emits the concrete metric.cpu, which the metric.* trigger
    // covers — a `match` edge must bridge them.
    expect(g.edges.some((e) => e.kind === 'match' && e.source === 'evt:metric.cpu' && e.target === 'evt:metric.*')).toBe(true);
  });
});

describe('filterCascadeFromEvents', () => {
  // a → r1 → h1 → (emit) b → r2 → h2(sink)
  const data: DispatcherRules = {
    rules: [
      { name: 'r1', on_event: 'a', when: null, do: [{ handler: 'h1', args: {} }], version: 1 },
      { name: 'r2', on_event: 'b', when: null, do: [{ handler: 'h2', args: {} }], version: 1 },
    ],
    handler_emits: { h1: ['b'], h2: [] },
    system_edges: [],
  };

  test('empty selection returns the full cascade unchanged', () => {
    const full = buildCascade(data);
    expect(filterCascadeFromEvents(full, [])).toBe(full);
  });

  test('forward cascade from a trigger keeps its whole downstream chain', () => {
    const f = filterCascadeFromEvents(buildCascade(data), ['a']);
    const ids = new Set(f.nodes.map((n) => n.id));
    expect(ids).toEqual(new Set(['evt:a', 'rule:r1', 'hdl:h1', 'evt:b', 'rule:r2', 'hdl:h2']));
    expect(f.edges.every((e) => ids.has(e.source) && ids.has(e.target))).toBe(true);
  });

  test('a downstream trigger excludes upstream-only nodes', () => {
    const f = filterCascadeFromEvents(buildCascade(data), ['b']);
    const ids = new Set(f.nodes.map((n) => n.id));
    expect(ids).toEqual(new Set(['evt:b', 'rule:r2', 'hdl:h2']));
    expect(ids.has('evt:a')).toBe(false);
    expect(ids.has('rule:r1')).toBe(false);
  });
});
