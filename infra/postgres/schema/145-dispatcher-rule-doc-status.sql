-- 145-dispatcher-rule-doc-status.sql — ask, daily, whether any design
-- doc's status line still describes the doc.
--
-- WHY. A `**Status**:` line is hand-written and nothing updates it.
-- Answering a question is a flush, and the flush rewrites the
-- Decision-history section without touching the header — so status
-- drifts stale BY DEFAULT, only ever as fresh as the last time someone
-- happened to edit that line (feedback 0b8ae875, filed as the
-- mechanism behind bedda461 so that fixing the page would not look
-- like fixing the cause).
--
-- The detector already existed and reported to nobody:
-- `GET /api/design/stale-statuses` lists docs claiming live discussion
-- (draft / in-review / reopened) while the tracker holds zero open
-- questions. A check with no caller is not a mechanism — the same
-- defect as check-manifests-applied.sh, and the same fix.
--
-- A REPORT FEEDING A SWEEP, not a rejection at index time.
-- crates-and-layers.md reads "in-review (agent draft; the re-tiering
-- call is David's)" — legitimately waiting on a person with no
-- questions registered. Refusing to index that would be wrong, and a
-- rule that has to be right about intent will be wrong. A sweep puts
-- it in front of somebody who can tell the difference, and the
-- protocol's own fork decides: remediate the header, or clear.
INSERT INTO dispatcher_rules (name, version, status, on_event, when_expr, do_steps, delay, schedule_cadence, schedule_anchor, schedule_calendar) VALUES
  ('maintenance-sweep-doc-status-daily', 1, 'active', NULL, NULL,
   '[{"handler":"jobs.spawn","args":{"kind":"\"maintenance-sweep\"","subject_kind":"\"custom\"","subject":"\"doc-status\"","title":"\"Design doc status sweep\"","metadata.target":"\"doc-status\"","metadata.area":"\"docs\""}}]'::jsonb,
   NULL, 'daily', DATE '2026-08-17', NULL)
ON CONFLICT (name, version) DO NOTHING;
