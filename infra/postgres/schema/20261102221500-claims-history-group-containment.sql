-- A213122/213319, item7c07b911: a ranged cut count took4739ms in the
-- isolated recorded-week fixture because each cut rescanned180197 groups.
-- The exact half-open original-transaction interval has a native range
-- operator/index. Generation remains an explicit query predicate.
-- Schema-only, ordinary transactional CREATE INDEX; never CONCURRENTLY
-- on the boot path (78625ebd). No fact, registry or projection row changes.
CREATE INDEX IF NOT EXISTS jobs_claim_history_group_contains_cut
    ON jobs_claim_history_groups USING gist (int8range(lo, hi, '[)'));
