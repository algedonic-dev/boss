-- Derived read projection. No immutable producer fact or registry row changes.
-- Ordinary LOGGED tables retain the sequence-addressed lens across DB restart.
CREATE TABLE IF NOT EXISTS jobs_claim_history_head (
    singleton boolean PRIMARY KEY DEFAULT true CHECK(singleton),
    seq bigint NOT NULL CHECK(seq>=0), at timestamptz NOT NULL,
    observed boolean NOT NULL DEFAULT false,
    generation bigint NOT NULL DEFAULT 0 CHECK(generation>=0),
    pruned_through bigint NOT NULL DEFAULT 0 CHECK(pruned_through>=0),
    source_head bigint, source_at timestamptz, source_failed_at timestamptz,
    CHECK((source_head IS NULL)=(source_at IS NULL))
);
CREATE TABLE IF NOT EXISTS jobs_claim_history_cuts (
    generation bigint NOT NULL, seq bigint CHECK(seq>=0), at timestamptz NOT NULL,
    PRIMARY KEY(generation,seq)
);
CREATE TABLE IF NOT EXISTS jobs_claim_history_facts (
    generation bigint NOT NULL, seq bigint CHECK(seq>0), event_id uuid NOT NULL,
    fingerprint text NOT NULL, at timestamptz NOT NULL,
    PRIMARY KEY(generation,seq), UNIQUE(generation,event_id)
);
CREATE TABLE IF NOT EXISTS jobs_claim_history_groups (
    generation bigint NOT NULL, id uuid NOT NULL, lo bigint NOT NULL, hi bigint NOT NULL,
    evidence text NOT NULL, CHECK(lo>0 AND hi>=lo), PRIMARY KEY(generation,id)
);
CREATE TABLE IF NOT EXISTS jobs_claim_history_rows (
    generation bigint NOT NULL, kind text NOT NULL, key text NOT NULL, seq bigint NOT NULL,
    at timestamptz NOT NULL, body jsonb NOT NULL,
    PRIMARY KEY(generation,kind,key,seq)
);
CREATE INDEX IF NOT EXISTS jobs_claim_history_rows_latest
    ON jobs_claim_history_rows(generation,kind,key,seq DESC);
CREATE INDEX IF NOT EXISTS jobs_claim_history_rows_window
    ON jobs_claim_history_rows(generation,kind,at,seq);
CREATE TABLE IF NOT EXISTS jobs_claim_history_current (
    generation bigint NOT NULL, kind text NOT NULL, key text NOT NULL, seq bigint NOT NULL,
    at timestamptz NOT NULL, body jsonb NOT NULL,
    PRIMARY KEY(generation,kind,key)
);
CREATE INDEX IF NOT EXISTS jobs_claim_history_current_steps
    ON jobs_claim_history_current(generation,(body->>'status'),key) WHERE kind='step';
CREATE INDEX IF NOT EXISTS jobs_claim_history_current_step_jobs
    ON jobs_claim_history_current(generation,(body->>'job_id')) WHERE kind='step';
