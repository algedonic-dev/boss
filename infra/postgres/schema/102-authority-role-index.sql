-- 102-authority-role-index.sql — index the group-lens branch of the
-- work queue.
--
-- `GET /api/jobs/assignments` unions two lenses over in-flight steps:
-- mine-by-assignee (covered by the partial `steps_assignee` index
-- since 03-jobs) and claimable-by-role, which filters on
-- `metadata->>'authority_role'` and has had no index support at all.
-- The fleet view (`/api/views/fleet`) aggregates per-role depth over
-- the same predicate. Both queries touch only ready/active rows —
-- 0.15% of the steps table on the live playground — so the index is
-- partial over exactly that live set, like `steps_assignee`.
--
-- Expression index rather than promoting authority_role to a column:
-- the value is workflow-authored step metadata (registry.rs surfaces
-- it at materialization), immutable across PUTs, and read-only at
-- query time — a column would be a second copy of a fact that already
-- has one home (CLAUDE.md §9a).
CREATE INDEX IF NOT EXISTS steps_authority_role
    ON steps ((metadata->>'authority_role'))
    WHERE status IN ('ready', 'active');
