-- c8d22e63 / settled design a34bc69e: append beside immutable yard_moves.
-- A version alone never clears a finding; full native input receipts are retained.
CREATE TABLE IF NOT EXISTS yard_move_corrections (
    seq BIGINT PRIMARY KEY,
    id UUID NOT NULL UNIQUE,
    original_seq BIGINT NOT NULL REFERENCES yard_moves(seq),
    body TEXT NOT NULL,
    event_id UUID NOT NULL UNIQUE
);
INSERT INTO event_kinds(kind_pattern,source,description,suffix_domain) VALUES
 ('jobs.yard.move.diagnosed','jobs','A deterministic exact-original diagnosis with whole native input and model receipts',NULL),
 ('jobs.yard.move.corrected','jobs','An exact original finding is superseded by its retained eligible diagnosis; raw history remains unchanged',NULL),
 ('jobs.yard.move.withdrawn','jobs','A withdrawal restores the original finding without rewriting either record',NULL)
ON CONFLICT(kind_pattern) DO NOTHING;
