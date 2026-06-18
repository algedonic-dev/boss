-- =========================================================================
-- 03-jobs.sql — Jobs — Jobs, Steps, JobKind registry, Step UX Plugin registry.
-- =========================================================================


-- -----------------------------------------------------------------------------
-- Jobs — universal coordination primitive
-- -----------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS jobs (
    id              UUID PRIMARY KEY,
    kind            TEXT NOT NULL,
    subject_kind    TEXT NOT NULL,
    subject_id  TEXT NOT NULL,
    title           TEXT NOT NULL,
    owner_id        TEXT NOT NULL,
    status          TEXT NOT NULL DEFAULT 'draft' CHECK (status IN (
        'draft', 'open', 'blocked', 'pending-sign-off', 'closed', 'cancelled'
    )),
    priority        TEXT NOT NULL CHECK (priority IN (
        'emergency', 'urgent', 'standard', 'scheduled'
    )),
    opened_on       DATE NOT NULL,
    due_on          DATE,
    closed_on       DATE,
    metadata        JSONB NOT NULL DEFAULT '{}',
    tags            TEXT[] NOT NULL DEFAULT '{}',
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);


CREATE INDEX IF NOT EXISTS jobs_kind ON jobs(kind);

CREATE INDEX IF NOT EXISTS jobs_status ON jobs(status);

CREATE INDEX IF NOT EXISTS jobs_owner ON jobs(owner_id);

CREATE INDEX IF NOT EXISTS jobs_subject ON jobs(subject_kind, subject_id);

CREATE INDEX IF NOT EXISTS jobs_due ON jobs(due_on) WHERE due_on IS NOT NULL;


CREATE TABLE IF NOT EXISTS steps (
    id              UUID PRIMARY KEY,
    job_id          UUID NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    kind            TEXT NOT NULL DEFAULT 'generic',
    title           TEXT NOT NULL,
    assignee_id     TEXT,
    -- Status vocabulary: the 5 predicate-driven states of the v2
    -- StepStatus enum at crates/core/boss-core/src/job.rs.
    --   pending → ready → active → completed   (the traversed path)
    --   pending → skipped                       (provably N/A branch)
    status          TEXT NOT NULL DEFAULT 'pending' CHECK (status IN (
        'pending', 'ready', 'active', 'completed', 'skipped'
    )),
    sort_order      INTEGER NOT NULL DEFAULT 0,
    blocked_by      UUID[] NOT NULL DEFAULT '{}',
    -- Sign-off contract (docs/architecture-decisions.md §Step types
    -- are property bundles): role codes that must each stamp this step in
    -- its current shape before completion, and the stamps collected
    -- ([{authority_id, role, stamped_at, shape_hash}]). Stale stamps
    -- (shape changed after stamping) stay recorded as provenance.
    sign_offs_required JSONB NOT NULL DEFAULT '[]'::jsonb,
    sign_offs          JSONB NOT NULL DEFAULT '[]'::jsonb,
    -- Inline authoring: step-authored completion-contract
    -- fields, validated in union with the kind bundle's fields.
    fields             JSONB NOT NULL DEFAULT '[]'::jsonb,
    completed_on    DATE,
    metadata        JSONB NOT NULL DEFAULT '{}',
    notes           TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);


CREATE INDEX IF NOT EXISTS steps_job ON steps(job_id, sort_order);

CREATE INDEX IF NOT EXISTS steps_assignee ON steps(assignee_id) WHERE status IN ('ready', 'active');

CREATE INDEX IF NOT EXISTS steps_status ON steps(status);


-- ---------------------------------------------------------------------------
-- Job Kind Registry — see docs/architecture-decisions.md
-- (Jobs, JobKinds, Steps)
-- ---------------------------------------------------------------------------
-- Every Job in the system is an instance of a JobKind. This table is
-- the registry that lets department leads author new kinds without a
-- core-team PR. Append-only versioning: every edit creates a new
-- (kind, version+1) row. The partial unique index enforces exactly one
-- active row per kind at a time.
CREATE TABLE IF NOT EXISTS job_kinds (
    kind              TEXT NOT NULL,
    version           INT  NOT NULL,
    status            TEXT NOT NULL CHECK (status IN ('draft', 'active', 'retired')),
    label             TEXT NOT NULL,
    description       TEXT,
    category          TEXT NOT NULL,
    subject_kinds     JSONB NOT NULL,
    -- The flat step list. The DAG is implicit in each step's
    -- `ready_when` predicate — an edge A → B exists iff B's
    -- `ready_when` references A, not as an author-drawn graph.
    steps             JSONB NOT NULL,
    metadata_schema   JSONB NOT NULL DEFAULT '{}'::jsonb,
    entitlements      JSONB NOT NULL DEFAULT '{}'::jsonb,
    -- JobKind-level display/routing hints (not the per-Job metadata,
    -- not the metadata_schema describing per-Job fields). The first
    -- key is `surfaces` — which operational pages a JobKind appears
    -- on (e.g. `{ "surfaces": ["hr"] }`). Same `serde_json::Value`
    -- round-trip shape as metadata_schema / entitlements.
    metadata          JSONB NOT NULL DEFAULT '{}'::jsonb,
    -- JobKinds to spawn when a Job of this kind closes. Each entry
    -- is a `JobTrigger` (kind + subject_source + metadata_seed),
    -- per crates/boss-jobs/src/registry.rs. Empty array = no
    -- triggers (the common case). The runtime that fires triggers
    -- is shipped separately; this column is the data shape.
    on_complete_create JSONB NOT NULL DEFAULT '[]'::jsonb,
    owning_team       TEXT NOT NULL,
    authoring_job_id  UUID,
    -- Discriminates platform-bootstrap rows from operator edits.
    -- `bootstrap` = inserted by `boss-jobs-api`'s startup
    -- reconciler from `platform_kinds()`; any other value = the row
    -- came from a `job-kind-design` Job, the seed loader, or
    -- an admin PUT. The bootstrap reconciler uses this to decide
    -- whether a drifted row should self-heal (bootstrap-owned) or
    -- be preserved untouched (operator-owned). Same shape as
    -- `policy_rules.updated_by`.
    -- See docs/architecture-decisions.md (Jobs, JobKinds, Steps:
    -- JobKinds bootstrap through Jobs).
    created_by        TEXT NOT NULL DEFAULT 'bootstrap',
    created_at        TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (kind, version)
);


-- One active row per kind, at most. New publishes demote the previous
-- active row to retired in the same transaction.
CREATE UNIQUE INDEX IF NOT EXISTS job_kinds_one_active_per_kind
    ON job_kinds (kind) WHERE status = 'active';


CREATE INDEX IF NOT EXISTS job_kinds_category ON job_kinds (category) WHERE status = 'active';

CREATE INDEX IF NOT EXISTS job_kinds_authoring_job ON job_kinds (authoring_job_id)
    WHERE authoring_job_id IS NOT NULL;


-- Every Job row records the JobKind version it was born with, so the
-- snapshotted template can be looked up later (in-flight Jobs stay
-- pinned to the version they were opened under).
ALTER TABLE jobs ADD COLUMN IF NOT EXISTS job_kind_version INT NOT NULL DEFAULT 1;

CREATE INDEX IF NOT EXISTS jobs_kind_version ON jobs (kind, job_kind_version);


-- ---------------------------------------------------------------------------
-- Step UX Plugin Registry — see docs/architecture-decisions.md
-- (Step UX & frontend)
-- ---------------------------------------------------------------------------
-- Third-party step kinds ship as plugins and land here. The built-in
-- kinds (step_registry::v1()) stay implicit — the Rust registry merges
-- this table with the in-tree catalog at read time.
--
-- Shape mirrors job_kinds: append-only versioning, partial unique
-- index enforcing at most one active row per kind.
CREATE TABLE IF NOT EXISTS step_plugins (
    kind                TEXT NOT NULL,
    version             INT  NOT NULL,
    status              TEXT NOT NULL CHECK (status IN ('draft', 'active', 'retired')),
    label               TEXT NOT NULL,
    description         TEXT,
    category            TEXT NOT NULL,
    metadata_schema     JSONB NOT NULL,
    -- Gateway serves the frontend bundle from
    -- /var/lib/boss/step-plugins/<frontend_url>. The bundle is a static
    -- file on disk, not inline BYTEA.
    frontend_url        TEXT NOT NULL,
    owning_team         TEXT NOT NULL,
    authoring_job_id    UUID,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (kind, version)
);


CREATE UNIQUE INDEX IF NOT EXISTS step_plugins_one_active_per_kind
    ON step_plugins (kind) WHERE status = 'active';

CREATE INDEX IF NOT EXISTS step_plugins_category ON step_plugins (category) WHERE status = 'active';


-- Every step row records the plugin version it was born with
-- (0 = "no plugin pinned; use the active row"), so retired plugins
-- still render on in-flight Jobs.
ALTER TABLE steps ADD COLUMN IF NOT EXISTS step_plugin_version INT NOT NULL DEFAULT 0;


-- Step-as-Job recursion: a Step can embed a Job
-- when its work needs to be broken down into sub-tasks. Structural
-- column rather than a metadata-shape convention so Composite
-- traversal discovers embedded Jobs without parsing JSON.
-- Nullable; default null — existing Steps are unaffected.
ALTER TABLE steps ADD COLUMN IF NOT EXISTS embedded_job UUID REFERENCES jobs(id);

CREATE INDEX IF NOT EXISTS steps_embedded_job ON steps (embedded_job)
    WHERE embedded_job IS NOT NULL;


-- Baseline plugin seed. Idempotent (ON CONFLICT) so re-running the
-- schema on an existing DB is a no-op. `checklist` is the generic
-- checkbox-list surface, shipped as a plugin like any other step UX.
INSERT INTO step_plugins (
    kind, version, status, label, description, category,
    metadata_schema, frontend_url, owning_team
) VALUES (
    'checklist', 1, 'active', 'Checklist',
    'Generic checkbox list; auto-completes the step when every item is checked.',
    'generic',
    '{"type":"object","properties":{"items":{"type":"array","items":{"type":"object","properties":{"label":{"type":"string"},"checked":{"type":"boolean"}}}}}}',
    'checklist.js',
    'platform'
) ON CONFLICT (kind, version) DO NOTHING;


-- marketing-brief plugin — tier 1 of marketing-motion. Renders the
-- full brief body, target audience, and per-employee ack tracker. Acks
-- live inline in step.metadata.acknowledgements.
INSERT INTO step_plugins (
    kind, version, status, label, description, category,
    metadata_schema, frontend_url, owning_team
) VALUES (
    'marketing-brief', 1, 'active', 'Marketing Brief',
    'Cross-department alignment brief with body + audience + per-employee ack tracker. Backs tier 1 of marketing-motion.',
    'admin',
    '{"type":"object","properties":{"brief_md":{"type":"string"},"audience":{"type":"array","items":{"type":"string"}},"circulated_at":{"type":"string","format":"date-time"},"acknowledgements":{"type":"array","items":{"type":"object","properties":{"employee_id":{"type":"string"},"acknowledged_at":{"type":"string","format":"date-time"}}}}}}',
    'marketing-brief.js',
    'platform'
) ON CONFLICT (kind, version) DO NOTHING;


-- sr-triage plugin — tier 0 of field-service. Captures the mandatory
-- intake fields + the triage decision (dispatch / remote / parts-only).
INSERT INTO step_plugins (
    kind, version, status, label, description, category,
    metadata_schema, frontend_url, owning_team
) VALUES (
    'sr-triage', 1, 'active', 'SR Triage',
    'Service-request intake + triage decision. Captures mandatory fields at creation and records the dispatch/remote/parts-only decision at completion. Backs tier 0 of field-service.',
    'admin',
    '{"type":"object","properties":{"account_id":{"type":"string"},"device_serial":{"type":"string"},"failure_description":{"type":"string"},"priority":{"type":"string"},"intake_channel":{"type":"string"},"requester_contact_id":{"type":"string"},"jira_issue_key":{"type":"string"},"triage_outcome":{"type":"string","enum":["dispatch","remote","parts-only"]}},"required":["account_id","device_serial","failure_description","priority"]}',
    'sr-triage.js',
    'platform'
) ON CONFLICT (kind, version) DO NOTHING;


-- diagnostic-call plugin — tier 1 of field-service.
-- Call log with scheduled time, channel, attendees, join URL, notes,
-- and an optional manually-pasted recording URL.
INSERT INTO step_plugins (
    kind, version, status, label, description, category,
    metadata_schema, frontend_url, owning_team
) VALUES (
    'diagnostic-call', 1, 'active', 'Diagnostic Call',
    'Call log for scheduled video/phone sessions with a account. Captures scheduled time, channel, attendees, join URL, notes, optional recording URL. Backs tier 1 of field-service.',
    'admin',
    '{"type":"object","properties":{"scheduled_for":{"type":"string","format":"date-time"},"channel":{"type":"string"},"join_url":{"type":"string"},"attendees":{"type":"array","items":{"type":"string"}},"notes_md":{"type":"string"},"recording_url":{"type":"string"},"transcript_url":{"type":"string"},"ended_at":{"type":"string","format":"date-time"},"outcome":{"type":"string"}}}',
    'diagnostic-call.js',
    'platform'
) ON CONFLICT (kind, version) DO NOTHING;


-- marketing-launch plugin — tier 4 of marketing-motion.
-- Renders a compact launch calendar peek with the current motion
-- highlighted plus neighbors for timing conflicts. Read-only; date
-- editing happens inline on the step metadata.
INSERT INTO step_plugins (
    kind, version, status, label, description, category,
    metadata_schema, frontend_url, owning_team
) VALUES (
    'marketing-launch', 1, 'active', 'Marketing Launch',
    'Embedded launch-calendar peek with neighbors for timing-conflict awareness. Backs tier 4 of marketing-motion.',
    'admin',
    '{"type":"object","properties":{"launch_date":{"type":"string","format":"date"},"launch_channel":{"type":"string"},"notes":{"type":"string"}}}',
    'marketing-launch.js',
    'platform'
) ON CONFLICT (kind, version) DO NOTHING;


-- marketing-attribution plugin — tier 5 of marketing-motion.
-- Read-only rollup: linked opportunities + revenue-influenced + brief
-- ack rate. Asset-download enrichments are a future placeholder.
INSERT INTO step_plugins (
    kind, version, status, label, description, category,
    metadata_schema, frontend_url, owning_team
) VALUES (
    'marketing-attribution', 1, 'active', 'Marketing Attribution',
    'Read-only measurement rollup: opportunities + revenue-influenced + ack rate. Backs tier 5 of marketing-motion.',
    'admin',
    '{"type":"object","properties":{"measurement_days":{"type":"number"},"window_closes_at":{"type":"string","format":"date"},"closing_notes":{"type":"string"}}}',
    'marketing-attribution.js',
    'platform'
) ON CONFLICT (kind, version) DO NOTHING;


-- review-design plugin — tier 0 of the design-doc-review JobKind.
-- Fetches /api/design/docs/{doc_path}, lists open questions parsed
-- by boss-docs-api from `### Qn:` headings, gates step completion
-- on every question having a recorded resolution. Resolutions are
-- mirrored to /api/design/pending-decisions so the flush-jobs path
-- can extract them to ADRs — the system modeling its own development.
INSERT INTO step_plugins (
    kind, version, status, label, description, category,
    metadata_schema, frontend_url, owning_team
) VALUES (
    'review-design', 1, 'active', 'Review Design',
    'Renders open questions parsed from a docs/design/*.md file, gates step completion on every question having a recorded resolution. Backs tier 0 of design-doc-review.',
    'platform',
    '{"type":"object","properties":{"doc_path":{"type":"string"},"resolutions":{"type":"array","items":{"type":"object","properties":{"anchor":{"type":"string"},"decision":{"type":"string"}},"required":["anchor","decision"]}}},"required":["doc_path"]}',
    'review-design.js',
    'platform'
) ON CONFLICT (kind, version) DO NOTHING;

