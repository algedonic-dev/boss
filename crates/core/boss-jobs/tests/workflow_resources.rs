//! Console car 1: the authored bill must survive the protocol boundary.
use boss_jobs::registry::StepSpec;
use serde_json::json;

#[test]
fn a_resource_bill_survives_the_step_spec_roundtrip() {
    let bill = json!({"model":"fable","vcpu":"8","ram_gb":"32","disk_gb":"70"});
    let step: StepSpec = serde_json::from_value(json!({
        "title":"work","kind":"task","ready_when":"true","resources":bill
    }))
    .expect("authored step");
    let encoded = serde_json::to_value(step).expect("step wire");
    assert_eq!(
        encoded.get("resources"),
        Some(&bill),
        "a resource bill is protocol data, not an ignored annotation"
    );
}

#[test]
fn an_unauthored_bill_preserves_the_legacy_step_wire() {
    let step: StepSpec = serde_json::from_value(json!({
        "title":"work","kind":"task","ready_when":"true"
    }))
    .expect("legacy step");
    assert!(
        serde_json::to_value(step)
            .expect("step wire")
            .get("resources")
            .is_none()
    );
}

#[test]
fn authored_toml_bill_is_preserved_and_undeclared_pools_are_linted() {
    let text = r#"
[[workflow]]
kind="resources-test"
label="Resources test"
category="test"
subject_kinds=["custom"]
[[workflow.step]]
title="start"
kind="trigger"
ready_when="true"
resources={model="sol",vcpu=8,ram_gb=32,disk_gb=70}
[[workflow.step]]
title="done"
kind="outcome"
ready_when="steps.start.done"
terminal={outcome="done"}
"#;
    let specs = boss_jobs::seed_loader::parse_workflows(text, "platform", "fixture")
        .expect("resource declaration");
    let pools = boss_jobs::pools::load_pools(boss_jobs::pools::platform_pools_path())
        .expect("declared pools");
    let declared = |pools: &[boss_jobs::pools::PoolSpec]| {
        boss_jobs::pools::validate_step_declarations(
            &specs,
            &Declarations {
                pools,
                stations: &[],
                roles: RoleVocabulary::NotReadHere,
            },
        )
    };
    assert!(declared(&pools).is_empty());
    assert_eq!(
        serde_json::to_value(&specs[0].steps[0]).expect("wire")["resources"],
        json!({"model":"sol","vcpu":"8","ram_gb":"32","disk_gb":"70"})
    );
    let missing: Vec<_> = pools.into_iter().filter(|p| p.name != "vcpu").collect();
    let failures = declared(&missing);
    assert!(
        failures
            .iter()
            .any(|f| f.contains("resources-test") && f.contains("start") && f.contains("vcpu")),
        "{failures:?}"
    );
    assert!(
        boss_jobs::seed_loader::parse_workflows(
            &text.replace("vcpu=8", "vcpu=-1"),
            "platform",
            "fixture"
        )
        .is_err()
    );
}

// ---------------------------------------------------------------------
// Factory taxonomy M3 (item 0a78f8f8): a step names its executor kind.
// The lint that refuses a `resources` block naming an undeclared pool
// refuses an `executor` naming an undeclared kind, with the same three
// registries a deployment declares them in: a role is an employee-kind
// Class, a model class is a `robots:<class>` pool, and equipment is
// an equipment station row (T2a's `gates` and `loading-dock`).

use boss_jobs::step_executor::{Declarations, RoleVocabulary};
use std::collections::BTreeSet;

fn executor_fixture(executor: &str) -> Vec<boss_jobs::registry::WorkflowSpec> {
    let text = format!(
        r#"
[[workflow]]
kind="executor-test"
label="Executor test"
category="test"
subject_kinds=["custom"]
[[workflow.step]]
title="start"
kind="trigger"
ready_when="true"
[[workflow.step]]
title="work"
kind="task"
ready_when="steps.start.done"
executor="{executor}"
terminal={{outcome="done"}}
"#
    );
    boss_jobs::seed_loader::parse_workflows(&text, "platform", "fixture")
        .unwrap_or_else(|e| panic!("{executor}: {e}"))
}

fn lint(executor: &str, roles: RoleVocabulary<'_>) -> Vec<String> {
    let pools = boss_jobs::pools::load_pools(boss_jobs::pools::platform_pools_path())
        .expect("declared pools");
    let stations =
        boss_jobs::seed_loader::load_stations(boss_jobs::station_seed::platform_stations_path())
            .expect("declared stations");
    boss_jobs::pools::validate_step_declarations(
        &executor_fixture(executor),
        &Declarations {
            pools: &pools,
            stations: &stations,
            roles,
        },
    )
}

fn employee_roles() -> BTreeSet<String> {
    [
        "platform-admin".to_string(),
        "engineering-agent".to_string(),
    ]
    .into()
}

#[test]
fn a_declared_executor_of_each_kind_passes_the_declaration_lint() {
    let roles = employee_roles();
    for declared in [
        "role:platform-admin",
        "model:opus",
        "model:sol",
        "equipment:gates",
        "equipment:loading-dock",
    ] {
        let failures = lint(declared, RoleVocabulary::Read(&roles));
        assert!(failures.is_empty(), "{declared}: {failures:?}");
    }
}

#[test]
fn an_undeclared_executor_of_each_kind_is_refused_naming_what_is_missing() {
    let roles = employee_roles();
    for (declared, names) in [
        ("role:no-such-role", "employee role"),
        ("model:gpt", "robots:gpt"),
        ("equipment:no-such-station", "no station"),
        // A station that exists but is not equipment: the dock's
        // neighbour on the review side is a group's queue.
        ("equipment:design-review", "not an equipment station"),
    ] {
        let failures = lint(declared, RoleVocabulary::Read(&roles));
        assert!(
            failures.iter().any(|f| f.contains("executor-test")
                && f.contains("`work`")
                && f.contains(declared)
                && f.contains(names)),
            "{declared}: {failures:?}"
        );
    }
}

#[test]
fn a_role_executor_is_judged_only_where_the_class_registry_is_read() {
    // The tree holds no Class registry — roles are declared in the live
    // `classes` table, some through its door and in no migration — so a
    // reader without it says so rather than refusing every role or
    // passing one it never read. The seed reads the table before any
    // publish; this is the tree-wide pin's half.
    assert!(lint("role:no-such-role", RoleVocabulary::NotReadHere).is_empty());
    // The other two kinds are tree data and are judged regardless.
    assert!(!lint("model:gpt", RoleVocabulary::NotReadHere).is_empty());
    assert!(!lint("equipment:no-such-station", RoleVocabulary::NotReadHere).is_empty());
}

#[test]
fn a_credential_principal_is_not_an_executor_kind() {
    // `runner:ops` (design f623e425) is judged by the viability lint's
    // resolvable-principal check, never as a kind this lint must find.
    let roles = employee_roles();
    let pools = boss_jobs::pools::load_pools(boss_jobs::pools::platform_pools_path())
        .expect("declared pools");
    let mut specs = executor_fixture("role:platform-admin");
    specs[0].steps[1].executor = Some("runner:ops".into());
    assert!(
        boss_jobs::pools::validate_step_declarations(
            &specs,
            &Declarations {
                pools: &pools,
                stations: &[],
                roles: RoleVocabulary::Read(&roles),
            },
        )
        .is_empty()
    );
}

// ---------------------------------------------------------------------
// A SLOT MEANS A RUN IS WORKING (backlog af5dd547, allocation order item
// 2). A bill may take its model class from the run (`model = "from-run"`),
// resolved at the claim through the robot pool whose `models` lists
// the run's own model; the claim-time half is pinned on both adapters in
// flow_resource_claims.rs. These pin the declaration half and the tree.

fn model_pool(name: &str, models: serde_json::Value) -> boss_jobs::pools::PoolSpec {
    serde_json::from_value(
        json!({"name":name,"version":1,"status":"active","unit":"slot",
        "kind":"flow","capacity":{"state":"unknown"},"models":models}),
    )
    .expect("pool row")
}

#[test]
fn a_from_run_bill_is_declared_only_once_a_pool_meters_a_model_by_name() {
    let bill: boss_jobs::resources::ResourceBill =
        serde_json::from_value(json!({"model":"from-run"})).expect("bill");
    assert!(bill.takes_model_from_run());
    // With no pool listing a model, the bill is refused, naming why — the
    // publish order item 5 owes: pools with their `models` first.
    let tree = boss_jobs::pools::load_pools(boss_jobs::pools::platform_pools_path())
        .expect("declared pools");
    let unlisted: Vec<_> = tree
        .iter()
        .cloned()
        .map(|mut p| {
            p.models.clear();
            p
        })
        .collect();
    let refused =
        boss_jobs::pools::validate_bill(&bill, &unlisted).expect_err("no pool meters a model");
    assert!(refused.contains("from-run"), "{refused}");
    // The tree's own pools list theirs (35e9babf item 5), so against the
    // tree it is declared.
    assert!(
        boss_jobs::pools::validate_bill(&bill, &tree).is_ok(),
        "the tree's model-slot pools list the models a run is metered by"
    );
    assert!(
        boss_jobs::pools::validate_bill(&bill, &[model_pool("robots:opus", json!(["opus-5-5"]))])
            .is_ok()
    );
    // A fixed class is still judged against its own pool, as before.
    let fixed: boss_jobs::resources::ResourceBill =
        serde_json::from_value(json!({"model":"opus"})).expect("bill");
    assert!(!fixed.takes_model_from_run());
    assert!(boss_jobs::pools::validate_bill(&fixed, &tree).is_ok());
}

#[test]
fn only_a_model_slot_pool_lists_models_and_none_is_spelled_from_run() {
    assert!(
        model_pool("robots:opus", json!(["opus-5-5", "opus-5[1m]"]))
            .validate()
            .is_ok()
    );
    for (name, models) in [
        ("vcpu", json!(["opus-5-5"])),
        ("robots:opus", json!([""])),
        ("robots:opus", json!([" opus-5-5"])),
        ("robots:opus", json!(["opus-5-5", "opus-5-5"])),
        ("robots:from-run", json!([])),
    ] {
        assert!(
            model_pool(name, models.clone()).validate().is_err(),
            "{name} {models} must be refused"
        );
    }
}

/// Backlog 3901fc49: a robot class is judged by ONE grammar. The pool
/// checked only its `robots:` prefix while the bill refused any byte but a
/// letter, a digit or `-`, so `robots:bad_class` listing a model published,
/// a from-run claim resolved the run to `bad_class`, and the bill the claim
/// built refused it as a storage failure (HTTP 500, measured on both
/// adapters by review run dd37e7af). Each candidate is judged by the pool
/// and by the bill, and the two answers must be one answer.
#[test]
fn a_robot_pool_and_a_bill_judge_a_class_by_one_grammar() {
    use boss_jobs::resources::{ResourceBill, is_robot_class};
    for class in [
        "opus",
        "sol",
        "gpt-6",
        "agent-workspace",
        "bad_class",
        "bad:class",
        "",
        "Opus",
        "op us",
        "opus/5",
    ] {
        let pooled = model_pool(&format!("robots:{class}"), json!(["fixture-exact-model"]))
            .validate()
            .is_ok();
        let bill: ResourceBill = serde_json::from_value(json!({"model": class})).expect("bill");
        let billed = bill.validate().is_ok();
        assert_eq!(
            (pooled, billed),
            (is_robot_class(class), is_robot_class(class)),
            "class {class:?}: the pool said {pooled}, the bill said {billed}"
        );
    }
    assert!(is_robot_class("opus") && !is_robot_class("bad_class"));
    // The tree's own robot pools are classes a bill can carry.
    let tree = boss_jobs::pools::load_pools(boss_jobs::pools::platform_pools_path())
        .expect("declared pools");
    let classes: Vec<&str> = tree
        .iter()
        .filter_map(|p| p.name.strip_prefix(boss_jobs::resources::ROBOTS_PREFIX))
        .collect();
    assert!(!classes.is_empty(), "the tree declares robot pools");
    assert!(classes.iter().all(|c| is_robot_class(c)), "{classes:?}");
}

/// The tree-wide half of "a nomination or a backlog build claim holds no
/// slot": a reservation is made only at ready -> active, so what decides
/// it is which step carries a bill naming a model. Measured on main
/// 18366f8f: backlog-item `build` stays ACTIVE until its car closes and is
/// proven (agent_budget.rs), days, so a bill there holds a slot for days;
/// `agent-run` `building` is claimed at `--started` and released when
/// building exits. Since 35e9babf item 5 agent-run's `building` bills
/// `from-run` (held out of the drift publish until David's deliberate
/// publish); this holds the line that it is the only row that may.
#[test]
fn only_a_runs_building_may_bill_a_model_slot_and_a_backlog_build_bills_nothing() {
    let bundle =
        boss_jobs::seed_loader::load_workflows(boss_jobs::registry::platform_bundle_path())
            .expect("the platform bundle parses");
    let billing: Vec<String> = bundle
        .iter()
        .flat_map(|w| {
            w.steps
                .iter()
                .filter(|s| s.resources.as_ref().is_some_and(|b| b.model.is_some()))
                .map(move |s| format!("{}/{}", w.kind, s.title))
        })
        .collect();
    assert_eq!(
        billing,
        ["agent-run/building"],
        "a model slot is held only by a working run"
    );
    let building = bundle
        .iter()
        .find(|w| w.kind == "agent-run")
        .and_then(|w| w.steps.iter().find(|s| s.title == "building"))
        .and_then(|s| s.resources.as_ref())
        .expect("agent-run building bills");
    // Its vCPU is the worker's request, drawn from a host at the claim
    // (backlog 5608c1a2); the amount is held to dev-build.py's Pod by
    // infra/dev/dev-build_test.py, and the row never names the host.
    assert!(
        building.takes_model_from_run() && building.vcpu.is_some() && building.host.is_none(),
        "one agent-run row serves every model: the class is read off the run ({building:?})"
    );
    let build = bundle
        .iter()
        .find(|w| w.kind == "backlog-item")
        .and_then(|w| w.steps.iter().find(|s| s.title == "build"))
        .expect("backlog-item has its build step");
    assert!(
        build.resources.is_none(),
        "a backlog build claim is held for days and bills nothing: {:?}",
        build.resources
    );
}

/// THE BUNDLE'S OWN NUMBERS, DERIVED (backlog 5608c1a2). The share the
/// claim door bounds a builder's worker by is read off the station rows
/// that decide it, and this holds the resolver to those same rows: the
/// owner of agent-run floats exactly the host pools its row allocates;
/// each share is that row's allocation less what the rows standing in it
/// reserve of the same pool, conserved (floating plus reserved is the
/// allocation) and positive, so a run has somewhere to float. No literal:
/// review 593436d2 R2 struck the `16` this asserted at first, because an
/// allocation edit on engineering.toml would then have needed a code edit
/// to land — the number belongs to the row, and only the rule is pinned.
#[test]
fn a_run_floats_in_its_owners_share_of_the_build_node() {
    use boss_jobs::flow_resources::Amount;
    use std::collections::BTreeMap;
    let bundle =
        boss_jobs::seed_loader::load_workflows(boss_jobs::registry::platform_bundle_path())
            .expect("the platform bundle parses");
    let stations =
        boss_jobs::seed_loader::load_stations(boss_jobs::station_seed::platform_stations_path())
            .expect("the platform stations parse");
    let run = bundle
        .iter()
        .find(|w| w.kind == "agent-run")
        .expect("agent-run");
    let owner = boss_jobs::station_tree::protocol_owner(run).expect("agent-run names its owner");
    let rows = boss_jobs::station_tree::tree_rows(&stations);
    let station = rows
        .iter()
        .find(|r| r.id.as_deref() == Some(owner))
        .expect("agent-run's owner stands in the tree");
    let hosts: Vec<&String> = station
        .allocation
        .keys()
        .filter(|pool| boss_jobs::station_tree::host_of(pool).is_some())
        .collect();
    assert!(
        !hosts.is_empty(),
        "{} allocates no host pool, so a run's vCPU has no box to float on",
        station.name
    );
    let shares = boss_jobs::flow_resources::floating_shares(&stations, owner).expect("shares");
    assert_eq!(
        shares.iter().map(|(pool, _)| pool).collect::<Vec<_>>(),
        hosts,
        "one share per host pool {} allocates, and no other",
        station.name
    );
    for (pool, share) in &shares {
        assert_eq!(
            share.station, station.name,
            "{pool}: the share is the owner's"
        );
        let allocated = station
            .allocation
            .get(pool)
            .and_then(|q| q.amount().ok())
            .expect("the owner's allocation parses");
        assert_eq!(
            share.allocated, allocated,
            "{pool}: what the owner's row holds"
        );
        let reserved: BTreeMap<String, Amount> = rows
            .iter()
            .filter(|child| child.parent.as_deref() == Some(owner))
            .filter_map(|child| {
                child.allocation.get(pool).map(|q| {
                    (
                        child.name.clone(),
                        q.amount().expect("a reservation parses"),
                    )
                })
            })
            .collect();
        assert_eq!(
            share.reserved, reserved,
            "{pool}: what the stations standing in {} reserve of it",
            station.name
        );
        let floating = share.floating().expect("the share floats");
        let conserved = reserved
            .values()
            .try_fold(floating.clone(), |sum, q| sum.checked_add(q))
            .expect("the shares sum");
        assert_eq!(
            conserved, allocated,
            "{pool}: floating plus reserved is the allocation, nothing lost or made"
        );
        assert!(
            floating > Amount::zero(),
            "{pool}: {} floats nothing of it ({floating}), so no run can be placed there",
            station.name
        );
    }
}

/// EVERY MODEL A RUN IS DISPATCHED ON HAS ONE CLASS (35e9babf item 5).
/// agent-run's `from-run` bill reads the class off the run's own `model`,
/// which `boss dispatch` takes from the step's agent block, so a block
/// model no active model-slot pool lists is a run whose every claim is
/// refused `run-model-has-no-class`, and a model two pools list is one
/// refused `run-model-has-two-classes`. The tree's blocks are the half of
/// "every spelling dispatch writes" the tree can see; the tenant's rows
/// (close-a-period and measure-the-page still say `opus-5[1m]`, read live
/// 2026-11-06) and `--model gpt-6.1-sol` for Codex are listed on the pool
/// rows themselves, where the reading is cited.
#[test]
fn every_model_a_platform_agent_block_names_is_listed_by_exactly_one_model_slot_pool() {
    let pools = boss_jobs::pools::load_pools(boss_jobs::pools::platform_pools_path())
        .expect("declared pools");
    let bundle =
        boss_jobs::seed_loader::load_workflows(boss_jobs::registry::platform_bundle_path())
            .expect("the platform bundle parses");
    let models: std::collections::BTreeSet<String> = bundle
        .iter()
        .flat_map(|w| w.steps.iter())
        .filter_map(|s| s.agent.as_ref().map(|a| a.model.clone()))
        .collect();
    assert!(!models.is_empty(), "the bundle declares agent blocks");
    let unmetered: Vec<String> = models
        .iter()
        .filter_map(|model| {
            let classes: Vec<&str> = pools
                .iter()
                .filter(|p| p.models.iter().any(|m| m == model))
                .map(|p| p.name.as_str())
                .collect();
            (classes.len() != 1).then(|| format!("{model} -> {classes:?}"))
        })
        .collect();
    assert!(
        unmetered.is_empty(),
        "each agent-block model must be listed by exactly one model-slot pool: {unmetered:?}"
    );
}
