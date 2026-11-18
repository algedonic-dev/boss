-- migrate: no-transaction
--
-- Design c195e83e Q4 (approved 2026-10-17): the membership history read
-- finds a window row's original siblings by `original_group->>'id'`, and
-- nothing indexed that expression, so the sibling arm of the read was a
-- whole-table scan of audit_log (4.1 GB, 2.2M rows on 2026-10-17).
--
-- CONCURRENTLY, because audit_log takes every write the estate makes and
-- a plain CREATE INDEX would hold those writes for the minutes the build
-- takes. That is accepted as a one-time converge cost. CONCURRENTLY also
-- refuses to run inside a transaction block, which is what the marker on
-- the first line tells migrate.sh and TestDb: this file runs alone, each
-- statement in its own transaction, never batched with another file.
--
-- The DROP is the repair for a build that died part-way: a failed
-- CONCURRENTLY build leaves an INVALID index of this name, and this file
-- was not recorded, so the next converge runs it again. On a first run
-- there is nothing to drop. A no-transaction file holds plain statements
-- only (no dollar-quoted bodies), because TestDb runs them one by one.
DROP INDEX CONCURRENTLY IF EXISTS audit_log_original_group_id;
CREATE INDEX CONCURRENTLY audit_log_original_group_id
    ON audit_log ((original_group->>'id'));
