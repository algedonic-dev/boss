-- 107-dispatcher-rule-design-review-spawn.sql — reviews create
-- themselves (dogfooding arc e556c000, S2).
--
-- David, 2026-08-10: "If design review workflows aren't being
-- automatically sequenced using dispatcher or other, we aren't
-- really dogfooding our own software yet." Every review Job to date
-- was opened by hand — including one opened before its doc existed.
--
-- On `docs.design.indexed` (S1: emitted through the outbox when a
-- doc's review surface CHANGES — title/status/question counts; the
-- every-boot reindex of an unchanged doc emits nothing) with open
-- questions and no open review for the path, spawn the
-- design-doc-review Job. The dispatcher then assigns its review step
-- and messages the assignee — the whole intake loop machine-owned,
-- with the human entering exactly where judgment is wanted.
--
-- `when` binds FLAT payload identifiers (`open_questions`, `path` —
-- both top-level in the S1 payload; the binder resolves no dotted
-- paths). `open_review_exists` is the dedup: indexed re-fires on
-- every question-count change, and each firing must not open another
-- review (same shape as the restock rule's `open_restock_exists`).
INSERT INTO dispatcher_rules (name, version, status, on_event, when_expr, do_steps, delay, schedule_cadence, schedule_anchor, schedule_calendar) VALUES
  ('design-review-spawn', 1, 'active', 'docs.design.indexed',
   'open_questions > 0 AND NOT open_review_exists(path)',
   '[{"handler":"jobs.spawn","args":{"kind":"\"design-doc-review\"","subject_kind":"\"custom\"","subject":"path","title":"title","metadata.doc_path":"path","metadata.doc_title":"title"}}]'::jsonb,
   NULL, NULL, NULL, NULL)
ON CONFLICT (name, version) DO NOTHING;
