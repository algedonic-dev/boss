-- 108-event-kinds.sql — the last vocabulary joins the registries
-- (af1586e1; Q1-Q4 resolved 2026-08-10: table / flat fields now /
-- warn + drift guard / seed harvested from the live log).
--
-- Static kinds are rows; dynamic families are patterns whose suffix
-- domain is the registry that already owns it (step.* -> StepTypes).
-- The drift guard lives in boss-audit-integrity-check: an emitted
-- kind no pattern matches WARNS loudly (Q3: the log stays available
-- under drift; the nightly check and CI make it visible).
CREATE TABLE IF NOT EXISTS event_kinds (
    kind_pattern TEXT PRIMARY KEY,
    source       TEXT NOT NULL,
    description  TEXT NOT NULL DEFAULT '',
    -- The registry owning a family's suffix values (e.g. step_types).
    suffix_domain TEXT,
    -- Q2: flat field inventory [{name, type, note}]; starts empty,
    -- filled as consumers (encryption classification, rule authoring)
    -- need it.
    payload_fields JSONB NOT NULL DEFAULT '[]',
    created_at   TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  ('accounts.account.created', 'accounts', 'An account Subject entered the company', NULL),
  ('accounts.account.team-assigned', 'accounts', 'Account-team membership changed', NULL),
  ('asset.received', 'assets', 'A physical asset arrived and joined the fleet', NULL),
  ('calendar.reservation.reserved', 'calendar', 'A calendar slot was reserved', NULL),
  ('campaigns.campaign.created', 'boss-campaigns', 'A marketing campaign opened', NULL),
  ('commerce.invoice.created', 'commerce', 'An invoice was issued (line items + rollups)', NULL),
  ('commerce.invoice.paid', 'commerce', 'An invoice settled', NULL),
  ('commerce.invoice.past_due', 'commerce', 'An invoice crossed its due date', NULL),
  ('commerce.invoice.written_off', 'commerce', 'An invoice was written off', NULL),
  ('content.bulletin.created', 'content', 'A company bulletin published', NULL),
  ('customers.customer.created', 'boss-customers', 'A customer record entered the company', NULL),
  ('docs.design.decision_recorded', 'docs', 'A design-review decision was recorded (dogfooding S1)', NULL),
  ('docs.design.indexed', 'docs', 'A design doc''s review surface changed (title/status/question counts)', NULL),
  ('inventory.item.consumed', 'inventory', 'Raw stock drained into production', NULL),
  ('inventory.item.received', 'inventory', 'Raw stock received against a PO', NULL),
  ('inventory.item.upserted', 'inventory', 'An inventory item''s row state (full state, rebuild source)', NULL),
  ('inventory.overhead.absorbed', 'inventory', 'Production overhead absorbed into WIP', NULL),
  ('inventory.po.status_changed', 'inventory', 'A purchase order moved status', NULL),
  ('inventory.purchase_order.upserted', 'inventory', 'A purchase order''s row state', NULL),
  ('inventory.transferred', 'inventory', 'Stock moved between locations', NULL),
  ('inventory.vendor.created', 'inventory', 'A vendor Subject entered the company', NULL),
  ('inventory.vendor_invoice.approved', 'inventory', 'A vendor bill approved for payment', NULL),
  ('inventory.vendor_invoice.paid', 'inventory', 'A vendor bill paid', NULL),
  ('inventory.vendor_invoice.upserted', 'inventory', 'A vendor bill''s row state', NULL),
  ('jobs.job.closed', 'jobs', 'A Job reached a terminal outcome', NULL),
  ('jobs.job.created', 'jobs', 'A Job opened (full row state)', NULL),
  ('jobs.job.status_changed', 'jobs', 'A Job''s status transitioned', NULL),
  ('jobs.job.updated', 'jobs', 'A Job''s row state after an update', NULL),
  ('jobs.kind.published', 'jobs', 'A Workflow version published to the registry', NULL),
  ('jobs.step.completed', 'jobs', 'A Step completed (full row state)', NULL),
  ('jobs.step.created', 'jobs', 'A Step materialized (full row state)', NULL),
  ('jobs.step.signed_off', 'jobs', 'A required sign-off landed on a Step', NULL),
  ('jobs.step.updated', 'jobs', 'A Step''s row state after an update', NULL),
  ('kb.model.created', 'kb', 'An equipment-KB model entered the catalog', NULL),
  ('ledger.bill.approved', 'ledger', 'A general AP bill approved (DR expense / CR AP)', NULL),
  ('ledger.bill.paid', 'ledger', 'A general AP bill paid', NULL),
  ('ledger.inventory.capitalized', 'ledger', 'Consumed inputs capitalized into WIP (DR 1310)', NULL),
  ('ledger.inventory.transferred', 'inventory,ledger,products', 'Inventory value moved between GL accounts (emitted by inventory, ledger AND products - a cross-source kind the registry exists to expose)', NULL),
  ('ledger.payment.received', 'ledger', 'Customer payment received', NULL),
  ('ledger.payment.settled', 'ledger', 'Bank settlement matched a payment', NULL),
  ('ledger.payroll.run', 'ledger', 'A payroll run posted', NULL),
  ('ledger.tax.accrual.recorded', 'ledger', 'A tax accrual recorded', NULL),
  ('ledger.tax.accrued', 'ledger', 'Tax liability accrued (e.g. excise at packaging)', NULL),
  ('ledger.tax.filing.created', 'ledger', 'A tax filing opened', NULL),
  ('ledger.tax.remitted', 'ledger', 'A tax remittance paid a filing', NULL),
  ('messages.message.read', 'messages', 'An inbox message was read', NULL),
  ('messages.message.sent', 'messages', 'An inbox message was sent', NULL),
  ('people.employee.change-recorded', 'people', 'An employee change (hire/transfer/comp) recorded', NULL),
  ('people.employee.created', 'people', 'An employee Subject entered the company', NULL),
  ('people.employee.updated', 'people', 'An employee''s row state after an update', NULL),
  ('products.consumed', 'products', 'Finished goods drained (the consume that owns COGS)', NULL),
  ('products.inventory.upserted', 'products', 'FG inventory row state (value-primary)', NULL),
  ('products.produced', 'products', 'Finished goods produced from WIP', NULL),
  ('products.product.upserted', 'products', 'A product''s catalog row state', NULL),
  ('shipping.shipment.created', 'shipping', 'A shipment opened', NULL),
  ('shipping.tracking.recorded', 'shipping', 'Carrier tracking recorded on a shipment', NULL),
  ('step.ready.*', 'jobs', 'A Step became eligible - the dispatcher''s assignment + side-effect trigger', 'step_types'),
  ('step.done.*', 'jobs', 'A Step completed - the side-effect trigger the rule registry consumes', 'step_types'),
  ('step.assigned.*', 'jobs', 'A Step was assigned to an executor', 'step_types')
ON CONFLICT (kind_pattern) DO NOTHING;
