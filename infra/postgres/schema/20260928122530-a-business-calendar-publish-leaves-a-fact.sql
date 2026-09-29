-- 20260928122530-a-business-calendar-publish-leaves-a-fact.sql — the
-- business-calendar batch records what it wrote, and who.
--
-- Origin: backlog 05f61acf, an adversarial review of car d89bafeb
-- (2026-09-28). POST /api/calendar/business-calendars/batch asked
-- policy through the registry-write ladder and then discarded the
-- actor the ladder resolved, and PgCalendar::publish_business_calendars
-- staged no event (named `gap` in
-- crates/core/boss-events/writes-without-a-fact.txt, backlog 06590554).
-- So a ?mode=take that replaced a held calendar's closed-day set — the
-- days the dispatcher's timing triggers count business days from —
-- left no row, event or log line naming who did it.
--
-- Each code the batch CHANGES now stages ONE fact in the write's own
-- transaction, signed by that actor; a kept code, an identical
-- declaration and a take that restates the held row write nothing and
-- record nothing. The kinds are named for the policy resource
-- (`business-calendar`), outside `calendar.reservation.%`, which the
-- reservation rebuilder reads.
--
-- Declared here so the kinds do not ride inside a passing
-- audit-integrity run unread (infra/lint/emitted-kinds-are-declared.sh).

INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  ('business-calendar.declared', 'calendar', 'A business-calendar batch inserted one calendar (POST /api/calendar/business-calendars/batch): the row as written — code, name, weekend, closed — plus mode and declared_by, the actor the request signed with. One per inserted code, none for a kept one, none per batch', NULL),
  ('business-calendar.updated', 'calendar', 'A business-calendar batch under ?mode=take replaced one held calendar: the row as written — code, name, weekend, closed — plus mode, changes (each field from → to, so the replaced closed-day set is in the log) and updated_by, the actor the request signed with. None for a take that restates the held row', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;
