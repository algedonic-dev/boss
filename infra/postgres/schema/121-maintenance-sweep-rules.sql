-- 121-maintenance-sweep-rules.sql — the maintenance protocol's two
-- dispatcher rules: what admits a sweep, and what a sweep produces.
--
-- Origin (David, 2026-08-14): "We probably need protocol around
-- maintenance of the IT systems", then, on cadence and spawning:
-- "start with daily for the most part until we get evidence that
-- maintenance is overwhelming capacity", and "yes, we should let the
-- protocol spawn remediation jobs. If we find that an actor or
-- protocol is flooding the channel, we can adjust the protocol then."
--
-- WHY THIS EXISTS. Work was being found by breaking things — every car
-- today came from diagnosing a failure. A maintenance protocol
-- GENERATES work on a cadence instead, so the dock fills whether or not
-- anything broke. It also closes a real gap: a clean inspection
-- currently leaves no record at all, so "the disks were fine on the
-- 14th" is somebody's memory rather than a query.
--
-- The `maintenance-sweep` Workflow (published to the registry at
-- runtime, no deploy) forks on what the inspection finds:
--
--   opened -> inspect --+-- action_needed=true  -> remediate -> remediated
--                       +-- action_needed=false -> clear
--
-- Two terminals rather than one, deliberately. `clear` versus
-- `remediated` counts are what tell you whether a cadence is too loose
-- or is wasting capacity — the same shape as the 8-car consist
-- experiment's stopping condition. Collapsing them would make "how
-- often does this actually need attention?" unanswerable.
--
-- BOTH RULES WERE VALIDATED against the running dispatcher's own
-- /api/dispatcher/rules/_validate before being written here, which is
-- the loop the 2026-08-13 outage did without. Note the honest limit of
-- that check: `Rule::from_raw` proves the trigger XOR, the topic
-- pattern and every expression PARSE. It does not resolve handler names
-- (opaque strings until dispatch) and does not prove a `when` field
-- exists on the payload. Which is why rule 2 below reuses the exact
-- field names already proven in production by 117's
-- `complete-feedback-branch-on-car-merged` rather than inventing any.


-- 1. ADMISSION. Daily, per maintenance AREA. David expects several of
-- these — infra, latency report, and others — and some may go intraday
-- later; that is a `schedule_cadence` edit on a registry row, not a
-- deploy, which is the whole point of the protocol being data.
--
-- Starting with one area: disk headroom. It has already cost real time
-- twice, wearing two different disguises on train #22 (once as `ld
-- terminated with signal 7 [Bus error]`, because the linker mmaps its
-- output and ENOSPC surfaces as a bus error rather than as "disk
-- full"), and three machines hit 100% on 2026-08-13. A recurring
-- inspection is exactly the instrument that class of failure wants.
INSERT INTO dispatcher_rules (name, version, status, on_event, when_expr, do_steps, delay, schedule_cadence, schedule_anchor, schedule_calendar) VALUES
  ('maintenance-sweep-disk-daily', 1, 'active', NULL, NULL,
   '[{"handler":"jobs.spawn","args":{"kind":"\"maintenance-sweep\"","subject_kind":"\"custom\"","subject":"\"disk-headroom\"","title":"\"Disk headroom sweep\"","metadata.target":"\"disk-headroom\"","metadata.area":"\"infra\""}}]'::jsonb,
   NULL, 'daily', DATE '2026-08-14', NULL)
ON CONFLICT (name, version) DO NOTHING;


-- 2. WHAT A SWEEP PRODUCES. A sweep that concluded it needed work spawns
-- a `ship-a-change` car carrying a `backlog_item` edge back to the
-- sweep, so the car closes its own sweep on merge through the existing
-- feedback-obligation machinery (117). That is what makes the dock
-- self-filling rather than hand-loaded.
--
-- Fires on the sweep's TERMINAL OUTCOME, not on the `remediate` step's
-- completion. Two reasons. The step-done topic is `step.done.<step
-- kind>`, and `remediate` is a plain `task`, so subscribing there would
-- wake this rule on every task step in every workflow and rely on a
-- `when` to discard almost all of them. And the outcome is the more
-- honest trigger anyway: "this sweep concluded it needed work" is
-- precisely the condition that should produce a car.
--
-- David accepted the flooding risk explicitly: "If we find that an
-- actor or protocol is flooding the channel, we can adjust the protocol
-- then." Retiring or re-versioning this row is a registry write.
INSERT INTO dispatcher_rules (name, version, status, on_event, when_expr, do_steps, delay, schedule_cadence, schedule_anchor, schedule_calendar) VALUES
  ('spawn-car-on-sweep-remediated', 1, 'active', 'jobs.job.closed',
   'kind = "maintenance-sweep" AND outcome = "remediated"',
   '[{"handler":"jobs.spawn","args":{"kind":"\"ship-a-change\"","subject_kind":"\"custom\"","subject":"id","title":"title","metadata.backlog_item":"id"}}]'::jsonb,
   NULL, NULL, NULL, NULL)
ON CONFLICT (name, version) DO NOTHING;
