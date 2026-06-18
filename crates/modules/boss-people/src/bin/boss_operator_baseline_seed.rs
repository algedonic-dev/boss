//! `boss-operator-baseline-seed` — POST the operator-baseline
//! (system CEO/CTO/COO + bootstrap admin) to the people-api at
//! `POST /api/people`, idempotently.
//!
//! All data loading goes through the public API: this binary no
//! longer writes `people.employee.created` rows into `audit_log`
//! directly. Each hire is POSTed to the people-api, which runs the
//! policy check + validation and emits the event itself through the
//! service-mediated path — the same pipeline every other employee
//! lands through.
//!
//! Event time is clock-authoritative: the people-api stamps each
//! event from clock-api, so this binary sets no timestamp. The
//! caller primes the clock (e.g. via `/api/clock/configure`) before
//! running this.
//!
//! Source TOML lives at `infra/operator-baseline/operator_hires.toml`
//! (system-level concern, not tenant data).
//!
//! Idempotence: a 409 Conflict on a duplicate `id` is treated as
//! "already exists → skip". Safe to run twice; safe to run before or
//! after the brewery seed load.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use boss_people::types::Employee;
use clap::Parser;
use reqwest::blocking::Client;
use serde::Deserialize;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "boss-operator-baseline-seed",
    about = "Seed operator-baseline employees via the people-api POST /api/people",
    version
)]
struct Cli {
    /// People-API base URL. Defaults to `boss_ports::url("people")`.
    #[arg(long, default_value_t = boss_ports::url("people"))]
    people_base: String,

    /// TOML file describing the operator hires. Defaults to the
    /// in-tree `infra/operator-baseline/operator_hires.toml`.
    #[arg(long, default_value = "infra/operator-baseline/operator_hires.toml")]
    seed_path: PathBuf,
}

#[derive(Debug, Deserialize)]
struct OperatorSeed {
    hire: Vec<Employee>,
}

/// Find the bootstrap-admin email. Precedence:
///   1. BOSS_BOOTSTRAP_ADMIN_EMAIL env var
///   2. First `[[credential]]` row in BOSS_AUTH_FILE (default
///      /var/lib/boss/auth/credentials.toml). This is the file
///      the gateway's local_auth reads; using it as the source
///      means there's one canonical "who is the operator" file.
///
/// Returns None when neither produces a value — bootstrap runs
/// without injection, and the operator must POST /api/people
/// manually before login.
fn resolve_bootstrap_admin_email() -> Option<String> {
    if let Ok(email) = std::env::var("BOSS_BOOTSTRAP_ADMIN_EMAIL") {
        let trimmed = email.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    let auth_file = std::env::var("BOSS_AUTH_FILE")
        .unwrap_or_else(|_| "/var/lib/boss/auth/credentials.toml".to_string());
    let raw = std::fs::read_to_string(&auth_file).ok()?;
    let parsed: toml::Value = raw.parse().ok()?;
    parsed
        .get("credential")?
        .as_array()?
        .first()?
        .get("email")?
        .as_str()
        .map(|s| s.to_string())
}

fn display_name_from_email(email: &str) -> String {
    let local = email.split('@').next().unwrap_or(email);
    let mut chars = local.chars();
    match chars.next() {
        Some(c) => c.to_ascii_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .compact()
        .init();
    let cli = Cli::parse();

    let raw = fs::read_to_string(&cli.seed_path).with_context(|| {
        format!(
            "reading operator-baseline seed at {}",
            cli.seed_path.display()
        )
    })?;
    let mut seed: OperatorSeed =
        toml::from_str(&raw).with_context(|| format!("parsing {}", cli.seed_path.display()))?;

    // The bootstrap admin's email is the one
    // the operator will log in as. Inject an emp-bootstrap-admin
    // row at the head of the hire list with role=platform-admin
    // and email pulled from BOSS_BOOTSTRAP_ADMIN_EMAIL (or the
    // first email in the local credentials.toml file when the
    // env var is unset).
    //
    // This is the single bootstrap row the system needs to
    // bootstrap itself: after this row exists, the credential →
    // Employee resolution via lower(email) match works at login
    // time without needing the gateway to auto-provision.
    if let Some(bootstrap_email) = resolve_bootstrap_admin_email() {
        let already_present = seed.hire.iter().any(|h| {
            h.email
                .as_deref()
                .is_some_and(|e| e.eq_ignore_ascii_case(&bootstrap_email))
        });
        if !already_present {
            let bootstrap = Employee {
                id: "emp-bootstrap-admin".to_string(),
                name: Some(display_name_from_email(&bootstrap_email)),
                email: Some(bootstrap_email.clone()),
                role: Some("platform-admin".to_string()),
                department: Some("platform".to_string()),
                skill_level: None,
                skills: Vec::new(),
                hire_date: Some(
                    chrono::NaiveDate::from_ymd_opt(2023, 1, 1).expect("static date is valid"),
                ),
                location: Some("loc-hq".to_string()),
                manager_id: None,
                employment_type: Some("full-time".to_string()),
                status: Some("active".to_string()),
                certifications: Vec::new(),
                annual_salary_cents: None,
            };
            info!(
                operator_id = %bootstrap.id,
                email = bootstrap.email.as_deref().unwrap_or(""),
                "injecting bootstrap-admin Employee row at head of hire list"
            );
            seed.hire.insert(0, bootstrap);
        } else {
            info!(
                email = %bootstrap_email,
                "bootstrap-admin email already present in operator_hires.toml; no injection"
            );
        }
    } else {
        info!(
            "BOSS_BOOTSTRAP_ADMIN_EMAIL unset and no credentials file readable; \
             skipping bootstrap-admin Employee injection. Logins will require an \
             explicit /api/people POST or a matching template row in \
             operator_hires.toml."
        );
    }

    // The operator-baseline loads AS the public API like every
    // other external caller. The actor identity is a dedicated
    // platform-admin automation identity — these are founding
    // platform operators, not tenant employees.
    let user_header = serde_json::json!({
        "id": "automation:operator-baseline",
        "role": "platform-admin",
        "access_tier": "operator",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": "platform",
    })
    .to_string();

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        "x-boss-user",
        reqwest::header::HeaderValue::from_str(&user_header)
            .with_context(|| "x-boss-user header value")?,
    );
    headers.insert(
        "x-sim-origin",
        reqwest::header::HeaderValue::from_static("true"),
    );
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/json"),
    );

    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .with_context(|| "building reqwest client")?;

    let url = format!("{}/api/people", cli.people_base.trim_end_matches('/'));

    let mut inserted = 0u64;
    let mut skipped = 0u64;
    let mut failed = 0u64;
    for emp in &seed.hire {
        let resp = match client.post(&url).headers(headers.clone()).json(emp).send() {
            Ok(r) => r,
            Err(e) => {
                warn!(operator_id = %emp.id, error = %e, "POST operator transport error");
                failed += 1;
                continue;
            }
        };
        let status = resp.status();
        if status.is_success() {
            inserted += 1;
            info!(operator_id = %emp.id, role = emp.role.as_deref().unwrap_or(""), "operator hired");
        } else if status.as_u16() == 409 {
            skipped += 1;
            info!(operator_id = %emp.id, "operator already hired, skipping");
        } else {
            let body = resp.text().unwrap_or_default();
            warn!(operator_id = %emp.id, %status, body = %body, "POST operator failed");
            failed += 1;
        }
    }

    if failed > 0 {
        anyhow::bail!(
            "{failed} operator-baseline POSTs failed (inserted={inserted}, skipped={skipped}). \
             The operator-baseline must land before downstream references resolve."
        );
    }

    info!(inserted, skipped, "operator-baseline seed complete");
    Ok(())
}
