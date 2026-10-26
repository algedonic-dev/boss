use super::*;
use crate::move_diagnosis::HistoricalEvent;
use crate::{
    InMemoryJobs, membership_projection::InMemoryMembershipProjection, membership_replay::*,
};
use serde_json::{Value, json};

async fn fixture(reason: &str) -> (Arc<InMemoryJobs>, ProjectionIdentity) {
    let jobs = super::observation_tests::jobs();
    let identity = ProjectionIdentity::new(Some("alice".into()), crate::port::JobScope::All);
    let mut input = jobs
        .membership_snapshot(
            Utc::now() - Duration::hours(48),
            identity.actor.clone(),
            &identity.scope,
            SOURCE_ROW_LIMIT,
        )
        .await
        .unwrap();
    let population: Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/station-predicate-population-20261016.json"
    ))
    .unwrap();
    for (i, name) in ["original-a", "original-b"].iter().enumerate() {
        let mut station = population["data"][0].clone();
        station["name"] = json!(name);
        input.groups.push(ReplayGroup {
            original: None,
            rows: vec![HistoricalEvent {
                seq: i as i64 + 2,
                created_at: Some(input.since - Duration::seconds(1)),
                event: boss_core::event::Event::new(
                    "jobs",
                    crate::events::STATION_PUBLISHED,
                    station,
                    input.since - Duration::seconds(1),
                ),
            }],
        });
    }
    input.audit_cutoff = 3;
    input.source_unavailable = vec![
        Unavailable {
            since: input.since,
            until: input.until,
            reason: reason.into(),
        },
        Unavailable {
            since: input.since + Duration::minutes(1),
            until: input.until,
            reason: "distinct original gap".into(),
        },
    ];
    jobs.answer_membership(input);
    (jobs, identity)
}

#[tokio::test]
async fn original_refusals_survive_the_actual_owner_and_window_attempts() {
    let (jobs, identity) = fixture("original α source gap").await;
    let store = Arc::new(InMemoryMembershipProjection::new());
    let service = MembershipProjectionService::new(
        jobs.clone(),
        store.clone(),
        Arc::new(boss_clock_client::WallClockClient),
    );
    let why = service.refresh_one(&identity).await.unwrap_err();
    assert!(why.contains("48-hour history") && why.contains("24-hour history"));
    assert!(
        store.load(&identity).await.unwrap().is_none(),
        "an unread fold still cannot publish"
    );
    let value = serde_json::to_value(service.observe_refresh(&identity).await.unwrap()).unwrap();
    let refusals = value["latest_completed"]["refusals"]
        .as_array()
        .expect("typed refusal records lost at candidate boundary");
    assert_eq!(refusals.len(), 2);
    for (record, attempt) in refusals.iter().zip(["initial_48h", "fallback_24h"]) {
        assert_eq!(record["attempt"], attempt);
        assert_eq!(record["complete"], true);
        assert_eq!(
            record["total_bytes"].as_u64().unwrap() as usize,
            serde_json::to_vec(&record["original"]).unwrap().len()
        );
        assert_eq!(record["retained_bytes"], record["total_bytes"]);
        assert_eq!(record["omitted_spans"], 0);
        assert_eq!(record["omitted_stations"], 0);
        let reading = &record["original"]["unread"];
        assert_eq!(reading["audit_cutoff"], 3);
        assert_eq!(reading["denied"], false);
        let stations = reading["stations"].as_array().unwrap();
        assert_eq!(stations.len(), 2);
        for (station, name) in stations.iter().zip(["original-a", "original-b"]) {
            assert_eq!(station["station"], name);
            assert_eq!(station["scope"], json!(crate::port::JobScope::All));
            let spans = station["unknown"].as_array().unwrap();
            assert!(
                spans
                    .iter()
                    .any(|span| span["reason"] == "original α source gap")
            );
            assert!(
                spans
                    .iter()
                    .any(|span| span["reason"] == "distinct original gap")
            );
            assert!(
                spans
                    .iter()
                    .all(|span| span["since"].is_string() && span["until"].is_string())
            );
        }
    }
    assert_ne!(
        refusals[0]["original"]["unread"]["since"],
        refusals[1]["original"]["unread"]["since"]
    );
    for foreign in [
        ProjectionIdentity::new(Some("bob".into()), crate::port::JobScope::All),
        ProjectionIdentity::new(Some("alice".into()), crate::port::JobScope::None),
    ] {
        let foreign =
            serde_json::to_value(service.observe_refresh(&foreign).await.unwrap()).unwrap();
        assert!(foreign["latest_completed"].is_null());
        assert!(foreign["cached_failure"].is_null());
    }
}

#[tokio::test]
async fn oversized_originals_are_explicitly_unretained_and_still_refuse_publication() {
    let (jobs, identity) = fixture(&"original α gap".repeat(10_000)).await;
    let store = Arc::new(InMemoryMembershipProjection::new());
    let service = MembershipProjectionService::new(
        jobs,
        store.clone(),
        Arc::new(boss_clock_client::WallClockClient),
    );
    assert!(service.refresh_one(&identity).await.is_err());
    let value = serde_json::to_value(service.observe_refresh(&identity).await.unwrap()).unwrap();
    let flight = &value["latest_completed"];
    assert_eq!(flight["record_complete"], false);
    for record in flight["refusals"].as_array().unwrap() {
        assert_eq!(record["complete"], false);
        assert!(record["original"].is_null());
        assert!(record["total_bytes"].as_u64().unwrap() > 65_536);
        assert_eq!(record["retained_bytes"], 0);
        assert_eq!(record["retained_spans"], 0);
        assert_eq!(record["omitted_spans"], record["total_spans"]);
        assert_eq!(record["omitted_causes"], record["total_causes"]);
        assert_eq!(record["retained_causes"], 0);
        assert_eq!(record["omitted_stations"], record["total_stations"]);
        assert_eq!(record["audit_cutoff"], 3);
        assert_eq!(record["denied"], false);
        assert!(record["since"].is_string() && record["until"].is_string());
        assert!(record["total_spans"].as_u64().unwrap() >= 4);
        assert_eq!(
            record["unavailable_reason"],
            "original refusal exceeds flight diagnostic byte bound"
        );
    }
    assert!(store.load(&identity).await.unwrap().is_none());
    assert!(serde_json::to_vec(&value).unwrap().len() < 8_192);
}

#[tokio::test]
async fn denied_source_still_refuses_before_a_fold_without_fabricating_an_original() {
    let (jobs, _) = fixture("original denied gap").await;
    let identity = ProjectionIdentity::new(Some("alice".into()), crate::port::JobScope::None);
    let service = MembershipProjectionService::new(
        jobs,
        Arc::new(InMemoryMembershipProjection::new()),
        Arc::new(boss_clock_client::WallClockClient),
    );
    let why = service.refresh_one(&identity).await.unwrap_err();
    assert!(why.contains("invalid native membership census scope"));
    let value = serde_json::to_value(service.observe_refresh(&identity).await.unwrap()).unwrap();
    let records = value["latest_completed"]["refusals"].as_array().unwrap();
    assert!(
        records.is_empty(),
        "source census denies before any typed fold result exists"
    );
    assert!(
        service
            .observe_refresh(&ProjectionIdentity::new(
                Some("alice".into()),
                crate::port::JobScope::All
            ))
            .await
            .unwrap()
            .latest_completed
            .is_none()
    );
}

#[tokio::test]
async fn cached_original_failure_is_not_reassigned_to_a_new_active_flight() {
    let (jobs, identity) = fixture("cached original α").await;
    let service = Arc::new(MembershipProjectionService::new(
        jobs.clone(),
        Arc::new(InMemoryMembershipProjection::new()),
        Arc::new(boss_clock_client::WallClockClient),
    ));
    assert!(service.read(&identity, 24).await.unwrap().is_none());
    let (stop, receiver) = tokio::sync::watch::channel(false);
    let updater = tokio::spawn(service.clone().run(receiver));
    let cached = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let v =
                serde_json::to_value(service.observe_refresh(&identity).await.unwrap()).unwrap();
            if v["cached_failure_flight"].is_string() {
                break v;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    stop.send(true).unwrap();
    updater.await.unwrap();
    assert_eq!(
        cached["cached_failure"]["id"],
        cached["cached_failure_flight"]
    );
    assert!(
        cached["cached_failure"]["refusals"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["original"].to_string().contains("cached original α"))
    );
    let gate = Arc::new((tokio::sync::Notify::new(), tokio::sync::Notify::new()));
    *jobs.projection_source_gate.lock().unwrap() = Some(gate.clone());
    let next = service.clone();
    let next_identity = identity.clone();
    let owner = tokio::spawn(async move { next.refresh_one(&next_identity).await });
    tokio::time::timeout(std::time::Duration::from_secs(5), gate.0.notified())
        .await
        .unwrap();
    let active = serde_json::to_value(service.observe_refresh(&identity).await.unwrap()).unwrap();
    gate.1.notify_one();
    assert!(owner.await.unwrap().is_err());
    assert_ne!(active["active"]["id"], cached["cached_failure_flight"]);
    assert_eq!(active["cached_failure"], cached["cached_failure"]);
    assert!(active["active"]["refusals"].as_array().unwrap().is_empty());
    let completed =
        serde_json::to_value(service.observe_refresh(&identity).await.unwrap()).unwrap();
    assert_ne!(
        completed["latest_completed"]["id"],
        completed["cached_failure"]["id"]
    );
    assert_eq!(completed["cached_failure"], cached["cached_failure"]);
}

#[tokio::test]
async fn a_refused_delta_keeps_its_cause_before_rebuild_and_current_fallback() {
    let (jobs, identity) = fixture("original gap").await;
    let mut input = jobs
        .membership_snapshot(
            Utc::now() - Duration::hours(48),
            identity.actor.clone(),
            &identity.scope,
            SOURCE_ROW_LIMIT,
        )
        .await
        .unwrap();
    input.source_unavailable.clear();
    jobs.answer_membership(input.clone());
    let store = Arc::new(InMemoryMembershipProjection::new());
    let service = MembershipProjectionService::new(
        jobs.clone(),
        store.clone(),
        Arc::new(boss_clock_client::WallClockClient),
    );
    service.refresh_one(&identity).await.unwrap();
    let old = store.load(&identity).await.unwrap().unwrap();
    let start = input.until;
    input.until += Duration::seconds(1);
    input.source_unavailable.push(Unavailable {
        since: start,
        until: input.until,
        reason: "new original delta gap".into(),
    });
    jobs.answer_membership(input);
    assert!(service.refresh_one(&identity).await.is_err());
    let value = serde_json::to_value(service.observe_refresh(&identity).await.unwrap()).unwrap();
    let records = value["latest_completed"]["refusals"].as_array().unwrap();
    assert_eq!(
        records
            .iter()
            .map(|r| r["attempt"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["delta", "rebuild_48h", "fallback_24h"]
    );
    assert!(
        records
            .iter()
            .all(|r| r["original"].to_string().contains("new original delta gap"))
    );
    assert_eq!(
        store.load(&identity).await.unwrap().unwrap().revision,
        old.revision
    );
}
