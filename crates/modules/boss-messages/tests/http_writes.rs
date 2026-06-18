//! HTTP-level write path tests for the messages service.
//!
//! Each test verifies one business contract via the actual HTTP router.

mod common;

use axum::http::StatusCode;
use boss_messages::types::{Message, MessageKind};
use boss_testing::TestRequest;
use common::{MessageTestApp, message_fixture};
use serde_json::json;

// ---------------------------------------------------------------------------
// POST /api/messages/send — compose
// ---------------------------------------------------------------------------

#[tokio::test]
async fn post_send_returns_201_created() {
    let app = MessageTestApp::new();

    let body = json!({
        "sender_id": "emp-1",
        "recipient_id": "emp-2",
        "subject": "Hello",
        "body": "Test body",
    });

    let resp = TestRequest::post("/api/messages/send")
        .json(&body)
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::CREATED);
}

#[tokio::test]
async fn post_send_emits_message_sent_event() {
    let app = MessageTestApp::new();

    let body = json!({
        "sender_id": "emp-1",
        "recipient_id": "emp-2",
        "subject": "Hello",
        "body": "Test body",
    });

    TestRequest::post("/api/messages/send")
        .json(&body)
        .send(&app.router)
        .await
        .assert_status(StatusCode::CREATED);

    app.bus.assert_event_emitted("messages.message.sent");
}

#[tokio::test]
async fn post_send_returns_new_message_id() {
    let app = MessageTestApp::new();

    let body = json!({
        "sender_id": "emp-1",
        "recipient_id": "emp-2",
        "subject": "Hi",
        "body": "Body",
    });

    let resp = TestRequest::post("/api/messages/send")
        .json(&body)
        .send(&app.router)
        .await;
    resp.assert_status(StatusCode::CREATED);

    let parsed: serde_json::Value = resp.assert_json();
    let id = parsed["id"].as_str().expect("response should include id");
    assert!(id.starts_with("msg-"), "expected msg- prefix, got {id}");
}

// ---------------------------------------------------------------------------
// POST /api/messages/{id}/read — mark as read
// ---------------------------------------------------------------------------

#[tokio::test]
async fn post_read_returns_200_ok() {
    let msg = message_fixture("msg-read-1");
    let app = MessageTestApp::with_messages(vec![msg]);

    let resp = TestRequest::post("/api/messages/msg-read-1/read")
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::OK);
}

#[tokio::test]
async fn post_read_emits_message_read_event() {
    let msg = message_fixture("msg-read-2");
    let app = MessageTestApp::with_messages(vec![msg]);

    TestRequest::post("/api/messages/msg-read-2/read")
        .send(&app.router)
        .await
        .assert_status(StatusCode::OK);

    let event = app.bus.assert_event_emitted("messages.message.read");
    assert_eq!(
        event.payload.get("id").and_then(|v| v.as_str()),
        Some("msg-read-2"),
    );
}

// ---------------------------------------------------------------------------
// DELETE /api/messages/{id}
// ---------------------------------------------------------------------------

#[tokio::test]
async fn delete_existing_message_returns_204() {
    let msg = message_fixture("msg-del-1");
    let app = MessageTestApp::with_messages(vec![msg]);

    let resp = TestRequest::delete("/api/messages/msg-del-1")
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn delete_existing_message_emits_deleted_event() {
    let msg = message_fixture("msg-del-2");
    let app = MessageTestApp::with_messages(vec![msg]);

    TestRequest::delete("/api/messages/msg-del-2")
        .send(&app.router)
        .await
        .assert_status(StatusCode::NO_CONTENT);

    let event = app.bus.assert_event_emitted("messages.message.deleted");
    assert_eq!(
        event.payload.get("id").and_then(|v| v.as_str()),
        Some("msg-del-2"),
    );
}

#[tokio::test]
async fn delete_nonexistent_message_returns_404() {
    let app = MessageTestApp::new();

    let resp = TestRequest::delete("/api/messages/msg-missing")
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_nonexistent_message_does_not_emit_event() {
    let app = MessageTestApp::new();

    TestRequest::delete("/api/messages/msg-missing")
        .send(&app.router)
        .await
        .assert_status(StatusCode::NOT_FOUND);

    app.bus.assert_event_not_emitted("messages.message.deleted");
}

// ---------------------------------------------------------------------------
// POST /api/messages/{id}/archive
// ---------------------------------------------------------------------------

#[tokio::test]
async fn archive_existing_message_returns_204() {
    let msg = message_fixture("msg-arch-1");
    let app = MessageTestApp::with_messages(vec![msg]);

    let resp = TestRequest::post("/api/messages/msg-arch-1/archive")
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn archive_existing_message_emits_archived_event() {
    let msg = message_fixture("msg-arch-2");
    let app = MessageTestApp::with_messages(vec![msg]);

    TestRequest::post("/api/messages/msg-arch-2/archive")
        .send(&app.router)
        .await
        .assert_status(StatusCode::NO_CONTENT);

    let event = app.bus.assert_event_emitted("messages.message.archived");
    assert_eq!(
        event.payload.get("id").and_then(|v| v.as_str()),
        Some("msg-arch-2"),
    );
}

#[tokio::test]
async fn archive_nonexistent_message_returns_404() {
    let app = MessageTestApp::new();

    let resp = TestRequest::post("/api/messages/msg-missing/archive")
        .send(&app.router)
        .await;

    resp.assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn archive_nonexistent_message_does_not_emit_event() {
    let app = MessageTestApp::new();

    TestRequest::post("/api/messages/msg-missing/archive")
        .send(&app.router)
        .await
        .assert_status(StatusCode::NOT_FOUND);

    app.bus
        .assert_event_not_emitted("messages.message.archived");
}

// ---------------------------------------------------------------------------
// GET /api/messages/{id}/thread — conversation thread
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_thread_returns_reply_chain() {
    // Build a thread: root -> reply1 -> reply2
    let mut root = message_fixture("msg-root");
    root.reply_to = None;
    let mut reply1 = message_fixture("msg-reply-1");
    reply1.reply_to = Some("msg-root".to_string());
    let mut reply2 = message_fixture("msg-reply-2");
    reply2.reply_to = Some("msg-reply-1".to_string());

    let app = MessageTestApp::with_messages(vec![root, reply1, reply2]);

    let resp = TestRequest::get("/api/messages/msg-root/thread")
        .send(&app.router)
        .await;
    resp.assert_status(StatusCode::OK);

    let msgs: Vec<Message> = resp.assert_json();
    assert!(
        msgs.len() >= 2,
        "expected thread to contain multiple messages, got {}",
        msgs.len()
    );
    // Root message must be in the thread.
    assert!(msgs.iter().any(|m| m.id == "msg-root"));
}

#[tokio::test]
async fn compose_with_reply_to_creates_reply() {
    let root = message_fixture("msg-parent");
    let app = MessageTestApp::with_messages(vec![root]);

    let body = json!({
        "sender_id": "emp-1",
        "recipient_id": "emp-2",
        "subject": "Re: Hi",
        "body": "Replying",
        "reply_to": "msg-parent",
    });

    let resp = TestRequest::post("/api/messages/send")
        .json(&body)
        .send(&app.router)
        .await;
    resp.assert_status(StatusCode::CREATED);

    let parsed: serde_json::Value = resp.assert_json();
    let new_id = parsed["id"].as_str().unwrap();

    // Fetch the new message and verify reply_to is set.
    let fetch = TestRequest::get(format!("/api/messages/{new_id}"))
        .send(&app.router)
        .await;
    fetch.assert_status(StatusCode::OK);
    let fetched: Message = fetch.assert_json();
    assert_eq!(fetched.reply_to.as_deref(), Some("msg-parent"));
}

// ---------------------------------------------------------------------------
// GET /api/messages/inbox/{employee_id}
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_inbox_returns_messages_for_recipient() {
    let mut m1 = message_fixture("msg-inbox-1");
    m1.recipient_id = "emp-42".to_string();
    let mut m2 = message_fixture("msg-inbox-2");
    m2.recipient_id = "emp-42".to_string();
    let mut m3 = message_fixture("msg-inbox-3");
    m3.recipient_id = "emp-other".to_string();

    let app = MessageTestApp::with_messages(vec![m1, m2, m3]);

    let resp = TestRequest::get("/api/messages/inbox/emp-42")
        .send(&app.router)
        .await;
    resp.assert_status(StatusCode::OK);

    let msgs: Vec<Message> = resp.assert_json();
    assert_eq!(msgs.len(), 2);
    assert!(msgs.iter().all(|m| m.recipient_id == "emp-42"));
}

#[tokio::test]
async fn get_inbox_for_unknown_employee_returns_empty_list() {
    let app = MessageTestApp::new();

    let resp = TestRequest::get("/api/messages/inbox/emp-ghost")
        .send(&app.router)
        .await;
    resp.assert_status(StatusCode::OK);

    let msgs: Vec<Message> = resp.assert_json();
    assert!(msgs.is_empty());
}

// Silence unused warnings for MessageKind when only used in fixture.
#[allow(dead_code)]
fn _kind_marker() -> MessageKind {
    MessageKind::direct()
}
