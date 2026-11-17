-- D9/A1906: operational observations, NOT command state or audit events.
-- One UTC hour holds both populations and their mergeable distributions.
-- Only the private recorder writes; no public ingest or domain outbox.
CREATE TABLE IF NOT EXISTS sensor_request_latency_hours (
    hour BIGINT PRIMARY KEY CHECK (hour % 3600 = 0),
    payload JSONB NOT NULL CHECK (jsonb_typeof(payload) = 'object')
);
