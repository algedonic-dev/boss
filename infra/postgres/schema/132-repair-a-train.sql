-- 132-repair-a-train.sql — seed the `repair-a-train` Workflow.
--
-- Origin (David, 2026-08-14): "Let's create a protocol and then
-- exercise it", with a red train sitting in front of us.
--
-- WHY THIS EXISTS. A red train holds its whole consist hostage. Its
-- cars carry a `train` marker, so `parked_ready` no longer counts them
-- and they cannot board anything else; and the conductor merges only
-- on a green verdict, so the train cannot recover on its own. Before
-- this protocol there was no defined path out of that state — train
-- 20260814-2139 sat red from 22:00 with five cars aboard, and the only
-- reason anyone noticed was that someone went looking for why nothing
-- had landed.
--
-- THE FORK IS THE POINT. There are exactly two honest endings for a
-- red train, and the protocol refuses to blur them:
--
--   repaired  — the fault is fixable in place; the consist survives.
--   cancelled — it is not; the train dies and its cars are RELEASED
--               back to the dock so they can board the next one.
--
-- Two terminals rather than one because the ratio is the measurement.
-- A pipeline that repairs everything is one where CI catches nothing
-- late; a pipeline that cancels often is one whose cars are being
-- assembled with conflicts nobody checked. Collapsing them would make
-- "is our CI failing early enough?" unanswerable — the same reasoning
-- as the maintenance sweep's clear/remediated split (121).
--
-- `cars_held` is a required field on diagnosis specifically so the
-- COST is recorded, not just the cause. Five cars stuck for two hours
-- is the number that argues for fixing the class rather than the
-- instance.
--
-- Published to the registry at runtime and exercised immediately
-- against the live red train (repair job ebf203a6, which reached the
-- `repaired` terminal). This file is the seed so a fresh database has
-- it too.
INSERT INTO workflows (kind, version, status, label, description, category, subject_kinds, steps, owning_team)
VALUES ('repair-a-train', 1, 'active',
  'Repair a train whose CI went red',
  'A red train holds its whole consist hostage: the cars are marked boarded so they are no longer parked, and the conductor only merges on green, so nothing moves until someone acts. This protocol is that someone. It forks on the only question that matters — can this be repaired in place, or should the train be cancelled and its cars released back to the dock — and records the diagnosis either way, so ''why did this train die'' is a query.',
  'platform', '["custom"]'::jsonb,
  '[{"title":"opened","kind":"trigger","ready_when":"true","title_template":"Train {metadata.train} is red","sign_offs_required":[],"fields":[],"authority_role":"platform-admin","metadata_defaults":null},{"title":"diagnose","kind":"checklist","ready_when":"steps.opened.done","title_template":"Diagnose what turned {metadata.train} red","sign_offs_required":[],"fields":[{"name":"failing_check","field_type":"string","required":true},{"name":"root_cause","field_type":"string","required":true},{"name":"cars_held","field_type":"string","required":true},{"name":"items","field_type":"array","required":true}],"authority_role":"platform-admin","metadata_defaults":null},{"title":"repair","kind":"task","ready_when":"steps.diagnose.done AND job.metadata.repairable != \"false\"","title_template":"Repair {metadata.train} in place","sign_offs_required":[],"fields":[{"name":"fix_branch","field_type":"string","required":true},{"name":"verified","field_type":"string","required":true}],"authority_role":"platform-admin","metadata_defaults":null},{"title":"repaired","kind":"outcome","ready_when":"steps.repair.done","terminal":{"outcome":"repaired"},"title_template":"Train repaired","sign_offs_required":[],"fields":[],"authority_role":"platform-admin","metadata_defaults":null},{"title":"cancel","kind":"task","ready_when":"steps.diagnose.done AND job.metadata.repairable = \"false\"","title_template":"Cancel {metadata.train} and release its cars","sign_offs_required":[],"fields":[{"name":"reason","field_type":"string","required":true},{"name":"cars_released","field_type":"string","required":true}],"authority_role":"platform-admin","metadata_defaults":null},{"title":"cancelled","kind":"outcome","ready_when":"steps.cancel.done","terminal":{"outcome":"cancelled"},"title_template":"Train cancelled, cars released","sign_offs_required":[],"fields":[],"authority_role":"platform-admin","metadata_defaults":null}]'::jsonb,
  'platform')
ON CONFLICT (kind, version) DO NOTHING;
