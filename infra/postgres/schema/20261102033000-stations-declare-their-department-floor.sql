-- Optional presentation declarations; legacy stations remain unassigned.
ALTER TABLE stations ADD COLUMN department TEXT;
ALTER TABLE stations ADD COLUMN floor JSONB;
