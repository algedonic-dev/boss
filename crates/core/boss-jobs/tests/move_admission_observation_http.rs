//! Exact admission evidence never supplies missing historical registration.
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use boss_jobs::port::JobsRepository;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;
#[path = "yard_move_corrections/fixture.rs"]
#[allow(dead_code)]
mod fixture;

async fn get(app: axum::Router, path: &str) -> (StatusCode, Value) {
    get_as(app, path, "operator").await
}

async fn get_as(app: axum::Router, path: &str, role: &str) -> (StatusCode, Value) {
    let user = boss_policy_client::types::User {
        id: "agent-codex".into(),
        role: role.into(),
        department: None,
        access_tier: boss_policy_client::types::AccessTier::User,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
    };
    let request = Request::builder()
        .uri(path)
        .header("x-boss-user", serde_json::to_string(&user).unwrap())
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(json!(String::from_utf8_lossy(&bytes))),
    )
}

#[tokio::test]
async fn malformed_observation_paths_require_authority_before_parsing() {
    let app = fixture::fixture().await;
    let id = Uuid::new_v4();
    for path in [
        format!("/api/yard/moves/not-a-sequence/diagnoses/{id}/admission-observations/{id}"),
        format!("/api/yard/moves/1/diagnoses/not-a-diagnosis/admission-observations/{id}"),
        format!("/api/yard/moves/1/diagnoses/{id}/admission-observations/not-a-request"),
    ] {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(&path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{path}");
        let (status, _) = get_as(app.clone(), &path, "unprivileged").await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "denied malformed path {path}"
        );
        let (status, _) = get(app.clone(), &path).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "authorized malformed path {path}"
        );
    }
}

async fn input() -> (
    boss_jobs::move_diagnosis::Diagnosis,
    boss_jobs::move_admission::Snapshot,
) {
    let (app, jobs) = fixture::fixture_with_parent_cause().await;
    let (status, body) = fixture::post(
        app,
        "/api/yard/moves/1/diagnoses",
        json!({"request_id":Uuid::new_v4()}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let d = serde_json::from_value(body).unwrap();
    let s = jobs.move_admission_snapshot(&d).await.unwrap();
    (d, s)
}

#[tokio::test]
async fn malformed_native_evidence_is_refused_with_a_working_positive_control() {
    use boss_jobs::move_admission::observe;
    let (d, s) = input().await;
    let partition = boss_core::partition::Partition::Real;
    assert!(observe(Uuid::new_v4(), d.clone(), s.clone(), partition).is_ok());
    for variant in 0..16 {
        let mut d = d.clone();
        let mut s = s.clone();
        match variant {
            0 => s.native_admissions[0].created_at = None,
            1 => s.native_admissions[0].seq = s.captured_max_seq + 1,
            2 => s.native_admissions[0].event.id = Uuid::nil(),
            3 => s.native_admissions[0].event.source = "foreign".into(),
            4 => s.native_admissions[0].event.kind = "jobs.job.updated".into(),
            5 => s.native_admissions[0].event.payload["id"] = json!(Uuid::new_v4()),
            6 => s.native_admissions[0].event.payload["_partition"] = json!("shadow"),
            7 => s.native_admissions[0].event.payload["partition"] = json!("shadow"),
            8 => s.parent.last_mut().unwrap().created_at = None,
            9 => s.parent.last_mut().unwrap().event.id = Uuid::new_v4(),
            10 => s.captured_max_seq = d.original.r#move.cause_seq - 1,
            11 => d.original_hash = "wrong".into(),
            12 => {
                d.original.r#move.kind = "other".into();
                d.original_hash = boss_jobs::move_diagnosis::hash(&d.original).unwrap();
            }
            13 => {
                let duplicate = s.native_admissions[0].clone();
                s.native_admissions.push(duplicate);
            }
            14 => s.parent.last_mut().unwrap().event.payload["_partition"] = json!("simulated"),
            _ => s.native_admissions[0].event.id = s.parent.last().unwrap().event.id,
        }
        assert!(
            observe(Uuid::new_v4(), d, s, partition).is_err(),
            "variant {variant}"
        );
    }
}

#[tokio::test]
async fn absent_ambiguous_and_disagreed_observations_remain_evidence_only() {
    use boss_jobs::move_admission::{self, Outcome};
    let (d, s) = input().await;
    let p = boss_core::partition::Partition::Real;
    let mut absent = s.clone();
    absent.native_admissions.clear();
    assert_eq!(
        move_admission::observe(Uuid::new_v4(), d.clone(), absent, p)
            .unwrap()
            .outcome,
        Outcome::AbsentInSnapshot
    );
    let mut ambiguous = s.clone();
    let mut second = ambiguous.native_admissions[0].clone();
    second.seq = s.captured_max_seq + 1;
    second.event.id = Uuid::new_v4();
    ambiguous.captured_max_seq = second.seq;
    ambiguous.native_admissions.push(second);
    assert_eq!(
        move_admission::observe(Uuid::new_v4(), d.clone(), ambiguous, p)
            .unwrap()
            .outcome,
        Outcome::AmbiguousAdmission
    );
    let mut disagreement = s.clone();
    disagreement.parent[0].event.payload["observation_control"] = json!("newly visible original");
    let result = move_admission::observe(Uuid::new_v4(), d.clone(), disagreement, p).unwrap();
    assert_eq!(result.outcome, Outcome::SliceDisagreement);
    assert_eq!(
        serde_json::to_value(&result.diagnosis).unwrap(),
        serde_json::to_value(&d).unwrap()
    );
    assert!(!boss_jobs::move_diagnosis::validated(&result.diagnosis));
}

#[tokio::test]
async fn observation_replay_conserves_the_whole_record_and_refuses_tampering() {
    use boss_jobs::move_admission::{self, observe};
    let (d, s) = input().await;
    let o = observe(
        Uuid::new_v4(),
        d.clone(),
        s,
        boss_core::partition::Partition::Real,
    )
    .unwrap();
    let event = boss_core::event::Event::new(
        "jobs",
        move_admission::EVENT_KIND,
        json!({"job_id":d.original.r#move.packet,"_actor":"operator","_partition":"real","observation":o}),
        d.original.r#move.at,
    );
    let events = [event.clone(), event.clone()];
    let replay = move_admission::reconstruct(&events).unwrap();
    assert_eq!(replay.len(), 1);
    assert_eq!(
        serde_json::to_value(&replay[0].1).unwrap(),
        serde_json::to_value(&o).unwrap()
    );
    for variant in 0..6 {
        let mut event = event.clone();
        match variant {
            0 => event.payload["observation"]["outcome"] = json!("absent-in-snapshot"),
            1 => event.payload["observation"]["diagnosis_hash"] = json!("wrong"),
            2 => event.payload["job_id"] = json!(Uuid::new_v4()),
            3 => event.payload["_partition"] = json!("shadow"),
            4 => event.payload["observation"]["snapshot"]["cutoff_slices"]["hash"] = json!("wrong"),
            _ => event.payload["observation"]["order"] = json!("before-cause"),
        }
        assert!(
            move_admission::reconstruct(&[event]).is_err(),
            "variant {variant}"
        );
    }
}

#[test]
fn the_existing_diagnosis_model_fingerprint_is_unchanged() {
    // Exact deployed v2 model retained by native diagnosis159d37ac; this reader must not change it.
    assert_eq!(
        boss_jobs::move_diagnosis::model(),
        "registered-agent-handoff-v2:da0c4a6ddef6fa9120bf8bc663c5d37bb13c1437d2106425d7cb28848a2c5bdf"
    );
}

#[tokio::test]
async fn old_model_receipts_keep_their_whole_eligible_or_unknown_reading_on_replay() {
    for partial in [false, true] {
        let (app, jobs) = if partial {
            fixture::fixture_with_parent_cause().await
        } else {
            fixture::fixture_and_jobs().await
        };
        let (_, value) = fixture::post(
            app,
            "/api/yard/moves/1/diagnoses",
            json!({"request_id":Uuid::new_v4()}),
        )
        .await;
        assert_eq!(
            value["status"],
            if partial { "unknown" } else { "eligible" }
        );
        let diagnosis: boss_jobs::move_diagnosis::Diagnosis =
            serde_json::from_value(value).unwrap();
        let snapshot = jobs.move_admission_snapshot(&diagnosis).await.unwrap();
        for model in [
            "registered-agent-handoff-v1:e35171c32522014ef10ea9ba15e534f20cf6b1237853167fa64139365a92a067",
            "registered-agent-handoff-v2:da0c4a6ddef6fa9120bf8bc663c5d37bb13c1437d2106425d7cb28848a2c5bdf",
        ] {
            let mut original = diagnosis.clone();
            original.model = model.into();
            let bytes = serde_json::to_vec(&original).unwrap();
            let observation = boss_jobs::move_admission::observe(
                Uuid::new_v4(),
                original.clone(),
                snapshot.clone(),
                boss_core::partition::Partition::Real,
            )
            .unwrap();
            assert_eq!(serde_json::to_vec(&observation.diagnosis).unwrap(), bytes);
            let event = boss_core::event::Event::new(
                "jobs",
                boss_jobs::move_admission::EVENT_KIND,
                json!({"job_id":original.original.r#move.packet,"_actor":"operator","_partition":"real","observation":observation}),
                original.original.r#move.at,
            );
            let replay = boss_jobs::move_admission::reconstruct(&[event]).unwrap();
            assert_eq!(serde_json::to_vec(&replay[0].1.diagnosis).unwrap(), bytes);
        }
    }
}

#[tokio::test]
async fn before_cause_evidence_does_not_change_an_existing_eligible_diagnosis() {
    let app = fixture::fixture().await;
    let (_, d) = fixture::post(
        app.clone(),
        "/api/yard/moves/1/diagnoses",
        json!({"request_id":Uuid::new_v4()}),
    )
    .await;
    assert_eq!(d["status"], "eligible");
    let path = format!(
        "/api/yard/moves/1/diagnoses/{}/admission-observations",
        d["id"].as_str().unwrap()
    );
    let (status, o) = fixture::post(app, &path, json!({"request_id":Uuid::new_v4()})).await;
    assert_eq!(status, StatusCode::CREATED, "{o}");
    assert_eq!(o["order"], "before-cause");
    assert_eq!(o["diagnosis"], d);
}

#[tokio::test]
async fn hard_bounds_refuse_whole_evidence_instead_of_truncating() {
    use boss_jobs::move_admission::{self, observe};
    let (d, s) = input().await;
    let p = boss_core::partition::Partition::Real;
    let mut large = s.clone();
    large.native_admissions[0].event.payload["metadata"]["large_control"] =
        json!("x".repeat(move_admission::MAX_BYTES));
    let error = observe(Uuid::new_v4(), d.clone(), large, p).unwrap_err();
    assert!(error.contains("transport bound"), "{error}");
    let mut rows = s.clone();
    rows.parent = vec![rows.parent[0].clone(); move_admission::MAX_ROWS + 1];
    assert!(
        observe(Uuid::new_v4(), d.clone(), rows, p)
            .unwrap_err()
            .contains("bound exceeded")
    );
    let mut admissions = s.clone();
    admissions.native_admissions = vec![admissions.native_admissions[0].clone(); 3];
    assert!(
        observe(Uuid::new_v4(), d, admissions, p)
            .unwrap_err()
            .contains("snapshot bound")
    );
}

#[tokio::test]
async fn the_observation_door_refuses_caller_selected_evidence_and_foreign_bindings() {
    let (app, _) = fixture::fixture_with_parent_cause().await;
    let (_, diagnosis) = fixture::post(
        app.clone(),
        "/api/yard/moves/1/diagnoses",
        json!({"request_id":Uuid::new_v4()}),
    )
    .await;
    let path = format!(
        "/api/yard/moves/1/diagnoses/{}/admission-observations",
        diagnosis["id"].as_str().unwrap()
    );
    for extra in [
        "packet",
        "captured_max_seq",
        "native_admissions",
        "created_at",
        "proof_request",
    ] {
        let mut body = json!({"request_id":Uuid::new_v4()});
        body[extra] = json!("caller-value");
        let (status, _) = fixture::post(app.clone(), &path, body).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{extra}");
    }
    let request_id = Uuid::new_v4();
    let (status, _) = fixture::post(app.clone(), &path, json!({"request_id":request_id})).await;
    assert_eq!(status, StatusCode::CREATED);
    let bad = format!(
        "/api/yard/moves/1/diagnoses/{}/admission-observations",
        Uuid::new_v4()
    );
    let (status, _) = fixture::post(app.clone(), &bad, json!({"request_id":request_id})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let anonymous = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(json!({"request_id":Uuid::new_v4()}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(anonymous.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn later_native_admission_is_separate_immutable_evidence_not_clearance() {
    let (app, _) = fixture::fixture_with_parent_cause().await;
    let (_, before) = get(app.clone(), "/api/yard/moves").await;
    let (status, diagnosis) = fixture::post(
        app.clone(),
        "/api/yard/moves/1/diagnoses",
        json!({"request_id":Uuid::new_v4()}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(diagnosis["status"], "unknown");
    let path = format!(
        "/api/yard/moves/1/diagnoses/{}/admission-observations",
        diagnosis["id"].as_str().unwrap()
    );
    let request = json!({"request_id":Uuid::new_v4()});
    let (status, observation) = fixture::post(app.clone(), &path, request.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "{observation}");
    assert_eq!(observation["diagnosis"], diagnosis);
    assert_eq!(observation["outcome"], "observed");
    assert_eq!(observation["order"], "after-cause");
    assert_eq!(
        observation["snapshot"]["cutoff_slices"]["source"],
        "conserved-diagnosis"
    );
    assert!(
        observation["diagnosis"]["inputs"]["run"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let admission = &observation["snapshot"]["native_admissions"][0];
    assert!(
        admission["seq"].as_i64().unwrap() > diagnosis["original"]["cause_seq"].as_i64().unwrap()
    );
    assert!(admission["created_at"].is_string());
    assert_eq!(observation["diagnosis"]["status"], "unknown");
    let (status, retry) = fixture::post(app.clone(), &path, request.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(retry, observation);
    let read_path = format!("{path}/{}", request["request_id"].as_str().unwrap());
    let (status, stored) = get(app.clone(), &read_path).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(stored, observation);
    let (_, after) = get(app.clone(), "/api/yard/moves").await;
    for key in ["moves", "dispositions", "undeclared"] {
        assert_eq!(after[key], before[key]);
    }
    let (status, _) = fixture::post(
        app,
        "/api/yard/moves/1/corrections",
        json!({"request_id":Uuid::new_v4(),"diagnosis_id":observation["id"]}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn exact_recorded_admission_is_readable_without_correction_authority() {
    let (app, jobs) = fixture::fixture_with_parent_cause().await;
    let (status, diagnosis) = fixture::post(
        app.clone(),
        "/api/yard/moves/1/diagnoses",
        json!({"request_id":Uuid::new_v4()}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let path = format!(
        "/api/yard/moves/1/diagnoses/{}/admission-observations",
        diagnosis["id"].as_str().unwrap()
    );
    let request_id = Uuid::new_v4();
    let (status, original) =
        fixture::post(app.clone(), &path, json!({"request_id":request_id})).await;
    assert_eq!(status, StatusCode::CREATED);
    let reader = boss_policy_client::types::User {
        id: "agent-codex".into(),
        role: "evidence-reader".into(),
        department: None,
        access_tier: boss_policy_client::types::AccessTier::User,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
    };
    let before = jobs.recorded_events();
    let denied = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&path)
                .header("x-boss-user", serde_json::to_string(&reader).unwrap())
                .header("content-type", "application/json")
                .body(Body::from(json!({"request_id":Uuid::new_v4()}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        denied.status(),
        StatusCode::FORBIDDEN,
        "read authority must not create evidence"
    );
    let read_path = format!("{path}/{request_id}");
    for id in ["", " \t "] {
        let unnamed = boss_policy_client::types::User {
            id: id.into(),
            ..reader.clone()
        };
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(&read_path)
                    .header("x-boss-user", serde_json::to_string(&unnamed).unwrap())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "unnamed {id:?}");
    }
    for (wrong_path, expected) in [
        (
            format!(
                "/api/yard/moves/2/diagnoses/{}/admission-observations/{request_id}",
                diagnosis["id"].as_str().unwrap()
            ),
            StatusCode::NOT_FOUND,
        ),
        (
            format!(
                "/api/yard/moves/1/diagnoses/{}/admission-observations/{request_id}",
                Uuid::new_v4()
            ),
            StatusCode::CONFLICT,
        ),
    ] {
        let (status, _) = get_as(app.clone(), &wrong_path, "evidence-reader").await;
        assert_eq!(status, expected, "foreign binding {wrong_path}");
    }
    let (status, _) = get_as(
        app.clone(),
        &format!("{path}/{}", Uuid::new_v4()),
        "evidence-reader",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, denied) = get_as(app.clone(), &read_path, "scoped-evidence-reader").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{denied}");
    let (status, observed) = get_as(app.clone(), &read_path, "evidence-reader").await;
    assert_eq!(status, StatusCode::OK, "{observed}");
    assert_eq!(
        observed, original,
        "read the whole retained receipt unchanged"
    );
    assert_eq!(
        jobs.recorded_events().len(),
        before.len(),
        "GET emits no facts"
    );
}
