-- 135-sign-off-plugin.sql — register the `sign-off` step surface.
--
-- Origin (David, 2026-08-15, feedback b1aa1f5f, on the doc-flatten
-- ratify step): "Missing custom step UX".
--
-- WHY THIS ONE FIRST. `sign-off` is the most common step kind in the
-- platform protocols — ship-a-change's `gate`, doc-flatten's `review`,
-- protocol-retro's `ratify`, incident's `prevention` — and it was the
-- only one of them with no plugin, so every one rendered the generic
-- surface. Eight plugins were registered and none covered the kind
-- that appears most.
--
-- WHAT THE GENERIC SURFACE CANNOT SHOW, which is the argument for a
-- bundle rather than better labels:
--
--   * `sign_offs_required` is a list of ROLES and the step will not
--     complete until each has a stamp. The generic surface offers a
--     Complete button that simply fails, which teaches an operator to
--     distrust buttons. This surface withholds it and names who is
--     missing.
--   * A stamp pins the `step_shape_hash` at the moment of signing, so
--     editing the step afterwards invalidates it and completion answers
--     409 with a `stale_roles` list. That is the right rule — a
--     signature is on a specific thing, not on a step id — and it is
--     invisible until it bites.
--
-- No new step KIND is introduced, which is what makes this shippable
-- as data: `sign-off` is already in step_types.toml, so this is a
-- registry row plus a JS bundle and needs no binary. The bundle reaches
-- the cluster through the step-plugins ConfigMap that
-- cluster-deploy-runner.sh rebuilds from infra/step-plugins/ on every
-- converge.
INSERT INTO step_plugins (
    kind, version, status, label, description, category,
    metadata_schema, frontend_url, owning_team
) VALUES (
    'sign-off', 1, 'active', 'Sign off',
    'Custom Step UX for sign-off steps: lists each required role with its stamp or its absence, offers a per-role sign button, withholds Complete until every signature is in, and surfaces the server''s stale-stamp explanation when a step was edited after signing.',
    'coordination',
    '{"type":"object","properties":{}}',
    'sign-off.js',
    'platform'
)
ON CONFLICT (kind, version) DO NOTHING;
