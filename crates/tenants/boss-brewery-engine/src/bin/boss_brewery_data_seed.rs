//! `boss-brewery-data-seed` — CLI shell over
//! [`boss_brewery_engine::prepare::seed_tenant_data`]. POSTs the
//! canonical brewery accounts + vendors + employees + messages +
//! bulletins + finished-goods + raw materials + equipment catalog +
//! serialized assets to the live API, idempotently. Each successful
//! POST lands as an event in audit_log so the projection rebuilders
//! reconstruct the rows alongside the jobs/commerce/inventory events.
//!
//! The seeding logic lives in the library (`prepare::tenant_data`) so
//! the offline path and the live demo drive identical code; this
//! binary just resolves the per-service / gateway base URLs and hands
//! off. See that module for the seed order + idempotence contract.
//!
//! Retained after the brewery-driver convergence (the sibling
//! `boss-brewery-bootstrap` + `boss-brewery-engine` bins folded into
//! `boss-brewery-sim`'s `prepare` / `run` subcommands and were
//! deleted) for the ONE topology `prepare` doesn't serve: the
//! Playwright scratch stack (`apps/web/tests/globalSetup.ts`), which
//! needs per-service scratch ports with some solo services pointed at
//! a dead port to skip them. This bin's granular `--<service>-base`
//! flags are exactly that knob. It's a thin shell over the same lib
//! fn `prepare` calls, so there's no logic to drift.
//!
//! Usage:
//!   boss-brewery-data-seed                    # per-service ports
//!                                             # (boss_ports defaults)
//!   boss-brewery-data-seed --gateway-base ... # route everything
//!                                             # through the gateway
//!   boss-brewery-data-seed --people-base ...  # override an
//!                                             # individual service
//!                                             # (the Playwright
//!                                             # scratch stack does
//!                                             # this, pointing solo
//!                                             # services at a dead
//!                                             # port to skip them)

use std::path::Path;

use anyhow::Result;
use boss_brewery_engine::prepare::{SeedBases, seed_tenant_data};
use clap::Parser;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "boss-brewery-data-seed",
    about = "Seed brewery accounts + vendors into the live API",
    version
)]
struct Cli {
    /// Per-service base URLs. Accounts → people-api, vendors →
    /// inventory-api, messages, content/bulletins, calendar.
    /// Defaults pulled from `boss_ports::url(...)` so the
    /// 7060/7250-class collision can't slip back in. Pass
    /// `--gateway-base` to override all of them with a single
    /// gateway URL.
    #[arg(long, default_value_t = boss_ports::url("people"))]
    people_base: String,

    /// Accounts API base — account create / get / exists / update
    /// lives on its own port (7550), separate from the people-api.
    /// Accounts must POST here, not to /api/people: a misrouted
    /// account drops silently and downstream commerce.invoice.create
    /// then FK-fails (also silently — the batch endpoint swallows
    /// skips), surfacing only as a 404 on the counterparty's later
    /// invoice/paid PUT.
    #[arg(long, default_value_t = boss_ports::url("accounts"))]
    accounts_base: String,

    #[arg(long, default_value_t = boss_ports::url("inventory"))]
    inventory_base: String,

    #[arg(long, default_value_t = boss_ports::url("messages"))]
    messages_base: String,

    #[arg(long, default_value_t = boss_ports::url("content"))]
    content_base: String,

    #[arg(long, default_value_t = boss_ports::url("calendar"))]
    calendar_base: String,

    #[arg(long, default_value_t = boss_ports::url("products"))]
    products_base: String,

    #[arg(long, default_value_t = boss_ports::url("catalog"))]
    catalog_base: String,

    #[arg(long, default_value_t = boss_ports::url("assets"))]
    assets_base: String,

    /// Ledger API base — used by the finished-product pre-seed to
    /// post opening-balance journal entries (DR 1320 FG / CR 3000
    /// Retained Earnings) matching each PUT'd starter row, so the
    /// GL agrees with the inventory snapshot on day 1.
    #[arg(long, default_value_t = boss_ports::url("ledger"))]
    ledger_base: String,

    /// If set, all per-service base URLs are ignored; the seeder
    /// routes through the gateway URL with the canonical
    /// /api/people/* / /api/inventory/* / /api/messages/* /
    /// /api/content/* / /api/calendar/* / /api/catalog/* /
    /// /api/assets/* prefixes.
    #[arg(long)]
    gateway_base: Option<String>,

    /// Operator-tier x-boss-user header. Defaults to a hardcoded
    /// system-seed identity since the seeder runs out-of-band.
    #[arg(long)]
    x_boss_user: Option<String>,

    /// Seed bundle root (the `examples/brewery/seeds/` dir).
    /// Used by the raw-materials opening-balance step to load
    /// `parts.toml` so the JE amounts track the toml without
    /// the seeder having to duplicate the catalog. Defaults to
    /// the same convention as boss-brewery-engine.
    #[arg(long, default_value = "examples/brewery/seeds")]
    seeds_dir: String,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .compact()
        .init();
    let cli = Cli::parse();

    // --gateway-base routes every service through one URL; otherwise
    // each service keeps its own (boss_ports-default or overridden)
    // base. The scratch test stack relies on the per-service form,
    // pointing solo services at a dead port to skip them.
    let bases = match cli.gateway_base.clone() {
        Some(g) => SeedBases::all(&g),
        None => SeedBases {
            people: cli.people_base,
            accounts: cli.accounts_base,
            inventory: cli.inventory_base,
            messages: cli.messages_base,
            content: cli.content_base,
            calendar: cli.calendar_base,
            products: cli.products_base,
            catalog: cli.catalog_base,
            assets: cli.assets_base,
            ledger: cli.ledger_base,
        },
    };

    seed_tenant_data(
        &bases,
        Path::new(&cli.seeds_dir),
        cli.x_boss_user.as_deref(),
    )
}
