-- 139-watchlist-lens.sql — the filer's watchlist declares how it is
-- drawn, and asks for the one thing a list cannot answer.
--
-- Origin (David, 2026-08-15): "I like the 'My Watchlist', and I think
-- we show that more as job cards moving through stations instead of
-- just a static list."
--
-- The watchlist has been a station lens since 118 — `my-watchlist`,
-- predicate `metadata_equals: {submitted_by: "@me"}`, bound per caller.
-- What it could not do is say WHERE each packet had got to, because
-- `GET /api/stations/{name}/queue` serves bare Jobs: the handler
-- fetches steps only when the PREDICATE reads step state, and this
-- predicate reads metadata. So the surface could list the packets a
-- filer sent and their outcome, and nothing about the journey.
--
-- `lens.with_steps` closes that, and is opt-in per station for a
-- reason worth stating: it costs one `list_steps` per member. Most
-- lenses render a row from the Job alone and should not pay it. A lens
-- that stands packets at the stop they reached cannot avoid it —
-- "where is this one" is a fact about its steps.
--
-- Declared on the LENS rather than inferred from the predicate because
-- the two answer different questions: the predicate says who is in the
-- queue, the lens says what the reader needs in order to draw it.

UPDATE stations
   SET lens = '{
         "title": "My watchlist",
         "subtitle": "What you sent, and where it got to",
         "with_steps": true
       }'::jsonb
 WHERE name = 'my-watchlist' AND status = 'active' AND lens IS NULL;
