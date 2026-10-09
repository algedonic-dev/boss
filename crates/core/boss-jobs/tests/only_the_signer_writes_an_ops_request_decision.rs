//! Only the signer writes an ops-request's `decision` and `comment`
//! (design 09618594, question `signer`, David 2026-10-06; the signer
//! half of backlog 6c9183de).
//!
//! WHY. The approve step's `decision` is what `execute` and `refused`
//! fork on and it sits inside the shape the passkey signs. Until the
//! tree's ops-request row declared a writer for it, anyone with Update
//! on the step could merge `decision = "approved"` onto a rendered plan
//! and leave the approver one tap from signing a decision he never made.
//! The mechanism landed inert in car a4a6009c (train #964): a field
//! whose writer is `signer` is written only through a verified gateway
//! session that holds a sign-off role the step requires. Its tests drive
//! a fixture step. These drive THE TREE'S ROW, through the same doors,
//! because the row is the declaration and the declaration is the car.
//!
//! WHAT IS REHEARSED HERE, AND WHY IT HAS TO BE. This approve step is
//! how David authorises every mutating verb, including the ones that
//! would repair the system. A declaration that is wrong for the live
//! estate — the session does not verify, the sign-off grant is missing —
//! locks him out of approving anything. So the road back is driven end
//! to end: publish the declared row, watch the signer's own write be
//! refused under each failure, publish the no-writer row back with
//! NOTHING an approval supplies (no cookie, no passkey, no sign-off, no
//! ops-request), and watch a freshly filed request take the decision
//! again. A request admitted under the declared row stays under it —
//! packets are pinned — so it is cancelled and refiled, never converted.
//!
//! Nothing here touches a live system of record: every app is an
//! in-memory router, and the registry publishes go through the same
//! HTTP routes `boss workflow publish` calls.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, PresenceKey, router};
use boss_jobs::owner_resolution::RosterLookup;
use boss_jobs::registry::{WorkflowSpec, seedable_platform_workflows};
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, WorkflowRegistry};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use hmac::{Hmac, KeyInit, Mac};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const KEY: &[u8; 32] = b"test-key-0123456789abcdef0123456";
const SIGNER_KEYS: [&str; 2] = ["decision", "comment"];
const RUNNER_KEYS: [&str; 5] = ["plan", "verb", "host", "args", "rendered_plan_sha256"];

// ---------------------------------------------------------------------
// The two rows
// ---------------------------------------------------------------------

/// The tree's row: `infra/platform/workflows/ops-request.toml`, read by
/// the loader the seed and `boss workflow publish` both use.
fn declared_row() -> WorkflowSpec {
    seedable_platform_workflows()
        .into_iter()
        .find(|w| w.kind == "ops-request")
        .expect("the ops-request protocol is in the platform bundle")
}

/// The row as it stood before this car: the same file without the two
/// signer fields. `the_car_adds_two_signer_fields_and_nothing_else`
/// holds that this is the WHOLE difference, so publishing this is
/// publishing the row that was live.
fn no_writer_row() -> WorkflowSpec {
    let mut spec = declared_row();
    for step in spec.steps.iter_mut().filter(|s| s.title == "approve") {
        step.fields
            .retain(|f| !SIGNER_KEYS.contains(&f.name.as_str()));
    }
    spec
}

// ---------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------

struct Roster;

#[async_trait::async_trait]
impl RosterLookup for Roster {
    async fn active_holders(&self, role: &str) -> Result<Vec<String>, String> {
        Ok(match role {
            "platform-admin" => vec!["emp-david".to_string()],
            _ => Vec::new(),
        })
    }

    async fn is_active_employee(&self, id: &str) -> Result<bool, String> {
        Ok(id == "emp-david")
    }
}

/// What the live estate is assumed to give the signer path. Each `false`
/// is one way the declaration could be wrong for it.
#[derive(Clone, Copy)]
struct Estate {
    /// The jobs API can read the gateway's session key.
    session_key: bool,
    /// `platform-admin` holds sign-off on `step-signoff:platform-admin`
    /// (boss-policy-client's default grant, `Scope::All`).
    sign_off_grant: bool,
}

const HEALTHY: Estate = Estate {
    session_key: true,
    sign_off_grant: true,
};

struct Fixture {
    app: axum::Router,
    jobs: Arc<InMemoryJobs>,
}

/// A jobs API whose registry holds `live` as the active ops-request row.
fn fixture(live: WorkflowSpec, estate: Estate) -> Fixture {
    let jobs = Arc::new(InMemoryJobs::new());
    let mut policy = FakePolicyClient::builder();
    for (action, resource) in [
        (Action::Create, Resource::job()),
        (Action::Read, Resource::job()),
        (Action::Update, Resource::job()),
        (Action::Update, Resource::step()),
        (Action::Read, Resource::workflow()),
        (Action::Create, Resource::workflow()),
        (Action::Update, Resource::workflow()),
        (Action::Publish, Resource::workflow()),
    ] {
        policy = policy.allow("platform-admin", action, resource, Scope::All);
    }
    if estate.sign_off_grant {
        policy = policy.allow(
            "platform-admin",
            Action::SignOff,
            Resource::new("step-signoff:platform-admin"),
            Scope::All,
        );
    }
    let policy: Arc<dyn PolicyClient> = Arc::new(policy.build());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let kinds = Arc::new(InMemoryWorkflows::for_fixture());
    kinds.seed(live).expect("seed the live ops-request row");
    let mut state = JobsApiState {
        kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
        roster: Some(Arc::new(Roster)),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    if estate.session_key {
        state.presence_key = Some(Arc::new(PresenceKey::fixed(KEY.to_vec())));
    }
    Fixture {
        app: router(state),
        jobs,
    }
}

// ---------------------------------------------------------------------
// Callers
// ---------------------------------------------------------------------

/// The `x-boss-user` a caller presents. Self-asserted on the machine
/// door, which is the whole reason it is never read as a writer.
fn identity(id: &str) -> String {
    json!({
        "id": id,
        "role": "platform-admin",
        "access_tier": "operator",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": "platform",
    })
    .to_string()
}

/// Every caller that is NOT the signer, each holding Update on the step:
/// an agent session, the ops runner's account, and David's own id typed
/// onto the machine door with no session behind it.
const NON_SIGNERS: [&str; 3] = ["agent-claude", "automation:ops-runner", "emp-david"];

/// The gateway's session cookie as the deployed gateway writes it (the
/// wire car a4a6009c's test pins), for David as platform-admin.
fn session_cookie(key: &[u8]) -> String {
    let payload = URL_SAFE_NO_PAD.encode(
        json!({
            "u": "david",
            "e": boss_core::presence::now_epoch() + 3600,
            "r": "platform-admin",
            "i": "emp-david",
            "t": "user",
        })
        .to_string(),
    );
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(key).unwrap();
    mac.update(payload.as_bytes());
    format!(
        "boss_session={payload}.{}",
        URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
    )
}

async fn send(
    app: &axum::Router,
    method: &str,
    uri: &str,
    who: &str,
    cookie: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-boss-user", identity(who));
    if let Some(cookie) = cookie {
        request = request.header("cookie", cookie);
    }
    let body = match body {
        Some(body) => {
            request = request.header("content-type", "application/json");
            Body::from(body.to_string())
        }
        None => Body::empty(),
    };
    let response = app
        .clone()
        .oneshot(request.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
    };
    (status, json)
}

/// David's browser: his session cookie, and the identity the gateway
/// derives from it and injects.
async fn as_david(app: &axum::Router, method: &str, uri: &str, body: Value) -> (StatusCode, Value) {
    send(
        app,
        method,
        uri,
        "emp-david",
        Some(&session_cookie(KEY)),
        Some(body),
    )
    .await
}

// ---------------------------------------------------------------------
// Packets
// ---------------------------------------------------------------------

/// One filed ops-request that needs an approval, and its approve step.
struct Filed {
    job: String,
    approve: String,
}

impl Filed {
    fn merge_door(&self) -> String {
        format!("/api/jobs/{}/steps/{}/metadata", self.job, self.approve)
    }
    fn record_door(&self) -> String {
        format!("{}/records", self.merge_door())
    }
    fn step_put(&self) -> String {
        format!("/api/jobs/{}/steps/{}", self.job, self.approve)
    }
}

async fn approve_step(app: &axum::Router, job: &str) -> Value {
    let (status, steps) = send(
        app,
        "GET",
        &format!("/api/jobs/{job}/steps"),
        "emp-david",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "GET steps: {steps}");
    steps
        .as_array()
        .and_then(|steps| steps.iter().find(|s| s["spec_slug"] == "approve"))
        .cloned()
        .unwrap_or_else(|| panic!("the request has no approve step: {steps}"))
}

/// File a request the way `boss ops` does, then have the runner render
/// its plan onto the approve step — the five keys in one patch, signed
/// as the runner's account with no credential, exactly as today.
async fn file_request(app: &axum::Router) -> Filed {
    let job = uuid::Uuid::new_v4().to_string();
    let (status, body) = send(
        app,
        "POST",
        "/api/jobs",
        "emp-david",
        None,
        Some(json!({
            "id": job,
            "kind": "ops-request",
            "subject": {"subject_kind": "custom", "id": "forge"},
            "title": "commission a disk",
            "owner_id": "emp-david",
            "status": "open",
            "priority": "standard",
            "metadata": {
                "host": "forge",
                "verb": "commission-a-disk",
                "args": ["/dev/disk/by-id/nvme-the-new-one"],
                "requires_approval": true,
            },
            "tags": [],
            "opened_on": "2026-10-06",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "filing the request: {body}");
    let approve = approve_step(app, &job).await["id"]
        .as_str()
        .expect("step id")
        .to_string();
    let request = Filed { job, approve };
    let (status, body) = send(
        app,
        "PATCH",
        &request.merge_door(),
        "automation:ops-runner",
        None,
        Some(json!({
            "plan": "PLAN commission-a-disk on forge\n",
            "verb": "commission-a-disk",
            "host": "forge",
            "args": ["/dev/disk/by-id/nvme-the-new-one"],
            "rendered_plan_sha256": "ab".repeat(32),
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "THE RUNNER HALF IS UNTOUCHED: the runner renders its five keys with no credential, \
         as it does today: {body}"
    );
    request
}

async fn metadata(app: &axum::Router, request: &Filed) -> Value {
    approve_step(app, &request.job).await["metadata"].clone()
}

/// The patch both surfaces send for an approval: ApprovalSurface.svelte
/// `decide()` and sign-off.js each merge these three keys in one PATCH.
fn the_surfaces_patch(decision: &str) -> Value {
    json!({
        "decision": decision,
        "decided_at": "2026-10-06T17:00:00.000Z",
        "comment": "read the plan; the by-id target is the new drive",
    })
}

/// A 409 that names the key, its declared writer and the door.
fn assert_names_the_door(status: StatusCode, body: &Value, key: &str, door: &str, context: &str) {
    assert_eq!(status, StatusCode::CONFLICT, "{context}: {body}");
    let refused = body["refused_keys"]
        .as_array()
        .unwrap_or_else(|| panic!("{context}: the refusal names no keys: {body}"));
    assert!(
        refused
            .iter()
            .any(|r| r["key"] == key && r["writer"] == "signer"),
        "{context}: `{key}` must be refused naming its writer `signer`: {body}"
    );
    assert_eq!(body["door"], door, "{context}: the refusal names the door");
}

// ---------------------------------------------------------------------
// Publishing, through the routes `boss workflow publish` calls
// ---------------------------------------------------------------------

/// Draft then publish `row`, asking the registry for nothing but the
/// caller's `publish` authority on `workflow`: the request carries no
/// cookie, no presence ticket and no sign-off, and files no packet.
/// Answers the version that went live, read back from the active row.
async fn publish(app: &axum::Router, row: &WorkflowSpec) -> i64 {
    let (status, draft) = send(
        app,
        "PUT",
        "/api/workflows/ops-request",
        "emp-david",
        None,
        Some(serde_json::to_value(row).unwrap()),
    )
    .await;
    assert!(
        status.is_success(),
        "the draft is accepted (and the publish lint accepts a signer writer): {status} {draft}"
    );
    let (status, published) = send(
        app,
        "POST",
        "/api/workflows/ops-request/publish",
        "emp-david",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "publish: {published}");
    let live = active_row(app).await;
    assert_eq!(live["version"], draft["version"], "the draft went live");
    live["version"].as_i64().expect("a version")
}

async fn active_row(app: &axum::Router) -> Value {
    let (status, row) = send(
        app,
        "GET",
        "/api/workflows/ops-request",
        "emp-david",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "GET the active row: {row}");
    row
}

/// `name -> writer` for the active row's approve step.
fn approve_writers(row: &Value) -> Vec<(String, Option<String>)> {
    row["steps"]
        .as_array()
        .and_then(|steps| steps.iter().find(|s| s["title"] == "approve"))
        .and_then(|approve| approve["fields"].as_array())
        .map(|fields| {
            fields
                .iter()
                .map(|f| {
                    (
                        f["name"].as_str().unwrap_or_default().to_string(),
                        f["writer"].as_str().map(str::to_string),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------
// The declaration
// ---------------------------------------------------------------------

/// THE CAR IS TWO FIELDS. The signer's keys are declared, to the signer;
/// the runner's five are exactly as they were — no writer, until the car
/// that delivers the runner credential — and nothing else in the row
/// declares a writer or an executor. And the row lints: a `signer`
/// writer needs no enrolled principal, only a required sign-off role.
#[test]
fn the_car_adds_two_signer_fields_and_nothing_else() {
    let row = declared_row();
    let approve = row
        .steps
        .iter()
        .find(|s| s.title == "approve")
        .expect("an approve step");
    for key in SIGNER_KEYS {
        let field = approve
            .fields
            .iter()
            .find(|f| f.name == key)
            .unwrap_or_else(|| panic!("the approve step must declare `{key}` as a field"));
        assert_eq!(
            field.writer.as_deref(),
            Some(boss_jobs::field_writer::SIGNER_WRITER),
            "`{key}` is the signer's"
        );
        assert!(
            !field.required,
            "`{key}` is not required: a step completed with no decision still reaches `refused`"
        );
        assert!(
            approve.metadata_defaults.get(key).is_none(),
            "`{key}` has no default: a default is the protocol writing a reserved key"
        );
    }
    for key in RUNNER_KEYS {
        let field = approve
            .fields
            .iter()
            .find(|f| f.name == key)
            .unwrap_or_else(|| panic!("`{key}` is still a declared field"));
        assert_eq!(
            field.writer, None,
            "`{key}` declares no writer in this car: `runner:ops` waits for the runner credential"
        );
    }
    assert!(
        approve
            .sign_offs_required
            .iter()
            .any(|r| r == "platform-admin"),
        "the signer is judged against the step's own required role"
    );
    let elsewhere: Vec<String> = row
        .steps
        .iter()
        .flat_map(|s| {
            s.fields
                .iter()
                .filter(|f| f.writer.is_some())
                .map(move |f| format!("{}.{}", s.title, f.name))
        })
        .filter(|name| name != "approve.decision" && name != "approve.comment")
        .collect();
    assert!(elsewhere.is_empty(), "no other writer: {elsewhere:?}");
    assert!(
        row.steps.iter().all(|s| s.executor.is_none()),
        "no step declares a credential executor in this car"
    );
    if let Err(problems) = boss_jobs::workflow_lint::gate_active(&row) {
        panic!("the declared row must pass the publish lint: {problems:?}");
    }

    // The road back publishes a row with no writer at all.
    let back = no_writer_row();
    assert!(
        back.steps
            .iter()
            .all(|s| s.fields.iter().all(|f| f.writer.is_none())),
        "the no-writer row declares no writer"
    );
    if let Err(problems) = boss_jobs::workflow_lint::gate_active(&back) {
        panic!("the no-writer row must pass the publish lint: {problems:?}");
    }
}

// ---------------------------------------------------------------------
// The refusal
// ---------------------------------------------------------------------

/// Every caller that is not the signer, at every door that writes step
/// metadata, is refused — and the step is left exactly as it was.
#[tokio::test]
async fn a_non_signer_is_refused_at_every_door_that_writes_the_decision() {
    let Fixture { app, .. } = fixture(declared_row(), HEALTHY);
    let request = file_request(&app).await;
    let before = metadata(&app, &request).await;
    let merge = format!("PATCH {}", request.merge_door());
    let record = format!("POST {}", request.record_door());

    for who in NON_SIGNERS {
        // The merge door, one key at a time.
        for (key, value) in [("decision", "approved"), ("comment", "looks fine")] {
            let (status, body) = send(
                &app,
                "PATCH",
                &request.merge_door(),
                who,
                None,
                Some(json!({ key: value })),
            )
            .await;
            assert_names_the_door(status, &body, key, &merge, &format!("{who} merging {key}"));
            assert_eq!(
                body["asked_by"], who,
                "who asked is reported, never believed"
            );
        }
        // The merge door, with the exact patch the surfaces send. The
        // whole patch is refused: `decided_at` does not land alone.
        let (status, body) = send(
            &app,
            "PATCH",
            &request.merge_door(),
            who,
            None,
            Some(the_surfaces_patch("approved")),
        )
        .await;
        assert_names_the_door(
            status,
            &body,
            "decision",
            &merge,
            &format!("{who} sending the surfaces' patch"),
        );
        // The first-record door.
        for key in SIGNER_KEYS {
            let (status, body) = send(
                &app,
                "POST",
                &request.record_door(),
                who,
                None,
                Some(json!({"key": key, "value": "approved", "expected_absence": true})),
            )
            .await;
            assert_names_the_door(
                status,
                &body,
                key,
                &record,
                &format!("{who} recording {key}"),
            );
        }
        // The step PUT writes no metadata at all (e39a9d2a): refused,
        // naming the merge door — which is the door refused above.
        let (status, body) = send(
            &app,
            "PUT",
            &request.step_put(),
            who,
            None,
            Some(json!({"metadata": {"decision": "approved"}})),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{who} PUT metadata: {body}");
        assert!(
            body.to_string().contains("merge door"),
            "the step PUT names the merge door: {body}"
        );
        // The sign-off door writes a stamp, never metadata — and this
        // step's stamp needs a passkey, so it writes nothing here.
        let (status, body) = send(
            &app,
            "POST",
            &format!("{}/sign-offs", request.step_put()),
            who,
            None,
            Some(json!({"role": "platform-admin"})),
        )
        .await;
        assert!(
            !status.is_success(),
            "{who}: a presence-assured stamp is not produced by a session alone: {status} {body}"
        );
        // And the completion PUT cannot stand in for the decision.
        let (status, body) = send(
            &app,
            "PUT",
            &request.step_put(),
            who,
            None,
            Some(json!({"status": "completed"})),
        )
        .await;
        assert!(
            !status.is_success(),
            "{who}: the approve step does not complete without a passkey: {status} {body}"
        );
    }

    // A cookie nobody issued is not a session: David's id, a cookie
    // signed with another key.
    let forged = session_cookie(b"another-key-0123456789abcdef0123");
    let (status, body) = send(
        &app,
        "PATCH",
        &request.merge_door(),
        "emp-david",
        Some(&forged),
        Some(json!({"decision": "approved"})),
    )
    .await;
    assert_names_the_door(status, &body, "decision", &merge, "a forged session cookie");

    assert_eq!(
        metadata(&app, &request).await,
        before,
        "nothing any of them sent reached the step"
    );
}

/// THE LEGITIMATE WRITER STILL WORKS. Both surfaces send one PATCH of
/// `decision`, `decided_at` and `comment` from the approver's browser,
/// which carries his gateway session; that write lands, as him.
#[tokio::test]
async fn the_approvers_own_session_writes_the_decision_as_the_surfaces_send_it() {
    let Fixture { app, jobs } = fixture(declared_row(), HEALTHY);
    let request = file_request(&app).await;
    let events_before = jobs.recorded_events().len();

    let (status, body) = as_david(
        &app,
        "PATCH",
        &request.merge_door(),
        the_surfaces_patch("approved"),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "the signer's write: {body}");
    let now = metadata(&app, &request).await;
    assert_eq!(now["decision"], "approved");
    assert_eq!(now["decided_at"], "2026-10-06T17:00:00.000Z");
    assert_eq!(
        now["comment"],
        "read the plan; the by-id target is the new drive"
    );
    assert_eq!(
        now["plan"], "PLAN commission-a-disk on forge\n",
        "the runner's plan is untouched by the decision"
    );
    let events = jobs.recorded_events();
    assert!(
        events.len() > events_before
            && events[events_before..]
                .iter()
                .all(|e| e.payload["_actor"] == "emp-david"),
        "the write is recorded as the session's person: {:?}",
        &events[events_before..]
    );

    // He may change his mind before he signs (Reject runs the same
    // ceremony), and an emptied comment is sent as null and deleted.
    let (status, body) = as_david(
        &app,
        "PATCH",
        &request.merge_door(),
        json!({"decision": "rejected", "decided_at": "2026-10-06T17:01:00.000Z", "comment": null}),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "a changed decision: {body}");
    let now = metadata(&app, &request).await;
    assert_eq!(now["decision"], "rejected");
    assert!(now.get("comment").is_none(), "the comment was deleted");

    // A machine caller re-sending what is stored changes nothing and is
    // answered as the no-op it is; changing it is still refused.
    let (status, body) = send(
        &app,
        "PATCH",
        &request.merge_door(),
        "agent-claude",
        None,
        Some(json!({"decision": "rejected"})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "an unchanged re-send: {body}"
    );
    let (status, _) = send(
        &app,
        "PATCH",
        &request.merge_door(),
        "agent-claude",
        None,
        Some(json!({"decision": "approved"})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "and the ABA write back is refused"
    );
    assert_eq!(metadata(&app, &request).await["decision"], "rejected");
}

// ---------------------------------------------------------------------
// In flight
// ---------------------------------------------------------------------

/// A request filed BEFORE the publish keeps the fields it was admitted
/// with: its approve step declares no writer, so it is approved exactly
/// as before and no open request is stranded by the publish. The price
/// is that it is also exactly as writable as before, and the convert
/// door will not move it onto the declared row — a writer declared where
/// there was none would present a value anyone wrote as the signer's.
#[tokio::test]
async fn a_request_in_flight_keeps_the_row_it_was_admitted_under() {
    let Fixture { app, .. } = fixture(no_writer_row(), HEALTHY);
    let in_flight = file_request(&app).await;

    let declared_version = publish(&app, &declared_row()).await;
    let filed_after = file_request(&app).await;

    // In flight: a machine caller still writes the decision, and so does
    // the approver, with or without a verifiable session.
    let (status, body) = send(
        &app,
        "PATCH",
        &in_flight.merge_door(),
        "agent-claude",
        None,
        Some(json!({"comment": "pinned to the row with no writer"})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "an in-flight request answers as its pinned row says: {body}"
    );
    let (status, body) = as_david(
        &app,
        "PATCH",
        &in_flight.merge_door(),
        the_surfaces_patch("approved"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "the approver's write: {body}"
    );
    assert_eq!(metadata(&app, &in_flight).await["decision"], "approved");

    // Filed after: the declared row.
    let (status, _) = send(
        &app,
        "PATCH",
        &filed_after.merge_door(),
        "agent-claude",
        None,
        Some(json!({"decision": "approved"})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a request filed after the publish is guarded"
    );

    // Never converted: the move is refused, naming the writer.
    let (status, body) = send(
        &app,
        "POST",
        &format!("/api/jobs/{}/convert", in_flight.job),
        "emp-david",
        None,
        Some(json!({"to_version": declared_version})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "an in-flight request is not converted onto the declared row: {body}"
    );
    assert!(
        body.to_string().contains("signer"),
        "the refusal names the writer that would have appeared: {body}"
    );
    assert_eq!(body["converted"], false);
}

// ---------------------------------------------------------------------
// The lockout, and the road back
// ---------------------------------------------------------------------

/// IF THE DECLARATION IS WRONG FOR THE ESTATE, DAVID CANNOT APPROVE —
/// AND THE ROAD BACK DOES NOT PASS THROUGH AN APPROVAL.
///
/// Two ways the signer path can fail on a live estate, each driven
/// against the declared row: the jobs API cannot read the gateway's
/// session key, or the sign-off grant the signer is judged by is
/// missing. In both his own write is refused 409 with the reason, which
/// is the lockout: no decision, so no approval of any mutating verb.
///
/// The road back is then walked with exactly what a locked-out operator
/// still has — `publish` on `workflow`, presented on the machine door.
/// No cookie, no passkey, no sign-off, no ops-request: so it cannot be
/// circular. It restores the no-writer row; a request filed after it is
/// approved the old way; and the request that was admitted under the
/// declared row stays refused and is not convertible, which is why the
/// rule is cancel and refile.
#[tokio::test]
async fn a_locked_out_approver_publishes_the_no_writer_row_back_without_an_approval() {
    for (estate, why) in [
        (
            Estate {
                session_key: false,
                ..HEALTHY
            },
            "session verifier unavailable",
        ),
        (
            Estate {
                sign_off_grant: false,
                ..HEALTHY
            },
            "holds no required sign-off role",
        ),
    ] {
        // The estate as it is today: the no-writer row is live.
        let Fixture { app, .. } = fixture(no_writer_row(), estate);
        let was_live = active_row(&app).await["version"].as_i64().unwrap();

        // THE PUBLISH. From here the refusal is live.
        let declared_version = publish(&app, &declared_row()).await;
        assert!(declared_version > was_live);
        let writers = approve_writers(&active_row(&app).await);
        for key in SIGNER_KEYS {
            assert!(
                writers.contains(&(key.to_string(), Some("signer".to_string()))),
                "the live row now reserves `{key}`: {writers:?}"
            );
        }

        // THE LOCKOUT. David's own write, from his own browser.
        let stuck = file_request(&app).await;
        let (status, body) = as_david(
            &app,
            "PATCH",
            &stuck.merge_door(),
            the_surfaces_patch("approved"),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "{why}: the approver is refused: {body}"
        );
        assert!(
            body.to_string().contains(why),
            "the refusal says why, so the lockout is diagnosable from the answer: {body}"
        );
        assert!(
            metadata(&app, &stuck).await.get("decision").is_none(),
            "no decision landed"
        );

        // THE ROAD BACK. `publish` sends no cookie on either request,
        // and this fixture has no key or no grant, so nothing an
        // approval supplies could have helped it.
        let restored_version = publish(&app, &no_writer_row()).await;
        assert!(restored_version > declared_version);
        let live = active_row(&app).await;
        assert!(
            approve_writers(&live).iter().all(|(_, w)| w.is_none()),
            "the live row declares no writer again: {:?}",
            approve_writers(&live)
        );
        assert_eq!(
            serde_json::to_value(&no_writer_row().steps).unwrap(),
            live["steps"],
            "and its steps are the no-writer row's, field for field"
        );

        // OLD BEHAVIOUR BACK, for a request filed now.
        let refiled = file_request(&app).await;
        let (status, body) = as_david(
            &app,
            "PATCH",
            &refiled.merge_door(),
            the_surfaces_patch("approved"),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::NO_CONTENT,
            "{why}: a request filed after the road back takes the decision: {body}"
        );
        assert_eq!(metadata(&app, &refiled).await["decision"], "approved");

        // THE STUCK ONE STAYS STUCK — pinned to the declared row — and
        // cannot be moved off it, so it is cancelled and refiled.
        let (status, _) = as_david(
            &app,
            "PATCH",
            &stuck.merge_door(),
            the_surfaces_patch("approved"),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "{why}: a request admitted under the declared row keeps it after the road back"
        );
        let (status, body) = send(
            &app,
            "POST",
            &format!("/api/jobs/{}/convert", stuck.job),
            "emp-david",
            None,
            Some(json!({"to_version": restored_version})),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "{why}: nor is it converted back — a live step's writer does not move: {body}"
        );
    }
}

/// THE WHOLE REHEARSAL ON A HEALTHY ESTATE, in the order the operator
/// will run it: publish the declared row; the positive control (the
/// signer's write lands) and the negative control (a non-signer's is
/// refused); publish the no-writer row back; the old behaviour returns.
#[tokio::test]
async fn publish_then_both_controls_then_the_road_back() {
    let Fixture { app, .. } = fixture(no_writer_row(), HEALTHY);

    publish(&app, &declared_row()).await;
    let guarded = file_request(&app).await;
    let (status, body) = send(
        &app,
        "PATCH",
        &guarded.merge_door(),
        "agent-claude",
        None,
        Some(json!({"comment": "negative control"})),
    )
    .await;
    assert_names_the_door(
        status,
        &body,
        "comment",
        &format!("PATCH {}", guarded.merge_door()),
        "the negative control",
    );
    let (status, body) = as_david(
        &app,
        "PATCH",
        &guarded.merge_door(),
        the_surfaces_patch("approved"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "the positive control: {body}"
    );

    publish(&app, &no_writer_row()).await;
    let open_again = file_request(&app).await;
    let (status, body) = send(
        &app,
        "PATCH",
        &open_again.merge_door(),
        "agent-claude",
        None,
        Some(json!({"comment": "negative control"})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "after the road back a request filed now answers as it did before the car: {body}"
    );
}
