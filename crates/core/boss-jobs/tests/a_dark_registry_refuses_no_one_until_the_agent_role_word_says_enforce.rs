//! A dark agents registry refuses no one until the `agent-role` word
//! says `enforce` — and under `enforce` it refuses, it does not admit.
//!
//! Backlog 4e51bf23, review cdff423f N6 and N4. The car that taught the
//! login door to judge a registered agent by its row lands with the word
//! ABSENT, which is `off`, and its whole claim on landing is "nothing
//! changes for any caller". The reviewer ran eleven mutants against the
//! car's own tests and five survived, the first of them the one the
//! landing rests on:
//!
//!   M5   the 503 arm in ANY mode — at `off`, one registry blip refuses
//!        every agent and every address-shaped login;
//!   M4   `off` still lists the registry on every agent request;
//!   M6   `enforce` fails OPEN on a dark alias read;
//!   M1b  the binary's wiring answers `enforce` whatever the file says;
//!   M10  the `report` line is never said (pinned in a process of its
//!        own: `the_agent_role_door_says_what_it_would_judge_and_keeps_
//!        the_storage_error_in_the_log.rs`).
//!
//! These are the reviewer's attack tests, committed. The registry here
//! is the in-memory one behind a double that counts both reads and can
//! be made to fail either, with a storage error whose text looks like
//! what a database driver says — because N4 is that the 503 used to
//! hand that text to whoever signed `agent-x`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::routing::any;
use boss_core::machine_gate::{Mode, ModeSwitch, read_mode};
use boss_core::port::EventBus;
use boss_core::publish::PublishMode;
use boss_core::publisher::{DomainPublisher, EventStamp};
use boss_jobs::agents::{
    AgentInput, AgentRow, AgentsBatchOutcome, AgentsError, AgentsRegistry, AutomationActor,
    AutomationsSeedOutcome, InMemoryAgents, LoginDoor, ROW_ROLE_MODE_FILE, resolve_login,
};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use tower::ServiceExt;

/// What the double's storage error says: the shape of a driver's text,
/// with a host and a user in it. No refusal body may carry any of it.
const STORAGE_DETAIL: &str = "error communicating with database: host=10.9.9.9 user=boss";

/// The in-memory registry, with both of the door's reads counted and
/// each able to fail.
struct CountingRegistry {
    inner: InMemoryAgents,
    alias_read_fails: AtomicBool,
    listing_fails: AtomicBool,
    alias_reads: AtomicUsize,
    listings: AtomicUsize,
}

impl CountingRegistry {
    fn darken(&self) {
        self.alias_read_fails.store(true, Ordering::SeqCst);
        self.listing_fails.store(true, Ordering::SeqCst);
    }
}

#[async_trait]
impl AgentsRegistry for CountingRegistry {
    async fn resolve_login(&self, login: &str) -> Result<Option<String>, AgentsError> {
        self.alias_reads.fetch_add(1, Ordering::SeqCst);
        if self.alias_read_fails.load(Ordering::SeqCst) {
            return Err(AgentsError::Storage(STORAGE_DETAIL.into()));
        }
        self.inner.resolve_login(login).await
    }

    async fn list(&self) -> Result<Vec<AgentRow>, AgentsError> {
        self.listings.fetch_add(1, Ordering::SeqCst);
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

/// Answers the identity the handler was handed, so a test reads what
/// the door let through.
async fn echo(headers: HeaderMap) -> axum::Json<serde_json::Value> {
    let seen: serde_json::Value = headers
        .get("x-boss-user")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(serde_json::Value::Null);
    axum::Json(serde_json::json!({ "seen": seen }))
}

/// The live registry as measured 2026-10-08 — `agent-claude` holding
/// `engineering-agent`, `agent-codex` holding `null` — behind the door,
/// which is built by `wire`.
async fn world_wired(
    wire: impl FnOnce(LoginDoor) -> LoginDoor,
) -> (axum::Router, Arc<CountingRegistry>) {
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
                agent("agent-codex", "codex@algedonic.dev", None),
            ],
            PublishMode::InsertIfAbsent,
            &stamp,
        )
        .await
        .expect("declare the agents");
    let registry = Arc::new(CountingRegistry {
        inner,
        alias_read_fails: AtomicBool::new(false),
        listing_fails: AtomicBool::new(false),
        alias_reads: AtomicUsize::new(0),
        listings: AtomicUsize::new(0),
    });
    let door = Arc::new(wire(LoginDoor::new(
        registry.clone() as Arc<dyn AgentsRegistry>,
        publisher,
    )));
    let app = axum::Router::new()
        .route("/echo", any(echo))
        .layer(axum::middleware::from_fn_with_state(door, resolve_login));
    (app, registry)
}

async fn world(mode: Mode) -> (axum::Router, Arc<CountingRegistry>) {
    world_wired(|door| door.judging_rows_when(move || mode)).await
}

/// The header the pod's doors build — `platform-admin` at operator tier
/// whoever the id is — with every scope field filled, so a field the
/// door dropped or rewrote shows.
fn asserting_admin(id: &str) -> String {
    format!(
        r#"{{"id":"{id}","role":"platform-admin","access_tier":"operator","territory_account_ids":["acct-9"],"direct_report_ids":["emp-x"],"department":"finance"}}"#
    )
}

async fn call(app: &axum::Router, method: &str, id: &str) -> (StatusCode, serde_json::Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri("/echo")
                .header("x-boss-user", asserting_admin(id))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let json = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| serde_json::Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, json)
}

/// Every shape of id that reaches the jobs port: two aliases, a
/// registered id, an agent id with no row, a person, an automation, the
/// unnamed reader, an address no alias maps, and the break-glass
/// session.
const CALLERS: [&str; 9] = [
    "claude@algedonic.dev",
    "agent-claude",
    "codex@algedonic.dev",
    "agent-ghost",
    "emp-david",
    "automation:train-conductor",
    "operator:unidentified",
    "nobody@example.test",
    "break-glass-operator",
];

/// The callers `enforce` asks the registry about: an address (it may be
/// an alias) and an id in the registry's own namespace.
fn is_asked_of_the_registry(id: &str) -> bool {
    id.contains('@') || id.starts_with("agent-")
}

fn every_asserted_field_arrived(seen: &serde_json::Value, who: &str) {
    assert_eq!(seen["role"], "platform-admin", "{who}: {seen}");
    assert_eq!(seen["access_tier"], "operator", "{who}: {seen}");
    assert_eq!(seen["department"], "finance", "{who}: {seen}");
    assert_eq!(
        seen["territory_account_ids"],
        serde_json::json!(["acct-9"]),
        "{who}: {seen}"
    );
    assert_eq!(
        seen["direct_report_ids"],
        serde_json::json!(["emp-x"]),
        "{who}: {seen}"
    );
}

/// OFF IS MAIN (mutant M4). For every caller on a read and on a write,
/// every field arrives as sent and only an alias's id is resolved; and
/// the door makes the one alias read main made and never lists the
/// registry.
#[tokio::test]
async fn off_passes_every_asserted_field_and_reads_the_registry_once_as_before() {
    let (app, registry) = world(Mode::Off).await;
    for method in ["GET", "POST"] {
        for id in CALLERS {
            let who = format!("{method} signed {id}");
            let (status, body) = call(&app, method, id).await;
            assert_eq!(status, StatusCode::OK, "{who}: {body}");
            every_asserted_field_arrived(&body["seen"], &who);
            let want = match id {
                "claude@algedonic.dev" => "agent-claude",
                "codex@algedonic.dev" => "agent-codex",
                other => other,
            };
            assert_eq!(body["seen"]["id"], want, "{who}");
        }
    }
    assert_eq!(
        registry.listings.load(Ordering::SeqCst),
        0,
        "at off the door never lists the agents registry: a listing per agent request is a \
         cost and a failure road main does not have"
    );
    assert_eq!(
        registry.alias_reads.load(Ordering::SeqCst),
        2 * CALLERS.len(),
        "one alias read per request, the read main already made"
    );
}

/// THE PROPERTY THE LANDING RESTS ON (mutant M5). With the word absent
/// the car is live at `off`, and a registry that cannot answer either
/// read must refuse no one: every caller is answered 200 with what it
/// sent, as on main, and no 503 exists.
#[tokio::test]
async fn off_with_a_dark_registry_answers_every_caller_as_before() {
    let (app, registry) = world(Mode::Off).await;
    registry.darken();
    for method in ["GET", "POST"] {
        for id in CALLERS {
            let who = format!("{method} signed {id}, registry dark, word off");
            let (status, body) = call(&app, method, id).await;
            assert_eq!(status, StatusCode::OK, "{who}: {body}");
            every_asserted_field_arrived(&body["seen"], &who);
            assert_eq!(
                body["seen"]["id"], id,
                "{who}: an id nothing could resolve passes as it arrived"
            );
        }
    }
    assert_eq!(registry.listings.load(Ordering::SeqCst), 0);
}

/// `report` refuses nothing either, dark or not: it is the word's
/// rehearsal, and a rehearsal that can 503 is an outage.
#[tokio::test]
async fn report_with_a_dark_registry_answers_every_caller_as_before() {
    let (app, registry) = world(Mode::Report).await;
    for id in CALLERS {
        let (status, body) = call(&app, "POST", id).await;
        assert_eq!(status, StatusCode::OK, "{id}: {body}");
        every_asserted_field_arrived(&body["seen"], id);
    }
    assert_eq!(
        registry.listings.load(Ordering::SeqCst),
        4,
        "report lists once for each request an agent signed (two aliases, a registered id, an \
         agent id with no row) and for no one else"
    );
    registry.listing_fails.store(true, Ordering::SeqCst);
    let (status, body) = call(&app, "POST", "claude@algedonic.dev").await;
    assert_eq!(status, StatusCode::OK, "listing dark: {body}");
    every_asserted_field_arrived(&body["seen"], "listing dark");
    registry.darken();
    for id in CALLERS {
        let (status, body) = call(&app, "POST", id).await;
        assert_eq!(status, StatusCode::OK, "both reads dark, {id}: {body}");
        every_asserted_field_arrived(&body["seen"], id);
    }
}

/// ENFORCE FAILS CLOSED (mutant M6). A dark alias read under `enforce`
/// is a 503 for every id the registry would have been asked about, never
/// the asserted role passed through; a person, an automation, the
/// unnamed reader and the break-glass session are not the registry's
/// question and still pass, so the operator's own roads survive it.
#[tokio::test]
async fn enforce_with_a_dark_alias_read_refuses_an_agent_and_admits_a_person() {
    let (app, registry) = world(Mode::Enforce).await;
    registry.alias_read_fails.store(true, Ordering::SeqCst);
    for id in CALLERS {
        let (status, body) = call(&app, "GET", id).await;
        if is_asked_of_the_registry(id) {
            assert_eq!(
                status,
                StatusCode::SERVICE_UNAVAILABLE,
                "{id}: a registry that cannot answer must not be the way around the row: {body}"
            );
            assert!(
                body.get("seen").is_none(),
                "{id}: the handler was never reached: {body}"
            );
        } else {
            assert_eq!(status, StatusCode::OK, "{id}: {body}");
            every_asserted_field_arrived(&body["seen"], id);
        }
    }
}

/// The second read fails closed too: the alias resolved, the listing
/// did not, and the agent's role is as unknown as before. An address no
/// alias maps is not listed for, and passes.
#[tokio::test]
async fn enforce_with_a_dark_listing_refuses_an_agent_by_its_registered_id() {
    let (app, registry) = world(Mode::Enforce).await;
    registry.listing_fails.store(true, Ordering::SeqCst);
    for (signed, actor) in [
        ("claude@algedonic.dev", "agent-claude"),
        ("agent-claude", "agent-claude"),
        ("agent-ghost", "agent-ghost"),
    ] {
        let (status, body) = call(&app, "GET", signed).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{signed}: {body}");
        assert_eq!(body["actor"], actor, "{signed}: {body}");
    }
    let (status, body) = call(&app, "GET", "nobody@example.test").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = call(&app, "GET", "emp-david").await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// THE 503 SAYS A FIXED SENTENCE (review cdff423f N4). It names the
/// agents registry and the file that turns the judgement off, and it
/// carries nothing the storage layer said: the id that reaches this arm
/// is asserted, so the body is readable by any caller that signs
/// `agent-x` while the registry is dark.
#[tokio::test]
async fn the_503_names_the_agents_registry_and_carries_no_storage_error() {
    let (app, registry) = world(Mode::Enforce).await;
    let mut bodies = Vec::new();
    registry.alias_read_fails.store(true, Ordering::SeqCst);
    for id in ["claude@algedonic.dev", "agent-x", "nobody@example.test"] {
        bodies.push(call(&app, "POST", id).await);
    }
    registry.alias_read_fails.store(false, Ordering::SeqCst);
    registry.listing_fails.store(true, Ordering::SeqCst);
    for id in ["claude@algedonic.dev", "agent-x"] {
        bodies.push(call(&app, "POST", id).await);
    }
    assert_eq!(bodies.len(), 5);
    for (status, body) in bodies {
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
        assert_eq!(
            body["error"], "the agents registry could not answer, so this agent's role is unknown",
            "{body}"
        );
        let hint = body["hint"].as_str().expect("a hint");
        assert!(hint.contains(ROW_ROLE_MODE_FILE), "{hint}");
        let keys: Vec<&str> = body
            .as_object()
            .expect("an object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            ["actor", "error", "hint"],
            "the body is these three fields and no other: {body}"
        );
        let text = body.to_string();
        for leaked in [
            STORAGE_DETAIL,
            "storage",
            "database",
            "10.9.9.9",
            "user=boss",
        ] {
            assert!(
                !text.contains(leaked),
                "the 503 hands the caller `{leaked}` from the storage error: {text}"
            );
        }
    }
}

/// THE BINARY'S WIRING (mutant M1b), first half: what the wiring does.
/// `LoginDoor::judging_rows_by_the_mounted_word` mounts the word and
/// hands the switch to `judging_rows_by_word`; this is that second
/// call, on a switch whose file is in a temp directory. An ABSENT file
/// is `off` through the wiring — the asserted role passes and nothing
/// is listed — and it is the switch that is asked on each request, not
/// a value taken when the door was built: the word written later judges
/// the next request, and the word taken away stops judging.
#[tokio::test]
async fn an_absent_word_reads_as_off_through_the_binarys_wiring() {
    let dir = tempfile::tempdir().expect("a temp directory");
    let file = dir.path().join("agent-role");
    let reading = read_mode(&file, "agent role");
    assert_eq!(reading, (Mode::Off, None), "an absent file reads as off");
    let word = Arc::new(ModeSwitch::new("agent role", &file, reading));
    let (app, registry) = world_wired({
        let word = Arc::clone(&word);
        move |door| door.judging_rows_by_word(word)
    })
    .await;

    let (status, body) = call(&app, "POST", "claude@algedonic.dev").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["seen"]["id"], "agent-claude");
    every_asserted_field_arrived(&body["seen"], "word absent");
    assert_eq!(
        registry.listings.load(Ordering::SeqCst),
        0,
        "off: no listing"
    );
    registry.darken();
    let (status, body) = call(&app, "POST", "agent-claude").await;
    assert_eq!(status, StatusCode::OK, "word absent, registry dark: {body}");
    registry.alias_read_fails.store(false, Ordering::SeqCst);
    registry.listing_fails.store(false, Ordering::SeqCst);

    for (text, role, tier) in [
        ("report\n", "platform-admin", "operator"),
        ("enforce\n", "engineering-agent", "user"),
        ("off\n", "platform-admin", "operator"),
        ("enforce\n", "engineering-agent", "user"),
    ] {
        std::fs::write(&file, text).expect("write the word");
        word.observe(word.read_blocking());
        let (status, body) = call(&app, "POST", "claude@algedonic.dev").await;
        assert_eq!(status, StatusCode::OK, "{text:?}: {body}");
        assert_eq!(body["seen"]["role"], role, "{text:?}: {body}");
        assert_eq!(body["seen"]["access_tier"], tier, "{text:?}: {body}");
    }
    std::fs::remove_file(&file).expect("take the word away");
    word.observe(word.read_blocking());
    let (status, body) = call(&app, "POST", "claude@algedonic.dev").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    every_asserted_field_arrived(&body["seen"], "word taken away");
}

/// THE BINARY'S WIRING, second half: that the binary uses it. No test
/// runs `boss_jobs_api.rs`, so its text is held instead: the door is
/// built with the one call the test above covers, and the binary holds
/// no closure, no mount and no mode word of its own for the door —
/// which is what a wiring that answers `enforce` whatever the file says
/// would have to add.
#[test]
fn the_binary_wires_the_door_to_the_mounted_word_and_to_nothing_else() {
    let bin = include_str!("../src/bin/boss_jobs_api.rs");
    let live: String = bin
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    let squeezed: String = live.split_whitespace().collect();
    assert_eq!(
        squeezed
            .matches(
                "boss_jobs::agents::LoginDoor::new(agents,door_publisher)\
                 .judging_rows_by_the_mounted_word(),"
            )
            .count(),
        1,
        "boss_jobs_api.rs builds the login door with judging_rows_by_the_mounted_word() and \
         nothing after it"
    );
    for own in [
        "judging_rows_when",
        "judging_rows_by_word",
        "ROW_ROLE_MODE_FILE",
        "Mode::Enforce",
    ] {
        assert!(
            !live.contains(own),
            "boss_jobs_api.rs spells `{own}`: the door's word is mounted and read in \
             agents/door.rs, where a test runs it, never in the binary"
        );
    }
}
