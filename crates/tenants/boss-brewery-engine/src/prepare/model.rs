//! [`prepare_model`] — the brewery's single tenant-prepare entry
//! point. Against a running BOSS stack — each service on its own port
//! by default, or all behind one gateway URL — it seeds the entire
//! brewery tenant model through the public API, in dependency order:
//!
//! 1. classes — POST /api/classes/batch (the taxonomy that employee +
//!    account writes validate against, so it lands first).
//! 2. JobKinds — the brewery workflow registry ([`super::publish_job_kinds`]).
//! 3. policy — tenant role grants ([`boss_policy::bootstrap`]).
//! 4. data — operators, employees, accounts, vendors, messages,
//!    finished-goods, raw materials, equipment, assets, and opening
//!    balances ([`super::seed_tenant_data`]).
//!
//! This collapses what reset-to-baseline / seed-brewery-tenant.sh
//! drove as four scattered binary + curl steps into one library call,
//! so the offline regen, the live demo, and CI all run identical code.
//!
//! Two adjacent concerns stay with the caller by design:
//!
//! - **Clock loop-bounds.** [`super::seed_tenant_data`] rebases the
//!   clock to the sim epoch internally (the pre-sim provisioning
//!   window) so seed writes stamp at day 0; the epoch_end + warp_factor
//!   that bound the sim's run are set by whoever drives the clock
//!   (reset-to-baseline / the sim daemon), not here.
//! - **Platform baseline.** The platform operator-baseline
//!   (`bootstrap-admin`, minted from a deploy credentials file) and
//!   the projection rebuilds are platform-init / read-model concerns —
//!   not tenant data, and not API-orchestratable — so they remain in
//!   the deploy/reset orchestration.

use std::path::Path;

use anyhow::{Context, Result};
use reqwest::blocking::Client;
use tracing::info;

use super::{SeedBases, publish_job_kinds, seed_tenant_data};

/// Seed the entire brewery tenant model through the public API.
///
/// `gateway_base` selects the routing: `None` sends each service to
/// its own localhost port (`boss_ports` defaults) — the reset /
/// quickstart path, which seeds with the gateway stopped. `Some(url)`
/// routes every `/api/*` prefix through one gateway URL (a deployment
/// whose gateway is up during seeding). `seeds_dir` is the brewery
/// seed bundle (`examples/brewery/seeds`). Idempotent throughout —
/// safe to re-run.
pub fn prepare_model(gateway_base: Option<&str>, seeds_dir: &Path) -> Result<()> {
    // classes / jobs / policy each take a single base; the tenant-data
    // seeder takes the full per-service map. A gateway override points
    // all of them at one URL; otherwise each resolves to its own port.
    let classes_base = gateway_base
        .map(str::to_string)
        .unwrap_or_else(|| boss_ports::url("classes"));
    let calendar_base = gateway_base
        .map(str::to_string)
        .unwrap_or_else(|| boss_ports::url("calendar"));
    let jobs_base = gateway_base
        .map(str::to_string)
        .unwrap_or_else(|| boss_ports::url("jobs"));
    let policy_base = gateway_base
        .map(str::to_string)
        .unwrap_or_else(|| boss_ports::url("policy"));
    let data_bases = match gateway_base {
        Some(g) => SeedBases::all(g),
        None => SeedBases::from_ports(),
    };

    info!(
        gateway = ?gateway_base,
        seeds = %seeds_dir.display(),
        "preparing brewery tenant model"
    );

    // 1. Classes first — employee role + account-type writes validate
    //    against the Class registry.
    seed_classes(&classes_base, seeds_dir)?;

    // 1b. Business calendars — reference data (banking/tax holidays) the
    //     dispatcher's timing triggers and the simulator resolve business
    //     days from. Like classes: load before anything that consumes them.
    seed_business_calendars(&calendar_base, seeds_dir)?;

    // 2. JobKinds — the workflow registry the sim drives. dev=true
    //    auto-walks the sign-off step (the brewery seed runs
    //    unattended, same as `boss-brewery-bootstrap --dev`).
    //    publish_job_kinds takes the job_kinds.toml FILE (not the dir).
    publish_job_kinds(
        &jobs_base,
        &seeds_dir.join("job_kinds.toml"),
        true,
        false,
        None,
    )?;

    // 3. Tenant policy grants — core ships only platform rules; the
    //    brewery org chart's row-level access matrix arrives here.
    boss_policy::bootstrap::publish_policy_rules(
        &policy_base,
        &seeds_dir.join("policy_rules.toml"),
        false,
        "brewery-policy-bootstrap",
        None,
    )?;

    // 4. Tenant data — operators, employees, accounts, vendors,
    //    messages, finished-goods, raw materials, equipment, assets,
    //    + opening balances.
    seed_tenant_data(&data_bases, seeds_dir, None)?;

    info!("brewery tenant model prepared");
    Ok(())
}

/// POST the brewery's Class registry (`seeds/classes.json`) to
/// `/api/classes/batch`. Classes are the taxonomy employee + account
/// writes validate against, so they land before any of those.
///
/// `x-sim-origin: true` lets the batch land as seed-origin data
/// (matching the reset-to-baseline curl); the seed-loader identity
/// carries platform-admin provenance.
fn seed_classes(api_base: &str, seeds_dir: &Path) -> Result<()> {
    let path = seeds_dir.join("classes.json");
    let body =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;

    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let url = format!("{}/api/classes/batch", api_base.trim_end_matches('/'));
    let resp = client
        .post(&url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header("x-sim-origin", "true")
        .header(
            "x-boss-user",
            r#"{"id":"automation:classes-seed","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}"#,
        )
        .body(body)
        .send()
        .with_context(|| format!("POST {url}"))?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("POST {url} → {status} {}", resp.text().unwrap_or_default());
    }
    info!(path = %path.display(), "brewery classes seeded");
    Ok(())
}

/// POST the brewery's business calendars (`seeds/business_calendars.json`)
/// to `/api/calendar/business-calendars/batch`. These are the banking +
/// tax calendars the dispatcher's timing triggers and the simulator
/// resolve business days from — DATA, not hardcoded Rust. Same
/// seed-origin provenance as the Class registry.
fn seed_business_calendars(api_base: &str, seeds_dir: &Path) -> Result<()> {
    let path = seeds_dir.join("business_calendars.json");
    let body =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;

    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let url = format!(
        "{}/api/calendar/business-calendars/batch",
        api_base.trim_end_matches('/')
    );
    let resp = client
        .post(&url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header("x-sim-origin", "true")
        .header(
            "x-boss-user",
            r#"{"id":"automation:calendar-seed","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}"#,
        )
        .body(body)
        .send()
        .with_context(|| format!("POST {url}"))?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("POST {url} → {status} {}", resp.text().unwrap_or_default());
    }
    info!(path = %path.display(), "brewery business calendars seeded");
    Ok(())
}
