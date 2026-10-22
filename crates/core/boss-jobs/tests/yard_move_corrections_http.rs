//! Native-input correction routes and their conservative domain controls.
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use boss_core::event::Event;
use boss_policy_client::types::{AccessTier, User};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;
#[path = "yard_move_corrections/fixture.rs"]
mod fixture;
use fixture::{fixture, post};

async fn clock_fixture(parent_cause: bool) -> boss_jobs::move_diagnosis::Diagnosis {
    let app = if parent_cause {
        fixture::fixture_with_parent_cause().await.0
    } else {
        fixture().await
    };
    let (status, body) = post(
        app,
        "/api/yard/moves/1/diagnoses",
        json!({"request_id":Uuid::new_v4()}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let mut receipt: boss_jobs::move_diagnosis::Diagnosis = serde_json::from_value(body).unwrap();
    let original = &receipt.original.r#move;
    let cause = receipt
        .inputs
        .run
        .iter_mut()
        .chain(&mut receipt.inputs.parent)
        .find(|row| row.seq == original.cause_seq && row.event.id == original.cause_event_id)
        .unwrap();
    assert_eq!(cause.created_at, Some(original.at));
    // Synthetic reproduction of the native 243213 microsecond producer/audit gap.
    cause.event.timestamp = original.at - chrono::Duration::microseconds(243213);
    receipt
}

#[tokio::test]
async fn audit_cause_clock_uses_exact_insertion_instead_of_producer_time() {
    use boss_jobs::move_diagnosis::{self, Status};
    let receipt = clock_fixture(false).await;
    let original = serde_json::to_value(&receipt.original).unwrap();
    let inputs = serde_json::to_value(&receipt.inputs).unwrap();
    let result =
        move_diagnosis::diagnose(Uuid::new_v4(), receipt.original, receipt.inputs).unwrap();
    assert_eq!(result.status, Status::Eligible, "{}", result.why);
    assert!(result.model.starts_with("registered-agent-handoff-v2:"));
    assert_eq!(result.model.split_once(':').unwrap().1.len(), 64);
    assert_eq!(serde_json::to_value(&result.original).unwrap(), original);
    assert_eq!(serde_json::to_value(&result.inputs).unwrap(), inputs);
}

#[tokio::test]
async fn audit_cause_clock_missing_wrong_or_foreign_binding_stays_unknown() {
    use boss_jobs::move_diagnosis::{self, Status};
    let receipt = clock_fixture(false).await;
    for variant in 0..5 {
        let mut inputs = receipt.inputs.clone();
        let cause = inputs
            .run
            .iter_mut()
            .chain(&mut inputs.parent)
            .find(|row| row.seq == receipt.original.r#move.cause_seq)
            .unwrap();
        // Matching producer time cannot substitute for missing or wrong audit time.
        cause.event.timestamp = receipt.original.r#move.at;
        match variant {
            0 => cause.created_at = None,
            1 => {
                cause.created_at =
                    Some(receipt.original.r#move.at + chrono::Duration::microseconds(1))
            }
            2 => cause.seq += 1,
            3 => cause.event.id = Uuid::new_v4(),
            _ => cause.event.kind = "another.event.kind".into(),
        }
        let result =
            move_diagnosis::diagnose(Uuid::new_v4(), receipt.original.clone(), inputs).unwrap();
        assert_eq!(
            result.status,
            Status::Unknown,
            "variant{variant}:{}",
            result.why
        );
        assert_eq!(
            result.why,
            "original cause absent from bounded native history"
        );
    }
}

#[tokio::test]
async fn audit_cause_clock_match_does_not_invent_a_run_admission() {
    use boss_jobs::move_diagnosis::{self, Status};
    let receipt = clock_fixture(true).await;
    assert!(receipt.inputs.run.is_empty());
    let result =
        move_diagnosis::diagnose(Uuid::new_v4(), receipt.original, receipt.inputs).unwrap();
    assert_eq!(result.status, Status::Unknown);
    assert_eq!(result.why, "admission missing");
    assert!(!move_diagnosis::validated(&result));
}

#[tokio::test]
async fn audit_cause_clock_model_change_retains_legacy_unknown_receipts_whole() {
    use boss_jobs::move_diagnosis::{self, Entry, Status};
    let mut legacy = clock_fixture(true).await;
    legacy.model = "registered-agent-handoff-v1:unavailable-fixture-artifact".into();
    legacy.status = Status::Unknown;
    legacy.why = "original cause absent from bounded native history".into();
    let original = legacy.original.clone();
    let original_json = serde_json::to_value(&original).unwrap();
    let old = Entry::Diagnosis {
        receipt: Box::new(legacy.clone()),
    };
    let old_json = serde_json::to_value(&old).unwrap();
    let fresh =
        move_diagnosis::diagnose(Uuid::new_v4(), original.clone(), legacy.inputs.clone()).unwrap();
    assert_ne!(fresh.id, legacy.id);
    assert_eq!(fresh.status, Status::Unknown);
    assert_eq!(fresh.why, "admission missing");
    let new = Entry::Diagnosis {
        receipt: Box::new(fresh),
    };
    assert!(!move_diagnosis::append(std::slice::from_ref(&old), &old, &original).unwrap());
    assert!(move_diagnosis::append(std::slice::from_ref(&old), &new, &original).unwrap());
    let entries = [old, new];
    let events: Vec<_> = entries.iter().enumerate().map(|(index, entry)| Event::new(
        "jobs", entry.event_kind(),
        json!({"entry":entry,"job_id":original.r#move.packet,"_actor":"synthetic-fixture","ledger_seq":index+1}),
        original.r#move.at,
    )).collect();
    let replay = move_diagnosis::reconstruct(&events).unwrap();
    assert_eq!(serde_json::to_value(&replay[0].1).unwrap(), old_json);
    assert_eq!(serde_json::to_value(&original).unwrap(), original_json);
    assert_eq!(
        move_diagnosis::hash(&entries).unwrap(),
        move_diagnosis::hash(&replay.into_iter().map(|(_, e)| e).collect::<Vec<_>>()).unwrap()
    );
    assert!(!move_diagnosis::validated(&legacy));
    assert!(
        move_diagnosis::active(&entries, &original)
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_parent_cause_before_run_admission_retains_an_unknown_diagnosis() {
    use boss_jobs::{
        move_diagnosis::{self, Diagnosis, Status},
        port::JobsRepository,
    };
    let (app, jobs) = fixture::fixture_with_parent_cause().await;
    let (_, before) = get(app.clone(), "/api/yard/moves").await;
    let original = &before["moves"][0];
    let cutoff = original["cause_seq"].as_i64().unwrap();
    let run_id =
        boss_core::job::JobId::from_uuid(original["packet"].as_str().unwrap().parse().unwrap());
    let parent_id = boss_core::job::JobId::from_uuid(
        original["handoff_from"].as_str().unwrap().parse().unwrap(),
    );
    let expected_run = jobs.historical_events(&run_id, cutoff, 4096).await.unwrap();
    let expected_parent = jobs
        .historical_events(&parent_id, cutoff, 4096)
        .await
        .unwrap();
    assert!(expected_run.is_empty());
    assert!(!expected_parent.is_empty());
    let request = json!({"request_id":Uuid::new_v4()});
    let (status, body) = post(app.clone(), "/api/yard/moves/1/diagnoses", request.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let receipt: Diagnosis = serde_json::from_value(body.clone()).unwrap();
    assert_eq!(receipt.status, Status::Unknown);
    assert_eq!(receipt.why, "admission missing");
    assert_eq!(
        serde_json::to_value(&receipt.inputs.run).unwrap(),
        serde_json::to_value(expected_run).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&receipt.inputs.parent).unwrap(),
        serde_json::to_value(expected_parent).unwrap()
    );
    assert_eq!(serde_json::to_value(&receipt.original).unwrap(), *original);
    assert!(!move_diagnosis::validated(&receipt));
    let (status, retry) = post(app.clone(), "/api/yard/moves/1/diagnoses", request).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(retry, body);
    let proof = fixture::install_proof(&jobs, &receipt).await;
    let (status, _) = post(
        app.clone(),
        "/api/yard/moves/1/corrections",
        json!({"request_id":Uuid::new_v4(),"diagnosis_id":receipt.id,"proof_request":proof}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (_, after) = get(app, "/api/yard/moves").await;
    assert_eq!(before["moves"], after["moves"]);
    assert_eq!(before["dispositions"], after["dispositions"]);
    assert_eq!(before["undeclared"], after["undeclared"]);
}

#[tokio::test]
async fn a_native_diagnosis_computes_eligibility_and_conserves_the_original() {
    let (status, body) = post(
        fixture().await,
        "/api/yard/moves/1/diagnoses",
        json!({"request_id":Uuid::new_v4()}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["status"], "eligible", "{body}");
    assert_eq!(body["original"]["seq"], 1);
    assert_eq!(body["original"]["declared"], false);
    assert_eq!(body["original"]["dispatch"], Value::Null);
    assert!(
        body["inputs"].is_object(),
        "full native inputs are retained: {body}"
    );
}

#[tokio::test]
async fn a_caller_cannot_supply_its_own_false_interpretation_or_proof() {
    let (status, body) = post(
        fixture().await,
        "/api/yard/moves/1/diagnoses",
        json!({"request_id":Uuid::new_v4(),"false_interpretation":true,"proof":{"exit":0}}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
}

#[tokio::test]
async fn exact_correction_is_idempotent_and_withdrawal_restores_the_original_finding() {
    let (app, jobs) = fixture::fixture_and_jobs().await;
    let (status, diagnosis) = post(
        app.clone(),
        "/api/yard/moves/1/diagnoses",
        json!({"request_id":Uuid::new_v4()}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{diagnosis}");
    let proof =
        fixture::install_proof(&jobs, &serde_json::from_value(diagnosis.clone()).unwrap()).await;
    let request =
        json!({"request_id":Uuid::new_v4(),"diagnosis_id":diagnosis["id"],"proof_request":proof});
    let (status, receipt) = post(
        app.clone(),
        "/api/yard/moves/1/corrections",
        request.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{receipt}");
    let (status, retry) = post(app.clone(), "/api/yard/moves/1/corrections", request).await;
    assert_eq!(status, StatusCode::OK, "{retry}");
    assert_eq!(receipt, retry);
    let (status, withdrawal) = post(
        app,
        "/api/yard/moves/1/withdrawals",
        json!({"request_id":Uuid::new_v4(),"correction_id":receipt["id"]}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{withdrawal}");
    assert_eq!(withdrawal["original_seq"], 1);
}

async fn get(app: axum::Router, path: &str) -> (StatusCode, Value) {
    let user = User {
        id: "agent-codex".into(),
        role: "operator".into(),
        access_tier: AccessTier::User,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: None,
    };
    let response = app
        .oneshot(
            Request::builder()
                .uri(path)
                .header("x-boss-user", serde_json::to_string(&user).unwrap())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or_else(|_| json!(String::from_utf8_lossy(&bytes))),
    )
}

#[tokio::test]
async fn the_record_conserves_raw_active_superseded_and_withdrawn_rows() {
    let (app, jobs) = fixture::fixture_and_jobs().await;
    let (_, before) = get(app.clone(), "/api/yard/moves").await;
    assert_eq!(before["moves"].as_array().unwrap().len(), 1);
    assert_eq!(before["dispositions"][0]["disposition"]["status"], "active");
    let (_, diagnosis) = post(
        app.clone(),
        "/api/yard/moves/1/diagnoses",
        json!({"request_id":Uuid::new_v4()}),
    )
    .await;
    let proof =
        fixture::install_proof(&jobs, &serde_json::from_value(diagnosis.clone()).unwrap()).await;
    let (status, correction) = post(
        app.clone(),
        "/api/yard/moves/1/corrections",
        json!({"request_id":Uuid::new_v4(),"diagnosis_id":diagnosis["id"],"proof_request":proof}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{correction}");
    let (_, after) = get(app.clone(), "/api/yard/moves").await;
    assert_eq!(before["moves"], after["moves"]);
    assert_eq!(
        after["dispositions"][0]["disposition"]["status"],
        "superseded"
    );
    assert_eq!(after["undeclared"]["moves"], 0);
    assert_eq!(
        after["history"],
        // raw = active + superseded + unknown + superseded_mover: a row
        // another mover wrote is its own count (backlog 3976779c).
        json!({"raw":1,"active":0,"superseded":1,"unknown":0,"superseded_mover":0})
    );
    let (status, body) = post(
        app.clone(),
        "/api/yard/moves/1/withdrawals",
        json!({"request_id":Uuid::new_v4(),"correction_id":correction["id"]}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (_, restored) = get(app, "/api/yard/moves").await;
    assert_eq!(before["moves"], restored["moves"]);
    assert_eq!(
        restored["dispositions"][0]["disposition"]["status"],
        "active"
    );
    assert_eq!(before["undeclared"], restored["undeclared"]);
}

#[tokio::test]
async fn native_history_refuses_missing_cause_cross_packet_cutoff_and_duplicate_protocols() {
    use boss_jobs::move_diagnosis::{Diagnosis, Status, diagnose};
    let (_, body) = post(
        fixture().await,
        "/api/yard/moves/1/diagnoses",
        json!({"request_id":Uuid::new_v4()}),
    )
    .await;
    let receipt: Diagnosis = serde_json::from_value(body).unwrap();
    assert_eq!(receipt.status, Status::Eligible);
    for variant in 0..5 {
        let mut inputs = receipt.inputs.clone();
        match variant {
            0 => inputs.run.clear(),
            1 => inputs.run[0].seq = receipt.original.r#move.cause_seq + 1,
            2 => inputs.run[0].event.payload["id"] = json!(Uuid::new_v4()),
            3 => inputs.workflows.push(inputs.workflows[0].clone()),
            _ => inputs.station_history.clear(),
        }
        let result = diagnose(receipt.id, receipt.original.clone(), inputs).unwrap();
        assert_eq!(
            result.status,
            Status::Unknown,
            "variant {variant}: {}",
            result.why
        );
    }
}

#[tokio::test]
async fn proofs_are_closed_replayable_and_invalidity_never_clears_an_original() {
    use boss_jobs::move_diagnosis::{self, Diagnosis, Entry, Status};
    let (_, body) = post(
        fixture().await,
        "/api/yard/moves/1/diagnoses",
        json!({"request_id":Uuid::new_v4()}),
    )
    .await;
    let receipt: Diagnosis = serde_json::from_value(body).unwrap();
    assert!(move_diagnosis::validated(&receipt));
    let original = receipt.original.clone();
    let diagnosis = Entry::Diagnosis {
        receipt: Box::new(receipt.clone()),
    };
    let correction = Entry::Correction {
        id: Uuid::new_v4(),
        original_seq: original.seq,
        diagnosis_id: receipt.id,
        original_hash: receipt.original_hash.clone(),
        proof: Some(Box::new(fixture::machine_proof(&receipt))),
    };
    let withdrawal = Entry::Withdrawal {
        id: Uuid::new_v4(),
        original_seq: original.seq,
        correction_id: correction.id(),
    };
    let entries = vec![diagnosis.clone(), correction.clone(), withdrawal.clone()];
    let events: Vec<_> = entries
        .iter()
        .enumerate().map(|(index,entry)| {
            Event::new(
                "jobs",
                entry.event_kind(),
                json!({"entry":entry,"job_id":original.r#move.packet,"_actor":"agent-codex","ledger_seq":index+1}),
                original.r#move.at,
            )
        })
        .collect();
    let replay = move_diagnosis::reconstruct(&events).unwrap();
    assert_eq!(
        move_diagnosis::hash(&entries).unwrap(),
        move_diagnosis::hash(&replay.into_iter().map(|(_, e)| e).collect::<Vec<_>>()).unwrap()
    );
    assert!(
        move_diagnosis::active(&entries, &original)
            .unwrap()
            .is_none()
    );
    let mut ordered = events.clone();
    for (index, event) in ordered.iter_mut().enumerate() {
        event.payload["ledger_seq"] = json!(index + 1);
    }
    ordered.reverse();
    let replay = move_diagnosis::reconstruct(&ordered).unwrap();
    assert_eq!(
        move_diagnosis::hash(&entries).unwrap(),
        move_diagnosis::hash(&replay.into_iter().map(|(_, e)| e).collect::<Vec<_>>()).unwrap(),
        "ledger order is a recorded fact, not relay scheduling"
    );
    for variant in 0..4 {
        let mut altered = receipt.clone();
        match variant {
            0 => altered.model = "a later reader alone".into(),
            1 => altered.inputs.run.clear(),
            2 => altered.original.r#move.packet = Uuid::new_v4().to_string(),
            _ => altered.status = Status::Genuine,
        }
        assert!(!move_diagnosis::validated(&altered));
        let invalid = vec![
            Entry::Diagnosis {
                receipt: Box::new(altered),
            },
            correction.clone(),
        ];
        assert!(move_diagnosis::active(&invalid, &original).is_err());
    }
    assert!(move_diagnosis::reconstruct(&[events[1].clone()]).is_err());
}

#[tokio::test]
async fn a_forged_status_future_protocol_or_unsupported_carriage_never_becomes_proof() {
    use boss_jobs::move_diagnosis::{self, Diagnosis, Status};
    let (_, body) = post(
        fixture().await,
        "/api/yard/moves/1/diagnoses",
        json!({"request_id":Uuid::new_v4()}),
    )
    .await;
    let receipt: Diagnosis = serde_json::from_value(body).unwrap();
    for variant in 0..4 {
        let mut original = receipt.original.clone();
        let mut inputs = receipt.inputs.clone();
        match variant {
            0 => inputs.workflows[0].status = boss_jobs::registry::WorkflowStatus::Draft,
            1 => inputs.workflows[0].created_at = original.r#move.at + chrono::Duration::days(1),
            2 => original.r#move.lineage = Some("boarded_jobs".into()),
            _ => inputs.stations[0].created_at = original.r#move.at + chrono::Duration::days(1),
        }
        let result = move_diagnosis::diagnose(receipt.id, original, inputs).unwrap();
        assert_eq!(
            result.status,
            Status::Unknown,
            "variant{variant}:{}",
            result.why
        );
    }
}
