-- 144-estate-subjects.sql — BOSS's own substrate, as Subjects.
--
-- WHY. BOSS models the brewery's physical world — locations, assets,
-- vendors — as Subjects and reasons about it. It models its OWN
-- physical world nowhere. Every fact about the estate has had to be
-- fetched by opening a shell, and none of it was reachable from inside
-- BossNET. David, 2026-08-16: "I have wanted a view on the physical
-- infrastructure for a while... it makes it almost impossible to
-- foresee bottlenecks without having the data available to BossNET."
--
-- Three designs converged on the same answer and all three are now
-- accepted: bossnet-physical-topology Q1 ("new `node` and
-- `service-instance` Subject kinds"), infrastructure-view Q1 ("yes, and
-- let this page be the forcing function"), and dev-node-checkout Q1
-- ("a service-instance, with no new kind" — a leasable dev box is one
-- of these, not a third thing).
--
-- WHAT THIS COSTS IF IT IS WRONG. It has already cost. An agent
-- investigating "David cannot see his own feedback" read the roster
-- from boss-gcp, found no emp-david, and published a root cause that
-- had to be retracted — because two complete BOSS deployments exist
-- and nothing recorded which one is authoritative. `authoritative` on
-- service_instances below is that fact, written down once.
--
-- THE TAXONOMY. `node` specialises `object` (the "what" axis, tracked
-- physical things — a machine is one). `service-instance` specialises
-- `intangible`, the axis for identity-bearing things with no physical
-- embodiment, which already hosts the contract / SLA / lease family —
-- and a leased dev box belongs with leases.
--
-- NOT birth-by-job. `workflow` and `custom` carry
-- `metadata.birth = 'job'` because the Job creating them IS their
-- birth record. A node exists whether or not anyone opens a packet
-- about it, so these stay fail-closed: a Job pointing at a node id
-- that was never declared is a mistake worth refusing.

INSERT INTO subject_kinds (kind, label, description, owning_team, sort_order, parent_kind) VALUES
    ('node',             'Node',             'A machine BOSS runs on. Carries its address, its role, and its DECLARED capacity — cpu, memory, disk. Observed state (free space now) is an event, not a column: a measurement has a timestamp and belongs in the log.', 'platform', 90, 'object'),
    ('service-instance', 'Service instance', 'One (service, node, environment) triple: which port it serves, which database it reads, and whether it is AUTHORITATIVE for that data. boss-ports already answers service -> port; this is the missing service -> node -> database -> authority.', 'platform', 95, 'intangible')
ON CONFLICT (kind) DO NOTHING;

-- The domain rows. `subjects` is an identity registry — its own comment
-- says the label is "display convenience only; never authoritative —
-- the domain row owns naming" — so the attributes live here.
--
-- DECLARED capacity only, which is bossnet-physical-topology Q2's
-- accepted answer: "declared capacity in the tree, observed state in
-- the log." A node's cpu/memory/disk is intent, versioned and
-- reviewable, and it arrives through a car. "cp-2 has 188GB free right
-- now" is a measurement and does not belong in a migration. The two
-- disagreeing is then a finding rather than a mystery — which is
-- exactly the check that would have caught the 63GB orphaned CI volume.
CREATE TABLE IF NOT EXISTS nodes (
    id            TEXT PRIMARY KEY,
    label         TEXT NOT NULL,
    address       TEXT NOT NULL,
    role          TEXT NOT NULL,
    cpu           INTEGER,
    memory_gb     INTEGER,
    disk_gb       INTEGER,
    notes         TEXT,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    retired_at    TIMESTAMPTZ
);

CREATE TABLE IF NOT EXISTS service_instances (
    id            TEXT PRIMARY KEY,
    label         TEXT NOT NULL,
    service       TEXT NOT NULL,
    node_id       TEXT NOT NULL REFERENCES nodes(id),
    environment   TEXT NOT NULL,
    port          INTEGER,
    database_url  TEXT,
    -- THE BIT THAT ENDS THE CONFUSION. Exactly one instance should
    -- claim authority for a given dataset; a conformance check can
    -- assert that, which turns split-brain from folklore into a
    -- finding. Cluster is authoritative (topology Q5, David:
    -- "Cluster should be authoritative").
    authoritative BOOLEAN NOT NULL DEFAULT FALSE,
    -- A dev box is a service-instance that can be leased. Non-leasable
    -- instances (the jobs API, the forge) are simply never claimed.
    leasable      BOOLEAN NOT NULL DEFAULT FALSE,
    notes         TEXT,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    retired_at    TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS service_instances_node ON service_instances(node_id);
CREATE INDEX IF NOT EXISTS service_instances_leasable
    ON service_instances(leasable) WHERE retired_at IS NULL;

-- The estate as measured on 2026-08-16, not as remembered.
-- cp-* figures are `kubectl get nodes -o custom-columns` (capacity, so
-- the memory is the node's total and the ephemeral figure is its
-- allocatable disk); boss-gcp and the forge host are `df -h` / `nproc`
-- over ssh.
INSERT INTO nodes (id, label, address, role, cpu, memory_gb, disk_gb, notes) VALUES
    ('cp-1',     'cp-1',        '10.20.0.11',   'talos-control-plane', 8,  15,  236, 'Runs kanidm. Cluster VIP is 10.20.0.10.'),
    ('cp-2',     'cp-2',        '10.20.0.12',   'talos-control-plane', 12, 31,  236, 'Most cores of the three; boss-dev is pinned here.'),
    ('cp-3',     'cp-3',        '10.20.0.13',   'talos-control-plane', 8,  31,  463, 'Largest disk of the three.'),
    ('forge',    'Forge host',  '10.20.0.15',   'forge',               16, 30,  228, 'Forgejo, the container registry, and the CI runner. A cold CI job needs ~74GB of target/.'),
    ('boss-gcp', 'boss-gcp',    '34.45.110.40', 'conductor',           4,  15,  48,  'The train conductor and the cadence loop. 48GB is the smallest disk in the estate and nothing watches it.')
ON CONFLICT (id) DO NOTHING;

INSERT INTO service_instances (id, label, service, node_id, environment, port, database_url, authoritative, leasable, notes) VALUES
    ('boss-cluster',  'BOSS (cluster)',     'boss',     'cp-2',     'prod', 7900, 'in-cluster postgres',              TRUE,  FALSE, 'The system of record. Pods float across cp-1/2/3; node_id records where the workload sits today, not a pin.'),
    ('boss-gcp-local','BOSS (boss-gcp)',    'boss',     'boss-gcp', 'demo', 7900, 'postgres://boss@127.0.0.1/boss',   FALSE, FALSE, 'A SECOND complete deployment holding different data — 66 user-feedback packets against the cluster''s 168. Explicitly NOT authoritative (topology Q5). Reading the roster here produced a retracted root cause on packet e8665893.'),
    ('forgejo',       'Forgejo',            'forgejo',  'forge',    'prod', 3000, NULL,                               TRUE,  FALSE, 'The pipeline: repo david/boss, container registry, Actions runner. Not the GitHub mirror.'),
    ('kanidm',        'Kanidm',             'kanidm',   'cp-1',     'prod', NULL, 'own state',                        TRUE,  FALSE, 'Passkey-first IdP. NOTE: idm-kanidm.md states "the cluster is a client of identity, never its host" and this row records that the deployment contradicts it — see correction 4c8259ea.'),
    ('boss-dev-0',    'Dev node 0',         'boss-dev', 'cp-2',     'dev',  NULL, 'sidecar postgres 16 on 127.0.0.1', FALSE, TRUE,  'The leasable workspace: 12 cores, 188GB node-local scratch for CARGO_TARGET_DIR, a 40GB Longhorn PVC for the clone, and a Postgres 16 sidecar so 127.0.0.1:5432 cannot be production. Ran the full gate green in 4m50s.')
ON CONFLICT (id) DO NOTHING;

-- Identity rows, so a Job may name any of these as its subject.
INSERT INTO subjects (kind, id, label)
    SELECT 'node', id, label FROM nodes
ON CONFLICT (kind, id) DO NOTHING;

INSERT INTO subjects (kind, id, label)
    SELECT 'service-instance', id, label FROM service_instances
ON CONFLICT (kind, id) DO NOTHING;
