-- 20260928012919-a-subject-kind-edit-leaves-a-fact.sql — the
-- SubjectKind registry's first write door records what it changed.
--
-- Origin: backlog abc2e9d5. Measured 2026-09-28 at origin/main
-- 3cac3689: crates/core/boss-subject-kinds/src/http.rs routed only
-- GETs, so setting metadata.module on eight kinds for the
-- /it/registry/subjects page reached for a migration UPDATE that no
-- event records — the shape migrations-declare-schema-only.sh refuses
-- for subject_kinds (fa25700f). PATCH /api/subject-kinds/{kind}/metadata
-- is the door instead, and this is its fact.
--
-- ONE FACT PER CHANGE, staged on the outbox in the UPDATE's own
-- transaction (the class.updated shape). None for a patch that restates
-- what the row holds.
--
-- Declared here so the kind does not ride inside a passing
-- audit-integrity run unread (infra/lint/emitted-kinds-are-declared.sh).

INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  ('subject_kind.updated', 'subject-kinds', 'A metadata merge changed one SubjectKind row''s metadata (PATCH /api/subject-kinds/{kind}/metadata: a key replaces, a null deletes): kind, changed (the keys that moved), before and after holding each changed key only on the side where it is present, and updated_by, the actor the request signed with. None for a patch that restates the held row', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;
