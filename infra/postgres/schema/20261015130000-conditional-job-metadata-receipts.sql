-- Actor-bound immutable command receipts (design 2bd4a54e).
CREATE TABLE IF NOT EXISTS job_metadata_receipts (
    job_id UUID NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    actor TEXT NOT NULL,
    command_id UUID NOT NULL,
    receipt TEXT NOT NULL,
    PRIMARY KEY (job_id, actor, command_id)
);
INSERT INTO event_kinds (kind_pattern, source, description) VALUES
    ('jobs.metadata.conditionally_merged', 'jobs', 'Immutable conditional metadata command receipt; rebuilds idempotency projection')
ON CONFLICT (kind_pattern) DO NOTHING;
