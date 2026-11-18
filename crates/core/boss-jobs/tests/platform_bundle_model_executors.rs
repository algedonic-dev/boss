//! The steps robots of declared MODEL CLASSES execute, and the classes that
//! can carry them (design ccfe87ff "Using the taxonomy: the rows that
//! make the doors bite", decided by David 2026-11-05; pass item 1,
//! backlog 446ab559).
//!
//! The decision was "build and the two reviews by model class;
//! everything else stays undeclared until a constraint is real". What
//! the record showed when this was built (2026-11-05, 200 newest
//! ship-a-change packets and 300 newest backlog-items read through the
//! jobs API) is that each of the three steps is performed by BOTH model
//! families today. A declaration now names a set of model classes:
//!
//! - `backlog-item/build` — held by agent-claude-b, agent-claude and
//!   agent-codex. Its agent block names `opus-5-5`, the one priced model
//!   every block names (`platform_bundle_agent_blocks.rs`), and its
//!   brief is a Claude-harness brief, so a Codex run of it is a run the
//!   packet describes as an opus run it is not. That was the 2026-11-05
//!   constraint, `model:opus`. On 2026-11-08 David superseded it
//!   (backlog b89af803: "let Codex build deletion-only cars ... arbitrary
//!   limitations in some of our protocols ... a good example") after the
//!   rule refused Codex's dispatch of two deletion items while the opus
//!   pool ran at its limit; the step now admits every class a robot
//!   records (`model:opus|sol`, the fourth pin below derives the set from
//!   the robot pools), and a robot whose agents row records a class
//!   outside it, or none, is refused at the claim and never nominated.
//!   Who is nominated stays the conductor's judgment.
//! - `ship-a-change/review` and `ship-a-change/landed-review` — the
//!   reviewer is meant to be a DISTINCT reader of the builder's car, and
//!   the cross-family reader is agent-codex (26 of 194 completed
//!   reviews, 24 of 83 completed landed-reviews in that read). One class
//!   cannot say "a different reader": `model:opus` cuts the only
//!   cross-family reviewer, `model:sol` cuts the two Claudes who do the
//!   rest. The shared parser and claim predicate already accept a set;
//!   backlog f4425aad completes the two missing declarations. Both
//!   reviews admit `model:opus|sol`; an unknown or unlisted class is
//!   still refused. Admission is separate from advisory launch settings.
//!
//! A step that names a class nobody can carry strands: no robot is
//! nominated and every claim is refused. So the second pin holds every
//! class a step demands to an active `robots:<class>` pool in the
//! tree — the same predicate the seed's pre-publish lint applies
//! (`pools::validate_step_declarations`), stated here per class, with a
//! control that shows it bites.

use std::collections::BTreeSet;

use boss_jobs::pools::{PoolSpec, load_pools, platform_pools_path};
use boss_jobs::registry::{WorkflowSpec, WorkflowStatus, platform_bundle_path};
use boss_jobs::seed_loader::load_workflows;
use boss_jobs::step_executor::{Capability, ExecutorDemand, demanded_by, judge_capability};

fn bundle() -> Vec<WorkflowSpec> {
    load_workflows(platform_bundle_path()).expect("the platform bundle parses")
}

fn pools() -> Vec<PoolSpec> {
    load_pools(platform_pools_path()).expect("the one-file-per-pool bundle parses")
}

/// Every `model:<class>` declaration in the bundle, one line each:
/// `kind/step model:<class>`.
fn model_steps(bundle: &[WorkflowSpec]) -> BTreeSet<String> {
    bundle
        .iter()
        .flat_map(|w| {
            w.steps.iter().filter_map(move |s| {
                match ExecutorDemand::parse(s.executor.as_deref()?) {
                    Ok(Some(demand @ ExecutorDemand::Model(_))) => {
                        Some(format!("{}/{} {}", w.kind, s.title, demand.declared()))
                    }
                    _ => None,
                }
            })
        })
        .collect()
}

fn decided_model_steps() -> BTreeSet<String> {
    [
        "backlog-item/build model:opus|sol",
        "ship-a-change/landed-review model:opus|sol",
        "ship-a-change/review model:opus|sol",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

/// The steps demanding a class that no ACTIVE `robots:<class>` pool
/// declares, named `kind/step model:<class>`.
fn uncarried(bundle: &[WorkflowSpec], pools: &[PoolSpec]) -> Vec<String> {
    let declared: BTreeSet<&str> = pools
        .iter()
        .filter(|p| p.status == WorkflowStatus::Active)
        .filter_map(|p| p.name.strip_prefix("robots:"))
        .collect();
    model_steps(bundle)
        .into_iter()
        .filter(|line| {
            // A set (`model:opus|sol`) is carried only when every class is.
            line.rsplit_once("model:")
                .is_none_or(|(_, set)| set.split('|').any(|class| !declared.contains(class)))
        })
        .collect()
}

#[test]
fn the_steps_executed_by_model_class_are_the_decided_ones() {
    // Both classes since David 2026-11-08 (backlog b89af803); `model:opus`
    // alone from 2026-11-05 to then (the header's measurement).
    assert_eq!(model_steps(&bundle()), decided_model_steps());

    // The set preserves both reviewer families; a one-family edit is
    // a policy change, not a spelling change.
    let ship = bundle()
        .into_iter()
        .find(|w| w.kind == "ship-a-change")
        .expect("ship-a-change is authored");
    for slug in ["review", "landed-review"] {
        let step = ship
            .steps
            .iter()
            .find(|s| s.title == slug)
            .unwrap_or_else(|| panic!("ship-a-change/{slug} is authored"));
        assert_eq!(
            step.executor.as_deref(),
            Some("model:opus|sol"),
            "ship-a-change/{slug}"
        );
    }
}

#[test]
fn every_model_class_a_step_demands_is_declared_by_a_model_slot_pool() {
    let bundle = bundle();
    let pools = pools();
    assert_eq!(uncarried(&bundle, &pools), Vec::<String>::new());

    // The control: the same bundle against the pools minus ONE of the
    // classes the three steps demand names each step — a set is carried
    // only when every class is, so the pin bites on either.
    for missing in ["robots:opus", "robots:sol"] {
        let without: Vec<PoolSpec> = pools
            .iter()
            .filter(|p| p.name != missing)
            .cloned()
            .collect();
        assert_eq!(
            uncarried(&bundle, &without),
            decided_model_steps().into_iter().collect::<Vec<_>>(),
            "without {missing}"
        );
    }

    // Each model family a registered robot runs has a class to record
    // (the agents batch door refuses a class no active pool declares):
    // `opus` for agent-claude and agent-claude-b, which run opus-5-5,
    // and `sol` for agent-codex, which runs gpt-6.1-sol. The rows that
    // record them are the tenant's (seeds/agents.toml), not this tree's.
    for class in ["opus", "sol"] {
        assert!(
            pools
                .iter()
                .any(|p| p.name == format!("robots:{class}") && p.status == WorkflowStatus::Active),
            "no active pool robots:{class}"
        );
    }
}

/// The build step admits EVERY model class a robot in the estate can
/// record (David, 2026-11-08 ~20:55Z, backlog b89af803: "Yes, let
/// Codex build deletion-only cars. We definitely have some arbitrary
/// limitations in some of our protocols that we need to improve on,
/// and this is a good example."). The set is derived, not spelled: a
/// robot's `model_class` is admitted by the agents batch door only when
/// an active `robots:<class>` pool declares it, so the active robot
/// pools in the tree ARE the classes a robot can record — on 2026-11-08
/// GET /api/agents read back opus for agent-claude and agent-claude-b
/// and sol for agent-codex, one each. A class the pools gain that this
/// step does not admit is the next Codex idle while the opus pool runs
/// at its limit (measured 2026-11-08 18:13Z: two deletion builds refused
/// with "Build executor demands model:opus; claimant agent-codex is
/// model class sol"). Who is nominated stays the conductor's judgment.
#[test]
fn the_build_step_admits_every_model_class_a_robot_pool_declares() {
    let backlog = bundle()
        .into_iter()
        .find(|w| w.kind == "backlog-item")
        .expect("backlog-item is authored");
    let demand = demanded_by(&backlog, Some("build"))
        .expect("a well-formed declaration")
        .expect("build names its executor");
    let admitted: BTreeSet<String> = demand
        .model_classes()
        .expect("build is executed by model class")
        .iter()
        .cloned()
        .collect();

    let recordable: BTreeSet<String> = pools()
        .iter()
        .filter(|p| p.status == WorkflowStatus::Active)
        .filter_map(|p| p.name.strip_prefix("robots:"))
        .map(str::to_string)
        .collect();
    assert!(
        recordable.len() >= 2,
        "the control: fewer than two robot pools ({recordable:?}) and this pin would pass on a \
         one-class declaration"
    );
    assert_eq!(
        admitted, recordable,
        "backlog-item/build admits {admitted:?}; the active robots:<class> pools declare \
         {recordable:?}"
    );
}

/// The claim half, over the bundle's own row: the build step admits
/// every robot whose agents row records a class the row names — opus
/// for the two Claudes, sol for Codex — and refuses a row recording
/// none, which is every robot until the tenant's rows declare their
/// classes, and why this row's publish waits on the live read-back.
#[test]
fn the_build_step_is_claimed_only_by_a_robot_recording_its_class() {
    let backlog = bundle()
        .into_iter()
        .find(|w| w.kind == "backlog-item")
        .expect("backlog-item is authored");
    let demand = demanded_by(&backlog, Some("build"))
        .expect("a well-formed declaration")
        .expect("build names its executor");
    assert_eq!(
        demand,
        ExecutorDemand::Model(vec!["opus".into(), "sol".into()])
    );

    let judged = |holder: &str, class: Option<&str>| {
        judge_capability(
            &demand,
            holder,
            &Capability::Model {
                class: class.map(str::to_string),
                known: true,
            },
        )
    };
    assert_eq!(judged("agent-claude", Some("opus")), Ok(()));
    assert_eq!(judged("agent-claude-b", Some("opus")), Ok(()));
    // Codex's sol since 2026-11-08 (backlog b89af803) — the refusal the
    // v19 row measured on two deletion items that day no longer fires.
    assert_eq!(judged("agent-codex", Some("sol")), Ok(()));
    // A class no pool declares is still refused, so the set is a set
    // and not "any robot".
    let other = judged("agent-other", Some("haiku")).unwrap_err();
    assert!(other.found.contains("runs model class haiku"), "{other:?}");
    let classless = judged("agent-claude", None).unwrap_err();
    assert!(
        classless.found.contains("records no model class"),
        "{classless:?}"
    );
}

/// The authored review steps must carry the same two-family demand the
/// claim door enforces; a nomination cannot repair an absent declaration.
#[test]
fn both_review_steps_admit_the_declared_model_classes_and_refuse_others() {
    let ship = bundle()
        .into_iter()
        .find(|w| w.kind == "ship-a-change")
        .expect("ship-a-change is authored");
    for slug in ["review", "landed-review"] {
        let demand = demanded_by(&ship, Some(slug))
            .expect("the authored declaration parses")
            .unwrap_or_else(|| panic!("ship-a-change/{slug} must declare its review executors"));
        assert_eq!(
            demand,
            ExecutorDemand::Model(vec!["opus".into(), "sol".into()]),
            "ship-a-change/{slug}"
        );
        for (holder, class) in [
            ("agent-claude", "opus"),
            ("agent-claude-b", "opus"),
            ("agent-codex", "sol"),
        ] {
            assert_eq!(
                judge_capability(
                    &demand,
                    holder,
                    &Capability::Model {
                        class: Some(class.into()),
                        known: true
                    },
                ),
                Ok(()),
                "ship-a-change/{slug}: {holder}"
            );
        }
        for capability in [
            Capability::Model {
                class: Some("haiku".into()),
                known: true,
            },
            Capability::Model {
                class: Some("sol".into()),
                known: false,
            },
            Capability::Model {
                class: None,
                known: true,
            },
        ] {
            assert!(
                judge_capability(&demand, "agent-other", &capability).is_err(),
                "{slug}: {capability:?}"
            );
        }
    }
}
