// How an ops-request's verb ENDED, read from its execute record — the
// web's spelling of boss-jobs' `ops_execution::classify` (design
// b7bb1625, backlog 50a1578f). The CLI, the region glyphs and the
// dispatcher's judges read every record through that one classifier;
// until this module the yard, the signals feed and the approve block
// each read `disposition` and `exit_code` by hand, and two of them
// printed a refusal reason no record held ("outside the allowlist")
// and drew an answered verb with no exit as healthy. A fact that lives
// twice gets an equality test (CLAUDE.md §9a): ops-execution.test.ts
// holds this reader to the Rust classifier's own matrix,
// crates/core/boss-jobs/src/ops_execution_matrix.json.
//
// PURE, over the execute step's metadata as the runner wrote it. Four
// readings: refused (nothing ran, its recorded reason or none),
// answered (exit 0, its effect qualification beside it), failed (the
// verb ran and exited nonzero; 75 is the estate's NOT YET), and
// indeterminate (the record does not say how the verb ended — never
// success, never "nothing ran").

import { NOT_YET_EXIT } from './yard/yard';

/** The runner's own line when `timeout` killed the verb (Rust
 *  `TIMEOUT_MARKER`); an exit of 124 without it proves nothing. */
export const TIMEOUT_MARKER = '[ops-runner: command killed at ';

export type Effect =
  | Readonly<{ kind: 'shown'; text: string }>
  | Readonly<{ kind: 'unproven'; text: string }>
  | Readonly<{ kind: 'unread'; text: string }>
  | Readonly<{ kind: 'undeclared' }>;

export type Execution =
  | Readonly<{ kind: 'refused'; reason: string | null }>
  | Readonly<{ kind: 'answered'; exit: string; effect: Effect }>
  | Readonly<{ kind: 'failed'; exit: string; timedOut: boolean; notYet: boolean }>
  | Readonly<{ kind: 'indeterminate'; exit: string | null; cause: string }>;

/** The recorded exit, normalised as Rust's `recorded_exit` does: a
 *  string or a number; null for absent, null or empty; anything else as
 *  its JSON text so an indeterminate reading keeps the raw value. */
function recordedExit(md: Readonly<Record<string, unknown>>): string | null {
  const v = md.exit_code;
  if (v === undefined || v === null || v === '') return null;
  return typeof v === 'string' ? v : JSON.stringify(v);
}

const text = (md: Readonly<Record<string, unknown>>, key: string): string | null => {
  const v = md[key];
  return typeof v === 'string' && v.trim() !== '' ? v : null;
};

/** PURE: classify a COMPLETED execute step's metadata. */
export function readExecution(md: Readonly<Record<string, unknown>>): Execution {
  const raw = recordedExit(md);
  const disposition = md.disposition;
  if (disposition === 'refused') {
    if (raw !== null) {
      return { kind: 'indeterminate', exit: raw, cause: 'a refusal that records an exit — the record contradicts itself' };
    }
    return { kind: 'refused', reason: text(md, 'reason') ?? text(md, 'output') };
  }
  if (disposition === 'answered') {
    if (raw === null) return { kind: 'indeterminate', exit: null, cause: 'the verb was invoked and no exit was recorded' };
    if (!/^[0-9]+$/.test(raw)) return { kind: 'indeterminate', exit: raw, cause: 'the recorded exit is not an exit status' };
    if (/^0+$/.test(raw)) {
      const unproven = text(md, 'effect_unproven');
      const shown = text(md, 'effect');
      const unread = text(md, 'effect_unread');
      const effect: Effect =
        unproven !== null ? { kind: 'unproven', text: unproven }
        : shown !== null ? { kind: 'shown', text: shown }
        : unread !== null ? { kind: 'unread', text: unread }
        : { kind: 'undeclared' };
      return { kind: 'answered', exit: '0', effect };
    }
    const output = typeof md.output === 'string' ? md.output : '';
    return { kind: 'failed', exit: raw, timedOut: raw === '124' && output.includes(TIMEOUT_MARKER), notYet: raw === String(NOT_YET_EXIT) };
  }
  return { kind: 'indeterminate', exit: raw, cause: 'the execute record names no disposition this reader knows' };
}

/** The decided `outcome_kind` for a reading (Rust `outcome_kind`). */
export function outcomeKind(e: Execution): 'aborted' | 'completed' | 'not_yet' | 'failed' {
  switch (e.kind) {
    case 'refused':
      return 'aborted';
    case 'answered':
      return 'completed';
    case 'failed':
      return e.notYet ? 'not_yet' : 'failed';
    case 'indeterminate':
      return 'failed';
  }
}

/** Only a clean exit that did not fail its own effect check is a
 *  success (Rust `succeeded`). */
export const executionSucceeded = (e: Execution): boolean => e.kind === 'answered' && e.effect.kind !== 'unproven';

/** The tone a surface draws: NOT YET warns, every other non-success is
 *  trouble. */
export function executionTone(e: Execution): 'ok' | 'warn' | 'err' {
  if (executionSucceeded(e)) return 'ok';
  return e.kind === 'failed' && e.notYet ? 'warn' : 'err';
}

const firstLine = (s: string): string => s.split('\n')[0]?.trim() ?? '';

/** The one word a surface prints for a reading — the CLI's words:
 *  failed and not yet are never "answered". */
export function executionWord(e: Execution): 'refused' | 'answered' | 'failed' | 'not yet' | 'indeterminate' {
  return e.kind === 'failed' && e.notYet ? 'not yet' : e.kind;
}

/** What follows the word, led by its own separator: the exit and the
 *  effect qualification, or the reason a refusal recorded — and none it
 *  did not record. */
export function executionDetail(e: Execution): string {
  switch (e.kind) {
    case 'refused': {
      const why = e.reason === null ? '' : firstLine(e.reason);
      return ` — ${why !== '' ? why : 'the runner recorded no reason'}`;
    }
    case 'answered': {
      const q = e.effect.kind === 'undeclared' || e.effect.kind === 'shown' ? '' : ` · effect ${e.effect.kind} — ${firstLine(e.effect.text)}`;
      return ` · exit ${e.exit}${q}`;
    }
    case 'failed':
      return ` · exit ${e.exit}${e.timedOut ? ' (killed at its timeout)' : ''}`;
    case 'indeterminate':
      return ` — ${e.cause}${e.exit !== null ? ` (exit recorded as ${e.exit})` : ''}`;
  }
}

/** The one line every web surface prints for a reading. */
export const executionLine = (e: Execution): string => `${executionWord(e)}${executionDetail(e)}`;
