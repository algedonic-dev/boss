-- 126-answer-question-plugin.sql — register the Step UX for the
-- `approval` Workflow's decide step.
--
-- David, 2026-08-14, on the protocol's first outing: "I don't have any
-- context for the question. The sender should be able to supply info,
-- probably a markdown panel, that they want me to see when I answer
-- the question. This will also provide the canvas to provide evidence
-- or the artifact for an approval version of this general question,
-- which is 'does this meet your bar?' essentially."
--
-- The generic surface renders a step's fields as inputs, which is
-- correct for a checklist and useless here: it shows `verdict` and
-- `answer` as two empty boxes with no sight of what is being decided.
-- The plugin renders the asker's markdown beside the question, so the
-- evidence and the decision are on screen together.
--
-- The bundle reaches the pod by the ordinary route — the converge
-- runner rebuilds the step-plugins ConfigMap from infra/step-plugins/
-- on every run, so this row and its file ship on the same car.
--
-- `answer-question` is registered in the StepType registry
-- (crates/core/boss-jobs/seeds/step_types.toml) with the two required
-- fields; this row only says which bundle draws it.
INSERT INTO step_plugins (
    kind, version, status, label, description, category,
    metadata_schema, frontend_url, owning_team
) VALUES (
    'answer-question', 1, 'active', 'Answer a question',
    'Custom Step UX for the approval Workflow: renders the asker''s question, the markdown context they supplied (evidence, a measurement, the artifact under review), and their proposed answer behind a control that copies it into the answer box. Verdict and answer are both required, so a decision cannot complete as an empty step.',
    'coordination',
    '{"type":"object","properties":{"verdict":{"type":"string","enum":["approved","declined","answered"]},"answer":{"type":"string"}}}',
    'answer-question.js',
    'platform'
)
ON CONFLICT (kind, version) DO NOTHING;
