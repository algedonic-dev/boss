-- 129-publish-to-github-rule.sql — the daily admission for the
-- `publish-to-github` protocol.
--
-- Origin (David, 2026-08-14): "Can we prep a push to Github? I want to
-- share our work with another project? This can just be a protocol
-- that we run on a daily basis by default but can be invoked ad hoc."
--
-- WHY THIS EXISTS. Shipping moved to the internal forge on 2026-08-12.
-- The public GitHub mirror stopped being fed the same day and nothing
-- announced it, because nothing broke — by 2026-08-14 the mirror was
-- 34 commits and 378 files behind, and that was discovered by someone
-- thinking to ask rather than by any instrument. A daily protocol
-- turns "is the public mirror current?" into a query.
--
-- Most firings should close on the `nothing-to-publish` terminal. That
-- is the point, and it is why the Workflow has that terminal at all:
-- the ratio of `nothing-to-publish` to `published` is what says
-- whether daily is the right cadence. If it is nearly all noise,
-- loosen the cadence — a `schedule_cadence` edit on this row, not a
-- deploy.
--
-- THE PUSH IS NOT AUTOMATED, DELIBERATELY. The protocol measures the
-- drift, runs the secrets gate, and lists the files that would become
-- public for the first time; then it stops at a `sign-off` step. The
-- target repo is public and publishing is not reversible in the way
-- deleting is — a force-push removes a commit from the branch but not
-- from anything that already cloned, forked, or indexed it. So the
-- last step before the network is a human, and `declined` is a
-- first-class terminal rather than a failure.
--
-- VALIDATED against the running dispatcher's own
-- /api/dispatcher/rules/_validate before being written here
-- (`{"error":null,"ok":true}`), which is the loop the 2026-08-13
-- outage did without. Note that the validate endpoint takes the API
-- shape (nested `schedule`, `do`) while this table stores the flat
-- columns below; they are the same rule, spelled two ways.
INSERT INTO dispatcher_rules (name, version, status, on_event, when_expr, do_steps, delay, schedule_cadence, schedule_anchor, schedule_calendar) VALUES
  ('publish-to-github-daily', 1, 'active', NULL, NULL,
   '[{"handler":"jobs.spawn","args":{"kind":"\"publish-to-github\"","subject_kind":"\"custom\"","subject":"\"github-mirror\"","title":"\"Publish to the public GitHub mirror\"","metadata.target":"\"origin/main\"","metadata.area":"\"platform\""}}]'::jsonb,
   NULL, 'daily', DATE '2026-08-15', NULL)
ON CONFLICT (name, version) DO NOTHING;
