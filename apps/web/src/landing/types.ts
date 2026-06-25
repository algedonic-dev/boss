// Wire types for the landing page's read-only surface. Mirrors
// the boss-jobs JobKindSpec shape — kept narrow (only the fields
// the graph + side panel use) so a future server-side change
// doesn't ripple through the SPA more than necessary.

export type JobKindStep = Readonly<{
  /// Stable kebab-case slug, unique within the JobKind. Edges in the
  /// Mermaid graph are derived from sibling steps' `ready_when`
  /// predicates referencing this slug as `steps.<title>.done`.
  title: string;
  kind: string;
  /// Readiness predicate. `"true"` marks an opening trigger.
  ready_when?: string;
  title_template?: string;
  sign_offs_required?: string[];
  authority_role?: string | null;
  metadata_defaults?: Readonly<Record<string, unknown>>;
  terminal?: Readonly<{ outcome: string }> | null;
}>;

export type JobKindSpec = Readonly<{
  kind: string;
  version: number;
  status: string;
  label: string;
  description?: string | null;
  category: string;
  subject_kinds: ReadonlyArray<string>;
  steps: ReadonlyArray<JobKindStep>;
  /// Free-form JobKind-level metadata blob (the `surfaces` hint
  /// lives here). Optional on the landing surface — only consumed
  /// where a page needs it.
  metadata?: Record<string, unknown>;
}>;

export type JobKindSummary = Readonly<{
  kind: string;
  label: string;
  category: string;
}>;

// Re-export so existing landing importers (`import { SimClockState }
// from './types'`) keep working after the type moved to web-kit.
export type { SimClockState } from '@boss/web-kit/sim-clock-types';
// …and import it locally too — `export type {…} from` does not bind
// the name in this module's scope, but JobLiveSummary below uses it.
import type { SimClockState } from '@boss/web-kit/sim-clock-types';

export type JobLiveSummary = Readonly<{
  counts: Readonly<Record<string, number>>;
  open_total: number;
  recent: ReadonlyArray<JobLiveRow>;
  /// Sim_clock snapshot. `null` for in-memory test paths or
  /// fresh DBs that haven't seeded a sim_clock row yet.
  sim_clock?: SimClockState | null;
}>;

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
