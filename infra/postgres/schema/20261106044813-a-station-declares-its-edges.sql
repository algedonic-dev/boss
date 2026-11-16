-- A station declares its edges: the stations a packet goes to next
-- (pass item 4 of design ccfe87ff, backlog 1bb8c479). Optional, by
-- station name; the platform seed refuses an edge to a station no active
-- row declares before it publishes. Every row before this one declares
-- none, and NOT NULL DEFAULT '{}' reads it that way — an empty list,
-- which StationSpec skips on the wire, so the row serializes as before.
ALTER TABLE stations ADD COLUMN edges TEXT[] NOT NULL DEFAULT '{}';
