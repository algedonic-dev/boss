//! A department-scoped brewery grant admits the role's holders in that
//! department and refuses the same role anywhere else (backlog
//! f7daf124, re-scoped by its triage on 2026-09-28).
//!
//! A `department:<code>` scope becomes `Predicate::DepartmentIs`, which
//! admits a caller only when the caller's own `department` is `<code>`.
//! Five grants named `brewhouse`, `lab` and `hr` — codes the collapse
//! onto the departments registry (c87e3d6d) left undeclared, while the
//! roles' holders sit in `production`, `qa` and `people` — so each
//! admitted no one, silently. That every scope names a declared code is
//! pinned tree-wide in
//! crates/core/boss-testing/tests/a_department_is_a_registry_row_not_a_class.rs;
//! this file judges the grants the way a request is judged: the seeded
//! rules, the engine's decision, the caller's department.

use std::collections::BTreeSet;
use std::path::PathBuf;

use boss_policy_client::{
    AccessTier, Action, Decision, FakePolicyClient, PolicyClient, Resource, User,
    scope_to_predicate,
};
use serde_json::Value;

fn seeds() -> PathBuf {
    boss_testing::repo_root().join("examples/brewery/seeds")
}

/// A fresh brewery instance's rules: core's defaults, then the seed.
fn seeded_policy() -> FakePolicyClient {
    let seed =
        boss_policy_client::seed_loader::load_policy_rules(&seeds().join("policy_rules.toml"))
            .expect("the brewery policy seed loads");
    boss_policy_client::defaults::default_rules()
        .into_iter()
        .chain(seed)
        .filter(|r| r.active)
        .fold(FakePolicyClient::builder(), |b, r| {
            b.allow(r.role, r.action, r.resource, r.scope)
        })
        .build()
}

/// The departments the roster's holders of `role` sit in.
fn departments_of(role: &str) -> BTreeSet<String> {
    let path = seeds().join("employees.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    let rows: Value = serde_json::from_str(&text).expect("employees.json parses");
    rows.as_array()
        .expect("employees.json is an array")
        .iter()
        .filter(|e| e["role"] == role)
        .filter_map(|e| e["department"].as_str().map(str::to_string))
        .collect()
}

fn in_department(role: &str, department: &str) -> User {
    User {
        id: format!("emp-{role}-in-{department}"),
        role: role.to_string(),
        access_tier: AccessTier::User,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: Some(department.to_string()),
    }
}

/// Whether the seeded rules admit `user` to `action` on `resource`
/// rows: allowed, and the allowed scope's predicate does not narrow the
/// caller to nothing.
async fn admitted(policy: &FakePolicyClient, user: &User, action: Action, resource: &str) -> bool {
    match policy
        .check(user, action, Resource::new(resource))
        .await
        .expect("the in-memory policy answers")
    {
        Decision::Allow { scope } => scope_to_predicate(&scope, user)
            .owner_allow_list(user)
            .is_none_or(|owners| !owners.is_empty()),
        Decision::Deny { .. } => false,
    }
}

/// The five grants the triage measured inert, by role, action and
/// resource.
const REPOINTED: [(&str, Action, &str); 5] = [
    ("head-brewer", Action::Update, "step"),
    ("head-brewer", Action::SignOff, "step"),
    ("lab-tech", Action::SignOff, "step"),
    ("qa-supervisor", Action::SignOff, "step"),
    ("recruiter", Action::Create, "employee"),
];

#[tokio::test]
async fn each_repointed_grant_admits_its_holders_in_their_own_department() {
    let policy = seeded_policy();
    let mut inert = Vec::new();
    for (role, action, resource) in REPOINTED {
        let homes = departments_of(role);
        assert!(!homes.is_empty(), "{role}: the roster staffs no one");
        for home in &homes {
            if !admitted(&policy, &in_department(role, home), action, resource).await {
                inert.push(format!("{role} in {home}: {action:?} {resource}"));
            }
        }
    }
    assert!(
        inert.is_empty(),
        "department-scoped grants that refuse the role's own holders — the scope names a \
         department they do not sit in: {inert:#?}"
    );
}

#[tokio::test]
async fn the_same_role_in_another_department_is_refused() {
    let policy = seeded_policy();
    for (role, action, resource) in REPOINTED {
        let elsewhere = in_department(role, "finance");
        assert!(
            !departments_of(role).contains("finance"),
            "{role}: pick a department no holder sits in"
        );
        assert!(
            !admitted(&policy, &elsewhere, action, resource).await,
            "{role} in finance was admitted to {action:?} {resource} — a department scope \
             must refuse the same role outside its department"
        );
    }
}
