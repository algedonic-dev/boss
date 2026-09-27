//! `GET /api/dispatcher/schedule` — each scheduled rule's last firing and
//! next due time, end to end through the dispatcher's real router
//! (design ea906603, car 2 of the top board's plan).
//!
//! The top board's NEXT UP row needs "scheduled rules due next", and until
//! this read nothing served it: `/api/dispatcher/rules` carries each
//! schedule's cadence and anchor with no time of day and no firing, and
//! the firing record (`dispatcher_firings`) says only what already ran.
//! What this pins:
//!
//! 1. **Every scheduled rule the dispatcher enforces is answered**, from
//!    the shipped registry seeded the way the dispatcher boots it, with a
//!    next due time and its last firing (or the reason it has none).
//! 2. **The scope rule of the machine-firings car** (backlog e5f7b51e,
//!    carried to the rule reads by 493cebf3): a caller whose policy scope
//!    reads no packets — a request with no identity is one — gets the
//!    schedule `null`, and the rules withheld, with a scope reason, and
//!    never reaches the rule table; a narrow scope still reads; a policy
//!    service that cannot answer is refused, not waved through. `readyz`
//!    is the one read left open, and it carries liveness numbers only.

mod common;

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_dispatcher::cascade::Cascade;
use boss_dispatcher::http::{HttpState, router};
use boss_dispatcher::liveness::DispatcherLiveness;
use boss_jobs::dispatcher_firings::{
    DispatcherFiringsRepository, InMemoryDispatcherFirings, LastFiring,
};
use boss_policy_client::types::{AccessTier, User};
use boss_policy_client::{
    Action, Decision, FakePolicyClient, PolicyClient, PolicyClientError, Predicate, Resource, Scope,
};
use boss_testing::TestDb;
use chrono::{DateTime, Utc};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

const NOW: &str = "2026-09-27T14:32:10Z";

fn t(rfc3339: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(rfc3339).unwrap().into()
}

fn policy() -> Arc<dyn PolicyClient> {
    Arc::new(
        FakePolicyClient::builder()
            .allow("operator", Action::Read, Resource::job(), Scope::All)
            .allow("builder", Action::Read, Resource::job(), Scope::Self_)
            .build(),
    )
}

/// A policy service that cannot be asked — the outage shape, which is an
/// error and must never read as an allow.
struct UnreachablePolicy;

#[async_trait]
impl PolicyClient for UnreachablePolicy {
    async fn check(
        &self,
        _user: &User,
        _action: Action,
        _resource: Resource,
    ) -> Result<Decision, PolicyClientError> {
        Err(PolicyClientError::Unreachable("connection refused".into()))
    }

    async fn scope_predicate(
        &self,
        _user: &User,
        _resource: Resource,
    ) -> Result<Predicate, PolicyClientError> {
        Err(PolicyClientError::Unreachable("connection refused".into()))
    }
}

fn state(pool: sqlx::PgPool, policy: Arc<dyn PolicyClient>) -> HttpState {
    let firings: Arc<dyn DispatcherFiringsRepository> =
        Arc::new(InMemoryDispatcherFirings::new(vec![(
            "cadence-silence-sweep-daily".to_string(),
            LastFiring {
                firing_id: "dispatcher:cadence-silence-sweep-daily:clock-day:2026-09-27".into(),
                fired_on: "clock.day".into(),
                fired_at: t("2026-09-27T00:00:03Z"),
            },
        )]));
    HttpState {
        live: Arc::new(DispatcherLiveness::default()),
        pool,
        authored_rules_dir: None,
        cascade: Arc::new(Cascade::default()),
        policy,
        clock: Arc::new(boss_clock_client::FixedClockClient::new(
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
        calendar: Arc::new(boss_calendar_client::FakeCalendarClient::new()),
        firings: Some(firings),
    }
}

/// A pool that answers nothing: port 1 refuses. A caller the scope
/// refuses must never reach the rule table, so a handler that touched
/// this would report a load error, not the scope reason.
fn unreachable_pool() -> sqlx::PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(std::time::Duration::from_secs(2))
        .connect_lazy("postgres://boss:boss@127.0.0.1:1/boss")
        .expect("a lazy pool never connects at construction")
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

async fn get(app: &axum::Router, role: Option<&str>) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method("GET")
        .uri("/api/dispatcher/schedule");
    if let Some(role) = role {
        req = req.header("x-boss-user", user_header(role));
    }
    let resp = app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

fn row<'a>(v: &'a Value, name: &str) -> &'a Value {
    v["schedule"]
        .as_array()
        .and_then(|rows| rows.iter().find(|r| r["name"] == name))
        .unwrap_or_else(|| panic!("no schedule row {name}: {v}"))
}

#[tokio::test(flavor = "multi_thread")]
async fn every_scheduled_rule_answers_its_last_firing_and_next_due() {
    let db = TestDb::new().await;
    let shipped = common::shipped_raw_rules(&db).await;
    let scheduled: Vec<&str> = shipped
        .rules
        .iter()
        .filter(|r| r.schedule.is_some())
        .map(|r| r.name.as_str())
        .collect();
    assert!(
        scheduled.contains(&"cadence-silence-sweep-daily"),
        "the fixture's firing names a rule the tree still ships: {scheduled:?}"
    );

    let app = router(state(db.pool.clone(), policy()));
    for role in ["operator", "builder"] {
        let (status, v) = get(&app, Some(role)).await;
        assert_eq!(status, StatusCode::OK, "{role}: {v}");
        assert!(v["schedule_error"].is_null(), "{role}: {v}");
        assert_eq!(v["now"], "2026-09-27T14:32:10Z", "{role}: {v}");
        let rows = v["schedule"].as_array().expect("the schedule is read");
        assert_eq!(
            rows.len(),
            scheduled.len(),
            "{role}: one row per scheduled rule, no event rule: {v}"
        );

        let sweep = row(&v, "cadence-silence-sweep-daily");
        assert_eq!(sweep["cadence"], "daily");
        assert_eq!(sweep["last_fired"]["at"], "2026-09-27T00:00:03Z");
        assert_eq!(sweep["next_due"], "2026-09-28T00:00:00Z");
        for r in rows {
            // Never omitted: a last firing or the reason there is none,
            // and a next due or the reason there is none.
            assert!(
                !r["last_fired"].is_null() || r["last_fired_why"].is_string(),
                "{role}: {r}"
            );
            assert!(
                !r["next_due"].is_null() || r["next_due_why"].is_string(),
                "{role}: {r}"
            );
        }
    }
}

/// The scope rule the machine-firings car applied to the yard's reads
/// (backlog e5f7b51e), applied here from the first day: a caller whose
/// scope reads no packets reads nothing about the machinery that moves
/// them — and a caller with NO identity is one.
#[tokio::test(flavor = "multi_thread")]
async fn a_caller_who_reads_no_packets_reads_no_schedule() {
    let app = router(state(unreachable_pool(), policy()));
    for (who, role) in [
        ("a denied role", Some("nobody")),
        ("no identity at all", None),
    ] {
        let (status, v) = get(&app, role).await;
        assert_eq!(status, StatusCode::OK, "{who}: {v}");
        assert_eq!(v["schedule"], Value::Null, "{who}: {v}");
        assert!(
            v["schedule_error"]
                .as_str()
                .is_some_and(|e| e.contains("scope") && e.contains("reads no packets")),
            "{who}: withheld by scope, and says so rather than 'could not be read': {v}"
        );
    }
}

/// Any GET on the dispatcher's router, as `role` (`None`: no header).
async fn get_at(app: &axum::Router, uri: &str, role: Option<&str>) -> (StatusCode, String) {
    let mut req = Request::builder().method("GET").uri(uri);
    if let Some(role) = role {
        req = req.header("x-boss-user", user_header(role));
    }
    let resp = app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&body).into_owned())
}

/// The rule reads, each a read of the registry that names every rule.
const RULE_READS: [&str; 3] = [
    "/api/dispatcher/rules",
    "/api/dispatcher/rules/auto-park-on-gate-green/versions",
    "/api/dispatcher/rules/auto-park-on-gate-green/versions/1",
];

/// THE SCOPE RULE, APPLIED TO THE RULE READS (backlog 493cebf3). Until
/// then `GET /api/dispatcher/rules` and its version reads asked nothing,
/// so any gateway session — a basic guest that reads no packets among
/// them — read every rule's name, trigger, guard and handler, beside a
/// schedule that refused the same caller. Now that caller is told the
/// rules are withheld, by scope, and the table is never reached: the
/// pool here refuses every connection, so a handler that touched it would
/// answer a load error instead.
#[tokio::test(flavor = "multi_thread")]
async fn a_caller_who_reads_no_packets_reads_no_rules() {
    let app = router(state(unreachable_pool(), policy()));
    for (who, role) in [
        ("a denied role", Some("nobody")),
        ("no identity at all", None),
    ] {
        let (status, body) = get_at(&app, "/api/dispatcher/rules", role).await;
        // The feed's own failure shape, which every reader already
        // treats as a failed read (the cascade, the rules page, the
        // departments readiness read): `error` set, nothing listed.
        assert_eq!(status, StatusCode::OK, "{who}: {body}");
        let v: Value = serde_json::from_str(&body).unwrap();
        assert!(
            v["error"]
                .as_str()
                .is_some_and(|e| e.contains("scope") && e.contains("withheld")),
            "{who}: withheld by scope, and says so: {v}"
        );
        assert_eq!(v["rules"], serde_json::json!([]), "{who}: {v}");
        assert_eq!(v["handler_emits"], serde_json::json!({}), "{who}: {v}");
        assert_eq!(v["system_edges"], serde_json::json!([]), "{who}: {v}");

        for uri in &RULE_READS[1..] {
            let (status, body) = get_at(&app, uri, role).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{who}: GET {uri}: {body}");
            assert!(
                body.contains("scope") && body.contains("withheld"),
                "{who}: GET {uri}: {body}"
            );
        }
    }
}

/// The narrow scope still reads (backlog 493cebf3's third finding): the
/// rule is "reads no packets", not "reads every packet", so a builder
/// whose job scope is its own packets reads the rules as the operator
/// does. A later tightening to an unrestricted-only gate fails here.
#[tokio::test(flavor = "multi_thread")]
async fn a_narrow_scope_still_reads_the_rules() {
    let db = TestDb::new().await;
    common::shipped_raw_rules(&db).await;
    let app = router(state(db.pool.clone(), policy()));
    for role in ["operator", "builder"] {
        for uri in RULE_READS {
            let (status, body) = get_at(&app, uri, Some(role)).await;
            assert_eq!(status, StatusCode::OK, "{role}: GET {uri}: {body}");
            assert!(
                body.contains("auto-park-on-gate-green"),
                "{role}: GET {uri}: {body}"
            );
            assert!(!body.contains("withheld"), "{role}: GET {uri}: {body}");
        }
    }
}

/// A policy outage withholds the rule reads (fail closed, D9) and says
/// it was the policy check — never a rule list waved through.
#[tokio::test(flavor = "multi_thread")]
async fn a_policy_outage_withholds_the_rules() {
    let app = router(state(unreachable_pool(), Arc::new(UnreachablePolicy)));
    for uri in RULE_READS {
        let (_, body) = get_at(&app, uri, Some("operator")).await;
        assert!(body.contains("policy check failed"), "GET {uri}: {body}");
        assert!(
            !body.contains("auto-park-on-gate-green\""),
            "GET {uri}: {body}"
        );
    }
}

/// `readyz` STAYS OPEN, and names nothing (backlog 493cebf3, the
/// operator's ruling on the finding): its readers — the estate observer's
/// dead-letter read, the OSS quickstart's launch wait, a tenant sim's
/// pre-Go gate — read it with no identity, and an alarm that must ask
/// policy before it can speak is an arm that needs the patient
/// (CLAUDE.md §Diagnosis). So it answers a caller with no identity under
/// a policy outage, and every value it carries is a number or a flag:
/// liveness, never a rule's name.
#[tokio::test(flavor = "multi_thread")]
async fn readyz_answers_anyone_and_names_no_rule() {
    let app = router(state(unreachable_pool(), Arc::new(UnreachablePolicy)));
    let (status, body) = get_at(&app, "/api/dispatcher/readyz", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    let fields = v.as_object().expect("readyz is an object");
    assert!(fields.contains_key("ready"), "{v}");
    assert!(fields.contains_key("dead_letters"), "{v}");
    for (k, val) in fields {
        assert!(
            val.is_number() || val.is_boolean(),
            "readyz carries liveness only; `{k}` is {val}"
        );
    }
}

/// A policy service that cannot answer is refused, never read as an
/// allow (fail closed, D9) — and says it was the policy check.
#[tokio::test(flavor = "multi_thread")]
async fn a_policy_outage_withholds_the_schedule() {
    let app = router(state(unreachable_pool(), Arc::new(UnreachablePolicy)));
    let (status, v) = get(&app, Some("operator")).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["schedule"], Value::Null, "{v}");
    assert!(
        v["schedule_error"]
            .as_str()
            .is_some_and(|e| e.contains("policy check failed")),
        "{v}"
    );
}
