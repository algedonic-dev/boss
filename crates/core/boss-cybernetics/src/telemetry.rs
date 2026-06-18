//! S3 telemetry event kinds emitted by the Cybernetics loop.
//!
//! Every step of the dispatch lifecycle produces an event on the bus.
//! The observability dashboard subscribes to `cybernetics.>` to render
//! live state. Keep payload shapes stable — they are a public contract.

use boss_core::event::Event;
use chrono::{DateTime, Utc};

pub const MESSAGE_ENQUEUED: &str = "cybernetics.message.enqueued";
pub const MESSAGE_REJECTED: &str = "cybernetics.message.rejected";
pub const DISPATCH_REQUESTED: &str = "cybernetics.dispatch.requested";
pub const DISPATCH_STARTED: &str = "cybernetics.dispatch.started";
pub const DISPATCH_DENIED: &str = "cybernetics.dispatch.denied";
pub const DISPATCH_SKIPPED: &str = "cybernetics.dispatch.skipped";
pub const DISPATCH_COMPLETED: &str = "cybernetics.dispatch.completed";
pub const COST_RECORDED: &str = "cybernetics.cost.recorded";

/// Build an event with the Cybernetics source prefix for a given
/// VM. `timestamp` must come from the authoritative clock
/// (`state.clock.now().await.now`) — the runtime sources it from
/// the ClockClient handed to `Cybernetics::new`.
pub fn event(
    vm: &str,
    kind: &'static str,
    payload: serde_json::Value,
    timestamp: DateTime<Utc>,
) -> Event {
    Event::new(format!("cybernetics/{vm}"), kind, payload, timestamp)
}
