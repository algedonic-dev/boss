//! Axum HTTP handlers for the messages API.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use boss_classes_client::ClassesClient;
use boss_core::primitives::ClassRef;
use boss_core::publisher::DomainPublisher;
use boss_policy::{AccessTier, User};
use boss_policy_client::CurrentUser;

use crate::port::{MessageError, MessageRepository};
use crate::types::{EntityRef, Message, MessageKind};

/// Gate used by the scoped message endpoints. Two categories pass:
///
/// 1. **Trusted internal callers** — no `x-boss-user` header means the
///    request arrived over loopback from a sibling service (escalation,
///    inventory, etc.) or from a test harness. These get `role=guest`
///    from the extractor's default; we treat them as internal.
/// 2. **Operator-tier callers** — explicit elevation (e.g., an
///    operator inspecting someone else's inbox) bypasses the match.
///
/// Everyone else has to match the `employee_id` / `sender_id` they
/// claim. The gateway always injects `x-boss-user` for external
/// requests, so real sessions never land in the trusted-internal path.
fn is_trusted(user: &User) -> bool {
    user.role == "guest" || user.access_tier == AccessTier::Operator
}

pub struct MessageApiState<R: MessageRepository> {
    pub messages: Arc<R>,
    pub publisher: Option<DomainPublisher>,
    pub clock: Arc<dyn boss_clock_client::ClockClient>,
    /// Class registry for `MessageKind` validation. When configured,
    /// `send` checks an explicitly-supplied kind against the active
    /// Class set under `(subject_kind='message')`; an omitted kind
    /// defaults to `direct` and skips the gate. When `None`, the API
    /// is permissive (test path) — matching the marketing-asset kind
    /// gate in boss-catalog. The production binary always wires `Some`
    /// from the required `classes_api_url`.
    pub classes_client: Option<Arc<dyn ClassesClient>>,
}

pub fn router<R: MessageRepository + 'static>(state: MessageApiState<R>) -> Router {
    let shared = Arc::new(state);
    Router::new()
        .route("/api/messages/health", get(health))
        .route("/api/messages/inbox/{employee_id}", get(inbox::<R>))
        .route("/api/messages/unread/{employee_id}", get(unread::<R>))
        .route(
            "/api/messages/{id}",
            get(get_message::<R>).delete(delete_message::<R>),
        )
        .route("/api/messages/{id}/read", post(mark_read::<R>))
        .route("/api/messages/{id}/archive", post(archive_message::<R>))
        .route("/api/messages/{id}/thread", get(thread::<R>))
        .route("/api/messages/send", post(send_message::<R>))
        .route("/api/messages/batch", post(batch_messages::<R>))
        .with_state(shared)
}

async fn batch_messages<R: MessageRepository + 'static>(
    State(state): State<Arc<MessageApiState<R>>>,
    Json(body): Json<Vec<Message>>,
) -> Response {
    let now = boss_clock_client::now_from(&state.clock).await;
    let mut inserted = 0usize;
    for msg in &body {
        if let Err(e) = state.messages.send(msg).await {
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
        // Emit MESSAGE_SENT per row so the messages rebuilder can
        // reproduce the projection from audit_log alone — otherwise
        // sim-seeded chatter and bulk imports would be wiped on the
        // next boss-rebuild-all cycle.
        if let Some(pub_) = &state.publisher {
            pub_.emit_at(
                crate::events::MESSAGE_SENT,
                serde_json::to_value(msg).unwrap_or_else(|_| serde_json::json!({"id": msg.id})),
                now,
            )
            .await;
        }
        inserted += 1;
    }
    (
        StatusCode::OK,
        Json(serde_json::json!({"ok": true, "inserted": inserted})),
    )
        .into_response()
}

#[cfg(feature = "postgres")]
const STORAGE: &str = "postgres";
#[cfg(not(feature = "postgres"))]
const STORAGE: &str = "in-memory";

async fn health() -> Json<boss_core::startup::HealthResponse> {
    Json(boss_core::startup::health_response(
        "boss-messages-api",
        env!("CARGO_PKG_VERSION"),
        STORAGE,
    ))
}

#[derive(Serialize)]
struct UnreadResponse {
    count: u32,
}

#[derive(Serialize)]
struct OkResponse {
    ok: bool,
}

async fn inbox<R: MessageRepository + 'static>(
    State(state): State<Arc<MessageApiState<R>>>,
    CurrentUser(user): CurrentUser,
    Path(employee_id): Path<String>,
) -> Response {
    // Security gate: a real session may only read its own inbox;
    // operators or trusted internal callers may read any.
    if !is_trusted(&user) && user.id != employee_id {
        return StatusCode::FORBIDDEN.into_response();
    }
    match state.messages.inbox(&employee_id).await {
        Ok(msgs) => Json(msgs).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn unread<R: MessageRepository + 'static>(
    State(state): State<Arc<MessageApiState<R>>>,
    CurrentUser(user): CurrentUser,
    Path(employee_id): Path<String>,
) -> Response {
    if !is_trusted(&user) && user.id != employee_id {
        return StatusCode::FORBIDDEN.into_response();
    }
    match state.messages.unread_count(&employee_id).await {
        Ok(count) => Json(UnreadResponse { count }).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn get_message<R: MessageRepository + 'static>(
    State(state): State<Arc<MessageApiState<R>>>,
    Path(id): Path<String>,
) -> Response {
    match state.messages.message_by_id(&id).await {
        Ok(Some(msg)) => Json(msg).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, format!("no message with ID {id}")).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn thread<R: MessageRepository + 'static>(
    State(state): State<Arc<MessageApiState<R>>>,
    Path(id): Path<String>,
) -> Response {
    match state.messages.thread(&id).await {
        Ok(msgs) => Json(msgs).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn mark_read<R: MessageRepository + 'static>(
    State(state): State<Arc<MessageApiState<R>>>,
    Path(id): Path<String>,
) -> Response {
    // Generate the timestamp once and use it for both the projection
    // write and the event payload, so a rebuild from audit_log
    // produces an identical `read_at` value.
    let read_at = boss_clock_client::now_from(&state.clock).await;
    match state.messages.mark_read(&id, read_at).await {
        Ok(()) => {
            if let Some(pub_) = &state.publisher {
                pub_.emit_at(
                    crate::events::MESSAGE_READ,
                    serde_json::json!({
                        "id": id,
                        "read_at": read_at,
                    }),
                    read_at,
                )
                .await;
            }
            Json(OkResponse { ok: true }).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn delete_message<R: MessageRepository + 'static>(
    State(state): State<Arc<MessageApiState<R>>>,
    Path(id): Path<String>,
) -> Response {
    match state.messages.delete_message(&id).await {
        Ok(()) => {
            if let Some(pub_) = &state.publisher {
                let now = boss_clock_client::now_from(&state.clock).await;
                pub_.emit_at(
                    crate::events::MESSAGE_DELETED,
                    serde_json::json!({
                        "id": id,
                        "deleted_at": now,
                    }),
                    now,
                )
                .await;
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Err(MessageError::NotFound(msg)) => (StatusCode::NOT_FOUND, msg).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn archive_message<R: MessageRepository + 'static>(
    State(state): State<Arc<MessageApiState<R>>>,
    Path(id): Path<String>,
) -> Response {
    match state.messages.archive_message(&id).await {
        Ok(()) => {
            if let Some(pub_) = &state.publisher {
                let now = boss_clock_client::now_from(&state.clock).await;
                pub_.emit_at(
                    crate::events::MESSAGE_ARCHIVED,
                    serde_json::json!({
                        "id": id,
                        "archived_at": now,
                    }),
                    now,
                )
                .await;
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Err(MessageError::NotFound(msg)) => (StatusCode::NOT_FOUND, msg).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
struct ComposeRequest {
    /// Optional caller-supplied message id. Deterministic callers (the
    /// dispatcher's `messages.notify` handler) pass a stable id like
    /// `notify:{step_id}:{recipient}` so a redelivered notification
    /// (JetStream at-least-once) collapses on the messages `ON CONFLICT
    /// (id) DO NOTHING` insert instead of minting a duplicate inbox row.
    /// Absent for direct/human callers → a random `msg-{uuid}` (never
    /// time-based, so same-instant sends don't collide).
    #[serde(default)]
    id: Option<String>,
    sender_id: String,
    recipient_id: String,
    subject: String,
    body: String,
    entity_ref: Option<EntityRef>,
    kind: Option<MessageKind>,
    reply_to: Option<String>,
}

/// Validate an explicitly-supplied `MessageKind` against the Class
/// registry under `(subject_kind='message')`.
///
/// `MessageKind` is a free-text wrapper (the closed enum was lifted to a
/// String-newtype in v1.1.0). Identity-first: an omitted kind defaults
/// to `direct` and skips the gate — it only fires once a value is
/// supplied. Same contract as `check_marketing_kind` in boss-catalog:
/// permissive when no registry is wired (test path), fail-closed 503
/// when unreachable, 400 on an unregistered code.
async fn check_kind(
    classes_client: Option<&Arc<dyn ClassesClient>>,
    kind: Option<&MessageKind>,
) -> Result<(), Response> {
    let Some(kind) = kind else {
        return Ok(());
    };
    let Some(client) = classes_client else {
        return Ok(());
    };
    let class_ref = ClassRef::new("message", kind.as_str());
    match client.class_exists(&class_ref).await {
        Ok(true) => Ok(()),
        Ok(false) => Err((
            StatusCode::BAD_REQUEST,
            format!(
                "unknown message kind `{kind}` — register it as a Class \
                 first (subject_kind='message')"
            ),
        )
            .into_response()),
        Err(e) => Err((
            StatusCode::SERVICE_UNAVAILABLE,
            format!("classes registry unreachable: {e}"),
        )
            .into_response()),
    }
}

async fn send_message<R: MessageRepository + 'static>(
    State(state): State<Arc<MessageApiState<R>>>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<ComposeRequest>,
) -> Response {
    // Security gate: a real session may only send as itself;
    // operators or trusted internal callers may spoof any sender
    // (e.g., system-authored escalation signals).
    if !is_trusted(&user) && user.id != body.sender_id {
        return (
            StatusCode::FORBIDDEN,
            "sender_id does not match authenticated user",
        )
            .into_response();
    }
    // Class-registry gate: a *present* kind must be a registered Class
    // under (subject_kind='message'). An absent kind defaults to
    // `direct` below and is not gated.
    if let Err(resp) = check_kind(state.classes_client.as_ref(), body.kind.as_ref()).await {
        return resp;
    }
    let msg = Message {
        // Caller-supplied deterministic id wins (lets a redelivered
        // notification collapse on ON CONFLICT (id)); otherwise a random
        // id. The fallback is RANDOM, never time-based — two sends in the
        // same instant must get distinct ids.
        id: body
            .id
            .unwrap_or_else(|| format!("msg-{}", uuid::Uuid::new_v4().as_simple())),
        sender_id: body.sender_id,
        recipient_id: body.recipient_id,
        subject: body.subject,
        body: body.body,
        entity_ref: body.entity_ref,
        kind: body.kind.unwrap_or_else(MessageKind::direct),
        sent_at: boss_clock_client::now_from(&state.clock).await,
        read_at: None,
        reply_to: body.reply_to,
    };

    match state.messages.send(&msg).await {
        Ok(()) => {
            if let Some(pub_) = &state.publisher {
                // Full row state so the rebuilder can reconstruct
                // the messages projection from this event alone. Any
                // listener that needs only the id can still pluck it
                // out of `payload.id`.
                pub_.emit_at(
                    crate::events::MESSAGE_SENT,
                    serde_json::to_value(&msg)
                        .unwrap_or_else(|_| serde_json::json!({ "id": msg.id })),
                    msg.sent_at,
                )
                .await;
            }
            (
                StatusCode::CREATED,
                Json(serde_json::json!({"ok": true, "id": msg.id})),
            )
                .into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    use crate::in_memory::InMemoryMessages;
    use crate::types::*;
    use chrono::Utc;

    fn test_message(id: &str, recipient: &str, read: bool) -> Message {
        Message {
            id: id.to_string(),
            sender_id: "sender-001".to_string(),
            recipient_id: recipient.to_string(),
            subject: format!("Subject {id}"),
            body: format!("Body {id}"),
            entity_ref: None,
            kind: MessageKind::DIRECT.into(),
            sent_at: Utc::now(),
            read_at: if read { Some(Utc::now()) } else { None },
            reply_to: None,
        }
    }

    fn test_app() -> Router {
        let messages = Arc::new(InMemoryMessages::new(vec![
            test_message("msg-001", "emp-001", false),
            test_message("msg-002", "emp-001", true),
            test_message("msg-003", "emp-002", false),
        ]));
        router(MessageApiState {
            messages,
            publisher: None,
            clock: Arc::new(boss_clock_client::WallClockClient),
            classes_client: None,
        })
    }

    fn app_with_classes(classes: Arc<dyn ClassesClient>) -> Router {
        let messages = Arc::new(InMemoryMessages::new(vec![]));
        router(MessageApiState {
            messages,
            publisher: None,
            clock: Arc::new(boss_clock_client::WallClockClient),
            classes_client: Some(classes),
        })
    }

    async fn post_send(app: Router, body: serde_json::Value) -> axum::http::Response<Body> {
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/messages/send")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn send_rejected_when_kind_unknown() {
        use boss_classes_client::FakeClassesClient;
        // Registry knows only `signal`; the request asks for `urgent`
        // → 400 with the actionable error message.
        let classes = Arc::new(FakeClassesClient::with(vec![ClassRef::new(
            "message", "signal",
        )])) as Arc<dyn ClassesClient>;
        let app = app_with_classes(classes);
        let resp = post_send(
            app,
            serde_json::json!({
                "sender_id": "emp-1",
                "recipient_id": "emp-2",
                "subject": "hi",
                "body": "there",
                "kind": "urgent",
            }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        assert!(
            body.contains("urgent") && body.contains("subject_kind='message'"),
            "error must name the rejected kind and the registry shape, got: {body}"
        );
    }

    #[tokio::test]
    async fn send_accepts_registered_kind() {
        use boss_classes_client::FakeClassesClient;
        let classes = Arc::new(FakeClassesClient::with(vec![ClassRef::new(
            "message", "signal",
        )])) as Arc<dyn ClassesClient>;
        let app = app_with_classes(classes);
        let resp = post_send(
            app,
            serde_json::json!({
                "sender_id": "emp-1",
                "recipient_id": "emp-2",
                "subject": "hi",
                "body": "there",
                "kind": "signal",
            }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn send_skips_kind_gate_when_absent() {
        use boss_classes_client::FakeClassesClient;
        // Identity-first: a send with no kind defaults to `direct` and
        // is not gated even with a strict registry wired.
        let classes = Arc::new(FakeClassesClient::with(vec![ClassRef::new(
            "message", "signal",
        )])) as Arc<dyn ClassesClient>;
        let app = app_with_classes(classes);
        let resp = post_send(
            app,
            serde_json::json!({
                "sender_id": "emp-1",
                "recipient_id": "emp-2",
                "subject": "hi",
                "body": "there",
            }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn health_ok() {
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .uri("/api/messages/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn inbox_returns_messages() {
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .uri("/api/messages/inbox/emp-001")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let msgs: Vec<Message> = serde_json::from_slice(&body).unwrap();
        assert_eq!(msgs.len(), 2);
    }

    #[tokio::test]
    async fn unread_count_ok() {
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .uri("/api/messages/unread/emp-001")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["count"], 1);
    }

    #[tokio::test]
    async fn get_message_found() {
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .uri("/api/messages/msg-001")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn get_message_not_found() {
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .uri("/api/messages/msg-999")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn mark_read_ok() {
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/messages/msg-001/read")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["ok"], true);
    }

    #[tokio::test]
    async fn delete_message_ok() {
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/messages/msg-001")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn delete_message_not_found() {
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/messages/msg-999")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn archive_message_ok() {
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/messages/msg-001/archive")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn archive_message_not_found() {
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/messages/msg-999/archive")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
