-- 148-dedupe-sweep-spawned-cars.sql — stop a recurring sweep from
-- minting one car per firing for a finding that has not changed.
--
-- DEFECT e74b32a1, measured 2026-08-17. `maintenance-sweep-build-
-- caches-daily` runs every day and `spawn-car-on-sweep-remediated`
-- turns each remediated sweep into a ship-a-change car. Nothing asks
-- whether a car for the same finding is already open, so a condition
-- that persists across days produces a car per day.
--
-- What that looked like on the board: `dcff2c74` (sweep 3241aa67,
-- 08-17) and `5621606f` (sweep 191b9142, 08-16), both titled exactly
-- "Stale build cache sweep", both with no summary, no branch, no body
-- — the title is templated from the sweep kind, so the two rows are
-- identical and the only way to tell them apart is to open each and
-- read its metadata. The agent clearing the board nearly closed one as
-- a duplicate of the other and had to revert the flag.
--
-- THE KEY IS THE SUBJECT, and this is the whole reason the guard could
-- not be written before today. Dedup needs a stable identity for "the
-- same finding", and the close payload carried none:
--
--   `id`     — a fresh uuid every firing.
--   `title`  — templated per target, so it cannot separate a repeat
--              from a different finding either.
--   subject  — `stale-build-caches`. Stable, and exactly the thing.
--
-- So the payload roster gains `subject_id` (all three close emit sites
-- now stamp it, same always-present contract as `kind` and `title` —
-- an ABSENT identifier is a PredicateFailed → Retry → dead-letter
-- storm, not a quiet false). 137 added `title` to this roster for this
-- same rule; this is the next field it needed.
--
-- THE GUARD ITSELF IS NOT NEW. `design-review-spawn` has always
-- carried `NOT open_review_exists(path)`. This rule simply never got
-- the equivalent. `open_car_exists(subject_id)` is that predicate,
-- keyed on the sweep target the car now records in
-- `metadata.sweep_target`.
--
-- NOT DONE HERE: suppressing the sweep. Both sweeps did their job —
-- one of them is where 89b27e60 was diagnosed. The defect is in what
-- happens to their output, not in running them.

UPDATE event_kinds
   SET payload_fields = payload_fields || '[
         {"name": "subject_id", "type": "string", "note": "added 2026-08-18 for spawn-car-on-sweep-remediated v2 dedup (e74b32a1)"}
       ]'::jsonb
 WHERE kind_pattern = 'jobs.job.closed'
   AND NOT payload_fields @> '[{"name": "subject_id"}]'::jsonb;

-- RETIRE v1 BEFORE INSERTING v2. `dispatcher_rules_one_active_per_name`
-- (41-dispatcher.sql) is a UNIQUE index on `name` WHERE status =
-- 'active', so only one version of a rule may be active at a time.
-- Inserting v2 as active while v1 still is violates it and rolls the
-- whole migration back:
--
--   ERROR: duplicate key value violates unique constraint
--          "dispatcher_rules_one_active_per_name"
--   DETAIL: Key (name)=(spawn-car-on-sweep-remediated) already exists.
--
-- Caught by CI on PR 77, which is the one place it could be caught:
-- the failure needs a database with v1 already applied, and the
-- statements are individually valid, so neither `bash -n` nor reading
-- the file finds it. There is no window where the rule is missing —
-- migrate.sh runs the file in ONE transaction, so the retire and the
-- insert commit together.
UPDATE dispatcher_rules
   SET status = 'retired'
 WHERE name = 'spawn-car-on-sweep-remediated'
   AND version = 1;

-- v2: same spawn, now guarded, and recording the key it dedupes on.
-- Append-only — v1's row stays for anything admitted under it.
INSERT INTO dispatcher_rules
    (name, version, status, on_event, when_expr, do_steps,
     delay, schedule_cadence, schedule_anchor, schedule_calendar)
VALUES
  ('spawn-car-on-sweep-remediated', 2, 'active', 'jobs.job.closed',
   'kind = "maintenance-sweep" AND outcome = "remediated" AND NOT open_car_exists(subject_id)',
   '[{"handler":"jobs.spawn","args":{"kind":"\"ship-a-change\"","subject_kind":"\"custom\"","subject":"id","title":"title","metadata.backlog_item":"id","metadata.sweep_target":"subject_id"}}]'::jsonb,
   NULL, NULL, NULL, NULL)
ON CONFLICT (name, version) DO NOTHING;
