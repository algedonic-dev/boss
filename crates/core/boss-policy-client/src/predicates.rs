//! Turn a (`user`, `scope`) pair into a resource-agnostic `Predicate`.
//!
//! The policy engine uses this internally; each domain service also
//! uses it when it needs a list-endpoint filter.

use crate::types::{Predicate, Scope, User};

/// Translate a Scope + the user's context into a Predicate the caller's
/// repository can consume. Returns `Predicate::None` if the scope
/// disallows any rows, `Predicate::Unrestricted` for `Scope::All`.
pub fn scope_to_predicate(scope: &Scope, user: &User) -> Predicate {
    match scope {
        Scope::None => Predicate::None,
        Scope::All => Predicate::Unrestricted,
        Scope::Self_ => Predicate::OwnerIs {
            user_id: user.id.clone(),
        },
        Scope::Territory => Predicate::AccountIn {
            account_ids: user.territory_account_ids.clone(),
        },
        Scope::Team => {
            // Self + direct reports.
            let mut ids = vec![user.id.clone()];
            ids.extend(user.direct_report_ids.iter().cloned());
            Predicate::OwnerIn { user_ids: ids }
        }
        Scope::Department(d) => Predicate::DepartmentIs {
            department: d.clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user_with(id: &str, territory: Vec<&str>, reports: Vec<&str>) -> User {
        User {
            id: id.to_string(),
            role: "sales-rep".to_string(),
            access_tier: crate::types::AccessTier::User,
            territory_account_ids: territory.iter().map(|s| s.to_string()).collect(),
            direct_report_ids: reports.iter().map(|s| s.to_string()).collect(),
            department: None,
        }
    }

    #[test]
    fn none_and_all_map_to_bounds() {
        let u = user_with("emp-1", vec![], vec![]);
        assert!(matches!(
            scope_to_predicate(&Scope::None, &u),
            Predicate::None
        ));
        assert!(matches!(
            scope_to_predicate(&Scope::All, &u),
            Predicate::Unrestricted
        ));
    }

    #[test]
    fn self_emits_owner_is() {
        let u = user_with("emp-42", vec![], vec![]);
        match scope_to_predicate(&Scope::Self_, &u) {
            Predicate::OwnerIs { user_id } => assert_eq!(user_id, "emp-42"),
            other => panic!("expected OwnerIs, got {other:?}"),
        }
    }

    #[test]
    fn territory_emits_account_in() {
        let u = user_with("emp-5", vec!["p-1", "p-2", "p-3"], vec![]);
        match scope_to_predicate(&Scope::Territory, &u) {
            Predicate::AccountIn { account_ids } => {
                assert_eq!(account_ids, vec!["p-1", "p-2", "p-3"]);
            }
            other => panic!("expected AccountIn, got {other:?}"),
        }
    }

    #[test]
    fn team_emits_owner_in_with_self_plus_reports() {
        let u = user_with("emp-mgr", vec![], vec!["emp-a", "emp-b"]);
        match scope_to_predicate(&Scope::Team, &u) {
            Predicate::OwnerIn { user_ids } => {
                assert_eq!(user_ids, vec!["emp-mgr", "emp-a", "emp-b"]);
            }
            other => panic!("expected OwnerIn, got {other:?}"),
        }
    }

    #[test]
    fn department_emits_department_is() {
        let u = user_with("emp-1", vec![], vec![]);
        match scope_to_predicate(&Scope::Department("service".into()), &u) {
            Predicate::DepartmentIs { department } => assert_eq!(department, "service"),
            other => panic!("expected DepartmentIs, got {other:?}"),
        }
    }
}
