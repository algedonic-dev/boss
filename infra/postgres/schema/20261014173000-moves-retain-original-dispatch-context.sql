-- a4de1e72: a registered dispatch is more specific than a region pair.
-- Keep its original typed context with the derived move. Existing rows
-- remain NULL (unknown); no later registration is backfilled over them.
ALTER TABLE yard_moves ADD COLUMN IF NOT EXISTS dispatch_context TEXT;
