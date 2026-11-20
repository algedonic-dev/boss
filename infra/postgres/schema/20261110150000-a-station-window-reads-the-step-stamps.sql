-- A station's flow is counted over the step record, not replayed
-- (design 7b8ed079, "Station membership is read, not replayed", car 1;
-- backlog 95a9eca2).
--
-- The yard's flow figures counted packet-membership crossings from a
-- replay of the audit log, and that projection has been stale since
-- 2026-10-30 (urgent b3572a26). They now count steps: an arrival is a
-- step whose ready flip (`became_ready_at`) falls in the window, a
-- departure one whose completion (`completed_at`) does. The read
-- (`station_window_packets` in crates/core/boss-jobs/src/postgres.rs)
-- asks for exactly `became_ready_at >= $1 OR completed_at >= $1`, and
-- neither column had an index, so every map read walked the whole steps
-- table. Partial over the rows that carry a stamp, which every step
-- written since the stamps landed does, and nothing older needs.
--
-- Schema only, ordinary transactional CREATE INDEX; never CONCURRENTLY
-- on the boot path (78625ebd).
CREATE INDEX IF NOT EXISTS steps_became_ready_at
    ON steps (became_ready_at)
    WHERE became_ready_at IS NOT NULL;
CREATE INDEX IF NOT EXISTS steps_completed_at
    ON steps (completed_at)
    WHERE completed_at IS NOT NULL;
