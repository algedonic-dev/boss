-- 20260928142220-a-class-edited-outside-the-doors-names-its-birth.sql
-- — class.declared and class.updated each have one more origin: a
-- birth the caller names.
--
-- Origin: backlog 93f361af. The backfill door refuses a Class with no
-- fact whose row was changed after its insert (review of car ce5ce2de,
-- HIGH-1: its live body is not its birth). Measured live 2026-09-28:
-- the LLC's invoice Classes hosting, sponsorship and support, inserted
-- 2026-09-16 with no metadata and given gl_account by a bare UPDATE
-- (boss tenant publish --take classes, before 10dabe13 gave edits a
-- fact) on 2026-09-26 — so no fresh rebuild reproduced them.
--
-- POST /api/classes/{kind}/{code}/backfill-declared with a body
-- {born, changed, source} records, for such a Class, the class.declared
-- of the body it was born with AND the class.updated that took it to the
-- live row, each carrying `backfilled` ('backfilled from <source> on
-- <date>') and `backfill_source`, and only when that one edit — changing
-- exactly the fields `changed` names — replays from nothing to the live
-- row. The declare carries born_at (the row's created_at) and the edit
-- edited_at (its updated_at) and edited_by null (unknown): each fact's
-- own timestamp is the day it was recorded, and updated_by the actor
-- that recorded it (review of car 8778f12f, finding 2). Same two facts,
-- so their descriptions say the new origin.

UPDATE event_kinds
   SET description = 'One Class row was declared: by a tenant''s batch insert (POST /api/classes/batch, insert-if-absent — one per inserted row, none for a kept row, none per batch), or recorded late for a row the batch door inserted before it staged this fact (POST /api/classes/{kind}/{code}/backfill-declared, carrying backfilled: the provenance, and the row as it stood before its first logged fact — or, given a body {born, changed, source} for a row changed outside the doors, the body it was born with, beside a backfilled class.updated, carrying backfill_source and born_at, the row''s created_at). The row plus declared_by, the actor the request signed with'
 WHERE kind_pattern = 'class.declared';

UPDATE event_kinds
   SET description = 'An edit changed one Class row''s editable body (PUT /api/classes/{kind}/{code}, the door boss tenant publish --take classes edits through), or recorded late the edit a row with no fact was given outside the doors before 10dabe13 (POST /api/classes/{kind}/{code}/backfill-declared with a body {born, changed, source}: carrying backfilled, the provenance; backfill_source, where the born body is recorded; edited_at, the row''s updated_at when it was recorded; and edited_by null, because who made that edit is unknown). The key, changed (the field names), before and after holding only those fields, and updated_by, the actor the request signed with. None for a PUT that restates the held row'
 WHERE kind_pattern = 'class.updated';
