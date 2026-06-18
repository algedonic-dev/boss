-- =========================================================================
-- 07-content.sql — Content — HR bulletins, company manual, file references.
-- =========================================================================


CREATE TABLE IF NOT EXISTS bulletins (
    id              UUID PRIMARY KEY,
    title           TEXT NOT NULL,
    body            TEXT NOT NULL,
    actor_id       TEXT NOT NULL,
    posted_on       DATE NOT NULL DEFAULT CURRENT_DATE,
    expires_on      DATE,
    priority        TEXT NOT NULL
                    CHECK (priority IN ('normal','pinned','urgent'))
                    DEFAULT 'normal',
    audience        JSONB NOT NULL DEFAULT '{"all": true}'::jsonb,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);


-- Covering index for the common "what's live today?" read path. A
-- partial index with `WHERE expires_on IS NULL OR expires_on >=
-- CURRENT_DATE` would be tighter but Postgres rejects CURRENT_DATE
-- in index predicates (STABLE, not IMMUTABLE). Callers apply the
-- expiry filter at query time.
CREATE INDEX IF NOT EXISTS bulletins_active
    ON bulletins (posted_on DESC, priority, expires_on);


-- Per-employee dismissal state. Dismissed rows hide on the caller's My
-- Day forever, but stay visible on the admin surface + audit log.
CREATE TABLE IF NOT EXISTS bulletin_dismissals (
    bulletin_id     UUID NOT NULL REFERENCES bulletins(id) ON DELETE CASCADE,
    employee_id     TEXT NOT NULL,
    dismissed_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (bulletin_id, employee_id)
);


CREATE INDEX IF NOT EXISTS bulletin_dismissals_employee
    ON bulletin_dismissals (employee_id);


CREATE TABLE IF NOT EXISTS manual_sections (
    id              UUID PRIMARY KEY,
    slug            TEXT NOT NULL UNIQUE,
    parent_slug     TEXT REFERENCES manual_sections(slug),
    title           TEXT NOT NULL,
    body            TEXT NOT NULL,
    sort_order      INTEGER NOT NULL DEFAULT 0,
    audience        JSONB NOT NULL DEFAULT '{"all": true}'::jsonb,
    current_version INTEGER NOT NULL DEFAULT 1,
    published       BOOLEAN NOT NULL DEFAULT true,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);


CREATE INDEX IF NOT EXISTS manual_sections_tree
    ON manual_sections (parent_slug, sort_order);


CREATE TABLE IF NOT EXISTS manual_section_history (
    id              BIGSERIAL PRIMARY KEY,
    section_id      UUID NOT NULL REFERENCES manual_sections(id) ON DELETE CASCADE,
    version         INTEGER NOT NULL,
    title           TEXT NOT NULL,
    body            TEXT NOT NULL,
    audience        JSONB NOT NULL,
    edited_by       TEXT NOT NULL,
    edited_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    reason          TEXT,
    UNIQUE (section_id, version)
);


CREATE INDEX IF NOT EXISTS manual_section_history_section
    ON manual_section_history (section_id, version DESC);


-- -----------------------------------------------------------------------------
-- File references — first-class attachments on Subjects/Jobs/Steps/Events.
-- See docs/architecture-decisions.md §Content, files, knowledge.
--
-- Bytes live in object storage; this table is the metadata layer.
-- `id` mirrors the audit_log event id of the `content.file.attached`
-- event so the rebuilder can re-INSERT idempotently. `(bucket,
-- object_key)` is unique because object_key is `sha256/<hash>` and
-- two refs sharing the same hash share storage automatically (refcount
-- handled by the GC sweep, not here).
-- -----------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS file_refs (
    id            UUID PRIMARY KEY,
    target_kind   TEXT NOT NULL CHECK (target_kind IN ('subject', 'job', 'step', 'event')),
    target_id     TEXT NOT NULL,
    bucket        TEXT NOT NULL,
    object_key    TEXT NOT NULL,
    sha256        TEXT NOT NULL,
    size_bytes    BIGINT NOT NULL CHECK (size_bytes >= 0),
    mime          TEXT NOT NULL,
    filename      TEXT NOT NULL,
    uploaded_by   TEXT NOT NULL,
    uploaded_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    deleted_at    TIMESTAMPTZ,
    UNIQUE (bucket, object_key)
);


CREATE INDEX IF NOT EXISTS file_refs_target
    ON file_refs (target_kind, target_id) WHERE deleted_at IS NULL;

CREATE INDEX IF NOT EXISTS file_refs_sha
    ON file_refs (sha256);

