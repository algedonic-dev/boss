-- A station stands at a placement on its parent's floor (backlog
-- e3a927c3, David 2026-11-19): `{x, y, w, h}` in floor units, so the
-- station view draws every station at a fixed place an operator learns.
-- ADDITIVE ONLY: a NULL reads as absent, which StationSpec skips on the
-- wire, so every row before this one serializes as before, the view lays
-- it out on its fallback grid, and a build that predates the column names
-- it in neither its SELECT nor its INSERT.
ALTER TABLE stations ADD COLUMN placement JSONB;
