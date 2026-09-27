-- 20260927011205-editing-or-retiring-a-class-leaves-a-fact.sql — the
-- Class registry's edit and retire doors record what they changed.
--
-- Origin: backlog 10dabe13. Measured 2026-09-27 at origin/main
-- 6d14407c: only POST /api/classes/batch staged a fact
-- (class.declared, migration 20260917071313). PUT
-- /api/classes/{kind}/{code} and POST .../retire each ran a bare
-- UPDATE with no outbox write, so the three rows `boss tenant publish
-- --take classes` edited through the PUT door at ~23:00Z 2026-09-26
-- left 0 class events in the log, and no rebuilder could reproduce
-- the edited labels or a retirement.
--
-- ONE FACT PER CHANGE, staged on the outbox in the UPDATE's own
-- transaction (the class.declared shape). class.updated only when the
-- edit changed the editable body — a PUT restating the held row writes
-- nothing and records nothing; class.retired only for the call that
-- set retired_at — the idempotent repeat moved nothing.
--
-- Declared here so the kinds do not ride inside a passing
-- audit-integrity run unread (infra/lint/emitted-kinds-are-declared.sh).

INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  ('class.updated', 'classes', 'An edit changed one Class row''s editable body (PUT /api/classes/{kind}/{code}, the door boss tenant publish --take classes edits through): the key, changed (the field names), before and after holding only those fields, and updated_by, the actor the request signed with. None for a PUT that restates the held row', NULL),
  ('class.retired', 'classes', 'A retire withdrew one Class from active use (POST /api/classes/{kind}/{code}/retire): the key, the retired_at the row now holds, and retired_by, the actor the request signed with. None for the idempotent repeat, whose stamp did not move', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;
