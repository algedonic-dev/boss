//! The dispatcher handlers' client presents the machine token as it
//! stands NOW, not as it stood when the dispatcher booted (design
//! 6805c764, car 2, the handlers slice; backlog 2710c8fc).
//!
//! WHAT WAS MEASURED (review S1 of car 2 slice 1, 2026-09-26, and the
//! pin `no_client_bakes_the_machine_token_in`, row
//! `handlers/common.rs`). `api_client()` read the token ONCE, into
//! reqwest `default_headers`, and every handler holds the client it was
//! built with for the life of the process — about sixty of them, built
//! once at boot in `boss_dispatcher.rs`. The broker rotates by moving
//! `current` to `previous` and then revoking `previous` (car 3), so at
//! an enforcing gate every handler would have been refused the moment
//! the boot-time value was revoked, until someone restarted the
//! dispatcher: the ordering the file source exists to remove.
//!
//! WHAT THIS HOLDS. One client, built once before the token changes,
//! sends the value `current` holds when each request is made: the first
//! value, then the rotated one, with no rebuild. The requests really
//! leave the process (a listener reads their header block), because a
//! reqwest client merges its default headers at send time, so a request
//! merely BUILT would show nothing for the old client and prove nothing.
//!
//! Its own test binary, like `webhook_notify_sends_no_machine_token`:
//! the process's token source (`machine_token::shared`) latches the
//! token directory the first time anything asks, so the directory is
//! set here before any client exists.

use boss_dispatcher_handlers::handlers::common::api_client;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

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
async fn one_client_built_at_boot_sends_the_rotated_token() {
    let dir = boss_testing::scratch_dir("api-client-rotation");
    boss_testing::scratch::write_file(&dir.join("current"), "token-at-boot");
    // SAFETY: the only test in this binary, and no thread reads the
    // environment yet — nothing has built a client.
    unsafe { std::env::set_var(boss_core::machine_token::TOKEN_DIR_ENV, &dir) };

    // Built ONCE, as the dispatcher builds each handler's client at boot.
    let client = api_client();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/api/jobs", listener.local_addr().unwrap());

    let (first, sent) = tokio::join!(one_request(&listener), client.get(&url).send());
    sent.unwrap();
    assert!(
        first.contains("x-boss-machine-token: token-at-boot"),
        "control: the handlers' client sent no token at all, so the rotation below proves \
         nothing:\n{first}"
    );

    // The broker's rotation: `current` now holds a new value.
    boss_testing::scratch::write_file(&dir.join("current"), "token-after-rotation");

    // The source re-reads every REREAD (5 s); allow three of them.
    let deadline = Instant::now() + boss_core::machine_token::REREAD * 3;
    let mut seen = String::new();
    while Instant::now() < deadline {
        let (head, sent) = tokio::join!(one_request(&listener), client.get(&url).send());
        sent.unwrap();
        seen = head;
        if seen.contains("x-boss-machine-token: token-after-rotation") {
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!(
        "the handlers' client, built before the rotation, still sends the boot-time token \
         {:?} after three re-read intervals — a revoke of `previous` would refuse every \
         handler until the dispatcher restarts:\n{seen}",
        boss_core::machine_token::REREAD * 3
    );
}
