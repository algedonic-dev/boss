-- 120-event-kinds-station-quarantine.sql — the station boot-quarantine
-- marker joins the event_kinds registry.
--
-- The sibling of 115. `station_lint::gate_active` now gates every
-- publish, so no API path can put an unviable station row into the
-- ACTIVE slot — but the rows seeded by 116/118 and any row written
-- straight to the table never pass through publish at all. The boot
-- pass closes that gap the same way `workflow_quarantine` does.
--
-- Simpler than the Workflow case in one specific way, and it is worth
-- writing down: a station has no refuse-to-start path. Workflow
-- quarantine must refuse when open Jobs are pinned to the offending
-- row, because auto-retiring would strand live work. Station
-- membership is DERIVED — evaluated from the predicate at read time,
-- with no station field on the packet (116) — so nothing is ever
-- pinned to a station version and retiring one strands nothing.
--
-- The retirement itself records `jobs.station.retired` (116) through
-- the registry's own transactional path; this marker is the loud one,
-- carrying the lint problems that condemned the row so the log answers
-- "why did this queue disappear?" without a re-lint. No ref-check
-- rules: the payload names a stations row, not a projection row.
INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  ('jobs.station.quarantined', 'jobs', 'Boot retired an active station that failed the viability lint, and continued starting', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;
