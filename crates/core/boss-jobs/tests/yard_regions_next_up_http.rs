//! `GET /api/yard/regions` carries `next_up` — the top board's NEXT UP
//! row, end to end through the real router (backlog 74569e94, design
//! ea906603 Q3, car 3).
//!
//! What this pins:
//!
//! 1. **One read folds every source**: the cadence rows and the dock
//!    (the next board, the clock window), the dispatcher's schedule, and
//!    the credentials registry — each row carrying `kind`, `title`, `at`,
//!    `estimate`, `basis` and `source`.
//! 2. **The dispatcher is asked AS THE VIEWER**: the schedule reader is
//!    handed the caller the request named, so the dispatcher's own scope
//!    rule judges them.
//! 3. **A reader not wired is a row saying so**, never a missing row —
//!    an empty board reads as "nothing is coming".
//! 4. **A caller whose scope reads no packets gets the empty map's empty
//!    row**, and neither cross-read is made for them.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::InMemoryJobs;
use boss_jobs::cadence::{CadenceRepository, CadenceRuleRow, InMemoryCadence};
use boss_jobs::credentials::{CredentialRow, CredentialsRegistry, InMemoryCredentials};
use boss_jobs::dispatcher_schedule::{DispatcherSchedule, ScheduledRule};
use boss_jobs::http::{JobsApiState, router};
use boss_policy_client::types::{AccessTier, User};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use chrono::{DateTime, Utc};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const NOW: &str = "2026-09-27T17:08:00Z";

fn t(rfc3339: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(rfc3339).unwrap().into()
}

/// A schedule reader that records whom it was asked for.
struct Recording {
    answer: Result<Vec<ScheduledRule>, String>,
    asked_for: Mutex<Vec<String>>,
}

#[async_trait]
impl DispatcherSchedule for Recording {
    async fn schedule(&self, viewer: &User) -> Result<Vec<ScheduledRule>, String> {
        self.asked_for.lock().unwrap().push(viewer.id.clone());
        self.answer.clone()
    }
}

fn window_rule() -> CadenceRuleRow {
    CadenceRuleRow {
        name: "train-window".into(),
        verb: "run".into(),
        basis: "clock".into(),
        every_minutes: None,
        at_times: Some(json!(["06:05", "18:05"])),
        min_dock_depth: None,
        cooldown_minutes: None,
        cadence: None,
        anchor_date: None,
        business_calendar: None,
    }
}

fn scheduled_credential() -> CredentialRow {
    CredentialRow {
        id: "cloudflare-tunnel-credentials".into(),
        kind: "cloudflare-tunnel-credentials".into(),
        issuer: "cloudflare".into(),
        principal: "tunnel".into(),
        scopes: json!([]),
        storage_location: "boss/tunnel/credentials.json".into(),
        consumers: json!([]),
        rotation_policy: "scheduled".into(),
        rotated_at: None,
        notes: String::new(),
    }
}

fn app(
    schedule: Option<Arc<Recording>>,
    credentials: Option<Arc<dyn CredentialsRegistry>>,
) -> axum::Router {
    let jobs = Arc::new(InMemoryJobs::new());
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("operator", Action::Read, Resource::job(), Scope::All)
            // A NARROWED scope — its own packets only (backlog 0964ba80).
            .allow("sales", Action::Read, Resource::job(), Scope::Self_)
            // A department grant held OUTSIDE its department: the caller
            // sits in `it`, so this translates to no packets at all.
            .allow(
                "outsider",
                Action::Read,
                Resource::job(),
                Scope::Department("finance".into()),
            )
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let cadence: Arc<dyn CadenceRepository> = Arc::new(InMemoryCadence::new(vec![window_rule()]));
    let state = JobsApiState {
        cadence: Some(cadence),
        dispatcher_schedule: schedule.map(|s| s as Arc<dyn DispatcherSchedule>),
        credentials,
        ..JobsApiState::minimal(
            jobs,
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::FixedClockClient::new(
                boss_clock_client::ClockNow {
                    now: t(NOW),
                    simulated: false,
                    epoch_start: None,
                    epoch_end: None,
                    paused: false,
                    restart_in_progress: false,
                    warp_factor: None,
                },
            )),
        )
    };
    router(state)
}

async fn get(app: &axum::Router, role: &str) -> (StatusCode, Value) {
    let who = serde_json::to_string(&User {
        id: "emp-david".to_string(),
        role: role.to_string(),
        access_tier: AccessTier::User,
        territory_account_ids: Vec::new(),
        direct_report_ids: Vec::new(),
        department: Some("it".to_string()),
    })
    .unwrap();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/yard/regions")
                .header("x-boss-user", who)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

fn rows<'a>(v: &'a Value, kind: &str) -> Vec<&'a Value> {
    v["next_up"]
        .as_array()
        .unwrap_or_else(|| panic!("next_up is not a list: {v}"))
        .iter()
        .filter(|r| r["kind"] == kind)
        .collect()
}

#[tokio::test]
async fn next_up_folds_the_window_the_schedule_and_the_registry_asked_as_the_viewer() {
    let reader = Arc::new(Recording {
        answer: Ok(vec![ScheduledRule {
            name: "publish-to-github".into(),
            cadence: "daily".into(),
            next_due: Some(t("2026-09-28T00:00:00Z")),
            next_due_why: None,
            when: None,
        }]),
        asked_for: Mutex::new(Vec::new()),
    });
    let app = app(
        Some(reader.clone()),
        Some(Arc::new(InMemoryCredentials::new(vec![
            scheduled_credential(),
        ]))),
    );
    let (status, v) = get(&app, "operator").await;
    assert_eq!(status, StatusCode::OK, "{v}");

    let window = rows(&v, "train-window");
    assert_eq!(window.len(), 1, "{v}");
    assert_eq!(window[0]["at"], "2026-09-27T18:05:00Z");
    assert_eq!(window[0]["estimate"], false);
    for key in ["title", "basis", "source"] {
        assert!(window[0][key].is_string(), "{key}: {}", window[0]);
    }

    let scheduled = rows(&v, "scheduled");
    assert_eq!(scheduled.len(), 1, "{v}");
    assert_eq!(scheduled[0]["title"], "publish-to-github");
    assert_eq!(scheduled[0]["at"], "2026-09-28T00:00:00Z");
    assert_eq!(
        *reader.asked_for.lock().unwrap(),
        vec!["emp-david".to_string()],
        "the dispatcher is asked as the viewer"
    );

    let rotation = rows(&v, "rotation-due");
    assert_eq!(rotation.len(), 1, "{v}");
    assert!(rotation[0]["at"].is_null());
    assert!(rotation[0]["unread"].is_null());

    // Soonest first: the window (18:05) before the publish (00:00).
    let first = &v["next_up"][0];
    assert_eq!(first["kind"], "train-window", "{v}");
}

#[tokio::test]
async fn a_reader_not_wired_is_an_unread_row_never_a_missing_one() {
    let app = app(None, None);
    let (status, v) = get(&app, "operator").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    for kind in ["scheduled", "rotation-due"] {
        let r = rows(&v, kind);
        assert_eq!(r.len(), 1, "{kind}: {v}");
        assert!(
            r[0]["unread"]
                .as_str()
                .is_some_and(|w| w.contains("not wired") || w.contains("no ")),
            "{kind}: {}",
            r[0]
        );
        assert!(r[0]["at"].is_null());
    }
}

#[tokio::test]
async fn a_caller_whose_scope_reads_no_packets_gets_an_empty_row_and_no_cross_read() {
    let reader = Arc::new(Recording {
        answer: Ok(vec![]),
        asked_for: Mutex::new(Vec::new()),
    });
    let app = app(Some(reader.clone()), None);
    let (status, v) = get(&app, "guest").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["next_up"], json!([]), "{v}");
    assert!(reader.asked_for.lock().unwrap().is_empty());
}

/// A DEPARTMENT GRANT HELD OUTSIDE ITS DEPARTMENT READS NO PACKETS
/// (review 500f8a23 of backlog 0964ba80). The policy answers such a
/// caller a predicate, not `None`, and the jobs API translates it to an
/// EMPTY scope — so the map read no packets for it, but the gate beside
/// the rows asked only the policy's own answer, and the caller was
/// handed the dispatcher's schedule and the credentials registry. The
/// translated scope decides now, as it decides the rows.
#[tokio::test]
async fn a_department_grant_held_outside_its_department_reads_no_packets() {
    let reader = Arc::new(Recording {
        answer: Ok(vec![]),
        asked_for: Mutex::new(Vec::new()),
    });
    let app = app(
        Some(reader.clone()),
        Some(Arc::new(InMemoryCredentials::new(vec![
            scheduled_credential(),
        ]))),
    );
    let (status, v) = get(&app, "outsider").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["next_up"], json!([]), "{v}");
    assert!(
        reader.asked_for.lock().unwrap().is_empty(),
        "the dispatcher was asked for a caller whose scope reads no packets"
    );
}

/// A NARROWED SCOPE IS NOT HANDED THE SCHEDULE OR THE CREDENTIALS
/// REGISTRY (backlog 0964ba80). Neither read is scoped by packet: the
/// schedule names every rule the dispatcher runs and the registry every
/// credential and where it is stored. The rule the crossings and the
/// estate hosts keep (070de88c) is the rule here: below a full scope a
/// read that is not scoped by packet is not made. The row still says
/// each source is there — WITHHELD, never "could not be read" and never
/// missing, since an absent row reads as "nothing is coming". The
/// control is the operator, read off the same reader and registry.
#[tokio::test]
async fn a_narrowed_scope_is_not_handed_the_schedule_or_the_credentials() {
    let reader = Arc::new(Recording {
        answer: Ok(vec![ScheduledRule {
            name: "publish-to-github".into(),
            cadence: "daily".into(),
            next_due: Some(t("2026-09-28T00:00:00Z")),
            next_due_why: None,
            when: None,
        }]),
        asked_for: Mutex::new(Vec::new()),
    });
    let app = app(
        Some(reader.clone()),
        Some(Arc::new(InMemoryCredentials::new(vec![
            scheduled_credential(),
        ]))),
    );
    let (status, v) = get(&app, "sales").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert!(
        reader.asked_for.lock().unwrap().is_empty(),
        "the dispatcher was asked for a caller who reads only its own packets"
    );
    let text = v["next_up"].to_string();
    assert!(
        !text.contains("publish-to-github") && !text.contains("cloudflare-tunnel-credentials"),
        "a narrowed scope was handed a read that is not scoped by packet: {text}"
    );
    for kind in ["scheduled", "rotation-due"] {
        let r = rows(&v, kind);
        assert_eq!(r.len(), 1, "{kind}: one row saying so, never none: {v}");
        let why = r[0]["unread"].as_str().unwrap_or_default();
        assert!(why.contains("withheld"), "{kind}: {}", r[0]);
        // The refusal is a structured flag on the row, not only words
        // the web must recognise (backlog 1805bac0, CLAUDE.md 9a).
        assert_eq!(r[0]["withheld"], true, "{kind}: {}", r[0]);
        let basis = r[0]["basis"].as_str().unwrap_or_default();
        assert!(
            !basis.contains("could not be read"),
            "{kind}: a refusal in a failure's words: {}",
            r[0]
        );
        assert!(r[0]["at"].is_null(), "{kind}: {}", r[0]);
    }
    // The packet-scoped rows still ride: the clock window is the cadence
    // registry's, which the yard status this caller is handed reads too.
    assert_eq!(rows(&v, "train-window").len(), 1, "{v}");

    // The control: the operator, off the same reader and registry.
    let (_, operator) = get(&app, "operator").await;
    assert_eq!(
        rows(&operator, "scheduled")[0]["title"],
        "publish-to-github"
    );
    assert_eq!(
        rows(&operator, "rotation-due")[0]["title"],
        "rotation: cloudflare-tunnel-credentials"
    );
    // A full scope's rows carry no withheld flag (backlog 1805bac0).
    for r in operator["next_up"].as_array().unwrap() {
        assert!(r.get("withheld").is_none(), "{r}");
    }
    assert_eq!(
        *reader.asked_for.lock().unwrap(),
        vec!["emp-david".to_string()]
    );
}
