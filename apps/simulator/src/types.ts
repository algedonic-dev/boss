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
