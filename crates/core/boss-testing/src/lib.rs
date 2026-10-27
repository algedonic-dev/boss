//! Shared test utilities for Boss services.
//!
//! Provides:
//! - `TestRequest` builder for sending HTTP requests to an axum Router
//! - `TestResponse` wrapper with assertion helpers that produce
//!   useful error messages
//! - `RecordingEventBus` that captures published events for verification
//! - Custom assertion functions designed for agent-friendly failure messages
//! - `scratch_dir` for a fixture root this process and uid own outright
//! - `dark_port`, a loopback port that refuses every connect and that no
//!   other test can be handed while it is held
//! - `git_config_isolated`, the one way a test closes every channel git
//!   reads `safe.directory` from, so a host's `[safe] directory = *`
//!   cannot switch off an ownership refusal the test depends on
//! - `without_proxy`, the one way a test keeps the proxy variables of
//!   the shell that ran the suite off a child, so the machine-token host
//!   rule's proxy withholding cannot turn a stamped row red (50708d76)
//! - `announce`, the one definition of a stub server stating its port
//!   to the test that spawned it, written whole or not at all
//! - `leaked_policy`, the AST pass behind `boss-leaked-policy`: it counts
//!   the code branches CLAUDE.md §9 names, which is the half of the §9
//!   measurement `infra/codebase-metrics.sh` could not count with a
//!   regex. It lives here because this crate already holds the tests
//!   that read the repository AS AN ARTEFACT (`gate_sh.rs`,
//!   `codebase_metrics_sh.rs`) rather than any service's behaviour.
//! - `reexec`, the one way a test re-runs its own binary as a child and
//!   bounds only the child's work, never its start (backlog 7e30ddb0)
//! - `adapters_agree!`, one body of assertions run against every adapter
//!   of a port, one test per (case, adapter) — see `adapter_suite`
//!   (design 3036296f, mechanism C)

pub mod adapter_suite;
pub mod announce;
pub mod assertions;
pub mod dark_port;
pub mod feed;
pub mod git;
pub mod kubectl_secret_stub;
pub mod leaked_policy;
pub mod ops_runner_stub;
pub mod passkey;
pub mod production_source;
pub mod proxy;
pub mod rbac;
pub mod recording_bus;
pub mod reexec;
pub mod request;
pub mod scratch;
#[cfg(feature = "postgres")]
pub mod test_db;
pub mod test_values;
pub mod tree;

pub use assertions::*;
pub use dark_port::{DarkPort, dark_port};
pub use feed::feed_stdin;
pub use git::git_config_isolated;
pub use proxy::{PROXY_VARS, without_proxy};
pub use recording_bus::RecordingEventBus;
pub use request::{TestRequest, TestResponse};
pub use scratch::{copy_exec, create_dir, scratch_dir, scratch_path, write_exec, write_file};
#[cfg(feature = "postgres")]
pub use test_db::TestDb;
pub use tree::{
    LINT_LIB_DIRS, TREE_WIDE_FILE_PATTERN, TREE_WIDE_TEST_PATTERN, copy_gate_sh, copy_lint_libs,
    dispatcher_rules_dir, repo_root, tree_match, tree_wide_pattern, tunnel_ingress_summary,
};
