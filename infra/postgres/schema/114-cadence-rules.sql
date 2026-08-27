-- 114-cadence-rules.sql — the conductor's cadence becomes protocol
-- data (docs/design/protocol-cadence.md; David, 2026-08-12,
-- bacca14e: "We want every protocol internalized so we can measure,
-- experiment, and update").
--
-- The train's scheduling knowledge used to live in two systemd
-- timers on a box — invisible to the log, changeable only with
-- sudo. Each cadence rule now names the `boss train` verb it fires,
-- the basis it fires on, and the basis' parameters; the
-- `boss train cadence` loop (crates/orchestrators/boss-cli/src/
-- cadence.rs) evaluates the active rows against boss-clock time and
-- runs the verb. systemd is demoted to keeping that loop alive.
--
-- Bases (exactly one parameter group per row, enforced by CHECK):
--   wall        — an interval: fire once per `every_minutes` bucket
--                 (buckets anchored at midnight UTC of the current
--                 boss-clock day).
--   clock       — times-of-day: fire once per `at_times` window
--                 (JSONB array of "HH:MM", UTC — the same 06:00/18:00
--                 the retired timer spelled OnCalendar).
--   queue-depth — dock pressure: fire when the count of parked ready
--                 cars (ship-a-change Jobs at review with a branch
--                 pushed, read from the jobs API) reaches
--                 `min_dock_depth`, at most once per
--                 `cooldown_minutes`. This is the measured fix for
--                 bursty arrivals against a twice-daily boarding.
--
-- Append-only + versioned like dispatcher_rules / step_plugins: a
-- new version supersedes the prior active row (retire it, insert the
-- new one); the partial unique index keeps exactly one 'active' row
-- per rule name. Changing the train's cadence is a data change with
-- an audit trail, not a unit-file edit.

CREATE TABLE IF NOT EXISTS cadence_rules (
    name    TEXT NOT NULL,
    version INT  NOT NULL,
    status  TEXT NOT NULL CHECK (status IN ('draft', 'active', 'retired')),
    -- The `boss train` verb this rule fires. The executor spawns the
    -- verb as a child of the same binary; the conductor's flock makes
    -- an overlapping run exit clean.
    verb    TEXT NOT NULL CHECK (verb IN ('preflight', 'reconcile', 'board', 'run')),
    basis   TEXT NOT NULL CHECK (basis IN ('wall', 'clock', 'queue-depth')),
    every_minutes    INT,     -- wall: interval between firings
    at_times         JSONB,   -- clock: ["06:00","18:00"] times-of-day, UTC
    min_dock_depth   INT,     -- queue-depth: parked-ready-car threshold
    cooldown_minutes INT,     -- queue-depth: minimum minutes between firings
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (name, version),
    CHECK (
        (basis = 'wall'
            AND every_minutes IS NOT NULL AND every_minutes > 0
            AND at_times IS NULL AND min_dock_depth IS NULL
            AND cooldown_minutes IS NULL)
     OR (basis = 'clock'
            AND at_times IS NOT NULL
            AND every_minutes IS NULL AND min_dock_depth IS NULL
            AND cooldown_minutes IS NULL)
     OR (basis = 'queue-depth'
            AND min_dock_depth IS NOT NULL AND min_dock_depth > 0
            AND cooldown_minutes IS NOT NULL AND cooldown_minutes > 0
            AND every_minutes IS NULL AND at_times IS NULL)
    )
);

CREATE UNIQUE INDEX IF NOT EXISTS cadence_rules_one_active_per_name
    ON cadence_rules (name) WHERE status = 'active';

-- Every firing is a row, so "how often does the train actually run"
-- is a query, not folklore. The deterministic firing_id
-- (`cadence:<name>:<window-stamp>`, minute resolution) is the
-- exactly-once guard: a restart, a second cadence instance, or a
-- re-evaluated tick all compute the same id for the same window and
-- the primary key dedupes the claim (protocol-cadence Q3). Catch-up
-- after downtime claims at most the single most-recent missed window
-- per rule — no thundering backfill.
CREATE TABLE IF NOT EXISTS cadence_firings (
    firing_id TEXT PRIMARY KEY,
    rule_name TEXT NOT NULL,
    verb      TEXT NOT NULL,
    basis     TEXT NOT NULL,
    -- Boss-clock time at the claim, bound by the executor from
    -- ClockClient — never SQL NOW().
    fired_at  TIMESTAMPTZ NOT NULL,
    -- Evidence: dock depth at a queue-depth firing, then the verb's
    -- exit code + runtime seconds merged in after the run.
    detail    JSONB NOT NULL DEFAULT '{}'::jsonb
);

CREATE INDEX IF NOT EXISTS cadence_firings_rule_recency
    ON cadence_firings (rule_name, fired_at DESC);

-- Seed: the two retired timers, carried over verbatim as data, plus
-- the queue-depth rule the timers could never express.
--   train-reconcile — the 10-minute early-warning reconcile
--                     (was boss-pr-train-reconcile.timer, *:00/10:00).
--   train-window    — the twice-daily window, reconcile + board
--                     (was boss-pr-train.timer, 06:00/18:00 UTC).
--   train-board-on-dock-depth — board when four parked ready cars
--                     are waiting, at most every two hours: a burst
--                     of arrivals no longer waits for the next
--                     wall-clock window, and the 2h cooldown keeps
--                     "never under a moving train" true (a deploy
--                     budget is ~2h).
INSERT INTO cadence_rules
    (name, version, status, verb, basis, every_minutes, at_times, min_dock_depth, cooldown_minutes)
VALUES
    ('train-reconcile',           1, 'active', 'reconcile', 'wall',        10,   NULL,                     NULL, NULL),
    ('train-window',              1, 'active', 'run',       'clock',       NULL, '["06:00","18:00"]'::jsonb, NULL, NULL),
    ('train-board-on-dock-depth', 1, 'active', 'board',     'queue-depth', NULL, NULL,                     4,    120)
ON CONFLICT (name, version) DO NOTHING;
