//! Tenant policy-rule seeding, shared by the
//! `boss-policy-bootstrap` binary and tenant `prepare` steps (the
//! brewery's converged prepare calls this directly so the bin and
//! the live demo seed policy through one impl).
//!
//! [`publish_policy_rules`] POSTs each rule in a tenant's
//! `policy_rules.toml` to `/api/policy/rules`. Core ships only
//! platform rules (`platform-admin` / `audit-readonly` /
//! `smoke-tester` / `guest`) — see [`crate::default_rules`]. Tenant
//! role grants (ceo / coo / sales-rep / brewer / controller / …)
//! live in tenant seed data and arrive via this fn.
//!
//! Idempotent: each rule is GETd from `/api/policy/rules/{id}`
//! first; existing rows (operator-tuned or seeded by a prior run)
//! are left alone unless `force`. Hard-fails on any non-2xx
//! response, so a partial failure resumes cleanly on re-run.

use std::path::Path;

use anyhow::{Context, Result};
use reqwest::blocking::Client;
use serde_json::json;
use tracing::{info, warn};

use crate::seed_loader::load_policy_rules;

fn policy_url(api_base: &str, path: &str) -> String {
    format!("{}{}", api_base.trim_end_matches('/'), path)
}

/// POST each rule in `seeds` (a tenant's `policy_rules.toml`) to
/// `/api/policy/rules` on `api_base` (a policy-api or gateway base
/// URL), idempotently.
///
/// `force` overwrites existing rules instead of skipping them (use
/// after editing the seed file). `changed_by` is attributed to rows
/// this run upserts (convention: `"<tenant>-policy-bootstrap"`).
/// `x_boss_user` overrides the default platform-bootstrap header when
/// `Some`. Hard-fails on any non-2xx response.
pub fn publish_policy_rules(
    api_base: &str,
    seeds: &Path,
    force: bool,
    changed_by: &str,
    x_boss_user: Option<&str>,
) -> Result<()> {
    let user_header = x_boss_user.map(|s| s.to_string()).unwrap_or_else(|| {
        json!({
            "id": "automation:bootstrap",
            "role": "platform-admin",
            "access_tier": "operator",
            "territory_account_ids": [],
            "direct_report_ids": [],
            "department": "platform",
        })
        .to_string()
    });
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        "x-boss-user",
        reqwest::header::HeaderValue::from_str(&user_header).context("x-boss-user header value")?,
    );
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/json"),
    );

    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    let rules = load_policy_rules(seeds).with_context(|| format!("loading {}", seeds.display()))?;

    info!(
        seeds = %seeds.display(),
        api_base = %api_base,
        rule_count = rules.len(),
        force = force,
        "starting policy bootstrap"
    );

    let mut posted = 0usize;
    let mut skipped = 0usize;
    for rule in &rules {
        let get_url = policy_url(api_base, &format!("/api/policy/rules/{}", rule.id));
        let resp = client.get(&get_url).headers(headers.clone()).send()?;
        let exists = match resp.status().as_u16() {
            200 => true,
            404 => false,
            other => {
                anyhow::bail!(
                    "GET {get_url} → {other} {}",
                    resp.text().unwrap_or_default()
                );
            }
        };
        if exists && !force {
            skipped += 1;
            continue;
        }
        let post_url = policy_url(api_base, "/api/policy/rules");
        let body = json!({
            "rule": rule,
            "changed_by": changed_by,
        });
        let resp = client
            .post(&post_url)
            .headers(headers.clone())
            .json(&body)
            .send()?;
        if !resp.status().is_success() {
            anyhow::bail!(
                "POST {post_url} ({}) → {} {}",
                rule.id,
                resp.status(),
                resp.text().unwrap_or_default()
            );
        }
        posted += 1;
        if posted.is_multiple_of(25) {
            info!(posted, skipped, total = rules.len(), "progress");
        }
    }

    if posted == 0 && skipped == rules.len() {
        warn!(
            total = rules.len(),
            "every rule already exists; force to overwrite"
        );
    }

    info!(
        posted,
        skipped,
        total = rules.len(),
        "policy bootstrap complete"
    );
    Ok(())
}
