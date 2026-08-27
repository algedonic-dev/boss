-- scope-declaration — a custom Step UX for the `scope` step of
-- ship-a-change: the moment a person declares what a change contains
-- and what it deliberately leaves out.
--
-- WHY. David, holding one of these: "I don't know how to input my
-- decision within this UX / I think the wrong step UX is showing."
--
-- Both halves of that read correctly. The step is a bare `task` with
-- two required string fields, so the generic surface renders it as two
-- unlabelled textareas named `summary` and `excludes` — no prompt, no
-- example, and nothing saying that `excludes` is the field the step
-- exists for. A surface that says nothing about the decision it takes
-- IS the wrong UX; the reading was right even where the diagnosis was
-- not.
--
-- It is also the most-used human step in the car protocol: every car
-- begins here. `scope-declaration.js` asks the two questions in words —
-- what this car DOES, and what it deliberately does NOT do and why —
-- carries the reason the second one is required (registry.rs: "naming
-- what you are not doing is the act that keeps a change small"), and
-- shows the packet's own context: the branch, which is the Job's
-- Subject id, the backlog packet it answers, and the gate receipt when
-- the gate has already run on the branch.
--
-- WHY A NEW KIND AND NOT A PLUGIN ON `task`. Plugins register by step
-- kind and the SPA mounts by kind (apps/web/src/steps/StepSurface.svelte
-- prefers an active plugin over the built-in surface), so registering
-- for `task` would hijack every task step in the system. A dedicated
-- kind is the extension point the plugin system documents. Same
-- reasoning, same shape as schema 146 (correction-verdict).
--
-- The Rust StepRegistry does not need to learn this kind:
-- `validate_metadata` is permissive for kinds it does not know, and the
-- completion contract still comes from the step row's own authored
-- `fields`, which carry `summary` and `excludes` as required.
--
-- NOT ACTIVE YET, DELIBERATELY. ship-a-change's `scope` step is still
-- `kind = "task"`. Pointing it at this kind is a Workflow v2 — a
-- registry write, no deploy — and it is a follow-up to this row, so the
-- bundle can be deployed and looked at before every car in flight
-- starts rendering it.

INSERT INTO step_plugins (
    kind, version, status, label, description, category,
    metadata_schema, frontend_url, owning_team
) VALUES (
    'scope-declaration', 1, 'active', 'Scope declaration',
    'Asks a car''s author what the change does and what it deliberately does not do, with the reason the second question is required, then records summary + excludes and completes the step. Shows the branch (the Job''s Subject id), the backlog packet the change answers, and the gate receipt when the gate step is already complete.',
    'platform',
    '{"type":"object","properties":{"summary":{"type":"string"},"excludes":{"type":"string"}},"required":["summary","excludes"]}',
    'scope-declaration.js',
    'platform'
) ON CONFLICT (kind, version) DO NOTHING;
