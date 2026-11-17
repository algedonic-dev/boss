-- 20261018193608-a-lost-candidate-is-rejected-not-a-draft.sql — a
-- workflow row may be `rejected` (backlog 50ded63d).
--
-- Found closing the first live Tier 2 split (protocol-experiment
-- 4aa60dc9, an A/A on agent-run, 2026-10-16). Once an experiment admits
-- a packet to its candidate draft, the discard refuses that draft for
-- good — the packet runs its text (backlog ce8b7d66) — and publish
-- promotes the newest draft and takes no body, so a RETIRE verdict left
-- the kind with a loser publish would put live and a safe verb that
-- refused every later publish. The A/A was promoted to clear it, which
-- a real losing candidate cannot do.
--
-- `rejected` is that draft's end: kept, served to the packets pinned to
-- it, and never promoted (publish selects `status = 'draft'` only). It
-- is set by POST /api/workflows/{kind}/versions/{version}/reject, which
-- records jobs.kind.draft_rejected with the row.
--
-- Idempotent: drop-if-exists then add, the constraint idiom of
-- 20260924123307. The column's CHECK is the auto-named one from
-- 03-jobs.sql; the other registries sharing the Rust status enum
-- (stations, step_plugins, cadence_rules, dispatcher rules) keep their
-- three words.

ALTER TABLE workflows DROP CONSTRAINT IF EXISTS workflows_status_check;
ALTER TABLE workflows ADD CONSTRAINT workflows_status_check
    CHECK (status IN ('draft', 'active', 'retired', 'rejected'));

-- The new event kind, and its sibling the discard has emitted since
-- ebd7bb70 with no row covering it: the emitted-kinds lint reads only
-- the direct emit spellings, and the registry's events reach the outbox
-- through `workflow_registry_event`, so neither was caught at
-- authorship. Declared together so the draft lifecycle reads whole.
INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  ('jobs.kind.draft_rejected',  'jobs', 'A draft Workflow version that met traffic and lost was set rejected: kept for its pinned packets, never published', NULL),
  ('jobs.kind.draft_discarded', 'jobs', 'A draft Workflow version no packet was pinned to was deleted before it went live; its number stays spent', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;
