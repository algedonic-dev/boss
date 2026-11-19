-- 20261019213541-a-discarded-draft-is-a-declared-kind.sql — the draft
-- discard's event kind gets its event_kinds row (backlog 7d05ded2).
--
-- `jobs.kind.draft_discarded` (boss-jobs events.rs WORKFLOW_DRAFT_DISCARDED)
-- has been emitted since ebd7bb70 with no row covering it, and
-- infra/lint/emitted-kinds-are-declared.sh stayed green: the registry's
-- events reach the outbox through the helper
-- `events::workflow_registry_event(kind, …)`, a call shape the lint did
-- not read. The same car teaches the lint to derive such helpers, and
-- this row is the one offender that lint then names on origin/main
-- 299cc5c81. The tuple matches the one car
-- fix/a-retired-candidate-draft-is-rejected-not-blocking carries for the
-- same kind, so whichever lands second is a no-op.

INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  ('jobs.kind.draft_discarded', 'jobs', 'A draft Workflow version no packet was pinned to was deleted before it went live; its number stays spent', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;
