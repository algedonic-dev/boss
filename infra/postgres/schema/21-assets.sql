-- =========================================================================
-- 21-assets.sql — Assets — serial-numbered physical units + append-only asset event log.
-- =========================================================================


-- -----------------------------------------------------------------------------
-- Assets (serial-numbered physical units + append-only event log)
--
-- `assets` is a current-state projection rebuildable from `asset_events`.
-- The events table is the source of truth; the assets table is a summary
-- for fast reads and JOIN targets (account assets, warranty lookups, etc.).
-- -----------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS assets (
    asset_id           TEXT PRIMARY KEY,
    oem_serial          TEXT,
    -- Nullable: identity-first. An asset is `registered` (it exists)
    -- before it is identified, so `sku` is NULL until an Identified (or
    -- sku-bearing Received) event sets it. When non-null it must name a
    -- real catalog model — the FK enforces that, and an unidentified
    -- asset simply has no model-derived attributes yet.
    sku                 TEXT REFERENCES asset_models(sku),
    phase               TEXT NOT NULL CHECK (phase IN (
        'registered', 'received', 'triaging', 'refurbing', 'qa', 'ready',
        'shipped', 'installed', 'out-for-service', 'decommissioned'
    )),
    -- The typed custody edge (Q5): who HOLDS the asset — an account
    -- (device at a customer site), a location (brewhouse equipment).
    -- NULL pair = in stock / unheld. Validated via R2's edge
    -- registry once it lands.
    holder_kind          TEXT,
    holder_id            TEXT,
    warranty_through    DATE,                   -- NULL = out of warranty
    open_ticket_count   INTEGER NOT NULL DEFAULT 0,
    first_seen          DATE NOT NULL,
    last_event_at       DATE NOT NULL,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);


CREATE INDEX IF NOT EXISTS assets_phase ON assets(phase);

CREATE INDEX IF NOT EXISTS assets_holder ON assets(holder_kind, holder_id) WHERE holder_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS assets_warranty ON assets(warranty_through) WHERE warranty_through IS NOT NULL;


-- Per-asset open service tickets, maintained by PgAssets::append. The
-- existence of this table lets the projection update incrementally —
-- on a new ServiceJobClosed event, we can check whether the ticket
-- is actually open by looking it up here instead of replaying the full
-- event log for the asset_id.
--
-- It also turns the "list open tickets" and "open tickets per account"
-- queries from full event-log scans into simple selects.
CREATE TABLE IF NOT EXISTS asset_open_tickets (
    ticket_id      TEXT PRIMARY KEY,
    asset_id      TEXT NOT NULL REFERENCES assets(asset_id) ON DELETE CASCADE,
    summary        TEXT NOT NULL DEFAULT '',
    opened_on      DATE NOT NULL,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS asset_open_tickets_asset_id
    ON asset_open_tickets(asset_id);

CREATE INDEX IF NOT EXISTS asset_open_tickets_opened
    ON asset_open_tickets(opened_on DESC);


-- Events are the source of truth. The FK points from the projection TO
-- events (conceptually), not the other way around. asset_id is NOT
-- FK'd to assets because events arrive first; the projection is rebuilt
-- after. A partial index enforces that asset_ids are consistent within the
-- events table itself.
CREATE TABLE IF NOT EXISTS asset_events (
    id                  TEXT PRIMARY KEY,
    asset_id           TEXT NOT NULL,
    ts                  DATE NOT NULL,
    actor_id            TEXT NOT NULL,          -- who caused it: employee id or automation:<name>
    kind                TEXT NOT NULL,           -- discriminator: Received, Sold, etc.
    payload             JSONB NOT NULL DEFAULT '{}', -- kind-specific fields
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);


-- Primary query: "give me everything that happened to asset X, oldest first."
CREATE INDEX IF NOT EXISTS asset_events_asset_id_ts ON asset_events(asset_id, ts);


-- Secondary: "which events happened in this time range?" for dashboards.
-- BRIN instead of btree: `asset_events` is strictly append-only and
-- time-ordered, the textbook BRIN case. Inserts are ~2x faster and the
-- per-event cost stays flat as the table grows (a btree's doubles).
-- Range scans via Bitmap Heap Scan stay fast. The `ORDER BY ts DESC
-- LIMIT N` query pattern regresses (falls back to sort) but no
-- production code uses it — every time-ordered read in the assets
-- adapter is compound with `WHERE asset_id = $1`, served by
-- `asset_events_asset_id_ts` above.
CREATE INDEX IF NOT EXISTS asset_events_ts ON asset_events USING BRIN (ts);


-- Secondary: "all events of a given kind" for analytics / monitoring.
CREATE INDEX IF NOT EXISTS asset_events_kind ON asset_events(kind);


-- Per-asset Parts (docs/architecture-decisions.md §Primitives &
-- information architecture). Per-instance state that
-- the catalog Model doesn't capture: each installed unit has its
-- own firmware version + module set + license tier, and its own
-- set of attached accessories (handpieces, toner heads, scanners,
-- whatever the equipment kind defines). Needed for OTA update
-- targeting, service-call preparation, and intake state capture.
-- Exposed through the Parts primitive (boss-core::primitives::Part)
-- via GET /api/assets/assets/{id}/parts.
CREATE TABLE IF NOT EXISTS asset_software_configs (
    asset_id        TEXT PRIMARY KEY REFERENCES assets(asset_id) ON DELETE CASCADE,
    firmware_version TEXT NOT NULL,
    modules          JSONB NOT NULL DEFAULT '[]'::jsonb,
    license_tier     TEXT NOT NULL DEFAULT 'standard',
    last_updated_on  DATE,
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT NOW()
);


CREATE TABLE IF NOT EXISTS asset_accessories (
    id              BIGSERIAL PRIMARY KEY,
    asset_id       TEXT NOT NULL REFERENCES assets(asset_id) ON DELETE CASCADE,
    accessory_kind  TEXT NOT NULL,
    serial          TEXT,
    installed_on    DATE,
    removed_on      DATE,
    notes           TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS asset_accessories_asset ON asset_accessories(asset_id);

CREATE INDEX IF NOT EXISTS asset_accessories_installed
    ON asset_accessories(asset_id) WHERE removed_on IS NULL;

