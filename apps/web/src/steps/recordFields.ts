// Which declared fields the generic surface SHOWS rather than asks for
// (backlog 7ac7cd73, 2026-10-19).
//
// GenericSurface drew every declared field as a text box seeded from the
// metadata only when the value was a string. A declared object field —
// ship-a-change's `release` record {sha, review, verdict, by, at,
// main_sha} — therefore rendered as an EMPTY box over the record that was
// actually there (and its key was excluded from the leftover dump too, so
// the record showed nowhere), and a field a door writes (`hold_sha`)
// became a box a hand could overwrite. The car that met this (5101e080)
// left both fields undeclared rather than make the page lie, which made
// the registry silent about what its doors write (CLAUDE.md 9a).
//
// The rule is read off data the row already carries — no new schema:
//   - `field_type` `object` or `array`: a text box cannot produce one;
//   - a value already structured (whatever the type says): drawing it as
//     '' hides it, and any write from that box would replace it;
//   - a declared `writer` (boss_core::job::StepField::writer): the row
//     names the one credential principal the step doors admit for the
//     key, so a hand edit is a write the server refuses anyway.

import type { StepField } from '../jobs/types';

const STRUCTURED_TYPES: ReadonlySet<string> = new Set(['object', 'array']);

/// True when the surface shows this field's value and offers no input.
export function readOnlyField(f: StepField, value: unknown): boolean {
  return (
    STRUCTURED_TYPES.has(f.field_type) ||
    (typeof f.writer === 'string' && f.writer.length > 0) ||
    (value !== null && typeof value === 'object')
  );
}

/// The value as it is drawn: a structured value as its whole record,
/// indented; a scalar as its text; nothing written yet as null.
export function recordText(value: unknown): string | null {
  if (value === null || value === undefined) return null;
  if (typeof value === 'object') return JSON.stringify(value, null, 2);
  return String(value);
}

/// The fields the executor answers, seeded from metadata — the only keys
/// the surface may send back. A read-only field is never among them.
export function editableValues(
  fields: ReadonlyArray<StepField>,
  metadata: Readonly<Record<string, unknown>>,
): Record<string, string> {
  return Object.fromEntries(
    fields
      .filter((f) => !readOnlyField(f, metadata[f.name]))
      .map((f) => {
        const v = metadata[f.name];
        return [f.name, typeof v === 'string' ? v : ''];
      }),
  );
}

/// The answers a gesture may send: the boxes the operator EDITED on this
/// page — a value that differs from `baseline`, what the page last knew
/// the record held — and never every box it shows (backlog ec1e1980).
/// Sending every non-empty box re-sent what the page was drawn with on
/// each Save, Start and Complete, so a page left open reverted any key
/// another writer had changed since, and the audit log credited the
/// operator with a write they never made. The server's own guard
/// (field_writer::strip_unchanged_reserved) covers only a field that
/// declares a writer. An empty box is not an answer, and a read-only
/// field is never written from the page (7ac7cd73).
export function editedAnswers(
  values: Readonly<Record<string, string>>,
  baseline: Readonly<Record<string, string>>,
  readOnly: ReadonlySet<string>,
): Record<string, string> {
  return Object.fromEntries(
    Object.entries(values).filter(
      ([k, v]) => v.trim() !== '' && !readOnly.has(k) && v !== (baseline[k] ?? ''),
    ),
  );
}

/// The boxes after the page re-reads the step: each box the operator has
/// not touched (it still equals its baseline) takes what the record now
/// holds, and that becomes its baseline, so the page shows the record
/// rather than the value it was drawn with. A box being edited is left
/// exactly as the operator has it. `null` when nothing moved.
export function followRecord(
  values: Readonly<Record<string, string>>,
  baseline: Readonly<Record<string, string>>,
  live: Readonly<Record<string, string>>,
): { values: Record<string, string>; baseline: Record<string, string> } | null {
  const moved = Object.entries(live).filter(
    ([k, v]) => (values[k] ?? '') === (baseline[k] ?? '') && (values[k] !== v || baseline[k] !== v),
  );
  if (moved.length === 0) return null;
  const taken = Object.fromEntries(moved);
  return { values: { ...values, ...taken }, baseline: { ...baseline, ...taken } };
}

/// Required read-only fields with nothing written yet. The page cannot
/// supply them, so they hold Complete exactly as the server would.
export function missingRecords(
  fields: ReadonlyArray<StepField>,
  metadata: Readonly<Record<string, unknown>>,
): ReadonlyArray<string> {
  return fields
    .filter((f) => f.required && readOnlyField(f, metadata[f.name]))
    .filter((f) => {
      const text = recordText(metadata[f.name]);
      return text === null || text.trim() === '';
    })
    .map((f) => f.name);
}
