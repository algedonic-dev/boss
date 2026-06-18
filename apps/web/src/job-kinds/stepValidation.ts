// Client-side, non-blocking lint for a JobKind v2 step list.
//
// These checks are advisory: they surface authoring mistakes inline
// in the editor. The authoritative viability / fork-coverage /
// reachability analysis runs SERVER-SIDE at publish time in
// boss-jobs (a step can only be published if every fork outcome is
// covered and every terminal is reachable). Wiring that server-side
// lint into the editor live (so authors see it before publish) is a
// follow-up — it needs a dedicated boss-jobs "dry-run validate"
// endpoint that does not exist yet. Do NOT add a Rust endpoint as
// part of the UI migration; this module is the local stopgap.

import type { StepSpec } from './jobKindTypes';

export type StepWarning = Readonly<{
  /// Index of the offending step, or null for whole-spec warnings.
  stepIndex: number | null;
  message: string;
}>;

const SLUG_RE = /^[a-z][a-z0-9-]*$/;

/// Pull every `steps.<slug>` reference out of a ready_when predicate.
/// Slugs are kebab-case; we also tolerate the `.done` / `.metadata.x`
/// suffixes by stopping at the first non-slug char.
export function referencedSlugs(readyWhen: string): ReadonlyArray<string> {
  const out: string[] = [];
  const re = /steps\.([a-z][a-z0-9-]*)/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(readyWhen)) !== null) {
    out.push(m[1]!);
  }
  return out;
}

/// Compute the inline warnings for a step list. Pure; safe to call
/// on every keystroke.
export function lintSteps(steps: ReadonlyArray<StepSpec>): ReadonlyArray<StepWarning> {
  const warnings: StepWarning[] = [];
  const slugs = steps.map((s) => s.title.trim());
  const declared = new Set(slugs.filter((s) => s.length > 0));

  // --- Slug shape + uniqueness ---
  const seen = new Map<string, number>();
  steps.forEach((s, i) => {
    const slug = s.title.trim();
    if (slug.length === 0) {
      warnings.push({ stepIndex: i, message: 'Step slug is required.' });
      return;
    }
    if (!SLUG_RE.test(slug)) {
      warnings.push({
        stepIndex: i,
        message: `Slug "${slug}" must be kebab-case (lowercase, dashes, no leading digit).`,
      });
    }
    const prior = seen.get(slug);
    if (prior !== undefined) {
      warnings.push({
        stepIndex: i,
        message: `Duplicate slug "${slug}" (also step ${prior + 1}). Slugs must be unique.`,
      });
    } else {
      seen.set(slug, i);
    }
  });

  // --- Every referenced steps.<slug> resolves ---
  steps.forEach((s, i) => {
    for (const ref of referencedSlugs(s.ready_when)) {
      if (!declared.has(ref)) {
        warnings.push({
          stepIndex: i,
          message: `ready_when references steps.${ref}, which is not a declared step slug.`,
        });
      }
      if (ref === s.title.trim()) {
        warnings.push({
          stepIndex: i,
          message: `ready_when references its own slug (steps.${ref}) — a step cannot gate on itself.`,
        });
      }
    }
  });

  // --- At least one trigger (ready_when === "true") ---
  const triggers = steps.filter((s) => s.ready_when.trim() === 'true');
  if (steps.length > 0 && triggers.length === 0) {
    warnings.push({
      stepIndex: null,
      message:
        'No trigger step: at least one step needs ready_when = "true" so the Job has somewhere to start.',
    });
  }

  // --- At least one terminal ---
  const terminals = steps.filter((s) => s.terminal !== null);
  if (steps.length > 0 && terminals.length === 0) {
    warnings.push({
      stepIndex: null,
      message:
        'No terminal step: at least one step must be marked terminal so the Job can close.',
    });
  }
  terminals.forEach((s) => {
    if (s.terminal && s.terminal.outcome.trim().length === 0) {
      const idx = steps.indexOf(s);
      warnings.push({
        stepIndex: idx,
        message: 'Terminal step is missing an outcome.',
      });
    }
  });

  return warnings;
}
