// The mocked suite's timings reporter writes one line per test to the
// file the gate names, and nothing without one (backlog ebb750cd). That
// Playwright actually LOADS it from the config is proven by effect in
// mocked-runner-refuses-a-leak.test.ts, which already pays for a run.
import { expect, test } from 'bun:test';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import type { TestCase, TestResult } from '@playwright/test/reporter';

import { timingLine } from '../src/dev-load';
import TimingsReporter from './timings-reporter';

const WEB = join(import.meta.dir, '..');

function aTest(file: string, line: number, title: string): TestCase {
  return { title, location: { file: join(WEB, file), line, column: 3 } } as unknown as TestCase;
}

function aResult(duration: number, status: TestResult['status']): TestResult {
  return { duration, status } as unknown as TestResult;
}

test('each finished test is one line — where it is, what it is called, what it took, how it ended', () => {
  const dir = mkdtempSync(join(tmpdir(), 'timings-reporter-'));
  const outputFile = join(dir, 'web-timings.tsv');
  try {
    const reporter = new TimingsReporter({ outputFile });
    reporter.onTestEnd(
      aTest('tests/mocked/it-department-map-stations.mocked.spec.ts', 375, 'draws its Crew Board'),
      aResult(27_612, 'failed'),
    );
    reporter.onTestEnd(aTest('tests/mocked/jobs-page.mocked.spec.ts', 12, 'lists'), aResult(640, 'passed'));
    expect(readFileSync(outputFile, 'utf8')).toBe(
      timingLine(27_612, 'mocked', 'failed', 'tests/mocked/it-department-map-stations.mocked.spec.ts:375 › draws its Crew Board') +
        timingLine(640, 'mocked', 'passed', 'tests/mocked/jobs-page.mocked.spec.ts:12 › lists'),
    );
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('with no file named it is inert: a run outside the gate records nothing', () => {
  const reporter = new TimingsReporter({});
  expect(() => reporter.onTestEnd(aTest('tests/mocked/a.mocked.spec.ts', 1, 'a'), aResult(1, 'passed'))).not.toThrow();
});
