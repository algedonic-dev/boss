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

pub use bus::NatsEventBus;
