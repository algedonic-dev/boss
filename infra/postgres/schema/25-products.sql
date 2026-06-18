-- =========================================================================
-- 25-products.sql — Products — finished-goods catalog + per-location finished inventory.
-- =========================================================================


-- -----------------------------------------------------------------------------
-- Finished products — the catalog of *output* goods the tenant produces
-- and sells. Sibling to `parts` (raw inputs catalog) but tracks output
-- instead of input. The brewery's beer SKUs (FP-PALE-1-2-BBL,
-- FP-IPA-1-6-BBL, …) live here; `inventory_items` tracks ingredients +
-- packaging consumables only.
--
-- One row per SKU = one product at one package size. `package_unit`
-- is denorm metadata (the SKU itself encodes the unit) used for
-- /products UI rollups by package class. `product_kind` lets a tenant
-- group SKUs (every brewery beer is `product_kind='beer'`).
-- -----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS products (
    sku             TEXT PRIMARY KEY,
    name            TEXT NOT NULL,
    product_kind    TEXT NOT NULL,                          -- 'beer', 'cider', 'mead', 'refurb-device', ...
    package_unit    TEXT NOT NULL,                          -- '1/2-bbl-keg', '1/6-bbl-keg', '12oz-case', 'unit'
    description     TEXT,
    metadata        JSONB NOT NULL DEFAULT '{}'::jsonb,     -- abv, ibu, style, msrp_cents, ...
    active          BOOLEAN NOT NULL DEFAULT TRUE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);


CREATE INDEX IF NOT EXISTS products_active ON products(active) WHERE active = TRUE;

CREATE INDEX IF NOT EXISTS products_kind ON products(product_kind);


-- Per-location on-hand counts for finished products. Mirrors
-- `inventory_items` but keyed (sku, location) instead of sku alone,
-- because finished goods MOVE — brewhouse cooler → taproom →
-- distributor truck — and operators need to see "where is X right
-- now" not just total count. `location_id` is a soft reference to
-- locations(id) — not an FK constraint, so this table doesn't couple
-- to the locations load order.
CREATE TABLE IF NOT EXISTS finished_product_inventory (
    product_sku     TEXT NOT NULL REFERENCES products(sku) ON DELETE CASCADE,
    location_id     TEXT NOT NULL,
    on_hand         INTEGER NOT NULL DEFAULT 0,
    reserved        INTEGER NOT NULL DEFAULT 0,             -- earmarked for an open Job (wholesale-keg-order in flight, etc.)
    -- Weighted moving-average production cost per unit. Set on
    -- `products.produce` from the JobKind's `unit_cost_cents`;
    -- `products.consume` reads it to size the `finance.cogs.
    -- recognized` JE (DR 5100 / CR 1320 FG). This is how Model B
    -- COGS recognizes at sale time at actual standard cost, not
    -- a percentage shortcut. See docs/design/correctness-
    -- protocol.md.
    production_cost_cents BIGINT NOT NULL DEFAULT 0,
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (product_sku, location_id)
);


CREATE INDEX IF NOT EXISTS finished_product_inventory_sku ON finished_product_inventory(product_sku);

CREATE INDEX IF NOT EXISTS finished_product_inventory_location ON finished_product_inventory(location_id);

-- Products ref-check seed rows: events that REFERENCE an existing
-- products row. (Trigger + table live in 02-events.)
INSERT INTO audit_log_ref_checks (event_kind, field_path, ref_table, ref_column) VALUES
    ('products.produced',             'sku',         'products',        'sku'),
    ('products.consumed',             'sku',         'products',        'sku'),
    ('products.inventory.upserted',   'product_sku', 'products',        'sku')
ON CONFLICT (event_kind, field_path) DO NOTHING;

