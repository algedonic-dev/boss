//! Port (trait) for the campaigns domain. Adapters: PgCampaigns
//! (postgres) + InMemoryCampaigns (tests).

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::types::Campaign;

#[derive(Debug, thiserror::Error)]
pub enum CampaignsError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("storage: {0}")]
    Storage(String),
    #[error("invalid: {0}")]
    Invalid(String),
}

/// Refuse a campaign Postgres cannot store: a NUL byte in any text field
/// or anywhere inside the metadata (TEXT and JSONB both reject one).
/// Both adapters call this before writing, so the refusal is `Invalid`
/// naming the field on each — until the adapters-agree suite (backlog
/// be459ab9) Postgres answered with its encoding error as `Storage` (a
/// 500) and the double stored the byte.
pub fn refuse_nul(campaign: &Campaign) -> Result<(), CampaignsError> {
    fn json_holds_nul(v: &serde_json::Value) -> bool {
        match v {
            serde_json::Value::String(s) => s.contains('\0'),
            serde_json::Value::Array(a) => a.iter().any(json_holds_nul),
            serde_json::Value::Object(o) => {
                o.iter().any(|(k, v)| k.contains('\0') || json_holds_nul(v))
            }
            _ => false,
        }
    }
    let text = [
        ("id", campaign.id.as_str()),
        ("name", campaign.name.as_str()),
        ("status", campaign.status.as_str()),
    ];
    let field = text
        .iter()
        .find(|(_, v)| v.contains('\0'))
        .map(|(f, _)| *f)
        .or_else(|| json_holds_nul(&campaign.metadata).then_some("metadata"));
    match field {
        Some(f) => Err(CampaignsError::Invalid(format!(
            "{f} holds a NUL byte, which cannot be stored"
        ))),
        None => Ok(()),
    }
}

#[async_trait]
pub trait CampaignsRepository: Send + Sync {
    /// Create a campaign. Idempotent on `id` (ON CONFLICT DO
    /// NOTHING): re-POSTing an existing id is a no-op that reports
    /// `inserted = false` — and emits nothing, so replays and the
    /// daemon's boot-time pool sync can't double-write the log.
    ///
    /// The Pg adapter does the whole birth in ONE transaction:
    /// domain row + `subjects` identity row (Q1 write-through) +
    /// `campaigns.campaign.created` outbox event (#118); the double
    /// records the same fact (`events::campaign_created`).
    ///
    /// The row is stored with `created_at = now` at the column's
    /// precision, the microsecond. A NUL byte anywhere is refused
    /// `Invalid` naming the field (`refuse_nul`).
    async fn create_campaign_at(
        &self,
        campaign: &Campaign,
        now: DateTime<Utc>,
    ) -> Result<bool, CampaignsError>;

    /// One campaign; `None` for an id nobody holds — including one
    /// holding a NUL byte, which no stored id can.
    async fn get_campaign(&self, id: &str) -> Result<Option<Campaign>, CampaignsError>;

    /// All campaigns, newest first; a tie on `created_at` in BYTE order
    /// of `id` (backlog 2987fb2d: never the database's locale).
    async fn list_campaigns(&self) -> Result<Vec<Campaign>, CampaignsError>;
}
