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

#[async_trait]
pub trait CampaignsRepository: Send + Sync {
    /// Create a campaign. Idempotent on `id` (ON CONFLICT DO
    /// NOTHING): re-POSTing an existing id is a no-op that reports
    /// `inserted = false` — and emits nothing, so replays and the
    /// daemon's boot-time pool sync can't double-write the log.
    ///
    /// The Pg adapter does the whole birth in ONE transaction:
    /// domain row + `subjects` identity row (Q1 write-through) +
    /// `campaigns.campaign.created` outbox event (#118).
    async fn create_campaign_at(
        &self,
        campaign: &Campaign,
        now: DateTime<Utc>,
    ) -> Result<bool, CampaignsError>;

    async fn get_campaign(&self, id: &str) -> Result<Option<Campaign>, CampaignsError>;

    /// All campaigns, newest first.
    async fn list_campaigns(&self) -> Result<Vec<Campaign>, CampaignsError>;
}
