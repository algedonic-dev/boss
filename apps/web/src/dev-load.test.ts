// The web suites' budgets scale with the gate's load, only under the
// gate, and the gate can read back how long each test took (backlog
// ebb750cd).

import { describe, expect, test } from 'bun:test';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import { appendTimings, GATE_LOAD_ENV, GATE_LOAD_SCALE, loadScale, scaled, timingLine } from './dev-load';

describe('loadScale', () => {
  test('a developer run is unscaled: the variable unset or empty', () => {
    expect(loadScale({})).toBe(1);
    expect(loadScale({ [GATE_LOAD_ENV]: '' })).toBe(1);
    expect(loadScale({ [GATE_LOAD_ENV]: '0' })).toBe(1);
  });

  test('the gate-runner says it is a loaded gate, and every budget scales by the stated factor', () => {
    expect(GATE_LOAD_SCALE).toBeGreaterThan(1);
    expect(loadScale({ [GATE_LOAD_ENV]: '1' })).toBe(GATE_LOAD_SCALE);
    expect(scaled(15_000, { [GATE_LOAD_ENV]: '1' })).toBe(15_000 * GATE_LOAD_SCALE);
    expect(scaled(15_000, {})).toBe(15_000);
  });

  test('a value it cannot read is refused, naming the variable, not guessed at', () => {
    expect(() => loadScale({ [GATE_LOAD_ENV]: 'yes' })).toThrow(GATE_LOAD_ENV);
    expect(() => loadScale({ [GATE_LOAD_ENV]: '4' })).toThrow(GATE_LOAD_ENV);
  });
});

describe('the timings file', () => {
  test('one tab-separated line per test: ms, suite, result, name — a tab or newline in a name cannot split it', () => {
    expect(timingLine(27_612.4, 'mocked', 'failed', 'a.spec.ts:375 › draws\tits\nCrew Board')).toBe(
      '27612\tmocked\tfailed\ta.spec.ts:375 › draws its Crew Board\n',
    );
  });

  test('appends to the file it is given, and writes nothing when it is given none', () => {
    const dir = mkdtempSync(join(tmpdir(), 'dev-load-'));
    const path = join(dir, 'web-timings.tsv');
    try {
      appendTimings(path, [timingLine(1, 'unit:web', 'passed', 'a')]);
      appendTimings(path, [timingLine(2, 'mocked', 'passed', 'b')]);
      appendTimings(undefined, [timingLine(3, 'mocked', 'passed', 'c')]);
      expect(readFileSync(path, 'utf8')).toBe('1\tunit:web\tpassed\ta\n2\tmocked\tpassed\tb\n');
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
});
