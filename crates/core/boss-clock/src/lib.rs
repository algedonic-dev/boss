//! `boss-clock` — authoritative clock service.
//!
//! ## Design
//!
//! Every BOSS service needs to know "what time is it" — and a
//! simulator or back-test needs that answer to read a value other than
//! wallclock. The decision of whether time is sim or wall belongs in
//! one place, not scattered across every handler that calls
//! `Utc::now()` or inspects a request header.
//!
//! So: a single clock service. Services hold a `ClockClient` and call
//! `clock.now()` whenever they need the current time. They never
//! decide whether time is sim or wall; the clock service decides, per
//! deployment. Two modes:
//!
//! - **wall** — every `/api/clock/now` returns `Utc::now()` with
//!   `simulated: false`. Production deploys run this.
//! - **sim** — `/api/clock/now` returns `sim_clock.current_sim_date`
//!   (at midnight UTC, or an explicit instant when the sim
//!   driver uses sub-day ticks) with `simulated: true`. Demo
//!   deploys + back-tests run this. The sim engine advances
//!   the clock via `POST /api/clock/advance`.
//!
//! ## Why a service, not a library
//!
//! Sim and back-testing require *coordinated* time across many
//! processes: brewery-sim, the inventory api, the ledger api,
//! the SPA. A library-level Clock trait can't coordinate
//! across processes — each binary holds its own instance.
//! Centralizing the clock in one HTTP endpoint lets every reader
//! see the same answer. The `ClockClient` adds a short TTL cache
//! so the network hop costs sub-millisecond on warm cache.
//!
//! ## SIM marker
//!
//! `ClockNow` carries `simulated: bool`. Services include it in
//! every event payload they emit, so the audit log preserves the
//! sim-vs-real distinction forever. Future queries can filter
//! (`payload->>'simulated' = 'true'`) to scope a report to real
//! activity or sim activity alone.

#![forbid(unsafe_code)]

pub mod http;
pub mod types;

#[cfg(feature = "postgres")]
pub mod postgres;
