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
  /// Every row of the source, or only the rows the CALLER may read
  /// (backlog 5392cf23). A View is scoped to whoever runs it, so a
  /// shared View can show two people different numbers; this is what
  /// says so. A caller who may read none of the source gets a 403,
  /// never zero rows.
  scope: ResultScope;
}>;

export type ResultScope = 'all' | 'owners';

/// `GET /api/views/sources` — what each source offers a View author,
/// served from the lists boss-views' resolver selects and pushes with
/// (backlog 4a8939b5). The page used to keep its own copies of the
/// fields, the pushable names and the ceiling, "in step with query.rs
/// by hand"; two of the three had drifted.
export type ViewSources = Readonly<{
  scan_ceiling: number;
  sources: ReadonlyArray<SourceSchema>;
}>;

export type SourceSchema = Readonly<{
  source: ViewSource;
  /// Every field a row carries, in the column picker's order.
  fields: ReadonlyArray<string>;
  /// The fields a filter term on which the database answers.
  pushable: ReadonlyArray<PushableField>;
}>;

/// `text` pushes by equality or a set, `timestamp` by a range, `json`
/// through a dotted path (`payload.sku`, `metadata.department`).
export type PushableField = Readonly<{ field: string; type: 'text' | 'timestamp' | 'json' }>;
