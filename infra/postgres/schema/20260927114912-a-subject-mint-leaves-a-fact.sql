-- 20260927114912-a-subject-mint-leaves-a-fact.sql — the subject mint
-- door records what it wrote.
--
-- Origin: backlog 92473357, found by the mechanism A pin of design
-- 3036296f (crates/core/boss-events/writes-without-a-fact.txt named
-- subjects.rs::publish_subject and ::upsert_subject as `gap`).
-- POST /api/subjects and POST /api/subjects/{kind} — the door the
-- company identity comes through at every tenant publish, and the
-- sim's birth routes — wrote `subjects` on the pool and staged no
-- event, while rebuild_subjects TRUNCATEs the table and replays it
-- from audit_log. So every identity the door minted was gone after
-- the first rebuild, and a take-mode relabel reverted.
--
-- publish_subject now stages ONE fact in the write's own transaction:
-- `subjects.subject.minted` on an insert, `subjects.subject.relabelled`
-- when ?mode=take replaces a held label. A kept or unchanged
-- declaration moves nothing and stages nothing. rebuild_subjects
-- replays both kinds. The pool-level upsert_subject had no caller
-- outside its own tests and was deleted rather than given a fact.
--
-- Declared here so the kinds do not ride inside a passing
-- audit-integrity run unread (infra/lint/emitted-kinds-are-declared.sh).

INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  ('subjects.subject.minted', 'subjects', 'A subject identity was minted through the mint door (POST /api/subjects, POST /api/subjects/{kind}): subject_kind, id and the label it was minted with (null when none), signed by the caller. rebuild_subjects replays it.', NULL),
  ('subjects.subject.relabelled', 'subjects', 'A held subject''s label was replaced through the mint door under ?mode=take: subject_kind, id, from (the label it held) and label, signed by the caller. rebuild_subjects replays it.', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;
