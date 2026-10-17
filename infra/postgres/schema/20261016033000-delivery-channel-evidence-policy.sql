-- Eligibility and evidence are operating-model data, versioned with the
-- delivery policy. Existing versions remain heavy: NULL grants no lighter path.
ALTER TABLE delivery_policy ADD COLUMN channels JSONB NOT NULL DEFAULT 'null'::jsonb;
