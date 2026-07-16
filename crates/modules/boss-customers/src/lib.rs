//! boss-customers — the domain home for the `customer` Subject kind.
//!
//! Q4 of docs/design/subject-identity-and-relationships.md (approved
//! 2026-07-15), second half. The audit found `customer` registered
//! but fully inert: `/shop` checkouts stuffed the buyer's
//! email/name/phone into Job metadata on the catch-all
//! `acc-direct-shop` account. This crate is the durable home for the
//! person — hexagonal shape, mirroring boss-campaigns.
//!
//! Create = ONE transaction: domain row + `subjects` identity row
//! (Q1 write-through) + `customers.customer.created` outbox event
//! (#118). Id convention (R3, one mint authority per kind): a create
//! without an id derives `cust-<sha256(email)[..12]>` — same buyer,
//! same row, idempotent re-checkout, no PII in the id.

pub mod http;
pub mod in_memory;
pub mod port;
pub mod types;

#[cfg(feature = "postgres")]
pub mod postgres;
#[cfg(feature = "postgres")]
pub mod rebuild;

pub use in_memory::InMemoryCustomers;
#[cfg(feature = "postgres")]
pub use postgres::PgCustomers;
