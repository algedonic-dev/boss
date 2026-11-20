-- b6e1d969, A151558/A154416: search is a disposable projection of
-- subjects, jobs and original audit history, rebuilt by the existing
-- ten-minute chore. It is not the system of record. Logical pg_dump
-- still includes unlogged data on the primary by default; an unclean
-- PostgreSQL shutdown or a standby can instead leave this cache empty.
-- The next successful rebuild restores it; its cadence is not a bound
-- on rebuild duration or continuous reader availability.
--
-- Empty the cache BEFORE changing persistence so conversion cannot
-- rewrite the populated index. Both operations and the migration's
-- ledger entry share migrate.sh's ordinary transaction, whose local
-- five-second lock wait policy also covers a retry. Explicit FOR TABLE
-- publication membership or an unexpected inbound foreign key refuses
-- and rolls back this file, without CASCADE or a changed role/database
-- policy. Broad FOR ALL TABLES / FOR TABLES IN SCHEMA publications do
-- not refuse: this cache ceases to participate when it becomes unlogged.
--
-- A172555: the disposable rebuild holds ACCESS EXCLUSIVE throughout its
-- replay. Cancel that holder before waiting for our own lock, accepting
-- one failed rebuild rather than repeated failed boot-init attempts.
-- Cancellation is a signal, not a transactional effect we can roll back.
-- Other lock modes still refuse under the unchanged five-second bound.
SELECT pg_cancel_backend(holder.pid)
FROM (
    SELECT DISTINCT pid
    FROM pg_locks
    WHERE database = (SELECT oid FROM pg_database WHERE datname = current_database())
      AND relation = 'search_index'::regclass
      AND locktype = 'relation'
      AND mode = 'AccessExclusiveLock'
      AND granted
      AND pid <> pg_backend_pid()
) AS holder;
TRUNCATE search_index;
ALTER TABLE search_index SET UNLOGGED;
