-- 115-event-kinds-workflow-quarantine.sql — the boot-time quarantine
-- marker joins the event_kinds registry.
--
-- Boot used to refuse to start against any active Workflow that
-- failed the viability lint, which turned one bad registry row into a
-- whole-service outage (2026-08-13: a `protocol-retro` row with no
-- terminal published cleanly, lay latent, and killed the API on the
-- next pod roll). Boot now retires the offending row and continues.
--
-- The retirement itself records `jobs.kind.retired` (112) through the
-- registry's own path; this marker is the loud one — it carries the
-- lint problems that condemned the row, so the log answers "why did
-- this kind disappear?" without a re-lint. No ref-check rules: the
-- payload names a workflows row, not a projection row.
INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  ('jobs.kind.quarantined', 'jobs', 'Boot retired an active Workflow that failed the viability lint, and continued starting', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;
