-- 20261018001113-an-input-classification-is-declared.sql — the
-- execution-input classification door's event is declared.
--
-- Origin: design e72dc7c9, answering backlog 40f0436c. A dispatched
-- START printed every value a packet had ever recorded and asked the
-- worker to read all of it: 1,003,270 bytes on SOURCE 266f06e8, one
-- 836,756-byte line of it an Oct 11 observation. Under the opt-in
-- execution-inputs/v1 contract an operator holding publish on workflow
-- may classify ONE FROZEN VALUE of a packet as a mandatory or available
-- original through POST /api/jobs/{id}/input-classifications (`boss
-- classify`); the door appends one entry to the job's reserved
-- `input_classifications` list and records this event beside the
-- JOB_UPDATED row state, in the same transaction.
--
-- Declared here so the kind does not ride inside a passing
-- audit-integrity run unread (infra/lint/emitted-kinds-are-declared.sh).

INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  ('jobs.job.input_classified', 'jobs', 'An operator holding publish on workflow classified one frozen value of a packet for its executors'' START (POST /api/jobs/{id}/input-classifications, boss classify; design e72dc7c9): the value untouched, one entry added to the job''s append-only input_classifications list. Payload {job_id, index, classification: {job, pointer, sha256, size_bytes, class, purpose, reason, by, at}} with the actor as _actor. Informational: the job''s row state rides the sibling jobs.job.updated, which the rebuild replays', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;
