-- =========================================================================
-- 05-ml.sql — ML platform — models + predictions.
-- =========================================================================


-- -----------------------------------------------------------------------------
-- ML platform (boss-ml-api)
--
-- Models + predictions as append-only rows. Storage-only model kinds
-- take scores written by external code via POST /api/ml/predictions;
-- inference-driven kinds are scored by the ML runtime (see
-- inference_spec). See docs/architecture-decisions.md §ML platform.
-- -----------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS ml_models (
    id                  TEXT PRIMARY KEY,
    name                TEXT NOT NULL,
    -- Storage-only ModelKinds (predictions written by external code)
    -- plus the two inference-driven kinds: declarative-rule = SQL +
    -- payload template, no plugin code; heuristic-formula = composite
    -- scoring via a registered InferencePlugin. See
    -- docs/architecture-decisions.md §ML platform.
    kind                TEXT NOT NULL CHECK (kind IN (
        'risk-score',
        'regression',
        'classification',
        'anomaly-detection',
        'ranking',
        'declarative-rule',
        'heuristic-formula'
    )),
    version             TEXT NOT NULL,
    status              TEXT NOT NULL CHECK (status IN (
        'draft', 'active', 'shadow', 'retired'
    )) DEFAULT 'draft',
    accuracy            DOUBLE PRECISION,
    accuracy_metric     TEXT,
    training_data_ref   TEXT,
    description         TEXT,
    -- Dispatch spec for the inference-driven ModelKinds.
    -- NULL for storage-only kinds. For declarative-rule, holds
    -- {query, score_expr, entity_type, entity_id_column,
    -- payload_template}. For heuristic-formula, holds
    -- {plugin_name, entity_type, entity_query}.
    inference_spec      JSONB,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (name, version)
);


CREATE INDEX IF NOT EXISTS ml_models_status ON ml_models(status);

CREATE INDEX IF NOT EXISTS ml_models_kind ON ml_models(kind);


-- Append-only prediction log. No FK on (entity_type, entity_id)
-- because entities live in sibling services — boss-ml doesn't know
-- whether a account / device / opportunity actually exists.
CREATE TABLE IF NOT EXISTS ml_predictions (
    id              TEXT PRIMARY KEY,
    model_id        TEXT NOT NULL REFERENCES ml_models(id) ON DELETE CASCADE,
    entity_type     TEXT NOT NULL,
    entity_id       TEXT NOT NULL,
    score           DOUBLE PRECISION NOT NULL,
    payload         JSONB,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);


CREATE INDEX IF NOT EXISTS ml_predictions_model ON ml_predictions(model_id);

CREATE INDEX IF NOT EXISTS ml_predictions_entity
    ON ml_predictions(entity_type, entity_id);

CREATE INDEX IF NOT EXISTS ml_predictions_created ON ml_predictions(created_at DESC);

