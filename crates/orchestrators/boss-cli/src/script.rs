//! CTO Toolbox — script registry and execution.
//!
//! Scripts are agent specs with categories and schedules, held in an
//! embedded registry.

use anyhow::Result;

#[derive(Debug)]
struct Script {
    id: &'static str,
    name: &'static str,
    category: &'static str,
    description: &'static str,
    model: &'static str,
    schedule: &'static str,
}

const SCRIPTS: &[Script] = &[
    Script {
        id: "vendor-eol-tracker",
        name: "Vendor EOL Tracker",
        category: "scraper",
        description: "Scrapes vendor end-of-life announcements for tracked equipment models",
        model: "claude-haiku-4-5",
        schedule: "Daily 06:00 UTC",
    },
    Script {
        id: "competitor-pricing",
        name: "Competitor Pricing Monitor",
        category: "scraper",
        description: "Checks refurb marketplace listings for comparable model pricing trends",
        model: "claude-haiku-4-5",
        schedule: "Weekly Mon 08:00 UTC",
    },
    Script {
        id: "cert-expiry-alerter",
        name: "Certificate Expiry Alerter",
        category: "monitor",
        description: "Monitors TLS certificates, OAuth tokens, and API keys approaching expiration",
        model: "claude-haiku-4-5",
        schedule: "Daily 07:00 UTC",
    },
    Script {
        id: "warranty-cliff-detector",
        name: "Warranty Cliff Detector",
        category: "monitor",
        description: "Scans assets for clusters of warranties expiring in the same month",
        model: "claude-haiku-4-5",
        schedule: "Weekly Fri 12:00 UTC",
    },
    Script {
        id: "db-health-check",
        name: "Postgres Health Check",
        category: "health-check",
        description: "Checks table bloat, index usage, slow queries, and connection pool utilization",
        model: "claude-haiku-4-5",
        schedule: "Every 6h",
    },
    Script {
        id: "nats-throughput-check",
        name: "NATS Throughput Check",
        category: "health-check",
        description: "Measures event bus message rates, slow consumers, and subject cardinality",
        model: "claude-haiku-4-5",
        schedule: "Every 6h",
    },
    Script {
        id: "disk-cleanup",
        name: "Build Artifact Cleanup",
        category: "maintenance",
        description: "Prunes target/ directories and old log files when disk usage exceeds 80%",
        model: "claude-haiku-4-5",
        schedule: "Daily 03:00 UTC",
    },
    Script {
        id: "sim-nightly",
        name: "Nightly Simulation Run",
        category: "maintenance",
        description: "Runs boss-sim with default config and loads events into Postgres",
        model: "claude-sonnet-4-6",
        schedule: "Daily 02:00 UTC",
    },
];

pub async fn list(category: Option<&str>) -> Result<()> {
    let filtered: Vec<&Script> = match category {
        Some(cat) => SCRIPTS.iter().filter(|s| s.category == cat).collect(),
        None => SCRIPTS.iter().collect(),
    };

    if filtered.is_empty() {
        println!("No scripts found.");
        return Ok(());
    }

    // Header
    println!("{:<24} {:<14} {:<22} NAME", "ID", "CATEGORY", "SCHEDULE");
    println!("{}", "-".repeat(80));

    for s in &filtered {
        println!(
            "{:<24} {:<14} {:<22} {}",
            s.id, s.category, s.schedule, s.name
        );
    }

    println!("\n{} scripts registered.", filtered.len());
    Ok(())
}

pub async fn info(script_id: &str) -> Result<()> {
    let script = SCRIPTS
        .iter()
        .find(|s| s.id == script_id)
        .ok_or_else(|| anyhow::anyhow!("script not found: {script_id}"))?;

    println!("{}", script.name);
    println!("{}", "=".repeat(script.name.len()));
    println!();
    println!("ID:          {}", script.id);
    println!("Category:    {}", script.category);
    println!("Description: {}", script.description);
    println!("Model:       {}", script.model);
    println!("Schedule:    {}", script.schedule);

    Ok(())
}
