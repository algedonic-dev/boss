-- 116-stations.sql — the station registry (stations.md, Q1–Q4
-- ratified 2026-08-13).
--
-- A station is an abstract priority queue that routes or holds
-- job-packet traffic until there is bandwidth or capability to handle
-- the packet. Everything about a station is registry data — never a
-- code path. Membership is DERIVED: `predicate` is evaluated over
-- open Jobs at read time (no mutable current-station field on the
-- packet); motion is read from the event log.
--
-- Append-only + versioned like workflows / step_plugins /
-- dispatcher_rules: a new version supersedes the prior active row
-- (retire it, insert the new one); the partial unique index keeps
-- exactly one 'active' row per name.

CREATE TABLE IF NOT EXISTS stations (
    name          TEXT NOT NULL,
    version       INT  NOT NULL,
    status        TEXT NOT NULL CHECK (status IN ('draft', 'active', 'retired')),
    title         TEXT NOT NULL,
    -- The ratified taxonomy (stations.md): every executor has an
    -- `actor` station; `group` stations are served by a set of actors
    -- (departments, teams); `constraint` stations gate membership by
    -- capability predicates; `batch` stations are the SDLC's bundling
    -- points (loading dock, review queue, board windows).
    kind          TEXT NOT NULL CHECK (kind IN ('actor', 'group', 'constraint', 'batch')),
    -- The queue-membership predicate over packets, evaluated against
    -- open Jobs. Documented JSON shape (boss-jobs
    -- src/station_queue.rs `StationPredicate`): conjunction of
    -- optional clauses —
    --   kind:              exact Workflow kind
    --   status:            job status (kebab-case; the evaluation
    --                      universe is open Jobs, so this only
    --                      narrows further)
    --   tags_any:          at least one of these tags present
    --   metadata_present:  keys that must exist and be non-null
    --   metadata_absent:   keys that must be missing or null
    --   step:              { slug?, kind?, status_in?, assignee_id? }
    --                      — some step matches every given field
    predicate     JSONB NOT NULL,
    -- Queue ordering as data (Q2): an array of discipline keys
    -- applied lexicographically. Vocabulary: "priority" (emergency
    -- first), "age" (oldest opened_on first), "due" (earliest due_on
    -- first, undated last). Default is the ratified `priority, then
    -- age`; ties beyond the declared keys break on job id so the
    -- order is deterministic.
    discipline    JSONB NOT NULL DEFAULT '["priority","age"]'::jsonb,
    -- Optional bandwidth declaration (Q3) — ADVISORY first: the
    -- queue envelope reports over_limit, lenses warn, telemetry
    -- reads it. Enforcement only if the data says it matters.
    wip_limit     INT,
    -- Optional capability gate (Q3), Class-registry vocabulary:
    -- `{"roles": ["head-brewer", ...]}` — who may claim a packet
    -- FROM this station. Checked at the claim CAS when the claim
    -- names its station. NULL = any actor may claim.
    capability    JSONB,
    -- Visual rollup (view-level clutter control, not a data-model
    -- claim): the team/department grouping this station collapses
    -- into. NULL = top-level.
    rollup_parent TEXT,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (name, version)
);

CREATE UNIQUE INDEX IF NOT EXISTS stations_one_active_per_name
    ON stations (name) WHERE status = 'active';


-- The registry's writes join the log — same posture as
-- 112-event-kinds-workflow-registry.sql: every station write records
-- an outbox event in the transaction that writes the row. Payload is
-- the full station spec; no ref-check rules.
INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  ('jobs.station.draft_saved', 'jobs', 'A draft station version was appended to the registry (author saved, not live)', NULL),
  ('jobs.station.published',   'jobs', 'A station version went live, retiring any prior active row', NULL),
  ('jobs.station.retired',     'jobs', 'The active station version was retired with no successor', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;


-- Platform seed: only the SDLC batch stations ship here. Department /
-- team (`group`) and executor (`actor`) stations are TENANT data —
-- departments live in the Class registry (role metadata) and tenant
-- seeds, so their stations spawn as per-tenant rows, not platform
-- SQL. Same posture as dispatcher_rules seeds: plain INSERTs,
-- idempotent on (name, version).
INSERT INTO stations (name, version, status, title, kind, predicate, discipline, wip_limit, capability, rollup_parent) VALUES
  -- The yard's dock, moved from code to data: the ship-a-change cars
  -- parked at review, not yet boarded on a train (the dockRows
  -- predicate in apps/web/src/it/yard/yard.ts).
  ('loading-dock', 1, 'active', 'Loading dock — parked ship-a-change cars', 'batch',
   '{"kind": "ship-a-change", "status": "open", "metadata_present": ["branch"], "metadata_absent": ["train"], "step": {"slug": "review", "status_in": ["ready", "active"]}}'::jsonb,
   '["priority","age"]'::jsonb, NULL, NULL, NULL),
  -- The review queue: design-doc-review Jobs whose review step is
  -- open (design-doc-review spec, boss-jobs registry.rs).
  ('design-review', 1, 'active', 'Design review queue', 'batch',
   '{"kind": "design-doc-review", "status": "open", "step": {"slug": "review", "status_in": ["ready", "active"]}}'::jsonb,
   '["priority","age"]'::jsonb, NULL, NULL, NULL)
ON CONFLICT (name, version) DO NOTHING;
