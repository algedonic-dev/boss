// Pure transforms — the rules that keep a JobKind's step list coherent
// as it's edited from the graphical surface. Run via `bun test`.

import { describe, expect, test } from 'bun:test';
import type { StepSpec } from './jobKindTypes';
import {
  freshSlug,
  makeStep,
  patchStep,
  removeStep,
  renameSlug,
} from './stepEdits';

function step(title: string, ready_when = ''): StepSpec {
  return {
    title,
    kind: 'generic',
    ready_when,
    terminal: null,
    title_template: '',
    sign_offs_required: [],
    authority_role: null,
    metadata_defaults: {},
  };
}

describe('freshSlug', () => {
  test('kebab-cases the step-type kind', () => {
    expect(freshSlug('sign-off', [])).toBe('sign-off');
    expect(freshSlug('Acknowledgment', [])).toBe('acknowledgment');
  });

  test('dedupes against existing slugs with a numeric suffix', () => {
    const existing = [step('sign-off'), step('sign-off-2')];
    expect(freshSlug('sign-off', existing)).toBe('sign-off-3');
  });

  test('falls back to "step" when the kind has no usable chars', () => {
    expect(freshSlug('___', [])).toBe('step');
  });
});

describe('makeStep', () => {
  test('first step of a draft is a trigger (ready_when = "true")', () => {
    expect(makeStep('task', []).ready_when).toBe('true');
  });

  test('later steps start blank for the author to wire', () => {
    const made = makeStep('task', [step('first', 'true')]);
    expect(made.ready_when).toBe('');
    expect(made.kind).toBe('task');
    expect(made.title).toBe('task');
  });
});

describe('patchStep', () => {
  test('overlays only the matching step', () => {
    const steps = [step('a'), step('b')];
    const next = patchStep(steps, 'b', { kind: 'sign-off' });
    expect(next[0]!.kind).toBe('generic');
    expect(next[1]!.kind).toBe('sign-off');
  });

  test('returns a new array (no mutation)', () => {
    const steps = [step('a')];
    const next = patchStep(steps, 'a', { title_template: 'X' });
    expect(next).not.toBe(steps);
    expect(steps[0]!.title_template).toBe('');
  });
});

describe('removeStep', () => {
  test('drops the named step, leaves the rest', () => {
    const next = removeStep([step('a'), step('b'), step('c')], 'b');
    expect(next.map((s) => s.title)).toEqual(['a', 'c']);
  });
});

describe('renameSlug', () => {
  test('renames the step title', () => {
    const next = renameSlug([step('old')], 'old', 'new');
    expect(next[0]!.title).toBe('new');
  });

  test('rewrites .done and .metadata references in other steps', () => {
    const steps = [
      step('demand', 'true'),
      step('brew', 'steps.demand.done AND steps.demand.metadata.outcome = "go"'),
    ];
    const next = renameSlug(steps, 'demand', 'demand-check');
    expect(next[1]!.ready_when).toBe(
      'steps.demand-check.done AND steps.demand-check.metadata.outcome = "go"',
    );
  });

  test('does NOT corrupt a longer slug that shares a prefix', () => {
    const steps = [
      step('demand'),
      step('demand-check'),
      step('gate', 'steps.demand.done AND steps.demand-check.done'),
    ];
    const next = renameSlug(steps, 'demand', 'forecast');
    expect(next[2]!.ready_when).toBe(
      'steps.forecast.done AND steps.demand-check.done',
    );
    // the longer slug's own title is untouched
    expect(next[1]!.title).toBe('demand-check');
  });

  test('rewrites a bare reference with no suffix', () => {
    const next = renameSlug([step('gate', 'steps.a')], 'a', 'b');
    // `gate` doesn't reference itself; the `a` it gates on isn't in the
    // list, but the rewrite is purely textual and still fires.
    expect(next[0]!.ready_when).toBe('steps.b');
  });

  test('is a no-op when from === to', () => {
    const steps = [step('a', 'steps.a.done')];
    const next = renameSlug(steps, 'a', 'a');
    expect(next[0]!.ready_when).toBe('steps.a.done');
  });
});
