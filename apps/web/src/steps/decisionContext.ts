// The decision-context resolution chain — the data half of "a step
// must present the packet's case to whoever acts on it" (19db52de).
//
// David, 2026-08-18, on his publish sign-off: "There is just a sign
// and complete button, which doesn't seem like much of a choice." And
// generalising: "the current state of the packet isn't present in a
// useful manner to me as the decision-maker." The evidence always
// existed — in another step's metadata, or the job body — but the
// surface holding the button never showed it.
//
// The chain mirrors the review plugin's both-bags fallback, which this
// codebase already learned the hard way (a reviewer answered four
// questions blind because the prose sat in the OTHER metadata bag):
//
//   1. step.metadata.context_md   — the author addressed THIS step;
//   2. job.metadata.context_md    — the packet-level briefing;
//   3. job.metadata.message       — the filed text itself (every
//      user-feedback packet's case lives here, so all of them become
//      self-presenting without a single data write).
//
// Pure so it is testable without a DOM; the component owns the fetch.

export type DecisionContextSource = 'step' | 'job-context' | 'job-message';

export type DecisionContext = {
  text: string;
  source: DecisionContextSource;
};

function nonEmptyString(v: unknown): string | null {
  return typeof v === 'string' && v.trim().length > 0 ? v : null;
}

export function contextFromStep(
  stepMetadata: Record<string, unknown>,
): DecisionContext | null {
  const text = nonEmptyString(stepMetadata['context_md']);
  return text ? { text, source: 'step' } : null;
}

export function contextFromJob(
  jobMetadata: Record<string, unknown>,
): DecisionContext | null {
  const ctx = nonEmptyString(jobMetadata['context_md']);
  if (ctx) return { text: ctx, source: 'job-context' };
  const msg = nonEmptyString(jobMetadata['message']);
  if (msg) return { text: msg, source: 'job-message' };
  return null;
}
