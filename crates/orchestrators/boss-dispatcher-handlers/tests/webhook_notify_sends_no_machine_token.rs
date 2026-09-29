//! `webhook.notify` POSTs every forwarded event to whatever host
//! `BOSS_EVENT_WEBHOOK_URL` names — a party outside the estate — so the
//! request it sends must never carry the estate machine token
//! (backlog ee96c839, from the review of
//! fix/a-machine-client-follows-no-redirect, 2026-09-28). Until this
//! car it sent on `handlers::common::api_client()`, whose default
//! headers carry the token: the configured host itself received it,
//! redirects or not.
//!
//! Lives under `tests/` rather than beside the handler because the
//! token has to be PRESENT for the absence to mean anything, and the
//! process's token source (`machine_token::shared`) latches the token
//! directory the first time anything in the process asks. In the lib's
//! test binary another test's `api_client()` can get there first and
//! latch the default directory, which holds nothing — and then a
//! leaking handler would pass. Here this is the only test, the
//! directory is set before any client is built, and the control leg
//! proves the token is live by sending it on `api_client()` to the same
//! listener.

use boss_dispatcher::rules::handler::{Handler, InvocationContext};
use boss_dispatcher_handlers::handlers::common::api_client;
use boss_dispatcher_handlers::handlers::webhook_notify::WebhookNotify;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const TOKEN: &str = "estate-token-that-must-stay-home";

/// Accept one request on `listener`, answer 200, and return its header
/// block lowercased.
async fn one_request(listener: &TcpListener) -> String {
    let (mut sock, _) = listener.accept().await.unwrap();
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = sock.read(&mut chunk).await.unwrap();
        assert!(n > 0, "the client closed before sending its headers");
        buf.extend_from_slice(&chunk[..n]);
    }
    sock.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&buf).to_lowercase();
    text.split("\r\n\r\n")
        .next()
        .unwrap_or_default()
        .to_string()
}

#[tokio::test]
async fn the_webhook_post_carries_no_machine_token() {
    let dir = boss_testing::scratch_dir("webhook-notify-token");
    boss_testing::scratch::write_file(&dir.join("current"), TOKEN);
    // SAFETY: the only test in this binary, and no thread reads the
    // environment yet — nothing has built a client.
    unsafe { std::env::set_var(boss_core::machine_token::TOKEN_DIR_ENV, &dir) };

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/callback", listener.local_addr().unwrap());

    // Control: the jobs-API client carries the token here, so an absence
    // below is the handler's doing, not an empty source.
    let (control, sent) = tokio::join!(one_request(&listener), api_client().post(&url).send());
    sent.unwrap();
    assert!(
        control.contains(&format!("x-boss-machine-token: {TOKEN}")),
        "control: api_client() sent no token, so the leg below proves nothing:\n{control}"
    );

    let handler = WebhookNotify::new(Some(url.clone()));
    let ctx = InvocationContext {
        event_timestamp: None,
        rule_name: "forward-invoice-created".into(),
        triggering_event_id: "evt-1".into(),
        triggering_topic: "commerce.invoice.created".into(),
        event_payload: json!({ "id": "inv-1" }),
    };
    let (seen, invoked) = tokio::join!(one_request(&listener), handler.invoke(&[], &ctx));
    invoked.unwrap();
    assert!(
        seen.starts_with("post /callback"),
        "the handler did not POST the callback:\n{seen}"
    );
    assert!(
        !seen.contains("x-boss-machine-token") && !seen.contains(TOKEN),
        "webhook.notify sent the estate machine token to the configured webhook host:\n{seen}"
    );
}
