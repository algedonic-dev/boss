use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

mod deploy;
mod docs;
mod docs_flush;
mod doctor;
mod inspect;
mod ops;
mod script;
mod upgrade;

#[derive(Parser)]
#[command(name = "boss", about = "Boss operator + developer CLI", version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Post-install health check — verifies Postgres, NATS, gateway,
    /// tenant manifest, SPA bundle, and registered systemd services.
    Doctor,
    /// Emit an event to the local bus
    Emit {
        /// Event kind (e.g., "test.hello")
        kind: String,
        /// JSON payload
        #[arg(default_value = "{}")]
        payload: String,
    },
    /// Upgrade boss to the latest release
    Upgrade,
    /// CTO toolbox — list and inspect registered scripts
    Script {
        #[command(subcommand)]
        action: ScriptAction,
    },
    /// Build, install, and restart services
    Deploy {
        #[command(subcommand)]
        action: DeployAction,
    },
    /// Check health of all services, Postgres, NATS, and backups
    Status {
        /// Output as JSON (for Claude Code / machine parsing)
        #[arg(long)]
        json: bool,
    },
    /// Restart a service without rebuilding
    Restart {
        /// Service name (assets, catalog, people, commerce, etc.)
        service: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// View service logs via journalctl
    Logs {
        /// Service name
        service: String,
        /// Number of log lines to show
        #[arg(short = 'n', long, default_value = "50")]
        lines: u32,
        /// Follow log output (like tail -f)
        #[arg(short, long)]
        follow: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Trigger a manual backup (pg_dump + configs)
    Backup {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Asset maintenance subcommands
    Assets {
        #[command(subcommand)]
        action: AssetsAction,
    },
    /// Design decision tracker — reindex docs and flush pending jobs
    Docs {
        #[command(subcommand)]
        action: DocsAction,
    },
    /// Run the Boss simulator (thin wrapper around `boss-sim`)
    Sim {
        #[command(subcommand)]
        action: SimAction,
    },
    /// Ledger operations — rebuild the GL projection from financial_facts
    Ledger {
        #[command(subcommand)]
        action: LedgerAction,
    },
    /// Read-only diagnostic queries against the gateway's HTTP
    /// APIs. Replaces the `sudo -u postgres psql` muscle memory
    /// for diagnostic reads — raw SQL hides API gaps and the same
    /// muscle memory leads to raw SQL writes that bypass the
    /// audit_log + policy gate.
    Inspect {
        #[command(subcommand)]
        action: InspectAction,
    },
    /// Query the audit log for domain events
    Audit {
        /// Filter by event kind prefix (e.g., "catalog.model")
        #[arg(long)]
        kind: Option<String>,
        /// Filter by source service (e.g., "catalog")
        #[arg(long)]
        source: Option<String>,
        /// Maximum entries to show
        #[arg(short = 'n', long, default_value = "20")]
        limit: u32,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum DeployAction {
    /// List all deployable services
    List,
    /// Deploy a service (or all if no service specified)
    Run {
        /// Service name (e.g. assets, shipping, gateway). Omit for all.
        service: Option<String>,
        /// Skip cargo build (install existing binary only)
        #[arg(long)]
        skip_build: bool,
    },
    /// Build and deploy the web frontend
    Web,
    /// Remove debug build artifacts to free disk space
    Clean,
}

#[derive(Subcommand)]
enum AssetsAction {
    /// Rebuild the `systems` projection table from the `system_events` log.
    /// Idempotent — safe to run on a healthy DB.
    RebuildProjection {
        /// Postgres URL. Defaults to the local assets service DB.
        #[arg(long, default_value = "postgres://boss:boss@127.0.0.1/boss")]
        postgres_url: String,
    },
}

#[derive(Subcommand)]
enum DocsAction {
    /// Re-scan docs/design/*.md and refresh the boss-docs cache
    Reindex,
    /// Pick up every queued flush job, apply decisions to the
    /// markdown file, commit, push, and mark the job succeeded.
    /// Use this when a human says "flush pending design jobs."
    FlushPending,
}

#[derive(Subcommand)]
enum SimAction {
    /// Replay a simulation config against the live service APIs.
    /// Shells out to the installed `boss-sim` binary.
    Replay {
        /// Path to TOML config file (required)
        #[arg(short, long)]
        config: std::path::PathBuf,

        /// Override catalog JSON path from config
        #[arg(long)]
        catalog: Option<std::path::PathBuf>,

        /// API base URL (gateway or direct service)
        #[arg(long, default_value = "http://127.0.0.1:4443")]
        api_url: String,

        /// Live mode: each simulated day posts through real write APIs
        #[arg(long, default_value_t = false)]
        live: bool,
    },
}

#[derive(Subcommand)]
enum LedgerAction {
    /// Rebuild journal entries for every open period. Locked periods are
    /// never touched — their pinned rule version keeps them stable.
    /// Idempotent: running it twice produces the same projection.
    Rebuild {
        /// Postgres URL. Defaults to the local Boss DB.
        #[arg(long, default_value = "postgres://boss:boss@127.0.0.1/boss")]
        postgres_url: String,
        /// Output as JSON (for machine parsing)
        #[arg(long)]
        json: bool,
    },
    /// List all periods with their status + totals.
    Periods {
        #[arg(long, default_value = "postgres://boss:boss@127.0.0.1/boss")]
        postgres_url: String,
        #[arg(long)]
        json: bool,
    },
    /// Lock a period by starting date (YYYY-MM-DD). Pins the active rule
    /// version and writes a checksum. Rejects further writes to that
    /// period until unlocked.
    Lock {
        /// Period starts_on date (e.g. 2026-03-01)
        starts_on: String,
        #[arg(long, default_value = "postgres://boss:boss@127.0.0.1/boss")]
        postgres_url: String,
        /// Identifier recorded as who locked the period.
        #[arg(long, default_value = "operator")]
        locked_by: String,
    },
    /// Unlock a period by starting date. Clears lock fields and returns
    /// status to 'open'. Operator-tier action.
    Unlock {
        starts_on: String,
        #[arg(long, default_value = "postgres://boss:boss@127.0.0.1/boss")]
        postgres_url: String,
    },
}

#[derive(Subcommand)]
enum InspectAction {
    /// List invoices, optionally filtered by status / account_id.
    Invoices {
        /// Filter by status (outstanding | paid | past-due).
        #[arg(long)]
        status: Option<String>,
        /// Filter by account_id (exact match).
        #[arg(long)]
        account_id: Option<String>,
        /// Maximum entries to show.
        #[arg(short = 'n', long, default_value = "20")]
        limit: u32,
        /// Output as JSON (for jq / scripts).
        #[arg(long)]
        json: bool,
        /// Override the gateway URL. Defaults to BOSS_GATEWAY_URL
        /// or http://127.0.0.1:4443.
        #[arg(long)]
        gateway_url: Option<String>,
    },
    /// List accounts, optionally filtered by name substring.
    Accounts {
        /// Case-insensitive substring match against account name.
        #[arg(long, value_name = "NEEDLE")]
        name: Option<String>,
        #[arg(short = 'n', long, default_value = "20")]
        limit: u32,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        gateway_url: Option<String>,
    },
    /// List jobs, optionally filtered by status / kind / account_id.
    Jobs {
        /// Filter by status (open | closed | blocked | ...).
        #[arg(long)]
        status: Option<String>,
        /// Filter by JobKind (e.g. morning-brew, wholesale-keg-order).
        #[arg(long)]
        kind: Option<String>,
        /// Filter by account_id.
        #[arg(long)]
        account_id: Option<String>,
        #[arg(short = 'n', long, default_value = "20")]
        limit: u32,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        gateway_url: Option<String>,
    },
    /// List employees, optionally filtered by role.
    Employees {
        /// Filter by exact role code (e.g. ceo, cto, head-brewer).
        #[arg(long)]
        role: Option<String>,
        #[arg(short = 'n', long, default_value = "20")]
        limit: u32,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        gateway_url: Option<String>,
    },
}

#[derive(Subcommand)]
enum ScriptAction {
    /// List registered scripts
    List {
        /// Filter by category (scraper, monitor, health-check, maintenance)
        #[arg(long)]
        category: Option<String>,
    },
    /// Show details for a script
    Info {
        /// Script ID (e.g., fda-510k-scraper)
        id: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .compact()
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Doctor => doctor::run_install().await,
        Commands::Emit { kind, payload } => cmd_emit(kind, payload).await,
        Commands::Upgrade => upgrade::run().await,
        Commands::Script { action } => match action {
            ScriptAction::List { category } => script::list(category.as_deref()).await,
            ScriptAction::Info { id } => script::info(&id).await,
        },
        Commands::Deploy { action } => match action {
            DeployAction::List => deploy::list().await,
            DeployAction::Run {
                service,
                skip_build,
            } => deploy::run(service.as_deref(), skip_build).await,
            DeployAction::Web => deploy::deploy_web().await,
            DeployAction::Clean => deploy::clean().await,
        },
        Commands::Status { json } => ops::status(json).await,
        Commands::Restart { service, json } => ops::restart(&service, json).await,
        Commands::Logs {
            service,
            lines,
            follow,
            json,
        } => ops::logs(&service, lines, follow, json).await,
        Commands::Backup { json } => ops::backup(json).await,
        Commands::Assets { action } => match action {
            AssetsAction::RebuildProjection { postgres_url } => {
                cmd_assets_rebuild_projection(&postgres_url).await
            }
        },
        Commands::Docs { action } => match action {
            DocsAction::Reindex => docs::reindex().await,
            DocsAction::FlushPending => docs::flush_pending().await,
        },
        Commands::Ledger { action } => match action {
            LedgerAction::Rebuild { postgres_url, json } => {
                cmd_ledger_rebuild(&postgres_url, json).await
            }
            LedgerAction::Periods { postgres_url, json } => {
                cmd_ledger_periods(&postgres_url, json).await
            }
            LedgerAction::Lock {
                starts_on,
                postgres_url,
                locked_by,
            } => cmd_ledger_lock(&postgres_url, &starts_on, &locked_by).await,
            LedgerAction::Unlock {
                starts_on,
                postgres_url,
            } => cmd_ledger_unlock(&postgres_url, &starts_on).await,
        },
        Commands::Inspect { action } => match action {
            InspectAction::Invoices {
                status,
                account_id,
                limit,
                json,
                gateway_url,
            } => {
                let gw = inspect::resolve_gateway_url(gateway_url.as_deref());
                inspect::invoices(status.as_deref(), account_id.as_deref(), limit, json, &gw).await
            }
            InspectAction::Accounts {
                name,
                limit,
                json,
                gateway_url,
            } => {
                let gw = inspect::resolve_gateway_url(gateway_url.as_deref());
                inspect::accounts(name.as_deref(), limit, json, &gw).await
            }
            InspectAction::Jobs {
                status,
                kind,
                account_id,
                limit,
                json,
                gateway_url,
            } => {
                let gw = inspect::resolve_gateway_url(gateway_url.as_deref());
                inspect::jobs(
                    status.as_deref(),
                    kind.as_deref(),
                    account_id.as_deref(),
                    limit,
                    json,
                    &gw,
                )
                .await
            }
            InspectAction::Employees {
                role,
                limit,
                json,
                gateway_url,
            } => {
                let gw = inspect::resolve_gateway_url(gateway_url.as_deref());
                inspect::employees(role.as_deref(), limit, json, &gw).await
            }
        },
        Commands::Audit {
            kind,
            source,
            limit,
            json,
        } => ops::audit(kind.as_deref(), source.as_deref(), limit, json).await,
        Commands::Sim { action } => match action {
            SimAction::Replay {
                config,
                catalog,
                api_url,
                live,
            } => cmd_sim_replay(&config, catalog.as_deref(), &api_url, live).await,
        },
    }
}

/// Thin wrapper: forward every flag to the installed `boss-sim`
/// binary and inherit stdio so progress lines stream live.
async fn cmd_sim_replay(
    config: &std::path::Path,
    catalog: Option<&std::path::Path>,
    api_url: &str,
    live: bool,
) -> Result<()> {
    let mut cmd = std::process::Command::new("boss-sim");
    cmd.arg("replay")
        .arg("--config")
        .arg(config)
        .arg("--api-url")
        .arg(api_url);
    if let Some(c) = catalog {
        cmd.arg("--catalog").arg(c);
    }
    if live {
        cmd.arg("--live");
    }
    let status = cmd
        .status()
        .map_err(|e| anyhow::anyhow!("failed to spawn boss-sim: {e}. Is `boss-sim` on PATH?"))?;
    if !status.success() {
        anyhow::bail!("boss-sim exited with status {status}");
    }
    Ok(())
}

async fn cmd_ledger_rebuild(postgres_url: &str, json: bool) -> Result<()> {
    use sqlx::postgres::PgPoolOptions;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(postgres_url)
        .await?;

    if !json {
        println!("Rebuilding GL projection from financial_facts (open periods only)...");
    }
    let report = boss_ledger::rebuild(&pool)
        .await
        .map_err(|e| anyhow::anyhow!("rebuild failed: {e}"))?;

    if json {
        let out = serde_json::json!({
            "facts_processed": report.facts_processed,
            "entries_dropped": report.entries_dropped,
            "entries_created": report.entries_created,
            "periods_rebuilt": report.periods_rebuilt,
            "total_debits": report.total_debits.to_string(),
            "total_credits": report.total_credits.to_string(),
            "balanced": report.is_balanced(),
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        println!(
            "  {} facts processed → {} entries created ({} dropped, {} periods rebuilt)",
            report.facts_processed,
            report.entries_created,
            report.entries_dropped,
            report.periods_rebuilt,
        );
        println!(
            "  trial balance: debits=${} credits=${}  {}",
            report.total_debits,
            report.total_credits,
            if report.is_balanced() {
                "BALANCED"
            } else {
                "MISMATCH"
            },
        );
    }

    if !report.is_balanced() {
        anyhow::bail!(
            "trial balance mismatch: debits={} credits={}",
            report.total_debits,
            report.total_credits
        );
    }
    Ok(())
}

async fn cmd_ledger_periods(postgres_url: &str, json: bool) -> Result<()> {
    use sqlx::postgres::PgPoolOptions;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(postgres_url)
        .await?;
    let periods = boss_ledger::periods::list_periods(&pool)
        .await
        .map_err(|e| anyhow::anyhow!("list_periods: {e}"))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&periods)?);
    } else {
        println!(
            "{:<12}  {:<8}  {:>8}  {:>14}  {:>14}  LOCKED_BY",
            "STARTS_ON", "STATUS", "ENTRIES", "DEBITS", "CREDITS"
        );
        println!("{}", "-".repeat(80));
        for p in &periods {
            println!(
                "{:<12}  {:<8}  {:>8}  {:>14}  {:>14}  {}",
                p.starts_on,
                p.status,
                p.entry_count,
                p.total_debits,
                p.total_credits,
                p.locked_by.as_deref().unwrap_or("-")
            );
        }
    }
    Ok(())
}

async fn cmd_ledger_lock(postgres_url: &str, starts_on: &str, locked_by: &str) -> Result<()> {
    use sqlx::postgres::PgPoolOptions;
    let date: chrono::NaiveDate = starts_on
        .parse()
        .map_err(|e| anyhow::anyhow!("bad date `{starts_on}`: {e}"))?;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(postgres_url)
        .await?;
    let id: uuid::Uuid =
        sqlx::query_scalar("SELECT id FROM gl_periods WHERE kind = 'month' AND starts_on = $1")
            .bind(date)
            .fetch_optional(&pool)
            .await?
            .ok_or_else(|| anyhow::anyhow!("no period with starts_on={starts_on}"))?;
    let stamp = boss_core::publisher::EventStamp::new(
        "ledger",
        boss_core::actor::ActorId::Automation("operator-cli".into()),
        chrono::Utc::now(),
    );
    let checksum = boss_ledger::periods::lock_period(&pool, id, locked_by, &stamp, locked_by)
        .await
        .map_err(|e| anyhow::anyhow!("lock_period: {e}"))?;
    println!("locked period {starts_on} — {checksum}");
    Ok(())
}

async fn cmd_ledger_unlock(postgres_url: &str, starts_on: &str) -> Result<()> {
    use sqlx::postgres::PgPoolOptions;
    let date: chrono::NaiveDate = starts_on
        .parse()
        .map_err(|e| anyhow::anyhow!("bad date `{starts_on}`: {e}"))?;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(postgres_url)
        .await?;
    let id: uuid::Uuid =
        sqlx::query_scalar("SELECT id FROM gl_periods WHERE kind = 'month' AND starts_on = $1")
            .bind(date)
            .fetch_optional(&pool)
            .await?
            .ok_or_else(|| anyhow::anyhow!("no period with starts_on={starts_on}"))?;
    let stamp = boss_core::publisher::EventStamp::new(
        "ledger",
        boss_core::actor::ActorId::Automation("operator-cli".into()),
        chrono::Utc::now(),
    );
    boss_ledger::periods::unlock_period(&pool, id, &stamp, "operator-cli")
        .await
        .map_err(|e| anyhow::anyhow!("unlock_period: {e}"))?;
    println!("unlocked period {starts_on}");
    Ok(())
}

async fn cmd_assets_rebuild_projection(postgres_url: &str) -> Result<()> {
    use sqlx::postgres::PgPoolOptions;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(postgres_url)
        .await?;
    let assets = boss_assets::PgAssets::new(pool);
    println!("Rebuilding systems projection from system_events log...");
    let written = assets
        .rebuild_projection()
        .await
        .map_err(|e| anyhow::anyhow!("rebuild failed: {e}"))?;
    println!("Wrote {written} rows.");
    Ok(())
}

async fn cmd_emit(kind: String, payload: String) -> Result<()> {
    let payload: serde_json::Value = serde_json::from_str(&payload)?;
    // CLI one-off — boundary tool that builds an Event for stdout
    // inspection (not for publishing). Wall-clock at the boundary
    // matches the operator's `now`; nothing reads this event.
    let event = boss_core::event::Event::new("cli", kind, payload, chrono::Utc::now());
    println!("{}", serde_json::to_string_pretty(&event)?);
    Ok(())
}
