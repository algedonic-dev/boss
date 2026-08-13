// The packet-card vocabulary — the data shape and protocol coloring
// behind PacketCard.svelte, the one visual for a job packet anywhere
// a queue renders (feedback d69033dd). Moved here from
// apps/web/src/it/yard/yard.ts when the card was promoted out of the
// yard; yard.ts re-exports these so the definitions live exactly once.

// What a card shows. `branch` is the mono provenance line — a git
// branch in the train yard, the actionable step title in My Day; each
// lens maps its rows into this shape, never the other way around.
export type PacketCardData = Readonly<{
  id: string;
  kind: string;
  branch: string;
  title: string;
  tags: readonly string[];
  sim: boolean;
  skipReason?: string | null;
}>;

// Categorical hues for protocol chips, tuned to sit quietly on the
// VOID/INK grounds. SIGNAL teal is deliberately absent — it stays the
// one live accent — and ok/warn/err stay reserved for state.
export const PROTOCOL_PALETTE: readonly string[] = [
  '#7FB4D8', // slate blue
  '#C9A96B', // brass
  '#A98FD1', // lilac
  '#6BBFB4', // sea
  '#D18F9E', // rose
  '#9DBF6B', // moss
  '#8FA1D1', // periwinkle
  '#B4B48C', // sage
];

// Deterministic kind → hue. A hash, not a lookup table, so a workflow
// published tomorrow gets its color with zero code change (registries
// over hardcoded paths — the palette is the only fixed data).
export function protocolHue(kind: string): string {
  // FNV-1a with an avalanche finish — a plain multiplicative roll
  // mod 8 sent ship-a-change and pr-train to the same slot.
  let h = 2166136261;
  for (let i = 0; i < kind.length; i++) {
    h ^= kind.charCodeAt(i);
    h = Math.imul(h, 16777619) >>> 0;
  }
  h ^= h >>> 15;
  h = Math.imul(h, 2246822519) >>> 0;
  h ^= h >>> 13;
  return PROTOCOL_PALETTE[(h >>> 0) % PROTOCOL_PALETTE.length] ?? PROTOCOL_PALETTE[0]!;
}
