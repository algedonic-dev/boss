-- 20261018020000-an-automation-row-records-its-read-tier.sql — the
-- read authority recorded on an automation's own registry row.
--
-- Origin: backlog f21804d8, design 600c07d7 (decided 2026-10-17). The
-- movement observer (automation:yard-mover) must qualify for the
-- complete native graph with explicit, recorded, read-only authority:
-- the machinery reads ask an Operator-or-Auditor tier
-- (boss_jobs::trust::can_read), and this table recorded a role and no
-- tier. The only tier rule there was infers Operator for agent-shaped
-- ids from a role, which the design refuses for an automation — a role
-- is not a tier.
--
-- NULL for every row: adding the column grants no one anything, and
-- nothing infers a value. 'auditor' — the existing read-only machinery
-- tier, which admits the operator doors' reads and none of their
-- writes — is the only value a row may record; there is deliberately no
-- 'operator'. The CHECK is the SQL spelling of
-- boss_jobs::agents::automations::RecordedTier, and the adapter refuses
-- a value it cannot name rather than reading it as no tier.
--
-- SCHEMA ONLY, like the table: no migration may record a tier
-- (migrations-declare-schema-only.sh lists automation_actors). A row
-- arrives with one only through the platform bundle's insert-if-absent,
-- which never stamps a tier over a held row nor revokes one, and whose
-- automation.declared fact carries the tier it inserted.

ALTER TABLE automation_actors
    ADD COLUMN IF NOT EXISTS access_tier TEXT CHECK (access_tier IN ('auditor'));

COMMENT ON COLUMN automation_actors.access_tier IS
  'The read authority recorded for this exact id (backlog f21804d8, '
  'design 600c07d7): auditor, or NULL. Never inferred from role; a '
  'family member of a signs_for prefix never inherits it.';

UPDATE event_kinds
   SET description = 'The platform seed inserted one automation_actors row from the bundle infra/platform/automations/ (insert-if-absent): the row as declared — id, role, description, signs_for, and access_tier when the row records one — plus declared_by, the seed''s actor. One per inserted row, none for a row the registry already held, none per run'
 WHERE kind_pattern = 'automation.declared';
