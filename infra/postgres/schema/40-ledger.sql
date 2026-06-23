-- =========================================================================
-- 40-ledger.sql — Ledger — financial facts, general ledger, payroll, tax, AP bills.
-- =========================================================================


-- -----------------------------------------------------------------------------
-- Financial facts
-- Per docs/architecture-decisions.md §Finance & ledger: a purpose-built synchronous fact log
-- that is written in the same transaction as the domain row it describes.
-- Rules in boss-ledger consume rows from here and project journal
-- entries. audit_log stays as the async broad-strokes event mirror.
-- -----------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS financial_facts (
    id              UUID PRIMARY KEY,
    kind            TEXT NOT NULL,            -- 'finance.invoice.issued', 'finance.bill.paid', ...
    happened_on     DATE NOT NULL,            -- business date of the event
    recorded_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    payload         JSONB NOT NULL,           -- kind-specific schema
    source_table    TEXT,                     -- 'invoices', 'vendor_invoices', NULL for manual
    source_id       TEXT,                     -- business-entity id
    created_by      TEXT NOT NULL
);


-- Idempotent re-emission: the same (kind, source) tuple can be written
-- repeatedly during replay and only the first one sticks. Postgres
-- treats NULLs as distinct in a unique index, so manual facts (where
-- source_table/source_id are NULL) don't collide with each other — that
-- is the desired behavior.
CREATE UNIQUE INDEX IF NOT EXISTS financial_facts_kind_source
    ON financial_facts (kind, source_table, source_id);


-- Primary access pattern: walk facts chronologically during a rebuild.
CREATE INDEX IF NOT EXISTS financial_facts_happened_on
    ON financial_facts (happened_on, kind);


-- -----------------------------------------------------------------------------
-- General Ledger projection
-- Per docs/architecture-decisions.md §Finance & ledger: chart of accounts, versioned posting
-- rules, and journal entries as a projection over financial_facts. Live
-- posting runs inside the same tx as the fact write; the rebuild command
-- re-projects from facts when rules or chart evolve.
-- -----------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS gl_accounts (
    id              UUID PRIMARY KEY,
    code            TEXT NOT NULL UNIQUE,         -- '1100', '4100', etc.
    name            TEXT NOT NULL,
    kind            TEXT NOT NULL CHECK (kind IN (
        'asset', 'liability', 'equity', 'revenue', 'expense'
    )),
    normal_side     TEXT NOT NULL CHECK (normal_side IN ('debit', 'credit')),
    parent_id       UUID REFERENCES gl_accounts(id),
    is_active       BOOLEAN NOT NULL DEFAULT true,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    retired_at      TIMESTAMPTZ
);


-- Starter chart of accounts — brewery-shaped (the OSS demo tenant).
-- Operators add/rename accounts via /admin/gl-accounts; this seed
-- is idempotent via ON CONFLICT DO NOTHING so re-applying is safe.
-- Tenants with different domain shapes (e.g. used-device-shop) ship
-- their own overlay seed under examples/<tenant>/seeds/.
INSERT INTO gl_accounts (id, code, name, kind, normal_side) VALUES
    ('00000000-0000-0000-0000-000000001000', '1000', 'Cash', 'asset', 'debit'),
    ('00000000-0000-0000-0000-000000001010', '1010', 'Cash in Transit', 'asset', 'debit'),
    ('00000000-0000-0000-0000-000000001100', '1100', 'Accounts Receivable', 'asset', 'debit'),
    -- Inventory three-tier (Model B cost flow): raw materials
    -- enter at PO line cost (DR 1300 / CR 2100 A/P); production
    -- consumes raw into WIP (DR 1310 / CR 1300); packaging
    -- transfers WIP to finished goods at standard cost (DR 1320 /
    -- CR 1310); sale recognizes COGS against FG (DR 5100 / CR 1320).
    -- See docs/design/correctness-protocol.md.
    ('00000000-0000-0000-0000-000000001300', '1300', 'Inventory — Raw Materials', 'asset', 'debit'),
    ('00000000-0000-0000-0000-000000001310', '1310', 'Inventory — Work in Process', 'asset', 'debit'),
    ('00000000-0000-0000-0000-000000001320', '1320', 'Inventory — Finished Goods', 'asset', 'debit'),
    ('00000000-0000-0000-0000-000000001500', '1500', 'Fixed Assets', 'asset', 'debit'),
    -- Contra-asset: credit-normal balance reduces 1500's
    -- carrying value. Monthly depreciation periodic posts
    -- DR 6900 / CR 1510 per asset; retirement disposal
    -- reverses cost + accumulated-depreciation in one JE.
    ('00000000-0000-0000-0000-000000001510', '1510', 'Accumulated Depreciation', 'asset', 'credit'),
    ('00000000-0000-0000-0000-000000002100', '2100', 'Accounts Payable', 'liability', 'credit'),
    ('00000000-0000-0000-0000-000000002150', '2150', 'Payroll Liability', 'liability', 'credit'),
    -- Deferred Revenue: the target account for ASC 606 ratable
    -- invoice lines (docs/architecture-decisions.md §Finance & ledger).
    ('00000000-0000-0000-0000-000000002200', '2200', 'Deferred Revenue', 'liability', 'credit'),
    ('00000000-0000-0000-0000-000000002300', '2300', 'Sales Tax Payable', 'liability', 'credit'),
    ('00000000-0000-0000-0000-000000002310', '2310', 'Income Tax Payable', 'liability', 'credit'),
    ('00000000-0000-0000-0000-000000002320', '2320', 'Excise Tax Payable', 'liability', 'credit'),
    ('00000000-0000-0000-0000-000000003000', '3000', 'Retained Earnings', 'equity', 'credit'),
    -- Revenue side. Category labels on invoice lines (new-sales,
    -- used-sales, service, parts, contracts) drive routing into
    -- these accounts via the invoice_issued posting rule.
    ('00000000-0000-0000-0000-000000004100', '4100', 'Revenue — Wholesale Beer Sales', 'revenue', 'credit'),
    ('00000000-0000-0000-0000-000000004110', '4110', 'Revenue — Direct-to-Consumer', 'revenue', 'credit'),
    ('00000000-0000-0000-0000-000000004120', '4120', 'Revenue — Taproom', 'revenue', 'credit'),
    ('00000000-0000-0000-0000-000000004130', '4130', 'Revenue — Seasonal & Specialty', 'revenue', 'credit'),
    ('00000000-0000-0000-0000-000000004140', '4140', 'Revenue — Distribution Contracts', 'revenue', 'credit'),
    -- COGS. 5100 is the catch-all today (production-side cost on
    -- new-sales / used-sales / service / contracts); 5200 is the
    -- packaging-heavy "parts" category. A more granular split
    -- (5100 ingredients vs 5300 production labor) is a follow-up.
    ('00000000-0000-0000-0000-000000005100', '5100', 'COGS — Beer Production', 'expense', 'debit'),
    ('00000000-0000-0000-0000-000000005200', '5200', 'COGS — Packaging', 'expense', 'debit'),
    ('00000000-0000-0000-0000-000000006100', '6100', 'Operating Expense — Payroll', 'expense', 'debit'),
    ('00000000-0000-0000-0000-000000006200', '6200', 'Operating Expense — Rent', 'expense', 'debit'),
    ('00000000-0000-0000-0000-000000006300', '6300', 'Operating Expense — General', 'expense', 'debit'),
    ('00000000-0000-0000-0000-000000006400', '6400', 'Payroll Taxes & Benefits', 'expense', 'debit'),
    ('00000000-0000-0000-0000-000000006500', '6500', 'Income Tax Expense', 'expense', 'debit'),
    ('00000000-0000-0000-0000-000000006550', '6550', 'Excise Tax Expense', 'expense', 'debit'),
    -- 6700 Bad Debt Expense — counterpart for finance.invoice.written_off
    -- (DR 6700 / CR 1100). Past-due invoices that never get collected
    -- roll off A/R into this expense account instead of accumulating as
    -- permanent fake receivables.
    ('00000000-0000-0000-0000-000000006700', '6700', 'Bad Debt Expense', 'expense', 'debit'),
    -- 6900 Depreciation Expense — counterpart for the monthly
    -- `depreciation-run` JobKind (DR 6900 / CR 1510) and for
    -- the retirement-disposal JE that reverses cost basis.
    -- Straight-line depreciation over the asset's useful life
    -- (catalog-driven; default 10y for brewery vessels, 5y for
    -- pumps + valves, 25y for buildings + structural).
    ('00000000-0000-0000-0000-000000006900', '6900', 'Depreciation Expense', 'expense', 'debit')
ON CONFLICT (code) DO NOTHING;


CREATE TABLE IF NOT EXISTS gl_rule_versions (
    id              UUID PRIMARY KEY,
    version         INT NOT NULL UNIQUE,
    effective_from  TIMESTAMPTZ NOT NULL,
    description     TEXT NOT NULL,
    is_active       BOOLEAN NOT NULL DEFAULT false
);


-- Exactly one active version at a time. Partial unique index gives us
-- that invariant without a trigger.
CREATE UNIQUE INDEX IF NOT EXISTS gl_rule_versions_one_active
    ON gl_rule_versions (is_active) WHERE is_active = true;


INSERT INTO gl_rule_versions (id, version, effective_from, description, is_active)
VALUES (
    '00000000-0000-0000-0000-000000000001',
    1,
    '2026-04-18 00:00:00+00',
    'BOSS RuleSet — AR/AP lifecycle, payroll, tax accrual + remittance, COGS, inventory transfer, ASC 606 deferred revenue + periodic recognition, period close.',
    true
) ON CONFLICT (version) DO NOTHING;


CREATE TABLE IF NOT EXISTS gl_periods (
    id              UUID PRIMARY KEY,
    kind            TEXT NOT NULL CHECK (kind IN ('month', 'year')),
    starts_on       DATE NOT NULL,
    ends_on         DATE NOT NULL,
    status          TEXT NOT NULL CHECK (status IN ('open', 'locked')) DEFAULT 'open',
    locked_at       TIMESTAMPTZ,
    locked_by       TEXT,
    locked_rule_version_id  UUID REFERENCES gl_rule_versions(id),
    locked_checksum TEXT,
    UNIQUE (kind, starts_on)
);


CREATE INDEX IF NOT EXISTS gl_periods_range ON gl_periods (starts_on, ends_on);


CREATE TABLE IF NOT EXISTS gl_journal_entries (
    id              UUID PRIMARY KEY,
    fact_id         UUID NOT NULL REFERENCES financial_facts(id),
    rule_version_id UUID NOT NULL REFERENCES gl_rule_versions(id),
    posted_on       DATE NOT NULL,
    period_id       UUID NOT NULL REFERENCES gl_periods(id),
    memo            TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (fact_id, rule_version_id)
);


CREATE INDEX IF NOT EXISTS gl_journal_entries_posted ON gl_journal_entries (posted_on);

CREATE INDEX IF NOT EXISTS gl_journal_entries_period ON gl_journal_entries (period_id);


CREATE TABLE IF NOT EXISTS gl_journal_lines (
    id              UUID PRIMARY KEY,
    journal_entry_id UUID NOT NULL REFERENCES gl_journal_entries(id) ON DELETE CASCADE,
    account_id      UUID NOT NULL REFERENCES gl_accounts(id),
    debit_cents     BIGINT NOT NULL DEFAULT 0,
    credit_cents    BIGINT NOT NULL DEFAULT 0,
    currency        TEXT NOT NULL DEFAULT 'USD' CHECK (length(currency) = 3),
    memo            TEXT,
    sort_order      SMALLINT NOT NULL DEFAULT 0,
    CHECK (debit_cents >= 0 AND credit_cents >= 0),
    CHECK (NOT (debit_cents > 0 AND credit_cents > 0)),
    CHECK (NOT (debit_cents = 0 AND credit_cents = 0))
);


CREATE INDEX IF NOT EXISTS gl_journal_lines_entry ON gl_journal_lines (journal_entry_id);

CREATE INDEX IF NOT EXISTS gl_journal_lines_account ON gl_journal_lines (account_id);


-- -----------------------------------------------------------------------------
-- GL daily rollup — convenience projection over the journal.
-- A second-level projection (the journal is itself a projection of
-- financial_facts): pre-aggregates each account's debit + credit totals
-- per posted_on date, plus the cash attributed to the account that day.
-- The financial statements (trial balance, balance sheet, income
-- statement, cash flow) read THIS table instead of GROUP-BY-ing the full
-- gl_journal_lines × gl_journal_entries join on every request — turning a
-- ~400k-line scan into a ~few-thousand-row sum. Maintained two ways that
-- must agree (the principle behind replay-check):
--   * live    — incremented per journal entry inside post_fact_in_tx's
--               insert_entry (same tx as the line write)
--   * rebuild — TRUNCATE + full re-aggregate at the end of the ledger
--               rebuild (boss-ledger::rebuild)
-- `cash_flow_cents` is the net cash attributed to this account on this
-- day: for every journal entry that moves the cash pool (1000 + 1010),
-- the pool's net change is split across the entry's non-pool offset
-- accounts in proportion to their credit-net share, with truncating
-- division (matching the rebuild SQL's trunc()::bigint). It is 0 for the
-- pool accounts themselves and for accounts on non-cash entries — so the
-- cash-flow statement sums this column over a window instead of
-- re-attributing the whole ledger per request.
-- -----------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS gl_account_daily (
    account_id      UUID NOT NULL REFERENCES gl_accounts(id),
    posted_on       DATE NOT NULL,
    debit_cents     BIGINT NOT NULL DEFAULT 0,
    credit_cents    BIGINT NOT NULL DEFAULT 0,
    cash_flow_cents BIGINT NOT NULL DEFAULT 0,
    PRIMARY KEY (account_id, posted_on)
);


-- Statement queries filter by posted_on (as_of / from..to) then group by
-- account; the date index keeps the range scan tight.
CREATE INDEX IF NOT EXISTS gl_account_daily_posted ON gl_account_daily (posted_on);


-- ASC 606 revenue recognition — one row per ratable obligation, driven
-- by the `boss-ledger-recognize` scheduler
-- (docs/architecture-decisions.md §Finance & ledger).
CREATE TABLE IF NOT EXISTS revenue_schedules (
    id                        TEXT PRIMARY KEY,
    source_kind               TEXT NOT NULL,
    source_id                 TEXT NOT NULL,
    account_id               TEXT NOT NULL,
    revenue_category          TEXT NOT NULL,
    revenue_account           TEXT NOT NULL,
    deferred_account          TEXT NOT NULL,
    total_cents               BIGINT NOT NULL,
    start_date                DATE NOT NULL,
    end_date                  DATE NOT NULL,
    frequency                 TEXT NOT NULL CHECK (frequency IN ('monthly', 'quarterly')),
    recognized_to_date_cents  BIGINT NOT NULL DEFAULT 0,
    next_recognition_date     DATE NOT NULL,
    status                    TEXT NOT NULL CHECK (status IN ('active', 'paused', 'closed')),
    created_at                TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at                TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CHECK (end_date >= start_date),
    CHECK (recognized_to_date_cents >= 0 AND recognized_to_date_cents <= total_cents)
);


-- Scheduler sweep pattern: "what's due today?" — partial index on
-- `status='active'` keeps the scan bounded to live obligations only.
CREATE INDEX IF NOT EXISTS revenue_schedules_next ON revenue_schedules (next_recognition_date)
    WHERE status = 'active';


CREATE INDEX IF NOT EXISTS revenue_schedules_source
    ON revenue_schedules (source_kind, source_id);


-- Deferred constraint: every journal entry's debits must equal its credits
-- at commit time. Enforced by trigger because no declarative CHECK can
-- aggregate across rows.
CREATE OR REPLACE FUNCTION gl_journal_entries_balanced()
RETURNS TRIGGER AS $$
DECLARE
    dtotal BIGINT;
    ctotal BIGINT;
BEGIN
    SELECT COALESCE(SUM(debit_cents), 0), COALESCE(SUM(credit_cents), 0)
      INTO dtotal, ctotal
      FROM gl_journal_lines
      WHERE journal_entry_id = COALESCE(NEW.journal_entry_id, OLD.journal_entry_id);
    IF dtotal <> ctotal THEN
        RAISE EXCEPTION 'journal entry % is unbalanced: debits=% credits=%',
            COALESCE(NEW.journal_entry_id, OLD.journal_entry_id), dtotal, ctotal;
    END IF;
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;


CREATE CONSTRAINT TRIGGER gl_journal_lines_balanced_trg
    AFTER INSERT OR UPDATE OR DELETE ON gl_journal_lines
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION gl_journal_entries_balanced();


-- Reject writes to a locked period. Defense-in-depth with the application
-- check in `post_fact_in_tx` — a buggy caller that tries to write directly
-- still fails at the DB.
CREATE OR REPLACE FUNCTION gl_reject_write_to_locked_period()
RETURNS TRIGGER AS $$
DECLARE
    period_status TEXT;
BEGIN
    SELECT status INTO period_status FROM gl_periods WHERE id = NEW.period_id;
    IF period_status = 'locked' THEN
        RAISE EXCEPTION 'cannot write journal entry to locked period %', NEW.period_id
            USING ERRCODE = 'raise_exception';
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;


DROP TRIGGER IF EXISTS gl_journal_entries_period_lock_trg ON gl_journal_entries;

CREATE TRIGGER gl_journal_entries_period_lock_trg
    BEFORE INSERT ON gl_journal_entries
    FOR EACH ROW EXECUTE FUNCTION gl_reject_write_to_locked_period();


-- -----------------------------------------------------------------------------
-- Fact-projection registry — rebuilds `financial_facts` from `audit_log`.
-- Per the rebuilders doctrine, every projection must be derivable from
-- audit_log via a deterministic pure function. `financial_facts` is the
-- ledger's input and stays an independent log; this table
-- is the rebuild bridge: one row per (real-world event kind → fact kind),
-- describing how to project the event payload into a fact.
--
-- 1:1 contract: each `event_kind` projects to exactly one `fact_kind`.
-- Splitting two facts out of one event (e.g. an "approved AND paid"
-- bundle) is solved by emitting separate transition events upstream,
-- not by stuffing branching logic into the projection.
-- -----------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS gl_fact_projection_rules (
    event_kind        TEXT PRIMARY KEY,                 -- audit_log.kind to filter on
    fact_kind         TEXT NOT NULL,                    -- financial_facts.kind to emit
    source_table      TEXT NOT NULL,                    -- written verbatim into financial_facts.source_table
    source_id_path    TEXT NOT NULL,                    -- JSON pointer (RFC 6901) into event.payload, e.g. '/invoice_id'
    happened_on_path  TEXT,                             -- NULL → fall back to event.timestamp::date
    created_by_path   TEXT                              -- NULL → fall back to event.source
);


-- Seed rules. A rule may exist before any upstream code emits its
-- `event_kind`: it simply doesn't fire until audit_log has a matching
-- event, so the projection is ready the moment an emitter ships.
INSERT INTO gl_fact_projection_rules (event_kind, fact_kind, source_table, source_id_path, happened_on_path, created_by_path) VALUES
    ('commerce.invoice.created',           'finance.invoice.issued',     'invoices',          '/id',                 '/issued_on', NULL),
    -- Write-offs: commerce.invoice.written_off (emitted by the brewery
    -- `[counterparty.bad-debt-writeoff]` 60 sim-days after past-due)
    -- projects to finance.invoice.written_off. The posting rule in
    -- boss-ledger/src/rules.rs maps that to DR 6700 Bad Debt Expense
    -- / CR 1100 A/R, dropping the receivable from the books.
    ('commerce.invoice.written_off',       'finance.invoice.written_off','invoices',          '/id',                 '/issued_on', NULL),
    -- Burden absorption: labor + overhead capitalized into WIP at
    -- production-consume time. The bridge emits inventory.labor.absorbed
    -- (which inventory-api writes to audit_log + financial_facts), and on
    -- rebuild this rule re-creates the matching
    -- finance.inventory.transferred fact from audit_log alone — so the
    -- WIP balance stays correct through a TRUNCATE-then-replay rebuild.
    ('inventory.labor.absorbed',           'finance.inventory.transferred', 'ledger_labor_absorbed', '/source_id',       '/happened_on', NULL),
    -- WIP→FG cost transfer + FG→COGS recognition: products-api
    -- writes the live financial_facts row inside the same tx as
    -- the inventory delta, then emits products.produced /
    -- products.consumed events whose payloads carry source_id +
    -- happened_on. These rules reproduce the same financial_facts
    -- row on rebuild via the audit_log-only seed bundle path.
    -- Without them the WIP balance grows unbounded (absorption +
    -- raw transfers debit WIP but the matching FG credit vanishes
    -- on bundle import); the Q invariant catches the gap.
    ('products.produced',                  'finance.inventory.transferred', 'products_produce',      '/source_id',       '/happened_on', NULL),
    ('products.consumed',                  'finance.cogs.recognized',       'products_consume',      '/source_id',       '/happened_on', NULL),
    -- DELIBERATELY NOT mapped: `commerce.invoice.paid`. The
    -- `finance.invoice.paid` rule (rules.rs) is a single-shot
    -- "DR Cash / CR AR" — the "we don't model bank float"
    -- shortcut. A tenant whose AR-aging counterparty fires
    -- `commerce.invoice.paid` AND whose bank-clearing chain emits
    -- the canonical `ledger.payment.received` +
    -- `ledger.payment.settled` pair would double-credit AR if both
    -- rules fired (AR goes negative). So there is no auto-projection
    -- here: tenants that don't model bank float can still emit
    -- `finance.invoice.paid` facts directly via `record_fact_in_tx`
    -- (the manual-entry path), but no audit_log event projects to it.
    -- New emitters should prefer the two-phase pair below.
    ('inventory.vendor_invoice.approved',  'finance.bill.approved',      'vendor_invoices',   '/vendor_invoice_id',  '/approved_on', NULL),
    ('inventory.vendor_invoice.paid',      'finance.bill.paid',          'vendor_invoices',   '/vendor_invoice_id',  '/paid_on',   NULL),
    -- Ledger-owned general AP bills (rent, utilities, insurance, …),
    -- decoupled from the inventory parts vendor-invoice. The
    -- `expense-bill` step's `bill_category` (free text, routed by
    -- bill_accounts.toml) selects the debit; the credit is always 2100
    -- A/P. Same `finance.bill.{approved,paid}` fact kinds as the
    -- inventory path above, so the `bill_approved`/`bill_paid` posting
    -- rules (boss-ledger/src/rules.rs) are reused UNCHANGED. The event
    -- payload carries bill_id + bill_category + amount_cents + the date,
    -- so a TRUNCATE-then-replay rebuild re-derives the fact from
    -- audit_log alone (source_table is a provenance label, never joined).
    ('ledger.bill.approved',               'finance.bill.approved',      'ledger_bills',      '/bill_id',            '/approved_on', NULL),
    ('ledger.bill.paid',                   'finance.bill.paid',          'ledger_bills',      '/bill_id',            '/paid_on',   NULL),
    -- Model B inventory cost flow: `inventory.transferred`
    -- carries asset→asset moves (raw→WIP from parts.consume,
    -- WIP→FG from products.produce). `products.cogs.recognized`
    -- (below) carries the asset→expense move at sale time.
    -- source_id uses "<sku>@<timestamp>" so each transfer gets
    -- its own fact row while replays remain idempotent.
    ('inventory.transferred',              'finance.inventory.transferred', 'inventory_consume', '/source_id',       '/consumed_on', NULL),
    ('ledger.payment.received',            'finance.payment.received',   'bank_settlements',  '/settlement_id',      '/received_on', NULL),
    ('ledger.payment.settled',             'finance.payment.settled',    'bank_settlements',  '/settlement_id',      '/settled_on',  NULL),
    ('ledger.payroll.run',                 'finance.payroll.run',        'payroll_runs',      '/run_id',             '/run_date',  NULL),
    ('ledger.revenue.recognized',          'finance.revenue.recognized', 'revenue_schedules', '/source_id',          '/post_date', NULL),
    ('ledger.tax.accrued',                 'finance.tax.accrued',        'tax_filings',       '/filing_id',          '/posted_on', NULL),
    -- Standalone tax accrual (federal beer excise: DR 6550 / CR 2320 per
    -- brew batch) — no tax_filings row, so it CANNOT share the
    -- `/filing_id`-keyed `ledger.tax.accrued` rule above (that extracts a
    -- NULL source_id here and drops the fact on rebuild). Its own kind
    -- keys off `/accrual_id` (the live source_id) so the rebuild
    -- reproduces the same `finance.tax.accrued` fact. `tax_accruals` is a
    -- provenance label only — there is no such table; source_table is
    -- written verbatim and never joined.
    ('ledger.tax.accrual.recorded',        'finance.tax.accrued',        'tax_accruals',      '/accrual_id',         '/posted_on', NULL),
    ('ledger.tax.remitted',                'finance.tax.remitted',       'tax_filings',       '/filing_id',          '/filed_on',  NULL),
    ('ledger.period.closed',               'finance.period.closed',      'gl_periods',        '/period_id',          '/period_end', NULL),
    ('ledger.manual_entry.submitted',      'finance.manual.entry',       'manual_entries',    '/entry_id',           '/posted_on', '/created_by'),
    -- Seed-side opening balances + any other tenant-issued
    -- inventory-transferred call route through
    -- /api/ledger/inventory-transferred which emits this kind.
    -- The handler-side source_table is preserved in the event
    -- payload so rebuild reproduces the original tag (e.g.
    -- 'brewery_seed_opening_balance' for opening JEs). Without
    -- this rule, a TRUNCATE-then-replay rebuild would lose every
    -- opening balance because they're inserted via the handler,
    -- not via an upstream audit-event flow.
    ('ledger.inventory.transferred',       'finance.inventory.transferred', 'manual_inventory_transferred', '/source_id', '/happened_on', NULL)
ON CONFLICT (event_kind) DO NOTHING;


-- -----------------------------------------------------------------------------
-- Bank settlement projection — the two-phase payment float.
-- One row per
-- account payment. Row lands when the payment is received
-- (finance.payment.received fact posted); row flips to settled
-- when the bank-clearing generator sweeps it 1-3 business days
-- later (finance.payment.settled fact posted).
-- -----------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS bank_settlements (
    id                  TEXT PRIMARY KEY,
    -- NULLable so the commerce rebuilder can detach + reattach
    -- bank_settlements rows around a TRUNCATE-then-replay of
    -- invoices. The rebuild's intent (see boss-commerce
    -- src/rebuild.rs): UPDATE invoice_id=NULL, TRUNCATE invoices
    -- CASCADE (skipping bank_settlements), re-upsert invoices,
    -- bank_settlements rows survive with detached invoice_id
    -- pointers that snap back via the same deterministic
    -- inv-step-{step_id} key the live emitter mints. NOT NULL
    -- would force a full bank_settlements rebuild from
    -- audit_log too, which today's design treats as live state
    -- written by /api/ledger/bank-settlements/* endpoints.
    invoice_id          TEXT,
    received_on         DATE NOT NULL,
    expected_settle_on  DATE NOT NULL,
    settled_on          DATE,
    amount_cents        BIGINT NOT NULL CHECK (amount_cents > 0),
    bank_provider       TEXT NOT NULL,
    payment_method      TEXT NOT NULL CHECK (payment_method IN ('ach', 'wire', 'check', 'card')),
    status              TEXT NOT NULL CHECK (status IN ('pending', 'settled', 'returned')),
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);


CREATE INDEX IF NOT EXISTS bank_settlements_pending_due
    ON bank_settlements (expected_settle_on)
    WHERE status = 'pending';


CREATE INDEX IF NOT EXISTS bank_settlements_invoice
    ON bank_settlements (invoice_id);


-- -----------------------------------------------------------------------------
-- Payroll projection — biweekly paycheck runs. One `payroll_runs` row
-- per pay period; one `payroll_run_lines` row per employee per run. The
-- journal entry is aggregated per run, not per employee — the
-- per-employee detail lives in payroll_run_lines for the drill-down
-- view. Employer payroll taxes + benefits accrue to 2150 Payroll
-- Liability alongside employee withholding; the tax-authorities
-- generator unwinds 2150 via finance.tax.remitted when the next
-- 941/940 deadline hits.
-- -----------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS payroll_runs (
    id                  TEXT PRIMARY KEY,
    run_date            DATE NOT NULL,
    period_start        DATE NOT NULL,
    period_end          DATE NOT NULL,
    gross_cents         BIGINT NOT NULL CHECK (gross_cents >= 0),
    employer_tax_cents  BIGINT NOT NULL CHECK (employer_tax_cents >= 0),
    withheld_cents      BIGINT NOT NULL CHECK (withheld_cents >= 0),
    net_cents           BIGINT NOT NULL CHECK (net_cents >= 0),
    employee_count      INT NOT NULL CHECK (employee_count >= 0),
    provider            TEXT NOT NULL DEFAULT 'adp',
    status              TEXT NOT NULL CHECK (status IN ('draft', 'submitted', 'posted')),
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CHECK (net_cents = gross_cents - withheld_cents)
);


CREATE INDEX IF NOT EXISTS payroll_runs_run_date
    ON payroll_runs (run_date DESC);


CREATE TABLE IF NOT EXISTS payroll_run_lines (
    run_id          TEXT NOT NULL REFERENCES payroll_runs(id) ON DELETE CASCADE,
    employee_id     TEXT NOT NULL,
    gross_cents     BIGINT NOT NULL CHECK (gross_cents >= 0),
    withheld_cents  BIGINT NOT NULL CHECK (withheld_cents >= 0),
    net_cents       BIGINT NOT NULL CHECK (net_cents >= 0),
    department      TEXT NOT NULL,
    role            TEXT NOT NULL,
    PRIMARY KEY (run_id, employee_id),
    CHECK (net_cents = gross_cents - withheld_cents)
);


CREATE INDEX IF NOT EXISTS payroll_run_lines_employee
    ON payroll_run_lines (employee_id);


-- -----------------------------------------------------------------------------
-- Tax — sales tax rates + tax filings projection.
-- Sales tax
-- accrues per invoice line (inline via the `tax_lines` payload on
-- `finance.invoice.issued`, credit 2300 Sales Tax Payable). Payroll
-- tax accrues per run (employer-side 6400 + withheld already in
-- 2150). Corporate income tax accrues quarterly per estimate. The
-- `tax_authorities` sim generator sweeps balances into `tax_filings`
-- rows keyed by (kind, jurisdiction, period) and emits
-- `finance.tax.remitted` when the due date arrives, draining the
-- liability account back into 1000 Cash.
-- -----------------------------------------------------------------------------

-- Per-state sales-tax rate. Seeded with flat rates for the handful
-- of states Boss actually ships into today; adding a state later is
-- a row-level migration, not a code change. Rate is in basis points
-- (100 bps = 1.00%). Source: state DOR rate schedules as of 2026-01-01,
-- rounded to the nearest 25bps. Exempt accounts short-circuit before
-- this table is consulted.
CREATE TABLE IF NOT EXISTS sales_tax_rate_by_state (
    state           TEXT PRIMARY KEY,      -- two-letter US code ('CA', 'TX', ...)
    jurisdiction    TEXT NOT NULL,         -- fully-qualified filing jurisdiction ('US-CA')
    rate_bps        INTEGER NOT NULL CHECK (rate_bps >= 0 AND rate_bps <= 2000),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);


INSERT INTO sales_tax_rate_by_state (state, jurisdiction, rate_bps) VALUES
    ('CA', 'US-CA', 725),
    ('TX', 'US-TX', 625),
    ('FL', 'US-FL', 600),
    ('NY', 'US-NY', 800),
    ('IL', 'US-IL', 625),
    ('PA', 'US-PA', 600),
    ('OH', 'US-OH', 575),
    ('GA', 'US-GA', 400),
    ('NC', 'US-NC', 475),
    ('MI', 'US-MI', 600),
    ('AZ', 'US-AZ', 560),
    ('WA', 'US-WA', 650),
    ('MA', 'US-MA', 625),
    ('VA', 'US-VA', 530),
    ('NJ', 'US-NJ', 663),
    ('CO', 'US-CO', 290),
    ('MD', 'US-MD', 600),
    ('TN', 'US-TN', 700),
    ('IN', 'US-IN', 700),
    ('MO', 'US-MO', 425),
    ('WI', 'US-WI', 500),
    ('MN', 'US-MN', 688),
    ('OR', 'US-OR', 0),
    ('NH', 'US-NH', 0),
    ('MT', 'US-MT', 0),
    ('DE', 'US-DE', 0),
    ('AK', 'US-AK', 0)
ON CONFLICT (state) DO NOTHING;


-- Tax-kind reference data: maps each filing kind to the GL accounts it
-- touches + how its amount is derived. Reference data, not core code,
-- so a new tax regime is a row here rather than a code change +
-- migration. `tax_filings.kind` FKs this table.
-- `expense_account` is set only for kinds that accrue against an expense
-- (income tax → 6500); NULL means "no accrual, drains an existing
-- liability balance". `derive_basis` names the at-create amount
-- derivation the ledger runs (NULL = use the caller's amount).
CREATE TABLE IF NOT EXISTS tax_kinds (
    kind              TEXT PRIMARY KEY,
    liability_account TEXT NOT NULL REFERENCES gl_accounts(code),
    expense_account   TEXT REFERENCES gl_accounts(code),
    derive_basis      TEXT,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

INSERT INTO tax_kinds (kind, liability_account, expense_account, derive_basis) VALUES
    ('sales',       '2300', NULL,   'period-sales-tax'),
    ('income',      '2310', '6500', 'prior-quarter-net-income'),
    ('payroll_941', '2150', NULL,   'period-payroll-941'),
    ('payroll_940', '2150', NULL,   NULL),
    ('excise',      '2320', NULL,   'period-excise')
ON CONFLICT (kind) DO NOTHING;


CREATE TABLE IF NOT EXISTS tax_filings (
    id              TEXT PRIMARY KEY,
    kind            TEXT NOT NULL REFERENCES tax_kinds(kind),
    jurisdiction    TEXT NOT NULL,                  -- 'US-CA', 'US-FEDERAL', ...
    period_start    DATE NOT NULL,
    period_end      DATE NOT NULL,
    due_on          DATE NOT NULL,
    filed_on        DATE,
    amount_cents    BIGINT NOT NULL CHECK (amount_cents >= 0),
    -- Which liability account drained on remit. Denormalized from
    -- tax_kinds at create time so the tax-liability view can render
    -- without re-deriving from kind (tax_kinds.liability_account is the
    -- FK-checked source of truth).
    liability_account TEXT NOT NULL,
    status          TEXT NOT NULL CHECK (status IN ('accrued', 'filed', 'paid')),
    provider        TEXT NOT NULL DEFAULT 'self',   -- 'self', 'adp' (for 941/940), 'avalara', ...
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CHECK (period_end >= period_start)
);


-- Distinct filings per (kind, jurisdiction, period) so a replay can
-- upsert without collision. The PK carries the sim-generated id
-- (deterministic across runs), this unique index defends against a
-- real integration that reuses a provider-generated id for a
-- different period.
CREATE UNIQUE INDEX IF NOT EXISTS tax_filings_period
    ON tax_filings (kind, jurisdiction, period_start, period_end);


CREATE INDEX IF NOT EXISTS tax_filings_due
    ON tax_filings (due_on)
    WHERE status = 'accrued';


CREATE INDEX IF NOT EXISTS tax_filings_status
    ON tax_filings (status, due_on DESC);


-- -----------------------------------------------------------------------------
-- Ledger bills — general accounts-payable subledger owned by the GL.
-- A "bill" is a general AP concept (rent, utilities, insurance,
-- services, …), NOT coupled to an inventory purchase order or per-SKU
-- lines the way `vendor_invoices` is. The free `bill_category` is
-- routed to a debit account by bill_accounts.toml (boss-ledger/seeds/);
-- the credit is always 2100 A/P. `lines` is an opaque metadata bag (no
-- part_sku). Adding a new kind of spend is a JobKind writing a
-- `bill_category` + a bill_accounts.toml row — zero code. Posts via the
-- finance.bill.{approved,paid} rules.
-- -----------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS ledger_bills (
    id              TEXT PRIMARY KEY,
    vendor          TEXT NOT NULL,
    bill_category   TEXT NOT NULL,                  -- free text; routed by bill_accounts.toml
    amount_cents    BIGINT NOT NULL CHECK (amount_cents > 0),
    currency        TEXT NOT NULL DEFAULT 'USD' CHECK (length(currency) = 3),
    issued_on       DATE NOT NULL,
    due_on          DATE,
    approved_on     DATE,                           -- set at approval (= happened_on for the JE)
    paid_on         DATE,
    status          TEXT NOT NULL CHECK (status IN ('approved', 'paid')) DEFAULT 'approved',
    lines           JSONB NOT NULL DEFAULT '[]'::jsonb,   -- opaque metadata bag (no part_sku)
    memo            TEXT,
    created_by      TEXT NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);


CREATE INDEX IF NOT EXISTS ledger_bills_status   ON ledger_bills (status, due_on);

CREATE INDEX IF NOT EXISTS ledger_bills_issued   ON ledger_bills (issued_on DESC);

CREATE INDEX IF NOT EXISTS ledger_bills_category ON ledger_bills (bill_category);


-- ---------------------------------------------------------------------------
-- financial_facts.supersede — append-only correction path
-- ---------------------------------------------------------------------------
-- financial_facts is append-only, so a bad row is never DELETEd — it is
-- corrected with a "supersede" marker. The bad row stays in the table
-- for audit, gets a non-NULL `supersede_reason` explaining why,
-- optionally points to a corrected fact via `superseded_by`, and the
-- rebuild/replay paths skip it.
--
-- Conventions:
--   - `superseded_by IS NULL  AND supersede_reason IS NULL` → live row
--   - `superseded_by IS NULL  AND supersede_reason IS NOT NULL`
--                                        → retired row, no replacement
--   - `superseded_by IS NOT NULL` → retired row pointing to its
--                                   replacement (whose source_id
--                                   must differ from the retired
--                                   row's; the unique index still
--                                   enforces one active fact per
--                                   triple, so corrections must use
--                                   a fresh source_id like
--                                   "INV-001-v2").
--
-- `superseded_by` is a documentation pointer, not an FK — the
-- replacement fact's id may be drawn from a different domain or
-- created externally, so we don't enforce referential integrity.
ALTER TABLE financial_facts
    ADD COLUMN IF NOT EXISTS superseded_by    UUID;

ALTER TABLE financial_facts
    ADD COLUMN IF NOT EXISTS supersede_reason TEXT;

