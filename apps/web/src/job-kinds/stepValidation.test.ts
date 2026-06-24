import { describe, expect, test } from 'bun:test';
import { lintSteps } from './stepValidation';
import type { StepSpec } from './jobKindTypes';

const step = (over: Partial<StepSpec>): StepSpec => ({
  title: 'x',
  kind: 'generic',
  ready_when: 'true',
  title_template: '',
  sign_offs_required: [],
  authority_role: null,
  metadata_defaults: {},
  ...over,
});

const hasNoTerminalWarning = (steps: StepSpec[]): boolean =>
  lintSteps(steps).some((w) => w.message.includes('No terminal step'));

describe('lintSteps — terminal presence', () => {
  // The API omits `terminal` for non-terminal steps (serde
  // skip_serializing_if), so a loaded step reads back with terminal
  // === undefined, not null. A `!== null` filter wrongly counted those
  // as terminals; `!= null` must treat absent and null identically.
  test('an absent (undefined) terminal does NOT count as a terminal', () => {
    expect(hasNoTerminalWarning([step({ title: 'start' })])).toBe(true);
  });

  test('an explicit null terminal does NOT count as a terminal', () => {
    expect(hasNoTerminalWarning([step({ title: 'start', terminal: null })])).toBe(true);
  });

  test('a real terminal outcome satisfies the terminal check', () => {
    expect(
      hasNoTerminalWarning([step({ title: 'done', terminal: { outcome: 'completed' } })]),
    ).toBe(false);
  });
});
