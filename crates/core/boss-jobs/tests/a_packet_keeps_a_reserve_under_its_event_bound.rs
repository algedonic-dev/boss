//! A packet keeps a reserve under its event bound, so it can always be
//! closed (backlog 4f8783fb, design 6ab85352 car 1).
//!
//! Every Job write emits `jobs.job.updated` carrying the WHOLE row, and
//! the outbox refuses any event over `MAX_STAGED_EVENT_BYTES` (983,040).
//! So the cost of a write is the size of the packet, not of the change,
//! and a packet that grows to the bound can take no further write of any
//! kind. Measured 2026-10-15 on 6cd6c935: a short evidence note answered
//! 500, "event kind jobs.job.updated encodes 983662 bytes, over 983040".
//! Measured 2026-11-13: 044b189b's last staged event sits about 26 bytes
//! under the bound, so even its close (closed_on, closed_at, outcome)
//! cannot be staged, and its silence alarm refiles every 15 minutes.
//!
//! The bound stays. Under it the jobs service keeps a budget:
//!
//!   1. At half the bound a write lands and says so — the response
//!      carries the `boss-size-warning` header naming the event's size,
//!      the bound, the budget and the repair door.
//!   2. At three quarters, a write that GROWS the packet is refused 413,
//!      naming the size, the budget, the bound and the repair door, and
//!      nothing is written.
//!   3. A write that holds or shrinks the packet, and a close, is never
//!      refused by the budget — so the 25% reserve (245,760 bytes, far
//!      more than any close adds) guarantees closure by construction.
//!   4. Over the bound itself no write carrying the row can be staged;
//!      that refusal is a 413 naming the repair door, not a 500 — and a
//!      merge that would cross the bound in one write is refused the
//!      same way, naming the packet and its size.
//!   5. Except the close (backlog b96df4f8): a packet already over the
//!      bound closes on a bounded event carrying the close's fields and
//!      the sha256 of the row it leaves, rather than standing open.
//!
//! These run on the in-memory adapter, which mirrors the Postgres one;
//! `a_packet_keeps_a_reserve_under_its_event_bound_pg.rs` drives the
//! same doors through the transactional adapter.

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::owner_resolution::RosterLookup;
use boss_jobs::registry::{StepSpec, Terminal, WorkflowSpec};
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, JobsRepository, WorkflowRegistry};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const KIND: &str = "evidence-heavy";
/// The outbox's bound, spelled as the refusal must spell it.
const BOUND: usize = 983_040;
/// Three quarters of it: the budget a growing write may not reach.
const BUDGET: usize = 737_280;
/// The size the refused note on 6cd6c935 encoded to (2026-10-15).
const SPECIMEN: usize = 983_662;
const DOOR: &str = "boss job externalise";
const WARNING: &str = "boss-size-warning";

/// One piece of work that is a declared terminal, so completing it
/// closes the packet through the close door.
fn spec() -> WorkflowSpec {
    let mut spec = WorkflowSpec::platform_seed(
        KIND,
        KIND,
        "platform",
        vec!["custom".into()],
        vec![StepSpec {
            title: "work".into(),
            kind: "task".into(),
            ready_when: "true".into(),
            title_template: "The only work".into(),
            authority_role: Some("platform-admin".into()),
            terminal: Some(Terminal {
                outcome: "done".into(),
            }),
            ..Default::default()
        }],
    );
    spec.metadata = json!({ "owner_role": "platform-admin" });
    spec
}

struct AdminRoster;

#[async_trait]
impl RosterLookup for AdminRoster {
    async fn active_holders(&self, role: &str) -> Result<Vec<String>, String> {
        Ok(match role {
            "platform-admin" => vec!["emp-bootstrap-admin".to_string()],
            _ => Vec::new(),
        })
    }
    async fn is_active_employee(&self, id: &str) -> Result<bool, String> {
        Ok(id == "emp-bootstrap-admin")
    }
}

fn admin_header() -> String {
    json!({
        "id": "emp-bootstrap-admin",
        "role": "platform-admin",
        "access_tier": "operator",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": "platform",
    })
    .to_string()
}

fn app() -> (axum::Router, Arc<InMemoryJobs>) {
    let kinds = Arc::new(InMemoryWorkflows::for_fixture());
    kinds.seed(spec()).expect("seed the kind");
    let jobs = Arc::new(InMemoryJobs::new());
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow(
                "platform-admin",
                Action::Create,
                Resource::job(),
                Scope::All,
            )
            .allow("platform-admin", Action::Read, Resource::job(), Scope::All)
            .allow(
                "platform-admin",
                Action::Update,
                Resource::job(),
                Scope::All,
            )
            .allow("platform-admin", Action::Close, Resource::job(), Scope::All)
            .allow(
                "platform-admin",
                Action::Update,
                Resource::step(),
                Scope::All,
            )
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState {
        kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
        roster: Some(Arc::new(AdminRoster)),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    (router(state), jobs)
}

struct Answer {
    status: StatusCode,
    warning: Option<Value>,
    body: String,
}

async fn send(app: &axum::Router, method: &str, uri: &str, body: Option<Value>) -> Answer {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("x-boss-user", admin_header())
        .body(match body {
            Some(body) => Body::from(body.to_string()),
            None => Body::empty(),
        })
        .expect("request");
    let response = app.clone().oneshot(request).await.expect("router responds");
    let status = response.status();
    let warning = response
        .headers()
        .get(WARNING)
        .map(|v| serde_json::from_slice(v.as_bytes()).expect("the warning header is JSON"));
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes();
    Answer {
        status,
        warning,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

async fn get_job(app: &axum::Router, id: &str) -> Value {
    let answer = send(app, "GET", &format!("/api/jobs/{id}"), None).await;
    assert_eq!(
        answer.status,
        StatusCode::OK,
        "read failed: {}",
        answer.body
    );
    serde_json::from_str(&answer.body).expect("job json")
}

/// The metadata of an evidence-heavy packet in 6cd6c935's shape: a few
/// large evidence values (its six largest, scaled by `scale`), a pile of
/// short notes, and one pad that tunes the total.
fn evidence_metadata(scale: f64, pad: usize) -> Value {
    let mut md = serde_json::Map::new();
    for (i, size) in [146_766, 94_561, 83_526, 76_102, 73_139, 72_904]
        .iter()
        .enumerate()
    {
        let n = (*size as f64 * scale) as usize;
        md.insert(format!("evidence_{i}"), Value::String("e".repeat(n)));
    }
    for i in 0..170 {
        md.insert(
            format!("note_{i:03}"),
            Value::String(format!("short note {i}")),
        );
    }
    md.insert("pad".into(), Value::String("p".repeat(pad)));
    Value::Object(md)
}

async fn open_packet(app: &axum::Router, metadata: Value) -> String {
    let answer = send(
        app,
        "POST",
        "/api/jobs",
        Some(json!({
            "kind": KIND,
            "subject": { "subject_kind": "custom", "id": "/system/evidence" },
            "title": "An evidence-heavy packet",
            "owner_id": "emp-bootstrap-admin",
            "priority": "standard",
            "status": "open",
            "metadata": metadata,
            "tags": ["test"],
        })),
    )
    .await;
    assert_eq!(
        answer.status,
        StatusCode::CREATED,
        "create: {}",
        answer.body
    );
    let job: Value = serde_json::from_str(&answer.body).expect("job json");
    job["id"].as_str().expect("job id").to_string()
}

/// The encoded size of the `jobs.job.updated` a merge of `note` onto
/// packet `id` would stage, built from the packet's own recorded create
/// — the same row, the same signer — so the fixture is sized by the
/// encoding the outbox judges, not by a guess at it.
fn update_bytes_after(jobs: &InMemoryJobs, id: &str, note: &(&str, &str)) -> usize {
    let created = jobs
        .recorded_events()
        .into_iter()
        .find(|e| e.kind == "jobs.job.created" && e.payload["id"] == id)
        .expect("the create was recorded");
    let mut event = created.clone();
    event.kind = "jobs.job.updated".into();
    event.payload["metadata"][note.0] = Value::String(note.1.into());
    serde_json::to_vec(&event).expect("encode").len()
}

/// A packet whose `jobs.job.updated`, once `note` is merged, encodes to
/// `target` bytes: opened once to measure, then opened again with its pad
/// moved by the difference. The pad is ASCII, so each byte of it is one
/// byte of the event.
async fn packet_whose_note_encodes_to(
    app: &axum::Router,
    jobs: &InMemoryJobs,
    target: usize,
    note: &(&str, &str),
) -> String {
    let base = 10_000;
    let probe = open_packet(app, evidence_metadata(1.0, base)).await;
    let measured = update_bytes_after(jobs, &probe, note);
    let pad = base + target - measured;
    let id = open_packet(app, evidence_metadata(1.0, pad)).await;
    let sized = update_bytes_after(jobs, &id, note);
    assert!(
        sized.abs_diff(target) <= 16,
        "fixture: the note's event would encode to {sized}, wanted {target}"
    );
    id
}

/// A packet whose row encodes to roughly `fraction` of the bound.
async fn packet_at(app: &axum::Router, fraction: f64) -> String {
    let target = (BOUND as f64 * fraction) as usize;
    // 6cd6c935's six largest values total 547 KB; scale them under the
    // target and let the pad make up the rest.
    let scale = (target as f64 * 0.6) / 547_000.0;
    let fixed = serde_json::to_vec(&evidence_metadata(scale, 0))
        .expect("encode")
        .len();
    open_packet(
        app,
        evidence_metadata(scale, target.saturating_sub(fixed + 1_500)),
    )
    .await
}

fn names_size_budget_bound_and_door(body: &str, id: &str) {
    for needle in [
        BOUND.to_string(),
        BUDGET.to_string(),
        format!("{DOOR} {id}"),
    ] {
        assert!(body.contains(&needle), "the refusal names {needle}: {body}");
    }
}

async fn note(app: &axum::Router, id: &str, key: &str, value: Value) -> Answer {
    send(
        app,
        "PATCH",
        &format!("/api/jobs/{id}/metadata"),
        Some(json!({ key: value })),
    )
    .await
}

async fn complete_the_terminal(app: &axum::Router, id: &str) -> Answer {
    let job = get_job(app, id).await;
    let step = job["steps"][0]["id"].as_str().expect("step id").to_string();
    send(
        app,
        "PUT",
        &format!("/api/jobs/{id}/steps/{step}"),
        Some(json!({ "status": "completed" })),
    )
    .await
}

#[tokio::test]
async fn the_native_specimen_is_refused_413_naming_its_size_budget_bound_and_door() {
    let (app, jobs) = app();
    let annotation = ("reliability_note", "short evidence note, 2026-10-15");
    let id = packet_whose_note_encodes_to(&app, &jobs, SPECIMEN, &annotation).await;

    let answer = note(&app, &id, annotation.0, Value::String(annotation.1.into())).await;

    assert_eq!(
        answer.status,
        StatusCode::PAYLOAD_TOO_LARGE,
        "a note that would stage a {SPECIMEN}-byte event is refused 413: {}",
        answer.body
    );
    names_size_budget_bound_and_door(&answer.body, &id);
    let named: Vec<usize> = answer
        .body
        .split(|c: char| !c.is_ascii_digit())
        .filter_map(|n| n.parse().ok())
        .filter(|n: &usize| n.abs_diff(SPECIMEN) <= 16)
        .collect();
    assert!(
        !named.is_empty(),
        "the refusal names the event's own size, about {SPECIMEN}: {}",
        answer.body
    );
    let job = get_job(&app, &id).await;
    assert!(
        job["metadata"].get(annotation.0).is_none(),
        "nothing was written"
    );
}

#[tokio::test]
async fn a_write_that_grows_a_packet_past_its_budget_is_refused_and_writes_nothing() {
    let (app, _) = app();
    let id = packet_at(&app, 0.76).await;

    let answer = note(
        &app,
        &id,
        "late_note",
        Value::String("one more line".into()),
    )
    .await;

    assert_eq!(
        answer.status,
        StatusCode::PAYLOAD_TOO_LARGE,
        "a growing write at 76% of the bound is refused: {}",
        answer.body
    );
    names_size_budget_bound_and_door(&answer.body, &id);
    let job = get_job(&app, &id).await;
    assert!(
        job["metadata"].get("late_note").is_none(),
        "nothing was written"
    );
}

#[tokio::test]
async fn a_write_that_holds_or_shrinks_a_packet_over_its_budget_is_admitted() {
    let (app, _) = app();
    let id = packet_at(&app, 0.80).await;
    let job = get_job(&app, &id).await;
    let held = job["metadata"]["note_001"].clone();

    let same = note(&app, &id, "note_001", held).await;
    assert_eq!(
        same.status,
        StatusCode::NO_CONTENT,
        "a write that leaves the packet the size it was lands: {}",
        same.body
    );
    let smaller = note(&app, &id, "evidence_0", Value::Null).await;
    assert_eq!(
        smaller.status,
        StatusCode::NO_CONTENT,
        "a write that shrinks the packet lands: {}",
        smaller.body
    );
    let job = get_job(&app, &id).await;
    assert!(
        job["metadata"].get("evidence_0").is_none(),
        "the removal landed"
    );
}

#[tokio::test]
async fn a_close_at_99_percent_of_the_bound_is_admitted() {
    let (app, _) = app();
    let id = packet_at(&app, 0.99).await;

    let answer = complete_the_terminal(&app, &id).await;

    assert!(
        answer.status.is_success(),
        "a close is never refused by the budget: {} {}",
        answer.status,
        answer.body
    );
    let job = get_job(&app, &id).await;
    assert_eq!(job["status"], "closed", "the packet closed");
    assert_eq!(job["metadata"]["outcome"], "done", "with its outcome");
}

#[tokio::test]
async fn a_write_past_half_the_bound_lands_carrying_a_size_warning() {
    let (app, _) = app();
    let id = packet_at(&app, 0.55).await;

    let answer = note(
        &app,
        &id,
        "late_note",
        Value::String("one more line".into()),
    )
    .await;

    assert_eq!(
        answer.status,
        StatusCode::NO_CONTENT,
        "it lands: {}",
        answer.body
    );
    let warning = answer
        .warning
        .unwrap_or_else(|| panic!("a write past half the bound carries the {WARNING} header"));
    let bytes = warning["event_bytes"].as_u64().expect("event_bytes") as usize;
    assert!(
        (BOUND / 2..BUDGET).contains(&bytes),
        "the warning names the event's size, between half and three quarters: {warning}"
    );
    assert_eq!(warning["bound"], BOUND, "{warning}");
    assert_eq!(warning["budget"], BUDGET, "{warning}");
    assert_eq!(warning["door"], format!("{DOOR} {id}"), "{warning}");
}

#[tokio::test]
async fn a_write_under_half_the_bound_carries_no_warning() {
    let (app, _) = app();
    let id = packet_at(&app, 0.30).await;

    let answer = note(
        &app,
        &id,
        "late_note",
        Value::String("one more line".into()),
    )
    .await;

    assert_eq!(
        answer.status,
        StatusCode::NO_CONTENT,
        "it lands: {}",
        answer.body
    );
    assert!(answer.warning.is_none(), "no warning under half the bound");
}

#[tokio::test]
async fn a_put_that_grows_a_packet_past_its_budget_is_refused() {
    let (app, _) = app();
    let id = packet_at(&app, 0.76).await;
    let mut job = get_job(&app, &id).await;
    job["metadata"]["late_note"] = Value::String("one more line".into());

    let answer = send(&app, "PUT", &format!("/api/jobs/{id}"), Some(job)).await;

    assert_eq!(
        answer.status,
        StatusCode::PAYLOAD_TOO_LARGE,
        "a growing whole-row write at 76% is refused: {}",
        answer.body
    );
    names_size_budget_bound_and_door(&answer.body, &id);
    let job = get_job(&app, &id).await;
    assert!(
        job["metadata"].get("late_note").is_none(),
        "nothing was written"
    );
}

#[tokio::test]
async fn a_step_merge_that_grows_its_step_past_the_budget_is_refused() {
    let (app, _) = app();
    let id = packet_at(&app, 0.05).await;
    let job = get_job(&app, &id).await;
    let step = job["steps"][0]["id"].as_str().expect("step id").to_string();
    let uri = format!("/api/jobs/{id}/steps/{step}/metadata");

    let first = send(
        &app,
        "PATCH",
        &uri,
        Some(json!({ "transcript": "t".repeat(BUDGET - 20_000) })),
    )
    .await;
    assert_eq!(
        first.status,
        StatusCode::NO_CONTENT,
        "under the budget the step takes it: {}",
        first.body
    );
    assert!(
        first.warning.is_some(),
        "and says it is past half the bound"
    );

    let second = send(
        &app,
        "PATCH",
        &uri,
        Some(json!({ "appendix": "a".repeat(40_000) })),
    )
    .await;
    assert_eq!(
        second.status,
        StatusCode::PAYLOAD_TOO_LARGE,
        "a step write that grows its event past the budget is refused: {}",
        second.body
    );
    names_size_budget_bound_and_door(&second.body, &id);
}

/// 044b189b's shape: a packet whose last write staged within a few bytes
/// of the bound. Its close adds `closed_on`, `closed_at` and `outcome`,
/// so its full-row event cannot be staged — the bound is the bus's, and
/// this car does not move it.
///
/// That close was REFUSED, by the bound, and the packet stood open with
/// every step terminal for good: no write could shrink it either, since
/// every write re-emitted the same row (backlog b96df4f8, measured live
/// 2026-11-17 on 044b189b). The close now stages a bounded fact in place
/// of the row — the close's own fields and the sha256 of the row it
/// leaves — and the packet closes.
#[tokio::test]
async fn a_packet_over_the_bound_closes_on_a_bounded_event_naming_its_row() {
    let (app, jobs) = app();
    // Re-merging a key the packet already holds, unchanged, sizes the
    // packet's OWN row: its next state event, before the close adds to it.
    let unchanged = ("note_000", "short note 0");
    let id = packet_whose_note_encodes_to(&app, &jobs, BOUND - 26, &unchanged).await;

    let answer = complete_the_terminal(&app, &id).await;

    assert!(
        answer.status.is_success(),
        "a packet over the bound closes: {} {}",
        answer.status,
        answer.body
    );
    let job = get_job(&app, &id).await;
    assert_eq!(job["status"], "closed", "the packet closed");
    assert_eq!(job["metadata"]["outcome"], "done", "with its outcome");
    assert!(
        job["metadata"]["closed_at"].is_string(),
        "and its close instant"
    );

    let events = jobs.recorded_events();
    let ours = |e: &&boss_core::event::Event| e.payload["id"] == id;
    let closes: Vec<_> = events
        .iter()
        .filter(ours)
        .filter(|e| e.kind == "jobs.job.closed_over_bound")
        .collect();
    assert_eq!(closes.len(), 1, "one bounded close is recorded");
    let close = closes[0];
    let bytes = serde_json::to_vec(close).expect("encode").len();
    assert!(
        bytes < 4_096,
        "the bounded close carries the change, not the row: {bytes} bytes"
    );
    assert_eq!(close.payload["status"], "closed");
    assert_eq!(close.payload["merged"]["outcome"], "done");
    assert!(
        close.payload["whole_row_event_bytes"].as_u64().unwrap_or(0) > BOUND as u64,
        "it names why the row did not ride: {}",
        close.payload
    );
    // The digest is of the row the close left, as the whole-row event
    // would have carried it: the stored row, re-encoded, hashes to it.
    let stored = jobs
        .get_job(&boss_core::job::JobId::from_uuid(id.parse().expect("uuid")))
        .await
        .expect("read")
        .expect("the packet");
    let (sha, size) = boss_jobs::packet_budget::row_digest(&stored);
    assert_eq!(close.payload["row_sha256"], sha, "the digest names the row");
    assert_eq!(close.payload["row_bytes"], size);
    assert!(
        !events
            .iter()
            .filter(ours)
            .any(|e| e.kind == "jobs.job.updated" && e.payload["status"] == "closed"),
        "no whole-row close was staged"
    );
    assert!(
        events
            .iter()
            .filter(ours)
            .any(|e| e.kind == "jobs.job.closed"),
        "the close marker every close rule listens on still fires"
    );
}

/// A merge that would carry a packet ACROSS the bound in one write — not
/// creep up to the budget — is refused too, naming the packet and the size
/// its event would have reached, and writes nothing (backlog b96df4f8:
/// the guard that keeps the next 044b189b from being built).
#[tokio::test]
async fn a_merge_that_would_cross_the_bound_is_refused_naming_the_packet_and_its_size() {
    let (app, _) = app();
    let id = packet_at(&app, 0.60).await;

    let answer = note(&app, &id, "mailbox", Value::String("m".repeat(BOUND / 2))).await;

    assert_eq!(
        answer.status,
        StatusCode::PAYLOAD_TOO_LARGE,
        "a merge that would cross the bound is refused: {}",
        answer.body
    );
    assert!(
        answer.body.contains(&format!("job {id}"))
            && answer.body.contains("would encode to")
            && answer.body.contains(&BOUND.to_string()),
        "the refusal names the packet, its size and the bound: {}",
        answer.body
    );
    let job = get_job(&app, &id).await;
    assert!(
        job["metadata"].get("mailbox").is_none(),
        "nothing was written"
    );
}

/// A packet used as a MAILBOX — 044b189b's 762 keys, one small merge at a
/// time — stops growing at its budget and keeps its reserve, so it still
/// closes on its WHOLE row: the bounded close is for packets that grew
/// before the budget existed, not a road any packet takes now.
#[tokio::test]
async fn a_mailbox_packet_stops_at_its_budget_and_still_closes_on_its_whole_row() {
    let (app, jobs) = app();
    let id = packet_at(&app, 0.50).await;

    let mut refused = None;
    for i in 0..200 {
        let answer = note(
            &app,
            &id,
            &format!("message_{i:03}"),
            Value::String("x".repeat(8_000)),
        )
        .await;
        if answer.status != StatusCode::NO_CONTENT {
            refused = Some((i, answer));
            break;
        }
    }
    let (i, answer) = refused.expect("the mailbox is stopped before 200 messages");
    assert_eq!(
        answer.status,
        StatusCode::PAYLOAD_TOO_LARGE,
        "{}",
        answer.body
    );
    names_size_budget_bound_and_door(&answer.body, &id);
    let job = get_job(&app, &id).await;
    assert!(
        job["metadata"].get(format!("message_{i:03}")).is_none(),
        "the refused message was not written"
    );

    let closed = complete_the_terminal(&app, &id).await;
    assert!(closed.status.is_success(), "{}", closed.body);
    assert!(
        jobs.recorded_events()
            .iter()
            .any(|e| e.kind == "jobs.job.updated"
                && e.payload["id"] == id
                && e.payload["status"] == "closed"),
        "the close carried the whole row: the reserve held"
    );
}
