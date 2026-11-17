use super::protocol::*;
use super::*;

/// A close naming an outcome no completion contract protects keeps the
/// semantics it had before this car. The packet is a REGISTERED kind on
/// its pinned version: since train #1206 (car a4e1a4a2) a whole-job PUT
/// on a kind whose pinned protocol cannot be read is refused 503 before
/// any close logic runs, so an unregistered kind no longer reaches the
/// completion contract at all and could not show what this test claims
/// (measured on the merge of origin/main into 73d19940, 2026-10-19).
#[tokio::test]
async fn an_ad_hoc_legacy_close_keeps_its_existing_semantics() {
    let f = Fixture::new().await;
    let spec = boss_jobs::seed_loader::parse_workflows(
        include_str!("../../../../../infra/platform/workflows/ship-a-change.toml"),
        "platform",
        "ship-a-change.toml",
    )
    .unwrap()
    .remove(0);
    let mut job = Job::new(
        "ship-a-change",
        Subject::new("custom", "legacy"),
        "Legacy work",
        "emp-observer",
        Priority::Standard,
        chrono::NaiveDate::from_ymd_opt(2026, 10, 15).unwrap(),
    );
    job.workflow_version = spec.version;
    job.status = boss_core::job::JobStatus::Open;
    f.jobs.create_job(&job).await.unwrap();
    let mut whole = serde_json::to_value(&job).unwrap();
    whole["status"] = json!("closed");
    whole["metadata"] = json!({"outcome":"abandoned"});
    assert_eq!(
        call(&f.app, "PUT", format!("/api/jobs/{}", job.id), whole)
            .await
            .0,
        204
    );
}
#[tokio::test]
async fn complete_content_and_exact_attachment_targets_are_required_at_completion() {
    for fault in [
        "missing",
        "target",
        "digest",
        "bytes",
        "partial",
        "duplicate",
    ] {
        let f = Fixture::with_content_fault(fault).await;
        let original = f.observe().await;
        let _qualification = f.qualification(&original).await;
        assert_eq!(
            call_as(
                &f.app,
                "emp-reviewer",
                "PUT",
                f.path(f.qualify),
                json!({"status":"completed"})
            )
            .await
            .0,
            409,
            "{fault}"
        );
        assert_ne!(
            f.jobs.get_step(&f.qualify).await.unwrap().unwrap().status,
            StepStatus::Completed,
            "{fault}"
        );
    }
}

#[tokio::test]
async fn client_review_fields_cannot_substitute_for_the_immutable_resource_bindings() {
    for fault in [
        "observation_event",
        "observation_digest",
        "reviewer",
        "source_head",
        "asset_head",
        "asset",
        "suite",
        "self-review",
    ] {
        let f = Fixture::new().await;
        let original = f.observe().await;
        let mut value = boss_jobs::browser_qualification::judge(
            &original,
            &packet(&f.jobs, f.observation).await,
            &packet(&f.jobs, f.car).await,
            "emp-reviewer",
            "Reviewed every original.",
        )
        .unwrap();
        let actor = if fault == "self-review" {
            value["reviewer"] = json!("emp-observer");
            "emp-observer"
        } else {
            "emp-reviewer"
        };
        match fault {
            "observation_event" => value[fault] = json!(uuid::Uuid::new_v4()),
            "observation_digest" | "source_head" | "asset_head" => {
                value[fault] = json!("b".repeat(64))
            }
            "reviewer" => value[fault] = json!("emp-another-reviewer"),
            "suite" | "asset" => value[fault]["sha256"] = json!("b".repeat(64)),
            _ => {}
        }
        assert_eq!(
            call_as(
                &f.app,
                actor,
                "POST",
                format!("{}/metadata/records", f.path(f.qualify)),
                json!({"key":"qualification","value":value,"expected_absence":true})
            )
            .await
            .0,
            201
        );
        assert_eq!(
            call_as(
                &f.app,
                actor,
                "PUT",
                f.path(f.qualify),
                json!({"status":"completed"})
            )
            .await
            .0,
            409,
            "{fault}"
        );
    }
}
