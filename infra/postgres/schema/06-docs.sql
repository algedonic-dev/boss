-- =========================================================================
-- 06-docs.sql — Design doc review — decision tracker read-caches + flush jobs.
-- =========================================================================


-- -----------------------------------------------------------------------------
-- Design doc review (boss-docs-api)
--
-- Four tables backing the in-app design decision tracker. Git is the
-- source of truth for the markdown files under `docs/design/*.md`.
-- These tables hold:
--   1. `design_docs` + `design_questions` — read-caches over the git
--      state, refreshed by POST /api/design/reindex.
--   2. `design_pending_decisions` — ephemeral in-flight human clicks.
--   3. `design_flush_jobs` — immutable job records handed off to the
--      worker (Claude in chat today; a dispatched agent later).
-- The decision flow is described in docs/architecture-decisions.md
-- (How decisions evolve).
-- -----------------------------------------------------------------------------

-- Read-cache of design doc metadata. One row per markdown file under
-- docs/design/. Refreshed on reindex.
CREATE TABLE IF NOT EXISTS design_docs (
    path              TEXT PRIMARY KEY,
    title             TEXT NOT NULL,
    status            TEXT NOT NULL CHECK (status IN (
        'draft', 'in-review', 'approved', 'shipped', 'reopened',
        'superseded', 'living'
    )),
    pending_count     INTEGER NOT NULL DEFAULT 0,
    word_count        INTEGER NOT NULL DEFAULT 0,
    last_modified     TIMESTAMPTZ NOT NULL,
    last_author       TEXT NOT NULL,
    last_indexed_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_commit_sha   TEXT NOT NULL,
    content_html      TEXT NOT NULL
);


CREATE INDEX IF NOT EXISTS design_docs_status ON design_docs(status);

-- 2026-07-08: widen the CHECK to the full DocStatus vocabulary —
-- `living` (settled references) and `reopened` were missing, so a
-- reindex of any doc carrying them failed at the DB layer while the
-- Rust enum happily produced them. The schema_matches_doc_status_enum
-- pg test pins the two lists together now. Idempotent swap for
-- existing databases.
ALTER TABLE design_docs DROP CONSTRAINT IF EXISTS design_docs_status_check;
ALTER TABLE design_docs ADD CONSTRAINT design_docs_status_check CHECK (status IN (
    'draft', 'in-review', 'approved', 'shipped', 'reopened',
    'superseded', 'living'
));

CREATE INDEX IF NOT EXISTS design_docs_pending
    ON design_docs(pending_count) WHERE pending_count > 0;


-- Docs that FAILED to index, and why.
--
-- A rejected doc has no design_docs row by definition — that is what
-- rejection means — so without this table it is simply absent, and the
-- absence looks identical to "nobody wrote that doc yet".
--
-- docs/design/transactional-audit-log.md sat rejected from 2026-07-29
-- to 2026-08-04 for a real and correctly-detected reason (status
-- `living`, which asserts no open discussion, while carrying two
-- unresolved questions). The check worked; the message named the fix.
-- Nobody saw it, because the reason was returned in the reindex API
-- response and then discarded. A review process that silently drops
-- work is missing its algedonic signal — a pointed omission in this
-- codebase of all codebases.
--
-- Rows are owned by reindex: upserted when a doc fails, deleted the
-- moment it indexes cleanly. So a non-empty table means "right now,
-- these docs are not in the tracker", not "these once failed".
CREATE TABLE IF NOT EXISTS design_doc_rejections (
    path            TEXT PRIMARY KEY,
    reason          TEXT NOT NULL,
    -- When this doc started failing, preserved across reindexes so the
    -- surface can say "invisible for six days" rather than just
    -- "failed". Age is the part that makes it actionable.
    first_seen_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_seen_at    TIMESTAMPTZ NOT NULL DEFAULT NOW()
);


-- Read-cache of extracted open questions per doc. Re-derived on every
-- reindex via delete-and-insert per doc in a single transaction.
CREATE TABLE IF NOT EXISTS design_questions (
    id              TEXT PRIMARY KEY,           -- '{doc_path}#Q1'
    doc_path        TEXT NOT NULL
                      REFERENCES design_docs(path) ON DELETE CASCADE,
    anchor          TEXT NOT NULL,              -- 'Q1' or '{slug}-open-0'
    ordinal         INTEGER NOT NULL,
    title           TEXT NOT NULL,
    body_md         TEXT NOT NULL,
    proposal        TEXT,
    context_md      TEXT,
    -- Heading carries `(resolved)`. The question stays a row — the doc
    -- keeps its decision record — but stops counting as open. Without
    -- it every parsed question counted as open, so a doc whose review
    -- had just been flushed still reported its questions outstanding.
    resolved        BOOLEAN NOT NULL DEFAULT false,
    indexed_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

ALTER TABLE design_questions
    ADD COLUMN IF NOT EXISTS resolved BOOLEAN NOT NULL DEFAULT false;


CREATE INDEX IF NOT EXISTS design_questions_doc ON design_questions(doc_path);


-- Ephemeral in-flight decisions. Written by the decision panel as the
-- human clicks through questions. Cleared at flush job creation time
-- when the rows are snapshotted into the job's immutable payload.
CREATE TABLE IF NOT EXISTS design_pending_decisions (
    id              TEXT PRIMARY KEY,
    doc_path        TEXT NOT NULL,
    anchor          TEXT NOT NULL,
    kind            TEXT NOT NULL CHECK (kind IN ('accept', 'override')),
    resolution      TEXT NOT NULL,
    rationale       TEXT,
    decided_by      TEXT NOT NULL,
    decided_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (doc_path, anchor)
);


CREATE INDEX IF NOT EXISTS design_pending_decisions_doc
    ON design_pending_decisions(doc_path);


-- Flush jobs. Handed off to a worker (Claude in chat today; a
-- dispatched agent later). Immutable payload captures the snapshot of
-- pending decisions at job creation time so the worker can be replaced
-- without changing the contract.
CREATE TABLE IF NOT EXISTS design_flush_jobs (
    id              TEXT PRIMARY KEY,
    doc_path        TEXT NOT NULL
                      REFERENCES design_docs(path),
    status          TEXT NOT NULL CHECK (status IN (
        'queued', 'running', 'succeeded', 'failed'
    )),
    requested_by    TEXT NOT NULL,
    queued_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    started_at      TIMESTAMPTZ,
    completed_at    TIMESTAMPTZ,
    payload         JSONB NOT NULL,
    commit_sha      TEXT,
    error           TEXT
);


CREATE INDEX IF NOT EXISTS design_flush_jobs_status ON design_flush_jobs(status);

CREATE INDEX IF NOT EXISTS design_flush_jobs_queued ON design_flush_jobs(queued_at DESC);

