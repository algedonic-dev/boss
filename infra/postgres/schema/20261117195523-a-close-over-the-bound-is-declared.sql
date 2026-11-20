-- 20261117195523-a-close-over-the-bound-is-declared.sql — the bounded
-- close's event is declared.
--
-- Origin: backlog b96df4f8. Every Job write stages its whole row as one
-- jobs.job.updated, and the outbox refuses any event over 983,040
-- bytes. Work-session 044b189b grew past that bound as a mailbox (~984
-- KB, 762 keys) before the packet budget existed, and from 2026-11-03
-- it stood open with every step terminal: its close re-emitted the
-- whole row and was refused, as was every write that could have
-- repaired it. A close whose whole-row event is over the bound now
-- stages this event instead, in the same transaction as the row.
--
-- Declared here so the kind does not ride inside a passing
-- audit-integrity run unread (infra/lint/emitted-kinds-are-declared.sh).

INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  ('jobs.job.closed_over_bound', 'jobs', 'A packet whose whole row is over the outbox event bound was closed (backlog b96df4f8). A state event carrying a delta, not the row: payload {id, status, closed_on, merged, row_sha256, row_bytes, whole_row_event_bytes, bound} with the actor as _actor, where merged is the metadata keys the close owns and row_sha256 is the sha256 of the post-close row as its jobs.job.updated would have carried it. The rebuild applies the delta to the row earlier state events built and refuses to continue unless the result hashes to row_sha256', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;
