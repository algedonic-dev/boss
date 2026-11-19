-- A station is generic or equipment, and equipment declares its bays
-- (item 0a78f8f8; factory taxonomy M2 and Q1, design 4348b857, decided
-- 2026-11-02).
--
-- THE KIND. `batch` is deleted. A batch station is an equipment station
-- whose automaton's program is a cadence, so the dock becomes
-- `equipment`; the review queues and repair, which also said `batch`,
-- are served by sets of actors and become `group`. The stored rows are
-- relabelled HERE, every version of every name, by one rule: a `batch`
-- row declares no automaton and no bays, and an equipment row without
-- them is not one, so it is `group`. The kinds the rows ship with NOW
-- are declared in infra/platform/stations/ as new versions —
-- loading-dock v5 equipment, the four others group — and the seed
-- publishes them with their `jobs.station.published` facts. The LOG is
-- not relabelled: every earlier publication, and every move-diagnosis
-- receipt that copied one, keeps `batch` as it was recorded and hashed,
-- read by `StationKind::RetiredBatch`, which no publish accepts.
--
-- THE COLUMNS. `automaton` is the actor id of the automaton that
-- operates an equipment station (`automation:<slug>`); `bays` is how
-- many packets it serves at once. Both are required on `equipment` and
-- refused on every other kind — here, and by the viability lint every
-- publish passes (`station_lint::check_equipment`), so a direct write
-- cannot hold what the door refuses.
ALTER TABLE stations DROP CONSTRAINT stations_kind_check;
UPDATE stations SET kind = 'group' WHERE kind = 'batch';
ALTER TABLE stations ADD CONSTRAINT stations_kind_check
    CHECK (kind IN ('actor', 'group', 'constraint', 'equipment'));
ALTER TABLE stations ADD COLUMN automaton TEXT;
ALTER TABLE stations ADD COLUMN bays INT;
-- Each half spells IS NOT NULL: a CHECK passes on NULL, so
-- `bays >= 1` alone would admit an equipment row with no bays at all
-- (caught by stations_pg's raw-write probe before this file landed).
ALTER TABLE stations ADD CONSTRAINT stations_equipment_declares_its_bays CHECK (
    (kind = 'equipment'
        AND automaton IS NOT NULL AND automaton LIKE 'automation:_%'
        AND bays IS NOT NULL AND bays >= 1)
    OR (kind <> 'equipment' AND automaton IS NULL AND bays IS NULL)
);

-- THE GATE CAPACITY HAS ONE HOME (CLAUDE.md §9a). It was
-- `delivery_policy.gate_max_concurrent` (202609031000); it is now the
-- `bays` of the `gates` equipment station, which every reader takes —
-- `boss gate`, the resident supervisor, the conductor and the yard. The
-- train-conductor policy's v12 drops the key, and the column goes with
-- it rather than staying as a second copy nothing reads. What each
-- earlier policy version said about it stays in the header of
-- infra/platform/delivery-policy/train-conductor.toml, version by
-- version, and in the git history of that file.
ALTER TABLE delivery_policy DROP COLUMN gate_max_concurrent;
