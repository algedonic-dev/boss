//! boss-dispatcher: the queue watcher — clock, threshold and rule
//! reactions to the record.
//!
//! ## What it does
//!
//! - The rules runner (`rules`) reacts to `step.done.<kind>` and the
//!   other topics the `dispatcher_rules` registry names, running each
//!   rule's side effect through its handler.
//! - The schedule runner fires the clock-driven rules.
//! - `/api/dispatcher/readyz` (`liveness`, `http`) says whether those
//!   consumers are bound and draining.
//!
//! ## What it does NOT do: name who works a step
//!
//! It did, until backlog e1eaf72b (2026-11-11). A `dispatcher-steps`
//! consumer on `jobs.step.>` nominated every ready step to one executor
//! — a capability pick, an environment executor (`BOSS_DISPATCH_EXECUTOR_ID`)
//! and a role hash (`BOSS_DISPATCH_STRATEGY`) — so ~700 ready
//! backlog-item steps stood at a name, and the role hash, which read no
//! model, named agent-codex on 125 build steps `boss dispatch --next`
//! then refused it. David, 2026-11-11: "Work is supposed to sit at the
//! admission queue of a station now, not at a name or robot or
//! executor." So a ready step now stands at its station unassigned, and
//! an executor pulls it (`boss dispatch --next`), filtered by role and
//! model class at the claim door. Pinned by
//! `the_dispatcher_nominates_nobody` in boss-testing.
//!
//! - It does not drive Steps to completion; an executor does.
//! - It does not gate policy; policy checks fire on each write.

pub mod cascade;
pub mod config;
pub mod http;
pub mod liveness;
pub(crate) mod next_hop;
pub mod rules;
