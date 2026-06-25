// Shared sim-clock store for the SPA. Backs the SimClockBadge,
// the `appToday()` / `appNow()` helpers every page reads instead
// of `new Date()`, and the optional view-as-of override that lets
// operators scrub the whole UI to a past date for read-only
// inspection.
//
// Design — three layers of "what is today":
//   1. asOfOverride: when set, the user has explicitly pinned the
//      UI to a historical date via the SimClockBadge scrubber.
//      `appToday()` returns this; pages should display a banner
//      and suppress write actions.
//   2. simClock.current_sim_date: the brewery sim's current day.
//      Backed by `/api/jobs/sim-clock/stream` (SSE) with a
//      `/api/jobs/live` poll fallback. `null` on real tenants
//      without a sim_clock row.
//   3. Wall clock: terminal fallback. Real tenants land here.
//
// Why a `.svelte.ts` file: module-level `$state` only compiles in
// .svelte.ts modules, which is how Svelte 5 exposes shared
// reactive state without a Svelte store.
//
// See: docs/design/correctness-protocol.md (model the current time
// as an input we control, end-to-end — backend + SPA halves).

import type { SimClockState } from './sim-clock-types';

let _clock = $state<SimClockState | null>(null);
let _asOfOverride = $state<string | null>(null);

export const simClock = {
  get value(): SimClockState | null {
    return _clock;
  },
  get asOfOverride(): string | null {
    return _asOfOverride;
  },
  set(next: SimClockState | null): void {
    _clock = next;
  },
  setAsOfOverride(next: string | null): void {
    _asOfOverride = next;
  },
};

/**
 * Effective "today" as YYYY-MM-DD. Honors the view-as-of override
 * first, then the sim clock's current_sim_date, then wallclock.
 *
 * Use this anywhere a SPA page picks a default `as_of` query
 * parameter, an ISO date for a form field, or a "today" comparison
 * for due-date / overdue / window calculations.
 */
export function appToday(): string {
  if (_asOfOverride) return _asOfOverride;
  if (_clock?.current_sim_date) return _clock.current_sim_date;
  return new Date().toISOString().slice(0, 10);
}

/**
 * Effective "now" as a Date object. Sim time anchors at midnight
 * UTC of the sim day. Use for date-arithmetic — "due in N days",
 * "X days since hire" — where the SPA needs a Date, not a string.
 *
 * Do NOT use for capture timestamps (when a fetch landed, when a
 * user clicked Submit): those are wallclock concerns and belong
 * with `new Date()` directly.
 */
export function appNow(): Date {
  if (_asOfOverride) return new Date(`${_asOfOverride}T00:00:00Z`);
  if (_clock?.current_sim_date) {
    return new Date(`${_clock.current_sim_date}T00:00:00Z`);
  }
  return new Date();
}

/**
 * True when the user has scrubbed the clock to a past date via the
 * SimClockBadge "View as of" picker. Pages should render a banner
 * (banner copy is the page's responsibility) and disable any write
 * UI — historical views are read-only by design.
 */
export function isTimeTravelMode(): boolean {
  return _asOfOverride !== null;
}
