// Author-time dry-run lint. POSTs the draft's steps to the server's
// /_validate endpoint, which runs the same `validate_all` the publish
// path enforces — WITHOUT persisting. "ok: true" here means the spec
// publishes cleanly (the server reuses the same StepType registry, per
// docs/design/jobkind-authoring-ux.md D5). The graph editor paints
// `problems` onto the offending nodes live.

import type { StepSpec } from './jobKindTypes';

export type LintProblem = {
  /// Offending step slug; empty for whole-spec problems (no trigger, …).
  step: string;
  reason: string;
  message: string;
};

export type LintResult = {
  ok: boolean;
  problems: ReadonlyArray<LintProblem>;
};

export async function validateDraft(
  kind: string,
  steps: ReadonlyArray<StepSpec>,
): Promise<LintResult> {
  const r = await fetch('/api/jobs/kinds/_validate', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ kind, steps }),
    credentials: 'same-origin',
  });
  if (!r.ok) {
    throw new Error(`validate: HTTP ${r.status}`);
  }
  return (await r.json()) as LintResult;
}

/// Group problems by step slug for fast per-node lookup in the editor.
/// Whole-spec problems (empty `step`) land under the `''` key.
export function problemsByStep(
  result: LintResult,
): Map<string, LintProblem[]> {
  const out = new Map<string, LintProblem[]>();
  for (const p of result.problems) {
    const list = out.get(p.step) ?? [];
    list.push(p);
    out.set(p.step, list);
  }
  return out;
}
