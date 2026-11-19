-- A current projection, not a second history. Generations survive release;
-- immutable reserved/released facts are the reconstruction source.
-- Existing D1 registry versions and events remain immutable. The private
-- read/replay decoder handles their raw numeric amount tokens; only NEW
-- versions published through their registry door use decimal strings.

CREATE TABLE step_resource_claims (
    step_id UUID PRIMARY KEY,
    job_id UUID NOT NULL,
    generation BIGINT NOT NULL CHECK (generation > 0),
    holder TEXT NOT NULL,
    active BOOLEAN NOT NULL,
    body JSONB NOT NULL
);
CREATE TABLE step_resource_allocations (
    step_id UUID NOT NULL REFERENCES step_resource_claims(step_id) ON DELETE CASCADE,
    pool TEXT NOT NULL,
    amount NUMERIC NOT NULL CHECK (amount > 0),
    PRIMARY KEY (step_id,pool)
);
CREATE INDEX step_resource_allocations_pool ON step_resource_allocations(pool);
INSERT INTO event_kinds (kind_pattern, source, description)
VALUES
       ('jobs.step.reserved','jobs','A pinned whole FLOW vector was reserved atomically'),
       ('jobs.step.released','jobs','A generation and holder fenced FLOW vector was released'),
       ('jobs.step.resource_refused','jobs','A resource claim was refused without an allocation'),
       ('jobs.station.resource_refused','jobs','A station selection retained its resource refusal receipt')
ON CONFLICT (kind_pattern) DO NOTHING;
