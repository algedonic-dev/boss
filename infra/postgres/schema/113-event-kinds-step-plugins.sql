-- 113-event-kinds-step-plugins.sql — the step-plugin registry's
-- writes join the log (protocol-policy-publish.md, Constraints:
-- under 3P a protocol edit IS a network configuration change, so
-- every registry write records an outbox event in the same
-- transaction as the row). Same treatment 112 gave the workflow
-- registry; step_plugins was the remaining silent write path.
--
-- All three kinds are new: unlike the workflows table, no step-
-- plugin write ever emitted anything before, so draft_saved,
-- published, and retired all land here. No ref-check rules: the
-- payload is the StepPluginSpec itself, not a reference to a
-- projection row.
INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  ('jobs.step_plugin.draft_saved', 'jobs', 'A draft StepPlugin version was appended to the registry (author saved, not live)', NULL),
  ('jobs.step_plugin.published',   'jobs', 'A StepPlugin version went live: the latest draft flipped to active, retiring any prior active row', NULL),
  ('jobs.step_plugin.retired',     'jobs', 'The active StepPlugin version of a kind was retired with no successor', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;
