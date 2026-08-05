-- =========================================================================
-- 43-event-facts.sql — indexed projection of audit_log, for Views.
-- =========================================================================

-- A projection of `audit_log`, like `jobs` or `search_index` — rebuilt
-- from the log, never written to directly.
--
-- Why it exists: a View over `events` filtered in-process over a capped
-- scan. The cap was 5,000 rows against a log that is already 704,000, so
-- a View reached 0.7% of it and no filter could reach the rest. The cap
-- was honest (results carried `truncated`) and useless.
--
-- The fix is not a bigger cap. It is columns Postgres can index: the
-- fields a filter actually names — `kind`, `subject_kind`, `subject_id`
-- — are lifted out of the JSON payload so a predicate on them becomes an
-- index scan instead of a sequential read the service then filters.
--
-- `payload` is kept whole. Filters can name anything inside it; those
-- terms simply do not push down and are evaluated in-process against the
-- narrowed rows. That split is deliberate and is the seam a real query
-- planner grows into: teaching the extractor a new shape makes queries
-- faster, never more correct, because the full predicate is always
-- re-evaluated on whatever SQL returns.

CREATE TABLE IF NOT EXISTS event_facts (
    -- The log row this fact came from. Doubles as the ordering key:
    -- audit_log ids are monotonic, so `ORDER BY audit_id DESC` is
    -- "newest first" without depending on a clock.
    audit_id     BIGINT PRIMARY KEY,
    event_id     UUID NOT NULL,
    kind         TEXT NOT NULL,
    source       TEXT,
    -- The event's own clock-routed time, not the row's insert time.
    occurred_at  TIMESTAMPTZ NOT NULL,
    -- Lifted out of payload because these are what filters name. NULL
    -- where the event carries no subject.
    subject_kind TEXT,
    subject_id   TEXT,
    -- Kept whole: display needs it, and any filter term that did not
    -- push down is evaluated against it.
    payload      JSONB
);

-- `kind = "..."` is the overwhelmingly common filter; pairing it with
-- audit_id DESC lets the index serve the ORDER BY too, so a narrow
-- View never sorts.
CREATE INDEX IF NOT EXISTS event_facts_kind ON event_facts (kind, audit_id DESC);

-- "everything that happened to this Subject" — the question the
-- unified-information-layer claim is actually about.
CREATE INDEX IF NOT EXISTS event_facts_subject
    ON event_facts (subject_kind, subject_id, audit_id DESC);

-- Time-windowed reads ("what happened last week").
CREATE INDEX IF NOT EXISTS event_facts_occurred ON event_facts (occurred_at DESC);
