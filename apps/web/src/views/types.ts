// Wire types for Views. Mirrors crates/core/boss-views/src/types.rs;
// deserialized once, here, at the fetch call site.

export type ViewSource = 'subjects' | 'jobs' | 'steps' | 'events';
export type ViewLayout = 'table' | 'list' | 'count';
export type Visibility = 'private' | 'shared';

export type View = Readonly<{
  id: string;
  owner_id: string;
  title: string;
  source: ViewSource;
  filter: string;
  columns: ReadonlyArray<string>;
  layout: ViewLayout;
  visibility: Visibility;
  created_at: string;
  updated_at: string;
}>;

/// `owner_id` is deliberately absent: the server derives it from the
/// authenticated caller. It used to be here, which made ownership a
/// value the client could pick.
export type ViewInput = Readonly<{
  title: string;
  source: ViewSource;
  filter: string;
  columns: ReadonlyArray<string>;
  layout: ViewLayout;
  visibility: Visibility;
}>;

export type ViewResults = Readonly<{
  view_id: string;
  source: ViewSource;
  layout: ViewLayout;
  rows: ReadonlyArray<Record<string, unknown>>;
  matched: number;
  /// Filter terms the database answered. Zero with a filter set means
  /// nothing could be narrowed, so the count is over the newest rows
  /// only — a different and much weaker claim than a plain truncation.
  pushed_down: number;
  /// The scan hit its ceiling before running out of candidates, so
  /// `matched` is a floor. Surfaced in the UI rather than swallowed —
  /// a count presented as complete when it isn't is worse than no
  /// count.
  truncated: boolean;
}>;

/// Fields each source offers, for the column picker and as a hint for
/// what a filter can name. Kept in step with the SELECT lists in
/// boss-views' query.rs by hand — the alternative is a schema endpoint,
/// which is worth building when a fourth source appears.
export const SOURCE_FIELDS: Readonly<Record<ViewSource, ReadonlyArray<string>>> = {
  subjects: ['kind', 'id', 'label', 'created_at', 'retired_at'],
  jobs: [
    'id',
    'kind',
    'subject_kind',
    'subject_id',
    'title',
    'owner_id',
    'status',
    'priority',
    'opened_on',
    'closed_on',
    'tags',
    'created_at',
  ],
  steps: [
    'id',
    'job_id',
    'kind',
    'title',
    'assignee_id',
    'status',
    'sort_order',
    'blocked_by',
    'completed_on',
    'notes',
    'created_at',
    'updated_at',
  ],
  events: ['id', 'event_id', 'kind', 'source', 'timestamp', 'payload'],
};
