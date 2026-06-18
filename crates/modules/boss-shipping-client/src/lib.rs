//! HTTP client port for reaching the `boss-shipping` service.
//!
//! Currently exposes one question: "what's the current state of
//! outbound shipments?" for the warehouse-status projection
//! (operations-needs session 3, E1). The shape matches the wire
//! response from `/api/shipping/shipments/status-summary`.

use async_trait::async_trait;
use boss_core::http_client::{self, HttpClientError, ServiceLabel};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

/// Service-name marker for the shared [`HttpClientError`]. Keeps the
/// `Display` text reading `"shipping service unreachable: …"`.
#[derive(Debug)]
pub struct Shipping;
impl ServiceLabel for Shipping {
    const NAME: &'static str = "shipping";
}

/// Transport error for the Shipping client. Alias of the shared
/// [`HttpClientError`] so existing constructors and matches keep
/// compiling.
pub type ShippingClientError = HttpClientError<Shipping>;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OutboundShipmentSummary {
    pub label_created: i64,
    pub picked_up: i64,
    pub in_transit: i64,
    pub exception: i64,
    /// Shipments delivered within the last 7 days — context for the
    /// "what went out this week" view.
    pub delivered_7d: i64,
    pub recent: Vec<OutboundShipmentRow>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutboundShipmentRow {
    pub id: String,
    pub status: String,
    pub carrier: String,
    pub destination: String,
    pub account_id: Option<String>,
    pub shipped_on: Option<NaiveDate>,
    pub estimated_delivery: Option<NaiveDate>,
    pub asset_id_count: usize,
}

#[async_trait]
pub trait ShippingClient: Send + Sync {
    /// Aggregate status view of outbound shipments currently in flight
    /// plus a small top-N of recent-delivery context.
    async fn outbound_shipment_summary(
        &self,
    ) -> Result<OutboundShipmentSummary, ShippingClientError>;
}

pub struct ReqwestShippingClient {
    base_url: String,
    http: reqwest::Client,
}

impl ReqwestShippingClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        let (base_url, http) = http_client::base(base_url);
        Self { base_url, http }
    }
}

#[async_trait]
impl ShippingClient for ReqwestShippingClient {
    async fn outbound_shipment_summary(
        &self,
    ) -> Result<OutboundShipmentSummary, ShippingClientError> {
        let url = format!(
            "{}/api/shipping/shipments/status-summary?direction=outbound",
            self.base_url
        );
        http_client::get_json(&self.http, &url).await
    }
}
