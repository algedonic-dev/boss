-- 20260926213740-a-discarded-workflow-version-stays-spent.sql — a
-- (kind, version) pair names ONE protocol forever.
--
-- Origin: backlog ce8b7d66, from the adversarial review of car 5d1c0b7a
-- (2026-09-26). The registry allocated a new version as MAX(version) + 1
-- over the `workflows` rows that exist, and discarding a draft
-- (DELETE /api/workflows/{kind}/versions/{v}, ebd7bb70) removes its row
-- — so discarding the NEWEST draft freed its number and the next draft
-- took it. A packet pinned to that pair (an experiment admits packets
-- to its draft candidate; `boss job convert --to vN` checks no status)
-- then silently ran a different protocol: other steps, other
-- predicates, other declared executors. In-flight packets stay pinned
-- to what they were admitted under, and a reused number breaks that
-- without touching the packet.
--
-- A DISCARDED NUMBER IS SPENT. Every allocation path (create_draft,
-- publish_authored, the bootstrap reconcile's republish) takes the next
-- number above BOTH the live rows and this table, and a discard writes
-- its (kind, version) here in the same transaction as the DELETE and
-- the jobs.kind.draft_discarded event. The draft's text itself is not
-- kept here: the discard event's payload is the full spec, so the log
-- already holds it.
--
-- BACKFILLED FROM THE LOG. Every discard ever made recorded
-- jobs.kind.draft_discarded with the spec as payload, so the numbers
-- already freed are recovered from audit_log rather than guessed. A
-- number that was freed and has since been reused by a live row is
-- recorded too; the allocator takes the greater of the two sources, so
-- that row is untouched.

CREATE TABLE IF NOT EXISTS workflow_discarded_versions (
    kind          TEXT NOT NULL,
    version       INT NOT NULL,
    discarded_at  TIMESTAMPTZ NOT NULL,
    discarded_by  TEXT NOT NULL,
    PRIMARY KEY (kind, version)
);

INSERT INTO workflow_discarded_versions (kind, version, discarded_at, discarded_by)
SELECT payload->>'kind',
       (payload->>'version')::int,
       MIN(created_at),
       COALESCE(MIN(payload->>'_actor'), 'unknown')
FROM audit_log
WHERE kind = 'jobs.kind.draft_discarded'
  AND jsonb_typeof(payload->'kind') = 'string'
  AND jsonb_typeof(payload->'version') = 'number'
GROUP BY 1, 2
ON CONFLICT (kind, version) DO NOTHING;
