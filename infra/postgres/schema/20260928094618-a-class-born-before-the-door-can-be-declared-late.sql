-- 20260928094618-a-class-born-before-the-door-can-be-declared-late.sql
-- — class.declared has a second origin: the backfill door.
--
-- Origin: backlog 9d345f9b. POST /api/classes/batch staged no fact
-- until 2026-09-17 (d9409039), so a Class declared through it before
-- then, and not seeded by a migration, has no birth in the log. Measured
-- live 2026-09-28: ten such Classes (employee drafting-agent,
-- engineering-agent, support-agent, operations, hosting, product,
-- engineering; invoice hosting, sponsorship, support), four of them
-- already retired through the door — so a log-rooted rebuild onto a
-- fresh database met a retirement of a row it did not hold, and
-- rebuild_classes stopped as drift.
--
-- POST /api/classes/{kind}/{code}/backfill-declared records that birth:
-- one class.declared carrying `backfilled` ('backfilled from the live
-- row on <date>'), the row as it stood before the first fact the log
-- holds for it. rebuild_classes reads a backfilled declare first among
-- its Class's facts. The kind is the same fact, so its description says
-- both origins.

UPDATE event_kinds
   SET description = 'One Class row was declared: by a tenant''s batch insert (POST /api/classes/batch, insert-if-absent — one per inserted row, none for a kept row, none per batch), or recorded late for a row the batch door inserted before it staged this fact (POST /api/classes/{kind}/{code}/backfill-declared, carrying backfilled: the provenance, and the row as it stood before its first logged fact). The row plus declared_by, the actor the request signed with'
 WHERE kind_pattern = 'class.declared';
