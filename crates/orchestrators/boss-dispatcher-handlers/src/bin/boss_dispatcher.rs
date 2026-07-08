//! boss-dispatcher — core service that auto-assigns ready Steps to
//! role-matched Employees. See lib.rs for the architectural rationale.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use boss_calendar_client::ReqwestCalendarClient;
use boss_dispatcher::config::DispatcherConfig;
use boss_dispatcher::dispatcher::{DispatcherCtx, run_loop};
use boss_dispatcher::http::{HttpState, router};
use boss_dispatcher::liveness::DispatcherLiveness;
use boss_dispatcher::rules::handler::HandlerRegistry;
use boss_dispatcher::rules::helpers_inventory::InventoryHelpers;
use boss_dispatcher::rules::jobs_spawn::JobsSpawn;
use boss_dispatcher::rules::registry::{Registry as RuleRegistry, load_active_rules};
use boss_dispatcher::rules::runner::RulesRunner;
use boss_dispatcher::rules::schedule_runner::{DEFAULT_CATCHUP_CAP, ScheduleRunner};
use boss_dispatcher_handlers::handlers::{
    bill_payment_batch::BillPaymentBatch, commerce_invoice_issue::CommerceInvoiceIssue,
    gate_resolve::GateResolve, inventory_bill_approve::InventoryBillApprove,
    inventory_overhead_absorb::InventoryOverheadAbsorb,
    inventory_parts_consume::InventoryPartsConsume, inventory_parts_produce::InventoryPartsProduce,
    inventory_po_place::InventoryPoPlace, inventory_receive::InventoryReceive,
    jobs_complete_step::JobsCompleteStep, jobs_subjob_resolve::JobsSubjobResolve,
    ledger_bill_approve::LedgerBillApprove, ledger_payroll_run_submit::LedgerPayrollRunSubmit,
    ledger_tax_accrue::LedgerTaxAccrue, ledger_tax_remit::LedgerTaxRemit,
    messages_notify::MessagesNotify, packaging_allocate::PackagingAllocate,
    people_hire::PeopleHire, people_terminate::PeopleTerminate, products_consume::ProductsConsume,
    products_consume_from_invoice::ProductsConsumeFromInvoice, products_produce::ProductsProduce,
    shipping_create::ShippingCreate, webhook_notify::WebhookNotify,
};
use tokio::net::TcpListener;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .compact()
        .init();

    let cfg = DispatcherConfig::default();
    info!(
        nats_url = %cfg.nats_url,
        jobs_api_url = %cfg.jobs_api_url,
        people_api_url = %cfg.people_api_url,
        inventory_api_url = %cfg.inventory_api_url,
        assignment_strategy = ?cfg.assignment_strategy,
        "boss-dispatcher starting"
    );

    let nats_client = async_nats::connect(&cfg.nats_url)
        .await
        .with_context(|| format!("connecting to NATS at {}", cfg.nats_url))?;
    let jetstream = async_nats::jetstream::new(nats_client);

    // `--reset-stream`: dev/regen one-shot. Drop the durable buffer (and its
    // consumers) and recreate it empty, then exit — so a fresh regen doesn't
    // replay the previous run's events against the just-reset database.
    if std::env::args().any(|a| a == "--reset-stream") {
        boss_nats::durable::reset_stream(&jetstream)
            .await
            .context("resetting BOSS_EVENTS stream")?;
        info!("BOSS_EVENTS stream reset; exiting");
        return Ok(());
    }
    // Durable dispatch requires the stream; fatal if JetStream is
    // unavailable — the dispatcher cannot guarantee delivery without it.
    boss_nats::durable::ensure_stream(&jetstream)
        .await
        .context("ensuring BOSS_EVENTS stream (JetStream required for durable dispatch)")?;

    let ctx = Arc::new(DispatcherCtx::new(
        cfg.jobs_api_url.clone(),
        cfg.people_api_url.clone(),
        cfg.assignment_strategy,
    ));
    // Shared consumer liveness — both loops mark it; /api/dispatcher/readyz
    // reads it. Lets readiness probes see the consumers actually bound, not
    // just the process answering /health.
    let live = Arc::new(DispatcherLiveness::default());
    let js_for_loop = jetstream.clone();
    let ctx_for_loop = ctx.clone();
    let live_for_loop = live.clone();
    tokio::spawn(async move {
        if let Err(e) = run_loop(ctx_for_loop, js_for_loop, live_for_loop).await {
            tracing::error!(error = %e, "dispatcher loop exited with error");
        }
    });

    // Postgres pool — the dispatcher loads its rule registry from the
    // append-only versioned `dispatcher_rules` table (replacing the legacy
    // rules.toml file) and serves it at /api/dispatcher/rules.
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(&cfg.postgres_url)
        .await
        .with_context(|| "connecting to Postgres for the dispatcher rule registry")?;

    // Load the rule registry from `dispatcher_rules` and start the rules
    // runner alongside the legacy role-assignment loop. They share the NATS
    // connection but subscribe to disjoint topics — the legacy loop owns
    // jobs.step.>, the runner owns whatever the registry declares.
    match load_active_rules(&pool)
        .await
        .and_then(RuleRegistry::from_raw)
    {
        Ok(registry) => {
            info!(
                rule_count = registry.rules().len(),
                "rules registry loaded from dispatcher_rules"
            );
            let mut handlers = HandlerRegistry::new();
            handlers.register(JobsSpawn::new(cfg.jobs_api_url.clone()));
            // D7 delegate-subjob write-back: on a child Job's
            // close, resolve the parent delegate-subjob step.
            handlers.register(JobsSubjobResolve::new(cfg.jobs_api_url.clone()));
            // System-completes zero-duration, no-role markers
            // (trigger / outcome / milestone) the moment they go
            // Ready, so a Job flows past its structural checkpoints
            // without an executor. Shares the dispatcher's StepType
            // registry to classify markers (vs. real no-role work
            // like `task`).
            handlers.register(JobsCompleteStep::new(
                cfg.jobs_api_url.clone(),
                ctx.registry.clone(),
            ));
            // Agent gate executor: on step.ready for a gate kind
            // (demand-gate / availability-gate), read real
            // finished-goods stock, decide the outcome, and complete
            // the gate with it — computer-speed, no workforce slot.
            // Shares the StepType registry to classify gates.
            handlers.register(GateResolve::new(
                cfg.jobs_api_url.clone(),
                cfg.products_api_url.clone(),
                ctx.registry.clone(),
            ));
            // Packaging allocation — splits a brewed batch across formats by
            // demand and writes the packaged quantities, so the whole batch
            // always packages (WIP → FG, never dumped).
            handlers.register(PackagingAllocate::new(
                cfg.jobs_api_url.clone(),
                cfg.products_api_url.clone(),
            ));
            // Step-completion handlers — F15 migration. Each
            // is a pure HTTP client to the relevant public API.
            handlers.register(InventoryPoPlace::new(cfg.inventory_api_url.clone()));
            handlers.register(InventoryReceive::new(
                cfg.inventory_api_url.clone(),
                cfg.ledger_api_url.clone(),
            ));
            handlers.register(InventoryBillApprove::new(cfg.inventory_api_url.clone()));
            handlers.register(BillPaymentBatch::new(
                "inventory.bill.payment_batch",
                cfg.inventory_api_url.clone(),
                "/api/inventory/vendor-invoices/batch-pay",
            ));
            handlers.register(InventoryPartsConsume::new(cfg.inventory_api_url.clone()));
            // Production-overhead absorption (DR 1310 / CR <driver
            // expense>) sized rate_cents_per_bbl × batch bbl at runtime —
            // the rate rides the rule args, the batch size the job's own
            // data, so the seed stamps no amounts. Needs the jobs API for
            // the batch-bbl read.
            handlers.register(InventoryOverheadAbsorb::new(
                cfg.jobs_api_url.clone(),
                cfg.inventory_api_url.clone(),
            ));
            handlers.register(InventoryPartsProduce::new(cfg.inventory_api_url.clone()));
            // FG cost basis is derived from the brew's real consumed-input
            // cost, not a plug. The drain-actual-wip basis drains exactly
            // what consume capitalized (the ledger's DR-1310 facts), so the
            // handler needs the jobs + inventory + ledger APIs.
            handlers.register(ProductsProduce::new(
                cfg.products_api_url.clone(),
                cfg.jobs_api_url.clone(),
                cfg.inventory_api_url.clone(),
                cfg.ledger_api_url.clone(),
            ));
            handlers.register(ProductsConsume::new(cfg.products_api_url.clone()));
            // Q2 (inventory-value-conservation): the consume owns COGS.
            // Every issued invoice's FG lines drain stock + recognize
            // COGS through the products surface, replacing commerce's
            // in-tx cross-module UPDATE + the invoice JE's COGS leg.
            handlers.register(ProductsConsumeFromInvoice::new(
                cfg.products_api_url.clone(),
            ));
            handlers.register(CommerceInvoiceIssue::new(cfg.commerce_api_url.clone()));
            handlers.register(ShippingCreate::new(cfg.shipping_api_url.clone()));
            // Outbound integration edge: forward matched events to a
            // configured external webhook (e.g. a regen's simulator
            // playing external counterparties). No-op when
            // BOSS_EVENT_WEBHOOK_URL is unset; the system stays
            // unaware of who, if anyone, is on the other end.
            handlers.register(WebhookNotify::new(cfg.webhook_url.clone()));
            handlers.register(LedgerTaxRemit::new(cfg.ledger_api_url.clone()));
            // Per-production excise-tax accrual (DR 6550 / CR 2320),
            // fired on `step.done.production-produce` — the brewery's
            // federal beer excise liability accrues at packaging time,
            // drained quarterly by the excise-tax-filing JobKind.
            handlers.register(LedgerTaxAccrue::new(cfg.ledger_api_url.clone()));
            handlers.register(LedgerPayrollRunSubmit::new(cfg.ledger_api_url.clone()));
            // General AP bills (rent/utilities/…) → ledger subledger.
            handlers.register(LedgerBillApprove::new(cfg.ledger_api_url.clone()));
            handlers.register(BillPaymentBatch::new(
                "ledger.bill.payment_batch",
                cfg.ledger_api_url.clone(),
                "/api/ledger/bills/pay-run",
            ));
            handlers.register(PeopleHire::new(cfg.people_api_url.clone()));
            handlers.register(PeopleTerminate::new(cfg.people_api_url.clone()));
            // Push notifier: step.ready.* -> message the role's
            // on-call member (the pull-side assignments query is
            // the actual work driver; this is awareness).
            handlers.register(MessagesNotify::new(
                cfg.people_api_url.clone(),
                cfg.messages_api_url.clone(),
            ));
            let helpers = Arc::new(InventoryHelpers::new(
                cfg.inventory_api_url.clone(),
                cfg.jobs_api_url.clone(),
            ));
            // The schedule runner shares the SAME handlers (jobs.spawn et
            // al.) + the SAME parsed registry as the event runner — only
            // the trigger differs (clock day vs NATS event). Both are
            // Clone (handlers are Arc'd; the registry is parsed Exprs), so
            // clone before moving the originals into the event runner.
            let sched_registry = registry.clone();
            let sched_handlers = handlers.clone();

            let runner = Arc::new(RulesRunner {
                registry,
                handlers,
                helpers,
            });
            let js_for_runner = jetstream.clone();
            let live_for_runner = live.clone();
            tokio::spawn(async move {
                if let Err(e) = runner.run(js_for_runner, live_for_runner).await {
                    tracing::error!(error = %e, "rules runner exited with error");
                }
            });

            // Clock-driven schedule runner: fires schedule-triggered rules
            // on sim-day boundaries off the clock SSE feed. No-op (returns
            // immediately) when the registry has no schedule rules.
            let schedule_runner = Arc::new(ScheduleRunner {
                registry: sched_registry,
                handlers: sched_handlers,
                clock_url: cfg.clock_api_url.clone(),
                pool: pool.clone(),
                calendar: Arc::new(ReqwestCalendarClient::new(cfg.calendar_api_url.clone())),
                catchup_cap: DEFAULT_CATCHUP_CAP,
            });
            let live_for_schedule = live.clone();
            tokio::spawn(async move {
                if let Err(e) = schedule_runner.run(live_for_schedule).await {
                    tracing::error!(error = %e, "schedule runner exited with error");
                }
            });
        }
        Err(e) => {
            warn!(error = %e, "failed to load dispatcher_rules registry; runner not started");
        }
    }

    let app = router(HttpState { live, pool });
    let bind: SocketAddr = cfg
        .http_bind
        .parse()
        .with_context(|| format!("invalid http_bind `{}`", cfg.http_bind))?;
    let listener = TcpListener::bind(bind)
        .await
        .with_context(|| format!("binding HTTP listener on {bind}"))?;
    info!(addr = %bind, "boss-dispatcher HTTP listening (health-only surface)");
    axum::serve(listener, app).await?;
    Ok(())
}
