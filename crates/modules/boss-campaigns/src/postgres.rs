//! Postgres adapter. The create path is the crate's contract in one
//! transaction: domain row + `subjects` identity row (Q1
//! write-through) + `campaigns.campaign.created` outbox event
//! (#118 transactional outbox). All three land or none do — a
//! campaign can no longer exist live without being reproducible
//! from the log.

use async_trait::async_trait;
use boss_core::event::Event;
use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::port::{CampaignsError, CampaignsRepository};
use crate::types::Campaign;

pub struct PgCampaigns {
    pool: PgPool,
}

impl PgCampaigns {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn storage(e: impl std::fmt::Display) -> CampaignsError {
    CampaignsError::Storage(e.to_string())
}

#[async_trait]
impl CampaignsRepository for PgCampaigns {
    async fn create_campaign_at(
        &self,
        campaign: &Campaign,
        now: DateTime<Utc>,
    ) -> Result<bool, CampaignsError> {
        let mut tx = self.pool.begin().await.map_err(storage)?;
        let inserted = sqlx::query(
            "INSERT INTO campaigns (id, name, status, starts_on, ends_on, metadata, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) \
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(&campaign.id)
        .bind(&campaign.name)
        .bind(&campaign.status)
        .bind(campaign.starts_on)
        .bind(campaign.ends_on)
        .bind(&campaign.metadata)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(storage)?
        .rows_affected()
            > 0;

        if inserted {
            boss_subject_kinds::subjects::record_subject_in_tx(
                &mut tx,
                "campaign",
                &campaign.id,
                Some(&campaign.name),
            )
            .await
            .map_err(CampaignsError::Storage)?;

            let payload = serde_json::json!({
                "id": campaign.id,
                "name": campaign.name,
                "status": campaign.status,
                "starts_on": campaign.starts_on,
                "ends_on": campaign.ends_on,
                "metadata": campaign.metadata,
            });
            let event = Event::new("boss-campaigns", "campaigns.campaign.created", payload, now);
            boss_events::outbox::record_event_in_tx(&mut tx, &event)
                .await
                .map_err(CampaignsError::Storage)?;
        }

        tx.commit().await.map_err(storage)?;
        Ok(inserted)
    }

    async fn get_campaign(&self, id: &str) -> Result<Option<Campaign>, CampaignsError> {
        let row = sqlx::query_as::<_, CampaignRow>(
            "SELECT id, name, status, starts_on, ends_on, metadata, created_at \
             FROM campaigns WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage)?;
        Ok(row.map(Campaign::from))
    }

    async fn list_campaigns(&self) -> Result<Vec<Campaign>, CampaignsError> {
        let rows = sqlx::query_as::<_, CampaignRow>(
            "SELECT id, name, status, starts_on, ends_on, metadata, created_at \
             FROM campaigns ORDER BY created_at DESC, id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;
        Ok(rows.into_iter().map(Campaign::from).collect())
    }
}

#[derive(sqlx::FromRow)]
struct CampaignRow {
    id: String,
    name: String,
    status: String,
    starts_on: Option<chrono::NaiveDate>,
    ends_on: Option<chrono::NaiveDate>,
    metadata: serde_json::Value,
    created_at: DateTime<Utc>,
}

impl From<CampaignRow> for Campaign {
    fn from(r: CampaignRow) -> Self {
        Campaign {
            id: r.id,
            name: r.name,
            status: r.status,
            starts_on: r.starts_on,
            ends_on: r.ends_on,
            metadata: r.metadata,
            created_at: Some(r.created_at),
        }
    }
}
