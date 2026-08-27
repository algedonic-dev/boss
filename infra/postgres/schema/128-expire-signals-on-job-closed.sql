-- 128-expire-signals-on-job-closed.sql — retire the inbox
-- notifications about a job once it closes.
--
-- David, 2026-08-14: "We still need a way to automatically expire
-- inbox messages for jobs that have moved past relevancy."
--
-- A ready-step notification is true when it is sent and stops being
-- true the moment the work behind it is done. Nothing ever retracted
-- them, so the platform admin's inbox reached 2,058 unread —
-- overwhelmingly signals about steps closed days ago. That number was
-- not a backlog, it was sediment, and an inbox nobody can read is a
-- channel that does not exist.
--
-- `jobs.job.closed` is the honest trigger rather than a sweep on a
-- clock: a closed job is exactly the moment its notifications stop
-- being about anything, so the expiry is precise instead of
-- approximately-daily. One firing per close expires across every
-- recipient at once.
--
-- ONLY UNREAD `signal` ROWS MOVE, and they are ARCHIVED, not deleted
-- and not marked read. The messages port carries the full reasoning;
-- the short version is that `read_at` would claim a person read it,
-- which is false, deletion would lose the record, and a `direct` is
-- one person addressing another and does not stop being addressed to
-- them because a job closed.
--
-- Companion to the on-call suppression, which stopped the flood at the
-- source. This one drains what the source already produced.
INSERT INTO dispatcher_rules (name, version, status, on_event, when_expr, do_steps, delay, schedule_cadence, schedule_anchor, schedule_calendar) VALUES
  ('expire-signals-on-job-closed', 1, 'active', 'jobs.job.closed', NULL,
   '[{"handler":"messages.expire_for_job","args":{}}]'::jsonb,
   NULL, NULL, NULL, NULL)
ON CONFLICT (name, version) DO NOTHING;
