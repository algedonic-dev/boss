-- 20261110225917-the-membership-replay-is-deleted.sql — the windowed
-- packet-membership replay's tables, dropped with the code that wrote
-- and read them (design 7b8ed079 car 2, backlog 3db0db44).
--
-- THE DECISION. Station membership is read, not replayed: a station's
-- members are its queue read, and its flow is a windowed count over the
-- step record's own timestamps (car 1, live 2026-11-10 22:42Z). Every
-- projection these tables held had been stale since 2026-10-30 (urgent
-- b3572a26), and the 48-hour window could not be built at all at
-- current volume (15361087). Car 2 deletes the replay, its observation
-- doors and its forge reader; nothing in the tree reads or writes these
-- tables any longer.
--
-- NO HISTORY IS LOST. Every row here was a disposable derivation
-- rebuilt from audit_log (the CREATE files call them that themselves);
-- audit_log stays the system of record and is not touched.
--
-- THE LOCK. Each DROP takes ACCESS EXCLUSIVE on its table, inside
-- migrate.sh's per-file transaction under its 5 s lock_timeout, on
-- boss-init's boot path. boss deploys with a Recreate rollout
-- (infra/cluster/manifests/boss.yaml), so the jobs API that held these
-- tables is stopped before boss-init runs and no session waits on them;
-- a lock that could not be had would roll the file back and fail the
-- boot loudly, never half-drop.
--
-- The CREATE files stay: migrations are append-only history
-- (migrations-append-only.sh), and a fresh database applies them and
-- then this drop. Children go before their parents; each table's
-- triggers go with it, and the trigger functions after them.
DROP TABLE IF EXISTS membership_projection_rows;
DROP TABLE IF EXISTS membership_projection_generations;
DROP TABLE IF EXISTS membership_fold_build_chunks;
DROP TABLE IF EXISTS membership_fold_builds;
DROP TABLE IF EXISTS membership_row_nodes;
DROP TABLE IF EXISTS membership_row_epochs;
DROP TABLE IF EXISTS membership_seed_entries;
DROP TABLE IF EXISTS membership_seed_builds;
DROP FUNCTION IF EXISTS invalidate_membership_row_basis();
DROP FUNCTION IF EXISTS invalidate_membership_header_basis();
DROP FUNCTION IF EXISTS membership_row_header_invalidate();
DROP FUNCTION IF EXISTS membership_row_nodes_invalidate();
