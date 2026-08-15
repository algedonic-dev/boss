-- 134-cadence-window-off-grid.sql — move the boarding window off the
-- wall grid, so it stops losing the conductor's lock to the reconcile.
--
-- THE DEFECT (filed 4ed0e791, measured 2026-08-15). `train-window`
-- fired at 06:00 and 18:00; `train-reconcile` fires every 10 minutes on
-- a grid anchored at midnight. 06:00 and 18:00 are both multiples of
-- ten, so the two rules fired in the SAME tick, every time. The
-- conductor's flock admits one of them and the loser logs "another
-- conductor run holds the lock — leaving" — and the window is the one
-- that leaves, having already claimed its firing row. The window is
-- then spent until the next one, twelve hours later.
--
-- The evidence is unambiguous once you know to look: every clock firing
-- in the journal reads `train-window verb=run rc=0 in 0s`, on
-- 2026-08-13, 08-14 and 08-15 alike, while a real board takes five or
-- six seconds. In the system's entire history the twice-daily window
-- has never boarded a train. Every train that ever departed did so on
-- the queue-depth rule.
--
-- WHY THAT MATTERS MORE THAN IT SOUNDS. 131-board-on-twelve.sql raises
-- the depth threshold to 12 and justifies the extra latency by naming
-- this rule as the backstop — "the train-window clock rule still
-- departs at 06:00 and 18:00 UTC regardless of depth, so nothing waits
-- more than about twelve hours". That backstop did not exist. A dock
-- holding eleven cars boarded nothing, indefinitely, and the rule meant
-- to rescue it was a no-op with a green exit code.
--
-- :05 IS THE WHOLE FIX, AND IT IS DATA. Nothing about the collision is
-- special to 06:00; it is special to "a multiple of the reconcile
-- interval". Five past is off that grid for any interval the reconcile
-- is likely to use (10, 15, 30, 60). This is the property
-- protocol-cadence.md claims for cadence-as-registry-data — the fix is
-- a number in a table, not a deploy.
--
-- THIS IS A RECONCILIATION, NOT THE FIRST WRITE. The rows were changed
-- on boss-gcp's live database on 2026-08-15 with David's go-ahead,
-- because that is the database the cadence loop actually reads and the
-- pipeline could not wait for a train it had no way to board. This
-- migration brings the CLUSTER copy to the same place, so the registry
-- an operator reads and the registry the loop obeys agree again
-- (`protocol-data-agrees-between-record-and-runtime`). Both statements
-- are idempotent, so applying this where the change already exists is a
-- no-op rather than a conflict.
--
-- THE DEEPER FIX IS NOT THIS FILE. David resolved protocol-cadence Q4
-- on 2026-08-15: window packets become ordinary packets claimed through
-- the same CAS the human queues use. A claim that happens where the
-- work happens cannot be spent by a verb that never ran. This migration
-- stops the bleeding; that decision removes the organ.

-- Retire first, then insert: `cadence_rules_one_active_per_name` is a
-- plain partial unique index, enforced per statement, so inserting an
-- active v2 while v1 is still active fails outright. Same class that
-- reddened two trains today (see
-- `a-registry-version-bump-retires-before-it-inserts` in
-- docs/invariants.toml, now checked by
-- infra/lint/registry-bump-retires-first.sh).
UPDATE cadence_rules
   SET status = 'retired'
 WHERE name = 'train-window'
   AND status = 'active'
   AND version = 1;

INSERT INTO cadence_rules (name, version, status, verb, basis, at_times)
VALUES ('train-window', 2, 'active', 'run', 'clock', '["06:05", "18:05"]'::jsonb)
ON CONFLICT (name, version) DO NOTHING;
