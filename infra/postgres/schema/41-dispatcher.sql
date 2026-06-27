-- 41-dispatcher.sql — dispatcher rule registry (the reactive side-effect layer).
--
-- GENERATED from infra/dispatcher/rules.toml by infra/dispatcher/gen-seed.py —
-- do NOT hand-edit the seed rows; edit rules.toml and regenerate. The
-- dispatcher loads the ACTIVE rows from this table at startup (it no longer
-- reads rules.toml at runtime) and /api/dispatcher/rules serves them. The
-- `dispatcher_rules_seed_matches_toml` test guards this seed against drift.
--
-- Append-only + versioned like step_plugins: a new version of a rule supersedes
-- the prior active row (retire it, insert the new one); the partial unique index
-- keeps exactly one 'active' row per rule name.

CREATE TABLE IF NOT EXISTS dispatcher_rules (
    name        TEXT NOT NULL,
    version     INT  NOT NULL,
    status      TEXT NOT NULL CHECK (status IN ('draft', 'active', 'retired')),
    -- A rule is triggered by EXACTLY ONE of `on_event` (NATS event) or a
    -- `schedule_*` group (clock-driven). on_event is nullable now so a
    -- scheduled rule can omit it; the application enforces the XOR.
    on_event    TEXT,
    when_expr   TEXT,                 -- the rule's `when` predicate (NULL = always)
    do_steps    JSONB NOT NULL,       -- [{"handler": "...", "args": {...}}, ...]
    delay       TEXT,                 -- optional delay spec
    -- Clock trigger (all-or-nothing): cadence + anchor are both set for a
    -- scheduled rule, both NULL for an event rule. The dispatcher fires the
    -- rule on each sim-DAY the cadence selects (postponed onto a business
    -- day when schedule_calendar names one).
    schedule_cadence  TEXT,          -- daily|weekly|biweekly|monthly|quarterly|annually
    schedule_anchor   DATE,          -- cadence anchor date
    schedule_calendar TEXT,          -- optional business-calendar code (us-banking, …)
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (name, version)
);

CREATE UNIQUE INDEX IF NOT EXISTS dispatcher_rules_one_active_per_name
    ON dispatcher_rules (name) WHERE status = 'active';

-- Single-row cursor for the clock-driven schedule runner: the last sim-DAY
-- whose schedule rules have already been fired. The runner reads it at
-- startup, fires `(cursor, today]` (capped), and persists the advance —
-- so a restart resumes where it left off instead of replaying or skipping.
-- The CHECK pins the table to one row (id = 1).
CREATE TABLE IF NOT EXISTS dispatcher_clock_cursor (
    id           INT PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    last_sim_day DATE
);

INSERT INTO dispatcher_rules (name, version, status, on_event, when_expr, do_steps, delay, schedule_cadence, schedule_anchor, schedule_calendar) VALUES
  ('spawn-restock-on-low-inventory', 1, 'active', 'inventory.item.consumed', 'on_hand <= reorder_point AND NOT open_restock_exists(part_sku)', '[{"handler":"jobs.spawn","args":{"kind":"\"ingredient-restock\"","subject_kind":"\"vendor\"","subject":"vendor_for(part_sku)","metadata.part_sku":"part_sku","metadata.trigger_name":"\"inventory-reorder-threshold\""}}]'::jsonb, NULL, NULL, NULL, NULL),
  ('po-place-on-procurement-step-done', 1, 'active', 'step.done.procurement', NULL, '[{"handler":"inventory.po.place","args":{}}]'::jsonb, NULL, NULL, NULL, NULL),
  ('receive-on-receiving-step-done', 1, 'active', 'step.done.receiving', NULL, '[{"handler":"inventory.receive","args":{}}]'::jsonb, NULL, NULL, NULL, NULL),
  ('bill-approve-on-bill-approval-step-done', 1, 'active', 'step.done.bill-approval', NULL, '[{"handler":"inventory.bill.approve","args":{}}]'::jsonb, NULL, NULL, NULL, NULL),
  ('bill-payment-batch-on-bill-payment-batch-step-done', 1, 'active', 'step.done.bill-payment-batch', NULL, '[{"handler":"inventory.bill.payment_batch","args":{}}]'::jsonb, NULL, NULL, NULL, NULL),
  ('ledger-bill-approve-on-expense-bill-step-done', 1, 'active', 'step.done.expense-bill', NULL, '[{"handler":"ledger.bill.approve","args":{}}]'::jsonb, NULL, NULL, NULL, NULL),
  ('ledger-bill-payment-batch-on-expense-bill-payment-step-done', 1, 'active', 'step.done.expense-bill-payment', NULL, '[{"handler":"ledger.bill.payment_batch","args":{}}]'::jsonb, NULL, NULL, NULL, NULL),
  ('invoice-issue-on-billing-step-done', 1, 'active', 'step.done.billing', NULL, '[{"handler":"commerce.invoice.issue","args":{"due_days":"30","default_revenue_category":"\"uncategorized\"","tax_rate_bps":"725","tax_jurisdiction":"\"US-CA\"","taxable_categories":"\"retail,taproom,event-package\""}}]'::jsonb, NULL, NULL, NULL, NULL),
  ('tax-remit-on-tax-remittance-step-done', 1, 'active', 'step.done.tax-remittance', NULL, '[{"handler":"ledger.tax.remit","args":{"remit":"true","provider":"\"in-house\""}}]'::jsonb, NULL, NULL, NULL, NULL),
  ('excise-accrue-on-production-produce-step-done', 1, 'active', 'step.done.production-produce', NULL, '[{"handler":"ledger.tax.accrue","args":{"rate_cents_per_bbl":"350","liability_account":"\"2320\"","expense_account":"\"6550\"","jurisdiction":"\"US-FEDERAL\""}}]'::jsonb, NULL, NULL, NULL, NULL),
  ('payroll-submit-on-payroll-release-step-done', 1, 'active', 'step.done.payroll-release', NULL, '[{"handler":"ledger.payroll.run.submit","args":{"periods_per_year":"26","withholding_bps":"2200","employer_cost_bps":"1500","provider":"\"in-house\""}}]'::jsonb, NULL, NULL, NULL, NULL),
  ('parts-consume-on-production-consume-step-done', 1, 'active', 'step.done.production-consume', NULL, '[{"handler":"inventory.parts.consume","args":{"reason":"\"morning-brew\""}}]'::jsonb, NULL, NULL, NULL, NULL),
  ('products-produce-on-production-produce-step-done', 1, 'active', 'step.done.production-produce', NULL, '[{"handler":"products.produce","args":{"cost_basis":"\"drain-actual-wip\""}},{"handler":"inventory.parts.consume","args":{"reason":"\"morning-brew-packaging\""}}]'::jsonb, NULL, NULL, NULL, NULL),
  ('parts-consume-on-repair-step-done', 1, 'active', 'step.done.repair', NULL, '[{"handler":"inventory.parts.consume","args":{"reason":"\"equipment-preventive-maintenance-repair\""}}]'::jsonb, NULL, NULL, NULL, NULL),
  ('people-hire-on-hr-hire-step-done', 1, 'active', 'step.done.hr-hire', NULL, '[{"handler":"people.hire","args":{}}]'::jsonb, NULL, NULL, NULL, NULL),
  ('people-terminate-on-hr-terminate-step-done', 1, 'active', 'step.done.hr-terminate', NULL, '[{"handler":"people.terminate","args":{}}]'::jsonb, NULL, NULL, NULL, NULL),
  ('shipment-side-effects-on-shipment-step-done', 1, 'active', 'step.done.shipment', NULL, '[{"handler":"shipping.create","args":{}},{"handler":"inventory.parts.consume","args":{"reason":"\"sale\""}},{"handler":"products.consume","args":{}}]'::jsonb, NULL, NULL, NULL, NULL),
  ('spawn-subjob-on-delegate-subjob-step-ready', 1, 'active', 'step.ready.delegate-subjob', NULL, '[{"handler":"jobs.spawn","args":{"kind":"metadata.subjob_kind","subject_kind":"subject_kind","subject":"subject_id","parent_step_id":"step_id"}}]'::jsonb, NULL, NULL, NULL, NULL),
  ('complete-marker-on-step-ready', 1, 'active', 'step.ready.*', NULL, '[{"handler":"jobs.complete_step","args":{}}]'::jsonb, NULL, NULL, NULL, NULL),
  ('resolve-agent-gate-on-step-ready', 1, 'active', 'step.ready.*', NULL, '[{"handler":"gate.resolve","args":{}}]'::jsonb, NULL, NULL, NULL, NULL),
  ('notify-assignee-on-step-ready', 1, 'active', 'step.ready.*', NULL, '[{"handler":"messages.notify","args":{}}]'::jsonb, NULL, NULL, NULL, NULL),
  ('resolve-subjob-on-child-job-closed', 1, 'active', 'jobs.job.closed', 'parent_step_id != null', '[{"handler":"jobs.subjob_resolve","args":{}}]'::jsonb, NULL, NULL, NULL, NULL),
  ('forward-billing-done-to-webhook', 1, 'active', 'step.done.billing', NULL, '[{"handler":"webhook.notify","args":{}}]'::jsonb, NULL, NULL, NULL, NULL),
  ('forward-procurement-done-to-webhook', 1, 'active', 'step.done.procurement', NULL, '[{"handler":"webhook.notify","args":{}}]'::jsonb, NULL, NULL, NULL, NULL),
  ('forward-shipment-done-to-webhook', 1, 'active', 'step.done.shipment', NULL, '[{"handler":"webhook.notify","args":{}}]'::jsonb, NULL, NULL, NULL, NULL),
  ('forward-handoff-done-to-webhook', 1, 'active', 'step.done.handoff', NULL, '[{"handler":"webhook.notify","args":{}}]'::jsonb, NULL, NULL, NULL, NULL),
  ('forward-receiving-done-to-webhook', 1, 'active', 'step.done.receiving', NULL, '[{"handler":"webhook.notify","args":{}}]'::jsonb, NULL, NULL, NULL, NULL),
  ('forward-invoice-paid-to-webhook', 1, 'active', 'commerce.invoice.paid', NULL, '[{"handler":"webhook.notify","args":{}}]'::jsonb, NULL, NULL, NULL, NULL),
  ('forward-invoice-past-due-to-webhook', 1, 'active', 'commerce.invoice.past_due', NULL, '[{"handler":"webhook.notify","args":{}}]'::jsonb, NULL, NULL, NULL, NULL),
  ('forward-vendor-invoice-to-webhook', 1, 'active', 'inventory.vendor_invoice_received', NULL, '[{"handler":"webhook.notify","args":{}}]'::jsonb, NULL, NULL, NULL, NULL),
  ('forward-tax-filing-to-webhook', 1, 'active', 'ledger.tax_filing_filed', NULL, '[{"handler":"webhook.notify","args":{}}]'::jsonb, NULL, NULL, NULL, NULL);
