-- 142-dispatcher-rule-cluster-conformance.sql — ask, daily, whether the
-- cluster still matches infra/cluster/manifests/.
--
-- WHY. The tree describes what should be running and nothing checked
-- that any of it was. boss-dev.yaml was merged on train 36, applied by
-- hand, and a day later a design doc asserted in review that the pod
-- had never run — while it had 25 hours of uptime and a bound volume.
-- Neither the repo nor any check could answer the question; the claim
-- was reasoned from two absences and was wrong.
--
-- check-manifests-applied.sh answers it, and a script nobody runs is
-- not a mechanism. This rule is what makes somebody ask: a
-- maintenance-sweep packet lands daily, an actor runs the check and
-- records findings + measured on the inspect step, and the protocol's
-- own fork decides — remediate when a manifest is absent from the
-- cluster, clear when they agree.
--
-- A SWEEP RATHER THAN A SYSTEMD TIMER, deliberately. A timer that
-- finds drift has nowhere to put the finding: no packet, no findings
-- field, no event log, nobody's queue. David, 2026-08-16: "let's try
-- and get as much maintenance and management into job protocols rather
-- than floating around scripts or system timers elsewhere."
--
-- A NEW migration rather than a regenerated 41-dispatcher.sql: 41 is
-- an applied migration and applied migrations are history
-- (docs/design/schema-migrations.md). gen-seed.py refuses to rewrite
-- it and names this pattern; 101-dispatcher-rule-step-assigned.sql is
-- the worked example. ON CONFLICT DO NOTHING keeps the file
-- re-runnable.
INSERT INTO dispatcher_rules (name, version, status, on_event, when_expr, do_steps, delay, schedule_cadence, schedule_anchor, schedule_calendar) VALUES
  ('maintenance-sweep-cluster-conformance-daily', 1, 'active', NULL, NULL,
   '[{"handler":"jobs.spawn","args":{"kind":"\"maintenance-sweep\"","subject_kind":"\"custom\"","subject":"\"cluster-conformance\"","title":"\"Cluster conformance sweep\"","metadata.target":"\"cluster-conformance\"","metadata.area":"\"cluster\""}}]'::jsonb,
   NULL, 'daily', DATE '2026-08-17', NULL)
ON CONFLICT (name, version) DO NOTHING;
