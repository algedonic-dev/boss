//! In-memory adapter — the test double (no mocks; a real
//! implementation of the port, minus durability). Held to Postgres by
//! `tests/the_adapters_agree_on_the_campaign_store_pg.rs` (backlog
//! be459ab9).

use std::collections::BTreeMap;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, DurationRound, TimeDelta, Utc};

use crate::port::{CampaignsError, CampaignsRepository, refuse_nul};
use crate::types::Campaign;

#[derive(Default)]
pub struct InMemoryCampaigns {
    rows: Mutex<BTreeMap<String, Campaign>>,
    recorded: Mutex<Vec<boss_core::event::Event>>,
}

impl InMemoryCampaigns {
    pub fn new() -> Self {
        Self::default()
    }

    /// Events the create path recorded — test visibility (the
    /// in-memory analogue of the Pg adapter's in-tx outbox write).
    pub fn recorded_events(&self) -> Vec<boss_core::event::Event> {
        self.recorded.lock().map(|v| v.clone()).unwrap_or_default()
    }
}

#[async_trait]
impl CampaignsRepository for InMemoryCampaigns {
    async fn create_campaign_at(
        &self,
        campaign: &Campaign,
        now: DateTime<Utc>,
    ) -> Result<bool, CampaignsError> {
        refuse_nul(campaign)?;
        let mut rows = self.rows.lock().unwrap();
        if rows.contains_key(&campaign.id) {
            return Ok(false);
        }
        // The column keeps microseconds; so does the double, or one
        // write reads back unequal across adapters (backlog be459ab9).
        let at = now
            .duration_trunc(TimeDelta::microseconds(1))
            .unwrap_or(now);
        let mut stored = campaign.clone();
        stored.created_at = Some(at);
        rows.insert(campaign.id.clone(), stored);
        if let Ok(mut recorded) = self.recorded.lock() {
            recorded.push(crate::events::campaign_created(campaign, now));
        }
        Ok(true)
    }

    async fn get_campaign(&self, id: &str) -> Result<Option<Campaign>, CampaignsError> {
        Ok(self.rows.lock().unwrap().get(id).cloned())
    }

    async fn list_campaigns(&self) -> Result<Vec<Campaign>, CampaignsError> {
        let rows = self.rows.lock().unwrap();
        let mut all: Vec<Campaign> = rows.values().cloned().collect();
        all.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(a.id.cmp(&b.id)));
        Ok(all)
    }
}
