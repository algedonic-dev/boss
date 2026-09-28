-- 20260927040222-a-recognized-cogs-fact-is-recorded.sql — the COGS
-- endpoint stages a ledger event, and a rebuild projects it back.
--
-- Origin: backlog de926d87, found by the design 3036296f mechanism A
-- pin. Measured 2026-09-27 at origin/main 548019cb: POST
-- /api/ledger/cogs-recognized (http/facts.rs cogs_recognized_handler)
-- wrote a `finance.cogs.recognized` fact and its journal entry and
-- staged NO event, where its sibling fact handlers (manual entry,
-- inventory transferred / capitalized) each stage one in the same
-- transaction. financial_facts is a projection of audit_log
-- (rebuild_facts TRUNCATEs it and replays), so every COGS entry that
-- endpoint posted vanished on the next rebuild.
--
-- The handler now stages `ledger.cogs.recognized` with the fact's own
-- payload, which carries source_table, source_id, happened_on and
-- created_by, gated on the call having inserted the fact (an
-- idempotent repeat records nothing). This rule projects it back:
-- the payload's source_table overrides the rule's fixed label
-- (rebuild_facts::project_event), and created_by is read through
-- `/created_by` because the endpoint lets its caller name the author.
-- `ledger_cogs_recognized` is a provenance label only, like
-- 'manual_inventory_transferred' — written verbatim, never joined.
--
-- No conflict target: 20260917173259 replaced the event_kind primary
-- key with an expression index on (event_kind, when_filter), which an
-- `ON CONFLICT (event_kind)` can no longer name.
INSERT INTO gl_fact_projection_rules (event_kind, fact_kind, source_table, source_id_path, happened_on_path, created_by_path) VALUES
    ('ledger.cogs.recognized', 'finance.cogs.recognized', 'ledger_cogs_recognized', '/source_id', '/happened_on', '/created_by')
ON CONFLICT DO NOTHING;

-- Declared so the kind does not ride inside a passing audit-integrity
-- run unread (infra/lint/emitted-kinds-are-declared.sh).
INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  ('ledger.cogs.recognized', 'ledger', 'Cost of goods recognized through POST /api/ledger/cogs-recognized (DR 5100 COGS / CR 1300 inventory by default): the finance.cogs.recognized fact payload verbatim, with source_table, source_id, happened_on and created_by. None for an idempotent repeat of the same source', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;
