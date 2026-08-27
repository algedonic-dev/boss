-- 130-watchlist-dismiss.sql — let the filer take a packet off their own
-- watchlist.
--
-- Origin (David, 2026-08-14, feedback 9707be05 on `/`):
--   "What happened: Can't remove items from my watchlist
--    What I expected: I would expect to be able to click a star or
--    something to stop watching"
--
-- WHY IT COULD NOT BE DONE BEFORE. `my-watchlist` (118) is an actor
-- station whose membership is `submitted_by = @me` — "this packet names
-- me as its filer". That is a FACT about the packet, not a preference,
-- so there was nothing to turn off. The list was automatic in both
-- directions: you never opted in, and you could not opt out.
--
-- THE FIX IS A PREDICATE CLAUSE, NOT AN ENDPOINT. Adding
-- `metadata_absent: ["watchlist_dismissed"]` means dismissing is
-- writing one key onto the packet, through the job PUT that already
-- exists. No new table, no per-actor join, no bespoke unwatch route.
--
-- A SINGLE FLAG IS CORRECT HERE, and it is worth saying why, because it
-- would be wrong on any other station. Membership is already narrowed
-- to `submitted_by = @me`, so exactly one person can ever see a given
-- packet through this station — its filer. A shared flag cannot leak
-- one actor's dismissal into another's view, because no two actors
-- share a row. A station whose predicate admitted several actors would
-- need per-actor state instead, and this clause would be a bug there.
--
-- Dismissal is not deletion: the packet keeps its `submitted_by`, keeps
-- its place in the log, and keeps closing and notifying as it always
-- did. Only this one reader's list stops showing it. Clearing the key
-- puts it back.
--
-- Validated against the running jobs API's /api/stations/_validate
-- before being written here (`{"ok":true,"problems":[]}`).
-- v1 retires FIRST, in the same transaction. `stations_one_active_per_name`
-- (116) is a plain partial unique index, so it is enforced per statement,
-- not at commit: inserting an active v2 while v1 is still active violates
-- it and takes the whole schema load down. Retire, then insert — the same
-- order 123-cadence-registry-reconcile.sql uses for the same reason. Two
-- active versions of one station name is the ambiguity the registry
-- exists to prevent, and the index is what enforces it.
UPDATE stations SET status = 'retired' WHERE name = 'my-watchlist' AND version = 1;

INSERT INTO stations (name, version, status, title, kind, predicate, discipline,
                      wip_limit, terminal_window_days, capability, rollup_parent) VALUES
  ('my-watchlist', 2, 'active', 'My watchlist — packets I filed', 'actor',
   '{"metadata_equals": {"submitted_by": "@me"}, "metadata_absent": ["watchlist_dismissed"]}'::jsonb,
   '["recency"]'::jsonb, NULL, 14, NULL, NULL)
ON CONFLICT (name, version) DO NOTHING;
