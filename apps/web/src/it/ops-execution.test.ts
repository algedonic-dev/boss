// The web's reading of an ops-request's execute record, held to the
// Rust classifier's own matrix (design b7bb1625, backlog 50a1578f).
// Both read crates/core/boss-jobs/src/ops_execution_matrix.json, so a
// record the CLI reads `failed` cannot render `answered` here.

import { readFileSync } from 'node:fs';
import { describe, expect, test } from 'bun:test';
import { executionLine, executionSucceeded, executionTone, outcomeKind, readExecution } from './ops-execution';

type Case = Readonly<{
  name: string;
  record: Record<string, unknown>;
  label: string;
  outcome_kind: string;
  exit: string | null;
  succeeded: boolean;
  not_yet: boolean;
  reason?: string | null;
  effect?: string;
  effect_text?: string;
  timed_out?: boolean;
}>;

const matrix = JSON.parse(
  readFileSync(new URL('../../../../crates/core/boss-jobs/src/ops_execution_matrix.json', import.meta.url), 'utf8'),
) as { cases: Case[] };

describe('readExecution reads the Rust classifier matrix the same way', () => {
  test('the matrix is the full one', () => {
    expect(matrix.cases.length).toBeGreaterThanOrEqual(19);
  });
  for (const c of matrix.cases) {
    test(c.name, () => {
      const e = readExecution(c.record);
      expect(e.kind).toBe(c.label as typeof e.kind);
      expect<string>(outcomeKind(e)).toBe(c.outcome_kind);
      expect(e.kind === 'refused' ? null : e.exit).toBe(c.exit);
      expect(executionSucceeded(e)).toBe(c.succeeded);
      expect(e.kind === 'failed' && e.notYet).toBe(c.not_yet);
      if (e.kind === 'refused') expect(e.reason).toBe(c.reason ?? null);
      if (e.kind === 'answered') {
        expect(e.effect.kind).toBe(c.effect as typeof e.effect.kind);
        expect(e.effect.kind === 'undeclared' ? undefined : e.effect.text).toBe(c.effect_text);
      }
      if (e.kind === 'failed') expect(e.timedOut).toBe(c.timed_out ?? false);
    });
  }
});

describe('the words a surface prints', () => {
  const line = (record: Record<string, unknown>): string => executionLine(readExecution(record));

  test('the legacy answered exit 1 says FAILED, never answered', () => {
    expect(line({ disposition: 'answered', exit_code: '1' })).toBe('failed · exit 1');
    expect(executionTone(readExecution({ disposition: 'answered', exit_code: '1' }))).toBe('err');
  });

  test('a refusal names the reason it recorded and invents none', () => {
    expect(line({ disposition: 'refused', reason: 'not in the allowlist\nmore' })).toBe('refused — not in the allowlist');
    const bare = line({ disposition: 'refused' });
    expect(bare).toBe('refused — the runner recorded no reason');
    expect(bare).not.toContain('allowlist');
  });

  test('75 is NOT YET, in the warn tone', () => {
    const e = readExecution({ disposition: 'answered', exit_code: '75' });
    expect(executionLine(e)).toBe('not yet · exit 75');
    expect(executionTone(e)).toBe('warn');
  });

  test('an unknown result is never success', () => {
    const e = readExecution({ disposition: 'answered' });
    expect(executionLine(e)).toBe('indeterminate — the verb was invoked and no exit was recorded');
    expect(executionTone(e)).toBe('err');
  });

  test('exit 0 keeps its effect qualification beside it', () => {
    expect(line({ disposition: 'answered', exit_code: '0' })).toBe('answered · exit 0');
    expect(line({ disposition: 'answered', exit_code: '0', effect_unread: 'prints no re-list' })).toBe('answered · exit 0 · effect unread — prints no re-list');
    const unproven = readExecution({ disposition: 'answered', exit_code: '0', effect_unproven: 'no line matched' });
    expect(executionLine(unproven)).toBe('answered · exit 0 · effect unproven — no line matched');
    expect(executionTone(unproven)).toBe('err');
    expect(executionTone(readExecution({ disposition: 'answered', exit_code: '0' }))).toBe('ok');
  });

  test('a timeout is named only on the wrapper kill line', () => {
    expect(line({ disposition: 'answered', exit_code: '124', output: '[ops-runner: command killed at 300s timeout]' })).toBe('failed · exit 124 (killed at its timeout)');
  });
});
