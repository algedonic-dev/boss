-- Rebuildable projections. Logical expiry is not physical teardown.
ALTER TABLE step_resource_claims ADD COLUMN lease_until TIMESTAMPTZ;
CREATE INDEX step_resource_claims_expiry ON step_resource_claims(lease_until,step_id) WHERE active AND lease_until IS NOT NULL;
CREATE TABLE resource_identity_bindings (
    acquisition UUID PRIMARY KEY,
    step_id UUID NOT NULL,
    body JSONB NOT NULL
);
CREATE TABLE resource_stock_obligations (
    acquisition UUID NOT NULL,
    pool TEXT NOT NULL,
    pool_version INTEGER NOT NULL CHECK(pool_version>0),
    amount NUMERIC NOT NULL CHECK(amount>0),
    returned_by UUID,
    body JSONB NOT NULL,
    PRIMARY KEY(acquisition,pool)
);
CREATE INDEX resource_stock_obligations_held ON resource_stock_obligations(pool) WHERE returned_by IS NULL;
CREATE UNIQUE INDEX resource_stock_return_evidence_once ON resource_stock_obligations(returned_by) WHERE returned_by IS NOT NULL;
INSERT INTO event_kinds(kind_pattern,source,description) VALUES
('jobs.step.resource_identity_bound','jobs','The current holder bound original resource identities once to its acquisition'),
('jobs.step.resource_lease_expired','jobs','A bounded sweep expired a lease while retaining physical stock obligations'),
('jobs.step.stock_returned','jobs','A named reclaimer consumed a qualified return witness once')
ON CONFLICT(kind_pattern) DO NOTHING;
