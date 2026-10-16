use serde_json::json;
use uuid::Uuid;
#[path = "yard_move_corrections/fixture.rs"]
mod fixture;

#[tokio::test]
async fn a_committed_correction_retries_its_original_receipt_after_unrelated_annotation() {
    use boss_jobs::{move_diagnosis::Diagnosis, port::JobsRepository};
    let (app, jobs) = fixture::fixture_and_jobs().await;
    let (_, body) = fixture::post(
        app.clone(),
        "/api/yard/moves/1/diagnoses",
        json!({"request_id":Uuid::new_v4()}),
    )
    .await;
    let diagnosis: Diagnosis = serde_json::from_value(body).unwrap();
    let id = fixture::install_proof(&jobs, &diagnosis).await;
    let request =
        json!({"request_id":Uuid::new_v4(),"diagnosis_id":diagnosis.id,"proof_request":id});
    let (status, original) = fixture::post(
        app.clone(),
        "/api/yard/moves/1/corrections",
        request.clone(),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::CREATED, "{original}");
    let job_id = boss_core::job::JobId::from_uuid(id);
    let mut job = jobs.get_job(&job_id).await.unwrap().unwrap();
    job.metadata["review_note"] = json!("annotation after successful execution");
    jobs.update_job(&job).await.unwrap();
    let (status, retry) = fixture::post(app, "/api/yard/moves/1/corrections", request).await;
    assert_eq!(status, axum::http::StatusCode::OK, "{retry}");
    assert_eq!(original, retry);
}

#[test]
fn only_the_existing_credentialed_runner_may_write_execution_evidence() {
    use boss_jobs::field_writer::{CredentialedCaller, refused, reserved_keys_changed};
    let spec = boss_jobs::registry::seedable_platform_workflows()
        .into_iter()
        .find(|w| w.kind == "ops-request")
        .unwrap();
    let execute = spec.steps.iter().find(|s| s.title == "execute").unwrap();
    let changes = reserved_keys_changed(
        &execute.fields,
        &json!({}),
        &json!({"disposition":"answered","exit_code":"0","output":"forged"}),
    );
    assert_eq!(changes.len(), 3);
    assert!(!refused(&changes, None, Some("forge")).is_empty());
    let own = CredentialedCaller {
        principal: "runner:ops".into(),
        actor_id: "automation:ops-runner".into(),
        host: Some("forge".into()),
    };
    assert!(refused(&changes, Some(&own), Some("forge")).is_empty());
    assert!(!refused(&changes, Some(&own), Some("boss-gcp")).is_empty());
}

#[tokio::test]
async fn protected_execution_positive_and_refusal_matrix() {
    use boss_jobs::move_diagnosis::{self, Diagnosis};
    let (_, body) = fixture::post(
        fixture::fixture().await,
        "/api/yard/moves/1/diagnoses",
        json!({"request_id":Uuid::new_v4()}),
    )
    .await;
    let diagnosis: Diagnosis = serde_json::from_value(body).unwrap();
    let proof = fixture::machine_proof(&diagnosis);
    move_diagnosis::proof::validate(&proof, &diagnosis).unwrap();
    for variant in 0..15 {
        let mut altered = proof.clone();
        match variant {
            0 => altered.execute.status = boss_core::job::StepStatus::Active,
            1 => altered.execute.metadata["exit_code"] = json!("1"),
            2 => altered.execute.metadata["disposition"] = json!("refused"),
            3 => {
                altered
                    .execute
                    .fields
                    .iter_mut()
                    .find(|f| f.name == "output")
                    .unwrap()
                    .writer = None
            }
            4 => {
                altered
                    .protocol
                    .steps
                    .iter_mut()
                    .find(|s| s.title == "execute")
                    .unwrap()
                    .executor = None
            }
            5 => altered.request.metadata["args"] = json!([Uuid::new_v4()]),
            6 => {
                altered.execute.metadata["output"] = json!(format!(
                    "{}\n{}",
                    altered.execute.metadata["output"].as_str().unwrap(),
                    altered.execute.metadata["output"].as_str().unwrap()
                ))
            }
            7 => {
                altered.execute.metadata["output"] =
                    json!("[ops-runner: output truncated — kept 1 of 2 bytes]")
            }
            8 => altered.proven.metadata["proof"]["overridden"] = json!("test"),
            9 => altered.request.opened_at = Some(altered.execute.completed_at.unwrap()),
            10 => altered.proven.metadata["proof"]["tree_head"] = json!("unknown"),
            11 => altered.proven.metadata["proof"]["stdout"] = json!("{}"),
            12 => altered.request.metadata["verb"] = json!("echo-caller-input"),
            13 => {
                altered.execute.completed_by =
                    Some(boss_core::actor::ActorId::human("forged-runner"))
            }
            _ => {
                altered.artifact.id = boss_core::job::JobId::new();
                altered.proven.job_id = altered.artifact.id;
                altered.request.metadata["args"] = json!([altered.artifact.id.to_string()]);
            }
        }
        if matches!(variant, 8 | 10 | 11) {
            altered.execute.metadata["output"] = json!(fixture::proof_output(
                &altered.artifact,
                &altered.proven.metadata["proof"]
            ));
        }
        assert!(
            move_diagnosis::proof::validate(&altered, &diagnosis).is_err(),
            "variant {variant}"
        );
    }
    let (app, jobs) = fixture::fixture_and_jobs().await;
    let (_, body) = fixture::post(
        app.clone(),
        "/api/yard/moves/1/diagnoses",
        json!({"request_id":Uuid::new_v4()}),
    )
    .await;
    let diagnosis: Diagnosis = serde_json::from_value(body).unwrap();
    let id = fixture::install_proof(&jobs, &diagnosis).await;
    let (status, body) = fixture::post(
        app,
        "/api/yard/moves/1/corrections",
        json!({"request_id":Uuid::new_v4(),"diagnosis_id":diagnosis.id,"proof_request":id}),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::CREATED, "{body}");
}

#[tokio::test]
async fn exact_structured_bindings_and_legacy_proof_remain_loud_on_replay() {
    use boss_jobs::move_diagnosis::{self, Diagnosis, Entry};
    let (_, body) = fixture::post(
        fixture::fixture().await,
        "/api/yard/moves/1/diagnoses",
        json!({"request_id":Uuid::new_v4()}),
    )
    .await;
    let diagnosis: Diagnosis = serde_json::from_value(body).unwrap();
    let proof = fixture::machine_proof(&diagnosis);
    for key in [
        "diagnosis_id",
        "original_seq",
        "original_hash",
        "packet",
        "cause_event_id",
        "cause_seq",
        "model",
        "artifact_head",
        "result",
    ] {
        let mut bad = proof.clone();
        let mut observed: serde_json::Value =
            serde_json::from_str(bad.proven.metadata["proof"]["stdout"].as_str().unwrap()).unwrap();
        observed[key] = json!("wrong");
        bad.proven.metadata["proof"]["stdout"] = json!(observed.to_string());
        bad.execute.metadata["output"] = json!(fixture::proof_output(
            &bad.artifact,
            &bad.proven.metadata["proof"]
        ));
        assert!(
            move_diagnosis::proof::validate(&bad, &diagnosis).is_err(),
            "{key}"
        );
    }
    let original = diagnosis.original.clone();
    let d = Entry::Diagnosis {
        receipt: Box::new(diagnosis.clone()),
    };
    let legacy = Entry::Correction {
        id: Uuid::new_v4(),
        original_seq: original.seq,
        diagnosis_id: diagnosis.id,
        original_hash: diagnosis.original_hash.clone(),
        proof: None,
    };
    assert!(move_diagnosis::append(std::slice::from_ref(&d), &legacy, &original).is_err());
    let entries = vec![d, legacy];
    assert!(move_diagnosis::active(&entries, &original).is_err());
    let mut counts = move_diagnosis::Conservation::default();
    counts.observe(&entries, &original);
    assert_eq!(counts.raw, 1);
    assert_eq!(counts.unknown, 1);
    assert_eq!(counts.superseded, 0);
}

#[tokio::test]
async fn absent_independent_completed_machine_proof_cannot_admit_a_correction() {
    let app = fixture::fixture().await;
    let (status, body) = fixture::post(
        app.clone(),
        "/api/yard/moves/1/diagnoses",
        json!({"request_id":Uuid::new_v4()}),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::CREATED);
    assert_eq!(body["status"], "eligible");
    let (status, body) = fixture::post(
        app,
        "/api/yard/moves/1/corrections",
        json!({"request_id":Uuid::new_v4(),"diagnosis_id":body["id"]}),
    )
    .await;
    assert_ne!(
        status,
        axum::http::StatusCode::CREATED,
        "no independently completed proof/artifact exists in fixture: {body}"
    );
}

#[tokio::test]
async fn original_registration_is_required_and_unrelated_unsupported_routes_do_not_clear() {
    use boss_jobs::move_diagnosis::{self, Diagnosis, Status};
    let (status, body) = fixture::post(
        fixture::fixture().await,
        "/api/yard/moves/1/diagnoses",
        json!({"request_id":Uuid::new_v4()}),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::CREATED);
    let receipt: Diagnosis = serde_json::from_value(body).unwrap();
    assert_eq!(receipt.status, Status::Eligible);
    assert!(move_diagnosis::validated(&receipt));
    for variant in 0..5 {
        let mut input = receipt.inputs.clone();
        let mut original = receipt.original.clone();
        match variant {
            0 => {
                let row = input
                    .run
                    .iter_mut()
                    .find(|r| r.event.kind == boss_jobs::events::JOB_CREATED)
                    .unwrap();
                row.event.payload["metadata"]["profile"] = json!("forged-profile");
            }
            1 => {
                for row in &mut input.parent {
                    if row.event.payload.get("spec_slug").and_then(|v| v.as_str())
                        == Some("landed-review")
                    {
                        row.event.payload["metadata"]["agent_run"] = json!(Uuid::new_v4());
                    }
                }
            }
            2 => original.r#move.to = Some("publish".into()),
            3 => original.r#move.from = Some("garage".into()),
            _ => input.station_history[0].seq = original.r#move.cause_seq + 1,
        }
        let result = move_diagnosis::diagnose(receipt.id, original, input).unwrap();
        assert_ne!(
            result.status,
            Status::Eligible,
            "variant {variant}: {}",
            result.why
        );
        assert!(!move_diagnosis::validated(&result));
    }
}
