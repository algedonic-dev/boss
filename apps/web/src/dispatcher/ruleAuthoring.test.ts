import { describe, expect, test } from 'bun:test';

import { buildRuleSpec, type RuleForm } from './ruleAuthoring';

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
