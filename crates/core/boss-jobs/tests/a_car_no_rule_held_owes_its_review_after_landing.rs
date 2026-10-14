//! A CAR NO RULE HELD OWES ITS REVIEW AFTER IT LANDS (backlog bd331d38;
//! David, 2026-10-09: "Go with option A for the review hold").
//!
//! A parked car waits for an independent review before it boards only
//! when mutating_verb::judge finds its diff or its item carries a
//! trust_boundary mark (the gate's half, pinned in boss-cli's
//! `mutating_verb` tests). Every other car boards on its green — and
//! its review must then be OWED, on the record and countable, or the
//! decision trades a slow dock for silence. This pins the protocol's
//! half against the REAL platform bundle: ship-a-change's
//! `landed-review` step, what readies it, that `merged` waits for it,
//! and the `landed-review` station that counts the debt.

use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject};
use boss_jobs::WorkflowSpec;
use boss_jobs::registry::{materialize_steps, reevaluate, seedable_platform_workflows};
use boss_jobs::seed_loader::load_stations;
use boss_jobs::station_seed::platform_stations_path;
use chrono::NaiveDate;
use serde_json::{Value, json};

const REVIEWER: &str = "5eed5eed-0000-4000-8000-000000000000";

fn ship_a_change() -> WorkflowSpec {
    seedable_platform_workflows()
        .into_iter()
        .find(|s| s.kind == "ship-a-change")
        .expect("ship-a-change is in the platform bundle")
}

fn idx(spec: &WorkflowSpec, slug: &str) -> usize {
    spec.steps
        .iter()
        .position(|s| s.title == slug)
        .unwrap_or_else(|| panic!("ship-a-change has no step `{slug}`"))
}

/// A car through `review` — boarded — carrying `review_md` on that step
/// (a `release` when a review released it at the dock), landed when
/// `merged`.
fn boarded(spec: &WorkflowSpec, review_md: Value, job_md: &Value) -> Vec<Step> {
    let subject = Subject::new("custom", "fix/a-change");
    let mut steps = materialize_steps(spec, &subject, JobId::new(), job_md, StepId::new);
    for slug in ["opened", "scope", "build", "gate", "review"] {
        steps[idx(spec, slug)].status = StepStatus::Completed;
    }
    let review = &mut steps[idx(spec, "review")].metadata;
    if let (Value::Object(into), Value::Object(from)) = (review, review_md) {
        into.extend(from);
    }
    reevaluate(spec, &mut steps, &subject, job_md);
    steps
}

fn subject() -> Subject {
    Subject::new("custom", "fix/a-change")
}

/// Unheld, landed: `landed-review` is READY, and `merged` does not close
/// on the proof alone — it closes once the review is recorded.
#[test]
fn an_unheld_landed_car_owes_its_review_and_closes_only_after_it() {
    let spec = ship_a_change();
    let landed = json!({ "merged": "true" });

    // Boarded but not yet landed: nothing is owed yet, and nothing is
    // skipped — the step waits for the merge.
    let not_yet = boarded(&spec, json!({ "pr_url": "u" }), &json!({}));
    assert_eq!(
        not_yet[idx(&spec, "landed-review")].status,
        StepStatus::Pending
    );

    let mut steps = boarded(&spec, json!({ "pr_url": "u" }), &landed);
    assert_eq!(
        steps[idx(&spec, "landed-review")].status,
        StepStatus::Ready,
        "a car no rule held owes its review once it lands"
    );
    assert_eq!(steps[idx(&spec, "proven")].status, StepStatus::Ready);

    steps[idx(&spec, "proven")].status = StepStatus::Completed;
    reevaluate(&spec, &mut steps, &subject(), &landed);
    assert_eq!(
        steps[idx(&spec, "merged")].status,
        StepStatus::Pending,
        "the proof alone does not close a car whose review is still owed"
    );
    assert_eq!(
        steps[idx(&spec, "landed-review")].status,
        StepStatus::Ready,
        "and the owed review is not skipped"
    );

    steps[idx(&spec, "landed-review")].status = StepStatus::Completed;
    reevaluate(&spec, &mut steps, &subject(), &landed);
    assert_eq!(steps[idx(&spec, "merged")].status, StepStatus::Ready);
}

/// Released at the dock by a named review: its review is on the record
/// already, so it owes none after landing and closes on its proof.
#[test]
fn a_car_released_by_a_review_owes_no_second_one() {
    let spec = ship_a_change();
    let landed = json!({ "merged": "true" });
    let released = json!({ "pr_url": "u", "release": {
        "sha": "1111111111111111111111111111111111111111", "review": REVIEWER,
        "verdict": "release", "by": "emp-david", "at": "t" } });
    let mut steps = boarded(&spec, released, &landed);
    assert_eq!(
        steps[idx(&spec, "landed-review")].status,
        StepStatus::Pending,
        "reviewed before boarding"
    );
    steps[idx(&spec, "proven")].status = StepStatus::Completed;
    reevaluate(&spec, &mut steps, &subject(), &landed);
    assert_eq!(
        steps[idx(&spec, "merged")].status,
        StepStatus::Ready,
        "closes on its proof, as before"
    );
}

/// A car whose merge main lost owes no review of a change main does not
/// carry; its successor will.
#[test]
fn an_unlanded_car_owes_none() {
    let spec = ship_a_change();
    let lost = json!({ "merged": "true", "unlanded": "true" });
    let steps = boarded(&spec, json!({ "pr_url": "u" }), &lost);
    assert_eq!(
        steps[idx(&spec, "landed-review")].status,
        StepStatus::Pending
    );
}

/// The debt is a queue: the `landed-review` station counts every open
/// car whose review is owed, and no car that owes none.
#[test]
fn the_landed_review_station_counts_the_owed_reviews() {
    let spec = ship_a_change();
    let station = load_stations(platform_stations_path())
        .expect("the platform station bundle parses")
        .into_iter()
        .find(|s| s.name == "landed-review")
        .expect("the bundle declares `landed-review`");
    let day = NaiveDate::from_ymd_opt(2026, 10, 9).expect("a real day");
    let car = |steps: Vec<Step>| {
        let mut job = Job::new(
            "ship-a-change",
            subject(),
            "fix/a-change",
            "agent-claude",
            Priority::Standard,
            day,
        );
        job.status = JobStatus::Open;
        let steps = steps
            .into_iter()
            .map(|mut s| {
                s.job_id = job.id;
                s
            })
            .collect();
        (job, steps)
    };
    let landed = json!({ "merged": "true" });
    let owed = car(boarded(&spec, json!({ "pr_url": "u" }), &landed));
    let released = car(boarded(
        &spec,
        json!({ "pr_url": "u", "release": { "sha": "1", "review": REVIEWER } }),
        &landed,
    ));
    let at_dock = car(boarded(&spec, json!({ "pr_url": "u" }), &json!({})));
    let q = boss_jobs::evaluate_station(&station, vec![owed, released, at_dock], day);
    assert_eq!(q.total, 1, "only the landed car no review has read");
}

/// THE READERS' SPELLING IS THE ROW'S (backlog c851ed24). `boss orient`,
/// MY WORK, `boss dispatch` and `boss step complete` find the owed
/// review through `boss_jobs::car`'s constants; a rename in the protocol
/// or the station file that left them behind would make every one of
/// those readers answer "nothing owed" — the silence the step exists to
/// end.
#[test]
fn the_landed_review_constants_are_the_protocol_and_the_station() {
    use boss_jobs::car::{LANDED_REVIEW, LANDED_REVIEW_SLUG, LANDED_REVIEW_STATION};
    let spec = ship_a_change();
    let step = &spec.steps[idx(&spec, LANDED_REVIEW_SLUG)];
    assert_eq!(step.title_template, LANDED_REVIEW);
    let station = load_stations(platform_stations_path())
        .expect("the platform station bundle parses")
        .into_iter()
        .find(|s| s.name == LANDED_REVIEW_STATION)
        .expect("the bundle declares the landed-review station");
    assert_eq!(
        station
            .predicate
            .step
            .as_ref()
            .and_then(|s| s.slug.as_deref()),
        Some(LANDED_REVIEW_SLUG)
    );
}
