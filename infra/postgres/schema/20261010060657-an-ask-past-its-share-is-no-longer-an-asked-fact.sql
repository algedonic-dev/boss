-- 20261010060657-an-ask-past-its-share-is-no-longer-an-asked-fact.sql —
-- restate `machine_gate.asked` (review 42836ede of car 10ea15f6, N4;
-- backlog aba44bac).
--
-- 20261007122553 declared the kind as stated "at its first sighting in
-- this process or past its source's share". Since design e3d19d1a q1
-- (car 10ea15f6) an ask past its source's share is no longer an `asked`:
-- it names no asker, and the gate states it as a `tally_overflowed` of
-- scope `source`, which ends a clean window. The description still read
-- the old way, and a reader of the registry would take a spill for a
-- clean ask.
--
-- The log keeps what earlier builds said: a build before that decision
-- stated the spill as an `asked` with no key, and
-- `boss_core::gate_evidence::Gate::dirtied_by` reads such a fact as the
-- overflow it is. The description says both, so the registry and the
-- reader agree. Same kind, same source; no ref-check rule changes.
UPDATE event_kinds
   SET description = 'A caller shape asked the machine gate''s accepts route about one value no slot holds, at its first sighting in this process — answered none in report and 401 in enforce, named, and never a would-refuse (names the service, mode, tally start and key: peer, asserted user, method, route, presented asked). Past its source''s share an ask is no longer keyed and is stated as machine_gate.tally_overflowed of scope source (design e3d19d1a); a build before that stated it as an asked with no key (scope source), which the enforce window reads as dirty'
 WHERE kind_pattern = 'machine_gate.asked';
