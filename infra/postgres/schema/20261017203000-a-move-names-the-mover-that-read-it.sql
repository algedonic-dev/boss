-- 20261017203000-a-move-names-the-mover-that-read-it.sql
-- — each recorded move names the mover version that read it (backlog
-- c8d22e63, decided by David 2026-10-17).
--
-- WHY. A row keeps the from/to its mover read, and the undeclared band
-- re-judges every row in its 24h window against the routes derived now.
-- When train #726 fixed the mover (backlog 47880c86, 2026-09-27) the old
-- mover's misreadings (shed -> arrivals x48, off the map -> garage x1)
-- kept arrivals and the garage TROUBLED for up to a day after the fix
-- had landed, and an urgent item was filed on that residue. The mover
-- now writes `boss_jobs::moves::mover_version()` — a content address of
-- the source that decides placement — on every row, and the band counts
-- only the rows the CURRENT version wrote, so the deploy that carries a
-- mover fix clears what its predecessor misread.
--
-- NULL on every row written before this column existed: unknown, never
-- backfilled with a guessed version. Nothing here rewrites or deletes a
-- row; a superseded row stays in the record and on `/api/yard/moves`,
-- and only stops troubling a region.

ALTER TABLE yard_moves ADD COLUMN IF NOT EXISTS mover_version TEXT;

COMMENT ON COLUMN yard_moves.mover_version IS
  'The content-addressed version of the mover that recorded this row '
  '(boss_jobs::moves::mover_version). NULL on rows written before the '
  'stamp existed. The undeclared band counts only the current version''s '
  'rows. Backlog c8d22e63.';
