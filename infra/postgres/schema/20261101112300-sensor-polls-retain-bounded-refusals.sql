-- A111132: schema-only, bounded boot-path extension; no historical rewrite.
SET LOCAL lock_timeout = '2s';
ALTER TABLE sensors ADD COLUMN IF NOT EXISTS refused jsonb;
