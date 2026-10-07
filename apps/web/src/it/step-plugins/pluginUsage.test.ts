import {describe, expect, test} from 'bun:test';
import {readBindings, readCount} from './pluginUsage';

describe('native plugin usage decoding', () => {
  test('empty active list and a genuine zero remain known', () => {
    expect(readBindings([])).toEqual([]);
    expect(readCount({kind: 'custom-plugin', in_flight: 0}, 'custom-plugin')).toBe(0);
  });
  test('arbitrary plugin kinds preserve actual workflow version and step slug', () => {
    expect(readBindings([{kind: 'custom-work', version: 19, status: 'active', steps: [{kind: 'custom-plugin', title: 'inspect'}]}]))
      .toEqual([{workflow: 'custom-work', version: 19, kind: 'custom-plugin', step: 'inspect'}]);
  });
  test('count replies refuse missing, wrong identity and non-integral values', () => {
    for (const value of [null, [], {}, {kind: 'other', in_flight: 3},
      ...[-1, 0.5, '3', null, {}, Number.MAX_SAFE_INTEGER + 1].map((in_flight) => ({kind: 'custom-plugin', in_flight}))]) {
      expect(() => readCount(value, 'custom-plugin')).toThrow('Malformed native in-flight count');
    }
  });
  test('an incomplete or ambiguous binding population cannot claim none', () => {
    const row = {kind: 'custom-work', version: 1, status: 'active', steps: [{kind: 'custom-plugin', title: 'inspect'}]};
    for (const value of [null, {}, {items: []}, [null], [{...row, version: '1'}], [{...row, status: 'retired'}],
      [{...row, steps: [{}]}], [row, row], [{...row, steps: [...row.steps, ...row.steps]}]]) {
      expect(() => readBindings(value)).toThrow();
    }
  });
});
