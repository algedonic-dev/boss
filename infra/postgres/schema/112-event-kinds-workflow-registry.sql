-- 112-event-kinds-workflow-registry.sql — the workflow registry's
-- writes join the log (protocol-policy-publish.md, Constraints:
-- under 3P a protocol edit IS a network configuration change, so
-- every registry write records an outbox event in the same
-- transaction as the row).
--
-- `jobs.kind.published` already sits in the registry (108-event-
-- kinds.sql, harvested from the live log — the workflow-publish
-- step path emitted it). The two kinds below complete the
-- vocabulary for the writes that were previously silent:
-- create_draft and retire. No ref-check rules: the payload is the
-- WorkflowSpec itself, not a reference to a projection row.
INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  ('jobs.kind.draft_saved', 'jobs', 'A draft Workflow version was appended to the registry (author saved, not live)', NULL),
  ('jobs.kind.retired',     'jobs', 'The active Workflow version of a kind was retired with no successor', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;
