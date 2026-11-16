-- A station stands in a tree (T2 of design 4b27a65a, backlog 3f58c823):
-- a stable id, the id of the station it stands in, standing or ad hoc,
-- its seats, the machines provisioned into it and what it holds of each
-- pool from its parent. The address is derived from the parents and is
-- not a column. ADDITIVE ONLY: every row before this one is outside the
-- tree, and the defaults read it that way — NULLs, an empty list and an
-- empty object, which StationSpec skips on the wire, so the row
-- serializes as before; a build that predates these columns names none
-- of them in its SELECT or its INSERT. The one-root, parent-exists,
-- no-cycle and allocation rules are judged across rows at the platform
-- seed (boss_jobs::station_tree), not here.
ALTER TABLE stations ADD COLUMN id TEXT;
ALTER TABLE stations ADD COLUMN parent TEXT;
ALTER TABLE stations ADD COLUMN mode TEXT;
ALTER TABLE stations ADD COLUMN capacity INT;
ALTER TABLE stations ADD COLUMN machines TEXT[] NOT NULL DEFAULT '{}';
ALTER TABLE stations ADD COLUMN allocation JSONB NOT NULL DEFAULT '{}'::jsonb;
