-- 137-job-closed-payload-roster.sql — declare what `jobs.job.closed`
-- carries, so a rule that binds a field it does not can be caught before
-- it fires.
--
-- THE FAILURE (filed cf7ae3b5). A dispatcher rule can name an identifier
-- the event has never carried, and nothing catches it until the rule
-- fires in production. There it is not a quiet skip: the evaluator
-- returns UnknownIdentifier, the runner turns that into PredicateFailed
-- / ArgFailed and NAKs, and the event dead-letters after eight attempts.
-- `spawn-car-on-sweep-remediated` bound `title` against this exact topic
-- and produced eight WARN redeliveries seconds after a train merged.
--
-- WHY THIS COLUMN, AND WHY IT WAS EMPTY. `event_kinds.payload_fields`
-- has existed since 108, whose own comment says it is a "flat field
-- inventory ... filled as consumers (encryption classification, rule
-- authoring) need it". Rule authoring is now a consumer, and the column
-- was still `[]` on every one of its rows. This is the first roster.
--
-- A RATCHET, NOT AN INVENTORY. `payload_contract::unresolved_identifiers`
-- does not check a kind whose roster is empty, so declaring ONE topic
-- gates that topic's rules without requiring a complete census of every
-- event in the system first. A check that has to be total before it is
-- useful never lands. `jobs.job.closed` goes first because it is the
-- most-bound topic in the rule set (six rules) and the one that actually
-- dead-lettered.
--
-- FLAT ON PURPOSE. Only the ROOT segment of an identifier is a payload
-- key — `metadata.subworkflow` traverses into the value of `metadata` —
-- so declaring nested shape would buy nothing the check can use.
--
-- The roster is the union of the three emit sites
-- (boss-jobs/src/http/steps.rs and jobs.rs). Note `parent_step_id` is
-- declared here but is NOT emitted by one of those three sites; that
-- divergence is filed separately as da87e3a1 and is a different defect
-- class — an emit site failing to populate a declared field, rather than
-- a rule naming an undeclared one. Declaring it here is still correct:
-- the roster states what the topic's contract IS, and a live rule
-- (`resolve-subjob-on-child-job-closed`) already gates on it.
UPDATE event_kinds
   SET payload_fields = '[
         {"name": "id",             "type": "uuid",   "note": "the closing Job"},
         {"name": "closed_on",      "type": "date",   "note": "null until a terminal stamps it"},
         {"name": "kind",           "type": "string", "note": "workflow kind"},
         {"name": "outcome",        "type": "string", "note": "null on a catch-all close"},
         {"name": "title",          "type": "string", "note": "added 2026-08-15 for spawn-car-on-sweep-remediated"},
         {"name": "parent_step_id", "type": "uuid",   "note": "set when the Job is a delegated subjob; see da87e3a1"}
       ]'::jsonb
 WHERE kind_pattern = 'jobs.job.closed';
