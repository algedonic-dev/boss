//! A machine client goes where its URL says, whatever proxy the process's
//! environment names (backlog 1ff6cabe).
//!
//! **This file holds exactly one test, and that is the point.** It sets
//! the process's proxy variables, which every other test in a binary
//! would read too, and reqwest reads them when a client is BUILT — so the
//! variables are set before each client below, alone in its process.
//!
//! WHAT WAS MEASURED (builder run 71ba06b3, 2026-10-10, and again on
//! origin/main 777524920 by run 3c8db6f5). reqwest honours
//! `HTTP_PROXY`/`HTTPS_PROXY`/`ALL_PROXY` by default, loopback included,
//! and `machine_token::Client` did not opt out: with the six variables
//! pointed at a dark loopback port,
//! `boss-testing/tests/a_policy_rule_cannot_blind_the_coverage_reader.rs`
//! failed with "error sending request" — the policy service's coverage
//! read, carrying the estate token, had gone to the proxy instead of to
//! the jobs router beside it. A proxy that ANSWERS is worse than a dark
//! one: it receives the token. The control below shows exactly that,
//! for a plain client given the same header.
//!
//! The constructor now finishes every builder with `.no_proxy()`, the
//! way it already finished it with `redirect::Policy::none()`: whatever
//! the caller and the environment set, a stamped request leaves on the
//! direct route or not at all.

use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use boss_core::machine_token::{self, HEADER, Source};

const TOKEN: &str = "estate-token-value";

/// Accept one connection, record its request head (lowercased), answer
/// 200. A listener nobody reaches leaves its thread parked on `accept`
/// until the process exits, which is the test's own lifetime.
fn answer_once(listener: std::net::TcpListener) -> Receiver<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let Ok((mut sock, _)) = listener.accept() else {
            return;
        };
        let mut head = Vec::new();
        let mut buf = [0u8; 1024];
        while !head.windows(4).any(|w| w == b"\r\n\r\n") {
            match sock.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => head.extend_from_slice(&buf[..n]),
            }
        }
        let _ = tx.send(String::from_utf8_lossy(&head).to_lowercase());
        let _ =
            sock.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
    });
    rx
}

/// What the origin and the proxy each saw of one request, sent by
/// `send` to the origin's URL after the environment names the proxy.
/// The proxy is `127.0.0.2`, a different host from the origin's
/// `127.0.0.1` and loopback on every Linux network namespace.
fn leg(send: impl FnOnce(String)) -> (Option<String>, Option<String>) {
    let origin = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let origin_at = origin.local_addr().unwrap();
    let origin_saw = answer_once(origin);
    let proxy = std::net::TcpListener::bind("127.0.0.2:0").unwrap();
    let proxy_url = format!("http://{}", proxy.local_addr().unwrap());
    let proxy_saw = answer_once(proxy);
    for var in [
        "http_proxy",
        "HTTP_PROXY",
        "https_proxy",
        "HTTPS_PROXY",
        "all_proxy",
        "ALL_PROXY",
    ] {
        // SAFETY: this binary holds one test, so no other thread reads
        // the environment while it is written.
        unsafe { std::env::set_var(var, &proxy_url) };
    }
    for var in ["no_proxy", "NO_PROXY"] {
        // SAFETY: as above.
        unsafe { std::env::remove_var(var) };
    }
    send(format!("http://{origin_at}/api/x"));
    let wait = Duration::from_millis(500);
    (
        origin_saw.recv_timeout(wait).ok(),
        proxy_saw.recv_timeout(wait).ok(),
    )
}

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(f)
}

fn fixed() -> Arc<Source> {
    Arc::new(Source::fixed(Some(TOKEN.into())))
}

#[test]
fn a_machine_client_follows_no_proxy_the_environment_names() {
    // The control: a plain client, built under the same environment and
    // handed the same header, goes to the proxy — so the fixture is one
    // reqwest reads, and the token reaches whoever answers there.
    let (origin, proxy) = leg(|url| {
        block_on(async {
            reqwest::Client::new()
                .get(url)
                .header(HEADER, TOKEN)
                .send()
                .await
                .unwrap();
        })
    });
    assert_eq!(
        origin, None,
        "the control went direct: the fixture proxies nothing"
    );
    assert!(
        proxy.is_some_and(|h| h.contains(TOKEN)),
        "the control's proxy did not see the token, so this fixture cannot catch a leak"
    );

    let (origin, proxy) = leg(|url| {
        let client =
            machine_token::Client::build_with_source(reqwest::Client::builder(), fixed()).unwrap();
        block_on(async {
            client.get(url).send().await.unwrap();
        })
    });
    assert_eq!(
        proxy, None,
        "a machine client sent its request to the proxy"
    );
    assert!(
        origin.is_some_and(|h| h.contains(TOKEN)),
        "the origin did not receive the stamped request directly"
    );

    #[cfg(feature = "blocking")]
    {
        let (origin, proxy) = leg(|url| {
            machine_token::BlockingClient::build_with_source(
                reqwest::blocking::Client::builder(),
                fixed(),
            )
            .unwrap()
            .get(url)
            .send()
            .unwrap();
        });
        assert_eq!(
            proxy, None,
            "a blocking machine client sent its request to the proxy"
        );
        assert!(
            origin.is_some_and(|h| h.contains(TOKEN)),
            "the origin did not receive the blocking client's stamped request directly"
        );
    }
}

/// The blocking leg above compiles only under `--features blocking`, so
/// `cargo test -p boss-core` alone would skip it in silence (review
/// bad26eb2, finding 3). This marker exists only in that build and is
/// ignored, so the skip prints as `ignored, <reason>` in the run's own
/// output. It reads no environment, so it does not break the one-test
/// rule above; the workspace test run enables the feature (boss-cli
/// turns it on through unification) and this marker is not built there.
#[cfg(not(feature = "blocking"))]
#[test]
#[ignore = "the BlockingClient leg needs --features blocking; run cargo test -p boss-core --features blocking"]
fn the_blocking_leg_did_not_run_without_the_blocking_feature() {}
