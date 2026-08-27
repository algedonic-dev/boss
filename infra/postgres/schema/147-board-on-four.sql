-- Boarding threshold back to 4, where it started.
--
-- WHY. On 2026-08-17 the dock held three cars, gates green and branches
-- published, and no train boarded them. Four consecutive trains opened
-- and closed `outcome=cancelled` with terminal "Cancelled — nothing to
-- board" and zero cars, while three mergeable cars sat in the dock. The
-- depth rule had not fired since 2026-08-16 14:39.
--
-- Nothing was broken. `train-board-on-dock-depth` v3 requires depth 12
-- and the dock has not exceeded 3-5 in normal operation, so the rule
-- could not fire. A dry-run boarding, with the conductor's own
-- environment, boarded all three on the first try:
--
--   conductor: DRY: candidates: [('add47b6a', 'feat/estate-subjects'),
--                                ('24f5dd44', 'feat/presence-assurance'),
--                                ('87fe1901', 'feat/dev-pod-clones')]
--
-- The 06:05 window was ALSO correct to board nothing: those cars' gate
-- steps did not complete until later that day, so at 06:05 there was
-- genuinely nothing ready. The two triggers were each behaving; between
-- them they left a hole. A car that turns green just after a window has
-- no path onto a train until the next window, because the only other
-- trigger needs a dock four times deeper than this project produces.
--
-- Threshold history is 4 (114) -> 8 (123) -> 12 (131), each raise made
-- when a train was expensive. A train is cheaper now — the forge runner
-- cycles build-image, locomotive, web and fast in about three minutes
-- — so the ratchet is going back the way it came. `cooldown_minutes`
-- stays 120, which is what actually protects the single-concurrency
-- runner: at most one depth-triggered train every two hours regardless
-- of how fast cars arrive.
--
-- David chose 4 over 3 deliberately, accepting the consequence: a dock
-- of three still waits for the 06:05/18:05 window. This bounds train
-- frequency rather than latency. Bounding LATENCY needs a trigger that
-- fires on how long a car has been parked rather than on how many are
-- parked — a new `basis`, which is Rust rather than a row, and is not
-- what this change is.
--
-- THIS MIGRATION IS HALF THE CHANGE, as 131 warned in the same words:
-- the loop that actually enforces boarding reads boss-gcp's LOCAL
-- Postgres, not the cluster's, so this row is documentation until the
-- conductor moves onto /api/cadence/*. The local row was set to 4 by
-- hand alongside this file and both were verified. On 2026-08-13 doing
-- only one side is precisely what made the registry say 4 while the
-- running loop used 8.
--
-- Retire-and-reseed, matching 123 and 131: a threshold change is a NEW
-- version of the rule, never an edit to the row that was in force, so
-- "what was the threshold when this train boarded?" stays answerable
-- against cadence_firings. Retire BEFORE insert —
-- cadence_rules_one_active_per_name is a plain partial unique index,
-- enforced per statement, not deferred to commit
-- (infra/lint/registry-bump-retires-first.sh).

UPDATE cadence_rules
   SET status = 'retired'
 WHERE name = 'train-board-on-dock-depth'
   AND version = 3
   AND status <> 'retired';

INSERT INTO cadence_rules
    (name, version, status, verb, basis, every_minutes, at_times, min_dock_depth, cooldown_minutes)
VALUES
    ('train-board-on-dock-depth', 4, 'active', 'board', 'queue-depth', NULL, NULL, 4, 120)
ON CONFLICT (name, version) DO NOTHING;
