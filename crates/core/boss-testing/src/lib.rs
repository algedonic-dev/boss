//! Shared test utilities for Boss services.
//!
//! Provides:
//! - `TestRequest` builder for sending HTTP requests to an axum Router
//! - `TestResponse` wrapper with assertion helpers that produce
//!   useful error messages
//! - `RecordingEventBus` that captures published events for verification
//! - Custom assertion functions designed for agent-friendly failure messages
//! - `scratch_dir` for a fixture root this process and uid own outright
//! - `git_config_isolated`, the one way a test closes every channel git
//!   reads `safe.directory` from, so a host's `[safe] directory = *`
//!   cannot switch off an ownership refusal the test depends on
//! - `announce`, the one definition of a stub server stating its port
//!   to the test that spawned it, written whole or not at all
//! - `leaked_policy`, the AST pass behind `boss-leaked-policy`: it counts
//!   the code branches CLAUDE.md §9 names, which is the half of the §9
//!   measurement `infra/codebase-metrics.sh` could not count with a
//!   regex. It lives here because this crate already holds the tests
//!   that read the repository AS AN ARTEFACT (`gate_sh.rs`,
//!   `codebase_metrics_sh.rs`) rather than any service's behaviour.
//! - `adapters_agree!`, one body of assertions run against every adapter
//!   of a port, one test per (case, adapter) — see `adapter_suite`
//!   (design 3036296f, mechanism C)

pub mod adapter_suite;
pub mod announce;
pub mod assertions;
pub mod feed;
pub mod git;
pub mod kubectl_secret_stub;
pub mod leaked_policy;
pub mod ops_runner_stub;
pub mod production_source;
pub mod rbac;
pub mod recording_bus;
pub mod request;
pub mod scratch;
#[cfg(feature = "postgres")]
pub mod test_db;
pub mod tree;

pub use assertions::*;
pub use feed::feed_stdin;
pub use git::git_config_isolated;
pub use recording_bus::RecordingEventBus;
pub use request::{TestRequest, TestResponse};
pub use scratch::{copy_exec, create_dir, scratch_dir, scratch_path, write_exec, write_file};
#[cfg(feature = "postgres")]
pub use test_db::TestDb;
pub use tree::{
    LINT_LIB_DIRS, copy_gate_sh, copy_lint_libs, dispatcher_rules_dir, repo_root, tree_match,
    tunnel_ingress_summary,
};
