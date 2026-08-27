//! Actor-coverage telemetry — is the sim driving the WHOLE roster?
//!
//! The workforce only drives steps whose `authority_role` routes to a
//! role with holders; a role that appears in no live workflow step
//! never acts, and the employees holding it never complete anything.
//! Measured 2026-08-20: 14 of 43 roles ever acted; 259 of 411
//! employees (63%) had never completed a step — and nothing on the
//! simulator's face said so. This module makes that visible: a pure
//! function of (roster, per-actor completion tally, operator-exclusion
//! set) → per-role coverage + headline totals, served in the daemon's
//! `/telemetry` and rendered by the simulator SPA.
//!
//! Operator identities (role `platform-admin`, the bootstrap admin) are
//! excluded from sim driving BY DESIGN — the sim must never act as a
//! real login. A role whose every holder is operator-excluded is
//! reported as [`RoleStatus::Operator`] ("operator (not simulated)"),
//! never as dormant, and its holders stay out of the dormant math.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::Serialize;

/// Role actors the roster map doesn't know are attributed here —
/// consistent with the workforce's display-attribution fallback.
pub const UNROSTERED_ROLE: &str = "unassigned-role";

/// Coverage verdict for one role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RoleStatus {
    /// At least one holder completed a step this daemon lifetime.
    Acting,
    /// Simulatable holders exist but none has ever completed a step —
    /// the under-simulation signal this telemetry exists to surface.
    Dormant,
    /// Every holder is operator-excluded: not simulated by design
    /// (render as "operator (not simulated)", not as a gap).
    Operator,
}

/// One role's coverage row.
#[derive(Debug, Clone, Serialize)]
pub struct RoleCoverage {
    pub role: String,
    /// Employees holding this role on the roster.
    pub roster: u64,
    /// Of `roster`, how many are operator-excluded (not simulatable).
    pub operator_holders: u64,
    /// Distinct holders that completed ≥ 1 step this daemon lifetime.
    pub acting: u64,
    /// Steps completed by this role this daemon lifetime.
    pub completions: u64,
    pub status: RoleStatus,
}

/// The `actor_coverage` telemetry block: per-role rows (sorted by role
/// name — deterministic between polls) + headline totals. Dormant
/// totals never count operator-excluded roles/employees.
#[derive(Debug, Default, Clone, Serialize)]
pub struct ActorCoverage {
    pub roles_total: u64,
    pub roles_acting: u64,
    pub roles_dormant: u64,
    pub roles_operator: u64,
    pub employees_total: u64,
    pub employees_acting: u64,
    pub employees_operator: u64,
    pub roles: Vec<RoleCoverage>,
}

/// Compute the coverage block. Pure: `emp_roles` is the roster
/// (employee id → role), `completions_by_actor` the per-employee
/// completed-step tally (this daemon lifetime), `excluded` the
/// operator identities the sim never acts as. A completion by an actor
/// the roster doesn't know lands under [`UNROSTERED_ROLE`] and still
/// counts toward the employee totals, so acting can never exceed total.
pub fn compute(
    emp_roles: &HashMap<String, String>,
    completions_by_actor: &HashMap<String, u64>,
    excluded: &HashSet<String>,
) -> ActorCoverage {
    #[derive(Default)]
    struct Acc {
        roster: u64,
        operator_holders: u64,
        acting: u64,
        completions: u64,
    }

    // Fold the roster per role, marking operator-excluded holders and
    // crediting each holder's completions.
    let mut by_role: BTreeMap<&str, Acc> = BTreeMap::new();
    for (emp, role) in emp_roles {
        let acc = by_role.entry(role.as_str()).or_default();
        acc.roster += 1;
        if excluded.contains(emp) {
            acc.operator_holders += 1;
        }
        let n = completions_by_actor.get(emp).copied().unwrap_or(0);
        if n > 0 {
            acc.acting += 1;
            acc.completions += n;
        }
    }
    // Completions by actors the roster map doesn't know: attribute to
    // the fallback role so the work stays visible (and conserved).
    for (emp, &n) in completions_by_actor {
        if n > 0 && !emp_roles.contains_key(emp) {
            let acc = by_role.entry(UNROSTERED_ROLE).or_default();
            acc.acting += 1;
            acc.completions += n;
        }
    }

    let roles: Vec<RoleCoverage> = by_role
        .into_iter()
        .map(|(role, acc)| {
            let status = if acc.acting > 0 {
                RoleStatus::Acting
            } else if acc.roster > 0 && acc.operator_holders == acc.roster {
                RoleStatus::Operator
            } else {
                RoleStatus::Dormant
            };
            RoleCoverage {
                role: role.to_string(),
                roster: acc.roster,
                operator_holders: acc.operator_holders,
                acting: acc.acting,
                completions: acc.completions,
                status,
            }
        })
        .collect();

    let count_status = |s: RoleStatus| roles.iter().filter(|r| r.status == s).count() as u64;
    let unrostered_acting = completions_by_actor
        .iter()
        .filter(|&(emp, &n)| n > 0 && !emp_roles.contains_key(emp))
        .count() as u64;
    ActorCoverage {
        roles_total: roles.len() as u64,
        roles_acting: count_status(RoleStatus::Acting),
        roles_dormant: count_status(RoleStatus::Dormant),
        roles_operator: count_status(RoleStatus::Operator),
        employees_total: emp_roles.len() as u64 + unrostered_acting,
        employees_acting: completions_by_actor.values().filter(|&&n| n > 0).count() as u64,
        employees_operator: emp_roles.keys().filter(|e| excluded.contains(*e)).count() as u64,
        roles,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roster(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(e, r)| (e.to_string(), r.to_string()))
            .collect()
    }

    fn tally(pairs: &[(&str, u64)]) -> HashMap<String, u64> {
        pairs.iter().map(|(e, n)| (e.to_string(), *n)).collect()
    }

    fn excluded(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn dormant_is_roster_minus_acting() {
        // Two roles, one acts: brewer emp-1 completed steps; the whole
        // shipping-clerk roster never did. The clerk role must read
        // DORMANT on its face — this is the panel's reason to exist.
        let cov = compute(
            &roster(&[
                ("emp-1", "brewer"),
                ("emp-2", "brewer"),
                ("emp-3", "shipping-clerk"),
                ("emp-4", "shipping-clerk"),
            ]),
            &tally(&[("emp-1", 5)]),
            &excluded(&[]),
        );
        assert_eq!(cov.roles_total, 2);
        assert_eq!(cov.roles_acting, 1);
        assert_eq!(cov.roles_dormant, 1);
        assert_eq!(cov.roles_operator, 0);
        assert_eq!(cov.employees_total, 4);
        assert_eq!(cov.employees_acting, 1);

        let brewer = cov.roles.iter().find(|r| r.role == "brewer").unwrap();
        assert_eq!(brewer.roster, 2);
        assert_eq!(brewer.acting, 1, "distinct people, not completions");
        assert_eq!(brewer.completions, 5);
        assert_eq!(brewer.status, RoleStatus::Acting);

        let clerk = cov
            .roles
            .iter()
            .find(|r| r.role == "shipping-clerk")
            .unwrap();
        assert_eq!(clerk.roster, 2);
        assert_eq!(clerk.acting, 0);
        assert_eq!(clerk.completions, 0);
        assert_eq!(clerk.status, RoleStatus::Dormant);
    }

    #[test]
    fn operator_roles_are_labeled_not_dormant() {
        // platform-admin's every holder is operator-excluded: the sim
        // leaves their steps for the human BY DESIGN. Report it as
        // Operator — never in the dormant totals.
        let cov = compute(
            &roster(&[
                ("emp-1", "brewer"),
                ("emp-admin", "platform-admin"),
                ("emp-bootstrap-admin", "platform-admin"),
            ]),
            &tally(&[("emp-1", 1)]),
            &excluded(&["emp-admin", "emp-bootstrap-admin"]),
        );
        assert_eq!(cov.roles_total, 2);
        assert_eq!(cov.roles_acting, 1);
        assert_eq!(cov.roles_dormant, 0, "operator roles are not dormant");
        assert_eq!(cov.roles_operator, 1);
        assert_eq!(cov.employees_total, 3);
        assert_eq!(cov.employees_operator, 2);

        let admin = cov
            .roles
            .iter()
            .find(|r| r.role == "platform-admin")
            .unwrap();
        assert_eq!(admin.status, RoleStatus::Operator);
        assert_eq!(admin.operator_holders, 2);
    }

    #[test]
    fn mixed_role_with_an_excluded_holder_is_still_dormant() {
        // One holder excluded, one simulatable, neither acting: the
        // simulatable holder makes the role dormant, not operator.
        let cov = compute(
            &roster(&[("emp-1", "auditor"), ("emp-2", "auditor")]),
            &tally(&[]),
            &excluded(&["emp-1"]),
        );
        let auditor = cov.roles.iter().find(|r| r.role == "auditor").unwrap();
        assert_eq!(auditor.status, RoleStatus::Dormant);
        assert_eq!(auditor.operator_holders, 1);
        assert_eq!(cov.roles_dormant, 1);
        assert_eq!(cov.roles_operator, 0);
        assert_eq!(cov.employees_operator, 1);
    }

    #[test]
    fn unrostered_actor_lands_in_unassigned_role() {
        // A completion by an actor the roster map doesn't know still
        // counts — under UNROSTERED_ROLE, and in the employee totals,
        // so acting can never exceed total.
        let cov = compute(
            &roster(&[("emp-1", "brewer")]),
            &tally(&[("emp-ghost", 2)]),
            &excluded(&[]),
        );
        let ghost = cov
            .roles
            .iter()
            .find(|r| r.role == UNROSTERED_ROLE)
            .unwrap();
        assert_eq!(ghost.roster, 0);
        assert_eq!(ghost.acting, 1);
        assert_eq!(ghost.completions, 2);
        assert_eq!(ghost.status, RoleStatus::Acting);
        assert_eq!(cov.employees_total, 2, "roster + the unrostered actor");
        assert_eq!(cov.employees_acting, 1);
    }

    #[test]
    fn empty_inputs_are_all_zeroes() {
        let cov = compute(&HashMap::new(), &HashMap::new(), &HashSet::new());
        assert_eq!(cov.roles_total, 0);
        assert_eq!(cov.employees_total, 0);
        assert!(cov.roles.is_empty());
    }

    #[test]
    fn roles_are_sorted_by_name() {
        // Deterministic order between polls — the SPA table must not
        // jitter.
        let cov = compute(
            &roster(&[("e1", "zymurgist"), ("e2", "auditor"), ("e3", "brewer")]),
            &tally(&[]),
            &excluded(&[]),
        );
        let names: Vec<&str> = cov.roles.iter().map(|r| r.role.as_str()).collect();
        assert_eq!(names, ["auditor", "brewer", "zymurgist"]);
    }

    #[test]
    fn status_serializes_lowercase_for_the_spa() {
        // The SPA's RoleStatus union is 'acting' | 'dormant' |
        // 'operator' — pin the wire casing.
        assert_eq!(
            serde_json::to_value(RoleStatus::Operator).unwrap(),
            serde_json::json!("operator")
        );
        assert_eq!(
            serde_json::to_value(RoleStatus::Dormant).unwrap(),
            serde_json::json!("dormant")
        );
    }

    #[test]
    fn zero_completion_tally_rows_do_not_count_as_acting() {
        // A defensive guard: an entry that exists with n=0 (e.g. a
        // future pre-registration) must not read as acting.
        let cov = compute(
            &roster(&[("emp-1", "brewer")]),
            &tally(&[("emp-1", 0)]),
            &excluded(&[]),
        );
        assert_eq!(cov.employees_acting, 0);
        assert_eq!(cov.roles.first().unwrap().status, RoleStatus::Dormant);
    }
}
