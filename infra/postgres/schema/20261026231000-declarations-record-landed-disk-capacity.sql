-- c426bc8a: request fields in old role-only facts were not updates.
-- New capacity changes identify exactly what landed; no old fact is
-- reinterpreted and no migration rewrites the estate's node rows.
UPDATE event_kinds
SET description = 'A tree estate declaration inserted a node, appended roles, or refreshed explicit disk capacity. The payload retains the request and declared_by; landed.node and landed.roles identify actual inserts, and optional landed.disk_gb holds the actual before/after change. Requested fields without a landed marker must not be replayed as updates. No fact for a no-op.'
WHERE kind_pattern = 'node.declared' AND source = 'jobs';
