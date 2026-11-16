-- 20261108094034-a-pool-is-retired-through-its-door.sql — the live
-- pools door (backlog f399458f, David 2026-11-08: "we should be able to
-- make these registry changes via a different protocol than the code
-- changes").
--
-- POST /api/pools/{name}/retire takes a pool's active version out of
-- service and records this fact in the same transaction; its payload is
-- the row it retired. Publishing needs no new kind: the door writes the
-- `jobs.pool.published` fact the seed already writes. Schema only: the
-- table's status CHECK already holds `retired`.
INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain)
VALUES
  ('jobs.pool.retired', 'jobs', 'A resource pool''s active version was retired through the live pools door, atomically with its outbox fact', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;
