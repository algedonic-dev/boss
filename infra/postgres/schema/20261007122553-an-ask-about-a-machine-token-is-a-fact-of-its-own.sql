-- 20261007122553-an-ask-about-a-machine-token-is-a-fact-of-its-own.sql —
-- declare `machine_gate.asked` (backlog 89fc511e; review 0c0e3f01, B1).
--
-- Every gated service answers `GET /api/machine-gate/accepts` with the
-- slot a presented value matches. A credential rotation asks every gate
-- about the value it has just staged, about a minute before kubelet
-- projects the Secret, and is answered `none`. That ask used to be
-- tallied as a caller presenting a mismatch and stated as
-- `machine_gate.would_refuse`, so one rotation of the estate token or of
-- the probe reader ended the 72-hour clean window on every gated port
-- (review f09db7f0, F3) — the window row C of the enforce order (design
-- b08725c2) waits on.
--
-- An ask is now a fact of its own: one non-empty value no slot holds, on
-- a GET of that route, is tallied as `asked` and stated once per caller
-- shape per process as `machine_gate.asked`. It is named, so that a
-- value tried there — by a rotation, or by anyone holding a stale one —
-- is on the record like a try on any other route; and it is no
-- would-refuse, because an ask is answered `none` whatever the mode.
-- `boss_core::gate_evidence::Fact::dirties` does not count it.
--
-- Declared in the car that first emits it: an emitted-but-undeclared
-- kind is the defect the audit integrity check exists to catch. Held
-- equal to `boss_core::gate_evidence::KINDS` with the rows of
-- 20261001141502 by boss-events' `every_gate_evidence_kind_is_registered`.
-- No ref-check rule: a caller shape references no row.
INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  ('machine_gate.asked', 'every gated service', 'A caller shape asked the machine gate''s accepts route about one value no slot holds, at its first sighting in this process or past its source''s share — answered none in report and 401 in enforce, named, and never a would-refuse (names the service, mode, tally start and key: peer, asserted user, method, route, presented asked)', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;
