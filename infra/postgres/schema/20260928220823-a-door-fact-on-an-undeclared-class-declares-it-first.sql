-- 20260928220823-a-door-fact-on-an-undeclared-class-declares-it-first.sql
-- — class.declared has one more origin: the edit and retire doors.
--
-- Origin: backlog 6c2aa86c. The backfill door's only check on a change
-- made outside the doors (updated_at moved off created_at) ran only when
-- the Class's log was EMPTY, so the first edit or retirement through a
-- door on an undeclared Class switched it off for good: a row changed
-- outside the doors and then retired through them was backfilled with
-- the outside body as its birth. Measured live 2026-09-28: 4 Classes
-- held a retirement and no declare (employee/it, node/legacy-stack,
-- marketing-asset/retro, marketing-asset/brief-body) and 80 of 93 active
-- rows held no fact at all.
--
-- PUT /api/classes/{kind}/{code} and POST …/retire now record, in their
-- own transaction and before their own fact, the backfilled
-- class.declared of a Class the log does not declare — the row as it
-- stood before its first logged fact — when the row's stamps show no
-- write the log did not hear of (updated_at is still its insert's, or
-- the retirement its newest fact records). When they show one, the door
-- refuses 409, records nothing, and names the door that will take it:
-- POST …/backfill-declared with {"observed": true, "source": "<why>"}
-- records an OBSERVED birth — the row as it stands with its logged facts
-- un-applied, a retirement no fact records kept — carrying birth:
-- 'observed', backfill_source, born_at and row_updated_at (the row's
-- stamps) and drift (why they could not prove it). The review of this car
-- found five live Classes no other door would take (asset/triaging,
-- refurbing, qa, ready and message/archived, retired by migrations).
-- Same fact, so its description says both new origins.

UPDATE event_kinds
   SET description = 'One Class row was declared: by a tenant''s batch insert (POST /api/classes/batch, insert-if-absent — one per inserted row, none for a kept row, none per batch), or recorded late for a row the batch door inserted before it staged this fact (POST /api/classes/{kind}/{code}/backfill-declared, carrying backfilled: the provenance, and the row as it stood before its first logged fact — or, given a body {born, changed, source} for a row changed outside the doors, the body it was born with, beside a backfilled class.updated, carrying backfill_source and born_at, the row''s created_at), or recorded the same way by the edit or retire door (PUT /api/classes/{kind}/{code}, POST …/retire) in its own transaction, before its own fact, for a Class the log did not yet declare, or recorded as observed on an operator''s word (the backfill door with a body {observed: true, source}: carrying birth: observed, backfill_source, born_at, row_updated_at and drift, and keeping a retirement no fact records). The row plus declared_by, the actor the request signed with'
 WHERE kind_pattern = 'class.declared';
