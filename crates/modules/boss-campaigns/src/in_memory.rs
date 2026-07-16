//! In-memory adapter — the test double (no mocks; a real
//! implementation of the port, minus durability).

use std::collections::BTreeMap;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::port::{CampaignsError, CampaignsRepository};
use crate::types::Campaign;

#[derive(Default)]
pub struct InMemoryCampaigns {
    rows: Mutex<BTreeMap<String, Campaign>>,
}

impl InMemoryCampaigns {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl CampaignsRepository for InMemoryCampaigns {
    async fn create_campaign_at(
        &self,
        campaign: &Campaign,
        now: DateTime<Utc>,
    ) -> Result<bool, CampaignsError> {
        let mut rows = self.rows.lock().unwrap();
        if rows.contains_key(&campaign.id) {
            return Ok(false);
        }
        let mut stored = campaign.clone();
        stored.created_at = Some(now);
        rows.insert(campaign.id.clone(), stored);
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
