//! `GET /api/yard/rule-firings` — every dispatcher rule's newest firing
//! and its dead-letters, end-to-end through the real router against the
//! in-memory adapters (backlog 43c4451a, found by page-audit 08a444bc).
//!
//! The rules list at `/it/registry/rules` could not say when a rule last
//! fired or whether its handler was failing, so a stalled
//! `auto-park-on-gate-green` and an idle one painted the same row. What
//! this pins:
//!
//! 1. **Every rule the firing record holds is answered once**, with its
//!    newest instant — the rules list's second reading of the record the
//!    world map's borders already read.
//! 2. **The dead-letters roll up per rule** from the packets carrying
//!    one, inside the firing record's own retention window, naming the
//!    packet that holds the newest.
//! 3. **Unread is not empty.** A jobs API with no firing record wired,
//!    or a caller whose scope reads no packets, gets `null` with the
//!    reason — never an empty list a page would paint as "never fired"
//!    or "no failures".
//! 4. **Nor does a narrowed scope read either half** (backlog d0058c92):
//!    the record is not scoped by packet, so below a scope that reads
//!    every packet both halves are `null`, each saying it was WITHHELD.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, JobId, JobStatus, Priority, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::dispatcher_firings::{
    DispatcherFiringsRepository, InMemoryDispatcherFirings, LastFiring, RETENTION_DAYS,
};
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::{InMemoryJobs, JobsRepository};
use boss_policy_client::types::{AccessTier, User};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use chrono::{DateTime, NaiveDate, Utc};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

const NOW: &str = "2026-09-24T12:00:00Z";

struct UnreadRecord(std::sync::atomic::AtomicUsize);

#[async_trait::async_trait]
impl DispatcherFiringsRepository for UnreadRecord {
    async fn dead_letter_page(
        &self,
        _: &str,
        _: DateTime<Utc>,
        _: usize,
        _: usize,
    ) -> Result<
        boss_jobs::dispatcher_firings::DeadLetterPage,
        boss_jobs::dispatcher_firings::DispatcherFiringsError,
    > {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err(
            boss_jobs::dispatcher_firings::DispatcherFiringsError::Storage(
                "private storage diagnostic".into(),
            ),
        )
    }
    async fn last_firing(
        &self,
        _: &str,
    ) -> Result<Option<LastFiring>, boss_jobs::dispatcher_firings::DispatcherFiringsError> {
        unreachable!("detail endpoint must not read the firing rollup")
    }
    async fn last_firings(
        &self,
    ) -> Result<
        Vec<boss_jobs::dispatcher_firings::RuleLastFiring>,
        boss_jobs::dispatcher_firings::DispatcherFiringsError,
    > {
        unreachable!("detail endpoint must not read the firing rollup")
    }
    async fn unrouted_dead_letters(
        &self,
        _: DateTime<Utc>,
    ) -> Result<
        Vec<boss_jobs::dispatcher_firings::UnroutedDeadLetters>,
        boss_jobs::dispatcher_firings::DispatcherFiringsError,
    > {
        unreachable!("detail endpoint must not read the failure rollup")
    }
}

struct UnreadPolicy;

#[async_trait::async_trait]
impl PolicyClient for UnreadPolicy {
    async fn check(
        &self,
        _: &User,
        _: Action,
        _: Resource,
    ) -> Result<boss_policy_client::Decision, boss_policy_client::PolicyClientError> {
        Err(boss_policy_client::PolicyClientError::Unreachable(
            "private policy diagnostic".into(),
        ))
    }
    async fn scope_predicate(
        &self,
        _: &User,
        _: Resource,
    ) -> Result<boss_policy_client::Predicate, boss_policy_client::PolicyClientError> {
        Err(boss_policy_client::PolicyClientError::Unreachable(
            "private policy diagnostic".into(),
        ))
    }
}

#[tokio::test]
async fn retained_failures_refuse_before_repository_access_and_distinguish_outages() {
    let record = Arc::new(UnreadRecord(std::sync::atomic::AtomicUsize::new(0)));
    let policy = Arc::new(
        FakePolicyClient::builder()
            .allow("operator", Action::Read, Resource::job(), Scope::All)
            .allow("builder", Action::Read, Resource::job(), Scope::Self_)
            .build(),
    );
    let (app, _) = app_with_ports(Some(record.clone()), policy);
    let mut headers = vec![
        None,
        Some(user_header("builder")),
        Some(user_header("denied")),
    ];
    for id in boss_core::roles::ANONYMOUS_VISITOR_IDS
        .into_iter()
        .chain(["", "  "])
    {
        let mut user: Value = serde_json::from_str(&user_header("operator")).unwrap();
        user["id"] = json!(id);
        headers.push(Some(user.to_string()));
    }
    for header in headers {
        let mut request = Request::builder().uri("/api/yard/rule-firings/broker/dead-letters");
        if let Some(header) = header {
            request = request.header("x-boss-user", header);
        }
        let response = app
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(record.0.load(std::sync::atomic::Ordering::SeqCst), 0);
    }
    for (app, expected_calls) in [
        (app, 1),
        (
            app_with_ports(Some(record.clone()), Arc::new(UnreadPolicy)).0,
            1,
        ),
    ] {
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/yard/rule-firings/broker/dead-letters")
                    .header("x-boss-user", user_header("operator"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(body.get("total").is_none());
        assert!(!body.to_string().contains("private"));
        assert_eq!(
            record.0.load(std::sync::atomic::Ordering::SeqCst),
            expected_calls
        );
    }
}

#[tokio::test]
async fn retained_dead_letter_reader_refuses_withheld_and_unwired_records() {
    let (app, _) = app(None);
    for (role, expected) in [
        ("builder", StatusCode::FORBIDDEN),
        ("operator", StatusCode::SERVICE_UNAVAILABLE),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/yard/rule-firings/broker-mints-publish-token/dead-letters")
                    .header("x-boss-user", user_header(role))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected, "{role}");
    }
}

#[tokio::test]
async fn retained_dead_letter_reader_distinguishes_a_counted_empty_record_from_unread() {
    let (app, _) = app(Some(InMemoryDispatcherFirings::new(Vec::new())));
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/yard/rule-firings/broker-mints-publish-token/dead-letters")
                .header("x-boss-user", user_header("operator"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let page: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(page["data"], json!([]));
    assert_eq!(page["total"], 0);
}

#[tokio::test]
async fn retained_failure_details_require_identity_even_with_a_full_role_grant() {
    let (app, _) = app(Some(InMemoryDispatcherFirings::new(Vec::new())));
    let mut user: Value = serde_json::from_str(&user_header("operator")).unwrap();
    user["id"] = json!("");
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/yard/rule-firings/broker-mints-publish-token/dead-letters")
                .header("x-boss-user", user.to_string())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn retained_failure_http_page_keeps_complete_json_and_reports_its_window() {
    use boss_jobs::dispatcher_firings::RetainedDeadLetter;
    let detail = json!({"failures":[{"handler":"mint","error":"cause <tag> & \"quoted\""}],"nested":{"unknown":[1,true,null]}});
    let row = RetainedDeadLetter {
        firing_id: "dispatcher:broker:event-1".into(),
        rule: "broker".into(),
        fired_on: "ops.requested".into(),
        fired_at: t(NOW),
        detail: detail.clone(),
    };
    let (app, _) = app(Some(
        InMemoryDispatcherFirings::new(Vec::new()).with_retained_dead_letters(vec![row]),
    ));
    for role in ["operator", "it-lead"] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/yard/rule-firings/broker/dead-letters?limit=1&offset=0")
                    .header("x-boss-user", user_header(role))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let page: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(page["total"], 1);
        assert_eq!(page["data"][0]["detail"], detail);
        assert_eq!(page["data"][0]["firing_id"], "dispatcher:broker:event-1");
        assert_eq!(page["retention_days"], RETENTION_DAYS);
        assert_eq!(
            page["since"],
            json!(t(NOW) - chrono::Duration::days(RETENTION_DAYS))
        );
    }
}

#[tokio::test]
async fn retained_failure_http_page_refuses_invalid_query_bounds() {
    let (app, _) = app(Some(InMemoryDispatcherFirings::new(Vec::new())));
    for query in [
        "limit=0",
        "limit=101",
        "limit=-1",
        "limit=unknown",
        "offset=-1",
        "offset=18446744073709551615",
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/api/yard/rule-firings/broker/dead-letters?{query}"
                    ))
                    .header("x-boss-user", user_header("operator"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{query}");
    }
}

fn t(rfc3339: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(rfc3339).unwrap().into()
}

fn user_header(role: &str) -> String {
    serde_json::to_string(&User {
        id: "emp-david".to_string(),
        role: role.to_string(),
        access_tier: AccessTier::User,
        territory_account_ids: Vec::new(),
        direct_report_ids: Vec::new(),
        department: Some("it".to_string()),
    })
    .expect("a User always serialises")
}

fn firing(rule: &str, at: &str) -> (String, LastFiring) {
    (
        rule.to_string(),
        LastFiring {
            firing_id: format!("dispatcher:{rule}:{at}"),
            fired_on: "step.done.gate-verdict".into(),
            fired_at: t(at),
        },
    )
}

fn app(firings: Option<InMemoryDispatcherFirings>) -> (axum::Router, Arc<InMemoryJobs>) {
    let policy_client: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("operator", Action::Read, Resource::job(), Scope::All)
            // A narrow scope — its own packets — which reads NO firing
            // record since backlog d0058c92 (it did under 493cebf3).
            .allow("builder", Action::Read, Resource::job(), Scope::Self_)
            // `user_header` puts every caller in department `it`: the
            // insider's grant reads every packet, the outsider's none.
            .allow(
                "it-lead",
                Action::Read,
                Resource::job(),
                Scope::Department("it".into()),
            )
            .allow(
                "sales-lead",
                Action::Read,
                Resource::job(),
                Scope::Department("sales".into()),
            )
            .build(),
    );
    let dispatcher_firings = firings.map(|f| Arc::new(f) as Arc<dyn DispatcherFiringsRepository>);
    app_with_ports(dispatcher_firings, policy_client)
}

fn app_with_ports(
    dispatcher_firings: Option<Arc<dyn DispatcherFiringsRepository>>,
    policy_client: Arc<dyn PolicyClient>,
) -> (axum::Router, Arc<InMemoryJobs>) {
    let jobs = Arc::new(InMemoryJobs::new());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState {
        dispatcher_firings,
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy_client,
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
    (router(state), jobs)
}

fn packet(id: &str, status: JobStatus, metadata: Value) -> Job {
    Job {
        id: JobId::from_uuid(Uuid::parse_str(id).unwrap()),
        status,
        closed_on: (status == JobStatus::Closed)
            .then(|| NaiveDate::from_ymd_opt(2026, 9, 24).unwrap()),
        metadata,
        ..Job::new(
            "maintenance-sweep",
            Subject::new("custom", "s"),
            "a sweep",
            "emp-david",
            Priority::Standard,
            NaiveDate::from_ymd_opt(2026, 9, 24).unwrap(),
        )
    }
}

fn dead_letter(rule: &str, recorded_at: &str) -> Value {
    json!({ "dead_letter": {
        "rule": rule,
        "handler": "jobs.auto-park",
        "error": "POST /api/jobs returned 503",
        "failures": [format!("{rule}/jobs.auto-park: POST /api/jobs returned 503")],
        "attempts": 8,
        "class": "budget-exhausted",
        "topic": "step.done.gate-verdict",
        "event_id": "evt-1",
        "recorded_at": recorded_at,
    }})
}

async fn get(app: &axum::Router, role: &str) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/yard/rule-firings")
                .header("x-boss-user", user_header(role))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    (status, v)
}

#[tokio::test]
async fn every_rule_answers_its_newest_firing_and_its_dead_letters() {
    let (app, jobs) = app(Some(
        InMemoryDispatcherFirings::new(vec![
            firing("auto-park-on-gate-green", "2026-09-24T09:00:00Z"),
            firing("auto-park-on-gate-green", "2026-09-24T11:00:00Z"),
            firing("complete-marker-on-step-ready", "2026-09-23T08:00:00Z"),
        ])
        // 4b175523: a dead-letter on a topic that names no packet is a
        // row in the firing record, and the rollup counts it.
        .with_unrouted_dead_letters(vec![
            ("issue-invoice".into(), t("2026-09-24T10:00:00Z")),
            ("issue-invoice".into(), t("2026-09-24T10:30:00Z")),
        ]),
    ));
    let now = t(NOW);
    // The stalled shape: the rule fired at 11:00 and dead-lettered
    // after it, on two packets — one of them since closed by hand.
    for (id, status, md) in [
        (
            "11111111-1111-1111-1111-111111111111",
            JobStatus::Open,
            dead_letter("auto-park-on-gate-green", "2026-09-24T11:30:00Z"),
        ),
        (
            "22222222-2222-2222-2222-222222222222",
            JobStatus::Closed,
            dead_letter("auto-park-on-gate-green", "2026-09-24T11:45:00Z"),
        ),
        // Outside the firing record's window: not this period's failure.
        (
            "33333333-3333-3333-3333-333333333333",
            JobStatus::Open,
            dead_letter("complete-marker-on-step-ready", "2026-07-01T00:00:00Z"),
        ),
        (
            "44444444-4444-4444-4444-444444444444",
            JobStatus::Open,
            json!({ "channel": "monitoring" }),
        ),
    ] {
        jobs.create_job_at(&packet(id, status, md), now, &[])
            .await
            .unwrap();
    }

    let (status, v) = get(&app, "operator").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["retention_days"], RETENTION_DAYS);
    assert_eq!(v["now"], "2026-09-24T12:00:00Z");

    let firings = v["firings"].as_array().expect("firings read: {v}");
    assert_eq!(firings.len(), 2, "one entry per rule, never every firing");
    assert_eq!(firings[0]["rule"], "auto-park-on-gate-green");
    assert_eq!(firings[0]["fired_at"], "2026-09-24T11:00:00Z");
    assert_eq!(firings[0]["fired_on"], "step.done.gate-verdict");
    assert_eq!(firings[1]["rule"], "complete-marker-on-step-ready");
    assert_eq!(v["firings_error"], Value::Null);

    let dead = v["dead_letters"]
        .as_array()
        .expect("dead letters read: {v}");
    assert_eq!(dead.len(), 2, "{v}");
    assert_eq!(dead[0]["rule"], "auto-park-on-gate-green");
    assert_eq!(dead[0]["packets"], 2);
    assert_eq!(dead[0]["unrouted"], 0);
    assert_eq!(dead[0]["newest_at"], "2026-09-24T11:45:00Z");
    assert_eq!(
        dead[0]["newest_job_id"],
        "22222222-2222-2222-2222-222222222222"
    );
    assert_eq!(dead[1]["rule"], "issue-invoice");
    assert_eq!(dead[1]["packets"], 0);
    assert_eq!(dead[1]["unrouted"], 2, "{v}");
    assert_eq!(dead[1]["newest_at"], "2026-09-24T10:30:00Z");
    assert_eq!(dead[1]["newest_job_id"], Value::Null, "no packet to open");
    assert_eq!(v["dead_letters_error"], Value::Null);
}

#[tokio::test]
async fn an_unwired_record_and_an_unscoped_caller_answer_null_with_the_reason() {
    // No firing record wired to this jobs API: unread, and said so —
    // an empty list here would paint every rule "no firing recorded".
    let (unwired, _) = app(None);
    let (status, v) = get(&unwired, "operator").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["firings"], Value::Null, "{v}");
    assert!(
        v["firings_error"]
            .as_str()
            .is_some_and(|e| e.contains("not wired")),
        "{v}"
    );
    // The dead-letters that name no packet live in that same record, so
    // the count is unread too — never "none" missing a half (4b175523).
    assert_eq!(v["dead_letters"], Value::Null, "{v}");
    assert!(
        v["dead_letters_error"]
            .as_str()
            .is_some_and(|e| e.contains("name no packet")),
        "{v}"
    );
    // An unwired record is a failure, not a refusal: no withheld flag
    // (backlog 1805bac0).
    assert!(v.get("withheld").is_none(), "{v}");

    // A caller whose policy reads no packets cannot be told there are no
    // dead-letters on them: unknown, with the reason. Nor is it told the
    // firing record (backlog e5f7b51e): the half that asked no policy
    // answered every rule's name and newest instant to a caller with no
    // identity at all, while the half beside it refused that caller.
    let (wired, _) = app(Some(InMemoryDispatcherFirings::new(vec![firing(
        "auto-park-on-gate-green",
        "2026-09-24T11:00:00Z",
    )])));
    for (who, v) in [
        ("nobody", get(&wired, "nobody").await),
        ("no identity at all", get_unsigned(&wired).await),
    ] {
        let (status, v) = v;
        assert_eq!(status, StatusCode::OK, "{who}: {v}");
        assert_eq!(v["firings"], Value::Null, "{who}: {v}");
        assert!(
            v["firings_error"]
                .as_str()
                .is_some_and(|e| e.contains("scope")),
            "{who}: {v}"
        );
        assert_eq!(v["dead_letters"], Value::Null, "{who}: {v}");
        assert!(
            v["dead_letters_error"]
                .as_str()
                .is_some_and(|e| e.contains("scope")),
            "{who}: {v}"
        );
    }
}

/// The firing record with one rule's firing and one unrouted
/// dead-letter, over a dead-lettered packet `emp-david` owns — so a
/// `Self_` caller's own packets carry a dead-letter it could be told of.
async fn app_with_the_record() -> axum::Router {
    let (app, jobs) = app(Some(
        InMemoryDispatcherFirings::new(vec![firing(
            "auto-park-on-gate-green",
            "2026-09-24T11:00:00Z",
        )])
        .with_unrouted_dead_letters(vec![("issue-invoice".into(), t("2026-09-24T10:30:00Z"))]),
    ));
    jobs.create_job_at(
        &packet(
            "11111111-1111-1111-1111-111111111111",
            JobStatus::Open,
            dead_letter("auto-park-on-gate-green", "2026-09-24T11:30:00Z"),
        ),
        t(NOW),
        &[],
    )
    .await
    .unwrap();
    app
}

/// A NARROWED SCOPE READS NO FIRING RECORD (backlog d0058c92, review
/// abebd39c B1). This pin said the opposite until then — "the rule is
/// reads no packets, not reads every packet", backlog 493cebf3's third
/// finding. It PREDATES THE SCOPE WORK: the firing record names when
/// every rule last fired, whoever's packets it moved, which is the record
/// the IT map's borders and the dispatcher's own schedule withhold below
/// a full scope. Served here, the same instant reached the caller one
/// door over.
///
/// The dead-letters are withheld WITH it, both halves: the unrouted ones
/// are that same record, and a count of only the packet half would say
/// "none" of a rule failing only on topics that name no packet — the
/// both-or-neither rule of 4b175523. Each half says WITHHELD, never
/// "could not be read". A department grant held outside its department
/// translates to no packets at all, and is told that.
#[tokio::test]
async fn a_narrowed_scope_reads_no_firing_record_and_no_dead_letters() {
    let app = app_with_the_record().await;
    for (who, role, says) in [
        (
            "its own packets only",
            "builder",
            "does not read every packet",
        ),
        (
            "a department grant outside its department",
            "sales-lead",
            "reads no packets",
        ),
    ] {
        let (status, v) = get(&app, role).await;
        assert_eq!(status, StatusCode::OK, "{who}: {v}");
        for (half, why) in [
            ("firings", "firings_error"),
            ("dead_letters", "dead_letters_error"),
        ] {
            assert_eq!(v[half], Value::Null, "{who}: {half}: {v}");
            let why = v[why].as_str().unwrap_or_default();
            assert!(
                why.contains("scope") && why.contains(says),
                "{who}: {half} withheld by scope, and says so: {v}"
            );
            assert!(!why.contains("could not"), "{who}: {half}: {v}");
        }
        // The refusal is a structured flag, not only prose the web has
        // to recognise (backlog 1805bac0, CLAUDE.md 9a).
        assert_eq!(v["withheld"], true, "{who}: {v}");
        let text = v.to_string();
        for instant in ["11:00", "10:30", "11:30"] {
            assert!(!text.contains(instant), "{who}: {instant} leaked: {text}");
        }
    }
}

/// The operator control: a scope that reads every packet — unrestricted,
/// or a department grant inside its department — reads both halves off
/// the same record, so the narrowed pin cannot pass on a door that
/// withholds from everyone.
#[tokio::test]
async fn a_scope_that_reads_every_packet_reads_the_firing_record() {
    let app = app_with_the_record().await;
    for role in ["operator", "it-lead"] {
        let (status, v) = get(&app, role).await;
        assert_eq!(status, StatusCode::OK, "{role}: {v}");
        assert_eq!(v["firings_error"], Value::Null, "{role}: {v}");
        assert_eq!(v["firings"][0]["rule"], "auto-park-on-gate-green", "{v}");
        assert_eq!(v["firings"][0]["fired_at"], "2026-09-24T11:00:00Z", "{v}");
        assert_eq!(v["dead_letters_error"], Value::Null, "{role}: {v}");
        let dead = v["dead_letters"].as_array().expect("dead letters read");
        assert_eq!(dead.len(), 2, "{role}: {v}");
        // A full scope's payload is unchanged: no withheld flag at all
        // (backlog 1805bac0).
        assert!(v.get("withheld").is_none(), "{role}: {v}");
    }
}

/// The request exactly as the anonymous caller arrives: no header of
/// any kind (what the gateway forwards for a sessionless route).
async fn get_unsigned(app: &axum::Router) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/yard/rule-firings")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}
