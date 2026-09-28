//! Every brewery role may open the packets its surfaces file, and no
//! brewery role may open every kind (design 222fc982, backlog 6dc75abd).
//!
//! Opening a packet asks Create on `job:<kind>`, with Create on plain
//! `job` the all-kinds grant (`boss_jobs::open_authority`). A role the
//! seed grants neither for a kind one of its surfaces files gets a 403
//! from that surface — which is how every brewery role but two lost the
//! feedback widget when admission started asking (train #734). These run
//! the SAME decision admission runs, over the rules a fresh brewery
//! instance is seeded with (core's defaults plus `policy_rules.toml`),
//! for every role the tenant declares.
//!
//! Which surface files which kind, measured 2026-09-27 from the web:
//!
//! - `libs/web-kit/src/FeedbackControl.svelte` — `user-feedback`, from
//!   every page, for every signed-in role.
//! - `apps/web/src/shop/ShopProductPage.svelte` — `direct-shop-order`;
//!   the shop is always on (`canSeeRoute` in
//!   `libs/web-kit/src/session/permissions.ts`), so every role.
//! - `apps/web/src/parts/PartPage.svelte` — `ingredient-restock`, the
//!   reorder button, for every role that sees `parts` (a role whose Class
//!   row declares no surfaces sees every one).
//!
//! The generic New Job form (`JobsListPage.svelte`) files whatever kind
//! the role picks, so it is answered by whatever the role holds, and has
//! no grant of its own here — a grant for it would be the all-kinds one.

use std::collections::BTreeSet;
use std::path::PathBuf;

use boss_policy_client::{
    AccessTier, Action, FakePolicyClient, PolicyClient, Resource, Scope, User,
};
use serde_json::Value;

fn repo() -> PathBuf {
    boss_testing::repo_root()
}

fn seeds() -> PathBuf {
    repo().join("examples/brewery/seeds")
}

fn read_json(name: &str) -> Value {
    let path = seeds().join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()))
}

/// The role Class rows (`subject_kind = employee`, `member_attribute =
/// role`): code and declared surfaces, `None` when the row declares none
/// (such a role sees every surface).
fn role_rows() -> Vec<(String, Option<Vec<String>>)> {
    read_json("classes.json")
        .as_array()
        .expect("classes.json is an array")
        .iter()
        .filter(|c| c["subject_kind"] == "employee" && c["member_attribute"] == "role")
        .filter_map(|c| {
            let code = c["code"].as_str()?.to_string();
            let surfaces = c["metadata"]["surfaces"].as_array().map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(str::to_string))
                    .collect()
            });
            Some((code, surfaces))
        })
        .collect()
}

/// Every role the tenant declares or staffs: the Class rows and every
/// role an employee in the roster carries (a roster role with no Class
/// row still signs in).
fn every_role() -> BTreeSet<String> {
    let staffed = read_json("employees.json")
        .as_array()
        .expect("employees.json is an array")
        .iter()
        .filter_map(|e| e["role"].as_str().map(str::to_string))
        .collect::<BTreeSet<_>>();
    let declared = role_rows().into_iter().map(|(code, _)| code);
    staffed.into_iter().chain(declared).collect()
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

fn signed_in(role: &str) -> User {
    User {
        id: format!("emp-as-{role}"),
        role: role.to_string(),
        access_tier: AccessTier::User,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: None,
    }
}

async fn may_open(policy: &FakePolicyClient, role: &str, kind: &str) -> bool {
    boss_jobs::open_authority::may_open(policy, &signed_in(role), Some(kind))
        .await
        .expect("the in-memory policy answers")
        .is_allowed()
}

#[tokio::test]
async fn every_role_may_file_feedback_and_a_shop_order() {
    let policy = seeded_policy();
    let roles = every_role();
    boss_testing::assert_roster_floor!(roles, 40, "brewery roles (51 on 2026-09-27)");
    let mut missing = Vec::new();
    for role in &roles {
        for kind in ["user-feedback", "direct-shop-order"] {
            if !may_open(&policy, role, kind).await {
                missing.push(format!("{role} -> {kind}"));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "roles that cannot open what an always-on surface files — add them to the \
         grant in examples/brewery/seeds/policy_rules.toml: {missing:#?}"
    );
}

#[tokio::test]
async fn every_role_that_sees_parts_may_file_a_restock() {
    let policy = seeded_policy();
    let sees_parts: Vec<String> = role_rows()
        .into_iter()
        .filter(|(_, surfaces)| {
            surfaces
                .as_ref()
                .is_none_or(|s| s.iter().any(|x| x == "parts"))
        })
        .map(|(code, _)| code)
        .collect();
    boss_testing::assert_roster_floor!(sees_parts, 10, "roles that see parts (25 on 2026-09-27)");
    let mut missing = Vec::new();
    for role in &sees_parts {
        if !may_open(&policy, role, "ingredient-restock").await {
            missing.push(role.clone());
        }
    }
    assert!(
        missing.is_empty(),
        "roles that see the parts page but cannot open the ingredient-restock its \
         reorder button files: {missing:?}"
    );
}

#[tokio::test]
async fn no_brewery_role_holds_the_all_kinds_grant() {
    // The packet's warning: a tenant fixing feedback by granting Create
    // on `job` hands every role every kind, `ops-request` included.
    let policy = seeded_policy();
    let mut wide = Vec::new();
    for role in every_role() {
        let every_kind = policy
            .check(&signed_in(&role), Action::Create, Resource::job())
            .await
            .expect("the in-memory policy answers");
        if every_kind.is_allowed() || may_open(&policy, &role, "ops-request").await {
            wide.push(role);
        }
    }
    assert!(
        wide.is_empty(),
        "brewery roles that may open every kind (Create on plain `job`): {wide:?}"
    );
}

#[tokio::test]
async fn the_roles_that_held_every_kind_keep_the_kinds_they_file() {
    // sales-rep and events-coord held Create on `job` before the kind
    // scoping; each is narrowed to the kinds its work files, not cut off.
    let policy = seeded_policy();
    for (role, kind) in [
        ("sales-rep", "wholesale-keg-order"),
        ("sales-rep", "account-checkin"),
        ("events-coord", "taproom-event"),
        ("events-coord", "tap-launch"),
    ] {
        assert!(
            may_open(&policy, role, kind).await,
            "{role} lost {kind} when its all-kinds grant was narrowed"
        );
    }
}

/// The kinds of every `[[workflow]]` table in one TOML file — the shape
/// the brewery's `workflows.toml` and each platform file share.
fn workflow_kinds(path: PathBuf) -> Vec<String> {
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    let doc: toml::Value =
        toml::from_str(&text).unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()));
    doc.get("workflow")
        .and_then(|w| w.as_array())
        .into_iter()
        .flatten()
        .filter_map(|w| w.get("kind")?.as_str().map(str::to_string))
        .collect()
}

/// Every kind a Workflow in this tree defines: the brewery's own and the
/// platform's.
fn defined_kinds() -> BTreeSet<String> {
    let platform_dir = repo().join("infra/platform/workflows");
    let platform = std::fs::read_dir(&platform_dir)
        .unwrap_or_else(|e| panic!("reading {}: {e}", platform_dir.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .flat_map(workflow_kinds);
    workflow_kinds(seeds().join("workflows.toml"))
        .into_iter()
        .chain(platform)
        .collect()
}

#[test]
fn every_kind_a_grant_names_is_a_kind_that_exists() {
    // A grant on `job:user-feedbak` grants nothing and fails silently —
    // the role just gets a 403. Named here instead.
    let kinds = defined_kinds();
    boss_testing::assert_roster_floor!(kinds, 60, "workflow kinds (93 on 2026-09-27)");
    let seed =
        boss_policy_client::seed_loader::load_policy_rules(&seeds().join("policy_rules.toml"))
            .expect("the brewery policy seed loads");
    let prefix = Resource::job_of_kind("");
    let unknown: BTreeSet<String> = seed
        .iter()
        .filter_map(|r| r.resource.as_str().strip_prefix(prefix.as_str()))
        .filter(|k| !kinds.contains(*k))
        .map(str::to_string)
        .collect();
    assert!(
        unknown.is_empty(),
        "policy_rules.toml grants Create on job:<kind> for kinds no Workflow defines: {unknown:?}"
    );
    // And the narrow grants exist at all — the walk reads the right shape.
    assert!(
        seed.iter()
            .any(|r| r.resource == Resource::job_of_kind("user-feedback")
                && r.action == Action::Create
                && r.scope == Scope::All),
        "no Create on job:user-feedback in the seed — the walk is reading the wrong shape"
    );
}
