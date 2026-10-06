//! The facts a customer write leaves on the log, built ONCE for both
//! adapters. Until the adapters-agree suite (backlog be459ab9) only the
//! Postgres adapter built the birth fact, inline, and the double recorded
//! none — so no test answered by the double could see what a create puts
//! on the log.

use boss_core::event::Event;
use chrono::{DateTime, Utc};

use crate::types::Customer;

/// A customer record entered the company (declared in
/// `108-event-kinds.sql`).
pub const CUSTOMER_CREATED: &str = "customers.customer.created";

/// The birth fact for `customer`, stamped `now`. email/phone ride the
/// payload: the log is the system of record, and a column the log doesn't
/// carry is a column every rebuild silently loses (live-vs-rebuilt
/// divergence). This adds no exposure the system doesn't already have —
/// /shop writes customer_email into Job metadata, which flows through
/// jobs.job.created events today.
pub fn customer_created(customer: &Customer, now: DateTime<Utc>) -> Event {
    let payload = serde_json::json!({
        "id": customer.id,
        "name": customer.name,
        "email": customer.email,
        "phone": customer.phone,
        "metadata": customer.metadata,
    });
    Event::new("boss-customers", CUSTOMER_CREATED, payload, now)
}
