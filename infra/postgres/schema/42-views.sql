-- =========================================================================
-- 42-views.sql — the View registry.
-- =========================================================================

-- A **View** is a saved composition over the Information API: a source,
-- a filter, a set of columns, a layout. It is the personal rung of the
-- extensibility ladder — below "author a Workflow" there was previously
-- nothing, so an operator who wanted to look at the information a
-- different way had to ask for a frontend change or keep a spreadsheet.
--
-- This table holds DEFINITIONS, never results. That is the load-bearing
-- distinction: a View's content is computed from the same projections
-- every other surface reads, so it cannot drift from them and two people
-- running the same View see the same numbers. A row here that cached its
-- own results would be the silo this design exists to refuse.
--
-- Recorded in docs/design/home-workspace-and-department-apps.md.

CREATE TABLE IF NOT EXISTS views (
    id          TEXT PRIMARY KEY,
    -- The employee this View belongs to. Views are personal first;
    -- `visibility` is what widens them.
    owner_id    TEXT NOT NULL,
    title       TEXT NOT NULL,
    -- 'subjects' | 'jobs' | 'events' — the three the search index also
    -- unifies, because they are three projections of one log.
    source      TEXT NOT NULL,
    -- A boss-expr predicate, evaluated per row by boss-views. Stored as
    -- text and never concatenated into SQL: the filter runs in the
    -- service against a JSON row, which is what keeps operator-authored
    -- text away from the query planner.
    filter      TEXT NOT NULL DEFAULT '',
    -- Ordered field names. Empty means "the source's own shape".
    columns     TEXT[] NOT NULL DEFAULT '{}',
    -- 'table' | 'list' | 'count'
    layout      TEXT NOT NULL DEFAULT 'table',
    -- 'private' | 'shared'. Sharing takes no promotion Job (Q4) —
    -- flipping this IS the act of sharing. Inclusion in a department's
    -- views is a separate, later, submitted thing.
    visibility  TEXT NOT NULL DEFAULT 'private',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- The list query is always "everything I own, plus everything shared".
CREATE INDEX IF NOT EXISTS views_owner ON views (owner_id);
CREATE INDEX IF NOT EXISTS views_shared ON views (visibility) WHERE visibility = 'shared';
