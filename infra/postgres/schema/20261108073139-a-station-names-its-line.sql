-- A station names its line (design c4854fd4, backlog 85cb2103): the
-- Design token code its card rail draws, declared on the root and the
-- stations under it and inherited below by walking parents. ADDITIVE
-- ONLY: a NULL reads as absent, which StationSpec skips on the wire, so
-- every row before this one serializes as before, and a build that
-- predates the column names it in neither its SELECT nor its INSERT.
ALTER TABLE stations ADD COLUMN line TEXT;
