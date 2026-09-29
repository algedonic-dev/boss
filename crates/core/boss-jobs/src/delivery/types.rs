//! Wire types for the delivery-policy surface.
//!
//! `DeliveryPolicyRow` is deliberately the RAW registry row, exactly as
//! `cadence::types::CadenceRuleRow` is, and for the same reason: the
//! conductor owns the parse because it owns the consequence. A row with
//! a nonsense budget is skipped loudly in the conductor's journal and
//! the compiled fallback carries the train; an API that parsed and
//! 500'd would turn one bad registry row into a pipeline that cannot
//! board at all — a policy registry must not become a new way to wedge
//! every train.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::registry::WorkflowStatus;

/// One row of `delivery_policy`, unparsed.
///
/// ONE ROW IS THE WHOLE POLICY — the `workflows` shape, not the
/// `cadence_rules` shape. A Job pins one workflow version; a train pins
/// one policy version (`metadata.delivery_policy_version`), which is
/// only expressible if the version numbers one bundle rather than one
/// setting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryPolicyRow {
    pub name: String,
    pub version: i32,
    /// Strikes before boarding holds a car back.
    pub max_red_trains: i32,
    /// Hours without a step completion before an open train counts stalled.
    pub stall_hours: i32,
    /// Wall clock the whole consist check may spend.
    pub consist_budget_secs: i32,
    /// Characters of a failing lint's output that go on the record.
    pub consist_output_budget: i32,
    /// How many filenames a consist refusal names.
    pub consist_files_named: i32,
    /// Characters of conflicted-file list on a left-behind car's chip.
    pub skip_reason_file_budget: i32,
    /// Characters of a jobs-API blip's cause in the journal.
    pub blip_cause_budget: i32,
    /// GB of free disk the CI host's latest host-scope estate
    /// observation must show before boarding assembles a consist.
    pub ci_host_floor_gb: i32,
    /// How many gates `boss gate` admits at once before it refuses.
    /// Read by the gate CLI (the number it enforces) AND by the yard
    /// status surface (the number it draws as slots) — one source, so
    /// the capacity a page shows is the capacity the pipeline obeys.
    pub gate_max_concurrent: i32,
}

impl DeliveryPolicyRow {
    /// Every budget, named as its column is — the nine values the table
    /// CHECKs above 0.
    pub fn budgets(&self) -> [(&'static str, i32); 9] {
        [
            ("max_red_trains", self.max_red_trains),
            ("stall_hours", self.stall_hours),
            ("consist_budget_secs", self.consist_budget_secs),
            ("consist_output_budget", self.consist_output_budget),
            ("consist_files_named", self.consist_files_named),
            ("skip_reason_file_budget", self.skip_reason_file_budget),
            ("blip_cause_budget", self.blip_cause_budget),
            ("ci_host_floor_gb", self.ci_host_floor_gb),
            ("gate_max_concurrent", self.gate_max_concurrent),
        ]
    }
}

/// Would `delivery_policy` admit this row? The table's CHECKs — every
/// budget above 0 — as one Rust check, so a declaration is refused the
/// same way over either adapter.
///
/// WHY IT EXISTS (backlog be459ab9, found by the adapters-agree suite,
/// 2026-09-29). Postgres enforced the CHECKs and `InMemoryDeliveryPolicy`
/// enforced none, so a declaration the database refused as a storage
/// error naming a constraint landed in the double, retired the policy in
/// force, and was served to the conductor. Both adapters now run this
/// before writing and answer `DeliveryPolicyError::BadRequest` naming
/// the policy and the column. `the_policy_check_is_the_tables_check` (in
/// `the_adapters_agree_on_the_delivery_policy_registry_pg.rs`) holds it
/// to the live table BOTH ways. This is the registry's judgement only:
/// the conductor still parses what it reads (the module's `types` doc).
pub fn check_policy(row: &DeliveryPolicyRow) -> Result<(), String> {
    match row.budgets().into_iter().find(|(_, v)| *v <= 0) {
        Some((column, v)) => Err(format!(
            "delivery policy {}@{}: {column} must be above 0, got {v}",
            row.name, row.version
        )),
        None => Ok(()),
    }
}

/// One row of `delivery_policy` as DECLARED — the wire row (which
/// already carries `name` and `version`) plus the column the conductor
/// never reads (`status`) and the one the seed stamps (`created_at`).
/// This is what the platform bundle
/// (`infra/platform/delivery-policy/<name>.toml`) declares and what
/// `DeliveryPolicyRegistry::live_versions` reads back, so the equality
/// pin compares the same shape on both sides. The columns live ONCE,
/// in [`DeliveryPolicyRow`], flattened here rather than repeated
/// (CLAUDE.md §9a): a budget added to the row is added to the
/// declaration by construction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryPolicySpec {
    pub status: WorkflowStatus,
    #[serde(flatten)]
    pub row: DeliveryPolicyRow,
    /// When the deployment was built — stamped by the seed's clock on
    /// the row it writes; never part of the declaration.
    pub created_at: DateTime<Utc>,
}

impl DeliveryPolicySpec {
    pub fn name(&self) -> &str {
        &self.row.name
    }

    pub fn version(&self) -> i32 {
        self.row.version
    }
}
