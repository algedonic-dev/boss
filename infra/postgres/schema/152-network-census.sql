-- 152-network-census.sql — the packet-loss census: a cadence rule that
-- writes the network's conservation counts to the log as a measured
-- series.
--
-- Origin: docs/design/packet-loss.md, every question decided by David
-- in review 9fb9904f (2026-08-19). Q1 fixes the invariant — every
-- admitted packet reaches a terminal, AND every non-terminal packet is
-- visible at >= 1 station. Q3 puts the census on a cadence writing its
-- counts to the log, "so loss becomes a measured series rather than a
-- spot check — the same move that turned train timings into the
-- retro's evidence. The lens then reads the series instead of
-- recomputing it."
--
-- REPORT FIRST (Q2, "(a) then (b)"). This rule only measures: no
-- raiser, no threshold, no catch-all orphan station — "a noisy raiser
-- trains people to ignore it", and a catch-all station "converts a
-- visible defect into a tidy queue nobody reads". The raiser comes
-- later, calibrated against the base rate this series accumulates.
-- Destroyed-content detection is out of scope (Q4), named so it is
-- not mistaken for covered.
--
-- WHY A DISPATCHER RULE. A census is the clearest possible instance
-- of the dispatcher's narrowed charter (clock / threshold /
-- matchmaking): nothing has happened, and that is exactly the
-- condition being measured. No Workflow definition can declare "count
-- what is NOT moving once a day", because no packet causes it. The
-- rules.toml ratchet baseline rises by one in the same change, with
-- the timer exemption's reasoning beside it.
--
-- WHAT ONE FIRING PRODUCES: exactly one `jobs.network.census` event,
-- recorded by the jobs service (POST /api/network/census — the census
-- is computed in the `network.census` handler over the jobs API's own
-- read surfaces, and dispatcher handlers own no database). The
-- payload carries headline counts for real packets, the demo
-- tenant's separately (sim_* — 87% of packets are simulated, and a
-- headline that mixed them would measure the demo), an orphan id
-- sample capped at 20 (`orphans_truncated` says when), and the
-- instrument-honesty flags (`station_page_clipped`,
-- `per_actor_stations_unevaluated`). Field-by-field meaning lives in
-- the handler's module doc
-- (crates/orchestrators/boss-dispatcher-handlers/src/handlers/network_census.rs).
--
-- Anchored 2026-08-21, the day after this lands, so the first firing
-- is a full day's truth rather than a partial one. Daily per David's
-- standing cadence instruction; tightening it later is a
-- `schedule_cadence` edit on this registry row, not a deploy.
--
-- The rule body was validated against the running dispatcher's own
-- POST /api/dispatcher/rules/_validate before being written here
-- (the loop 121 established). Same honest limit as there: the
-- validator proves the trigger XOR and that everything parses; the
-- handler name stays an opaque string until dispatch — it is
-- registered in boss_dispatcher.rs in this same change.

-- The event kind joins the registry (108): a census firing is a new
-- vocabulary word, and the drift guard warns on kinds no pattern
-- matches.
INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  ('jobs.network.census', 'jobs', 'One packet-loss census firing: the network''s conservation counts (open/workable/stationed/orphaned, real and sim separated) as a measured series', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;

INSERT INTO dispatcher_rules (name, version, status, on_event, when_expr, do_steps, delay, schedule_cadence, schedule_anchor, schedule_calendar) VALUES
  ('network-census-daily', 1, 'active', NULL, NULL,
   '[{"handler":"network.census","args":{}}]'::jsonb,
   NULL, 'daily', DATE '2026-08-21', NULL)
ON CONFLICT (name, version) DO NOTHING;
