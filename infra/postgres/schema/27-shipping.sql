-- =========================================================================
-- 27-shipping.sql — Shipping — inbound/outbound shipment tracking.
-- =========================================================================


-- -----------------------------------------------------------------------------
-- Shipping (inbound and outbound shipment tracking)
-- -----------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS shipments (
    id                  TEXT PRIMARY KEY,
    direction           TEXT NOT NULL CHECK (direction IN ('inbound', 'outbound')),
    -- Lifecycle status; no DB CHECK. The Class registry validates
    -- values at the shipping API boundary under
    -- (subject_kind='shipment', member_attribute='status').
    status              TEXT NOT NULL,
    -- Free-text carrier code; tenants extend via the Class registry
    -- under (subject_kind='shipment'). Validation lives at the
    -- shipping API boundary, not a DB CHECK. Nullable: a shipment can
    -- be created identity-first (id + direction + endpoints) and have
    -- its carrier enriched later; the registry gate fires only when a
    -- value is present.
    carrier             TEXT,
    tracking_number     TEXT,
    origin              TEXT NOT NULL,
    destination         TEXT NOT NULL,
    po_id               TEXT,
    order_id            TEXT,
    account_id           TEXT,
    created_on          DATE NOT NULL,
    shipped_on          DATE,
    estimated_delivery  DATE,
    delivered_on        DATE,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);


CREATE INDEX IF NOT EXISTS shipments_direction ON shipments(direction);

CREATE INDEX IF NOT EXISTS shipments_status ON shipments(status);

CREATE INDEX IF NOT EXISTS shipments_account ON shipments(account_id) WHERE account_id IS NOT NULL;


CREATE TABLE IF NOT EXISTS shipment_assets (
    shipment_id     TEXT NOT NULL REFERENCES shipments(id) ON DELETE CASCADE,
    asset_id       TEXT NOT NULL,
    PRIMARY KEY (shipment_id, asset_id)
);


-- Per-line items carried on a shipment. Distinct from
-- `shipment_assets` (which references identity-bearing
-- used-device-shop systems) — line items are SKU + qty pairs for
-- finished products and parts: a wholesale-keg-order shipment of
-- "12× FP-PALE-1-2-BBL" lands here, not as 12 separate Subjects.
-- Populated by the shipping.create side effect from the JobKind
-- step's `line_items` metadata; the inventory.parts.consume
-- handler reads from the same metadata to decrement stock when
-- the shipment step transitions to done.
--
-- `idx` preserves authoring order on the originating Job so the
-- shipment detail view + inventory consume can render line items
-- in the same sequence the JobKind author defined them.
CREATE TABLE IF NOT EXISTS shipment_line_items (
    id                  BIGSERIAL PRIMARY KEY,
    shipment_id         TEXT NOT NULL REFERENCES shipments(id) ON DELETE CASCADE,
    idx                 INT NOT NULL,
    sku                 TEXT NOT NULL,
    qty                 INT NOT NULL CHECK (qty > 0),
    unit_price_cents    BIGINT,
    description         TEXT,
    UNIQUE (shipment_id, idx)
);


CREATE INDEX IF NOT EXISTS shipment_line_items_shipment
    ON shipment_line_items (shipment_id);

CREATE INDEX IF NOT EXISTS shipment_line_items_sku
    ON shipment_line_items (sku);


-- Per-scan tracking events emitted by carrier counterparties
-- (the brewery's keg-courier scans, real-world FedEx webhooks,
-- etc.). Each row is one carrier ping for a shipment; the
-- shipment's `status` column is the rollup, this table is the
-- timeline. Idempotent on (shipment_id, status, occurred_on)
-- so deterministic-id replays converge on a single row.
CREATE TABLE IF NOT EXISTS shipment_tracking_events (
    id              BIGSERIAL PRIMARY KEY,
    shipment_id     TEXT NOT NULL REFERENCES shipments(id) ON DELETE CASCADE,
    status          TEXT NOT NULL,
    occurred_on     DATE NOT NULL,
    stage_index     SMALLINT,
    detail          TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (shipment_id, status, occurred_on)
);


CREATE INDEX IF NOT EXISTS shipment_tracking_events_shipment
    ON shipment_tracking_events (shipment_id, occurred_on);

