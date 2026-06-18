//! People domain — Boss employees, org chart, certifications.
//!
//! Every piece of work inside the company is ultimately done by someone,
//! so this crate is referenced by many others: refurb jobs cite the tech,
//! service tickets name the assignee, sales opportunities have an owner.

pub mod assets_client;
#[cfg(feature = "postgres")]
pub mod employee_changes;
pub mod http;
pub mod in_memory;
pub mod people_config;
pub mod port;
#[cfg(feature = "postgres")]
pub mod postgres;
pub mod pto;
#[cfg(feature = "postgres")]
pub mod rebuild;
#[cfg(feature = "postgres")]
pub mod requisitions;
#[cfg(feature = "postgres")]
pub mod scope;
#[cfg(feature = "postgres")]
pub mod search;
pub mod types;
#[cfg(feature = "postgres")]
pub mod workflows;

pub use in_memory::InMemoryPeople;
pub use port::{PeopleError, PeopleRepository};
#[cfg(feature = "postgres")]
pub use postgres::PgPeople;
#[cfg(feature = "postgres")]
pub use rebuild::{RebuildError, RebuildReport, rebuild_people};
pub use types::*;
pub mod events;
