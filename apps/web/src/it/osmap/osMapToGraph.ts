// Turn the executor network into a graph the renderer can lay out.
//
// Kept out of the component because it is the part with rules worth
// asserting: how a node is placed, how an edge is weighted, and how
// simulated traffic is distinguished. See
// docs/design/operating-system-view.md.

import type { Node, Edge } from '@xyflow/svelte';

export type OsMapNode = {
  id: string;
  label: string;
  kind: 'department' | 'dispatcher' | 'unresolved';
  touched: number;
};

export type OsMapEdge = {
  source: string;
  target: string;
  handoffs: number;
  simulated: number;
};

export type OsMap = {
  nodes: ReadonlyArray<OsMapNode>;
  edges: ReadonlyArray<OsMapEdge>;
  handoffs_considered: number;
  high_water: number;
};

/// Radial layout, busiest first, clockwise from the top.
///
/// Deliberately deterministic rather than force-directed: this view
/// polls, and a physics layout would reshuffle the whole map every
/// few seconds as counts shift. An operator watching flow needs the
/// map to hold still so that MOVEMENT means something happened —
/// which is the entire point of a live instrument.
///
/// The dispatcher is pinned to the centre. It touches most edges, so
/// putting it on the rim drags every line across the canvas; in the
/// middle it reads as what it is — the thing everything routes
/// through.
export function layout(nodes: ReadonlyArray<OsMapNode>, radius = 300): Node[] {
  const rim = nodes.filter((n) => n.kind !== 'dispatcher');
  const centre = nodes.filter((n) => n.kind === 'dispatcher');
  const ordered = [...rim].sort((a, b) => b.touched - a.touched || a.id.localeCompare(b.id));

  const placed: Node[] = ordered.map((n, i) => {
    const angle = (i / Math.max(ordered.length, 1)) * 2 * Math.PI - Math.PI / 2;
    return {
      id: n.id,
      position: { x: Math.cos(angle) * radius, y: Math.sin(angle) * radius },
      data: { label: n.label, kind: n.kind, touched: n.touched },
      class: `os-node os-node-${n.kind}`,
    };
  });

  for (const n of centre) {
    placed.push({
      id: n.id,
      position: { x: 0, y: 0 },
      data: { label: n.label, kind: n.kind, touched: n.touched },
      class: `os-node os-node-${n.kind}`,
    });
  }
  return placed;
}

/// Stroke width from handoff volume.
///
/// Log-scaled because the range is enormous — the busiest edge on the
/// playground carries 1,821 handoffs and the quietest carries 1. A
/// linear scale renders everything except the top two as a hairline,
/// which hides exactly the small flows an operator is looking for.
export function strokeWidth(handoffs: number, max: number): number {
  if (max <= 1) return 1.5;
  const t = Math.log1p(Math.max(handoffs, 0)) / Math.log1p(max);
  return 1 + t * 7;
}

/// Simulated traffic is coloured apart (Q5). An edge is treated as
/// simulated when ALL of its handoffs are — a mixed edge is real work
/// that happens to include some synthetic, and colouring it as
/// simulated would understate what people actually did.
export function isSimulated(e: OsMapEdge): boolean {
  return e.handoffs > 0 && e.simulated >= e.handoffs;
}

export function toEdges(edges: ReadonlyArray<OsMapEdge>): Edge[] {
  const max = edges.reduce((m, e) => Math.max(m, e.handoffs), 0);
  return edges.map((e) => {
    const sim = isSimulated(e);
    return {
      id: `${e.source}->${e.target}`,
      source: e.source,
      target: e.target,
      // A self-edge needs a visible loop rather than a zero-length
      // line; intra-departmental handoffs are the reviewer's explicit
      // ask and the commonest edge in the data.
      type: e.source === e.target ? 'smoothstep' : 'default',
      animated: sim,
      label: e.handoffs > 0 ? String(e.handoffs) : undefined,
      style: `stroke:${sim ? '#a78bfa' : '#0f766e'};stroke-width:${strokeWidth(e.handoffs, max)}px`,
      data: { handoffs: e.handoffs, simulated: e.simulated },
    };
  });
}
