//! Axum routes for the credentials registry.
//!
//! `GET /api/credentials` and `GET /api/credentials/{id}` — the door
//! that turns a scope question into a lookup. The one rule from the
//! module doc holds hardest here: **a row carries locations, never
//! contents** — no secret value can pass through this API because no
//! secret value exists anywhere behind it. The registry knows
//! *about* credentials; possession stays in Secrets.
//!
//! Read access mirrors `delivery::http`: `crate::trust::can_read` —
//! operator-tier callers (`boss credential list`, the session actor
//! via boss-api) and the auditor tier (the forge-host audit, which
//! signs as `automation:forge-token-audit`). A request with no
//! `x-boss-user` header is refused: it was trusted here until
//! 2026-09-25 (backlog e84de48e), when the audit was the one reader
//! that relied on it.
//!
//! NO PACKET SCOPE IS ASKED, AND NONE NEEDS TO BE (checked for backlog
//! d0058c92, whose rule is that a caller whose scope does not read
//! every packet reads no record that is not scoped by packet). The tier
//! gate is already narrower: the gateway mints the operator tier only
//! for a passkey-elevated platform-admin session, and the auditor tier
//! not at all — it is the probe reader's, `audit-readonly`, which reads
//! every packet. A session of any role is user tier and refused here,
//! pinned by `a_session_reads_no_row_whatever_its_packet_scope`. A
//! header claiming operator tier with a narrow role can only come from a
//! holder of the machine token, who could claim any role at all, so a
//! scope question here would add a check without adding a refusal.
//!
//! An unknown id is a 404 that names it — unlike the delivery door,
//! there is no fallback for a missing credential row; an absent row
//! is a finding, and the caller should hear so unambiguously.
//!
//! `POST /api/credentials/{id}/rotation/{phase}` is the rotation
//! path's write — the census-door precedent. The broker
//! (`credential.rotate.forgejo`, a dispatcher handler) owns no
//! database, so the service that owns the registry provides the one
//! write: validate trust and shape, record exactly one
//! `credential.<phase>` event, and on the `installed` phase stamp
//! the row's `rotated_at` in the same transaction. DELIBERATELY A
//! DUMB DOOR: the evidence payload is owned by the handler that
//! observed the effects — identifiers (token name/id, Secret path,
//! value length) and observed effects only, NEVER a value — and a
//! door that second-guesses its instrument is a second instrument.
//! Operator tier only: this is a WRITE. (No door here trusts a
//! headerless caller since e84de48e, 2026-09-25.)
//!
//! ONE KIND OF ROW IS NOT DUMB: a runner credential's phases
//! (`ops-runner-credential`) pass the broker-stage door first
//! ([`super::broker_stage`], design 6e28ed42) — a verified workload
//! token bound to the row's open rotation packet, or a named refusal.
//! That door is off until a deployment configures it, and while it is
//! off this route behaves as described above for every row.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};

use boss_policy_client::{AccessTier, CurrentUser};

use crate::trust::{can_read, is_trusted};

use super::port::{CredentialsError, CredentialsRegistry};
use super::types::{CredentialBatch, RotationPhase, validate_credential};

pub struct CredentialsApiState {
    pub registry: Arc<dyn CredentialsRegistry>,
    /// The broker-stage door (design 6e28ed42): how a runner credential's
    /// stage phases are believed. `Off` unless the deployment configures it.
    pub stage: Arc<super::broker_stage::Door>,
    /// The read of the rotation packet a verified stage must belong to.
    pub packets: Option<Arc<dyn super::broker_stage::StagePackets>>,
}

impl CredentialsApiState {
    /// The registry's doors with the broker-stage door off — what every
    /// deployment runs until an activation configures one.
    pub fn new(registry: Arc<dyn CredentialsRegistry>) -> Self {
        Self {
            registry,
            stage: Arc::new(super::broker_stage::Door::Off),
            packets: None,
        }
    }

    pub fn with_stage(
        mut self,
        door: Arc<super::broker_stage::Door>,
        packets: Option<Arc<dyn super::broker_stage::StagePackets>>,
    ) -> Self {
        self.stage = door;
        self.packets = packets;
        self
    }
}

// The two reads admit what `crate::trust::can_read` admits: the
// operator machinery that always read here, and the auditor tier the
// recorded-probe reader carries (839335b7) — a row names where a value
// lives, never the value, so a read-only auditor learns nothing a
// rotation packet does not already say. The rotation-phase write
// below keeps its own operator-only check.

pub fn router(state: CredentialsApiState) -> Router {
    let shared = Arc::new(state);
    Router::new()
        .route("/api/credentials", get(list))
        .route("/api/credentials/batch", post(publish))
        .route("/api/credentials/{id}", get(get_one))
        .merge(delivery_routes())
        .route(
            "/api/credentials/{id}/rotation/{phase}",
            post(record_rotation_phase),
        )
        .with_state(shared)
}

fn delivery_routes() -> Router<Arc<CredentialsApiState>> {
    Router::new()
        .route("/api/credentials/{id}/delivery", post(record_delivery))
        .route(
            "/api/credentials/{id}/delivery/{attempt}",
            get(get_delivery),
        )
}

/// The authenticated owner observation surface, separable from registry
/// publishing and generic rotation commands when composing an adapter.
pub fn delivery_router(state: CredentialsApiState) -> Router {
    delivery_routes().with_state(Arc::new(state))
}

async fn record_delivery(
    State(state): State<Arc<CredentialsApiState>>,
    CurrentUser(user): CurrentUser,
    context: Option<axum::Extension<super::runner_delivery::ResolvedDelivery>>,
    Path(id): Path<String>,
    body: Result<
        Json<super::runner_delivery::DeliveryRequest>,
        axum::extract::rejection::JsonRejection,
    >,
) -> Response {
    let Json(request) = match body {
        Ok(request) => request,
        Err(_) => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                "delivery body must contain only job_id, attempt and secret_uid",
            )
                .into_response();
        }
    };
    let Some(axum::Extension(context)) = context else {
        return (
            StatusCode::FORBIDDEN,
            "authenticated runner generation unavailable",
        )
            .into_response();
    };
    if user.id != crate::runner_credential::ACTOR
        || !context.admits(&id, &request)
        || user.access_tier != AccessTier::Operator
    {
        return (
            StatusCode::FORBIDDEN,
            "runner delivery context does not match",
        )
            .into_response();
    }
    let Some(actor) = user.ambient_actor() else {
        return StatusCode::FORBIDDEN.into_response();
    };
    let stamp = boss_core::publisher::EventStamp::new("jobs", actor);
    match state.registry.record_delivery(&context, &stamp).await {
        Ok(observation) => (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({
                "recorded":true,"kind":"credential.verified","observation":observation
            })),
        )
            .into_response(),
        Err(error) => err_response(error),
    }
}

async fn get_delivery(
    State(state): State<Arc<CredentialsApiState>>,
    CurrentUser(user): CurrentUser,
    Path((id, attempt)): Path<(String, uuid::Uuid)>,
) -> Response {
    if !can_read(&user) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match state.registry.delivery_receipt(&id, attempt).await {
        Ok(Some(receipt)) => {
            let valid = receipt.credential_id == id
                && receipt.phase == "verified"
                && receipt.observation_id == format!("{attempt}:delivered")
                && receipt.actor == crate::runner_credential::ACTOR
                && !receipt.event_id.is_nil()
                && receipt.version == 1
                && serde_json::from_str::<serde_json::Value>(&receipt.evidence_json).is_ok_and(
                    |evidence| {
                        evidence["purpose"] == super::runner_delivery::PURPOSE
                            && evidence["credential_id"] == id
                            && evidence["attempt"] == attempt.to_string()
                            && boss_core::job::canonical_json_bytes(&evidence) == receipt.canonical
                    },
                );
            if !valid {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "original delivery receipt is invalid",
                )
                    .into_response();
            }
            Json(serde_json::json!({"receipt":receipt})).into_response()
        }
        Ok(None) => (StatusCode::NOT_FOUND, "no original delivery receipt").into_response(),
        Err(error) => err_response(error),
    }
}

/// The declaration door (backlog ee368d0c): `POST /api/credentials/
/// batch` takes an instance's `seeds/credentials.toml` rows, validates
/// every one with the same `validate_credential` the TOML loader ran
/// and refuses the whole batch (422, naming the row) on the first bad
/// one — a registry with a half-declared instance is worse than a
/// refusal — then lands them insert-if-absent by id. A key the shape
/// does not name (`value`, `token`) is refused by the JSON extractor
/// before this runs: nothing a value could ride in reaches the port.
/// Operator machinery like every tenant batch door (`is_trusted`).
async fn publish(
    State(state): State<Arc<CredentialsApiState>>,
    CurrentUser(user): CurrentUser,
    body: Result<Json<CredentialBatch>, axum::extract::rejection::JsonRejection>,
) -> Response {
    if !is_trusted(&user) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Json(body) = match body {
        Ok(b) => b,
        // The rejection's text quotes the offending JSON, which for an
        // unknown key is exactly the text that must not be echoed.
        Err(_) => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                "the batch did not parse as {tenant_id, credentials: [CredentialInput]}; \
                 a key the shape does not name (a value under any spelling) is refused",
            )
                .into_response();
        }
    };
    if body.tenant_id.trim().is_empty() {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            "tenant_id is required: the declaring tenant's [meta] tenant_id",
        )
            .into_response();
    }
    if let Some(why) = body
        .credentials
        .iter()
        .find_map(|c| validate_credential(c).err())
    {
        return (StatusCode::UNPROCESSABLE_ENTITY, why).into_response();
    }
    // Same envelope as the rotation door and the agents batch: the
    // actor the request signed with rides as `_actor` and is named
    // again as `declared_by` on each inserted row's fact.
    let actor = user
        .ambient_actor()
        .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into()));
    let stamp = boss_core::publisher::EventStamp::new("jobs", actor);
    match state
        .registry
        .publish(&body.tenant_id, &body.credentials, &stamp)
        .await
    {
        Ok(out) => Json(out).into_response(),
        Err(e) => err_response(e),
    }
}

fn err_response(e: CredentialsError) -> Response {
    match e {
        CredentialsError::InvalidObservation(m) => (StatusCode::BAD_REQUEST, m).into_response(),
        CredentialsError::ObservationConflict => {
            (StatusCode::CONFLICT, e.to_string()).into_response()
        }
        CredentialsError::Storage(m) => (StatusCode::INTERNAL_SERVER_ERROR, m).into_response(),
        CredentialsError::UnknownCredential(id) => (
            StatusCode::NOT_FOUND,
            format!("no credential {id:?} in the registry"),
        )
            .into_response(),
    }
}

async fn list(
    State(state): State<Arc<CredentialsApiState>>,
    CurrentUser(user): CurrentUser,
) -> Response {
    if !can_read(&user) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match state.registry.list().await {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => err_response(e),
    }
}

async fn get_one(
    State(state): State<Arc<CredentialsApiState>>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<String>,
) -> Response {
    if !can_read(&user) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match state.registry.get(&id).await {
        Ok(Some(row)) => Json(row).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            format!("no credential {id:?} in the registry"),
        )
            .into_response(),
        Err(e) => err_response(e),
    }
}

/// The rotation door. Trust (operator tier — the dispatcher stamps
/// `access_tier: operator` on every rule-as-actor header), phase (one
/// of the four the registry's `RotationPhase` names), shape (a JSON
/// object), then record verbatim: the event's `credential_id` is
/// injected from the path so every rotation event self-identifies,
/// and everything else in the payload is the instrument's own report.
async fn record_rotation_phase(
    State(state): State<Arc<CredentialsApiState>>,
    CurrentUser(user): CurrentUser,
    caller: Option<axum::Extension<crate::field_writer::CredentialedCaller>>,
    presented: Option<axum::Extension<super::broker_stage::Presented>>,
    Path((id, phase)): Path<(String, String)>,
    Json(evidence): Json<serde_json::Value>,
) -> Response {
    let presented = presented.map(|axum::Extension(presented)| presented);
    // Host installation acknowledgments come from the authenticated owner
    // command, never a phase instrument's caller-authored evidence (104a93d5).
    if caller.is_some()
        || evidence.get("purpose").and_then(serde_json::Value::as_str)
            == Some(super::runner_delivery::PURPOSE)
    {
        return (
            StatusCode::FORBIDDEN,
            "authenticated runner delivery requires the owning acknowledgment door",
        )
            .into_response();
    }
    if user.access_tier != AccessTier::Operator {
        return (
            StatusCode::FORBIDDEN,
            "the rotation door is operator machinery — operator tier required",
        )
            .into_response();
    }
    let Some(phase) = RotationPhase::parse(&phase) else {
        let valid: Vec<&str> = RotationPhase::ALL.iter().map(|p| p.as_str()).collect();
        return (
            StatusCode::BAD_REQUEST,
            format!(
                "unknown rotation phase {phase:?}; valid phases: {}",
                valid.join(", ")
            ),
        )
            .into_response();
    };
    let Some(mut payload) = evidence.as_object().cloned() else {
        return (
            StatusCode::BAD_REQUEST,
            "rotation evidence must be a JSON object",
        )
            .into_response();
    };
    payload.insert(
        "credential_id".to_string(),
        serde_json::Value::String(id.clone()),
    );

    // The broker-stage door (design 6e28ed42): a RUNNER credential's
    // stage is believed from a verified workload token and bound to its
    // own open rotation packet, or refused by name. Off and with nothing
    // presented — every deployment today — this judges nothing; every
    // other credential's phases never reach it (5840's legacy behaviour).
    // A row the registry does not hold falls through to the 404 below.
    let verified_stage = match stage_judgement(&state, &id, presented.as_ref(), &payload).await {
        Ok(verified) => verified.is_some(),
        Err(response) => return response,
    };

    // Same envelope construction as the census door: the actor rides
    // as `_actor` exactly as EventStamp injects it, the stamp is
    // wall-clock (sim time is retired from the record), and the
    // source is `jobs` — this service is the one recording. A verified
    // stage is recorded as the WORKLOAD the token proved, never as the
    // rule label the caller typed: that label confers no authority.
    let mut user = user;
    if verified_stage {
        user.id = super::broker_stage::ACTOR.to_string();
    }
    let actor = user
        .ambient_actor()
        .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into()));
    let stamp = boss_core::publisher::EventStamp::new("jobs", actor);

    match state
        .registry
        .record_rotation(&id, phase, serde_json::Value::Object(payload), &stamp)
        .await
    {
        Ok(outcome) => (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({
                "recorded": true,
                "kind": phase.event_kind(),
                "observation": outcome,
            })),
        )
            .into_response(),
        Err(e) => err_response(e),
    }
}

/// The broker-stage judgement for one rotation command: `Ok(None)` is
/// "record as before", `Ok(Some)` a verified stage bound to its packet,
/// `Err` the refusal — a 403 whose body is one fixed sentence naming why,
/// never a clean answer and never a byte of what was presented.
async fn stage_judgement(
    state: &CredentialsApiState,
    id: &str,
    presented: Option<&super::broker_stage::Presented>,
    payload: &serde_json::Map<String, serde_json::Value>,
) -> Result<Option<super::broker_stage::Verified>, Response> {
    use super::broker_stage::{Door, bind_packet, judge};
    if presented.is_none() && matches!(*state.stage, Door::Off) {
        return Ok(None);
    }
    let refuse = |why: super::broker_stage::Refusal| {
        tracing::warn!(credential = %id, %why, "a runner credential stage was refused");
        (
            StatusCode::FORBIDDEN,
            format!("runner credential stage refused: {why}"),
        )
            .into_response()
    };
    let kind = match state.registry.get(id).await {
        Ok(Some(row)) => row.kind,
        Ok(None) => return Ok(None),
        Err(e) => return Err(err_response(e)),
    };
    let Some(verified) = judge(&state.stage, &kind, presented).map_err(&refuse)? else {
        return Ok(None);
    };
    let evidence = serde_json::Value::Object(payload.clone());
    bind_packet(state.packets.as_deref(), id, &evidence)
        .await
        .map_err(&refuse)?;
    Ok(Some(verified))
}

// In a `tests` directory, where the tree-wide pins read a whole-file test
// module as test-only: its in-process `oneshot` requests type
// `x-boss-user` by hand, which a production sender may not
// (every_x_boss_user_sender_stamps_the_machine_token, gate-run 78244657).
#[cfg(test)]
#[path = "http/tests/stage.rs"]
mod stage_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::{CredentialRow, InMemoryCredentials};
    use axum::body::Body;
    use axum::http::Request;
    use boss_policy_client::{AccessTier, User};
    use http_body_util::BodyExt;
    use serde_json::json;
    use tower::ServiceExt;

    fn row(id: &str) -> CredentialRow {
        CredentialRow {
            id: id.into(),
            kind: "forgejo-access-token".into(),
            issuer: "forgejo (10.20.0.15)".into(),
            principal: "user david".into(),
            scopes: json!(["write:repository"]),
            storage_location: "k8s Secret boss-dev/boss-dev-forge-token key token".into(),
            consumers: json!([{ "kind": "secret-mount", "location": "/etc/boss-train/forge.token" }]),
            rotation_policy: "on-demand".into(),
            rotated_at: None,
            notes: "the row knows where the value lives, never what it is".into(),
        }
    }

    fn app(rows: Vec<CredentialRow>) -> Router {
        router(CredentialsApiState::new(Arc::new(
            InMemoryCredentials::new(rows),
        )))
    }

    fn operator_header() -> String {
        serde_json::to_string(&User {
            id: "emp-ops".into(),
            role: "platform-admin".into(),
            access_tier: AccessTier::Operator,
            territory_account_ids: Vec::new(),
            direct_report_ids: Vec::new(),
            department: Some("it".into()),
        })
        .unwrap()
    }

    fn user_tier_header() -> String {
        serde_json::to_string(&User {
            id: "emp-someone".into(),
            role: "member".into(),
            access_tier: AccessTier::User,
            territory_account_ids: Vec::new(),
            direct_report_ids: Vec::new(),
            department: Some("sales".into()),
        })
        .unwrap()
    }

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn the_list_answers_every_row_for_an_operator() {
        let app = app(vec![row("boss-dev-forge-token"), row("boss-machine-token")]);
        let resp = app
            .oneshot(
                Request::get("/api/credentials")
                    .header("x-boss-user", operator_header())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        let ids: Vec<&str> = body
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["boss-dev-forge-token", "boss-machine-token"]);
    }

    #[tokio::test]
    async fn a_headerless_caller_is_refused() {
        // Until 2026-09-25 this door trusted a caller with no
        // x-boss-user (the extractor's role=guest) — the forge-host
        // audit read it that way. A request without the identity
        // header is not trusted (backlog e84de48e): the audit now
        // signs as its own reader, and silence is a 403.
        let app = app(vec![row("boss-dev-forge-token")]);
        for path in ["/api/credentials", "/api/credentials/boss-dev-forge-token"] {
            let resp = app
                .clone()
                .oneshot(Request::get(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::FORBIDDEN, "{path}");
        }
    }

    fn probe_reader_header() -> String {
        serde_json::to_string(&User {
            id: "automation:run-car-probe-reader".into(),
            role: "audit-readonly".into(),
            access_tier: AccessTier::Auditor,
            territory_account_ids: Vec::new(),
            direct_report_ids: Vec::new(),
            department: Some("platform".into()),
        })
        .unwrap()
    }

    /// Measured 2026-09-16 (839335b7): `/api/credentials` answered the
    /// recorded-probe reader 403 on the live system of record, so no
    /// car about the credential registry could be proved. The reads
    /// admit the auditor tier; the rotation-phase write does not.
    #[tokio::test]
    async fn the_probe_reader_reads_the_registry_and_cannot_record_a_phase() {
        for path in ["/api/credentials", "/api/credentials/boss-dev-forge-token"] {
            let resp = app(vec![row("boss-dev-forge-token")])
                .oneshot(
                    Request::get(path)
                        .header("x-boss-user", probe_reader_header())
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK, "{path}");
        }
        let resp = app(vec![row("boss-dev-forge-token")])
            .oneshot(
                Request::post("/api/credentials/boss-dev-forge-token/rotation/issue")
                    .header("x-boss-user", probe_reader_header())
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"rotated_at":"2026-09-16T00:00:00Z"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::FORBIDDEN,
            "a read-only reader records nothing"
        );
    }

    #[tokio::test]
    async fn a_user_tier_caller_is_refused() {
        let app = app(vec![row("boss-dev-forge-token")]);
        let resp = app
            .oneshot(
                Request::get("/api/credentials")
                    .header("x-boss-user", user_tier_header())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    /// NARROWER THAN THE SCOPE RULE, ALREADY (backlog d0058c92, item 3).
    /// The IT map and the dispatcher's doors withhold a record not scoped
    /// by packet from a caller whose scope does not read every packet;
    /// this door asks no packet scope at all, because it asks the TIER,
    /// and every session the gateway mints is user tier unless it is a
    /// passkey-elevated platform-admin's. So a session reads no row here
    /// even when its role reads every packet — the platform-admin's own,
    /// unelevated — and so a narrowed one, whatever its role, reads none
    /// either. Both reads, both callers.
    #[tokio::test]
    async fn a_session_reads_no_row_whatever_its_packet_scope() {
        let unelevated_admin = serde_json::to_string(&User {
            id: "emp-ops".into(),
            role: "platform-admin".into(),
            access_tier: AccessTier::User,
            territory_account_ids: Vec::new(),
            direct_report_ids: Vec::new(),
            department: Some("it".into()),
        })
        .unwrap();
        let app = app(vec![row("boss-dev-forge-token")]);
        for (who, header) in [
            ("a narrowed session", user_tier_header()),
            ("an unelevated platform-admin", unelevated_admin),
        ] {
            for path in ["/api/credentials", "/api/credentials/boss-dev-forge-token"] {
                let resp = app
                    .clone()
                    .oneshot(
                        Request::get(path)
                            .header("x-boss-user", header.clone())
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(resp.status(), StatusCode::FORBIDDEN, "{who}: GET {path}");
            }
        }
    }

    #[tokio::test]
    async fn get_by_id_answers_the_row() {
        let app = app(vec![row("boss-dev-forge-token")]);
        let resp = app
            .oneshot(
                Request::get("/api/credentials/boss-dev-forge-token")
                    .header("x-boss-user", operator_header())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["scopes"], json!(["write:repository"]));
        assert_eq!(
            body["storage_location"], "k8s Secret boss-dev/boss-dev-forge-token key token",
            "the row says where the value lives, never what it is"
        );
    }

    #[tokio::test]
    async fn an_unknown_id_is_a_404_naming_it() {
        let app = app(vec![]);
        let resp = app
            .oneshot(
                Request::get("/api/credentials/no-such-credential")
                    .header("x-boss-user", operator_header())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "an absent credential row is a finding, not a fallback"
        );
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        assert!(String::from_utf8_lossy(&bytes).contains("no-such-credential"));
    }

    // ----- the rotation door -----

    fn rotation_app(rows: Vec<CredentialRow>) -> (Router, Arc<InMemoryCredentials>) {
        let registry = Arc::new(InMemoryCredentials::new(rows));
        let app = router(CredentialsApiState::new(registry.clone()));
        (app, registry)
    }

    fn post_phase(id: &str, phase: &str, body: serde_json::Value, header: &str) -> Request<Body> {
        Request::post(format!("/api/credentials/{id}/rotation/{phase}"))
            .header("x-boss-user", header)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    #[tokio::test]
    async fn a_recorded_phase_lands_as_an_event_naming_the_credential() {
        let (app, registry) = rotation_app(vec![row("boss-dev-forge-token")]);
        let resp = app
            .oneshot(post_phase(
                "boss-dev-forge-token",
                "minted",
                json!({
                    "job_id": "7ee101aa-3267-4745-8096-06d07df7e144",
                    "token_name": "boss-dev-forge-token-7ee101aa",
                    "token_id": 101,
                }),
                &operator_header(),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
        let body = body_json(resp).await;
        assert_eq!(body["kind"], "credential.minted");

        let events = registry.recorded_events().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "credential.minted");
        assert_eq!(events[0].source, "jobs");
        assert_eq!(
            events[0].payload["credential_id"], "boss-dev-forge-token",
            "the door injects the path id so every rotation event self-identifies"
        );
        assert_eq!(
            events[0].payload["token_name"],
            "boss-dev-forge-token-7ee101aa"
        );
    }

    #[tokio::test]
    async fn the_install_phase_stamps_rotated_at_and_the_others_do_not() {
        let (app, registry) = rotation_app(vec![row("boss-dev-forge-token")]);
        for phase in ["minted", "verified", "revoked"] {
            let resp = app
                .clone()
                .oneshot(post_phase(
                    "boss-dev-forge-token",
                    phase,
                    json!({}),
                    &operator_header(),
                ))
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::ACCEPTED);
        }
        assert!(
            registry
                .get("boss-dev-forge-token")
                .await
                .unwrap()
                .unwrap()
                .rotated_at
                .is_none(),
            "rotated_at records when the VALUE last changed — the install moment"
        );
        let resp = app
            .oneshot(post_phase(
                "boss-dev-forge-token",
                "installed",
                json!({ "value_length": 40 }),
                &operator_header(),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
        assert!(
            registry
                .get("boss-dev-forge-token")
                .await
                .unwrap()
                .unwrap()
                .rotated_at
                .is_some()
        );
    }

    #[tokio::test]
    async fn an_unknown_phase_is_a_400_naming_the_valid_ones() {
        let (app, registry) = rotation_app(vec![row("boss-dev-forge-token")]);
        let resp = app
            .oneshot(post_phase(
                "boss-dev-forge-token",
                "misplaced",
                json!({}),
                &operator_header(),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes).to_string();
        for phase in ["minted", "installed", "verified", "revoked"] {
            assert!(
                text.contains(phase),
                "400 must name the valid phases: {text}"
            );
        }
        assert!(registry.recorded_events().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_rotation_against_an_unknown_credential_is_a_404_naming_it() {
        let (app, registry) = rotation_app(vec![]);
        let resp = app
            .oneshot(post_phase(
                "ghost-credential",
                "minted",
                json!({}),
                &operator_header(),
            ))
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "an absent row is a finding — a rotation event may not detach from its row"
        );
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        assert!(String::from_utf8_lossy(&bytes).contains("ghost-credential"));
        assert!(registry.recorded_events().unwrap().is_empty());
    }

    #[tokio::test]
    async fn non_object_evidence_is_refused() {
        let (app, registry) = rotation_app(vec![row("boss-dev-forge-token")]);
        let resp = app
            .oneshot(post_phase(
                "boss-dev-forge-token",
                "minted",
                json!("a bare string"),
                &operator_header(),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert!(registry.recorded_events().unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_rotation_door_refuses_below_operator_tier_including_guest() {
        // A write with no presented identity would record an
        // unattributable rotation. (The READ doors refused a headerless
        // caller only from 2026-09-25, e84de48e; this one always did.)
        let (app, registry) = rotation_app(vec![row("boss-dev-forge-token")]);
        let user_resp = app
            .clone()
            .oneshot(post_phase(
                "boss-dev-forge-token",
                "minted",
                json!({}),
                &user_tier_header(),
            ))
            .await
            .unwrap();
        assert_eq!(user_resp.status(), StatusCode::FORBIDDEN);
        let guest_resp = app
            .oneshot(
                Request::post("/api/credentials/boss-dev-forge-token/rotation/minted")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(guest_resp.status(), StatusCode::FORBIDDEN);
        assert!(registry.recorded_events().unwrap().is_empty());
    }

    // ----- the declaration door (backlog ee368d0c) -----

    fn batch() -> serde_json::Value {
        json!({
            "tenant_id": "acme",
            "credentials": [{
                "id": "stripe-restricted-read",
                "kind": "stripe-restricted-key",
                "issuer": "stripe (the operator's dashboard, restricted key)",
                "principal": "the company's Stripe account",
                "scopes": ["charges: read"],
                "storage_location": "k8s Secret boss/boss-credential-broker-root key stripe-restricted-read",
                "consumers": [{ "kind": "env", "location": "dispatcher env BOSS_BROKER_STRIPE_KEY" }]
            }]
        })
    }

    fn post_batch(body: serde_json::Value, header: &str) -> Request<Body> {
        Request::post("/api/credentials/batch")
            .header("x-boss-user", header)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    /// A tenant's declaration lands insert-if-absent, leaves one
    /// `credential.declared` per inserted row signed as the caller,
    /// and a second publish inserts nothing.
    #[tokio::test]
    async fn a_tenant_declares_its_credentials_and_a_second_publish_inserts_nothing() {
        let (app, registry) = rotation_app(vec![]);
        let resp = app
            .clone()
            .oneshot(post_batch(batch(), &operator_header()))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body, json!({ "received": 1, "inserted": 1 }));
        let events = registry.recorded_events().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "credential.declared");
        assert_eq!(events[0].payload["declared_by"], "emp-ops");
        assert_eq!(events[0].payload["tenant_id"], "acme");
        assert!(
            events[0].payload.get("value").is_none(),
            "a declaration carries locations, never a value"
        );
        let resp = app
            .oneshot(post_batch(batch(), &operator_header()))
            .await
            .unwrap();
        assert_eq!(
            body_json(resp).await,
            json!({ "received": 1, "inserted": 0 })
        );
        assert_eq!(registry.recorded_events().unwrap().len(), 1);
        let row = registry
            .get("stripe-restricted-read")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            row.storage_location,
            "k8s Secret boss/boss-credential-broker-root key stripe-restricted-read"
        );
    }

    /// The whole batch is refused, naming the row, when one declaration
    /// is bad, when a row smuggles a value under an unknown key, or
    /// when no tenant is named; nothing lands.
    #[tokio::test]
    async fn a_bad_declaration_refuses_the_batch_by_name_and_a_value_is_refused_by_shape() {
        let (app, registry) = rotation_app(vec![]);
        let mut bad = batch();
        bad["credentials"][0]["storage_location"] = json!("");
        let resp = app
            .clone()
            .oneshot(post_batch(bad, &operator_header()))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        assert!(String::from_utf8_lossy(&bytes).contains("stripe-restricted-read"));

        let mut with_value = batch();
        with_value["credentials"][0]["value"] = json!("sk_live_not_a_real_key");
        let resp = app
            .clone()
            .oneshot(post_batch(with_value, &operator_header()))
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "an unknown key is refused before anything is stored"
        );

        let mut no_tenant = batch();
        no_tenant["tenant_id"] = json!("");
        let resp = app
            .clone()
            .oneshot(post_batch(no_tenant, &operator_header()))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

        assert!(registry.list().await.unwrap().is_empty());
        assert!(registry.recorded_events().unwrap().is_empty());
    }

    /// Writes are operator machinery: the auditor tier that reads the
    /// registry and a user-tier session are both refused.
    #[tokio::test]
    async fn the_declaration_door_is_operator_machinery() {
        for header in [probe_reader_header(), user_tier_header()] {
            let (app, registry) = rotation_app(vec![]);
            let resp = app.oneshot(post_batch(batch(), &header)).await.unwrap();
            assert_eq!(resp.status(), StatusCode::FORBIDDEN);
            assert!(registry.list().await.unwrap().is_empty());
        }
    }

    /// A phase instrument cannot claim the authenticated host acknowledgment
    /// that only the credential-owning delivery door can record (104a93d5).
    #[tokio::test]
    async fn an_asserted_actor_cannot_record_an_authenticated_runner_delivery() {
        let (app, registry) = rotation_app(vec![row("ops-runner-credential-forge")]);
        let forged = json!({
            "observation_id": "fake-attempt:delivered",
            "purpose": "authenticated-runner-delivery",
            "host": "forge",
            "job_id": uuid::Uuid::new_v4().to_string(),
            "actor": "automation:ops-runner"
        });
        let response = app
            .oneshot(
                Request::post("/api/credentials/ops-runner-credential-forge/rotation/verified")
                    .header("x-boss-user", operator_header())
                    .header("content-type", "application/json")
                    .body(Body::from(forged.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(registry.recorded_events().unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_installed_host_credential_records_its_exact_delivery() {
        installed_delivery(
            "ops-runner-credential-forge",
            "boss",
            "ops-runner-credential",
            "ops-runner-credential",
            true,
        )
        .await;
    }

    #[tokio::test]
    async fn delivery_uses_the_actual_declared_owner_not_a_deployment_spelling() {
        installed_delivery(
            "fake-host-runner-registry-row",
            "fake-namespace",
            "fake-runner-mount",
            "ops-runner-credential",
            true,
        )
        .await;
    }

    #[tokio::test]
    async fn a_different_credential_kind_cannot_acknowledge_runner_delivery() {
        installed_delivery(
            "ops-runner-credential-forge",
            "boss",
            "ops-runner-credential",
            "forgejo",
            false,
        )
        .await;
    }

    // The shell's typed JSON is accepted by the actual Jobs completion
    // door, with its own row/outbox receipt. This fake policy grants test
    // authority only; no live policy or executor declaration is installed.
    async fn complete_actual_delivery(
        dir: &std::path::Path,
        value: &str,
        packet: &str,
        attempt: &str,
        owner: &serde_json::Value,
    ) {
        use crate::{InMemoryJobs, JobsRepository};
        use boss_core::{
            job::{Job, Priority, Step, StepStatus, Subject},
            port::EventBus,
            publisher::DomainPublisher,
        };
        let jobs = Arc::new(InMemoryJobs::new());
        let bus = boss_testing::RecordingEventBus::new();
        let publisher = DomainPublisher::new(bus.clone() as Arc<dyn EventBus>, "jobs");
        let policy = Arc::new(
            boss_policy_client::FakePolicyClient::builder()
                .allow(
                    "platform-admin",
                    boss_policy_client::Action::Update,
                    boss_policy_client::Resource::step(),
                    boss_policy_client::Scope::All,
                )
                .allow(
                    "platform-admin",
                    boss_policy_client::Action::Read,
                    boss_policy_client::Resource::job(),
                    boss_policy_client::Scope::All,
                )
                .build(),
        );
        let state = crate::http::JobsApiState::minimal(
            jobs.clone(),
            bus,
            publisher,
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        );
        let app = crate::runner_credential::mount(crate::http::router(state), dir.into());
        let mut job = Job::new(
            "rotate-a-credential",
            Subject::new("custom", "fake-registry-row"),
            "Fake authenticated delivery",
            "emp-fake-owner",
            Priority::Standard,
            chrono::NaiveDate::from_ymd_opt(2026, 10, 5).unwrap(),
        );
        job.id = boss_core::job::JobId::from_uuid(uuid::Uuid::parse_str(packet).unwrap());
        job.metadata = json!({"host":"forge"});
        jobs.create_job(&job).await.unwrap();
        let mut step = Step::new(job.id, "credential-delivery", "Delivered", 0);
        step.status = StepStatus::Ready;
        jobs.add_step(&step).await.unwrap();
        let version = app
            .clone()
            .oneshot(
                Request::get(format!("/api/jobs/{}/steps/{}/version", job.id, step.id))
                    .header("x-boss-user", operator_header())
                    .header(crate::runner_credential::HEADER, value)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(version.status(), StatusCode::OK);
        let version = body_json(version).await;
        let request = json!({"operation_id":attempt,
            "expected":{"status":"ready","assignee_id":null,"version":version["version"]},
            "evidence":{"delivered_last_eight":"on-forge", "delivered_to":"fake-local-file",
                "delivered_receipt":owner}});
        let response = app
            .clone()
            .oneshot(
                Request::post(format!(
                    "/api/jobs/{}/steps/{}/complete-if",
                    job.id, step.id
                ))
                .header("x-boss-user", operator_header())
                .header(crate::runner_credential::HEADER, value)
                .header("content-type", "application/json")
                .body(Body::from(request.to_string()))
                .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let response = body_json(response).await;
        assert_eq!(status, StatusCode::OK, "{response}");
        assert_eq!(response["outcome"], "completed");
        let actual = jobs.get_step(&step.id).await.unwrap().unwrap();
        assert_eq!(
            actual.completed_by.as_ref().map(ToString::to_string),
            Some(crate::runner_credential::ACTOR.to_string())
        );
        assert_eq!(actual.metadata["delivered_receipt"], *owner);
        assert_eq!(actual.status, StepStatus::Completed);
    }

    async fn installed_delivery(
        credential_id: &str,
        namespace: &str,
        name: &str,
        kind: &str,
        admitted: bool,
    ) {
        let packet = uuid::Uuid::new_v4().to_string();
        let attempt = uuid::Uuid::new_v4().to_string();
        let dir = boss_testing::scratch_dir("runner-delivery-http");
        let value = "fake-runner-generation-installed-on-forge";
        let installed = json!({
            "job_id":packet,"host":"forge",
            "observation_id":format!("{attempt}:installed"),
            "secret_namespace":namespace,"secret_name":name,
            "secret_key":"forge.next","secret_uid":"fake-owner-uid",
            "precondition_version":"17","value_length":value.len(),
            "last_eight":"on-forge","replaced_staged_for":null,
            "carried_to_previous":null,"dropped_previous":null,
            "delivery":"off-host"
        });
        std::fs::write(dir.join("forge.next"), value).unwrap();
        std::fs::write(dir.join("forge.next.minted-for"), &packet).unwrap();
        std::fs::write(dir.join(format!("forge.recovery.{packet}.witness")),
            json!({"version":1,"job_id":packet,
                "credential_id":credential_id,
                "namespace":namespace,"name":name,
                "host":"forge","uid":"fake-owner-uid","attempt":attempt,
                "last_eight":"on-forge","promoted":false,
                "commands":{
                    "installed":installed,
                    "minted":{"job_id":packet,"host":"forge",
                        "observation_id":format!("{attempt}:minted"),
                        "issuer":"credential-broker (32 random bytes, base64url)",
                        "value_length":value.len()},
                    "verified":{"job_id":packet,"host":"forge",
                        "observation_id":format!("{attempt}:verified"),
                        "method":"api","observed":"resolved to host forge"},
                    "revoked":{"job_id":packet,"host":"forge",
                        "observation_id":format!("{attempt}:revoked"),
                        "delivered_last_eight":"on-forge","old_last_eight":null,
                        "carried_last_eight":null,
                        "confirmed_dead":"GET /api/jobs/runner-credential confirms host forge current; original old and carried values resolve to nothing"}
                }}).to_string()).unwrap();
        let mut declared = row(credential_id);
        declared.kind = kind.into();
        let (app, registry) = rotation_app(vec![declared]);
        let stage = boss_core::publisher::EventStamp::new(
            "jobs",
            boss_core::actor::ActorId::Automation("fake-broker-stage".into()),
        );
        registry
            .record_rotation(credential_id, RotationPhase::Installed, installed, &stage)
            .await
            .unwrap();
        let app = crate::runner_credential::mount(app, dir.clone());
        let identity = app
            .clone()
            .oneshot(
                Request::get(crate::runner_credential::WHOAMI_PATH)
                    .header("x-boss-user", operator_header())
                    .header(crate::runner_credential::HEADER, value)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let identity = body_json(identity).await;
        assert_eq!(
            identity["delivery"],
            json!({
                "credential_id":credential_id, "job_id":packet,
                "attempt":attempt,"secret_uid":"fake-owner-uid"
            }),
            "the installed host learns only its value-free owner context"
        );
        if admitted {
            for invalid in [
                json!({"job_id":uuid::Uuid::new_v4(),"attempt":attempt,"secret_uid":"fake-owner-uid"}),
                json!({"job_id":packet,"attempt":uuid::Uuid::new_v4(),"secret_uid":"fake-owner-uid"}),
                json!({"job_id":packet,"attempt":attempt,"secret_uid":"foreign-owner-uid"}),
            ] {
                let refused = app
                    .clone()
                    .oneshot(
                        Request::post(format!("/api/credentials/{credential_id}/delivery"))
                            .header("x-boss-user", operator_header())
                            .header(crate::runner_credential::HEADER, value)
                            .header("content-type", "application/json")
                            .body(Body::from(invalid.to_string()))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    refused.status(),
                    StatusCode::FORBIDDEN,
                    "authenticated possession cannot change the mounted owner binding"
                );
                assert_eq!(registry.recorded_events().unwrap().len(), 1);
            }
        }
        let response = app
            .clone()
            .oneshot(
                Request::post(format!("/api/credentials/{credential_id}/delivery"))
                    .header("x-boss-user", operator_header())
                    .header(crate::runner_credential::HEADER, value)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"job_id":packet,"attempt":attempt,
                    "secret_uid":"fake-owner-uid"})
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        if !admitted {
            assert_eq!(
                response.status(),
                StatusCode::CONFLICT,
                "an unrelated registry kind cannot own a host delivery acknowledgment"
            );
            assert_eq!(registry.recorded_events().unwrap().len(), 1);
            return;
        }
        assert_eq!(
            response.status(),
            StatusCode::ACCEPTED,
            "the already-installed host can acknowledge its bound generation"
        );
        let acknowledged = body_json(response).await;
        complete_actual_delivery(
            &dir,
            value,
            &packet,
            &attempt,
            &acknowledged["observation"]["receipt"],
        )
        .await;
        let replayed = app
            .clone()
            .oneshot(
                Request::post(format!("/api/credentials/{credential_id}/delivery"))
                    .header("x-boss-user", operator_header())
                    .header(crate::runner_credential::HEADER, value)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"job_id":packet,"attempt":attempt,
                    "secret_uid":"fake-owner-uid"})
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(replayed.status(), StatusCode::ACCEPTED);
        let replayed = body_json(replayed).await;
        assert_eq!(replayed["observation"]["outcome"], "replayed");
        assert_eq!(
            replayed["observation"]["receipt"], acknowledged["observation"]["receipt"],
            "lost acknowledgment replays the owner's original actor/time/event/bytes"
        );

        let readback = app
            .oneshot(
                Request::get(format!(
                    "/api/credentials/{credential_id}/delivery/{attempt}"
                ))
                .header("x-boss-user", operator_header())
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            readback.status(),
            StatusCode::OK,
            "promotion reads the original credential-owner receipt"
        );
        assert_eq!(
            body_json(readback).await["receipt"],
            acknowledged["observation"]["receipt"]
        );
        let events = registry.recorded_events().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].payload["_actor"], crate::runner_credential::ACTOR);
        assert_eq!(
            events[1].payload["purpose"],
            "authenticated-runner-delivery"
        );
        assert!(!events[1].payload.to_string().contains(value));
        let mut declared = row(credential_id);
        declared.kind = kind.into();
        let restored = Arc::new(InMemoryCredentials::new(vec![declared]));
        for event in &events {
            restored.restore_rotation(event).await.unwrap();
        }
        let restored_app = crate::runner_credential::mount(
            delivery_router(CredentialsApiState::new(restored.clone())),
            dir,
        );
        let replay = restored_app
            .oneshot(
                Request::post(format!("/api/credentials/{credential_id}/delivery"))
                    .header("x-boss-user", operator_header())
                    .header(crate::runner_credential::HEADER, value)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"job_id":packet,"attempt":attempt,
                    "secret_uid":"fake-owner-uid"})
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(replay.status(), StatusCode::ACCEPTED);
        let replay = body_json(replay).await;
        assert_eq!(replay["observation"]["outcome"], "replayed");
        assert_eq!(
            replay["observation"]["receipt"], acknowledged["observation"]["receipt"],
            "event restoration preserves the complete original owner observation"
        );
        assert!(
            restored.recorded_events().unwrap().is_empty(),
            "restoration and receipt replay emit no second event"
        );
    }

    #[tokio::test]
    async fn a_resolved_runner_cannot_record_the_brokers_stage_phases() {
        for phase in RotationPhase::ALL {
            let dir = boss_testing::scratch_dir("runner-stage-refusal");
            let value = "fake-host-only-runner-stage-control";
            std::fs::write(dir.join("forge.next"), value).unwrap();
            let (app, registry) = rotation_app(vec![row("ops-runner-credential-forge")]);
            let app = app.layer(axum::middleware::from_fn_with_state(
                Arc::new(dir),
                crate::runner_credential::resolve_runner_credential,
            ));
            let response = app
                .oneshot(
                    Request::post(format!(
                        "/api/credentials/ops-runner-credential-forge/rotation/{}",
                        phase.as_str()
                    ))
                    .header("x-boss-user", operator_header())
                    .header(crate::runner_credential::HEADER, value)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"observation_id":format!("fake:{}",phase.as_str())}).to_string(),
                    ))
                    .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::FORBIDDEN,
                "{} is stage authority",
                phase.as_str()
            );
            assert!(registry.recorded_events().unwrap().is_empty());
        }
    }
    #[tokio::test]
    async fn a_malformed_retained_delivery_is_loud_not_absent() {
        let attempt = uuid::Uuid::new_v4();
        let id = "ops-runner-credential-forge";
        let (app, registry) = rotation_app(vec![row(id)]);
        let stage = boss_core::publisher::EventStamp::new(
            "jobs",
            boss_core::actor::ActorId::Automation("fake-generic-stage".into()),
        );
        registry.record_rotation(id, RotationPhase::Verified,
            json!({"observation_id":format!("{attempt}:delivered"),"purpose":"unrelated-observation"}),
            &stage).await.unwrap();
        let response = app
            .oneshot(
                Request::get(format!("/api/credentials/{id}/delivery/{attempt}"))
                    .header("x-boss-user", operator_header())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "a retained but malformed observation is not an absent observation"
        );
    }
    #[tokio::test]
    async fn malformed_delivery_bodies_echo_no_presented_value() {
        let value = "fake-request-value-that-must-not-appear-in-a-response";
        let (app, _) = rotation_app(vec![row("ops-runner-credential-forge")]);
        let response = app
            .oneshot(
                Request::post("/api/credentials/ops-runner-credential-forge/delivery")
                    .header("x-boss-user", operator_header())
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(value).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        assert!(
            !String::from_utf8_lossy(&bytes).contains(value),
            "shape refusal must not echo caller-provided credential-like bytes"
        );
    }
}
