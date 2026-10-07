-- Generic immutable evidence projection (design 3bb90d7b).
-- The receipt is TEXT so JSONB cannot normalize the authoritative scalar form.
CREATE TABLE IF NOT EXISTS step_first_records (
    step_id UUID NOT NULL REFERENCES steps(id) ON DELETE CASCADE,
    key TEXT NOT NULL CHECK (length(key) BETWEEN 1 AND 256),
    receipt TEXT NOT NULL,
    PRIMARY KEY (step_id, key)
);

INSERT INTO event_kinds (kind_pattern, source, description) VALUES
    ('jobs.step.first_recorded', 'jobs', 'Full step state and immutable first evidence receipt')
ON CONFLICT (kind_pattern) DO NOTHING;
