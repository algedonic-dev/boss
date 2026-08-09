-- 105-job-edges-abort.sql — the dial turns: job links now refuse
-- unresolvable targets.
--
-- Precondition met 2026-08-09: the folklore was cleaned through the
-- jobs API (15 values, prefix → full id; one annotated suffix
-- preserved in backlog_item_note), and the machine writers were
-- audited clean — the conductor writes full ids (boarded_jobs since
-- its first solo board), the dispatcher writes none of these fields.
-- The only prefix writer was hand-authored payloads, which this dial
-- now refuses loudly instead of trusting habit.
--
-- Rollback is the same one-row UPDATE back to 'warn' per edge — the
-- dial subject_edges established.
UPDATE job_edges SET on_missing = 'abort'
 WHERE (source_kind, field_path) IN
       (('ship-a-change', 'backlog_item'),
        ('ship-a-change', 'train'),
        ('pr-train',      'boarded_jobs'));
