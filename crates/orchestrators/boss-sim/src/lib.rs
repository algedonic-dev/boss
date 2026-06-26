//! `boss-sim` — shape-driven simulation primitives.
//!
//! The crate ships the alphabet that every per-tenant engine
//! (`boss-brewery-engine`, `boss-used-device-shop-engine`)
//! composes: the day-loop runner + Periodic / Counterparty engines
//! (`engines`), the JobKind/Step/Subject day-loop body
//! (`shape_driven`), the SimOutput trait and its in-memory + live
//! HTTP adapters (`output`), the seeded PRNG (`rng`), the calendar
//! registry (`calendar`), and the cross-tenant sales-tax-rate table
//! (`tax_rates`).
//!
//! A tenant is described as data: a `tenant.toml`, a
//! `job_kinds.toml`, and a per-tenant engine crate.

/// Per-actor API-call tallies — the cockpit's "how the sim engages the
/// API, by who's acting" telemetry. Shared between the workforce + the
/// live output adapter.
pub mod api_activity;
pub mod calendar;
pub mod engines;
pub mod event_routes;
pub mod output;
pub mod rng;
pub mod scheduler;
pub mod shape_driven;
/// Jurisdictional sales-tax rate lookup. Used by the
/// `boss-ledger-batch` SalesTaxFilingRule. Pure helper data table.
pub mod tax_rates;
/// The sim-as-workforce executor: reads the live system (clock, open
/// assignments, real inventory) and works steps through the public API.
/// HTTP-only — it drives a live API stack.
#[cfg(feature = "http")]
pub mod workforce;
