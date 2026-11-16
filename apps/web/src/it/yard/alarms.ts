// THE ALARMS BOARD — every station that is neither clear nor full, with
// the server's own why (design e765b3fc §4a). It was the transit map's
// board (transit.ts) and rode onto the tree floor as R2.5a (c1996aae);
// R3 of the transit-map retirement (backlog 23a4b4d7) deleted the map and
// kept the board, so its rule lives here, ordered by the regions the
// server declares (`REGION_NAMES`, pinned to `boss_jobs::regions::REGIONS`
// by regions.test.ts) rather than by transit.ts's hand-placed stations.
//
// CURRENT STATE ONLY (David, 2026-11-08 00:50Z): the board reads each
// region's state and why from GET /api/yard/regions, never its trend.

import { REGION_NAMES, type RegionState, type Regions } from './regions';

/** The states that raise an alarm: not clear, and not full — a station
 *  at its capacity and moving is the good state (design e765b3fc §4a). */
type Alarming = Exclude<RegionState, 'clear' | 'full'>;

export type Alarm = Readonly<{ name: string; label: string; state: Alarming; why: string }>;

const SEVERITY: Readonly<Record<Alarming, number>> = { troubled: 0, attention: 1 };

const NO_READING = 'the server answered no reading for this region';

/** A region's name as words: `shop-floor` reads "shop floor". */
export const alarmLabel = (name: string): string => name.replace(/-/g, ' ');

/** Every region that is neither clear nor full, with the server's own
 *  why — troubled first, then attention, each in the declared order,
 *  then any region the payload carries that is not declared. A declared
 *  region the read did not carry is an alarm: an unread station is not a
 *  quiet one. */
export function alarmsOf(read: Regions): ReadonlyArray<Alarm> {
  const names = [...new Set<string>([...REGION_NAMES, ...read.regions.map((r) => r.name)])];
  return names
    .flatMap((name): Alarm[] => {
      const r = read.regions.find((x) => x.name === name);
      const state: RegionState = r?.state ?? 'troubled';
      if (state === 'clear' || state === 'full') return [];
      return [{ name, label: alarmLabel(name), state, why: r?.why ?? NO_READING }];
    })
    .map((a, i) => ({ a, i }))
    .sort((x, y) => SEVERITY[x.a.state] - SEVERITY[y.a.state] || x.i - y.i)
    .map(({ a }) => a);
}
