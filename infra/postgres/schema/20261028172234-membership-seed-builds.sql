-- The first membership starting state, built in bounded batches by the
-- owned updater's background (backlog 6bc062b1 fix 3, conductor decision
-- 2026-10-28T17:14Z). A disposable projection cache, never the audit log:
-- every row is a pure function of audit_log at the build's own fixed cut,
-- so deleting either table costs one rebuild and loses no fact.
--
-- One build per caller identity (the membership projection identity key).
-- `plan` is the fixed cut and the sorted packets it covers; `done_through`
-- is the last packet identity whose batch has committed, so a restart
-- resumes after it. The seed is published only once every batch of that
-- plan has committed.
CREATE TABLE membership_seed_builds (
    identity text PRIMARY KEY,
    plan jsonb NOT NULL,
    done_through text,
    entries bigint NOT NULL DEFAULT 0 CHECK (entries >= 0),
    bytes bigint NOT NULL DEFAULT 0 CHECK (bytes >= 0),
    planned_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
-- Per packet and entity, the latest audit id at or below the plan's cut.
CREATE TABLE membership_seed_entries (
    identity text NOT NULL REFERENCES membership_seed_builds(identity) ON DELETE CASCADE,
    packet text NOT NULL,
    entity text NOT NULL,
    audit_id bigint NOT NULL,
    PRIMARY KEY (identity, packet, entity)
);
