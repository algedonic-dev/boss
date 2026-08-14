// The filer's watchlist — the packets I filed, and what became of
// them.
//
// Origin (David, 2026-08-13): "Once the user feedback results in
// either a shipped change or some other terminal state, it can be
// closed without the filer approving. But, we should always notify the
// filer with the terminal state and it should show in their
// watchlist."
//
// This is a station lens, not a bespoke read: the whole surface is
// `GET /api/stations/my-watchlist/queue`, one registry row whose
// predicate is `metadata_equals: {submitted_by: "@me"}` and whose
// `@me` the server binds to the requesting actor. So the queue this
// module renders is derived membership over the caller's own
// policy-scoped packets — the same machinery every other queue lens
// reads through, with no per-person rows anywhere.
//
// Not to be confused with `accounts/WatchlistPage.svelte` at
// `/watchlist` — that is the account CHURN watchlist, a different
// domain and a different noun. "Watchlist" here is David's own word
// for the filer's receipt, and this one is scoped to a person, which
// is why it lives on My Day rather than at a route of its own.
//
// Two things this lens deliberately does NOT do:
//   - re-sort. Ordering is the station's declared discipline
//     (`recency` — newest activity first), and two orderings of one
//     queue is one too many.
//   - infer an outcome. The terminal state is read off the packet's
//     own `metadata.outcome`, which the terminal close stamps. A
//     closed packet with nothing recorded says "closed", because that
//     is the whole of what is known.

import { isSim, type PacketCardData } from '@boss/web-kit/ui/packet-card';

/// A packet as the station-queue envelope serializes it (bare Jobs,
/// no steps). Only the fields this lens reads.
export type WatchlistJob = Readonly<{
  id: string;
  kind: string;
  title: string;
  status: string;
  opened_on: string;
  closed_on?: string | null;
  tags?: readonly string[];
  metadata?: Record<string, unknown> | null;
  simulated?: boolean;
}>;

/// How a terminal state renders. `tone` names a DL status token —
/// never a color — so the watchlist stays inside the design language.
export type OutcomeTone = 'ok' | 'warn' | 'static';

export type Outcome = Readonly<{ label: string; tone: OutcomeTone }>;

/// The statuses that mean the packet has left the network.
const TERMINAL_STATUSES: readonly string[] = ['closed', 'cancelled'];

/// Tone per outcome. Only outcomes whose tone is NOT neutral need a
/// row here — everything else reads as itself in `--static`, so a
/// disposition added to the Workflow registry tomorrow shows up
/// without a code change (registries over hardcoded paths).
///
///   --ok    the filer's report produced a change
///   --warn  it ended without one; a decision they should notice
///   --static it went somewhere else, or we only know that it closed
const OUTCOME_TONES: Readonly<Record<string, OutcomeTone>> = {
  completed: 'ok',
  declined: 'warn',
};

/// The terminal state to show for a packet, or null while it is still
/// in flight. Pure.
export function outcomeOf(job: WatchlistJob): Outcome | null {
  if (!TERMINAL_STATUSES.includes(job.status)) return null;
  const recorded = (job.metadata ?? {})['outcome'];
  const label = typeof recorded === 'string' && recorded.length > 0 ? recorded : 'closed';
  return { label, tone: OUTCOME_TONES[label] ?? 'static' };
}

/// A packet in the shared card grammar. The mono provenance line is
/// where the feedback was filed (its route), falling back to when —
/// and once the packet closes, when it closed rides along, so a card
/// lifted out of this list still says what happened and when.
///
/// The outcome is NOT among the tags: tag chips are uniformly
/// `--static`, and the outcome's whole job is to carry a tone. It
/// renders beside the card instead.
export function watchlistPacket(job: WatchlistJob): PacketCardData {
  const md = (job.metadata ?? {}) as { route?: string };
  const where = md.route ?? `filed ${job.opened_on}`;
  return {
    id: job.id,
    kind: job.kind,
    branch: job.closed_on ? `${where} · closed ${job.closed_on}` : where,
    title: job.title,
    tags: (job.tags ?? []).filter(t => t !== 'feedback'),
    sim: isSim(job),
    skipReason: null,
  };
}

/// One rendered row: the card plus its terminal state, if it has one.
export type WatchlistEntry = Readonly<{
  card: PacketCardData;
  outcome: Outcome | null;
}>;

export type WatchlistState =
  | { kind: 'loading' }
  /// The station 404s (a deployment without the row yet) or 503s (no
  /// registry configured). Not an error — the section simply has
  /// nothing to say.
  | { kind: 'unavailable' }
  | { kind: 'error' }
  | { kind: 'ready'; entries: readonly WatchlistEntry[]; windowDays: number | null };

/// Pure classification of the queue endpoint's answer. Mirrors the
/// station map's reading of the same envelope so the two lenses agree
/// on what "the registry hasn't reached this deployment" looks like.
export function watchlistStateFromResponse(status: number, body: unknown): WatchlistState {
  if (status === 404 || status === 503) return { kind: 'unavailable' };
  if (status < 200 || status >= 300) return { kind: 'error' };
  if (body === null || typeof body !== 'object' || Array.isArray(body)) {
    return { kind: 'error' };
  }
  const envelope = body as {
    station?: unknown;
    data?: unknown;
    terminal_window_days?: unknown;
  };
  if (typeof envelope.station !== 'string' || !Array.isArray(envelope.data)) {
    return { kind: 'error' };
  }
  const jobs = envelope.data as WatchlistJob[];
  return {
    kind: 'ready',
    // Server order preserved: the station's discipline decided it.
    entries: jobs.map(j => ({ card: watchlistPacket(j), outcome: outcomeOf(j) })),
    windowDays:
      typeof envelope.terminal_window_days === 'number' ? envelope.terminal_window_days : null,
  };
}

/// The section's subtitle: why a closed packet is sitting in a queue.
/// Reads the window off the envelope rather than restating a constant,
/// so re-publishing the station row with a different window changes
/// what the page says.
export function windowNote(windowDays: number | null): string | null {
  if (windowDays === null) return null;
  const unit = windowDays === 1 ? 'day' : `${windowDays} days`;
  return `including packets closed in the last ${unit}`;
}

// ---------------------------------------------------------------------------
// I/O edge — a thin fetch over the pure mappers above.
// ---------------------------------------------------------------------------

export const WATCHLIST_STATION = 'my-watchlist';

/// Read-only, and safe for a guest: the server binds `@me` to the
/// requesting actor and answers an anonymous caller with an empty
/// queue rather than somebody else's.
export async function fetchWatchlist(): Promise<WatchlistState> {
  try {
    const r = await fetch(`/api/stations/${encodeURIComponent(WATCHLIST_STATION)}/queue`);
    const body: unknown = await r.json().catch(() => null);
    return watchlistStateFromResponse(r.status, body);
  } catch {
    return { kind: 'error' };
  }
}
