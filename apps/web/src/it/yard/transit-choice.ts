// THE VIEWER'S CROSSING TIME (David 2026-09-27: "maybe give me a few
// radio buttons"; backlog 7c581d3d). How long a live dot takes to cross
// a section is one viewer's convenience, not a fact about the yard, so it
// lives in that viewer's browser — never in the record, and never shared.
//
// Storage can refuse every call (a private window, blocked site data, a
// thumbnail capture), so each read and write is caught here and the map
// runs on the default: a refused read is 30 s, a refused write keeps the
// choice for this tab only. Storage is a parameter, so `bun test` drives
// both a working and a refusing one.

import { TRANSIT_CHOICES, TRANSIT_MS } from './live-motion';

export const TRANSIT_KEY = 'boss.it-map.transit-ms';

type Store = Pick<Storage, 'getItem' | 'setItem'>;

/** A stored value as a crossing: one of the four choices, else the default. */
export function transitOf(raw: string | null): number {
  const n = raw === null || !/^\d+$/.test(raw) ? NaN : Number(raw);
  return (TRANSIT_CHOICES as ReadonlyArray<number>).includes(n) ? n : TRANSIT_MS;
}

export function loadTransit(store: () => Store): number {
  try {
    return transitOf(store().getItem(TRANSIT_KEY));
  } catch {
    return TRANSIT_MS;
  }
}

export function saveTransit(store: () => Store, transit: number): void {
  try {
    store().setItem(TRANSIT_KEY, String(transit));
  } catch {
    // Storage refused: the choice holds for this tab and is not remembered.
  }
}
