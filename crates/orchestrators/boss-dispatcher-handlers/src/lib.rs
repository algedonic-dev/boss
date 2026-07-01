//! Side-effect handlers for the boss-dispatcher engine.
//!
//! The core `boss-dispatcher` crate owns the generic, rule-driven engine
//! (the [`Handler`](boss_dispatcher::rules::handler::Handler) trait, the
//! rule registry, the durable NATS consumers, topic routing). This crate
//! owns the **handlers** — the actual side effects a step transition
//! produces — plus the binary that wires engine + handlers together.
//!
//! Handlers are generic mechanisms parameterized by data: they read step
//! metadata + live state and call the domain HTTP APIs. They do NOT import
//! the module crates, so tenant behavior stays in seed data and this
//! library stays decoupled. New handlers land here as the model grows —
//! core never changes.
pub mod handlers;
