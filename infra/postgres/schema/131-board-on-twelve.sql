-- 131-board-on-twelve.sql — raise the boarding threshold from 8 to 12.
--
-- Origin (David, 2026-08-14): "Goal in general is to load up trains as
-- fast as we can and have CI be the blocker. I would love to start
-- having 12 car trains because we are waiting on CI."
--
-- THE REASONING. One train is one CI run, near enough: the pipeline
-- costs roughly the same whether it carries four cars or twelve. So
-- when CI is the bottleneck, the throughput lever is cars-per-train,
-- not trains-per-hour. Boarding at 8 spent a full CI cycle on each
-- half-full consist; boarding at 12 spends the same cycle on half again
-- as much work.
--
-- WHAT THIS COSTS. A car waits longer for a fuller train, so latency
-- per car rises. Two things bound it: the `train-window` clock rule
-- still departs at 06:00 and 18:00 UTC regardless of depth, so nothing
-- waits more than about twelve hours; and the 120-minute cooldown is
-- unchanged, so a dock that fills fast still cannot board more often
-- than before. Deliberately one variable at a time — if 12 turns out to
-- starve the dock, the evidence will be `cadence_firings` rows spaced
-- much further apart than they are today, and the fix is a number in
-- this table rather than a deploy.
--
-- THE SPLIT-BRAIN WARNING, because this is the exact row that caused
-- it. Until the conductor moves onto /api/cadence/* (this car adds the
-- surface; the switch is a follow-up), the loop that actually enforces
-- boarding reads boss-gcp's LOCAL Postgres, NOT the cluster's. This
-- migration only reaches the cluster. Changing the threshold therefore
-- means writing it in BOTH places, and on 2026-08-13 doing only one is
-- precisely what made the registry say 4 while the running loop used 8.
-- Both were set to 12 by hand alongside this file; when the conductor
-- reads the API, this row becomes the only copy and the hazard goes
-- away.
-- Retire-and-reseed, matching 123: a threshold change is a NEW version
-- of the rule, not an edit to the row that was in force. The retired
-- rows are how "what was the threshold when this train boarded?" stays
-- answerable against cadence_firings.
UPDATE cadence_rules
   SET status = 'retired'
 WHERE name = 'train-board-on-dock-depth'
   AND version = 2
   AND status <> 'retired';

INSERT INTO cadence_rules
    (name, version, status, verb, basis, every_minutes, at_times, min_dock_depth, cooldown_minutes)
VALUES
    ('train-board-on-dock-depth', 3, 'active', 'board', 'queue-depth', NULL, NULL, 12, 120)
ON CONFLICT (name, version) DO NOTHING;
