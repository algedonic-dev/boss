-- 127-initiated-by-free-form.sql — an agent can be recorded as having
-- initiated an employee change.
--
-- David, 2026-08-14, answering approval 7b466bb7: "Drop the FK now,
-- actors table later. The free-form id matches what ActorId already
-- is, and an actors table is a real registry that deserves its own
-- design rather than being introduced as a side effect of unblocking
-- one column."
--
-- THE MEASUREMENT (backlog item afe54132, open since 2026-08-07):
--
--   POST /api/people/emp-aa-405/changes  initiated_by=automation:claude
--     -> 422  employee_changes_initiated_by_fkey
--
-- `initiated_by` was `TEXT REFERENCES employees(id)`, so only a human
-- could be named. An agent is an executor by design — `ActorId::
-- Automation` exists for exactly that — and a column that can only
-- name a person forces either an OMISSION (the change has no actor) or
-- an IMPERSONATION (an agent's work filed under someone's name). The
-- row that prompted this was written with `initiated_by` left null and
-- the actor named in free-text notes, which is the omission.
--
-- Impersonation is the thing this system has spent real effort
-- removing: `boss-jobs/src/http/steps.rs` is explicit that an agent
-- "IS the CPU that did the work" and that redirecting its attribution
-- to a person would erase exactly what the `<mode>:<model>` actor id
-- exists to record. A schema that makes the honest answer impossible
-- is the one to change.
--
-- WHAT REPLACES THE CONSTRAINT. Nothing, deliberately, and it is worth
-- being clear-eyed: dropping this loses referential integrity on the
-- column. A typo'd `initiated_by` will now be stored rather than
-- refused. That is the accepted cost of the "actors table later" half
-- of David's answer — the real fix is one registry both employees and
-- automations resolve against, which is its own design (identity,
-- lifecycle, who may write a row) and not something to introduce as a
-- side effect here.
--
-- The column keeps its shape and its data. Every existing value is an
-- employee id and stays valid; this only widens what may be written.
ALTER TABLE employee_changes
    DROP CONSTRAINT IF EXISTS employee_changes_initiated_by_fkey;

COMMENT ON COLUMN employee_changes.initiated_by IS
    'Actor id that initiated the change — an employee id, or an ActorId slug such as automation:claude. Free-form since migration 127: agents are executors and a person-only column forced an omission or an impersonation. An actors registry both kinds resolve against is the intended end state (approval 7b466bb7).';
