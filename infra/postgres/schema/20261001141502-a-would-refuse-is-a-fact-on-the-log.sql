-- 20261001141502-a-would-refuse-is-a-fact-on-the-log.sql — the two
-- refusing gates state what they would refuse on the log, so their
-- clean windows are read from the log and not from a process (design
-- 21946380, backlog b0787727; amends rows C and D of design b08725c2).
--
-- The machine gate (every service port) and the policy check's mode
-- kept their evidence in process memory, and every train restarts the
-- pod, so no tally was ever older than about two hours and the 72-hour
-- window the enforce flips ask for could never close. Each gate now
-- states, through its service's outbox:
--
--   * `<gate>.recording_began` — service, mode, since: every process
--     start and every mode move, `off` included.
--   * `<gate>.would_refuse` / `<gate>.refused` — service, mode,
--     recording_since and the tally key: a key's FIRST sighting in a
--     process, never one per request (Q2 of the design, ratified beside
--     "No per-request events" in docs/architecture-decisions.md).
--   * `<gate>.tally_overflowed` — the first sighting past a key cap,
--     which names no caller.
--   * `<gate>.recording_ended` — instance, lost, unstated, clean: a
--     process's end, stated at SIGTERM once its queue drained. A watch
--     joins the next process's only across a CLEAN end, so a process
--     that died holding facts the log never took breaks it (review
--     e4417d48, B1).
--   * `<gate>.facts_lost` — count, since: facts the bounded queue could
--     not take, stated once the log takes writes again; it dirties the
--     window. Every fact also carries its process `instance`.
--   * `machine_gate.previous_presented` — a key presenting the
--     `previous` token slot, which every mode admits: what a rotation's
--     revoke drains on, and no would-refuse.
--   * `machine_gate.reader_presented` — a key presenting the
--     probe-reader credential (design b35c22b4): its reads are admitted
--     and its writes refused in EVERY mode, so `enforce` changes nothing
--     for it and it ends no clean window; on the log for the reader's
--     own rotation, as `previous` is for the estate token's.
--
-- The machine gate's facts are emitted by whichever service the gate is
-- mounted in (the event's source is that service's boss-ports name);
-- the policy check's by `policy`. Declared in the car that first emits
-- them: an emitted-but-undeclared kind is the defect class the audit
-- integrity check exists to catch. Held equal to
-- `boss_core::gate_evidence::KINDS` by boss-events'
-- `every_gate_evidence_kind_is_registered`. No ref-check rules: a
-- caller shape references no row.
INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  ('machine_gate.recording_began', 'every gated service', 'A service''s machine gate began recording in a mode (off | report | enforce): at process start and on every mode move (names the service, the mode, since when and the process instance)', NULL),
  ('machine_gate.would_refuse', 'every gated service', 'A caller shape the machine gate admitted but enforce would refuse, at its first sighting in this process (names the service, mode, tally start and key: peer, asserted user, method, route, presented)', NULL),
  ('machine_gate.refused', 'every gated service', 'A caller shape the machine gate refused in enforce, at its first sighting in this process (names the service, mode, tally start and key)', NULL),
  ('machine_gate.tally_overflowed', 'every gated service', 'The machine gate''s tally, or one source''s share of it, filled: what follows names no caller (names the service, mode, tally start and scope: tally | source)', NULL),
  ('machine_gate.recording_ended', 'every gated service', 'A process''s machine gate stated its end at SIGTERM after its queue drained (names the service, the process instance, facts lost and not yet stated, and whether the end is clean)', NULL),
  ('machine_gate.facts_lost', 'every gated service', 'Facts the machine gate could not hand to the log (its queue was full), stated once the log took writes again: any of them could have been a would-refuse (names the service, the process instance, the count and since when)', NULL),
  ('machine_gate.previous_presented', 'every gated service', 'A caller shape presented the previous machine-token slot, at its first sighting in this process — admitted in every mode; what a rotation''s revoke drains on (names the service, mode, tally start and key)', NULL),
  ('machine_gate.reader_presented', 'every gated service', 'A caller shape presented the probe-reader credential (reader.current | reader.next | reader.previous), at its first sighting in this process or past its source''s share — reads admitted and writes refused in every mode, so never a would-refuse (names the service, mode, tally start and key, a refused write keyed as (refused write))', NULL),
  ('policy.check.recording_began', 'policy', 'The policy check''s mode began recording (off | report | enforce): at process start and on every mode move (names the mode, since when and the process instance)', NULL),
  ('policy.check.would_refuse', 'policy', 'A caller shape the policy check answered but enforce would refuse, at its first sighting in this process (names the mode, tally start and key: arm, caller, role, peer)', NULL),
  ('policy.check.refused', 'policy', 'A caller shape the policy check refused in enforce, at its first sighting in this process (names the mode, tally start and key)', NULL),
  ('policy.check.tally_overflowed', 'policy', 'The policy check''s refusal tally filled: what follows names no caller (names the mode and tally start)', NULL),
  ('policy.check.recording_ended', 'policy', 'The policy check''s process stated its end at SIGTERM after its queue drained (names the process instance, facts lost and not yet stated, and whether the end is clean)', NULL),
  ('policy.check.facts_lost', 'policy', 'Facts the policy check could not hand to the log (its queue was full), stated once the log took writes again: any of them could have been a would-refuse (names the process instance, the count and since when)', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;
