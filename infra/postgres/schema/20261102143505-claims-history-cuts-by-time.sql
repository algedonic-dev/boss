-- Console car 3c (item c46a4e99): the review screen scrubs the claims history
-- by TIME, so the retained cuts are read by (generation, at) as well as by
-- their (generation, seq) key — the sequence-to-time index. A plain index on a
-- derived projection table bounded by retention; no fact or registry row
-- changes. Not CONCURRENTLY: that form cannot run inside the migration
-- transaction and darkened the system of record once (item 78625ebd).
CREATE INDEX IF NOT EXISTS jobs_claim_history_cuts_by_time
    ON jobs_claim_history_cuts(generation, at, seq);
