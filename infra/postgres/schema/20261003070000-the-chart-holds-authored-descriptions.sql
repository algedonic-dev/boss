-- Tax account descriptions are tenant declarations, not code-derived prose
-- (c8b71886). Existing charts remain unknown; no historical text is invented.
ALTER TABLE gl_accounts ADD COLUMN IF NOT EXISTS description TEXT;
