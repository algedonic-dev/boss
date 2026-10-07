//! The design-doc bundle's `review` step cannot complete with a
//! question unanswered (backlog 0ef658e6).
//!
//! Measured 2026-09-11: four design packets sat at `fold` with their
//! review COMPLETED and zero resolutions against 2, 3, 3 and 4 open
//! questions — twelve judgements recorded nowhere, and a fold with
//! nothing to fold from. `resolutions` was not a declared field: the
//! review surface wrote it, nothing required it, and nothing related
//! it to `questions`. The bundle now declares it — required at done,
//! elements `{anchor, decision}`, and `covers = "questions"` — and this
//! pins that contract THROUGH the bundle the seed loads and the
//! validator the step PUT runs, not through a hand-built spec.

use boss_core::job::{Job, Priority, StepId, StepStatus, Subject};
use boss_jobs::registry::{
    StepSpec, WorkflowSpec, materialize_steps, platform_bundle_path, reevaluate,
};
use boss_jobs::seed_loader::load_workflows;
use boss_jobs::step_registry::StepRegistry;
use serde_json::json;

fn bundled(kind: &str) -> WorkflowSpec {
    load_workflows(platform_bundle_path())
        .expect("the platform bundle parses")
        .into_iter()
        .find(|w| w.kind == kind)
        .unwrap_or_else(|| panic!("{kind} ships in the platform bundle"))
}

fn step<'a>(spec: &'a WorkflowSpec, title: &str) -> &'a StepSpec {
    spec.steps
        .iter()
        .find(|s| s.title == title)
        .unwrap_or_else(|| panic!("design-doc has a `{title}` step"))
}

fn questions() -> serde_json::Value {
    json!([
        {"anchor": "host", "title": "Which host?", "proposal": "forge"},
        {"anchor": "credentials", "title": "Where do credentials live?", "proposal": "/etc/boss-ops"},
        {"anchor": "acts", "title": "Which acts become verbs?", "proposal": "reads now"},
    ])
}

fn routed(
    flag: Option<serde_json::Value>,
    questions: serde_json::Value,
) -> (WorkflowSpec, Job, Vec<boss_core::job::Step>) {
    let spec = bundled("design-doc");
    let mut job = Job::new(
        "design-doc",
        Subject::new("custom", "design-routing"),
        "A design",
        "agent-codex",
        Priority::Standard,
        chrono::NaiveDate::from_ymd_opt(2026, 10, 2).unwrap(),
    );
    job.status = boss_core::job::JobStatus::Open;
    job.metadata =
        json!({"title": "A design", "markdown": "A recorded decision", "questions": questions});
    if let Some(flag) = flag {
        job.metadata["no_open_questions"] = flag;
    }
    let mut steps = materialize_steps(&spec, &job.subject, job.id, &job.metadata, StepId::new);
    assert!(
        boss_jobs::registry::missing_filer_fields(&steps).is_empty(),
        "fixture satisfies real admission fields"
    );
    assert_eq!(
        steps
            .iter()
            .find(|s| s.spec_slug.as_deref() == Some("fold"))
            .unwrap()
            .status,
        StepStatus::Pending,
        "fold never opens before drafting"
    );
    steps
        .iter_mut()
        .find(|s| s.spec_slug.as_deref() == Some("drafted"))
        .unwrap()
        .status = StepStatus::Completed;
    reevaluate(&spec, &mut steps, &job.subject, &job.metadata);
    (spec, job, steps)
}

fn status(steps: &[boss_core::job::Step], slug: &str) -> StepStatus {
    steps
        .iter()
        .find(|s| s.spec_slug.as_deref() == Some(slug))
        .unwrap()
        .status
}

#[test]
fn an_explicit_no_question_design_reaches_fold_without_a_review() {
    let (spec, job, mut steps) = routed(Some(json!("true")), json!([]));
    assert_eq!(
        status(&steps, "review"),
        StepStatus::Pending,
        "an unperformed review stays unperformed"
    );
    assert_eq!(status(&steps, "fold"), StepStatus::Ready);
    let station =
        boss_jobs::seed_loader::load_stations(boss_jobs::station_seed::platform_stations_path())
            .unwrap()
            .into_iter()
            .find(|s| s.name == "design-review")
            .unwrap();
    let queue = boss_jobs::evaluate_station(
        &station,
        vec![(job.clone(), steps.clone())],
        chrono::NaiveDate::from_ymd_opt(2026, 10, 2).unwrap(),
    );
    assert_eq!(queue.total, 0, "no question reaches the human review queue");
    let fold = steps
        .iter_mut()
        .find(|s| s.spec_slug.as_deref() == Some("fold"))
        .unwrap();
    fold.metadata["fold_change"] = json!("Nothing; the decision is already current truth");
    fold.metadata["folded_into"] = json!("none");
    StepRegistry::validate_authored_fields(&fold.fields, &fold.metadata).unwrap();
    fold.status = StepStatus::Completed;
    reevaluate(&spec, &mut steps, &job.subject, &job.metadata);
    assert_eq!(
        status(&steps, "published"),
        StepStatus::Ready,
        "bypassing an unperformed review still reaches settlement"
    );
}

#[test]
fn questions_always_require_review_and_complete_resolutions_before_fold() {
    for flag in [
        None,
        Some(json!("false")),
        Some(json!(true)),
        Some(json!("true")),
    ] {
        let (spec, job, mut steps) = routed(flag, questions());
        assert_eq!(status(&steps, "review"), StepStatus::Ready);
        assert_eq!(
            status(&steps, "fold"),
            StepStatus::Pending,
            "a bypass flag cannot conceal questions"
        );
        let station = boss_jobs::seed_loader::load_stations(
            boss_jobs::station_seed::platform_stations_path(),
        )
        .unwrap()
        .into_iter()
        .find(|s| s.name == "design-review")
        .unwrap();
        assert_eq!(
            boss_jobs::evaluate_station(
                &station,
                vec![(job.clone(), steps.clone())],
                chrono::NaiveDate::from_ymd_opt(2026, 10, 2).unwrap()
            )
            .total,
            1,
            "questions remain in the human review queue"
        );
        let review = steps
            .iter_mut()
            .find(|s| s.spec_slug.as_deref() == Some("review"))
            .unwrap();
        review.metadata["doc_path"] = json!("");
        let errors =
            StepRegistry::validate_authored_fields(&review.fields, &review.metadata).unwrap_err();
        assert!(
            errors.iter().any(|error| error.field == "resolutions"),
            "unresolved questions refuse review completion: {errors:?}"
        );
        review.metadata["resolutions"] = json!([
            {"anchor": "host", "decision": "forge"},
            {"anchor": "credentials", "decision": "/etc/boss-ops"},
            {"anchor": "acts", "decision": "reads now"}
        ]);
        StepRegistry::validate_authored_fields(&review.fields, &review.metadata).unwrap();
        review.status = StepStatus::Completed;
        reevaluate(&spec, &mut steps, &job.subject, &job.metadata);
        assert_eq!(status(&steps, "fold"), StepStatus::Ready);
    }
}

#[test]
fn only_the_explicit_string_flag_bypasses_even_an_empty_question_list() {
    for flag in [None, Some(json!("false")), Some(json!(true))] {
        let (_, _, steps) = routed(flag, json!([]));
        assert_eq!(status(&steps, "review"), StepStatus::Ready);
        assert_eq!(status(&steps, "fold"), StepStatus::Pending);
    }
}

#[tokio::test]
async fn publication_supersedes_live_v7_and_keeps_its_old_routing_pinned() {
    use boss_jobs::registry::{InMemoryWorkflows, WorkflowRegistry};
    let registry = InMemoryWorkflows::for_fixture();
    let mut old = bundled("design-doc");
    old.version = 7;
    old.steps
        .iter_mut()
        .find(|s| s.title == "review")
        .unwrap()
        .ready_when = "steps.drafted.done".into();
    old.steps
        .iter_mut()
        .find(|s| s.title == "fold")
        .unwrap()
        .ready_when = "steps.review.done".into();
    registry.seed(old.clone()).unwrap();
    let actor = boss_core::actor::ActorId::Automation("platform-workflow-seed".into());
    let published = registry
        .publish_authored(
            bundled("design-doc"),
            boss_core::job::JobId::new(),
            &actor,
            chrono::DateTime::UNIX_EPOCH,
        )
        .await
        .unwrap();
    assert_eq!(
        published.version, 8,
        "publication allocates above the actual live version"
    );
    assert_ne!(
        step(&published, "review").ready_when,
        step(&old, "review").ready_when
    );
    assert_eq!(
        step(
            &registry.get_version("design-doc", 7).await.unwrap(),
            "review"
        )
        .ready_when,
        "steps.drafted.done",
        "old packets retain the review contract admitted under"
    );
}

#[test]
fn the_bundle_declares_resolutions_covering_questions() {
    let spec = bundled("design-doc");
    let review = step(&spec, "review");
    let res = review
        .fields
        .iter()
        .find(|f| f.name == "resolutions")
        .expect("review declares `resolutions`");
    assert!(res.required, "required at done");
    assert_eq!(res.field_type, "array");
    assert_eq!(
        res.item_keys,
        vec!["anchor".to_string(), "decision".to_string()]
    );
    assert_eq!(res.covers.as_deref(), Some("questions"));
    // The workflow lint accepts the shape it ships.
    let errs = boss_jobs::workflow_lint::validate_workflow(&spec, &StepRegistry::v1());
    assert!(
        errs.is_empty(),
        "the bundled design-doc lints clean: {errs:?}"
    );
}

#[test]
fn a_review_with_a_question_unanswered_is_refused_naming_the_anchor() {
    let spec = bundled("design-doc");
    let review = step(&spec, "review");
    // Zero resolutions — the 2026-09-11 shape.
    let err = StepRegistry::validate_authored_fields(
        &review.fields,
        &json!({ "title": "t", "markdown": "m", "questions": questions(), "resolutions": [] }),
    )
    .unwrap_err();
    let msg = err
        .iter()
        .find(|e| e.field == "resolutions")
        .map(|e| e.message.clone())
        .unwrap_or_default();
    assert!(
        msg.contains("host") && msg.contains("credentials") && msg.contains("acts"),
        "every open anchor is named: {msg}"
    );
    // The field absent entirely — the surface never wrote it.
    let err = StepRegistry::validate_authored_fields(
        &review.fields,
        &json!({ "title": "t", "markdown": "m", "questions": questions() }),
    )
    .unwrap_err();
    assert!(err.iter().any(|e| e.field == "resolutions"), "{err:?}");
    // Two of three answered: the third is named, the two are not.
    let err = StepRegistry::validate_authored_fields(
        &review.fields,
        &json!({ "title": "t", "markdown": "m", "questions": questions(), "resolutions": [
            {"anchor": "host", "decision": "forge now"},
            {"anchor": "credentials", "decision": "/etc/boss-ops root 0600"}
        ]}),
    )
    .unwrap_err();
    let msg = err
        .iter()
        .find(|e| e.field == "resolutions")
        .unwrap()
        .message
        .clone();
    assert!(msg.contains("acts") && !msg.contains("host"), "{msg}");
    // All three: the review may complete. `doc_path` is the kind's
    // required field, declared on the step since backlog a14f04b3 —
    // `boss design` writes it (empty for a packet-borne design).
    StepRegistry::validate_authored_fields(
        &review.fields,
        &json!({ "title": "t", "markdown": "m", "doc_path": "", "questions": questions(), "resolutions": [
            {"anchor": "host", "decision": "forge now"},
            {"anchor": "credentials", "decision": "/etc/boss-ops root 0600"},
            {"anchor": "acts", "decision": "reads now; node-converge bounded"}
        ]}),
    )
    .expect("every question answered completes");
}
