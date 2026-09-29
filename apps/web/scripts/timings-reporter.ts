// WHAT EACH MOCKED SPEC TOOK, FOR THE GATE'S RECEIPT (backlog ebb750cd).
//
// The 03:11Z train of 2026-09-28 went red on a Crew Board paint that
// took 27.6 s in the gate and 0.65 s on the dev pod, and the only way to
// learn the 40x was to reproduce it by hand: the list reporter's
// per-test times scroll past in a log that dies with the gate pod, and
// the receipt kept only the failure. playwright.mocked.config.ts loads
// this beside the list reporter when the gate names a file
// (BOSS_WEB_TIMINGS, set by infra/gate.sh for its web phase), and gate.sh
// puts the slowest on the receipt as `web_timings`.
//
// One line per finished test, appended as it finishes rather than
// gathered for the end: a run the gate kills part-way still leaves what
// it measured, and the reporter holds no state of its own.
import { relative } from 'node:path';

import type { Reporter, TestCase, TestResult } from '@playwright/test/reporter';

import { appendTimings, timingLine } from '../src/dev-load';

export default class TimingsReporter implements Reporter {
  private readonly outputFile: string | undefined;

  constructor(options: Readonly<{ outputFile?: string }> = {}) {
    this.outputFile = options.outputFile;
  }

  onTestEnd(test: TestCase, result: TestResult): void {
    const where = `${relative(process.cwd(), test.location.file)}:${test.location.line}`;
    appendTimings(this.outputFile, [timingLine(result.duration, 'mocked', result.status, `${where} › ${test.title}`)]);
  }

  // Nothing is printed: the list reporter owns the terminal.
  printsToStdio(): boolean {
    return false;
  }
}
