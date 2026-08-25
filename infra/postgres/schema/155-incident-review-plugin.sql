-- incident-review — a custom Step UX for the incident-post-mortem
-- Workflow's "Human review of the findings" step.
--
-- WHY. Two feedback packets reported the post-mortem review step
-- rendering the findings unusably; one asked for "a custom step UX
-- that presented the findings that I needed to sign-off on". The
-- complaint is structural, the same shape 146-correction-verdict fixed:
-- everything a reviewer needs to judge lives elsewhere — the Job's own
-- semi-structured metadata (summary, evidence, mitigations, open
-- questions) and the answers the earlier steps recorded (timeline,
-- attribution, detection, simplification, actions) — all behind a
-- packet modal and a metadata dump. The question was on screen; the
-- material to answer it was not.
--
-- `incident-review.js` renders the whole post-mortem as one readable
-- document — the Job metadata as ordered sections (known keys
-- first-class, unknown keys as labeled prose, nothing dropped, no raw
-- JSON), then what each sibling step found, labeled by its fields —
-- and closes with the completion the step requires. The same
-- semi-structured renderer backs the /it/incidents archive
-- (apps/web/src/it/incidents/postMortemDoc.ts); the plugin carries its
-- own copy because bundles are standalone JS by design.
--
-- WHY A NEW KIND AND NOT A PLUGIN ON `task`. Plugins register by step
-- kind, and the SPA mounts by kind (StepSurface prefers an active
-- plugin over the built-in surface). Registering for `task` would
-- hijack every task step in the system. A dedicated kind is the
-- extension point the plugin system documents — same rationale as
-- 146-correction-verdict-plugin.sql. The Rust StepRegistry does not
-- need to learn this kind: `validate_metadata` is permissive for kinds
-- it does not know, and the completion contract still comes from the
-- step row's own authored `fields` (v1 authors none on `review`).
--
-- ACTIVATION. incident-post-mortem v1 declares its `review` step as a
-- bare `task`; this row does nothing to in-flight packets (they are
-- pinned to v1). The surface goes live when the next workflow version
-- repoints `review` to kind=incident-review at /system/workflows —
-- protocol data, no deploy.

INSERT INTO step_plugins (
    kind, version, status, label, description, category,
    metadata_schema, frontend_url, owning_team
) VALUES (
    'incident-review', 1, 'active', 'Incident review',
    'Renders the whole post-mortem as one readable document — the Job''s semi-structured metadata as ordered sections (known keys first-class, unknown keys as labeled prose, nothing dropped), then what each sibling step found labeled by its fields — and completes the review step. Reads the Job for its metadata and sibling steps; writes nothing but the step completion.',
    'platform',
    '{"type":"object","properties":{}}',
    'incident-review.js',
    'platform'
) ON CONFLICT (kind, version) DO NOTHING;
