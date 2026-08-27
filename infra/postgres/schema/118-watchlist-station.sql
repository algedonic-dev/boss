-- 118-watchlist-station.sql — per-actor stations, and the first one:
-- the filer's watchlist.
--
-- Origin (David, 2026-08-13): "Once the user feedback results in
-- either a shipped change or some other terminal state, it can be
-- closed without the filer approving. But, we should always notify the
-- filer with the terminal state and it should show in their
-- watchlist."
--
-- Two things had to give for a watchlist to be a station row rather
-- than a bespoke endpoint, and both land here as DATA:
--
--   1. A watchlist is PER-ACTOR, and predicates are static registry
--      data. The predicate shape gains a self placeholder — the
--      literal "@me" in a value position — which the evaluator binds
--      to the requesting actor before any packet is compared
--      (boss-jobs src/station_queue.rs `SELF` / `bind_self`). So
--      "my watchlist" is ONE row every actor can query, not one row
--      per employee that goes stale at every hire. The same
--      placeholder is what lets the taxonomy's "every executor has an
--      actor station" be data instead of a generator.
--
--      An unbindable placeholder (a guest — nobody to bind to) yields
--      an EMPTY queue, never a wide one.
--
--   2. A station's universe was open packets, because stations hold
--      in-flight traffic. A watchlist whose entries vanish at closure
--      is useless at exactly the moment it matters: the terminal state
--      IS the information the filer came for. `terminal_window_days`
--      keeps departed packets visible for N days after `closed_on`,
--      then ages them out. It is a RETENTION rule, not a membership
--      one, which is why it sits on the station row beside `discipline`
--      and `wip_limit` rather than inside the predicate — and why the
--      predicate stays clockless.

ALTER TABLE stations
    ADD COLUMN IF NOT EXISTS terminal_window_days INT;

COMMENT ON COLUMN stations.terminal_window_days IS
  'How long a terminal (closed/cancelled) packet stays in this station''s '
  'queue, counted from closed_on. NULL = in-flight packets only, which is '
  'every routing/holding station: a station that moves traffic has nothing '
  'to say about traffic that already left.';

COMMENT ON COLUMN stations.predicate IS
  'Queue-membership predicate over packets — a conjunction of optional '
  'clauses (boss-jobs src/station_queue.rs StationPredicate): kind, status, '
  'tags_any, metadata_present, metadata_absent, metadata_equals, '
  'step{slug,kind,status_in,assignee_id}. A metadata_equals value or a '
  'step.assignee_id of "@me" is the self placeholder, bound to the '
  'requesting actor at read time.';


-- The one actor station that ships as PLATFORM data.
--
-- 116-stations.sql seeded only batch stations and said actor stations
-- are tenant data, because an actor station meant a row per person and
-- the roster is a tenant's business. The self placeholder is exactly
-- what retires that constraint: one row, every actor, no roster
-- knowledge. Per-employee actor stations remain tenant data if a
-- tenant wants them.
--
-- Predicate: packets that recorded who filed them. Deliberately NOT
-- narrowed to `user-feedback` — the clause is "this packet names me as
-- its filer", and any surface that records a filer the same way earns
-- a place on that person's watchlist without a schema change here.
--
-- Discipline `recency` (newest activity first — closed_on when the
-- packet closed, else opened_on) rather than the ratified
-- `priority, age` default: a watchlist is READ, not pulled from. The
-- reader is asking "what became of my packets", and the answer is
-- whatever moved most recently. Priority ordering would bury a packet
-- that just closed under an emergency that has sat open for a week.
--
-- Window of 14 days: long enough that someone who files on a Friday
-- and reads their watchlist after a fortnight's leave still sees the
-- outcome; short enough that the list stays a list.
INSERT INTO stations (name, version, status, title, kind, predicate, discipline,
                      wip_limit, terminal_window_days, capability, rollup_parent) VALUES
  ('my-watchlist', 1, 'active', 'My watchlist — packets I filed', 'actor',
   '{"metadata_equals": {"submitted_by": "@me"}}'::jsonb,
   '["recency"]'::jsonb, NULL, 14, NULL, NULL)
ON CONFLICT (name, version) DO NOTHING;


-- The push-down the watchlist reads through: `metadata @> '{...}'` is
-- how a bound metadata_equals clause narrows in SQL instead of after
-- the page is drawn. Without it a per-actor station would page through
-- the newest MAX_LIMIT packets of the whole company and find few of
-- the caller's own.
CREATE INDEX IF NOT EXISTS jobs_metadata_gin ON jobs USING GIN (metadata jsonb_path_ops);
