-- 119-station-upstream.sql — a station can name the queue that feeds
-- it, so a lens can walk upstream.
--
-- Origin (David, 2026-08-13, feedback 3ccb79f5): "I am realizing that
-- I can't see upstream of the loading dock. I don't think we want to
-- add to this view, but I think this points out a useful navigational
-- aid we will want to add where it makes sense — navigating to the
-- upstream queues when jobs aren't materializing as expected. That is
-- how our actual operators will diagnose the running system too."
--
-- The principle: WHEN A QUEUE IS NOT FILLING AS EXPECTED, THE
-- DIAGNOSIS IS UPSTREAM. A lens that cannot walk upstream forces the
-- operator out of the system to guess. This column is a NAVIGATION
-- affordance, not more content in the queue — nothing here changes
-- what a station holds or how it orders it.
--
-- WHY ONE JSONB COLUMN, NOT TWO NULLABLE TEXT ONES
-- ------------------------------------------------
-- An upstream pointer is ONE fact with two inseparable halves: a label
-- without an href is a dead button, an href without a label is an
-- unlabelled one. Two nullable columns admit four states, two of which
-- are nonsense a reader has to defend against at every call site. One
-- nullable JSONB admits exactly the two states that mean something —
-- declared, or not — which is what `Option<StationUpstream>` says in
-- Rust and what `upstream: {label, href} | null` says in the queue
-- envelope. Same posture as `capability` (a structured optional value)
-- rather than `rollup_parent` (a bare scalar).
--
-- It also leaves room the table does not have to re-migrate for: when
-- evented motion gives the network map real edges, an upstream STATION
-- name joins the object beside the human-facing pair, and that is a
-- shape change in one Rust struct rather than another ALTER.

ALTER TABLE stations
    ADD COLUMN IF NOT EXISTS upstream JSONB;

COMMENT ON COLUMN stations.upstream IS
  'Optional navigation pointer to the queue that FEEDS this station: '
  '{"label": "FEEDBACK", "href": "/system/feedback"}. Rendered by lenses '
  'as a button anchored in the station''s own section — the operator''s '
  'walk upstream when packets are not materializing as expected. NULL = '
  'the station declares no upstream and no button renders. Never a '
  'membership or ordering claim; a station''s queue is its predicate.';


-- The two platform batch stations seeded by 116, filled in.
--
-- UPDATE rather than a new version row: `upstream` is platform-declared
-- navigation on the platform's own seed, in the same category as
-- `title`, and 116's rows are seeds nobody authored. `upstream IS NULL`
-- makes this strictly a fill-in — a deployment that already published
-- its own version with an upstream keeps it. Scoped to the ACTIVE row
-- so the affordance lands whatever version history a cluster has.

-- The loading dock holds parked `ship-a-change` cars, and a car names
-- its motivating packet through the declared `backlog_item` job edge
-- (104-job-edges.sql; 117-feedback-obligation.sql reads it to close
-- the loop). Those packets are `user-feedback` Jobs, and the surface
-- that lists them is the triage board — David: "Since these are
-- primarily user-feedback protocol jobs". So "the dock is emptier than
-- it should be" is answered one click upstream, at triage: is anything
-- sitting un-triaged, or did triage route it somewhere other than
-- `build`?
UPDATE stations
   SET upstream = '{"label": "FEEDBACK", "href": "/system/feedback"}'::jsonb
 WHERE name = 'loading-dock' AND status = 'active' AND upstream IS NULL;

-- The design-review queue holds `design-doc-review` Jobs, which the
-- `design-review-spawn` dispatcher rule opens off `docs.design.indexed`
-- for any doc with open questions and no open review
-- (107-dispatcher-rule-design-review-spawn.sql). The corpus that rule
-- reads is the design-doc index, and /system/design is the only surface
-- that shows it whole: every doc's open-question count, its pending
-- decisions, AND the indexer's REJECTIONS — which is precisely the
-- answer to "why has this doc never produced a review".
UPDATE stations
   SET upstream = '{"label": "DESIGN DOCS", "href": "/system/design"}'::jsonb
 WHERE name = 'design-review' AND status = 'active' AND upstream IS NULL;
