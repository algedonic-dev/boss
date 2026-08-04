-- =========================================================================
-- 09-clock.sql — Sim clock — drives the live brewery loop.
-- =========================================================================



-- =========================================================================
-- Sim clock — drives the live brewery loop (`boss-brewery-sim` daemon).
-- See docs/design/projection-rebuilders.md §G.
-- =========================================================================

-- Sim-time is a pure function of wall-time: `wall_anchor` +
-- `warp_factor` derive `sim_now` (see the formula-clock parameters
-- below). State persists so a daemon restart resumes from the same
-- anchor instead of replaying. Single-row table by convention; the
-- daemon UPSERTs id=1.
CREATE TABLE IF NOT EXISTS sim_clock (
    id                    INTEGER PRIMARY KEY DEFAULT 1
                          CHECK (id = 1),
    -- True while the clean-reset path (audit_log trim +
    -- boss-rebuild-all + clock-rewind) is mid-flight. Operators
    -- and the SimClockBadge poll this to render an in-progress
    -- spinner; the daemon ignores ticks while this is true. The
    -- background tokio task in boss-jobs-api flips this back to
    -- false on success or failure.
    restart_in_progress   BOOLEAN NOT NULL DEFAULT false,
    -- The MAX(audit_log.id) right after the canonical seed
    -- bundle was imported. The demo-loop restart endpoint
    -- DELETEs `audit_log` rows beyond this id (the live-tick
    -- accumulation), then rebuilds projections from the
    -- surviving rows — much faster than re-importing the
    -- 880k-event seed bundle (which uses single-row INSERTs).
    -- NULL on fresh DBs that haven't completed a seed import
    -- yet; in that case the restart endpoint errors out with
    -- a clear message.
    epoch_baseline_audit_id BIGINT,
    -- ----- Baseline provenance -----
    -- The baseline is a BUILD ARTIFACT with an age, and the demo
    -- replays it forever: every restart-epoch trims back to
    -- epoch_baseline_audit_id, so nothing merged after the cut —
    -- seed values, JobKind specs, Class rows — exists in the running
    -- tenant until someone re-cuts. That is a deliberate pin, not a
    -- bug, but it is only safe if the pin is legible.
    --
    -- It was not: a playground baseline cut 2026-07-11 replayed for
    -- 3.5 weeks while 6 seed files and 12 schema files moved under
    -- it, and a packaging fix merged two days after the cut was read
    -- back off the running demo as an open modeling gap. Answering
    -- "what is this demo built from, and how old is it?" required
    -- archaeology on audit_log.created_at.
    --
    -- Stamped together with epoch_baseline_audit_id by
    -- infra/seed-brewery-tenant.sh; surfaced at GET /api/clock/baseline.
    -- NULL on installs that predate this column or seed without git
    -- (docker images ship no .git) — absent provenance reads as
    -- "unknown", never as "current".
    baseline_cut_at       TIMESTAMPTZ,
    baseline_source_ref   TEXT,
    -- ----- Formula clock parameters -----
    -- Sim-time is derived by clock-api as:
    --   sim_now = epoch_start_date + (wall_now − wall_anchor − paused_offset) × warp_factor
    -- and capped at epoch_end_date. Only clock-api writes these
    -- fields; every other service reads via /api/clock/now.
    epoch_start_date      DATE NOT NULL,
    epoch_end_date        DATE,
    -- Sim-seconds per wall-second. 1000 = 1 sim-day per ~86
    -- wall-seconds (brewery playground default — kept modest so the
    -- live sim doesn't outrun less powerful machines). 1 = real-time.
    -- Backtests use very large values.
    warp_factor           DOUBLE PRECISION NOT NULL DEFAULT 1000.0,
    -- Wall-clock instant the formula's elapsed-counter was
    -- last reset (boot, /configure, /restart-epoch).
    wall_anchor           TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    paused                BOOLEAN NOT NULL DEFAULT false,
    -- Wall instant the current pause started. NULL if not paused.
    paused_at             TIMESTAMPTZ,
    -- Total wall-seconds of accumulated pause time. Subtracted
    -- from wall-elapsed so pause-then-resume doesn't teleport.
    paused_offset_seconds DOUBLE PRECISION NOT NULL DEFAULT 0.0,
    updated_at            TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

