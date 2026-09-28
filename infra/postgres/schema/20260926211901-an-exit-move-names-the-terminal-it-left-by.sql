-- 20260926211901-an-exit-move-names-the-terminal-it-left-by.sql
-- — the moves record names the off-ramp an exit took (backlog e23005c0).
--
-- WHY. A row with `to_region IS NULL` said a packet LEFT the map and
-- from where, never how. Drawing design exhibit f0313eda (2026-09-26,
-- 1105 moves) meant reading every exiting packet's outcome step by hand
-- to learn that 217 left marshalling by ops-request `answered` — a
-- terminal no route named — and every per-terminal count on the map
-- needed the same reads again. The mover now reads the packet back when
-- it records an exit and writes the slug of the terminal it closed on.
--
-- NULL on a move between regions, on every row written before this
-- column existed, and on an exit whose packet the mover could not read
-- back: unknown, judged by its route alone — never a guessed terminal.
-- A closed packet does not reopen, so the value a replay reads back is
-- the value the first recording wrote.

ALTER TABLE yard_moves ADD COLUMN IF NOT EXISTS terminal TEXT;

COMMENT ON COLUMN yard_moves.terminal IS
  'For an exit (to_region NULL): the slug of the terminal step the packet '
  'closed on. NULL on a move between regions and on an exit the mover '
  'could not read back. Backlog e23005c0.';
