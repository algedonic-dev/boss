//! Boss accounts domain — customer-side CRM primitives.
//!
//! Carve-out from `boss-people`. Today `boss-people` owns the
//! employee-side roster (HR, org chart, certifications,
//! requisitions, PTO); this crate owns the account-side surface
//! that previously lived alongside it but was a different
//! business domain crammed into one crate:
//!
//! - **accounts** — the Account record + CRUD + delete-guard.
//! - **account_team_members** — territory rep + customer-success
//!   assignments per account.
//! - **account_notes** — free-text + structured notes.
//! - **account_next_actions** — derived "what's next on this
//!   account" rollup, surfaced on the account detail view.
//! - **account_risk_scores** — derived churn-risk + ticket /
//!   contract aging signals.
//! - **support_cases** — inbound account complaints + requests.
//!
//! Hexagonal: routers carry `(pool, publisher, assets_client)`
//! triples; the surface is HTTP-first since the read/write
//! operations are domain CRUD with no port-style trait — that
//! layer would be a different refactor.

#[cfg(feature = "postgres")]
pub mod account_next_actions;
#[cfg(feature = "postgres")]
pub mod account_notes;
#[cfg(feature = "postgres")]
pub mod account_risk_scores;
#[cfg(feature = "postgres")]
pub mod account_team_members;
#[cfg(feature = "postgres")]
pub mod accounts;
#[cfg(feature = "accounts-api")]
pub mod accounts_api_config;
#[cfg(feature = "postgres")]
pub mod rebuild;
#[cfg(feature = "postgres")]
pub mod support_cases;

pub mod events;

#[cfg(feature = "postgres")]
pub use rebuild::{RebuildError, RebuildReport, rebuild_accounts};
