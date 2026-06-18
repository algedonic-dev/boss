// Build StepDag `nodes` + `edges` from a JobKind's step list — the
// authoring view (no live status). Shared by every surface that shows a
// JobKind's shape (root/landing, JobKind detail), so they all render
// through the one `StepDag` component instead of bespoke per-surface
// diagrams.
//
// v2-native: a step's dependencies are the sibling slugs its `ready_when`
// predicate references (`steps.<slug>.done`, `steps.<slug>.metadata.…`).
// A predicate that names the same upstream twice is one dependency, so we
// dedupe references here; `StepDag` also dedupes edges defensively.

import type { DagNode, DagEdge } from './StepDag.svelte';

type DagStep = {
  title: string;
  kind: string;
  ready_when?: string;
  title_template?: string | null;
  terminal?: { outcome: string } | null;
};

/** Pull the unique `steps.<slug>` references out of a `ready_when`. */
function referencedSlugs(readyWhen: string | undefined): string[] {
  if (!readyWhen) return [];
  const out = new Set<string>();
  const re = /steps\.([a-z][a-z0-9-]*)/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(readyWhen)) !== null) out.add(m[1]!);
  return [...out];
}

/** Title-case a kebab slug as a fallback display label. */
function humanize(slug: string): string {
  return slug.replace(/-/g, ' ').replace(/\b\w/g, (c) => c.toUpperCase());
}

/** Strip `{placeholder}` runs from a display template for a compact node. */
function nodeTitle(step: DagStep): string {
  const t = (step.title_template ?? '').replace(/\{[^}]+\}/g, '…').trim();
  return t || humanize(step.title);
}

export function jobKindToDag(steps: ReadonlyArray<DagStep>): {
  nodes: DagNode[];
  edges: DagEdge[];
} {
  const declared = new Set(steps.map((s) => s.title));
  const nodes: DagNode[] = steps.map((s) => ({
    id: s.title,
    title: nodeTitle(s),
    kind: s.kind,
    terminal: s.terminal?.outcome ?? null,
  }));
  const edges: DagEdge[] = steps.flatMap((s) =>
    referencedSlugs(s.ready_when)
      .filter((src) => declared.has(src) && src !== s.title)
      .map((src) => ({ from: src, to: s.title })),
  );
  return { nodes, edges };
}
