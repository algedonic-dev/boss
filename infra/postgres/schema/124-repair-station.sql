-- 124-repair-station.sql — the repair queue: trains whose CI went red.
--
-- David, 2026-08-13 (packet bb86d687): "How do these fixes get
-- accounted for? Are we spawning fix-it jobs?" and "Is that because
-- the previous train didn't land? Maybe we need a repair station queue
-- for trains with errors to better see what is going on."
--
-- This answers the second question, and it is a station rather than a
-- project because a station IS registry data — publishing one is a
-- write, and retiring it is a write too. Per the standing directive on
-- protocols (David, 2026-08-14: "introducing and changing protocol is
-- cheap ... edit them to find the right operating cadence"), the way
-- to find out whether a repair queue helps is to publish it and read
-- the counts, not to decide in advance.
--
-- WHAT IT HOLDS. A red train is a `pr-train` whose `ci` step recorded
-- `result = failing` — the same fact the yard's lamp reads (ciLamp in
-- apps/web/src/it/yard/yard.ts), which is the conductor's own verdict
-- and the only place redness is written. The predicate reads it there
-- rather than having the conductor stamp a second copy on the Job,
-- which would be the duplication CLAUDE.md 9a exists to stop. Reading
-- step metadata from a station predicate is new in this car
-- (StepMatch.metadata_equals); it needed the clause and did not need a
-- new fact.
--
-- WHAT IT DOES NOT DO, and this is the honest half of the answer. It
-- makes red trains VISIBLE as a queue; it does not make their repairs
-- ACCOUNTED, which is what David actually asked first. Commits that
-- repair a red train still land straight on the train branch with no
-- packet behind them, so the work is real and the record is silent.
-- Fixing that means the repair itself becomes a packet — a decision
-- about the ship-a-change protocol, not a station row, and left open
-- on bb86d687 deliberately rather than quietly folded in here.
--
-- Discipline is `age` alone, not the usual `priority, age`. Every
-- train carries the same priority, so ordering by it first would sort
-- by nothing and then by age; saying `age` says what actually happens.
-- The oldest red train is the one blocking the most cars.
--
-- No wip_limit. A WIP limit advises an operator to stop pulling work,
-- and nobody chooses to pull a red train — the queue being deep is the
-- signal, and an advisory number on top of it would add no
-- information.
INSERT INTO stations (name, version, status, title, kind, predicate, discipline, wip_limit, capability, rollup_parent) VALUES
  ('repair', 1, 'active', 'Repair — trains whose CI went red', 'batch',
   '{"kind": "pr-train", "status": "open", "step": {"slug": "ci", "metadata_equals": {"result": "failing"}}}'::jsonb,
   '["age"]'::jsonb, NULL, NULL, NULL)
ON CONFLICT (name, version) DO NOTHING;

-- The walk upstream from a red train is the yard: that is where the
-- consist, the lamps and the arrival board are, and a red train is
-- diagnosed by looking at what it is carrying.
UPDATE stations
   SET upstream = '{"label": "THE YARD", "href": "/system/yard"}'::jsonb
 WHERE name = 'repair'
   AND version = 1;
