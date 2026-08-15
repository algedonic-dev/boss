-- 133-dock-wip-limit.sql — give the loading dock a WIP limit of 24.
--
-- Origin (David, 2026-08-15): "Why don't we set a 2 full train WIP
-- limit before we fire a problem signal", after asking the sharper
-- question first — "Why are we leaving behind so many issues?"
--
-- The measurement that prompted it, 2026-08-14 17:00 to 2026-08-15
-- 00:30:
--
--   cars created        29
--   cars landed         11
--   cars in flight      19
--   trains run/arrived   5 / 2
--   oldest open car      1 day 23h
--
-- Arrivals ran at nearly 3x departures. A queue fed faster than it
-- drains does not merely grow — everything sitting in it ages, and
-- each aging packet finds its own way to go stale: branches drift,
-- migrations collide on a shared tail line, a red train holds five
-- cars hostage for four hours. Most of the mess that night was not
-- separate bugs. It was one queue nobody was allowed to say was too
-- long.
--
-- WHY 24. Two full trains at the current boarding threshold of 12
-- (131). One train's worth in flight is normal — that is just the
-- pipeline working. Two is the point where a second consist is already
-- waiting on the first, so anything beyond it cannot be in flight; it
-- can only be aging. David picked the number; the reasoning for
-- expressing it in TRAINS rather than cars is that it stays correct
-- when the boarding threshold changes, which it did twice in one day.
--
-- ADVISORY, NOT ENFORCED — deliberately, and the machinery already
-- works this way (`station_queue.rs`: "Never enforced here — lenses
-- warn, telemetry reads it"). A dock that REFUSED a car would push the
-- backpressure onto the author, who would park it somewhere the
-- instrument cannot see. Reporting `over_limit` puts the signal where
-- a decision can act on it: stop generating work and go find out why
-- nothing is landing. That is the algedonic reading — the signal's job
-- is to interrupt, not to block.
--
-- Validated against the running /api/stations/_validate before this
-- file was written (`{"ok":true,"problems":[]}`), which also runs
-- `station_lint`'s guard against a non-positive limit.
-- RETIRE FIRST, THEN INSERT. One active version per station name is the
-- ambiguity the registry exists to prevent, and
-- `stations_one_active_per_name` (116) is a plain partial unique index —
-- enforced per STATEMENT, not deferred to commit. Inserting an active v2
-- while v1 is still active violates it and takes the whole schema load
-- down, so every DB-backed test in the workspace aborts before running
-- and the error names a constraint rather than this file. That is
-- exactly how 130-watchlist-dismiss.sql reddened train 20260815-0420;
-- see `a-registry-version-bump-retires-before-it-inserts` in
-- docs/invariants.toml. The v1 row is still readable inside the
-- transaction after the UPDATE, so the SELECT below still finds it.
UPDATE stations SET status = 'retired'
 WHERE name = 'loading-dock' AND version = 1;

-- Every column is carried forward except the one being changed.
-- `upstream` is named explicitly because omitting it is silent: the new
-- row is valid, the station still works, and the only symptom is that
-- the dock lens loses its FEEDBACK link — a navigational aid that is
-- worse absent than broken, since it disappears exactly when someone is
-- diagnosing. `stations_pg::the_seeded_upstream_pointers_round_trip`
-- caught this on the train; it is pinned there.
INSERT INTO stations (name, version, status, title, kind, predicate, discipline,
                      wip_limit, terminal_window_days, capability, rollup_parent,
                      upstream)
SELECT name, 2, 'active', title, kind, predicate, discipline,
       24, terminal_window_days, capability, rollup_parent,
       upstream
  FROM stations
 WHERE name = 'loading-dock' AND version = 1
ON CONFLICT (name, version) DO NOTHING;
