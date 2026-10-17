use boss_jobs::cadence::{CadenceRepository, InMemoryCadence, NewFiring};
use chrono::{DateTime, Utc};
use serde_json::json;

struct BrokenPage {
    rows: Vec<NewFiring>,
    fail: bool,
}
#[async_trait::async_trait]
impl CadenceRepository for BrokenPage {
    async fn active_rules(
        &self,
    ) -> Result<Vec<boss_jobs::cadence::CadenceRuleRow>, boss_jobs::cadence::CadenceError> {
        Ok(vec![])
    }
    async fn last_firing(
        &self,
        _: &str,
    ) -> Result<Option<boss_jobs::cadence::LastFiring>, boss_jobs::cadence::CadenceError> {
        Ok(None)
    }
    async fn claim_firing(&self, _: &NewFiring) -> Result<bool, boss_jobs::cadence::CadenceError> {
        unreachable!()
    }
    async fn record_outcome(
        &self,
        _: &str,
        _: &boss_jobs::cadence::FiringOutcome,
    ) -> Result<(), boss_jobs::cadence::CadenceError> {
        unreachable!()
    }
    async fn firing_history(
        &self,
        _: &str,
        _: Option<&str>,
        _: Option<(DateTime<Utc>, String)>,
        _: usize,
    ) -> Result<Vec<NewFiring>, boss_jobs::cadence::CadenceError> {
        if self.fail {
            Err(boss_jobs::cadence::CadenceError::Storage(
                "fixture failed read".into(),
            ))
        } else {
            Ok(self.rows.clone())
        }
    }
}

fn at(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

#[tokio::test]
async fn older_success_is_separate_from_newest_unknown_and_conserves_the_whole_record() {
    let rule = serde_json::from_value(
        json!({"name":"history-rule","verb":"reconcile","basis":"wall","every_minutes":5}),
    )
    .unwrap();
    let repo = InMemoryCadence::new(vec![rule]);
    let rules = repo.active_rules().await.unwrap();
    let mut original = NewFiring {
        firing_id: "original-success".into(),
        rule_name: "history-rule".into(),
        verb: "reconcile".into(),
        basis: "wall".into(),
        fired_at: at("2026-10-16T00:00:00Z"),
        detail: json!({"rc":0,"extra_original":{"conserved":[1,2,3]},"observations":{"gate_queue":{
            "contract_version":1,"state":"completed","source_build":"exact-build",
            "observed_at":"2026-10-16T00:00:01Z","report":{"retained":[]},"error":null}}}),
    };
    repo.claim_firing(&original).await.unwrap();
    for (index, change) in [
        json!({"state":"unknown"}),
        json!({"state":"completed","error":"failed pass"}),
        json!({"state":"completed"}),
        json!(null),
    ]
    .into_iter()
    .enumerate()
    {
        let mut newer = original.clone();
        newer.firing_id = format!("newer-{index}");
        newer.fired_at = at("2026-10-16T00:01:00Z") + chrono::Duration::seconds(index as i64);
        newer.detail["observations"]["gate_queue"] = change;
        repo.claim_firing(&newer).await.unwrap();
    }
    let mut unrelated = original.clone();
    unrelated.firing_id = "latest-unrelated".into();
    unrelated.fired_at = at("2026-10-16T00:02:00Z");
    unrelated.detail = json!({"rc":1,"observations":{"another":{"state":"completed"}}});
    repo.claim_firing(&unrelated).await.unwrap();
    let now = at("2026-10-16T00:02:01Z");
    let reading = boss_jobs::gate_queue_observation::history(&repo, &rules, now, 2, 10).await;
    assert_eq!(reading["state"], "observed");
    assert_eq!(reading["record"], serde_json::to_value(&original).unwrap());
    // Qualification cannot stop before checking a returned page's entire
    // binding/order. Even a duplicate after a valid success invalidates it.
    let bad_page = BrokenPage {
        rows: vec![original.clone(), original.clone()],
        fail: false,
    };
    let corrupt = boss_jobs::gate_queue_observation::history(&bad_page, &rules, now, 2, 10).await;
    assert_eq!(corrupt["state"], "unknown");
    assert!(corrupt["record"].is_null());
    assert_eq!(
        corrupt["reads"][0]["rows"],
        serde_json::to_value(&bad_page.rows).unwrap()
    );
    let failed = BrokenPage {
        rows: vec![],
        fail: true,
    };
    assert_eq!(
        boss_jobs::gate_queue_observation::history(&failed, &rules, now, 2, 10).await["state"],
        "unknown"
    );
    let latest = repo.last_firing("history-rule").await.unwrap().unwrap();
    assert_eq!(
        boss_jobs::gate_queue_observation::project(
            &rules,
            &[("history-rule".into(), latest)],
            true,
            Some(now)
        )["state"],
        "unknown"
    );
    let bounded = boss_jobs::gate_queue_observation::history(&repo, &rules, now, 2, 1).await;
    assert_eq!(bounded["state"], "unknown");
    assert!(bounded["record"].is_null());
    assert!(
        bounded["reads"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty())
    );
    // A failed outcome cannot qualify merely because the nested pass says completed.
    original.firing_id = "newest-failed".into();
    original.fired_at = at("2026-10-16T00:02:00Z");
    original.detail["rc"] = json!(1);
    original.detail["observations"]["gate_queue"]["observed_at"] = json!("2026-10-16T00:02:00Z");
    repo.claim_firing(&original).await.unwrap();
    assert_eq!(
        boss_jobs::gate_queue_observation::history(&repo, &rules, now, 2, 10).await["record"]["firing_id"],
        "original-success"
    );
    let mut failed_pass = original.clone();
    failed_pass.firing_id = "completed-with-error".into();
    failed_pass.detail["rc"] = json!(0);
    failed_pass.detail["observations"]["gate_queue"]["error"] = json!("whole pass failed");
    repo.claim_firing(&failed_pass).await.unwrap();
    assert_eq!(
        boss_jobs::gate_queue_observation::history(&repo, &rules, now, 2, 10).await["record"]["firing_id"],
        "original-success"
    );
    // The actual yard HTTP reader must carry history alongside unknown
    // current health; a standalone helper is not the product contract.
    use std::sync::Arc;
    use tower::ServiceExt;
    let bus = boss_testing::RecordingEventBus::new();
    let mut state = boss_jobs::http::JobsApiState::minimal(
        Arc::new(boss_jobs::InMemoryJobs::new()),
        bus.clone(),
        boss_core::publisher::DomainPublisher::new(bus, "jobs"),
        Arc::new(boss_policy_client::PermissivePolicyClient),
        Arc::new(boss_clock_client::FixedClockClient::new(
            boss_clock_client::ClockNow {
                now,
                simulated: false,
                epoch_start: None,
                epoch_end: None,
                paused: false,
                restart_in_progress: false,
                warp_factor: None,
            },
        )),
    );
    state.cadence = Some(Arc::new(repo));
    let response = boss_jobs::http::router(state)
        .oneshot(
            axum::http::Request::builder()
                .uri("/api/yard/status")
                .header(
                    "x-boss-user",
                    json!({"id":"agent-test","role":"platform-admin","access_tier":"operator"})
                        .to_string(),
                )
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(response.status().is_success(), "{}", response.status());
    let bytes = axum::body::to_bytes(response.into_body(), 16 * 1024 * 1024)
        .await
        .unwrap();
    let wire: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(wire["gate_queue_supervision"]["state"], "unknown");
    assert_eq!(
        wire["gate_queue_supervision"]["last_successful"]["record"]["firing_id"],
        "original-success"
    );
}
