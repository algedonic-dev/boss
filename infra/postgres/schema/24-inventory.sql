-- =========================================================================
-- 24-inventory.sql — Inventory — parts stock, purchase orders, vendor bills + vendor CRM.
-- =========================================================================


-- -----------------------------------------------------------------------------
-- Inventory (parts stock levels + purchase orders)
-- -----------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS inventory_items (
    part_sku            TEXT PRIMARY KEY,
    bin                 TEXT NOT NULL,
    on_hand             INTEGER NOT NULL DEFAULT 0,
    allocated           INTEGER NOT NULL DEFAULT 0,
    reorder_point       INTEGER NOT NULL DEFAULT 0,
    reorder_qty         INTEGER NOT NULL DEFAULT 0,
    trailing_90d_usage  INTEGER NOT NULL DEFAULT 0,
    -- The row's total stock value in cents — the stored, CONSERVED
    -- quantity (costing PR 6a, Q1: value-primary). Receives add the
    -- exact line total (qty × PO unit price); consumes drain the
    -- proportional share round(value × qty / on_hand), the final
    -- unit absorbing the remainder so zero on_hand forces zero
    -- value. Every GL amount posted for this row IS a value delta,
    -- so balance(1300) == Σ value_cents cannot drift — the old
    -- integer-cent weighted average leaked a truncated cent per
    -- receive, a $100–$200 valuation step at 10–20K-unit scale.
    -- Design + decisions: docs/design/inventory-value-conservation.md.
    value_cents         BIGINT NOT NULL DEFAULT 0,
    -- Display-only unit cost, derived — never an input to a GL
    -- amount, never writable (a stale writer that still tries to
    -- INSERT/UPDATE it fails loudly here instead of silently
    -- re-averaging).
    avg_cost_cents      BIGINT GENERATED ALWAYS AS
        (CASE WHEN on_hand > 0 THEN value_cents / on_hand ELSE 0 END) STORED,
    -- The supplier's agreed unit price in cents — what the vendor
    -- charges us, set as data (parts.toml / operator edit), never
    -- computed. PO lines are priced from this at placement;
    -- avg_cost_cents is OUR cost and emerges from receipts at PO
    -- prices. NULL = no agreed price; auto-restock refuses to place
    -- an unpriced PO (loud, not silent).
    vendor_price_cents  BIGINT,
    -- Category of vendor that supplies this part (matches
    -- vendors.category). Lets primary_vendor_for_part resolve a
    -- category-appropriate supplier for an auto-restock before the
    -- part has any PO history. Kept as data: seeded per-SKU from
    -- examples/brewery/seeds/parts.toml.
    vendor_category     TEXT,
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);


CREATE TABLE IF NOT EXISTS vendors (
    id                TEXT PRIMARY KEY,
    -- Identity-first: only `id` is required; descriptive fields are
    -- nullable and enriched after the vendor exists. The payment_terms
    -- CHECK still rejects bad values but passes NULL. `category` is
    -- nullable — an un-categorized vendor is simply not yet an
    -- auto-restock target (vendor_for matches on category).
    name              TEXT,
    -- Denormalised "primary contact" scalars, mirroring the is_primary
    -- row in `vendor_contacts` (the normalized source of truth, below).
    -- The PO / A-P paths read these for a fast single-contact render.
    contact_name      TEXT,
    contact_email     TEXT,
    phone             TEXT,
    city              TEXT,
    state             TEXT,
    lead_time_days    SMALLINT NOT NULL DEFAULT 7,
    payment_terms     TEXT CHECK (payment_terms IS NULL OR payment_terms IN (
        'net-30', 'net-45', 'net-60', 'prepaid'
    )),
    -- Free-text per-tenant taxonomy. Brewery uses
    -- grain-supplier / hops-supplier / yeast-bank / packaging /
    -- specialty-ingredients / equipment / general; the
    -- used-device-shop tenant uses networking-components / optics /
    -- electronics / consumables / packaging / calibration /
    -- general. Class registry validates per-tenant. Nullable: an
    -- un-categorized vendor isn't an auto-restock target yet.
    category          TEXT,
    -- How the system expects this vendor to behave (supply lead time,
    -- fulfilment, AP timing) — per-actor data the simulator reads to drive
    -- the vendor's supply chain. Bootstrapped from the category Class's
    -- `behavior_template` (classes.metadata) at birth, marked hand-set;
    -- becomes data-derived from real performance over time. NULL until set
    -- (an uncategorized vendor has no template). Shape: see
    -- boss_inventory::types::VendorBehavior.
    behavior          JSONB,
    active            BOOLEAN NOT NULL DEFAULT true,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT NOW()
);


CREATE TABLE IF NOT EXISTS purchase_orders (
    id              TEXT PRIMARY KEY,
    vendor_id       TEXT REFERENCES vendors(id),
    -- Identity-first: a Draft PO can exist as a bare identity. Vendor,
    -- lines, and placement dates are required only to PLACE it
    -- (enforced at the API layer by PurchaseOrder::validate_placement
    -- when status leaves 'draft'), so they're nullable here.
    vendor          TEXT,
    status          TEXT NOT NULL CHECK (status IN (
        'draft', 'submitted', 'acknowledged', 'in-transit', 'received', 'closed'
    )),
    placed_on       DATE,
    expected_on     DATE,
    received_on     DATE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);


CREATE INDEX IF NOT EXISTS purchase_orders_status ON purchase_orders(status);


CREATE TABLE IF NOT EXISTS purchase_order_lines (
    po_id           TEXT NOT NULL REFERENCES purchase_orders(id) ON DELETE CASCADE,
    part_sku        TEXT NOT NULL,
    qty             INTEGER NOT NULL,
    unit_cost_cents BIGINT NOT NULL,
    currency        TEXT NOT NULL DEFAULT 'USD' CHECK (length(currency) = 3),
    PRIMARY KEY (po_id, part_sku)
);


-- -----------------------------------------------------------------------------
-- Vendor invoices + three-way match
--
-- Models the A/P specialist's daily work: receive vendor bills,
-- compare each to its originating PO (header + line totals) and
-- the receipt on file, auto-approve the matched ones, and flag
-- the mismatches for human review. Approve and pay post to the GL
-- (the dispatcher routes them to finance.bill.{approved,paid} facts).
--
-- Statuses:
--   received   — invoice arrived, not yet compared
--   matched    — three-way match succeeded, ready for payment
--   mismatched — match failed, needs A/P review
--   approved   — human (or auto-approve) cleared it for payment
--   paid       — payment issued
--
-- Amount lives on the invoice itself; discrepancy_cents captures
-- the gap between the invoice total and the PO total when a
-- mismatch is detected (positive = vendor overbilled, negative
-- = vendor undershipped).
-- -----------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS vendor_invoices (
    id                TEXT PRIMARY KEY,
    po_id             TEXT NOT NULL REFERENCES purchase_orders(id) ON DELETE CASCADE,
    vendor            TEXT NOT NULL,
    vendor_invoice_no TEXT NOT NULL,
    amount_cents      BIGINT NOT NULL,
    currency          TEXT NOT NULL DEFAULT 'USD' CHECK (length(currency) = 3),
    received_on       DATE NOT NULL,
    matched_on        DATE,
    approved_on       DATE,
    paid_on           DATE,
    status            TEXT NOT NULL CHECK (status IN (
        'received', 'matched', 'mismatched', 'approved', 'paid'
    )) DEFAULT 'received',
    discrepancy_cents BIGINT,
    -- Free-text discrepancy kind (nullable: a clean match carries none);
    -- tenants extend via the Class registry under
    -- (subject_kind='vendor-invoice'). Validation lives at the inventory
    -- API boundary, not a DB CHECK.
    discrepancy_kind  TEXT,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT NOW()
);


CREATE INDEX IF NOT EXISTS vendor_invoices_po       ON vendor_invoices(po_id);

CREATE INDEX IF NOT EXISTS vendor_invoices_status   ON vendor_invoices(status);

CREATE INDEX IF NOT EXISTS vendor_invoices_received ON vendor_invoices(received_on DESC);


-- -----------------------------------------------------------------------------
-- Vendor CRM — contacts, interactions, account team, contracts.
--
-- Separate tables (with shared UI components) for the Vendor Subject's
-- CRM. Schemas parallel the account-side CRM (account_contacts,
-- account_notes, account_team_members) but diverge where vendor
-- relationships need commercial fields the customer side doesn't.
-- See docs/design/procurement-team-needs.md.
--
-- Declared after purchase_orders + vendor_invoices because
-- vendor_interactions.linked_po_id FKs purchase_orders.
-- -----------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS vendor_contacts (
    id               TEXT PRIMARY KEY,
    vendor_id        TEXT NOT NULL REFERENCES vendors(id) ON DELETE CASCADE,
    name             TEXT NOT NULL,
    role             TEXT NOT NULL CHECK (role IN (
        'sales-rep',
        'account-manager',
        'customer-service',
        'technical-support',
        'finance-ap',
        'executive'
    )),
    email            TEXT NOT NULL,
    phone            TEXT,
    territory        TEXT,                   -- optional region assignment (e.g. 'west-coast')
    specialties      JSONB NOT NULL DEFAULT '[]'::jsonb,  -- array of part-category strings
    is_primary       BOOLEAN NOT NULL DEFAULT false,
    relationship_start DATE,
    notes            TEXT,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT NOW()
);


CREATE INDEX IF NOT EXISTS vendor_contacts_vendor ON vendor_contacts(vendor_id);


-- Exactly one is_primary=true per vendor. Partial unique index keeps
-- the invariant without a trigger; non-primary rows can coexist freely.
CREATE UNIQUE INDEX IF NOT EXISTS vendor_contacts_one_primary
    ON vendor_contacts (vendor_id) WHERE is_primary = true;


CREATE TABLE IF NOT EXISTS vendor_interactions (
    id               TEXT PRIMARY KEY,
    vendor_id        TEXT NOT NULL REFERENCES vendors(id) ON DELETE CASCADE,
    -- Nullable so we can log generic vendor touches (e.g. a company-
    -- level price-sheet drop) where no specific contact is involved.
    vendor_contact_id TEXT REFERENCES vendor_contacts(id) ON DELETE SET NULL,
    actor_id        TEXT NOT NULL REFERENCES employees(id),
    kind             TEXT NOT NULL CHECK (kind IN (
        'note',           -- free-form text
        'call',           -- phone
        'meeting',        -- in-person or video
        'email',          -- outbound email summary
        'rfq',            -- request-for-quote conversation
        'negotiation',    -- part of a vendor-negotiation Job arc
        'escalation',     -- urgent push on a stuck commitment
        'interaction'     -- catch-all, used by auto-posted events
    )),
    body             TEXT NOT NULL,
    -- Structured "what the rep said they'd do" commitments. Array of
    -- {summary, due_by?, linked_po_id?} objects. Rendered as a
    -- follow-up list on the vendor KB.
    commitments      JSONB NOT NULL DEFAULT '[]'::jsonb,
    linked_po_id     TEXT REFERENCES purchase_orders(id) ON DELETE SET NULL,
    linked_part_sku  TEXT,
    linked_job_id    TEXT,                    -- FK to jobs(id), kept loose since boss-jobs owns that table
    occurred_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- Soft delete — same rationale as account_notes.
    deleted_at       TIMESTAMPTZ,
    deleted_by       TEXT REFERENCES employees(id)
);


CREATE INDEX IF NOT EXISTS vendor_interactions_vendor_time
    ON vendor_interactions(vendor_id, occurred_at DESC)
    WHERE deleted_at IS NULL;


CREATE INDEX IF NOT EXISTS vendor_interactions_contact
    ON vendor_interactions(vendor_contact_id)
    WHERE deleted_at IS NULL;


CREATE TABLE IF NOT EXISTS vendor_account_team (
    id            TEXT PRIMARY KEY,
    vendor_id     TEXT NOT NULL REFERENCES vendors(id) ON DELETE CASCADE,
    employee_id   TEXT NOT NULL REFERENCES employees(id),
    role          TEXT NOT NULL CHECK (role IN (
        'primary',        -- single owning buyer on the Boss side
        'backup',         -- fills in when primary is out
        'escalation',     -- senior buyer / procurement manager
        'technical-liaison', -- MET/engineering touchpoint for spec Qs
        'finance-contact' -- A/P owner for disputes and discrepancies
    )),
    assigned_on   DATE NOT NULL DEFAULT CURRENT_DATE,
    notes         TEXT,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (vendor_id, role)  -- exactly one person per role per vendor
);


CREATE INDEX IF NOT EXISTS vendor_account_team_vendor
    ON vendor_account_team(vendor_id);

CREATE INDEX IF NOT EXISTS vendor_account_team_employee
    ON vendor_account_team(employee_id);


CREATE TABLE IF NOT EXISTS vendor_contracts (
    id                TEXT PRIMARY KEY,
    vendor_id         TEXT NOT NULL REFERENCES vendors(id) ON DELETE CASCADE,
    kind              TEXT NOT NULL CHECK (kind IN (
        'master-supply',  -- umbrella agreement
        'volume-commit',  -- annual volume → tier pricing
        'rate-card',      -- negotiated per-SKU pricing
        'rebate-program', -- volume rebate accrual terms
        'payment-terms',  -- Net-60, early-pay discount, etc.
        'nda',            -- standalone NDA
        'sla'             -- delivery / quality commitment
    )),
    title             TEXT NOT NULL,
    effective_on      DATE NOT NULL,
    expires_on        DATE,                    -- null = evergreen / manual renewal
    auto_renew        BOOLEAN NOT NULL DEFAULT false,
    -- Negotiated terms — structured per kind. For rate-card,
    -- `{ part_skus: [...], unit_cost_cents: ... }`; for volume-commit,
    -- `{ annual_units: X, tier_pricing: [{ min: 0, unit_cost_cents: N }, ...] }`;
    -- etc. Each kind is validated by boss-inventory at write time.
    terms             JSONB NOT NULL DEFAULT '{}'::jsonb,
    document_uri      TEXT,                    -- pointer to signed PDF in `documents`
    status            TEXT NOT NULL CHECK (status IN (
        'draft', 'active', 'expired', 'terminated'
    )) DEFAULT 'draft',
    signed_by_employee_id  TEXT REFERENCES employees(id),
    signed_at         TIMESTAMPTZ,
    notes             TEXT,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT NOW()
);


CREATE INDEX IF NOT EXISTS vendor_contracts_vendor
    ON vendor_contracts(vendor_id);

CREATE INDEX IF NOT EXISTS vendor_contracts_status_expires
    ON vendor_contracts(status, expires_on)
    WHERE status = 'active';


-- Vendor facts — accumulated from PO/delivery/invoice Steps.
CREATE TABLE IF NOT EXISTS vendor_facts (
    id              TEXT PRIMARY KEY,
    vendor_id       TEXT NOT NULL,
    fact_kind       TEXT NOT NULL,
    occurred_at     DATE NOT NULL,
    actor_id        TEXT,
    job_id          TEXT,
    step_id         TEXT,
    payload         JSONB NOT NULL DEFAULT '{}',
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);


CREATE INDEX IF NOT EXISTS vendor_facts_vendor ON vendor_facts(vendor_id, occurred_at DESC);

CREATE INDEX IF NOT EXISTS vendor_facts_kind ON vendor_facts(fact_kind);

-- Inventory ref-check seed rows: events that REFERENCE an existing
-- inventory_items row. The audit_log_ref_checks table +
-- audit_log_check_refs() trigger live in 02-events; these rows live
-- with the table they reference so the module stays independently
-- removable.
INSERT INTO audit_log_ref_checks (event_kind, field_path, ref_table, ref_column) VALUES
    ('inventory.transferred',         'part_sku',    'inventory_items', 'part_sku'),
    ('inventory.item.consumed',       'part_sku',    'inventory_items', 'part_sku')
ON CONFLICT (event_kind, field_path) DO NOTHING;

