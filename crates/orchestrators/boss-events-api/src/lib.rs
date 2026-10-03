//! Apps adapter for the existing events read service.
//! The pure gate-window judgment lives in boss-core; the durable
//! reader and existing audit/outbox routers remain in boss-events.

#[cfg(feature = "postgres")]
pub mod gate_window_http;
