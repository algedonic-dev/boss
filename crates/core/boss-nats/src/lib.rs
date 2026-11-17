//! NATS adapter for the Boss [`EventBus`] port.
//!
//! Events are published to a NATS subject equal to their `kind`
//! (e.g. `cybernetics.message.enqueued`). Subscription patterns use
//! native NATS wildcards (`*`, `>`) — the same semantics the in-memory
//! bus now honors, so application code is portable across adapters.
//!
//! Payload: JSON-encoded [`boss_core::event::Event`].

pub mod bus;
pub mod durable;
// A loopback NATS wire stub for the confirmed-publish tests (backlog
// 2e45840a); boss-events' relay test includes the same file by path.
#[cfg(test)]
mod nats_stub;

pub use bus::NatsEventBus;
