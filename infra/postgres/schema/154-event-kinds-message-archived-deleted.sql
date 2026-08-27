-- 154-event-kinds-message-archived-deleted.sql — the rest of the
-- inbox write vocabulary joins the registry (packet be4cd28f).
--
-- `boss-messages` emits four kinds; 108-event-kinds.sql declared two
-- of them (`sent`, `read`, harvested from the live log) and missed the
-- two the audit-integrity checker now names on every run:
--
--   messages.message.archived — emitted by PgMessageRepo::archive and
--     by the entity-past-relevancy sweep. Payload {id, archived_at},
--     plus {reason} on the sweep's rows.
--   messages.message.deleted — emitted by PgMessageRepo::delete.
--     Payload {id, deleted_at}.
--
-- The packet named `archived` alone; `deleted` is the same hole in the
-- same family, found while verifying, and leaving it would have kept
-- `unregistered_kinds` warning after the fix it asked for.
--
-- Rows, not a `messages.message.*` pattern. A pattern's suffix domain
-- has to be a registry that owns the values (108's `step.*` families
-- defer to `step_types`); the message verbs are a closed set written
-- in Rust with no registry behind them, so a pattern would declare a
-- vocabulary nothing bounds and silence future drift instead of
-- surfacing it. The two siblings already sit here as rows.
--
-- No ref-check rules: the payload names a `messages` projection row,
-- which the rebuilder owns, not an entity 43-event-facts.sql tracks.
INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  ('messages.message.archived', 'messages', 'An inbox message was archived (by its recipient, or by the past-relevancy sweep)', NULL),
  ('messages.message.deleted',  'messages', 'An inbox message was deleted — the rebuilder drops the projection row', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;
