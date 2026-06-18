-- =========================================================================
-- 23-commerce.sql — Commerce — opportunities, invoices + line items, service agreements.
-- =========================================================================


-- -----------------------------------------------------------------------------
-- Commerce (opportunities, invoices, revenue)
-- -----------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS opportunities (
    id              TEXT PRIMARY KEY,
    account_id       TEXT NOT NULL,
    owner_id        TEXT NOT NULL,
    stage           TEXT NOT NULL CHECK (stage IN (
        'lead', 'qualified', 'quoted', 'won', 'lost'
    )),
    value_cents     BIGINT NOT NULL,
    currency        TEXT NOT NULL DEFAULT 'USD' CHECK (length(currency) = 3),
    opened_on       DATE NOT NULL,
    closed_on       DATE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);


CREATE INDEX IF NOT EXISTS opportunities_stage ON opportunities(stage);

CREATE INDEX IF NOT EXISTS opportunities_account ON opportunities(account_id);

CREATE INDEX IF NOT EXISTS opportunities_owner ON opportunities(owner_id);


-- Invoices are line-item based. `amount_cents` on the header is a
-- cached rollup of the associated `invoice_line_items` rows (the sum
-- invariant is checked in the commerce adapter on write); each line
-- carries its own revenue category and source reference.
CREATE TABLE IF NOT EXISTS invoices (
    id              TEXT PRIMARY KEY,
    account_id       TEXT NOT NULL,
    issued_on       DATE NOT NULL,
    due_on          DATE NOT NULL,
    paid_on         DATE,
    -- AR lifecycle status; no DB CHECK. The Class registry validates
    -- values at the commerce API boundary under
    -- (subject_kind='invoice', member_attribute='status').
    status          TEXT NOT NULL,
    amount_cents    BIGINT NOT NULL,
    -- Sales tax collected on this invoice — additive to amount_cents
    -- (the account owes revenue + tax). Jurisdiction is the state
    -- code we remitted under (`US-CA`, `US-TX`, ...). Zero + NULL when
    -- the invoice is exempt or untaxed.
    tax_cents       BIGINT NOT NULL DEFAULT 0 CHECK (tax_cents >= 0),
    tax_jurisdiction TEXT,
    -- How the account paid. NULL = single-shot tenants that don't
    -- model bank float (commerce emits the finance.invoice.paid
    -- shortcut). When set, the bank-settlement POST drives the
    -- two-phase flow instead (finance.payment.received →
    -- finance.payment.settled).
    payment_method  TEXT CHECK (payment_method IN ('ach', 'wire', 'check', 'card')),
    currency        TEXT NOT NULL DEFAULT 'USD' CHECK (length(currency) = 3),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);


CREATE INDEX IF NOT EXISTS invoices_status ON invoices(status);

CREATE INDEX IF NOT EXISTS invoices_account ON invoices(account_id);

CREATE INDEX IF NOT EXISTS invoices_due ON invoices(due_on);


-- One line item per billable thing on an invoice. Revenue category
-- lives here, not on the invoice header, so a single document can
-- mix new-device sale + service contract + training visit with three
-- different revenue treatments. Downstream `GET /api/commerce/revenue`
-- derives monthly revenue via GROUP BY over this table.
CREATE TABLE IF NOT EXISTS invoice_line_items (
    id                  TEXT PRIMARY KEY,
    invoice_id          TEXT NOT NULL REFERENCES invoices(id) ON DELETE CASCADE,
    -- Free-text per-tenant taxonomy; no DB CHECK. The Class registry
    -- validates values per-tenant when a tenant.toml authors them under
    -- `(subject_kind='invoice', member_attribute='revenue_category')`.
    revenue_category    TEXT NOT NULL,
    amount_cents        BIGINT NOT NULL,
    currency            TEXT NOT NULL DEFAULT 'USD' CHECK (length(currency) = 3),
    description         TEXT NOT NULL,
    ref_id              TEXT,
    -- Finished-goods line bookkeeping. When `sku` is set, the
    -- invoice-creation handler decrements
    -- finished_product_inventory.on_hand by `qty` in the same
    -- tx and stamps `cost_basis_cents` from the FG row's
    -- weighted moving average. The `invoice_issued` posting
    -- rule reads these to emit matching COGS lines (DR 5100 /
    -- CR 1320) in the same JE, making revenue + COGS
    -- structurally inseparable for FG sales. NULL on
    -- non-FG lines (service, contracts).
    sku                 TEXT,
    qty                 INTEGER,
    cost_basis_cents    BIGINT,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);


-- The FG-line columns above are added idempotently so a DB created
-- before they existed still picks them up. Fresh DBs already have them
-- from the CREATE TABLE block.
ALTER TABLE invoice_line_items ADD COLUMN IF NOT EXISTS sku TEXT;

ALTER TABLE invoice_line_items ADD COLUMN IF NOT EXISTS qty INTEGER;

ALTER TABLE invoice_line_items ADD COLUMN IF NOT EXISTS cost_basis_cents BIGINT;


CREATE INDEX IF NOT EXISTS invoice_line_items_invoice ON invoice_line_items(invoice_id);

CREATE INDEX IF NOT EXISTS invoice_line_items_category ON invoice_line_items(revenue_category);



-- -----------------------------------------------------------------------------
-- Service agreements (recurring revenue contracts)
-- -----------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS service_agreements (
    id                  TEXT PRIMARY KEY,
    account_id           TEXT NOT NULL,
    -- Free-text per-tenant taxonomy. Brewery uses
    -- distribution-contract / taproom-supply / event-package;
    -- the used-device-shop tenant uses full-service / parts-only
    -- / pm-only / extended-warranty. Class registry validates
    -- per-tenant.
    type                TEXT NOT NULL,
    status              TEXT NOT NULL CHECK (status IN (
        'draft', 'active', 'expired', 'cancelled', 'pending-renewal'
    )),
    start_date          DATE NOT NULL,
    end_date            DATE NOT NULL,
    annual_value_cents  BIGINT NOT NULL,
    currency            TEXT NOT NULL DEFAULT 'USD' CHECK (length(currency) = 3),
    billing_frequency   TEXT NOT NULL CHECK (billing_frequency IN (
        'monthly', 'quarterly', 'annual'
    )),
    auto_renew          BOOLEAN NOT NULL DEFAULT true,
    covers_parts        BOOLEAN NOT NULL DEFAULT false,
    covers_labor        BOOLEAN NOT NULL DEFAULT true,
    covers_travel       BOOLEAN NOT NULL DEFAULT false,
    pm_visits_per_year  SMALLINT NOT NULL DEFAULT 2,
    response_sla_hours  SMALLINT NOT NULL DEFAULT 24,
    owner_id            TEXT NOT NULL,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);


CREATE INDEX IF NOT EXISTS service_agreements_account ON service_agreements(account_id);

CREATE INDEX IF NOT EXISTS service_agreements_status ON service_agreements(status);

CREATE INDEX IF NOT EXISTS service_agreements_end ON service_agreements(end_date);


CREATE TABLE IF NOT EXISTS agreement_assets (
    agreement_id    TEXT NOT NULL REFERENCES service_agreements(id) ON DELETE CASCADE,
    asset_id       TEXT NOT NULL,
    added_on        DATE NOT NULL,
    removed_on      DATE,
    PRIMARY KEY (agreement_id, asset_id)
);


-- Tax + payment-method columns, added idempotently so a DB created
-- before they existed still picks them up. Fresh DBs already have them
-- from the CREATE TABLE block above.
ALTER TABLE invoices ADD COLUMN IF NOT EXISTS tax_cents BIGINT NOT NULL DEFAULT 0;

ALTER TABLE invoices ADD COLUMN IF NOT EXISTS tax_jurisdiction TEXT;

ALTER TABLE invoices ADD COLUMN IF NOT EXISTS payment_method TEXT;

ALTER TABLE invoices
    DROP CONSTRAINT IF EXISTS invoices_tax_cents_check;

ALTER TABLE invoices
    ADD CONSTRAINT invoices_tax_cents_check CHECK (tax_cents >= 0);

ALTER TABLE invoices
    DROP CONSTRAINT IF EXISTS invoices_payment_method_check;

ALTER TABLE invoices
    ADD CONSTRAINT invoices_payment_method_check
    CHECK (payment_method IS NULL OR payment_method IN ('ach', 'wire', 'check', 'card'));

