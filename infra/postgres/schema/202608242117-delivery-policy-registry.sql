-- 202608242117-delivery-policy-registry.sql — the delivery pipeline's
-- POLICY becomes registry data, the way every other protocol in this
-- system already is (docs/design/delivery-as-protocol.md; approved by
-- David 2026-08-24, packet 080ced71).
--
-- WHAT WAS WRONG. `crates/orchestrators/boss-cli/src/train.rs` is 6,852
-- lines holding both the MECHANISM of delivery (merge, push, open,
-- watch, merge, report) and its POLICY (how many strikes hold a car,
-- how long a train may sit before its consist is released, which lints
-- the consist check runs, how much of a failure goes on the record).
-- Because the policy was compiled, every policy question cost a code
-- car — and that car had to ride the pipeline it was repairing. That is
-- how one conductor bug consumed six trains, and why this week read as
-- "patch fix after patch fix".
--
-- WHY A NEW TABLE RATHER THAN cadence_rules ROWS. train.rs line ~120
-- carries our own note that the stall threshold "belongs in the
-- cadence_rules registry". That named the right IDEA (this is protocol
-- data) and the wrong TABLE. `cadence_rules` answers exactly one
-- question — WHEN does a `boss train` verb fire — and every column on it
-- is a firing parameter, with a CHECK that forces exactly one basis
-- parameter group per row. None of the values below is a schedule:
-- `stall_hours` would have to invent a basis that fires nothing, and its
-- rule name would then appear in `cadence_firings`, the exactly-once
-- claim log, never claiming a window. The readers differ too: cadence
-- rows are read by the cadence LOOP deciding whether to spawn a verb;
-- these are read by the CONDUCTOR inside the verb. And pinning needs one
-- version per departing train (below), which a table of independently
-- versioned rules cannot offer.
--
-- ONE ROW IS THE WHOLE POLICY, which is the `workflows` shape rather
-- than the `cadence_rules` shape: a Job pins one workflow version, and a
-- train pins one policy version. The conductor stamps
-- `metadata.delivery_policy_version` on the train Job at boarding and
-- reconcile decides that train's fate against the version it departed
-- under, so editing a row mid-flight cannot rewrite the rules a train
-- left on.
--
-- THIS CHANGES NO BEHAVIOUR. Every value seeded below is the constant
-- that was compiled into train.rs at the moment this was written; the
-- car that carries this file moves where the numbers live and nothing
-- else. `delivery_policy_seed_matches_the_compiled_defaults` (boss-cli,
-- Postgres-backed) is the pin: the seeded row must parse to exactly the
-- conductor's compiled fallback, so the two cannot drift (CLAUDE.md
-- §9a).
--
-- Append-only + versioned like cadence_rules / dispatcher_rules /
-- workflows: a new version supersedes the prior active row (retire it,
-- then insert the new one — the partial unique index is enforced per
-- STATEMENT, see infra/lint/registry-bump-retires-first.sh).

CREATE TABLE IF NOT EXISTS delivery_policy (
    name    TEXT NOT NULL,
    version INT  NOT NULL,
    status  TEXT NOT NULL CHECK (status IN ('draft', 'active', 'retired')),

    -- THE HOLD. How many red trains a car may be released from before
    -- boarding leaves it behind until a human looks. One red is usually
    -- a neighbour's fault; a second aboard a DIFFERENT consist is the
    -- car itself. Without the hold, auto-cancel is a loop: the same
    -- consist re-boards, goes red, and cancels all night.
    max_red_trains INT NOT NULL CHECK (max_red_trains > 0),

    -- THE STALL WINDOW. Hours without a step completion before an open
    -- train counts stalled — the sentinel's stamp, and the auto-cancel
    -- that releases a red or aborted train's consist.
    stall_hours INT NOT NULL CHECK (stall_hours > 0),

    -- THE CONSIST CHECK'S EXCLUSIONS: [{"script": "...", "reason": "..."}].
    -- The roster is the assembled tree's `infra/lint/` directory MINUS
    -- these, so a lint arriving on the train is asked without anyone
    -- editing code. Each exclusion needs something a tree does not
    -- contain (a cargo build, a package manager, a live database), and
    -- a question the tree cannot answer is not one answerable in
    -- seconds. The reason travels with the entry because an unexplained
    -- exemption is how a check quietly stops covering anything.
    consist_excluded_lints JSONB NOT NULL,

    -- THE CONSIST CHECK'S THREE BUDGETS.
    --   secs   — wall clock the whole check may spend before it stops
    --            asking and lets the train go (the measured set costs
    --            ~9s, so this is roughly six times headroom).
    --   output — characters of a failing lint's output that go on the
    --            record: enough to act on, bounded so a chatty check
    --            cannot bloat a Job's metadata.
    --   files  — how many filenames a refusal names. The reason is read
    --            on a chip in the yard; past a handful it stops being a
    --            hint.
    consist_budget_secs   INT NOT NULL CHECK (consist_budget_secs > 0),
    consist_output_budget INT NOT NULL CHECK (consist_output_budget > 0),
    consist_files_named   INT NOT NULL CHECK (consist_files_named > 0),

    -- THE TWO REASON BUDGETS, both character counts.
    --   skip_reason_file_budget — the conflicted-file list on a left
    --            behind car's `metadata.skip_reason`, which PacketCard
    --            renders as a chip; past this the list becomes a count.
    --   blip_cause_budget       — the one-line cause of a jobs-API blip
    --            in the conductor's journal.
    skip_reason_file_budget INT NOT NULL CHECK (skip_reason_file_budget > 0),
    blip_cause_budget       INT NOT NULL CHECK (blip_cause_budget > 0),

    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (name, version)
);

CREATE UNIQUE INDEX IF NOT EXISTS delivery_policy_one_active_per_name
    ON delivery_policy (name) WHERE status = 'active';

-- Seed: today's compiled constants, carried over verbatim as data.
--
--   max_red_trains          2     train.rs MAX_RED_TRAINS
--   stall_hours             6     Config::stall_hours (was BOSS_TRAIN_STALL_HOURS,
--                                 an env knob nothing ever set)
--   consist_excluded_lints  4     train.rs CONSIST_CHECK_EXCLUDED
--   consist_budget_secs     60    train.rs CONSIST_CHECK_BUDGET
--   consist_output_budget   1200  train.rs CONSIST_OUTPUT_BUDGET
--   consist_files_named     6     train.rs CONSIST_FILES_NAMED
--   skip_reason_file_budget 96    train.rs SKIP_REASON_FILE_BUDGET
--   blip_cause_budget       80    train.rs BLIP_CAUSE_BUDGET
INSERT INTO delivery_policy (
    name, version, status,
    max_red_trains, stall_hours,
    consist_excluded_lints,
    consist_budget_secs, consist_output_budget, consist_files_named,
    skip_reason_file_budget, blip_cause_budget
) VALUES (
    'train-conductor', 1, 'active',
    2, 6,
    '[
      {"script": "svelte-check.sh",
       "reason": "runs `bun install --frozen-lockfile` and a typecheck — minutes, plus a network fetch, and it exits 1 outright on a box without bun"},
      {"script": "no-snapshot-arrays.sh",
       "reason": "reads the built `boss-ports-list` binary; with no target/ it can only report ''not found'', and building it is exactly what CI is for"},
      {"script": "conservation-invariants.sh",
       "reason": "psql + curl against a LIVE deployment — an invariant on the running system, which a tree cannot answer (it has its own systemd timer)"},
      {"script": "audit-ordering.sh",
       "reason": "psql against a live database — same: not a question about a tree"}
    ]'::jsonb,
    60, 1200, 6,
    96, 80
)
ON CONFLICT (name, version) DO NOTHING;
