-- 123-cadence-registry-reconcile.sql — make the system of record tell
-- the truth about the train's boarding threshold.
--
-- THE DIVERGENCE, measured 2026-08-14 ~17:10Z. The cadence loop is the
-- pipeline's sole scheduler, and it reads `cadence_rules` from
-- boss-gcp's LOCAL Postgres — the legacy instance — because the
-- 2026-08-13 split-brain fix (incident c4b4a6b0) redirected the train
-- units' JOB writes with BOSS_JOBS_URL and left the registry
-- connection alone. So the packets moved to the cluster and the
-- protocol data did not:
--
--   boss-gcp local (what actually runs)   cluster (the system of record)
--   ------------------------------------  ------------------------------
--   board v1 retired, min_dock_depth 4    board v1 ACTIVE, min_dock_depth 4
--   board v2 ACTIVE,  min_dock_depth 8    (no v2 at all)
--   244 rows in cadence_firings           0 rows in cadence_firings
--
-- The live threshold is 8; the SoR says 4. That is not a harmless
-- copy: on 2026-08-14 an agent read the cluster, concluded a dock of
-- four cars would trip the threshold, and reported to David that the
-- train would board immediately. It did not — it waited for the 18:00Z
-- clock window. The system of record answered "why has the train not
-- boarded" confidently and wrongly, which is the same failure the
-- split-brain incident was about.
--
-- HOW THE DRIFT HAPPENED, and why it is the class d0092947 named.
-- `train-board-on-dock-depth` v2 was created live on the running
-- instance at 2026-08-13 15:37 UTC — a runtime protocol edit, exactly
-- the thing the registry exists to make cheap. Nothing carried it into
-- a migration, so nothing carried it to the cluster, and nothing
-- detected the gap. d0092947 states the constraint in the abstract:
-- "a rule added at runtime drifts from rules.toml with nothing to
-- detect it". This is that, one table over, and it stayed invisible
-- for a day.
--
-- WHAT THIS MIGRATION DOES, AND DELIBERATELY DOES NOT DO. It
-- reconciles the cluster rows to what runs. It does NOT repoint the
-- conductor at the cluster database — that is the actual fix, it
-- changes a pipeline with cars parked in it, and it must land AFTER
-- this: flipping the connection while the cluster still said 4 would
-- silently halve the boarding threshold. Filed for David as the second
-- half of c7775816.
--
-- Nothing reads these cluster rows today (the cluster pod runs no
-- cadence loop of its own — verified, so there is no second scheduler
-- and no double-firing risk). This migration therefore cannot change
-- pipeline behaviour; it can only stop the SoR from lying.

-- Retire v1. The partial unique index allows exactly one active row
-- per name, so this must precede the v2 insert.
UPDATE cadence_rules
   SET status = 'retired'
 WHERE name = 'train-board-on-dock-depth'
   AND version = 1
   AND status <> 'retired';

-- v2 as it exists on the instance that actually schedules: dock
-- pressure of eight parked-ready cars, at most once every two hours.
-- Raised from four on 2026-08-13 after four-car boards proved too
-- eager against a consist that still costs 20-40 minutes of CI.
INSERT INTO cadence_rules
    (name, version, status, verb, basis, every_minutes, at_times, min_dock_depth, cooldown_minutes)
VALUES
    ('train-board-on-dock-depth', 2, 'active', 'board', 'queue-depth', NULL, NULL, 8, 120)
ON CONFLICT (name, version) DO NOTHING;
