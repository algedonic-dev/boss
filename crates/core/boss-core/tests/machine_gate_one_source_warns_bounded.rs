//! One source's spray logs a bounded number of WARN lines.
//!
//! **This file holds exactly one test, and that is the point.** It
//! asserts what the machine gate LOGGED, and a captured log is
//! deterministic only when the capturing test is alone in its process
//! with the subscriber installed globally before the first event
//! (`tests/common/mod.rs` carries the mechanism and the measurement;
//! `boss-testing/tests/a_log_capturing_test_owns_its_process.rs`
//! refuses any other shape). Written first beside the lib's own gate
//! tests, it read an empty log in the full run and a full one alone.
//!
//! Every new tally key is one WARN line — how a restart never loses the
//! fact that a caller missed — so before backlog 93bcf490 one LAN
//! caller varying `x-boss-user` wrote a line per key until the whole
//! tally was full. Now one source writes its share's worth of key lines
//! and ONE line naming it per presentation when the share fills,
//! however long it sends. A `previous` is never held to the share
//! (review ebc7b1cc, B1): it is keyed, with its own key line.

mod common;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::routing::get;
use boss_core::machine_gate::{
    MAX_KEYS_PER_SOURCE, MAX_TALLY_KEYS, MachineGate, Mode, Reading, Slots, gated,
};
use common::Captured;
use tower::ServiceExt;

#[tokio::test]
async fn one_sources_spray_warns_its_share_of_lines_and_names_itself_once() {
    let log = Captured::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(log.clone())
        .with_max_level(tracing::Level::WARN)
        .with_ansi(false)
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .expect("this binary holds one test, so nothing else has claimed the subscriber");

    let gate = Arc::new(MachineGate::new(
        "things",
        &[],
        Reading::new(
            Mode::Report,
            Slots::new(Some("cur".into()), None, Some("prev".into())),
        ),
    ));
    let app = gated(
        Router::new().route("/api/things/{id}", get(|| async { "read" })),
        Arc::clone(&gate),
    );
    let send = |user: String, token: Option<&'static str>| {
        let mut req = axum::http::Request::builder()
            .method("GET")
            .uri("/api/things/1")
            .header("x-boss-user", format!(r#"{{"id":"{user}"}}"#));
        if let Some(t) = token {
            req = req.header(boss_core::machine_token::HEADER, t);
        }
        let mut req = req.body(Body::empty()).unwrap();
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([10, 20, 0, 66], 40000))));
        app.clone().oneshot(req)
    };

    for i in 0..5000 {
        send(format!("spray-{i}"), None).await.unwrap();
    }
    let lines: Vec<String> = log.text().lines().map(str::to_string).collect();
    // Absolute, not only by the constant: a share the size of the whole
    // tally is the pre-fix gate, and would still equal its own `+ 1`.
    assert!(
        lines.len() <= MAX_TALLY_KEYS / 16 + 1,
        "one source logs at most a sixteenth of the tally's lines: {}",
        lines.len()
    );
    assert_eq!(
        lines.len(),
        MAX_KEYS_PER_SOURCE + 1,
        "a 5,000-user spray from one source: {} lines",
        lines.len()
    );
    let full: Vec<&String> = lines
        .iter()
        .filter(|l| l.contains("share of the tally"))
        .collect();
    assert_eq!(full.len(), 1, "{full:#?}");
    assert!(full[0].contains("10.20.0.66"), "{}", full[0]);
    assert!(
        lines.iter().all(|l| l.len() < 1024),
        "every line is bounded: {:?}",
        lines.iter().map(String::len).max()
    );

    // A token that matches no slot, past the share: a different fact
    // from `none`, so its own ONE line however many are sent.
    for i in 0..50 {
        send(format!("guess-{i}"), Some("a-guess")).await.unwrap();
    }
    let after: Vec<String> = log.text().lines().map(str::to_string).collect();
    let more = &after[lines.len()..];
    assert_eq!(more.len(), 1, "{more:#?}");
    assert!(
        more[0].contains("presenting mismatch") && more[0].contains("10.20.0.66"),
        "{}",
        more[0]
    );

    // A `previous` is never held to the share (review ebc7b1cc, B1): it
    // is keyed, so it is its own key line — one per caller, because only
    // a holder of the old secret can send it, and no sprayer can.
    send("still-on-the-old-value".to_string(), Some("prev"))
        .await
        .unwrap();
    let last: Vec<String> = log.text().lines().map(str::to_string).collect();
    let keyed = &last[after.len()..];
    assert_eq!(keyed.len(), 1, "{keyed:#?}");
    assert!(
        keyed[0].contains("still-on-the-old-value") && keyed[0].contains("Previous"),
        "{}",
        keyed[0]
    );
    let m = gate.misses();
    assert_eq!(m.overflow, 0, "{m:?}");
}
