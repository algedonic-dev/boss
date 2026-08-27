-- 149-publish-to-github-guard.sql — one mirror, one open publish packet.
--
-- DEFECT 9f0c566a, measured 2026-08-18: `publish-to-github-daily` (129)
-- asked no question before spawning, so while packet ab13f05f (08-17)
-- sat at its approval sign-off — unassigned, therefore notifying nobody
-- (13128a0c) — the next morning minted f4e9cdf6, identical title, same
-- subject. Same class as e74b32a1 / migration 148, on a SCHEDULED rule.
--
-- The guard is `NOT open_publish_exists("github-mirror")`, keyed on the
-- packet's declared SUBJECT — there is one mirror, so one open publish
-- packet is the invariant, and no metadata breadcrumb is needed.
--
-- WHAT MAKES THIS EXPRESSIBLE AT ALL: schedule rules now evaluate
-- `when` with the same helper resolver as event rules (this rule's car
-- carries that engine change). Before it, a `when` on a schedule rule
-- was parsed, stored, and silently ignored — so this migration must
-- land WITH that car, never alone: against an older binary the guard
-- would sit inert and the rule would keep double-spawning.
--
-- RETIRE v1 BEFORE INSERTING v2 — dispatcher_rules_one_active_per_name
-- is a plain partial unique index, per-statement; see 148 and the
-- invariant a-registry-version-bump-retires-before-it-inserts.
UPDATE dispatcher_rules
   SET status = 'retired'
 WHERE name = 'publish-to-github-daily'
   AND version = 1;

-- v2: same spawn, now guarded. Append-only — v1's row stays.
INSERT INTO dispatcher_rules (name, version, status, on_event, when_expr, do_steps, delay, schedule_cadence, schedule_anchor, schedule_calendar) VALUES
  ('publish-to-github-daily', 2, 'active', NULL,
   'NOT open_publish_exists("github-mirror")',
   '[{"handler":"jobs.spawn","args":{"kind":"\"publish-to-github\"","subject_kind":"\"custom\"","subject":"\"github-mirror\"","title":"\"Publish to the public GitHub mirror\"","metadata.target":"\"origin/main\"","metadata.area":"\"platform\""}}]'::jsonb,
   NULL, 'daily', DATE '2026-08-15', NULL)
ON CONFLICT (name, version) DO NOTHING;
