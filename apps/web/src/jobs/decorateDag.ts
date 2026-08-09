// Decorate a Workflow's DAG nodes with the fleet's live depth and
// the stage-duration flow — ONE definition shared by the Bottlenecks
// page and the IT flow network (§9a: the badge grammar drifting
// between two surfaces would have the same node telling two stories).
import type { DagNode } from './StepDag.svelte';

export type FleetNodeStat = Readonly<{
  slug: string;
  ready: number;
  active: number;
}>;
export type StageStat = Readonly<{
  slug: string;
  completed: number;
  p50_seconds: number;
}>;

export function fmtDur(seconds: number): string {
  if (seconds < 90) return `${Math.round(seconds)}s`;
  const m = seconds / 60;
  if (m < 120) return `${Math.round(m)}m`;
  return `${(m / 60).toFixed(1)}h`;
}

/// Depth lights the node (active > ready > neutral); the badge reads
/// "<depth> waiting | <n>× · p50 <dur>" with absent halves omitted.
export function decorateDagNodes(
  nodes: ReadonlyArray<DagNode>,
  fleet: ReadonlyArray<FleetNodeStat>,
  stages: ReadonlyArray<StageStat>,
): DagNode[] {
  const bySlug = new Map(fleet.map((n) => [n.slug, n]));
  const statBySlug = new Map(stages.map((s) => [s.slug, s]));
  return nodes.map((n) => {
    const server = bySlug.get(n.id);
    const depth = server ? server.ready + server.active : 0;
    const stat = statBySlug.get(n.id);
    const parts: string[] = [];
    if (depth > 0) parts.push(`${depth} waiting`);
    if (stat && stat.completed > 0) parts.push(`${stat.completed}× · p50 ${fmtDur(stat.p50_seconds)}`);
    return {
      ...n,
      status: depth > 0 ? ((server?.active ?? 0) > 0 ? 'active' : 'ready') : undefined,
      badge: parts.length > 0 ? parts.join('  |  ') : null,
    };
  });
}
