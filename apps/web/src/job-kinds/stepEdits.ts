// Pure, side-effect-free transforms on a JobKind v2 step list. The
// graphical authoring surface (palette, inspector) routes every edit
// through these so the rules live in one tested place rather than
// scattered across components. Each returns a NEW array — no mutation
// (CLAUDE.md §Immutable Data).
//
// `renameSlug` is the load-bearing one: a step's slug IS its identity
// (D1 — no per-step id), and predicates reference it as
// `steps.<slug>.done` / `steps.<slug>.metadata.<field>`. Renaming must
// rewrite every reference or the DAG silently breaks, which is exactly
// the sharp edge the free-text list editor has today.

import type { StepSpec } from './jobKindTypes';

const SLUG_PART_RE = /[^a-z0-9]+/g;

/// A unique kebab-case slug derived from a StepType `kind`, deduped
/// against the slugs already in `existing` with `-2`, `-3`, … . Falls
/// back to `step` when the kind has no usable characters.
export function freshSlug(
  kind: string,
  existing: ReadonlyArray<StepSpec>,
): string {
  const base =
    kind.toLowerCase().replace(SLUG_PART_RE, '-').replace(/^-+|-+$/g, '') ||
    'step';
  const taken = new Set(existing.map((s) => s.title));
  if (!taken.has(base)) return base;
  for (let n = 2; ; n++) {
    const candidate = `${base}-${n}`;
    if (!taken.has(candidate)) return candidate;
  }
}

/// Build a fresh step of the given StepType `kind`. The very first
/// step of a draft seeds `ready_when = "true"` so a brand-new kind has
/// a trigger immediately; later steps start blank for the author to
/// wire. Mirrors StepDagEditor's `addStep` defaults.
export function makeStep(
  kind: string,
  existing: ReadonlyArray<StepSpec>,
): StepSpec {
  return {
    title: freshSlug(kind, existing),
    kind,
    ready_when: existing.length === 0 ? 'true' : '',
    terminal: null,
    title_template: '',
    sign_offs_required: [],
    authority_role: null,
    metadata_defaults: {},
  };
}

/// Overlay `patch` onto the step whose slug is `slug`. Other steps are
/// returned untouched. Top-level fields are replaced wholesale (PATCH
/// semantics are the caller's concern).
export function patchStep(
  steps: ReadonlyArray<StepSpec>,
  slug: string,
  patch: Partial<StepSpec>,
): StepSpec[] {
  return steps.map((s) => (s.title === slug ? { ...s, ...patch } : s));
}

/// Drop the step whose slug is `slug`. References to it in other
/// steps' `ready_when` are left as-is — the live-lint surfaces them as
/// dangling, which is the honest signal (silently rewriting a delete
/// would hide a real authoring mistake).
export function removeStep(
  steps: ReadonlyArray<StepSpec>,
  slug: string,
): StepSpec[] {
  return steps.filter((s) => s.title !== slug);
}

function escapeRe(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

/// Rename a step's slug AND rewrite every `ready_when` reference to it
/// across the whole list, keeping the DAG intact (D1 rename-refactor).
/// Only whole-slug references match: `steps.demand` will NOT corrupt
/// `steps.demand-check` (the negative lookahead rejects a slug-char
/// continuation). A no-op when `from === to`.
export function renameSlug(
  steps: ReadonlyArray<StepSpec>,
  from: string,
  to: string,
): StepSpec[] {
  if (from === to) return [...steps];
  // `steps.<from>` not followed by another slug char ([a-z0-9-]), so a
  // shorter slug never matches inside a longer one.
  const re = new RegExp(`steps\\.${escapeRe(from)}(?![a-z0-9-])`, 'g');
  return steps.map((s) => ({
    ...s,
    title: s.title === from ? to : s.title,
    ready_when: (s.ready_when ?? '').replace(re, `steps.${to}`),
  }));
}
