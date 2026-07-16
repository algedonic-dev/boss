//! boss-campaigns — the domain home for the `campaign` Subject kind.
//!
//! Q4 of docs/design/subject-identity-and-relationships.md (approved
//! 2026-07-15): campaign graduates from identity-rows-only to a real
//! domain crate, hexagonal shape — domain types + port + HTTP surface
//! + rebuilder.
//!
//! The create path is the crate's whole reason to exist done right:
//! one transaction inserts the domain row, the `subjects` identity
//! row (Q1 write-through), and the `campaigns.campaign.created`
//! outbox event (the #118 transactional-outbox pattern — this is the
//! first domain crate born onto it, closing the gap where campaign
//! births minted identity with no audit event and were reproducible
//! only via the jobs.job.created pass).

pub mod http;
pub mod in_memory;
pub mod port;
pub mod types;

#[cfg(feature = "postgres")]
pub mod postgres;
#[cfg(feature = "postgres")]
pub mod rebuild;

pub use in_memory::InMemoryCampaigns;
#[cfg(feature = "postgres")]
pub use postgres::PgCampaigns;
