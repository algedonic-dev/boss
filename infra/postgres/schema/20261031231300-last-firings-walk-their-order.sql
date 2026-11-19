-- migrate: no-transaction
--
-- 7e12792b car 1: the newest firing per rule orders tied timestamps by
-- firing_id in byte order. The old two-key recency index leaves that
-- ordering to a sort over retained history. Match the complete reader
-- order, while excluding dead-letter rows that this read never returns.
--
-- The dispatcher writes continuously. As with the original-group and
-- window-cut indexes, each CONCURRENTLY statement runs outside a transaction.
-- A failed build is unrecorded, so retry removes its possibly INVALID index.
-- Bound lock acquisition in this migration session, not database/role policy.
SET lock_timeout = '5s';
DROP INDEX CONCURRENTLY IF EXISTS dispatcher_firings_newest_fired;
CREATE INDEX CONCURRENTLY dispatcher_firings_newest_fired
    ON dispatcher_firings (rule_name, fired_at DESC, firing_id COLLATE "C" DESC)
    WHERE outcome = 'fired';
RESET lock_timeout;
