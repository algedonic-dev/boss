// THE GATE'S LOAD, STATED BY THE GATE, AND WHAT IT COST, ON THE RECEIPT
// (backlog ebb750cd).
//
// Two PR trains were disassembled on 2026-09-28 by web-suite reds that
// were not code failures, both while three concurrent gates built cargo
// 20-wide on w-1's one NVMe:
//
//   00:01Z  scripts/bunfig-keys-take-effect.test.ts hit its 30 s budget
//           (30 155 ms; the unit run's slowest files were the leak pin at
//           38 s and a static pin at 15.5 s). The same file on the dev
//           pod, measured 2026-09-28 04:06Z: 5.0 s alone, 14.9 s inside
//           the whole unit run, 4 at a time.
//   03:11Z  it-department-map-stations.mocked.spec.ts:375 — a Crew Board
//           `toBeVisible` against the 15 s expect budget — took 27.6 s;
//           the same test on the same tree ran 658 / 676 / 624 ms three
//           times on the dev pod. Forty times slower, the page unchanged.
//
// A budget sized for a quiet pod is a coin toss inside a loaded gate,
// and a budget sized for the gate would be minutes of waiting on every
// developer's genuinely failing assertion. So the budgets stay stated
// once, for a quiet run, and the GATE-RUNNER says when it is not one:
// infra/gate-runner/run.sh exports GATE_LOAD_ENV=1, and every web
// budget — the mocked config's four, the unit runner's per-test
// timeout, and each hand-written budget that goes through `scaled` —
// is multiplied by GATE_LOAD_SCALE. Nothing else sets it: a developer
// run, `wt-web`, and forge CI's web job are unscaled.
//
// It is NOT a retry and it hides nothing: a surface that never renders
// still fails, with the same message, later. And because a larger budget
// also makes the load harder to see, the gate records what each test
// actually took (TIMINGS_ENV, below) and puts the slowest on its receipt
// as `web_timings` — so the next red, or the next near-miss, carries its
// own load factor instead of needing a reproduction on an idle pod.
//
// NODE-SAFE ON PURPOSE, like dev-workers.ts: playwright.mocked.config.ts
// imports it, and Playwright loads its config under node.

import { appendFileSync } from 'node:fs';

/// Set to `1` by the gate-runner, and by nothing else. Pinned equal to
/// infra/gate-runner/run.sh by boss-testing's
/// `the_gate_states_its_web_load.rs`.
export const GATE_LOAD_ENV = 'BOSS_WEB_GATE_LOAD';

/// How much longer every web budget is inside a loaded gate. 4 x the
/// quiet budgets gives the mocked suite a 60 s expect (the 03:11Z red
/// needed more than 15 s for a paint that takes 0.65 s quiet) and the
/// unit runner 120 s per test (the 00:01Z red needed more than 30 s for
/// a file that takes 5 s alone). The receipt's `web_timings` is where to
/// read whether it is enough.
export const GATE_LOAD_SCALE = 4;

/// The file the web runners append their per-test timings to: set by
/// infra/gate.sh for the web phase, beside its receipt. Unset, nothing
/// is written.
export const TIMINGS_ENV = 'BOSS_WEB_TIMINGS';

type Env = Readonly<Record<string, string | undefined>>;

/// 1 for a developer run; GATE_LOAD_SCALE inside a loaded gate. A value
/// this cannot read throws — a gate that meant to say "loaded" and
/// misspelled it must not run quietly on quiet budgets.
export function loadScale(env: Env = process.env): number {
  const said = env[GATE_LOAD_ENV];
  if (said === undefined || said === '' || said === '0') return 1;
  if (said === '1') return GATE_LOAD_SCALE;
  throw new Error(
    `${GATE_LOAD_ENV}=${JSON.stringify(said)}: expected 1 (a loaded gate) or unset (a quiet run); ` +
      'a value this cannot read is refused, not guessed at',
  );
}

/// A budget stated for a quiet run, as it applies to this one.
export function scaled(ms: number, env: Env = process.env): number {
  return ms * loadScale(env);
}

/// One record of the timings file: ms, suite, result, name — tab-separated,
/// one line, so a tab or a newline in a test's title cannot split it.
export function timingLine(ms: number, suite: string, result: string, name: string): string {
  const flat = (s: string): string => s.replace(/[\t\r\n]+/g, ' ');
  return `${Math.round(ms)}\t${flat(suite)}\t${flat(result)}\t${flat(name)}\n`;
}

/// Append `lines` to the timings file at `path`; nothing when there is
/// no path (a run outside the gate) or nothing to write.
export function appendTimings(path: string | undefined, lines: ReadonlyArray<string>): void {
  if (!path || lines.length === 0) return;
  appendFileSync(path, lines.join(''));
}
