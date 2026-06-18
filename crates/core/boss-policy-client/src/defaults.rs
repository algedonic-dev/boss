//! Default policy rules seeded at service startup.
//!
//! Core ships only the **platform-level** rules every BOSS deployment
//! needs to run:
//!
//! - `platform-admin` — the operator who owns the deployment. Broad
//!   Read across every shipped resource + Create/Update/Publish/
//!   Retire/Delete on the registry resources (`policy-rule`,
//!   `job-kind`, `step-plugin`) that govern how the platform behaves.
//! - `audit-readonly` — the external-auditor / OSS-anonymous-visitor
//!   role. Strictly Read on every shipped resource.
//! - `smoke-tester` — fixture role for the boss-testing harness.
//!   Read-only mirror of `audit-readonly`; isolated so a misconfigured
//!   test can't accidentally drift external-auditor expectations.
//! - `guest` — the unauth landing surface. Strictly
//!   `job-kind` Read; no other resource.
//!
//! Tenant role grants (sales-rep, service-tech, controllers, the
//! C-suite, department managers, …) live in **tenant seed data**, not
//! here. The 2026-05-24 tier-purity pass moved the prior ~365-line
//! used-device-shop org chart out of core — it was wrong on every
//! non-device-shop deployment (e.g. the brewery's role set never got
//! these grants), and it tied core's release cadence to one tenant's
//! HR model. Tenants seed their role matrix at first boot via the
//! `boss-policy-bootstrap` binary, which reads
//! `examples/<tenant>/seeds/policy_rules.toml` and POSTs each rule
//! to `/api/policy/rules`. See [`crate::seed_loader`] for the TOML
//! schema.
//!
//! Operators can edit any rule via the admin API and their changes
//! survive restarts: `bootstrap_reconcile` only refreshes rows whose
//! `updated_by = 'bootstrap'`. Operator-tuned rows are preserved.

use crate::types::{Action, PolicyRule as Rule, Resource, Scope};

/// The 13 resources the platform's `default_rules` enumerate Read
/// access over. Also consumed by `boss-policy::http::my_scope` to
/// know what set to evaluate the caller's scope against — modules
/// and tenants introduce their own via `Resource::new("specimen")`
/// and seed grants through the admin API, but the discovery
/// endpoint reports against this shipped floor.
pub fn shipped_resources() -> Vec<Resource> {
    vec![
        Resource::job(),
        Resource::step(),
        Resource::account(),
        Resource::employee(),
        Resource::invoice(),
        Resource::agreement(),
        Resource::asset(),
        Resource::shipment(),
        Resource::part(),
        Resource::purchase_order(),
        Resource::policy_rule(),
        Resource::job_kind(),
        Resource::step_plugin(),
    ]
}

pub fn default_rules() -> Vec<Rule> {
    use Action::*;
    let mut rules = Vec::new();
    let resources = shipped_resources();

    // ------------------------------------------------------------------
    // Platform admin — the operator running the BOSS deployment itself.
    // Broad **every-action** grant across every shipped resource. This
    // is the deploy-time superuser: it walks `job-kind-design` meta-
    // Jobs to register tenant JobKinds, runs `boss-policy-bootstrap`
    // to seed tenant role grants, runs `boss-brewery-data-seed` to
    // populate Subject rosters, and tunes any policy-rule after launch.
    //
    // Day-to-day business writes (a brewing batch's repair step, a
    // refurb-tech's job closure) still come from real employees with
    // their tenant roles. Those grants live in
    // `examples/<tenant>/seeds/policy_rules.toml`, not here.
    // ------------------------------------------------------------------
    for r in &resources {
        for action in [
            Read, Create, Update, Close, SignOff, Publish, Retire, Delete,
        ] {
            rules.push(Rule::new("platform-admin", r.clone(), action, Scope::All));
        }
    }

    // Step sign-off authority is enforced through policy against a
    // role-scoped `step-signoff:<role>` resource (see
    // `boss-jobs::http::update_step`). The platform JobKinds
    // (`job-kind-design`, `design-doc-review`) declare
    // `authority_role = "platform-admin"` on their approval steps, so
    // platform-admin needs SignOff on its own role-scoped resource —
    // the bare `step` grant above does NOT cover it, because the gate
    // authorizes against `step-signoff:platform-admin` when the step
    // carries that required role. Tenant roles get the equivalent
    // grant in `examples/<tenant>/seeds/policy_rules.toml`.
    rules.push(Rule::new(
        "platform-admin",
        Resource::new("step-signoff:platform-admin"),
        SignOff,
        Scope::All,
    ));

    // ------------------------------------------------------------------
    // Audit-readonly — external auditors / OSS anonymous visitors /
    // the seeded `emp-audit` login. Read on every shipped resource;
    // never Create/Update/Close/Publish/Retire/SignOff. The audit_log
    // itself + integrity-check checkpoints are accessed out-of-band
    // (boss-events tail-http + journal export), not through the policy
    // gate, so they don't appear here.
    // ------------------------------------------------------------------
    for r in &resources {
        rules.push(Rule::new("audit-readonly", r.clone(), Read, Scope::All));
    }

    // ------------------------------------------------------------------
    // Smoke-tester — fixture role for the boss-testing harness.
    // Reserved for `emp-smoke` (seeded by the schema). Mirrors
    // audit-readonly's rule matrix; isolated as a separate role so a
    // misconfigured smoke test can't accidentally drift production
    // external-auditor expectations.
    // ------------------------------------------------------------------
    for r in &resources {
        rules.push(Rule::new("smoke-tester", r.clone(), Read, Scope::All));
    }

    // ------------------------------------------------------------------
    // Guest — the unauth landing surface. The gateway forwards
    // `GET /api/jobs/kinds*` without a session; the
    // jobs-api then sees role `guest`, and this rule lets it answer.
    // Strictly read-only, strictly job-kind.
    // ------------------------------------------------------------------
    rules.push(Rule::new("guest", Resource::job_kind(), Read, Scope::All));

    rules
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_default_rule_has_unique_id() {
        let rules = default_rules();
        let mut ids = std::collections::HashSet::new();
        for r in &rules {
            assert!(
                ids.insert(r.id.clone()),
                "duplicate default rule id: {}",
                r.id
            );
        }
    }

    #[test]
    fn platform_admin_reads_every_resource() {
        let rules = default_rules();
        let reads: Vec<_> = rules
            .iter()
            .filter(|r| r.role == "platform-admin" && r.action == Action::Read)
            .collect();
        assert!(
            reads.len() >= 13,
            "platform-admin should have Read on every projection resource, got {}",
            reads.len()
        );
        for r in reads {
            assert_eq!(r.scope, Scope::All, "platform-admin reads are unrestricted");
        }
    }

    #[test]
    fn audit_readonly_only_has_read_grants() {
        let rules = default_rules();
        for r in rules.iter().filter(|r| r.role == "audit-readonly") {
            assert_eq!(
                r.action,
                Action::Read,
                "audit-readonly must never have non-Read actions; got {:?} on {:?}",
                r.action,
                r.resource
            );
        }
    }

    #[test]
    fn no_tenant_role_grants_in_core() {
        // The 2026-05-24 tier-purity pass moved the C-suite and the
        // department/IC role grants out of core. Pin that.
        let rules = default_rules();
        let banned = [
            "ceo",
            "coo",
            "cto",
            "cfo",
            "vp-sales",
            "sales-mgr",
            "sales-rep",
            "service-mgr",
            "service-tech",
            "refurb-supervisor",
            "refurb-tech",
            "qa-lead",
            "qa-tech",
            "warehouse-mgr",
            "warehouse-clerk",
            "parts-buyer",
            "controller",
            "ap-specialist",
            "hr-generalist",
            "recruiter",
            "support-specialist",
            "it-manager",
        ];
        for role in banned {
            let leak: Vec<_> = rules.iter().filter(|r| r.role == role).collect();
            assert!(
                leak.is_empty(),
                "tenant role `{role}` leaked into core defaults — move to tenant seed"
            );
        }
    }

    #[test]
    fn guest_only_reads_job_kinds() {
        let rules = default_rules();
        let guest: Vec<_> = rules.iter().filter(|r| r.role == "guest").collect();
        assert_eq!(guest.len(), 1);
        assert_eq!(guest[0].resource, Resource::job_kind());
        assert_eq!(guest[0].action, Action::Read);
    }
}
