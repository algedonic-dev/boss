-- 20261008085120-the-actor-role-report-states-its-shapes-on-the-log.sql —
-- declare the `actor_role.*` kinds (backlog e0bdba74; row H of design
-- b08725c2's order, the enforce arm of design abf9eeae).
--
-- Every service compares each request's ASSERTED role with the role its
-- registry row holds and tallies what `enforce` would answer
-- differently (design abf9eeae, car 2). Row H earns `enforce` on 72
-- hours with zero would-deny for every registered actor and zero
-- unregistered writers. The tally was process memory, and every train
-- restarts the pod (seven in twelve hours on 2026-10-08), so that
-- window could never be read: `GET /api/jobs/actor-role-reports`
-- answered `durable_window: false` as a constant. It is the defect
-- 20261001141502 fixed for the machine gate and the policy check, and
-- it is fixed the same way, by the same reader
-- (`boss_core::gate_evidence`, `Gate::ActorRole`):
--
--   * `actor_role.recording_began` — service, mode, since, instance:
--     every process start and every mode move, `off` included.
--   * `actor_role.would_refuse` — a shape `enforce` would answer
--     differently, at its FIRST sighting in a process, never one per
--     request ("No per-request events", docs/architecture-decisions.md).
--     Names the service, mode, tally start, the reason (would-deny |
--     would-change-scope | unregistered-writer | unjudged) and the
--     shape: actor id, asserted role, role of record, the door's action
--     and resource names, the lookup status and the two answers. No
--     header, credential, request body or path. A request whose
--     asserted and recorded answers agree states nothing.
--   * `actor_role.tally_overflowed` — the first observation past the
--     tally's bounds (512 shapes a process, 4096 bytes a shape), which
--     names no caller: a window holding one is never clean.
--   * `actor_role.recording_ended` — a process's end, stated at SIGTERM
--     once its queue drained; a watch joins the next process's only
--     across a clean one.
--   * `actor_role.facts_lost` — facts the bounded queue could not take,
--     stated once the log takes writes again; it dirties the window.
--
-- There is no `actor_role.refused`: this precursor refuses nothing, and
-- the car that makes `enforce` refuse declares the kind it then emits.
--
-- Emitted by whichever service the report is mounted in (the event's
-- source is that service's boss-ports name). Declared in the car that
-- first emits them: an emitted-but-undeclared kind is the defect the
-- audit integrity check exists to catch. Held equal to
-- `boss_core::gate_evidence::KINDS` with the rows of 20261001141502 and
-- 20261007122553 by boss-events' `every_gate_evidence_kind_is_registered`.
-- No ref-check rule: a shape references no row — an unregistered actor
-- is exactly an id with none.
INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  ('actor_role.recording_began', 'every service', 'A service''s actor-role report began recording in a mode (off | report): at process start and on every mode move (names the service, the mode, since when and the process instance)', NULL),
  ('actor_role.would_refuse', 'every service', 'A request shape the actor-role report answered as asserted but enforce would answer differently, at its first sighting in this process (names the service, mode, tally start, the reason and the shape: actor, asserted role, role of record, action, resource, lookup status and the two answers)', NULL),
  ('actor_role.tally_overflowed', 'every service', 'The actor-role report''s tally passed its bounds: what follows names no caller (names the service, mode and tally start)', NULL),
  ('actor_role.recording_ended', 'every service', 'A process''s actor-role report stated its end at SIGTERM after its queue drained (names the service, the process instance, facts lost and not yet stated, and whether the end is clean)', NULL),
  ('actor_role.facts_lost', 'every service', 'Facts the actor-role report could not hand to the log (its queue was full), stated once the log took writes again: any of them could have been a would-refuse (names the service, the process instance, the count and since when)', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;
