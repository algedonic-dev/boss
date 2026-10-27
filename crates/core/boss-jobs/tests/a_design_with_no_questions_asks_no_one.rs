//! A design that asks no question sends no one a decision (backlog
//! ac141c99, David's own report, 2026-10-17).
//!
//! The defect, measured on backlog-item 25022367: its design route
//! filed design 195276f0 with `--no-questions` (`questions = []`,
//! `no_open_questions = "true"`). design-doc skips its own review for
//! such a design and opens `fold` at filing (backlog 5932204e) — but the
//! ITEM's `design-review`, an `answer-question` routed to the
//! platform-admin holder, opened on `steps.draft-design.done` regardless.
//! Its question read "Decide design 195276f0 at /it/design"; the design
//! never reached that queue, because it had nothing to review, so David
//! held a decision with nothing to decide and no way to find out why.
//! agent-claude recorded his answer on the step by hand.
//!
//! THE FIX IS THE ROUTE, AS DATA. `boss design --no-questions --answers
//! <item>` completes the item's `draft-design` with `disposition =
//! "recorded"` beside the `design_id`; the item's `design-review` reads
//! the negative of it (the same shape the `duplicate` exit took,
//! 2d3cbeb2), and `build` reads it positively. So a design that records
//! its decisions is decided by its author at the moment design-doc
//! itself treats as the decision — filing, where its review is skipped
//! and its fold opens — and the build goes ahead as it decides, the
//! frame David set for exactly this case (backlog 4f71e608: "file with
//! --no-questions — a doc that records decisions rather than asking for
//! them — and David corrects any of them later through the ordinary
//! cheap-change path").
//!
//! END TO END, over the bundle the seed loads: the design-doc packet
//! as filed, the answered packet of BOTH kinds that carry a design
//! route (backlog-item and user-feedback), the draft's completion
//! through the declared field contract, and the station that renders a
//! person's design queue. A fixture copy of a spec would keep passing
//! while the shipped kind stayed broken.

use boss_core::job::{Job, Priority, Step, StepId, StepStatus, Subject};
use boss_jobs::registry::{WorkflowSpec, materialize_steps, platform_bundle_path, reevaluate};
use boss_jobs::seed_loader::load_workflows;
use boss_jobs::step_registry::StepRegistry;
use serde_json::json;

const DESIGN: &str = "195276f0-a14b-4c17-b236-736a89ee7df5";

fn bundled(kind: &str) -> WorkflowSpec {
    load_workflows(platform_bundle_path())
        .expect("the platform bundle parses")
        .into_iter()
        .find(|w| w.kind == kind)
        .unwrap_or_else(|| panic!("{kind} ships in the platform bundle"))
}

fn day() -> chrono::NaiveDate {
    chrono::NaiveDate::from_ymd_opt(2026, 10, 17).unwrap()
}

fn step_mut<'a>(steps: &'a mut [Step], slug: &str) -> &'a mut Step {
    steps
        .iter_mut()
        .find(|s| s.spec_slug.as_deref() == Some(slug))
        .unwrap_or_else(|| panic!("no step `{slug}`"))
}

fn status(steps: &[Step], slug: &str) -> StepStatus {
    steps
        .iter()
        .find(|s| s.spec_slug.as_deref() == Some(slug))
        .unwrap_or_else(|| panic!("no step `{slug}`"))
        .status
}

fn open(steps: &[Step], slug: &str) -> bool {
    matches!(status(steps, slug), StepStatus::Ready | StepStatus::Active)
}

/// Complete `slug` with `keys` laid over its metadata — judged by the
/// step's declared field contract first, as the completion door does.
fn complete(
    spec: &WorkflowSpec,
    job: &Job,
    steps: &mut [Step],
    slug: &str,
    keys: serde_json::Value,
) -> Result<(), String> {
    let step = step_mut(steps, slug);
    for (k, v) in keys.as_object().cloned().unwrap_or_default() {
        step.metadata[k] = v;
    }
    StepRegistry::validate_authored_fields(&step.fields, &step.metadata)
        .map_err(|e| format!("{slug}: {e:?}"))?;
    step.status = StepStatus::Completed;
    reevaluate(spec, steps, &job.subject, &job.metadata);
    Ok(())
}

/// The design 195276f0 was: filed with no question, answering `item`.
fn recorded_design(item: &str) -> (WorkflowSpec, Job, Vec<Step>) {
    let spec = bundled("design-doc");
    let mut job = Job::new(
        "design-doc",
        Subject::new("custom", "design"),
        "Bound agent evidence growth with recoverable owner-aware archival",
        "agent-codex",
        Priority::Standard,
        day(),
    );
    job.status = boss_core::job::JobStatus::Open;
    job.metadata = json!({
        "title": "Bound agent evidence growth with recoverable owner-aware archival",
        "markdown": "Decisions\n\nInventory and dry run first.",
        "questions": [],
        "no_open_questions": "true",
        "answers": item,
    });
    let mut steps = materialize_steps(&spec, &job.subject, job.id, &job.metadata, StepId::new);
    step_mut(&mut steps, "drafted").status = StepStatus::Completed;
    reevaluate(&spec, &mut steps, &job.subject, &job.metadata);
    (spec, job, steps)
}

/// A packet of `kind` routed to `design`, standing at its open draft.
fn routed_to_design(kind: &str) -> (WorkflowSpec, Job, Vec<Step>) {
    let spec = bundled(kind);
    let mut job = Job::new(
        kind,
        Subject::new("custom", "bosspipeline"),
        "Bound agent session and evidence storage",
        "emp-david",
        Priority::Standard,
        day(),
    );
    job.status = boss_core::job::JobStatus::Open;
    job.metadata = json!({ "message": "the store grows", "area": "platform" });
    let mut steps = materialize_steps(&spec, &job.subject, job.id, &job.metadata, StepId::new);
    let first = spec.steps[0].title.clone();
    step_mut(&mut steps, &first).status = StepStatus::Completed;
    reevaluate(&spec, &mut steps, &job.subject, &job.metadata);
    complete(
        &spec,
        &job,
        &mut steps,
        "triage",
        json!({ "disposition": "design", "evidence": "a decision, not a fix" }),
    )
    .expect("triage routes to design");
    assert!(
        open(&steps, "draft-design"),
        "{kind}: the design route opens the draft"
    );
    (spec, job, steps)
}

/// THE DEFECT, END TO END. The design asks nothing, so neither packet
/// puts a decision in front of a person: design-doc's own review stays
/// unperformed and its fold opens, and the answered packet's draft —
/// completed the way `boss design --no-questions --answers` completes
/// it — skips the review and opens the build.
#[test]
fn a_design_with_no_questions_routes_no_decision_to_a_person() {
    for kind in ["backlog-item", "user-feedback"] {
        let (_, design, dsteps) = recorded_design("item");
        assert!(
            !open(&dsteps, "review"),
            "design-doc performs no review of a design that asks nothing"
        );
        assert!(open(&dsteps, "fold"), "its author's fold is what opens");

        let (spec, job, mut steps) = routed_to_design(kind);
        complete(
            &spec,
            &job,
            &mut steps,
            "draft-design",
            json!({ "design_id": DESIGN, "disposition": "recorded" }),
        )
        .unwrap_or_else(|e| panic!("{kind}: the draft declares `recorded`: {e}"));

        assert!(
            !open(&steps, "design-review"),
            "{kind}: no `Decide the design` for a design with nothing to decide — it is `{:?}`",
            status(&steps, "design-review")
        );
        assert!(
            open(&steps, "build"),
            "{kind}: the build goes ahead as the design decides — it is `{:?}`",
            status(&steps, "build")
        );
        // No step a person decides is open on either packet.
        for (which, s) in [("design", &dsteps), (kind, &steps)] {
            let deciding: Vec<_> = s
                .iter()
                .filter(|st| {
                    matches!(st.status, StepStatus::Ready | StepStatus::Active)
                        && matches!(st.kind.as_str(), "answer-question" | "review-design")
                })
                .filter_map(|st| st.spec_slug.clone())
                .collect();
            assert!(
                deciding.is_empty(),
                "{which}: a decision opened for a person: {deciding:?}"
            );
        }
        // And the station a person's design queue is rendered from
        // holds neither.
        let station = boss_jobs::seed_loader::load_stations(
            boss_jobs::station_seed::platform_stations_path(),
        )
        .unwrap()
        .into_iter()
        .find(|s| s.name == "design-review")
        .unwrap();
        let queue = boss_jobs::evaluate_station(
            &station,
            vec![
                (design.clone(), dsteps.clone()),
                (job.clone(), steps.clone()),
            ],
            day(),
        );
        assert_eq!(queue.total, 0, "{kind}: nothing reaches the design queue");
    }
}

/// THE REGRESSION THIS MUST NOT CAUSE. A design with questions is filed
/// with no disposition (`boss design` writes none) or `filed`; either
/// way the founder's review opens and the build waits on its verdict.
#[test]
fn a_design_with_questions_still_opens_the_review() {
    for kind in ["backlog-item", "user-feedback"] {
        for keys in [
            json!({ "design_id": DESIGN }),
            json!({ "design_id": DESIGN, "disposition": "filed" }),
        ] {
            let (spec, job, mut steps) = routed_to_design(kind);
            complete(&spec, &job, &mut steps, "draft-design", keys.clone())
                .unwrap_or_else(|e| panic!("{kind} {keys}: {e}"));
            assert!(
                open(&steps, "design-review"),
                "{kind} {keys}: the review opens — it is `{:?}`",
                status(&steps, "design-review")
            );
            assert_eq!(
                status(&steps, "build"),
                StepStatus::Pending,
                "{kind} {keys}: nothing is built before the verdict"
            );
        }
    }
}

/// The value is ENUM-CHECKED: a misspelt `recorded` accepted in silence
/// would read as an ordinary draft and open the very review it exists
/// to refuse.
#[test]
fn a_misspelt_recorded_is_refused() {
    for kind in ["backlog-item", "user-feedback"] {
        let (spec, job, mut steps) = routed_to_design(kind);
        let refused = complete(
            &spec,
            &job,
            &mut steps,
            "draft-design",
            json!({ "design_id": DESIGN, "disposition": "recordd" }),
        );
        assert!(refused.is_err(), "{kind}: `recordd` must be refused");
    }
}
