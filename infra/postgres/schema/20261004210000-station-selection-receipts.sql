-- A station selection receipt is server-owned claim evidence, never packet
-- metadata supplied by a caller. Its request identity binds retries to the
-- original actor, station definition and selected step. The claim and this
-- receipt commit in the same transaction; replay changes neither.
-- TEXT conserves the original JSON bytes; JSONB would rewrite whitespace,
-- ordering and number representation before the receipt could be copied.
CREATE TABLE IF NOT EXISTS station_claim_receipts (
    request_id UUID PRIMARY KEY,
    receipt TEXT NOT NULL,
    CHECK (jsonb_typeof(receipt::jsonb) = 'object'),
    CHECK ((receipt::jsonb ->> 'request_id') IS NOT NULL AND receipt::jsonb ->> 'request_id' = request_id::text)
);

INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  ('jobs.station.claimed', 'jobs', 'A station selection atomically claimed its selected READY step and conserved the server receipt', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;
