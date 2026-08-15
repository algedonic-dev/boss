//! Cadence port — the four operations the train conductor needs from
//! the registry, and nothing else.
//!
//! No outbox events here, deliberately. `cadence_firings` IS the
//! measurement record: every firing is already a row with its window,
//! its basis, and (after the verb) its exit code and runtime. Adding a
//! parallel event stream would duplicate the fact without adding a
//! queryable one.

use async_trait::async_trait;

use super::types::{CadenceRuleRow, LastFiring, NewFiring};

#[derive(Debug, thiserror::Error)]
pub enum CadenceError {
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("storage: {0}")]
    Storage(String),
}

#[async_trait]
pub trait CadenceRepository: Send + Sync {
    /// Active rules, name-ordered. Rows are returned unparsed — see
    /// `types::CadenceRuleRow` for why the parse stays in the caller.
    async fn active_rules(&self) -> Result<Vec<CadenceRuleRow>, CadenceError>;

    /// The newest firing of `rule`, or `None` if it has never fired.
    async fn last_firing(&self, rule: &str) -> Result<Option<LastFiring>, CadenceError>;

    /// Claim a firing id. `Ok(true)` means this caller won the window
    /// and must run the verb; `Ok(false)` means someone else already
    /// holds it. Exactly-once rests on the `firing_id` primary key, so
    /// two conductors racing the same window cannot both win.
    async fn claim_firing(&self, new: &NewFiring) -> Result<bool, CadenceError>;

    /// Merge the verb's exit code and runtime into the firing's
    /// `detail`. Merging (not replacing) preserves whatever the claim
    /// recorded — e.g. the dock depth that triggered a queue-depth
    /// rule.
    async fn record_outcome(
        &self,
        firing_id: &str,
        rc: i32,
        runtime_secs: u64,
    ) -> Result<(), CadenceError>;
}
