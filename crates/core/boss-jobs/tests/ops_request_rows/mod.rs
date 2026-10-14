//! The two ops-request rows the row-E car is judged between (backlog
//! 6c9183de, finish ledger R2; design f623e425): the tree's row, which
//! reserves every key the host runner renders — and the execution of
//! `execute` — to the credential `runner:ops`, and the row it replaces,
//! which carries only the signer's half.
//!
//! ONE DEFINITION FOR BOTH TEST FILES (CLAUDE.md §9a). The signer's
//! tests (only_the_signer_writes_an_ops_request_decision.rs) are about
//! the signer half alone, and drive the row with the runner half taken
//! off; the runner's tests (the_ops_runner_is_the_only_writer_of_the_plan_a_human_signs.rs)
//! publish that same row as the road back. Two spellings of "the row
//! without the runner half" would be two rollbacks.
#![allow(dead_code)]

use boss_jobs::registry::{WorkflowSpec, seedable_platform_workflows};

/// The principal a runner credential resolves to — the door's own
/// constant, so this file cannot spell it differently.
pub const RUNNER: &str = boss_jobs::runner_credential::PRINCIPAL;

/// The keys the runner renders onto `approve` in one patch
/// (ops-runner.sh `render_plan`), the five onto `review`, and the seven
/// onto `nothing-to-do` (`close_nothing_to_do`).
pub const APPROVE_RUNNER_KEYS: [&str; 6] = [
    "plan",
    "verb",
    "host",
    "args",
    "rendered_plan_sha256",
    "review_run",
];
pub const REVIEW_RUNNER_KEYS: [&str; 5] = ["plan", "verb", "host", "args", "rendered_plan_sha256"];
pub const NOTHING_TO_DO_RUNNER_KEYS: [&str; 7] = [
    "plan",
    "verb",
    "host",
    "args",
    "rendered_plan_sha256",
    "plan_verb",
    "nothing_to_do",
];

/// The self-asserted guard the runner half replaces on `nothing-to-do`
/// (backlog aa816dd4): the account ops-runner.sh signs as.
pub const WRITTEN_BY: &str = "automation:ops-runner";

/// The tree's row: `infra/platform/workflows/ops-request.toml`, read by
/// the loader the seed and `boss workflow publish` both use.
pub fn declared_row() -> WorkflowSpec {
    seedable_platform_workflows()
        .into_iter()
        .find(|w| w.kind == "ops-request")
        .expect("the ops-request protocol is in the platform bundle")
}

/// The row with the runner half taken off: every `runner:ops` writer
/// and executor removed, and the `written_by` default it replaced put
/// back on `nothing-to-do` — the row the live registry carried before
/// this car (v10 to v12 declared the signer's two keys and nothing
/// else), and so the row David publishes to roll it back.
pub fn without_the_runner_half() -> WorkflowSpec {
    let mut spec = declared_row();
    for step in spec.steps.iter_mut() {
        for field in step.fields.iter_mut() {
            if field.writer.as_deref() == Some(RUNNER) {
                field.writer = None;
            }
        }
        if step.executor.as_deref() == Some(RUNNER) {
            step.executor = None;
        }
        if step.title == "nothing-to-do" {
            step.metadata_defaults[boss_jobs::written_by::KEY] =
                serde_json::Value::String(WRITTEN_BY.to_string());
        }
    }
    spec
}
