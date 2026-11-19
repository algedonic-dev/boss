-- Console design D2: declarations only. Reservations and leases are car 2.
CREATE TABLE resource_pools (
    name TEXT NOT NULL,
    version INT NOT NULL CHECK (version > 0),
    status TEXT NOT NULL CHECK (status IN ('active', 'retired')),
    spec JSONB NOT NULL CHECK (jsonb_typeof(spec) = 'object'),
    created_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (name, version),
    CHECK (spec->>'name' = name AND (spec->>'version')::INT = version)
);
CREATE UNIQUE INDEX resource_pools_one_active_per_name ON resource_pools(name) WHERE status='active';
INSERT INTO event_kinds(kind_pattern,source,description,suffix_domain) VALUES
('jobs.pool.published','jobs','A factory resource-pool version was published atomically with its outbox fact',NULL)
ON CONFLICT (kind_pattern) DO NOTHING;
