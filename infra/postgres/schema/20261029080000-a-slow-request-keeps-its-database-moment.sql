-- d9b39685, 2026-10-29: operational observations, NOT command state or
-- audit events. A request past the 2 s finish line keeps what PostgreSQL
-- was doing while it stalled (wait events, lock waits and their blockers,
-- the serving pool, the membership refresh in flight), so the next car is
-- chosen from evidence. Only the private recorder writes, inside its own
-- telemetry transaction; the slowest 32 per UTC hour are kept for the same
-- 72 hours as sensor_request_latency_hours.
CREATE TABLE IF NOT EXISTS sensor_request_latency_slow (
    id BIGSERIAL PRIMARY KEY,
    hour BIGINT NOT NULL CHECK (hour % 3600 = 0),
    duration_ns BIGINT NOT NULL CHECK (duration_ns > 0),
    sample JSONB NOT NULL CHECK (jsonb_typeof(sample) = 'object')
);
CREATE INDEX IF NOT EXISTS sensor_request_latency_slow_by_hour
    ON sensor_request_latency_slow (hour, duration_ns DESC);
