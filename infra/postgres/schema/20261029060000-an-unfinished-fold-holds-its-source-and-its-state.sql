-- 20261029060000-an-unfinished-fold-holds-its-source-and-its-state.sql —
-- an unfinished membership fold's storage holds its two parts, not one.
--
-- Origin: backlog 6bc062b1. Live flight ea75c846 (2026-10-29 05:27Z) read
-- the 24 h membership source (44,922 groups, 75,095 rows) and the resumable
-- fold refused after 96 groups: "serialized current-state checkpoint
-- exceeds byte bound". A fold build carries the fixed-cut source it resumes
-- from AND the fold's state; each is held to the 128 MiB checkpoint
-- allowance, and the whole was held to that same 128 MiB. Measured from the
-- window's own rows, the source alone was about 112 MiB, so no admitted
-- 24 h source had room left for the state it folds.
--
-- The whole is now held to the sum of the two allowances,
-- boss_jobs::membership_replay::fold_checkpoint::MAX_FOLD_BUILD_BYTES
-- (2 x 128 MiB = 268435456), carried in MAX_FOLD_CHUNKS (256) chunks of
-- PUBLICATION_BATCH_BYTES. The store test
-- unfinished_fold_transport_constraints_pin_the_existing_rust_bounds_and_rollback
-- holds both CHECKs to those constants.
ALTER TABLE membership_fold_builds DROP CONSTRAINT IF EXISTS membership_fold_builds_byte_count_check;
ALTER TABLE membership_fold_builds ADD CONSTRAINT membership_fold_builds_byte_count_check
    CHECK (byte_count > 0 AND byte_count <= 268435456);
ALTER TABLE membership_fold_build_chunks DROP CONSTRAINT IF EXISTS membership_fold_build_chunks_chunk_check;
ALTER TABLE membership_fold_build_chunks ADD CONSTRAINT membership_fold_build_chunks_chunk_check
    CHECK (chunk >= 0 AND chunk < 256);
