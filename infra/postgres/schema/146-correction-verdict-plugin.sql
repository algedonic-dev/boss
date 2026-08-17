-- correction-verdict — a custom Step UX for the accept/reject gate of
-- the correct-the-record Workflow.
--
-- WHY. David, with four corrections parked in his queue: "The UX for the
-- corrections is hard for me to understand the question and trade-off.
-- Let's improve how the context is shown with a custom Step UX plugin."
--
-- The complaint is structural, not cosmetic. The review step is a bare
-- `task`; its card reads "Accept the correction" and offers a `verdict`
-- field with two enum values whose names — accepted, unfounded — do not
-- say what either one causes. Everything needed to answer the question
-- sits on the PREVIOUS step: `claim`, `measured`, `method`, `where`, all
-- of it behind a packet modal and a metadata dump. The question was on
-- screen and the material to answer it was not, which is the definition
-- of a decision surface that does not support the decision.
--
-- `correction-verdict.js` puts the false claim beside the measurement
-- that contradicts it, collapses method and location under it, and
-- labels each verdict with its consequence ("opens Land it where the
-- claim lives" / "closes with the original claim intact"). No new
-- information — the same metadata, arranged so the trade-off is the
-- first thing read.
--
-- WHY A NEW KIND AND NOT A PLUGIN ON `task`. Plugins register by step
-- kind, and the SPA mounts by kind (apps/web/src/steps/StepSurface.svelte
-- prefers an active plugin over the built-in surface). Registering for
-- `task` would hijack every task step in the system. A dedicated kind is
-- the extension point the plugin system already documents — "a new step
-- kind with its own UX surface = a StepPlugin, JS only".
--
-- The Rust StepRegistry does not need to learn this kind:
-- `validate_metadata` is permissive for kinds it does not know, and the
-- completion contract still comes from the step row's own authored
-- `fields` (which carry `verdict` as required). So the gate is unchanged
-- and only the rendering moves.

INSERT INTO step_plugins (
    kind, version, status, label, description, category,
    metadata_schema, frontend_url, owning_team
) VALUES (
    'correction-verdict', 1, 'active', 'Correction verdict',
    'Renders a correct-the-record correction as claim-versus-measurement with the consequence of each verdict spelled out, then records verdict=accepted|unfounded and completes the step. Reads claim/measured/method/where from the sibling evidence step and corrects/corrects_title from the Job.',
    'platform',
    '{"type":"object","properties":{"verdict":{"type":"string","enum":["accepted","unfounded"]}},"required":["verdict"]}',
    'correction-verdict.js',
    'platform'
) ON CONFLICT (kind, version) DO NOTHING;
