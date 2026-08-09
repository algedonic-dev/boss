-- =========================================================================
-- 99-search.sql — Global search index (applies LAST).
-- =========================================================================

-- A projection of `audit_log`, like `jobs` or `financial_facts` — NOT a
-- query over the domain tables.
--
-- Recorded in docs/architecture-decisions.md §Search §Decision history (Q2).
-- Reading `accounts`/`assets`/`employees` in place is cheaper and is
-- what the previous implementation did, but it means anything that has
-- fallen out of a domain projection is unfindable — the wrong answer
-- for a system whose log is the record. This rebuilds from the log, so
-- search reproduces rather than drifts.
--
-- One row per findable thing, across the three kinds the design names
-- as the unified result set:
--   subject  — the identity-bearing things (accounts, employees, …)
--   job      — the work
--   event    — what happened
--
-- `subject_kind` / `subject_id` are the join key that makes the unified
-- answer possible: a hit on a Subject pulls the Jobs about it and the
-- events behind those, because all three carry the same reference.
-- Without that column this is three search boxes in a trenchcoat, which
-- is exactly what the design says it must not be.
CREATE TABLE IF NOT EXISTS search_index (
    ref_kind      TEXT NOT NULL CHECK (ref_kind IN ('subject', 'job', 'event')),
    -- '{kind}:{id}' for subjects, the job id for jobs, the audit_log id
    -- for events.
    ref_id        TEXT NOT NULL,
    -- What this row is ABOUT — the join key described above.
    subject_kind  TEXT,
    subject_id    TEXT,
    -- What a human reads in a result row.
    title         TEXT NOT NULL,
    -- Secondary matchable text (job kind, event kind, id fragments).
    -- Separate from `title` so ranking can weight them apart.
    body          TEXT NOT NULL DEFAULT '',
    -- Sim-time of the underlying fact. The design refuses to score
    -- across kinds — Subjects, then Jobs, then Events — so recency only
    -- ever breaks ties WITHIN a kind.
    occurred_at   TIMESTAMPTZ,
    indexed_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (ref_kind, ref_id)
);


-- Generated, not trigger-maintained: the rebuild is a bulk
-- TRUNCATE-then-insert, and a generated column cannot drift from the
-- row it summarises. Weights follow the design's ranking rule — 'A' for
-- the title (what you typed is probably a name), 'B' for the body.
ALTER TABLE search_index
    ADD COLUMN IF NOT EXISTS tsv tsvector
    GENERATED ALWAYS AS (
        setweight(to_tsvector('english', coalesce(title, '')), 'A') ||
        setweight(to_tsvector('english', coalesce(body, '')), 'B')
    ) STORED;

CREATE INDEX IF NOT EXISTS search_index_tsv ON search_index USING GIN (tsv);

-- The unified-result join: given a matching Subject, find its Jobs and
-- events without a second scan.
CREATE INDEX IF NOT EXISTS search_index_subject
    ON search_index (subject_kind, subject_id);

-- Prefix matching on ids and codes, which English tokenising handles
-- badly — an operator pasting `inv-step-c9cd8f…` is not writing prose.
CREATE INDEX IF NOT EXISTS search_index_ref_id_trgm
    ON search_index (ref_id text_pattern_ops);

CREATE INDEX IF NOT EXISTS search_index_kind ON search_index (ref_kind);


-- -----------------------------------------------------------------------------
-- Retired: search_all()
-- -----------------------------------------------------------------------------
--
-- The previous global search: one SQL function UNION-ing seven Tier-2
-- domain tables (employees, asset_models, assets, accounts, tickets,
-- bulletins, manual_sections), called from boss-people's
-- /api/people/search, for an Omnibox that no longer exists in the SPA.
--
-- Superseded rather than extended. Every decision in
-- docs/architecture-decisions.md §Search points away from it: it read domain
-- tables directly (Q2 chose a log-rooted projection), it lived in a
-- Tier-2 crate (Q1 put core identity in core), and it covered no Jobs
-- and no events at all — which is the entire unified-layer claim (Q3).
-- Its seven entity types are all reachable through `subjects` here.
--
-- Dropped rather than left dormant: two differently-shaped searches
-- over one tenant is the exact incoherence this system exists to avoid,
-- and a dormant one is the kind that gets called by accident.
DROP FUNCTION IF EXISTS search_all(text, integer);
