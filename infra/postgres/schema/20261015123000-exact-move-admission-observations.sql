-- c8d22e63 / design 6d996b25: evidence only, separate from correction eligibility.
CREATE TABLE IF NOT EXISTS yard_move_admission_observations (
    id UUID PRIMARY KEY,
    original_seq BIGINT NOT NULL REFERENCES yard_moves(seq),
    diagnosis_id UUID NOT NULL REFERENCES yard_move_corrections(id),
    body TEXT NOT NULL CHECK (octet_length(body) <= 2000000),
    event_id UUID NOT NULL UNIQUE
);
INSERT INTO event_kinds(kind_pattern,source,description,suffix_domain) VALUES
 ('jobs.yard.move.admission-observed','jobs','An exact-original-bound native admission observation; supplies no correction eligibility or fault clearance',NULL)
ON CONFLICT(kind_pattern) DO NOTHING;
