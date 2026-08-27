-- 141-design-review-level-sweep.sql — ask the level question on a
-- clock, not only on an edge (packet ae8a14f7).
--
-- `design-review-spawn` (107) asks a LEVEL question — "this doc has
-- open questions and no open review" — but only ever gets asked on an
-- EDGE: `docs.design.indexed`, which by design fires only when a
-- doc's review surface CHANGES. Close a review while its questions
-- are still open and the doc can never get another one, because
-- nothing about it will change again on its own.
--
-- MEASURED. The 2026-08-13 audit closed five reviews on the evidence
-- "questions resolved; tracker pending_count=0; decision history in
-- doc". That evidence was wrong twice over: `pending_count=0` also
-- means nobody ever answered, and payload-encryption's and
-- queue-visibility's Decision-history sections read `_None yet._`.
-- Roughly twenty-three questions across payload-encryption,
-- queue-visibility, workflow-ux-as-data, department-flow-dashboards
-- and dev-cluster became unreachable — not in anyone's queue, with no
-- mechanism that could put them there. Confirmed: a reindex over all
-- 38 docs spawned ZERO reviews, because no doc's surface had changed.
--
-- WHY A SCHEDULED RULE. The alternative considered and rejected was
-- making the reindex emit for unchanged docs: that turns every boot
-- into ~38 events, buys prompt spawning at the cost of a noisy log,
-- and still leaves the level unchecked between boots. This leaves the
-- edge rule exactly as it is — an optimisation that spawns promptly on
-- change — and stops it being the only path.
--
-- WHY THE SAME `jobs.spawn` ARGS AS 107. `docs.design.sweep` decides
-- only WHICH docs are orphaned; it then hands each one to `jobs.spawn`
-- as the payload the edge rule would have delivered. So the shape of a
-- design review lives in registry rows an operator can read, not in
-- handler code, and these args are 107's verbatim. If 107's args
-- change, these must change with them — a fact that lives twice
-- (CLAUDE.md 9a), pinned by
-- boss-dispatcher/tests/design_review_sweep_rules.rs.
--
-- DAILY, on the sim clock: the schedule runner fires on sim-DAY
-- boundaries and already handles catch-up, pause and restart. A doc
-- whose review was closed in error waits at most one sim-day rather
-- than forever.
--
-- IDEMPOTENCE. The sweep skips any doc that already has an open
-- `design-doc-review` — the same question `open_review_exists(path)`
-- answers for 107, asked through the jobs API. A second firing on the
-- same day finds the reviews it created and spawns nothing.
--
-- Rollback is `UPDATE dispatcher_rules SET status = 'retired'` on this
-- name; the edge rule keeps working exactly as before.
--
-- THE WIDER CLASS, worth auditing next: any rule whose `when` reads
-- like a STATE rather than a TRANSITION has this latent. 107's own
-- comment names `open_restock_exists` as the same shape.
INSERT INTO dispatcher_rules (name, version, status, on_event, when_expr, do_steps, delay, schedule_cadence, schedule_anchor, schedule_calendar) VALUES
  ('design-review-level-sweep', 1, 'active', NULL,
   NULL,
   '[{"handler":"docs.design.sweep","args":{"kind":"\"design-doc-review\"","subject_kind":"\"custom\"","subject":"path","title":"title","metadata.doc_path":"path","metadata.doc_title":"title"}}]'::jsonb,
   NULL, 'daily', DATE '2026-08-16', NULL)
ON CONFLICT (name, version) DO NOTHING;
