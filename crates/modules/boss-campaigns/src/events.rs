//! The facts a campaign write leaves on the log, built ONCE for both
//! adapters. Until the adapters-agree suite (backlog be459ab9) only the
//! Postgres adapter built the birth fact, inline, and the double recorded
//! none — so no test answered by the double could see what a create puts
//! on the log.

use boss_core::event::Event;
use chrono::{DateTime, Utc};

use crate::types::Campaign;

/// A marketing campaign opened (declared in `108-event-kinds.sql`).
pub const CAMPAIGN_CREATED: &str = "campaigns.campaign.created";

/// The birth fact for `campaign`, stamped `now`. It carries every
/// column but `created_at` (the event's own timestamp), because the
/// rebuilder (`rebuild.rs`) reproduces the row from this payload alone.
pub fn campaign_created(campaign: &Campaign, now: DateTime<Utc>) -> Event {
    let payload = serde_json::json!({
        "id": campaign.id,
        "name": campaign.name,
        "status": campaign.status,
        "starts_on": campaign.starts_on,
        "ends_on": campaign.ends_on,
        "metadata": campaign.metadata,
    });
    Event::new("boss-campaigns", CAMPAIGN_CREATED, payload, now)
}
