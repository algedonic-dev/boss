-- Backlog 635bbef3: GET /api/claims refused 0 of 6 at 19:15Z on
-- 2026-11-09 ('outer deadline exhausted; active_query=rows_bound
-- elapsed_ms=1930', every earlier phase 25 ms or less), on a build that
-- changed nothing in the read since car 8d8c141d was proven at 18:02Z.
-- The head read's live steps were found through
-- jobs_claim_history_current_steps, which indexes EVERY step by status:
-- the arm walked all READY steps — the open queue, 1,341 of them across
-- 1,378 open jobs that day — and fetched each from the heap to learn it
-- waited on nothing, and its acquisition arm read every retained
-- acquisition to test the same way. Both grow with the queue and the
-- week, not with the claims, and cost a random heap read each whenever
-- the pages are cold.
--
-- These partial indexes hold exactly the rows each arm answers, so their
-- predicates are written as the read writes them (CURRENT_ROWS in
-- crates/core/boss-jobs/src/claims_console/postgres.rs); the plan test
-- the_head_read_reads_the_live_claims_not_the_retained_projection fails
-- if the two drift, because the planner then falls back to a full walk.
-- The status index served only that arm, and every step write paid for
-- it, so it goes. Schema only, ordinary transactional CREATE INDEX;
-- never CONCURRENTLY on the boot path (78625ebd).
CREATE INDEX IF NOT EXISTS jobs_claim_history_current_live_steps
    ON jobs_claim_history_current(generation,key)
    WHERE kind='step' AND (body->>'status'='active' OR (body->>'status'='ready' AND body->'waiting_on'<>'null'::jsonb));
CREATE INDEX IF NOT EXISTS jobs_claim_history_current_live_acquisitions
    ON jobs_claim_history_current(generation,key)
    WHERE kind='acquisition' AND (body->'active'='true'::jsonb OR jsonb_path_exists(body, '$.stock_obligations[*] ? (@.returned_by == null)'));
DROP INDEX IF EXISTS jobs_claim_history_current_steps;
