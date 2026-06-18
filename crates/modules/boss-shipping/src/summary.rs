//! Shared `summarise_shipments` — takes a slice of Shipments and a
//! reference date, produces the `OutboundShipmentSummary` wire shape.
//!
//! Called directly by the in-memory adapter (small datasets) and by
//! the Postgres test harness. The Postgres prod path implements the
//! same contract via SQL aggregation instead, so full-table scans
//! don't trip the cross-service client timeout at scale.

use chrono::Duration;

use crate::types::{Shipment, ShipmentDirection, ShipmentStatus};

pub const STATUS_SUMMARY_RECENT_LIMIT: usize = 10;

pub fn summarise_shipments(
    all: &[Shipment],
    direction: ShipmentDirection,
    today: chrono::NaiveDate,
) -> boss_shipping_client::OutboundShipmentSummary {
    let filtered: Vec<&Shipment> = all.iter().filter(|s| s.direction == direction).collect();

    let mut label_created = 0i64;
    let mut picked_up = 0i64;
    let mut in_transit = 0i64;
    let mut exception = 0i64;
    let mut delivered_7d = 0i64;
    let week_ago = today - Duration::days(7);

    for s in &filtered {
        match s.status.as_str() {
            ShipmentStatus::LABEL_CREATED => label_created += 1,
            ShipmentStatus::PICKED_UP => picked_up += 1,
            ShipmentStatus::IN_TRANSIT => in_transit += 1,
            ShipmentStatus::EXCEPTION => exception += 1,
            ShipmentStatus::DELIVERED if s.delivered_on.is_some_and(|d| d >= week_ago) => {
                delivered_7d += 1;
            }
            // Delivered-but-stale rows, and any tenant-defined status
            // beyond the platform five, carry no summary bucket — they
            // fall through uncounted rather than panicking the open
            // newtype on an unrecognised value.
            _ => {}
        }
    }

    // In-flight first (any status but Delivered), sorted by most-recent
    // shipped_on; delivered rows get appended after so the preview has
    // context for "just delivered" if there's room.
    let mut in_flight: Vec<&Shipment> = filtered
        .iter()
        .copied()
        .filter(|s| !s.status.is_delivered())
        .collect();
    in_flight.sort_by_key(|s| std::cmp::Reverse(s.shipped_on.unwrap_or(s.created_on)));
    let mut delivered: Vec<&Shipment> = filtered
        .iter()
        .copied()
        .filter(|s| s.status.is_delivered())
        .collect();
    delivered.sort_by_key(|s| std::cmp::Reverse(s.delivered_on.unwrap_or(s.created_on)));

    let recent: Vec<boss_shipping_client::OutboundShipmentRow> = in_flight
        .iter()
        .chain(delivered.iter())
        .take(STATUS_SUMMARY_RECENT_LIMIT)
        .map(|s| boss_shipping_client::OutboundShipmentRow {
            id: s.id.clone(),
            status: s.status.as_str().to_string(),
            carrier: s
                .carrier
                .as_ref()
                .map(|c| c.as_str().to_string())
                .unwrap_or_default(),
            destination: s.destination.clone(),
            account_id: s.account_id.clone(),
            shipped_on: s.shipped_on,
            estimated_delivery: s.estimated_delivery,
            asset_id_count: s.asset_ids.len(),
        })
        .collect();

    boss_shipping_client::OutboundShipmentSummary {
        label_created,
        picked_up,
        in_transit,
        exception,
        delivered_7d,
        recent,
    }
}
