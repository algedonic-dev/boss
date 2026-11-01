-- migrate: no-transaction
--
-- 6bc062b1, conductor decision 2026-10-27: the existing window-cut
-- insertion-order check filters created_at and id. The created_at-only
-- index requires heap reads for id; keep the SQL and invariant unchanged
-- and carry both keys in one index instead.
--
-- audit_log takes every write, so build without blocking those writes.
-- As with the original-group index, retry an interrupted concurrent build
-- by dropping its possibly INVALID remnant before recreating it. These
-- statements run separately under migrate.sh and TestDb's marker above.
DROP INDEX CONCURRENTLY IF EXISTS audit_log_created_at_id;
CREATE INDEX CONCURRENTLY audit_log_created_at_id
    ON audit_log (created_at, id);
