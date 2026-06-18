//! Escalation router — auto-messages tenant-defined executives
//! when a critical ticket lands on a top-tier account.
//!
//! Subscribes to `jobs.job.created` events; for each new Job that
//! matches the rule (priority ∈ {emergency, urgent} AND the job's
//! subject is a platinum/gold account), resolves the executive
//! recipients (every Employee whose `role` Class is tagged
//! `metadata.is_executive = true` per the Class registry) via
//! `boss-people` and posts a `signal`-kind message to each via
//! `boss-messages`. Fire-and-forget: any failure is logged, never
//! blocks other events.
//!
//! v1 rule is hard-coded here rather than living in a database
//! table. Promoting to a configurable `escalation_rules` table is
//! the next pass if more than one rule shows up.
//!
//! ## Subject handling
//!
//! - `Subject::Account { id }` → the account_id is right there.
//! - `Subject` of kind `asset` → v1 skips this case; the
//!   asset→account lookup adds an extra HTTP hop and isn't
//!   required to ship the first useful escalation. A follow-up can
//!   add `GET /api/assets/{asset_id}` to cover asset-subject
//!   service tickets.
//! - Other subjects don't map to a account and are skipped.

use std::sync::Arc;
use std::time::Duration;

use boss_core::event::Event;
use boss_core::job::{Job, Priority, Subject};
use boss_core::port::EventBus;
use serde::Deserialize;
use tracing::{debug, info, warn};

use crate::events::JOB_CREATED;

/// Knobs the escalation router needs at startup. Every field has a
/// reasonable prod default so a local dev loop can spawn it with
/// `EscalationConfig::default()`.
#[derive(Debug, Clone)]
pub struct EscalationConfig {
    pub people_url: String,
    /// Accounts are served by the accounts-api (split out of boss-people),
    /// so the account lookup uses this base; `people_url` is the employee
    /// lookup.
    pub accounts_url: String,
    pub messages_url: String,
    pub request_timeout: Duration,
}

impl Default for EscalationConfig {
    fn default() -> Self {
        Self {
            people_url: std::env::var("BOSS_PEOPLE_URL")
                .unwrap_or_else(|_| boss_ports::url("people")),
            accounts_url: std::env::var("BOSS_ACCOUNTS_URL")
                .unwrap_or_else(|_| boss_ports::url("accounts")),
            messages_url: std::env::var("BOSS_MESSAGES_URL")
                .unwrap_or_else(|_| boss_ports::url("messages")),
            request_timeout: Duration::from_secs(5),
        }
    }
}

/// Slim view of `/api/people/accounts/{id}` — we only need tier +
/// name to decide whether the escalation fires. The accounts API
/// returns `AccountWithContacts` with `#[serde(flatten)]` on the
/// Account fields, so on the wire `tier` and `name` sit at the
/// top level alongside `contacts`; this struct must decode that
/// flat shape, not a nested `{ "account": { tier, name } }`.
#[derive(Debug, Deserialize)]
struct AccountSummary {
    tier: String,
    name: String,
}

#[derive(Debug, Deserialize)]
struct Employee {
    id: String,
    role: String,
    name: String,
}

/// Spawn the escalation subscriber in the background. Returns a
/// `JoinHandle` so the caller can await it on shutdown; normally you
/// just drop the handle and let the task run for the lifetime of the
/// service.
pub fn spawn_router(
    bus: Arc<dyn EventBus>,
    config: EscalationConfig,
) -> tokio::task::JoinHandle<()> {
    let client = match reqwest::Client::builder()
        .timeout(config.request_timeout)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "escalation router: failed to build http client; disabled");
            return tokio::spawn(async {});
        }
    };

    tokio::spawn(async move {
        let mut stream = match bus.subscribe(JOB_CREATED).await {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "escalation router: subscribe failed; disabled");
                return;
            }
        };
        info!(
            people_url = %config.people_url,
            messages_url = %config.messages_url,
            "escalation router subscribed to {}",
            JOB_CREATED,
        );
        while let Some(event) = stream.next().await {
            if let Err(e) = handle_event(&client, &config, &event).await {
                warn!(error = %e, "escalation router: handler error");
            }
        }
        info!("escalation router stream closed");
    })
}

async fn handle_event(
    client: &reqwest::Client,
    config: &EscalationConfig,
    event: &Event,
) -> Result<(), String> {
    let job: Job = match serde_json::from_value(event.payload.clone()) {
        Ok(j) => j,
        Err(_) => {
            // Replay payloads can diverge from the current Job shape.
            // Silently ignore rather than spam logs.
            return Ok(());
        }
    };

    if !priority_triggers(job.priority) {
        return Ok(());
    }
    let Some(account_id) = account_id_for_subject(&job.subject) else {
        return Ok(());
    };

    let account = fetch_account(client, &config.accounts_url, &account_id).await?;
    if !tier_triggers(&account.tier) {
        debug!(
            tier = %account.tier,
            account = %account_id,
            "escalation: account tier below threshold"
        );
        return Ok(());
    }

    let employees = fetch_employees(client, &config.people_url).await?;
    let recipients: Vec<&Employee> = employees
        .iter()
        .filter(|e| boss_core::roles::is_executive(&e.role))
        .collect();
    if recipients.is_empty() {
        warn!("escalation: no executive-role employees found; skipping");
        return Ok(());
    }

    let subject = format!(
        "[Escalation] {priority} ticket on {tier} account {name}",
        priority = priority_label(job.priority),
        tier = account.tier,
        name = account.name,
    );
    let body = format!(
        "A {priority}-priority {kind} job landed on {name} ({tier} tier).\n\n\
        Job: {title}\nJob ID: {job_id}\n\n\
        Open the Job detail to review and assign follow-up.",
        priority = priority_label(job.priority),
        kind = job.kind,
        name = account.name,
        tier = account.tier,
        title = job.title,
        job_id = job.id,
    );

    for recipient in recipients {
        send_signal(
            client,
            &config.messages_url,
            &recipient.id,
            &subject,
            &body,
            &job.id.to_string(),
        )
        .await
        .map_err(|e| {
            format!(
                "sending signal to {} ({}): {e}",
                recipient.name, recipient.id
            )
        })?;
    }

    info!(
        account = %account_id,
        tier = %account.tier,
        priority = %priority_label(job.priority),
        job = %job.id,
        "escalation: signals sent"
    );
    Ok(())
}

fn priority_triggers(p: Priority) -> bool {
    matches!(p, Priority::Emergency | Priority::Urgent)
}

fn tier_triggers(tier: &str) -> bool {
    matches!(tier, "platinum" | "gold")
}

fn account_id_for_subject(subject: &Subject) -> Option<String> {
    use boss_core::primitives::Subject as _;
    // System / PurchaseOrder / Campaign / Employee / Custom don't
    // map to a account here. System-subject tickets are the most
    // common gap; a follow-up can add an assets lookup.
    if subject.kind() == "account" {
        Some(subject.id().to_string())
    } else {
        None
    }
}

fn priority_label(p: Priority) -> &'static str {
    match p {
        Priority::Emergency => "emergency",
        Priority::Urgent => "urgent",
        Priority::Standard => "standard",
        Priority::Scheduled => "scheduled",
    }
}

async fn fetch_account(
    client: &reqwest::Client,
    accounts_url: &str,
    account_id: &str,
) -> Result<AccountSummary, String> {
    let url = format!("{accounts_url}/api/people/accounts/{account_id}");
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("GET {url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("GET {url}: status {}", resp.status()));
    }
    resp.json::<AccountSummary>()
        .await
        .map_err(|e| format!("decode {url}: {e}"))
}

async fn fetch_employees(
    client: &reqwest::Client,
    people_url: &str,
) -> Result<Vec<Employee>, String> {
    let url = format!("{people_url}/api/people");
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("GET {url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("GET {url}: status {}", resp.status()));
    }
    resp.json().await.map_err(|e| format!("decode {url}: {e}"))
}

async fn send_signal(
    client: &reqwest::Client,
    messages_url: &str,
    recipient_id: &str,
    subject: &str,
    body: &str,
    job_id: &str,
) -> Result<(), String> {
    let url = format!("{messages_url}/api/messages/send");
    let body_json = serde_json::json!({
        "sender_id": "automation:escalation-router",
        "recipient_id": recipient_id,
        "subject": subject,
        "body": body,
        "kind": "signal",
        "entity_ref": { "entity_type": "job", "entity_id": job_id },
    });
    let resp = client
        .post(&url)
        .json(&body_json)
        .send()
        .await
        .map_err(|e| format!("POST {url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("POST {url}: status {}", resp.status()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priority_emergency_and_urgent_trigger() {
        assert!(priority_triggers(Priority::Emergency));
        assert!(priority_triggers(Priority::Urgent));
        assert!(!priority_triggers(Priority::Standard));
        assert!(!priority_triggers(Priority::Scheduled));
    }

    #[test]
    fn tier_only_top_two_trigger() {
        assert!(tier_triggers("platinum"));
        assert!(tier_triggers("gold"));
        assert!(!tier_triggers("silver"));
        assert!(!tier_triggers("bronze"));
    }

    #[test]
    fn account_subject_extracts_id() {
        let s = Subject::new("account", "account-42");
        assert_eq!(account_id_for_subject(&s), Some("account-42".into()));
    }

    #[test]
    fn asset_subject_skipped_in_v1() {
        let s = Subject::new("asset", "SYS-123");
        assert_eq!(account_id_for_subject(&s), None);
    }

    /// Pin the wire-shape contract for `/api/people/accounts/{id}`.
    /// boss-accounts returns `AccountWithContacts` with
    /// `#[serde(flatten)]` on the `Account` field — so `tier` and
    /// `name` sit at the top level alongside `contacts`. Pre-fix
    /// the escalation router decoded into `AccountBundle { account:
    /// AccountSummary }` expecting a wrapper key that never
    /// existed; every escalation event silently dropped during
    /// regen.
    ///
    /// If the accounts API ever changes its response shape (drops
    /// the flatten, renames `tier`, etc.) this test breaks loudly
    /// instead of letting the escalation router silently no-op.
    #[test]
    fn account_summary_decodes_flat_wire_shape() {
        let body = serde_json::json!({
            "id": "account-42",
            "name": "Hopswell Brewing",
            "director": "Pat Director",
            "city": "Austin",
            "state": "TX",
            "tier": "gold",
            "customer_since": "2025-06-01",
            "territory_rep_id": "emp-rep-001",
            "account_type": "wholesale-distributor",
            "contacts": [],
        });
        let summary: AccountSummary =
            serde_json::from_value(body).expect("AccountSummary decodes the flat wire shape");
        assert_eq!(summary.tier, "gold");
        assert_eq!(summary.name, "Hopswell Brewing");
    }
}
