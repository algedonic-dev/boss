//! What the login door LOGS about the `agent-role` word — asserted in a
//! process of its own.
//!
//! **This file holds exactly one test, and that is the point**, for the
//! reason `station_boot_log.rs` gives: `tracing` keeps its
//! callsite-interest cache in process-global state while
//! `set_default` installs a subscriber for one thread, so a sibling test
//! reaching the same callsite with no subscriber can leave the capturing
//! test reading an empty log. It happened to this very assertion: the
//! reviewer's `the_report_line` sat beside seven siblings and FAILED
//! under every one of the eleven mutants, including ones that cannot
//! touch it (review cdff423f, mutants.out) — so "M10 is killed" was
//! noise. Here the subscriber is global and nothing runs beside it.
//!
//! Two facts, one log (backlog 4e51bf23):
//!
//!  - `report` says its one line (mutant M10): once for a request whose
//!    row differs from what it asserted, naming the agent and both
//!    roles; never for a row that agrees, a person, or the word at
//!    `off`. That line is the only place `report` says anything, so a
//!    door that never says it has no rehearsal.
//!  - the storage error the 503 no longer hands the caller (review N4)
//!    is in the server's log at ERROR, for both registry reads. Taking
//!    it out of the body without keeping it here would be the reduction
//!    that discards the only copy.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::any;
use boss_core::machine_gate::Mode;
use boss_core::port::EventBus;
use boss_core::publish::PublishMode;
use boss_core::publisher::{DomainPublisher, EventStamp};
use boss_jobs::agents::{
    AgentInput, AgentRow, AgentsBatchOutcome, AgentsError, AgentsRegistry, AutomationActor,
    AutomationsSeedOutcome, InMemoryAgents, LoginDoor, resolve_login,
};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use tower::ServiceExt;

use common::Captured;

const STORAGE_DETAIL: &str = "error communicating with database: host=10.9.9.9 user=boss";
const REPORT_LINE: &str =
    "agent-role report: under enforce this request would be judged by its row";

struct FailingRegistry {
    inner: InMemoryAgents,
    alias_read_fails: AtomicBool,
    listing_fails: AtomicBool,
}

#[async_trait]
impl AgentsRegistry for FailingRegistry {
    async fn resolve_login(&self, login: &str) -> Result<Option<String>, AgentsError> {
        if self.alias_read_fails.load(Ordering::SeqCst) {
            return Err(AgentsError::Storage(STORAGE_DETAIL.into()));
        }
        self.inner.resolve_login(login).await
    }

    async fn list(&self) -> Result<Vec<AgentRow>, AgentsError> {
        if self.listing_fails.load(Ordering::SeqCst) {
            return Err(AgentsError::Storage(STORAGE_DETAIL.into()));
        }
        self.inner.list().await
    }

    async fn publish(
        &self,
        rows: &[AgentInput],
        mode: PublishMode,
        stamp: &EventStamp,
    ) -> Result<AgentsBatchOutcome, AgentsError> {
        self.inner.publish(rows, mode, stamp).await
    }

    async fn list_automations(&self) -> Result<Vec<AutomationActor>, AgentsError> {
        self.inner.list_automations().await
    }

    async fn declare_automations(
        &self,
        rows: &[AutomationActor],
        stamp: &EventStamp,
    ) -> Result<AutomationsSeedOutcome, AgentsError> {
        self.inner.declare_automations(rows, stamp).await
    }
}

fn agent(id: &str, login: &str, role: Option<&str>) -> AgentInput {
    AgentInput {
        id: id.to_string(),
        display_name: id.to_string(),
        default_model: "opus-5".to_string(),
        aliases: vec![login.to_string()],
        role: role.map(str::to_string),
        department: None,
        hourly_budget_usd_micros: None,
        max_concurrent_runs: None,
    }
}

async fn world(mode: Mode) -> (axum::Router, Arc<FailingRegistry>) {
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let publisher = DomainPublisher::new(bus_dyn, "jobs");
    let inner = InMemoryAgents::new();
    let stamp = publisher.stamp_with_actor(publisher.default_actor()).await;
    inner
        .publish(
            &[
                agent(
                    "agent-claude",
                    "claude@algedonic.dev",
                    Some("engineering-agent"),
                ),
                agent("agent-admin", "admin@algedonic.dev", Some("platform-admin")),
            ],
            PublishMode::InsertIfAbsent,
            &stamp,
        )
        .await
        .expect("declare the agents");
    let registry = Arc::new(FailingRegistry {
        inner,
        alias_read_fails: AtomicBool::new(false),
        listing_fails: AtomicBool::new(false),
    });
    let door = Arc::new(
        LoginDoor::new(registry.clone() as Arc<dyn AgentsRegistry>, publisher)
            .judging_rows_when(move || mode),
    );
    let app = axum::Router::new()
        .route("/echo", any(|| async { "ok" }))
        .layer(axum::middleware::from_fn_with_state(door, resolve_login));
    (app, registry)
}

/// One request asserting `platform-admin` at operator tier; answers the
/// status and the body's text.
async fn call(app: &axum::Router, id: &str) -> (StatusCode, String) {
    let user = format!(
        r#"{{"id":"{id}","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":null}}"#
    );
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/echo")
                .header("x-boss-user", user)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn report_says_its_line_once_and_a_dark_registrys_detail_stays_in_the_log() {
    let log = Captured::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(log.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .expect("this binary holds one test, so nothing else has claimed the subscriber");
    // What the log gained since `from`.
    let since = |from: usize| log.text()[from..].to_string();

    // REPORT: one line for a row that differs, naming the agent, what it
    // asserted and what its row holds.
    let (app, registry) = world(Mode::Report).await;
    let mark = log.text().len();
    let (status, _) = call(&app, "claude@algedonic.dev").await;
    assert_eq!(status, StatusCode::OK);
    let said = since(mark);
    assert_eq!(
        said.matches(REPORT_LINE).count(),
        1,
        "report says its line once for a request whose row differs: {said:?}"
    );
    let line = said
        .lines()
        .find(|l| l.contains(REPORT_LINE))
        .expect("the line");
    assert!(line.contains("INFO"), "{line}");
    for part in [
        "actor=agent-claude",
        "asserted_role=platform-admin",
        "engineering-agent",
    ] {
        assert!(line.contains(part), "the line names {part}: {line}");
    }

    // ...and silence for a row that agrees with what was sent, and for a
    // person, who is not the registry's question.
    for id in ["admin@algedonic.dev", "emp-david"] {
        let mark = log.text().len();
        let (status, _) = call(&app, id).await;
        assert_eq!(status, StatusCode::OK);
        let said = since(mark);
        assert_eq!(said.matches(REPORT_LINE).count(), 0, "{id}: {said:?}");
    }

    // REPORT with the listing dark: admitted, and the log says nothing
    // was judged and why.
    registry.listing_fails.store(true, Ordering::SeqCst);
    let mark = log.text().len();
    let (status, _) = call(&app, "agent-claude").await;
    assert_eq!(status, StatusCode::OK);
    let said = since(mark);
    assert!(said.contains("nothing is judged in report"), "{said:?}");
    assert!(said.contains(STORAGE_DETAIL), "{said:?}");
    assert_eq!(said.matches(REPORT_LINE).count(), 0, "{said:?}");

    // OFF: the door says nothing about the word at all.
    let (app, _) = world(Mode::Off).await;
    let mark = log.text().len();
    let (status, _) = call(&app, "claude@algedonic.dev").await;
    assert_eq!(status, StatusCode::OK);
    let said = since(mark);
    assert!(!said.contains("agent-role"), "off says nothing: {said:?}");

    // ENFORCE, registry dark, each read in turn: the caller's 503 holds
    // none of the storage error and the server's log holds all of it, at
    // ERROR, beside the id that was refused.
    let (app, registry) = world(Mode::Enforce).await;
    for (alias_dark, listing_dark, signed, logged_as) in [
        (
            true,
            false,
            "claude@algedonic.dev",
            "login=claude@algedonic.dev",
        ),
        (false, true, "claude@algedonic.dev", "actor=agent-claude"),
    ] {
        registry
            .alias_read_fails
            .store(alias_dark, Ordering::SeqCst);
        registry.listing_fails.store(listing_dark, Ordering::SeqCst);
        let mark = log.text().len();
        let (status, body) = call(&app, signed).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
        assert!(
            !body.contains(STORAGE_DETAIL) && !body.contains("database"),
            "the caller is not handed the storage error: {body}"
        );
        let said = since(mark);
        let line = said
            .lines()
            .find(|l| l.contains("refused 503"))
            .unwrap_or_else(|| panic!("the refusal is logged: {said:?}"));
        assert!(line.contains("ERROR"), "{line}");
        assert!(
            line.contains(STORAGE_DETAIL),
            "the detail the caller no longer reads is kept for the operator: {line}"
        );
        assert!(line.contains(logged_as), "{line}");
    }
}
