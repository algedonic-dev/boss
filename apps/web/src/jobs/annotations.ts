// A packet's job-level metadata, as the job page reads it (backlog
// 1abb9928, 2026-09-27).
//
// Operators and agents annotate a packet through PATCH
// /api/jobs/{id}/metadata — a plan of action, an ETA, what needs
// David, the decisions taken — and the normal-operations rule requires
// that plan on every troubled packet. Until this module the job page
// read job.metadata only for the keys a workflow declares as link
// fields, so every one of those annotations reached the one human
// reading them only through the API, while the step page listed every
// step key. A record he cannot read is not a record for him.
//
// Pure functions, so the order and the shapes are pinned by
// annotations.test.ts and the page only draws what they answer.

/** The keys an alarm plan is written in, in the order the reader
 *  needs them: who holds it, what they will do, by when, and what is
 *  waiting on David. They lead the section on every packet. */
export const ALARM_KEYS = [
  'owner',
  'plan_of_action',
  'eta_utc',
  'needs_david',
  'steps_for_david',
] as const;

/** Decisions are recorded as `decided_<date>_<source>`; they follow
 *  the alarm keys, oldest first by the date the key carries. */
const DECIDED_PREFIX = 'decided_';

/** Keys written as prose even when a value happens to be short — a
 *  one-line plan is still a plan, and reads as a paragraph. */
const PROSE_KEYS: ReadonlySet<string> = new Set([
  'plan_of_action',
  'steps_for_david',
  'needs_david',
  'description',
]);

/** Any other string this long, or with a newline in it, is prose too. */
const PROSE_MIN_CHARS = 120;

/** Job metadata the page already renders somewhere else. `corrections`
 *  is the reserved list the job GET hands to each step it targets,
 *  drawn by StepCorrections beside that step (design 4105b020); the
 *  declared link fields are passed in by the caller, which renders
 *  them as Linked Jobs. Nothing else is skipped. */
const RENDERED_ELSEWHERE: ReadonlySet<string> = new Set(['corrections']);

export type AnnotationValue =
  | { kind: 'prose'; text: string }
  | { kind: 'text'; text: string }
  | { kind: 'list'; items: string[] }
  | { kind: 'structured'; json: string };

export type Annotation = { key: string; value: AnnotationValue };

const isScalar = (v: unknown): v is string | number | boolean | null =>
  v === null || typeof v === 'string' || typeof v === 'number' || typeof v === 'boolean';

/** How one annotation's value is drawn. */
export function classifyAnnotation(key: string, value: unknown): AnnotationValue {
  if (typeof value === 'string') {
    const prose =
      PROSE_KEYS.has(key) ||
      key.startsWith(DECIDED_PREFIX) ||
      value.includes('\n') ||
      value.length > PROSE_MIN_CHARS;
    return prose ? { kind: 'prose', text: value } : { kind: 'text', text: value };
  }
  if (isScalar(value)) return { kind: 'text', text: String(value) };
  if (Array.isArray(value) && value.every(isScalar)) {
    return { kind: 'list', items: value.map((v) => String(v)) };
  }
  return { kind: 'structured', json: JSON.stringify(value, null, 2) };
}

/** 0 for an alarm key (ranked among themselves by ALARM_KEYS), 1 for a
 *  decision, 2 for everything else. */
function band(key: string): number {
  if ((ALARM_KEYS as ReadonlyArray<string>).includes(key)) return 0;
  if (key.startsWith(DECIDED_PREFIX)) return 1;
  return 2;
}

function compareKeys(a: string, b: string): number {
  const ba = band(a);
  const bb = band(b);
  if (ba !== bb) return ba - bb;
  if (ba === 0) {
    const order = ALARM_KEYS as ReadonlyArray<string>;
    return order.indexOf(a) - order.indexOf(b);
  }
  return a < b ? -1 : a > b ? 1 : 0;
}

/** Every job-level annotation the page does not already render, in
 *  reading order. `linkFields` are the keys this job's workflow
 *  declares as job-to-job links (the Linked Jobs section). */
export function jobAnnotations(
  metadata: Readonly<Record<string, unknown>> | undefined,
  linkFields: ReadonlySet<string>,
): Annotation[] {
  return Object.keys(metadata ?? {})
    .filter((k) => !RENDERED_ELSEWHERE.has(k) && !linkFields.has(k))
    .sort(compareKeys)
    .map((key) => ({ key, value: classifyAnnotation(key, metadata?.[key]) }));
}

export type Segment = { kind: 'text'; text: string } | { kind: 'id'; id: string };

const UUID = /[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/gi;

/** A string split so each uuid in it can be drawn as a link to the
 *  packet it names; joined back, the segments are the string. */
export function idSegments(text: string): Segment[] {
  const out: Segment[] = [];
  let at = 0;
  for (const m of text.matchAll(UUID)) {
    const i = m.index ?? 0;
    if (i > at) out.push({ kind: 'text', text: text.slice(at, i) });
    out.push({ kind: 'id', id: m[0] });
    at = i + m[0].length;
  }
  if (at < text.length) out.push({ kind: 'text', text: text.slice(at) });
  return out;
}
