// Narrow wire types for the Simulator app's reads. Cloned from
// apps/web/src/landing/types.ts (only the fields these pages use) —
// domain wire types stay per-app and are NOT promoted to web-kit. The
// shared sim-clock type IS in web-kit; re-exported here for local
// importers.

export type { SimClockState } from '@boss/web-kit/sim-clock-types';
import type { SimClockState } from '@boss/web-kit/sim-clock-types';

// One row of the live recent-jobs feed (GET /api/jobs/live → recent[]).
export type JobLiveRow = Readonly<{
  id: string;
  kind: string;
  title: string;
  status: string;
  priority: string;
  subject_kind: string;
  subject_id: string;
  opened_on: string;
}>;

// GET /api/jobs/live payload. `counts` is per-kind open counts;
// `open_total` is the sum; `recent` is a small most-recent window;
// `sim_clock` is the engine's clock snapshot (null on fresh DBs).
export type JobLiveSummary = Readonly<{
  counts: Readonly<Record<string, number>>;
  open_total: number;
  recent: ReadonlyArray<JobLiveRow>;
  sim_clock?: SimClockState | null;
}>;

// One row of the audit-log tail (GET /api/events/tail → AuditEntry[]).
export type AuditEntry = Readonly<{
  event_id: string;
  timestamp: string;
  source: string;
  kind: string;
  payload: unknown;
}>;

// --- Simulator telemetry (GET /simulator/api/telemetry): how the daemon
// is engaging the public API. Mirrors boss-brewery-engine's
// sim_control::SimTelemetry (served by the daemon, proxied by
// boss-simulator). ---

export type SimCadence = Readonly<{
  sim_date: string | null;
  paused: boolean;
  epoch_start: string | null;
  epoch_end: string | null;
  warp_factor: number | null;
  days_per_tick: number | null;
  tick_interval_seconds: number | null;
}>;

// GET /simulator/api/clock — clock-api's authoritative ClockNow (it owns
// sim time + warp + paused). The cockpit reads its clock readouts here, so
// they're correct even while the daemon's /telemetry is down (e.g. mid
// seed-rebuild). Mirrors boss-clock's ClockNow.
export type ClockNow = Readonly<{
  now: string;
  simulated: boolean;
  epoch_start: string | null;
  epoch_end: string | null;
  paused: boolean;
  restart_in_progress: boolean;
  warp_factor: number | null;
}>;

// Workforce step transitions — the PUT /api/jobs/{}/steps engagement.
export type WorkforceStats = Readonly<{
  checkins: number;
  claimed: number;
  completed: number;
  deferred: number;
  in_progress: number;
  errors: number;
}>;

// Per-domain API writes — the step.done side-effect POSTs to the services.
export type ApiWrites = Readonly<{
  asset_events: number;
  invoices_created: number;
  invoices_updated: number;
  shipments: number;
  agreements: number;
  jobs: number;
  purchase_orders: number;
  messages: number;
  account_notes: number;
  tax_filings: number;
  bank_settlements: number;
  scheduled_assignments: number;
  revenue_schedules: number;
  days_flushed: number;
  errors: number;
}>;

// One tick's worth of engagement (the recent-activity ring buffer).
export type TickActivity = Readonly<{
  tick: number;
  sim_date: string | null;
  claimed: number;
  completed: number;
  deferred: number;
  errors: number;
}>;

// --- Per-actor API engagement (the cockpit's actor panels) ---
// The sim acts as the workforce (employees, by role) + the named
// counterparty chains (which decode to Account / Vendor / Bank), plus the
// Environment (world generation + materialization). Each actor's calls are
// tallied per endpoint, on the ack.
export type ActorKind = 'employee' | 'account' | 'vendor' | 'bank' | 'environment';

export type EndpointCount = Readonly<{
  endpoint: string;
  calls: number;
  errors: number;
}>;

export type ActorActivity = Readonly<{
  kind: ActorKind;
  label: string;
  calls: number;
  errors: number;
  // Distinct acting identities behind this rollup — how many people are
  // the `shipping-clerk` role, how many accounts the `ar-aging` chain
  // touched. 0 when no identity was attributed (Environment).
  distinct: number;
  endpoints: ReadonlyArray<EndpointCount>;
}>;

export type SimTelemetry = Readonly<{
  actor: string;
  role: string;
  api_base: string;
  started_unix: number;
  cadence: SimCadence;
  tick_count: number;
  last_tick_unix: number | null;
  workforce: WorkforceStats;
  api_writes: ApiWrites;
  recent: ReadonlyArray<TickActivity>;
  // How the sim engages the API, by who's acting (the actor panels).
  actors: ReadonlyArray<ActorActivity>;
  // Sim-date at the first tick — the calls/sim-day rate denominator.
  started_sim_date: string | null;
}>;

// --- Simulator behavior config (GET/POST /simulator/api/config). The
// editable subset of the daemon's effective config; every other field
// is preserved verbatim through the round-trip via the `[k: string]:
// unknown` passthrough on each level. The POST body must be a
// structurally-complete config (all fields from the GET, with edits
// applied) so the daemon's validation passes. NOT Readonly — the
// Controls editor binds inputs directly to the nested objects. ---

export type SimBehaviorConfig = {
  meta: { step_speed_multiplier?: number | null; [k: string]: unknown };
  job_rates: Record<
    string,
    {
      rate: number;
      weekday_multiplier?: number | null;
      weekend_multiplier?: number | null;
      [k: string]: unknown;
    }
  >;
  subject_rates: Record<string, { rate: number; [k: string]: unknown }>;
  counterparty: Record<
    string,
    {
      emit_probability: number;
      delay: { mean_days: number; spread_days?: number; [k: string]: unknown };
      [k: string]: unknown;
    }
  >;
  // AnomalyRates is #[serde(transparent)] on the daemon — each entry is
  // the flat prob-name → probability map directly (NOT wrapped in
  // `probs`). Round-trips transparently.
  anomalies: Record<string, Record<string, number>>;
  periodic: Record<
    string,
    { cadence: string; anchor_date: string; [k: string]: unknown }
  >;
  [k: string]: unknown;
};
