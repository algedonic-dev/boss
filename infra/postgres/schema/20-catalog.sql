-- =========================================================================
-- 20-catalog.sql — Catalog (Equipment KB) — asset models, parts, system reference data, documents, marketing assets.
-- =========================================================================


-- -----------------------------------------------------------------------------
-- System catalog (reference data, slow-changing)
-- -----------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS asset_models (
    sku                         TEXT PRIMARY KEY,
    name                        TEXT NOT NULL,
    manufacturer                TEXT NOT NULL,
    model_year                  SMALLINT NOT NULL CHECK (model_year BETWEEN 1980 AND 2100),
    -- Tenant-defined category. Validated downstream against the
    -- Class registry (subject_kind='asset', member_attribute='category')
    -- when a matching row exists; otherwise free-form. No DB CHECK, so a
    -- tenant declares its own categories without a schema migration.
    category                    TEXT NOT NULL,

    -- Commerce
    list_price_new_cents        BIGINT NOT NULL,
    typical_refurb_price_cents  BIGINT,
    currency                    TEXT NOT NULL DEFAULT 'USD' CHECK (length(currency) = 3),
    lead_time_days              SMALLINT,
    tagline                     TEXT NOT NULL,
    description                 TEXT NOT NULL,
    hero_image                  TEXT,

    -- Physical
    width_cm                    REAL NOT NULL,
    depth_cm                    REAL NOT NULL,
    height_cm                   REAL NOT NULL,
    weight_kg                   REAL NOT NULL,
    power_requirements          TEXT NOT NULL,

    -- Regulatory (tenant-defined; e.g. an FDA 510(k) id, a CE-mark id, etc.)
    clearance_id                TEXT,
    clearance_date              DATE,
    regulator_device_class      SMALLINT NOT NULL DEFAULT 2 CHECK (regulator_device_class BETWEEN 1 AND 3),

    -- Service profile
    preventive_maintenance_hours                    REAL NOT NULL,
    preventive_maintenance_interval_months          SMALLINT NOT NULL CHECK (preventive_maintenance_interval_months > 0),
    calibration_interval_months SMALLINT NOT NULL CHECK (calibration_interval_months > 0),
    required_skill_level        SMALLINT NOT NULL CHECK (required_skill_level BETWEEN 1 AND 5),
    depot_required              BOOLEAN NOT NULL DEFAULT false,

    -- Support
    end_of_support              DATE,
    current_firmware            TEXT,

    -- Tenant-defined kind-specific specs. The platform stores
    -- whatever JSON the tenant POSTs; validation against a
    -- per-category JSON schema lives in `asset_model_extras_schema`.
    -- See docs/design/equipment-specs-genericization.md for the
    -- typed-vs-extras boundary: physical / power / service profile /
    -- support / regulatory stay typed; everything else
    -- (networking-equipment specs, brewing-vessel specs, printer ppm,
    -- vehicle dimensions) lands here.
    extras                      JSONB NOT NULL DEFAULT '{}'::jsonb,

    created_at                  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at                  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);


CREATE INDEX IF NOT EXISTS asset_models_category ON asset_models(category);


-- ## asset_model_extras_schema — per-category JSON schema for `extras`
--
-- One row per `category_code`. The platform stores whatever JSON the
-- tenant supplies; before write it validates `extras` against the
-- schema row for that category. Categories without a row pass
-- through (the platform stays neutral).
CREATE TABLE IF NOT EXISTS asset_model_extras_schema (
    category_code  TEXT PRIMARY KEY,
    schema         JSONB NOT NULL,
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT NOW()
);


-- Use cases — free-text per-tenant taxonomy, so each tenant defines
-- its own vocabulary. No DB CHECK; a Class-registry row can validate
-- values per tenant when a tenant wants the gate.
CREATE TABLE IF NOT EXISTS asset_use_cases (
    sku             TEXT NOT NULL REFERENCES asset_models(sku) ON DELETE CASCADE,
    use_case        TEXT NOT NULL,
    PRIMARY KEY (sku, use_case)
);


-- Failure modes — serviced over the installed base.
CREATE TABLE IF NOT EXISTS asset_failure_modes (
    sku             TEXT NOT NULL REFERENCES asset_models(sku) ON DELETE CASCADE,
    code            TEXT NOT NULL,
    name            TEXT NOT NULL,
    frequency       REAL NOT NULL CHECK (frequency BETWEEN 0 AND 1),
    typical_fix     TEXT NOT NULL,
    PRIMARY KEY (sku, code)
);


-- preventive maintenance checklist items — ordered list per model.
CREATE TABLE IF NOT EXISTS asset_pm_checklist (
    sku             TEXT NOT NULL REFERENCES asset_models(sku) ON DELETE CASCADE,
    sort_order      SMALLINT NOT NULL,
    item            TEXT NOT NULL,
    PRIMARY KEY (sku, sort_order)
);


-- Spare parts and consumables share a parent table but carry different fields.
CREATE TABLE IF NOT EXISTS parts (
    part_sku        TEXT PRIMARY KEY,
    name            TEXT NOT NULL,
    description     TEXT NOT NULL,
    unit_price_cents BIGINT NOT NULL,
    currency        TEXT NOT NULL DEFAULT 'USD' CHECK (length(currency) = 3),
    lead_time_days  SMALLINT NOT NULL DEFAULT 7
);


CREATE TABLE IF NOT EXISTS asset_spare_parts (
    sku             TEXT NOT NULL REFERENCES asset_models(sku) ON DELETE CASCADE,
    part_sku        TEXT NOT NULL REFERENCES parts(part_sku),
    high_usage      BOOLEAN NOT NULL DEFAULT false,
    PRIMARY KEY (sku, part_sku)
);


CREATE TABLE IF NOT EXISTS asset_consumables (
    sku                   TEXT NOT NULL REFERENCES asset_models(sku) ON DELETE CASCADE,
    part_sku              TEXT NOT NULL REFERENCES parts(part_sku),
    treatments_per_unit   INTEGER,
    PRIMARY KEY (sku, part_sku)
);


-- Documents (manuals, spec sheets, certs).
CREATE TABLE IF NOT EXISTS asset_documents (
    id              BIGSERIAL PRIMARY KEY,
    sku             TEXT NOT NULL REFERENCES asset_models(sku) ON DELETE CASCADE,
    -- Free-text document kind; tenants extend via the Class registry
    -- under (subject_kind='asset', member_attribute='document-kind').
    -- Validation lives at the catalog API boundary, not a DB CHECK.
    kind            TEXT NOT NULL,
    title           TEXT NOT NULL,
    url             TEXT NOT NULL,
    version         TEXT,
    published       DATE,
    audience        TEXT NOT NULL CHECK (audience IN ('internal', 'customer', 'public'))
);


CREATE INDEX IF NOT EXISTS asset_documents_sku ON asset_documents(sku);


-- -----------------------------------------------------------------------------
-- Knowledge Base — shared documents + domain-specific facts
-- -----------------------------------------------------------------------------

-- Documents associated with any KB entity — one shared table across
-- entity kinds, discriminated by (entity_kind, entity_id).
CREATE TABLE IF NOT EXISTS documents (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    entity_kind     TEXT NOT NULL,
    entity_id       TEXT NOT NULL,
    doc_type        TEXT NOT NULL,
    title           TEXT NOT NULL,
    url             TEXT,
    version         TEXT,
    audience        TEXT NOT NULL DEFAULT 'internal',
    uploaded_by     TEXT,
    uploaded_at     TIMESTAMPTZ DEFAULT NOW(),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);


CREATE INDEX IF NOT EXISTS documents_entity ON documents(entity_kind, entity_id);

CREATE INDEX IF NOT EXISTS documents_type ON documents(doc_type);


-- -----------------------------------------------------------------------------
-- Marketing assets — standalone catalog for marketing-owned files
-- (photos, videos, decks, one-pagers, briefs, retros, logos,
-- templates). Separate from `documents` so the commercial metadata
-- (linked campaigns, brand review, version-via-supersedes) doesn't
-- fight the generic-docs shape. Versioning is event-sourced via
-- `supersedes_id` — no monotonic `version` column.
-- -----------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS marketing_assets (
    id                   TEXT PRIMARY KEY,
    title                TEXT NOT NULL,
    -- Free-text marketing-asset kind; tenants extend via the Class
    -- registry under subject_kind='marketing-asset' (the code is the
    -- kind string). Validation lives at the catalog API boundary, not
    -- a DB CHECK. Nullable: identity-first, an asset can be created
    -- before it's classified and tagged with a kind later (the API gate
    -- only fires when a value is present).
    kind                 TEXT,
    description          TEXT,
    file_url             TEXT,                    -- external URL (URL-only, no inline bytes)
    tags                 JSONB NOT NULL DEFAULT '[]'::jsonb,
    -- Linked entities — JSONB arrays keep joins loose; a richer
    -- normalized model lands when a query actually needs it.
    linked_device_skus   JSONB NOT NULL DEFAULT '[]'::jsonb,
    linked_account_ids  JSONB NOT NULL DEFAULT '[]'::jsonb,
    linked_campaign_ids  JSONB NOT NULL DEFAULT '[]'::jsonb,
    owner_id             TEXT REFERENCES employees(id),
    brand_reviewed_by    TEXT REFERENCES employees(id),
    brand_reviewed_at    TIMESTAMPTZ,
    -- Event-sourced versioning: a new version points at the one it
    -- replaces. Walking the chain gives the full history; the
    -- "current" version is whichever asset has no successor.
    supersedes_id        TEXT REFERENCES marketing_assets(id),
    retired_at           TIMESTAMPTZ,
    created_at           TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at           TIMESTAMPTZ NOT NULL DEFAULT NOW()
);


CREATE INDEX IF NOT EXISTS marketing_assets_kind ON marketing_assets(kind);

CREATE INDEX IF NOT EXISTS marketing_assets_owner ON marketing_assets(owner_id);

CREATE INDEX IF NOT EXISTS marketing_assets_supersedes ON marketing_assets(supersedes_id)
    WHERE supersedes_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS marketing_assets_active
    ON marketing_assets(created_at DESC) WHERE retired_at IS NULL;

-- Partial index for the tags + linked-* JSONB arrays uses GIN so
-- "assets tagged :hero" and "assets that reference VND-001" stay
-- cheap.
CREATE INDEX IF NOT EXISTS marketing_assets_tags_gin ON marketing_assets USING gin(tags);

CREATE INDEX IF NOT EXISTS marketing_assets_linked_skus_gin
    ON marketing_assets USING gin(linked_device_skus);

CREATE INDEX IF NOT EXISTS marketing_assets_linked_campaigns_gin
    ON marketing_assets USING gin(linked_campaign_ids);

