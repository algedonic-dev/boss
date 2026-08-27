-- 140-one-queued-flush-per-doc.sql — a doc can have at most one flush
-- waiting to run.
--
-- Origin: David finished the design-docs-as-data review on 2026-08-15
-- and could not see it anywhere. All eight answers HAD been recorded —
-- eight POSTs, eight 200s — but they had been swept into THREE
-- identical flush jobs, and `pending_count` (which the page reads) was
-- therefore 0. From his side a completed review looked like a review
-- that never registered.
--
-- WHY THREE. `post_flush_job` reads the pending rows OUTSIDE the
-- transaction and hands them to `create_flush_job` as a payload. The
-- `design-decision-flush-queue` rule fires once per recorded decision,
-- so eight near-simultaneous firings each read the same eight rows,
-- each opened a transaction, each saw rows still present, and each
-- inserted a job. Only the first DELETE removed anything. The handler's
-- own comment says a burst "settles clean" because later firings get a
-- 400 — true only if the earlier one has already committed, which under
-- a burst it has not.
--
-- The defensive read cannot fix this: any check inside one transaction
-- is blind to a sibling that has not committed. That is what a
-- constraint is for, and the registry already has the idiom —
-- `stations_one_active_per_name`, a partial unique index over the one
-- status that must be singular.
--
-- Scoped to `queued` deliberately. A doc accumulates any number of
-- succeeded and failed jobs over its life and that history is worth
-- keeping; what must never be true is two flushes waiting to write the
-- same file. The loser of the race gets a unique violation, which the
-- API turns into the same 400 the handler already treats as a no-op —
-- so the burst settles clean for real this time, by construction rather
-- than by timing.

CREATE UNIQUE INDEX IF NOT EXISTS design_flush_jobs_one_queued_per_doc
    ON design_flush_jobs (doc_path)
 WHERE status = 'queued';

COMMENT ON INDEX design_flush_jobs_one_queued_per_doc IS
  'At most one QUEUED flush per doc. Succeeded and failed jobs are '
  'history and unconstrained. The second writer in a burst loses here '
  'and its caller reads the violation as "already queued", which is '
  'the truth and a no-op.';
