//! Domain event subjects for shipping operations.
//!
//! Per `docs/design/projection-rebuilders.md`: state events carry
//! the full `Shipment` row state (including the `asset_ids` list
//! that drives `shipment_assets`) so the rebuild path can
//! reconstruct both projections from the event log alone.
//!
//! - `created` / `updated` — full Shipment payload.
//! - `deleted` — `{id, deleted_at}`.

pub const SHIPMENT_CREATED: &str = "shipping.shipment.created";
pub const SHIPMENT_UPDATED: &str = "shipping.shipment.updated";
pub const SHIPMENT_DELETED: &str = "shipping.shipment.deleted";
/// Carrier scan recorded against a shipment. Carries
/// `{shipment_id, status, occurred_on, stage_index}`. The
/// rebuilder upserts a `shipment_tracking_events` row + rolls
/// up the shipment's status column when the scan moves it to
/// in-transit / delivered.
pub const TRACKING_RECORDED: &str = "shipping.tracking.recorded";
