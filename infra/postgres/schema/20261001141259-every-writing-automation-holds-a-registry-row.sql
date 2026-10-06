-- 20261001141259-every-writing-automation-holds-a-registry-row.sql —
-- the automation half of the agents registry: one row for every
-- `automation:*` id that writes, carrying the role it signs as today.
--
-- Origin: backlog ddf0773e, design abf9eeae (decided 2026-10-01), car
-- 1 of 5. Every machine caller asserts platform-admin in its own
-- x-boss-user and every service believes it; the design moves the
-- role onto the record, resolved server-side from a registry (car 2).
-- People have employee rows and agents have `agents` rows; the
-- automation ids — 1,050 of 1,111 step completions when the item was
-- measured — had no row anywhere, so a registry read had no answer for
-- them. This table is that row. It refuses nothing and changes no one:
-- every row carries the role its caller already asserts, and nothing
-- decides on it until car 2's resolver, which ships in report mode.
--
-- A TABLE OF ITS OWN, not rows in `agents`: an agents row holding a
-- role is folded into the dispatcher's roster (roster_union) and is a
-- candidate for every step whose audience is that role, so an
-- automation holding platform-admin there would have been nominated
-- for the founder's own steps. Nothing that reads `agents` — the
-- roster, the login door, the claim gate, the budget — reads this.
--
-- SCHEMA ONLY. The rows are the platform bundle
-- infra/platform/automations/ (one file per row), published at every
-- start by boss-platform-workflow-seed, insert-if-absent; this table
-- joins REGISTRY_TABLES in migrations-declare-schema-only.sh, so no
-- later migration may insert one.
--
-- `signs_for` is a FAMILY: the dispatcher signs as
-- automation:rule:<name> for each rule it fires and the ops runner as
-- automation:ops-runner:<host>:<run>, so the signer's row declares the
-- prefix and answers for every member. UNIQUE, because a family signs
-- as one automation. The two CHECKs are the SQL spelling of
-- boss_jobs::agents::automations::{is_automation_id, is_family_prefix}.

CREATE TABLE IF NOT EXISTS automation_actors (
    id           TEXT PRIMARY KEY CHECK (id ~ '^automation:[a-z0-9][a-z0-9-]*$'),
    -- A Class code under (employee, role): the vocabulary an
    -- employee's and an agent's role are spelled in.
    role         TEXT NOT NULL CHECK (btrim(role) <> ''),
    description  TEXT NOT NULL CHECK (btrim(description) <> ''),
    signs_for    TEXT UNIQUE CHECK (signs_for ~ '^automation:[a-z0-9][a-z0-9-]*:$'),
    created_at   TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE automation_actors IS
  'Every automation:* id that writes, and the role it signs as — the '
  'automation half of the agents registry (backlog ddf0773e, design '
  'abf9eeae). Rows come from the platform bundle '
  'infra/platform/automations/, insert-if-absent at every start.';

COMMENT ON COLUMN automation_actors.signs_for IS
  'The id prefix this automation also signs as, one id per firing '
  '(automation:rule: for the dispatcher): every id under it is '
  'answered by this row. NULL for most.';

INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  ('automation.declared', 'jobs', 'The platform seed inserted one automation_actors row from the bundle infra/platform/automations/ (insert-if-absent): the row as declared — id, role, description, signs_for — plus declared_by, the seed''s actor. One per inserted row, none for a row the registry already held, none per run', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;
