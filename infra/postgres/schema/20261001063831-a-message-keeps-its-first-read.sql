-- 20261001063831-a-message-keeps-its-first-read.sql — marking a read
-- message read again changes nothing (backlog 624e92eb, the read half
-- of 9bda9726).
--
-- WHAT WAS WRONG. The mark-read door's UPDATE had no guard, so marking a
-- read message read again overwrote `read_at` and recorded a second
-- messages.message.read. From this migration on the doors and the
-- rebuild (crates/modules/boss-messages) move only an unread row, so the
-- FIRST read stands, and a repeat read already in the log is skipped on
-- replay.
--
-- THE BACKFILL IS DERIVED FROM THE LOG, AND LANDS WHERE THE REBUILD
-- LANDS. A message the old door marked more than once holds its LAST
-- read time; a rebuild now gives it its FIRST read after its last sent
-- event (a deleted id can be sent again; the rebuild replays the last),
-- so without this the next rebuild would move every such row. The
-- payload holds the wall clock's nanoseconds and the column holds
-- microseconds: a text cast ROUNDS, where the rebuild's sqlx bind
-- TRUNCATES, so the fraction is cut to six digits before the cast.
-- rebuild_e2e.rs
-- (the_read_migration_keeps_the_first_read_the_way_the_rebuild_does)
-- runs THIS FILE against legacy-shaped rows and holds the result equal
-- to a rebuild of the same log.
--
-- Only a message with more than one read event is considered, and only
-- a row already read whose time differs moves — so a row the log cannot
-- speak for is left alone, and a second application changes nothing.
WITH repeated AS (
    SELECT payload->>'id' AS id
      FROM audit_log
     WHERE kind = 'messages.message.read'
     GROUP BY 1
    HAVING COUNT(*) > 1
),
last_sent AS (
    SELECT DISTINCT ON (a.payload->>'id')
           a.payload->>'id' AS id, a.id AS seq, a.payload->>'read_at' AS sent_read_at
      FROM audit_log a
      JOIN repeated r ON r.id = a.payload->>'id'
     WHERE a.kind = 'messages.message.sent'
     ORDER BY a.payload->>'id', a.id DESC
),
first_read AS (
    SELECT DISTINCT ON (a.payload->>'id')
           a.payload->>'id' AS id,
           regexp_replace(a.payload->>'read_at', '(\.[0-9]{6})[0-9]+', '\1')::timestamptz AS read_at
      FROM audit_log a
      JOIN last_sent s ON s.id = a.payload->>'id'
     WHERE a.kind = 'messages.message.read'
       AND a.id > s.seq
       AND s.sent_read_at IS NULL
     ORDER BY a.payload->>'id', a.id
)
UPDATE messages m
   SET read_at = f.read_at
  FROM first_read f
 WHERE m.id = f.id
   AND m.read_at IS NOT NULL
   AND m.read_at <> f.read_at;
