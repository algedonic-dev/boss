// What /ux/views says about its sources, from `GET /api/views/sources`
// (backlog 4a8939b5), and about whose rows a result is (backlog
// 5392cf23). Pure functions; ViewsPage.svelte renders them and
// sources.test.ts pins them without a browser.
//
// The page used to hold three copies of what the server knows — each
// source's fields (types.ts SOURCE_FIELDS), what pushes into SQL
// (PUSHABLE_BY_SOURCE) and the scan ceiling (SCAN_CEILING_LABEL) — kept
// "in step with query.rs by hand". The events fields had lost
// subject_kind and subject_id, and the hint had lost payload.<path>,
// the very fix for a filter that reported 0 against a true 351. The
// copies are gone; everything here reads the served document.

import type { PushableField, SourceSchema, ViewResults, ViewSource, ViewSources } from './types';

const SOURCE_NAMES: ReadonlyArray<ViewSource> = ['subjects', 'jobs', 'steps', 'events'];
const PUSH_TYPES: ReadonlyArray<PushableField['type']> = ['text', 'timestamp', 'json'];

const isStrings = (v: unknown): v is ReadonlyArray<string> =>
  Array.isArray(v) && v.every((x) => typeof x === 'string');

function parseSchema(raw: unknown): SourceSchema {
  const o = (raw ?? {}) as Record<string, unknown>;
  const source = o['source'];
  if (typeof source !== 'string' || !SOURCE_NAMES.includes(source as ViewSource)) {
    throw new Error(`views sources: unknown source ${JSON.stringify(source)}`);
  }
  const pushable = o['pushable'];
  if (!isStrings(o['fields']) || !Array.isArray(pushable)) {
    throw new Error(`views sources: ${source} carries no fields or pushable list`);
  }
  const push = pushable.map((p: unknown) => {
    const q = (p ?? {}) as Record<string, unknown>;
    if (typeof q['field'] !== 'string' || !PUSH_TYPES.includes(q['type'] as PushableField['type'])) {
      throw new Error(`views sources: ${source} has a malformed pushable field`);
    }
    return { field: q['field'], type: q['type'] as PushableField['type'] };
  });
  return { source: source as ViewSource, fields: o['fields'], pushable: push };
}

/// The served document, or a throw naming what is wrong with it — so
/// fetchRemote reports a malformed answer as a failed read, never as a
/// source with no fields.
export function parseSources(raw: unknown): ViewSources {
  if (raw === null || typeof raw !== 'object' || Array.isArray(raw)) {
    throw new Error('views sources: expected an object');
  }
  const o = raw as Record<string, unknown>;
  if (typeof o['scan_ceiling'] !== 'number') {
    throw new Error('views sources: scan_ceiling is not a number');
  }
  if (!Array.isArray(o['sources'])) throw new Error('views sources: sources is not a list');
  return { scan_ceiling: o['scan_ceiling'], sources: o['sources'].map(parseSchema) };
}

const schemaOf = (s: ViewSources, source: ViewSource): SourceSchema | undefined =>
  s.sources.find((x) => x.source === source);

/// The fields the column picker offers for a source.
export function fieldsOf(s: ViewSources, source: ViewSource): ReadonlyArray<string> {
  return schemaOf(s, source)?.fields ?? [];
}

/// How many rows one View may scan, as the page prints it.
export function ceilingLabel(s: ViewSources): string {
  return s.scan_ceiling.toLocaleString('en-US');
}

/// What to filter on so the database narrows the scan: each text field
/// by name, each json field as a path into it, then the ranges.
export function pushableHint(s: ViewSources, source: ViewSource): string {
  const push = schemaOf(s, source)?.pushable ?? [];
  const named = push
    .filter((p) => p.type !== 'timestamp')
    .map((p) => (p.type === 'json' ? `${p.field}.<path>` : p.field));
  const ranges = push.filter((p) => p.type === 'timestamp').map((p) => `a ${p.field} range`);
  const all = [...named, ...ranges];
  if (all.length === 0) return 'a different field';
  if (all.length === 1) return all[0]!;
  return `${all.slice(0, -1).join(', ')}, or ${all[all.length - 1]}`;
}

/// The line under an owner-scoped count. A View runs as whoever runs
/// it, so the same shared View can count differently for two people;
/// when this one counted only the caller's rows, it says so.
export function scopeLine(res: Pick<ViewResults, 'scope'>): string | null {
  return res.scope === 'owners' ? 'only rows you may read' : null;
}
