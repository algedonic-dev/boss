//! Default event-route registrations for `LiveApiOutput`.
//!
//! Registers the topic→endpoint map every tenant runner uses to turn
//! shape-driven engine `emit_event` calls into HTTP POST/PUTs against the
//! live services. `boss-brewery-engine` (and any other `LiveApiOutput`
//! consumer) calls `register_default_event_routes(&mut output)` when it
//! constructs a `LiveApiOutput`; new routes land here as event topics ship,
//! deletes when a topic retires.

use crate::output::live::{EventHttpMethod, LiveApiOutput};

/// Registers the canonical topic→endpoint route map on a fresh
/// `LiveApiOutput`, so live runs route their `emit_event` calls to the
/// real services. Add a row here when a new event topic ships.
pub fn register_default_event_routes(out: &mut LiveApiOutput) {
    // Daily bank-clearing sweep. The PeriodicEngine injects
    // `_day` automatically; the path template substitutes it
    // into the as_of query param.
    out.register_event_route(
        "ledger.bank_sweep_request",
        "/api/ledger/bank-settlements/sweep?as_of={_day}",
        EventHttpMethod::Post,
    );
    // Payroll-run synthesize. The brewery's `payroll-run`
    // JobKind opens via `[periodic.biweekly-payroll]`, walks
    // calculate → review → release. The terminal `payroll-release`
    // step's `ledger.payroll.run.submit` side-effect handler
    // emits this topic with the per-tenant rate config (periods,
    // withholding bps, employer cost bps) + the run/period
    // dates. The synthesize endpoint queries active employees,
    // computes per-employee gross/withheld/net + aggregate
    // employer_tax, persists the run + emits the canonical
    // `ledger.payroll.run` event + posts the `finance.payroll.run`
    // fact + journal entry.
    out.register_event_route(
        "ledger.payroll.run.submit",
        "/api/ledger/payroll-runs/synthesize",
        EventHttpMethod::Post,
    );
    // Vendor invoice approval. The bill-approval step's
    // `inventory.bill.approve` side-effect emits this; the
    // inventory service inserts the vendor_invoice + the matching
    // `finance.bill.approved` financial_fact in the same tx; the
    // ledger projects DR Inventory / CR A/P. Idempotent on `id`.
    out.register_event_route(
        "inventory.bill_approved",
        "/api/inventory/vendor-invoices",
        EventHttpMethod::Post,
    );
    // Daily AP payment run — every approved vendor invoice is
    // settled in one POST. The `ap-payment-run` JobKind's
    // `bill-payment-batch` step fires `inventory.bill.payment_batch`
    // with the run-date paid_on; inventory-api updates each
    // vendor_invoice + emits the canonical VENDOR_INVOICE_PAID
    // events. The ledger's `bill_paid` rule projects DR A/P /
    // CR Cash.
    out.register_event_route(
        "inventory.bill.payment_batch",
        "/api/inventory/vendor-invoices/batch-pay",
        EventHttpMethod::Post,
    );
    // Vendor "posts" its invoice for a PO — the automated-counterparty path.
    // The brewery's synthesized per-vendor supplier specs emit this
    // ~lead-time after `step.done.procurement` (the vendor's API responding);
    // the from-po endpoint resolves the PO + lands the invoice `received`,
    // which the human bill-approval step then approves. The po_id is pulled
    // from the procurement step's metadata inside the folded trigger.
    out.register_event_route(
        "inventory.vendor_invoice_received",
        "/api/inventory/vendor-invoices/from-po/{trigger.metadata.po_id}",
        EventHttpMethod::Post,
    );
    // PO placement from the procurement StepType's
    // `inventory.po.place` side effect. Inventory create-order
    // upserts on `id`. Required because the downstream
    // bill-approval step's `po_id` must reference a real
    // purchase_orders row (FK via vendor_invoices.po_id).
    out.register_event_route(
        "inventory.po_placed",
        "/api/inventory/orders/batch",
        EventHttpMethod::Post,
    );
    // Receive-step side effects: per-line on_hand increment + PO
    // status flip. Without these the inventory loop never closes.
    out.register_event_route(
        "inventory.item_received",
        "/api/inventory/items/{part_sku}/receive",
        EventHttpMethod::Post,
    );
    out.register_event_route(
        "inventory.po_received",
        "/api/inventory/orders/{id}/status",
        EventHttpMethod::Put,
    );
    // Finished-product produce + consume — Phase 3a of the
    // finished-product tracking initiative. Brewery's morning-brew
    // packaging step's `products.produce` side-effect emits one
    // event per `produces_products` row; wholesale + shop shipment
    // steps' `products.consume` side-effect emits one per
    // `consumes_products` row.
    out.register_event_route(
        "products.item_produced",
        "/api/products/{sku}/inventory/produce",
        EventHttpMethod::Post,
    );
    out.register_event_route(
        "products.item_consumed",
        "/api/products/{sku}/inventory/consume",
        EventHttpMethod::Post,
    );
    // Model B GL cost flow. Two endpoints, two semantically
    // distinct moves:
    //   - inventory.transferred: asset → asset (raw → WIP via
    //     parts.consume, WIP → FG via products.produce)
    //   - cogs.recognized: asset → expense (FG → 5100 COGS via
    //     products.consume at sale)
    out.register_event_route(
        "finance.inventory.transferred",
        "/api/ledger/inventory-transferred",
        EventHttpMethod::Post,
    );
    // Burden absorption — production overhead capitalized into WIP at
    // production-consume time. Routed to inventory-api (not ledger-api)
    // so the canonical inventory.overhead.absorbed audit_log event lands
    // alongside the ITEM_CONSUMED / INVENTORY_TRANSFERRED siblings.
    // The handler in inventory-api writes the financial_fact +
    // journal entry too — same dual-write shape as consume_part.
    out.register_event_route(
        "inventory.overhead.absorbed",
        "/api/inventory/overhead-absorbed",
        EventHttpMethod::Post,
    );
    out.register_event_route(
        "finance.cogs.recognized",
        "/api/ledger/cogs-recognized",
        EventHttpMethod::Post,
    );
    // HR funnel — boss-people-sim-bridge handlers translate
    // brewery-hire's onboarding step + brewery-terminate's
    // offboard step into people.* events.
    out.register_event_route(
        "people.employee.created",
        "/api/people",
        EventHttpMethod::Post,
    );
    out.register_event_route(
        "people.employee.terminated",
        "/api/people/{id}/status",
        EventHttpMethod::Put,
    );
    // A/R aging — brewery's `[counterparty.ar-aging]` listens on
    // `step.done.billing` and re-emits paid (85%) / past_due (15%)
    // ~30 business days later. The trigger payload carries
    // `step_id`; the commerce-sim-bridge mints invoice ids as
    // `inv-step-{step_id}` so the path template resolves to the
    // same row deterministically.
    out.register_event_route(
        "commerce.invoice.paid",
        "/api/commerce/invoices/inv-step-{trigger.step_id}/paid",
        EventHttpMethod::Put,
    );
    out.register_event_route(
        "commerce.invoice.past_due",
        "/api/commerce/invoices/inv-step-{trigger.step_id}/past-due",
        EventHttpMethod::Put,
    );
    // Write-off — brewery's `[counterparty.bad-debt-writeoff]` listens
    // on `commerce.invoice.past_due` and re-fires this topic ~60
    // business days later. Post-#100 the counterparty receives TWO
    // past-due copies per invoice — ar-aging's sim-internal emission
    // (step_id buried at `trigger.trigger.step_id`) and the system's
    // webhook copy (the enriched invoice row, no trigger lineage) —
    // so a fixed path template can't resolve both drives. The
    // adapter endpoint resolves either shape (chain step_id →
    // `inv-step-{id}`; webhook copy → its own `inv-*` id) and
    // converges the double delivery on the single terminal flip.
    // Same class as from-paid-invoice (#102).
    out.register_event_route(
        "commerce.invoice.written_off",
        "/api/commerce/invoices/write-off/from-past-due",
        EventHttpMethod::Post,
    );
    // Tax filings fire as JobKinds (sales-tax-filing,
    // payroll-941-filing, income-tax-filing, excise-tax-filing) whose
    // terminal `tax-remittance` step's `ledger.tax.remit` side-effect
    // handler emits the canonical TaxFilingSnapshot, which the
    // ledger-sim-bridge POSTs to /api/ledger/tax-filings.
    // Customer-payment settlement. Brewery's `[counterparty.bank-ach]`
    // listens on `commerce.invoice.paid` and re-emits this topic
    // ~1 business day later (98.5% probability). The endpoint
    // unfolds the chained trigger payload, looks up the invoice's
    // amount/account/currency from the projection, and reuses
    // the bank_settlements + financial_facts + posting flow.
    out.register_event_route(
        "ledger.payment_settled",
        "/api/ledger/bank-settlements/from-paid-invoice",
        EventHttpMethod::Post,
    );
    // Shipment creation — fires from the shipment StepType's
    // `shipping.create` side effect (boss-shipping-sim-bridge).
    out.register_event_route(
        "shipping.shipment.created",
        "/api/shipping/shipments",
        EventHttpMethod::Post,
    );
    // Carrier-scan updates. Brewery's keg-courier CounterpartySpec
    // emits these three on a chained delay after `step.done.shipment`.
    // Each routes to a single adapter endpoint that walks the
    // trigger chain to find step_id, derives shipment_id, records a
    // shipment_tracking_events row, and rolls up the shipment
    // status when applicable. Idempotent on
    // (shipment_id, status, occurred_on).
    out.register_event_route(
        "delivery.tracking_in_transit",
        "/api/shipping/shipments/from-tracking-scan",
        EventHttpMethod::Post,
    );
    out.register_event_route(
        "delivery.tracking_out_for_delivery",
        "/api/shipping/shipments/from-tracking-scan",
        EventHttpMethod::Post,
    );
    out.register_event_route(
        "delivery.tracking_delivered",
        "/api/shipping/shipments/from-tracking-scan",
        EventHttpMethod::Post,
    );
    // Operational message broadcast. Brewery counterparties
    // (brewhouse-broadcast, dispatch-broadcast, billing-broadcast,
    // receiving-broadcast) emit this topic with a static
    // {sender_id, recipient_id, subject, body, kind} payload + the
    // trigger folded under `trigger`.
    out.register_event_route(
        "messages.broadcast",
        "/api/messages/send",
        EventHttpMethod::Post,
    );
    // Subject-birth routes. Shape-driven `birth_subjects` Poisson-
    // samples new Subjects per `[subject_rates.<kind>]` and emits
    // the configured `created_event_kind` topic with a synthesized
    // payload sized to satisfy the destination schema. Without
    // these routes the births still grow the in-memory pool but
    // never enter audit_log → soft-FK integrity scans flag every
    // downstream reference.
    out.register_event_route(
        "accounts.account.created",
        "/api/people/accounts",
        EventHttpMethod::Post,
    );
    out.register_event_route(
        "inventory.vendor.created",
        "/api/inventory/vendors",
        EventHttpMethod::Post,
    );
    // Campaign births have no domain table — their identity IS the
    // row (subject-model R1; identity-first). Route the birth to the
    // kind-scoped mint so tap-launch Jobs pass the uniform
    // subject-existence gate.
    out.register_event_route(
        "commerce.campaign.created",
        "/api/subjects/campaign",
        EventHttpMethod::Post,
    );
}
