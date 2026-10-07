//! Exercise public constructors in a private process: no mutation of the
//! test runner's global machine source, and no estate credential material.
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::time::{Duration, Instant};

use boss_clock_client::{ClockClient, ClockClientError, ReqwestClockClient, subscribe_ticks};
use futures::StreamExt;

const FIRST: &str = "synthetic-clock-first";
const SECOND: &str = "synthetic-clock-second";
const NOW: &str = r#"{"now":"2025-04-01T13:00:00Z","simulated":true}"#;

fn exercise(mode: &str, listed: bool, token: bool) -> Vec<String> {
    let dir = boss_testing::scratch_dir("clock-machine-transport");
    if token {
        std::fs::write(dir.join("current"), FIRST).unwrap();
    }
    // Use this process's non-loopback address for the unlisted-host leg;
    // loopback is deliberately always allowed by the shared host policy.
    let route = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
    route.connect("192.0.2.1:9").unwrap(); // route selection only: no datagram is sent
    let host = route.local_addr().unwrap().ip().to_string();
    assert!(!route.local_addr().unwrap().ip().is_loopback());
    let listener = TcpListener::bind("0.0.0.0:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base = format!("http://{host}:{}", listener.local_addr().unwrap().port());
    let server_mode = mode.to_owned();
    let server_dir = dir.clone();
    let server = std::thread::spawn(move || {
        let count = if matches!(server_mode.as_str(), "http-rotate" | "sse-rotate") {
            2
        } else {
            1
        };
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut requests = Vec::new();
        for index in 0..count {
            let mut socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "public clock transport did not connect"
                        );
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("accept: {error}"),
                }
            };
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut raw = Vec::new();
            let mut chunk = [0; 1024];
            while !raw.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                let n = socket.read(&mut chunk).unwrap();
                assert!(n > 0 && raw.len() < 16384);
                raw.extend_from_slice(&chunk[..n]);
            }
            requests.push(String::from_utf8(raw).unwrap().to_ascii_lowercase());
            if server_mode == "http-timeout" {
                std::thread::sleep(Duration::from_secs(5));
                continue;
            }
            let (status, body, extra) = match server_mode.as_str() {
                "http-error" => ("503 Unavailable", "unavailable".to_owned(), String::new()),
                "http-decode" => ("200 OK", "not-json".to_owned(), String::new()),
                "http-redirect" => (
                    "302 Found",
                    String::new(),
                    "Location: /unexpected\r\n".to_owned(),
                ),
                _ if server_mode.starts_with("sse") => (
                    "200 OK",
                    format!("data: {NOW}\n\n"),
                    "Content-Type: text/event-stream\r\n".to_owned(),
                ),
                _ => ("200 OK", NOW.to_owned(), String::new()),
            };
            let held_body = server_mode == "sse-rotate" && index == 0;
            write!(
                socket,
                "HTTP/1.1 {status}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n",
                body.len() + if held_body { 3 } else { 0 }
            )
            .unwrap();
            socket.flush().unwrap();
            if server_mode == "sse-delay" {
                // Headers arrive immediately; the body itself outlives the HTTP timeout.
                std::thread::sleep(Duration::from_millis(2300));
            }
            socket.write_all(body.as_bytes()).unwrap();
            socket.flush().unwrap();
            if index == 0 && matches!(server_mode.as_str(), "http-rotate" | "sse-rotate") {
                std::fs::write(server_dir.join("current"), SECOND).unwrap();
                if server_mode == "sse-rotate" {
                    // Keep the first connection alive past the source watcher interval.
                    std::thread::sleep(Duration::from_millis(5300));
                    socket.write_all(b":\n\n").unwrap();
                }
            }
        }
        requests
    });
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child_driver", "--nocapture"])
        .env("CLOCK_TRANSPORT_CHILD", mode)
        .env("CLOCK_TRANSPORT_URL", base)
        .env("BOSS_MACHINE_TOKEN_DIR", dir)
        .env(
            "BOSS_MACHINE_TOKEN_HOSTS",
            if listed {
                host.as_str()
            } else {
                "unrelated.invalid"
            },
        )
        .env_remove("HTTP_PROXY")
        .env_remove("http_proxy")
        .env_remove("HTTPS_PROXY")
        .env_remove("https_proxy")
        .env_remove("ALL_PROXY")
        .env_remove("all_proxy")
        .output()
        .unwrap();
    let requests = server.join().unwrap();
    assert!(
        output.status.success(),
        "public transport child failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    requests
}

fn header(request: &str) -> Option<&str> {
    request
        .lines()
        .find_map(|line| line.strip_prefix("x-boss-machine-token: "))
}

#[test]
fn http_carries_the_existing_machine_token() {
    let requests = exercise("http", true, true);
    assert!(requests[0].starts_with("get /api/clock/now "));
    assert_eq!(header(&requests[0]), Some(FIRST));
}

#[test]
fn sse_carries_the_existing_machine_token() {
    let requests = exercise("sse", true, true);
    assert!(requests[0].starts_with("get /api/clock/ticks "));
    assert_eq!(header(&requests[0]), Some(FIRST));
}

#[test]
fn both_transports_respect_destination_bounds_and_missing_mounts() {
    for mode in ["http", "sse"] {
        assert!(header(&exercise(mode, false, true)[0]).is_none());
        assert!(header(&exercise(mode, true, false)[0]).is_none());
    }
}

#[test]
fn http_rotation_reaches_an_existing_client() {
    let requests = exercise("http-rotate", true, true);
    assert_eq!(header(&requests[0]), Some(FIRST));
    assert_eq!(header(&requests[1]), Some(SECOND));
}

#[test]
fn sse_reconnect_refreshes_the_token_without_a_body_timeout() {
    let requests = exercise("sse-rotate", true, true);
    assert_eq!(header(&requests[0]), Some(FIRST));
    assert_eq!(header(&requests[1]), Some(SECOND));
    assert_eq!(requests.len(), 2);
}

#[test]
fn sse_body_remains_untimed() {
    assert_eq!(exercise("sse-delay", true, true).len(), 1);
}

#[test]
fn http_keeps_timeout_cache_status_decode_and_fallback_contracts() {
    for mode in [
        "http-timeout",
        "http-cache",
        "http-error",
        "http-decode",
        "http-fallback",
        "http-redirect",
    ] {
        assert_eq!(exercise(mode, true, true).len(), 1);
    }
}

#[tokio::test]
async fn child_driver() {
    let Ok(mode) = std::env::var("CLOCK_TRANSPORT_CHILD") else {
        return;
    };
    let url = std::env::var("CLOCK_TRANSPORT_URL").unwrap();
    if mode.starts_with("sse") {
        let stream = subscribe_ticks(url);
        futures::pin_mut!(stream);
        let at = Instant::now();
        for _ in 0..if mode == "sse-rotate" { 2 } else { 1 } {
            let tick = tokio::time::timeout(Duration::from_secs(15), stream.next())
                .await
                .unwrap()
                .unwrap();
            assert!(tick.simulated);
        }
        if mode == "sse-rotate" {
            assert!(at.elapsed() >= Duration::from_secs(7));
        }
    } else {
        let client = ReqwestClockClient::new(url);
        let client = if mode == "http-cache" {
            client
        } else {
            client.with_cache_ttl(Duration::ZERO)
        };
        let at = Instant::now();
        match mode.as_str() {
            "http-timeout" => {
                assert!(matches!(
                    client.try_now().await,
                    Err(ClockClientError::Transport(_))
                ));
                assert!(at.elapsed() >= Duration::from_millis(1800));
                assert!(at.elapsed() < Duration::from_secs(4));
            }
            "http-error" | "http-redirect" => {
                let expected = if mode == "http-error" { 503 } else { 302 };
                assert!(
                    matches!(client.try_now().await, Err(ClockClientError::Status { status, .. }) if status == expected)
                );
            }
            "http-decode" => {
                assert!(matches!(
                    client.try_now().await,
                    Err(ClockClientError::Decode(_))
                ));
            }
            "http-fallback" => {
                // The server closes after one successful response: the second read fails.
                assert!(client.try_now().await.unwrap().simulated);
                assert!(!client.now().await.simulated);
            }
            _ => {
                let first = client.try_now().await.unwrap();
                assert!(first.simulated);
                if mode == "http-cache" {
                    assert_eq!(client.try_now().await.unwrap().now, first.now);
                }
                if mode == "http-rotate" {
                    tokio::time::sleep(Duration::from_millis(5300)).await;
                    assert!(client.try_now().await.unwrap().simulated);
                }
            }
        }
    }
}
