// Flights — which in-flight changes are on for the person looking at
// this page (design c4c2a607, backlog 73c31776).
//
// A fork in source names its flight by code:
//
//   {#if flightOn('it-map-motion')} new path {:else} old path {/if}
//
// The string is the flight's identity, not its state. The state lives
// only in the flight's packet, and the server resolves it for this
// viewer: the gateway inlines the answer into index.html, so the first
// paint already takes the right path. A page with no readable inline
// answer retries the same read until the viewer's answer is known.
//
// Unlisted is off. With no answer at all every flight is off, so a
// failed read shows the old path — the safe one — while saying unread.

import { flightIsOn, flightsFromAnswer, type FlightsOn } from './flights-inline';

export { flightsFromAnswer, type FlightsOn };

const inlined = flightsFromAnswer((globalThis as { __BOSS_FLIGHTS__?: unknown }).__BOSS_FLIGHTS__);

export const flights = $state<{ on: FlightsOn; retrying: boolean }>({ on: inlined, retrying: false });

/// 04fcbecd: a roll is unread, not a lasting OFF decision. One read at
/// a time, with bounded backoff; a valid empty list is known and stops.
/// The mount owns cancellation, including the timer between attempts.
export async function loadFlights(signal?: AbortSignal): Promise<void> {
  let delay = 1_000;
  while (flights.on === null && !signal?.aborted) {
    flights.retrying = false;
    try {
      const r = await fetch('/api/flights/mine', { signal });
      const on = r.ok ? flightsFromAnswer(await r.json()) : null;
      if (signal?.aborted) return;
      flights.on = on;
    } catch {
      if (signal?.aborted) return;
      flights.on = null;
    }
    if (flights.on !== null) return;
    flights.retrying = true;
    await new Promise<void>(resolve => {
      const done = () => {
        clearTimeout(timer);
        signal?.removeEventListener('abort', done);
        resolve();
      };
      const timer = setTimeout(done, delay);
      signal?.addEventListener('abort', done, { once: true });
      if (signal?.aborted) done();
    });
    delay = Math.min(delay * 2, 30_000);
  }
}

/// Is the flight `code` on for this viewer? Only when the server said so.
export function flightOn(code: string): boolean {
  return flightIsOn(flights.on, code);
}
