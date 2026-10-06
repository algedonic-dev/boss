//! The machine-token revoke judges the gate's overflow by WHAT
//! overflowed (backlog 93bcf490, from review 1829e95f finding 2).
//!
//! One LAN caller varying `x-boss-user` filled every key of a gate's
//! tally, and the self-issued rotation's drain held every revoke while
//! the tally's `overflow` was above zero — for as long as that one caller
//! kept sending. The gate now gives each source its own share and counts
//! what a source sends past it by what it presented; the drain holds on
//! a `previous` there and on an overflow no one source can be charged
//! with, and names a source's other noise on the revoke without holding.
//!
//! Every case here sends real requests through the REAL gate's router
//! and hands the tally it records to `judge_drain`, so what the drain
//! judges is what the gate records, not a model of either. It lives
//! under `tests/` because it presents the machine token by hand, which
//! `no_client_bakes_the_machine_token_in.rs` refuses in any file it reads
//! as production — and a module file under `src/` is one.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ConnectInfo;
use boss_core::machine_gate::{
    MAX_KEYS_PER_SOURCE, MachineGate, Misses, Mode, Reading, Slots, gated,
};
use boss_dispatcher_handlers::handlers::credential_rotate_self_issued::{GateRead, judge_drain};
use chrono::Utc;
use tower::ServiceExt;

type Request = ([u8; 4], String, Option<&'static str>);

/// The tally a gate named `service` records after `requests`, each sent
/// through its own router from `peer`, asserting `user`, presenting
/// `token` — recording since three days ago, so the window is watched.
async fn tally_after(
    service: &str,
    requests: impl IntoIterator<Item = Request>,
) -> (String, GateRead<Misses>) {
    let gate = Arc::new(MachineGate::starting_at(
        service,
        &[],
        Reading::new(
            Mode::Report,
            Slots::new(Some("cur".into()), None, Some("prev".into())),
        ),
        Utc::now() - chrono::Duration::days(3),
    ));
    let app = gated(
        axum::Router::new().route("/api/x", axum::routing::get(|| async { "ok" })),
        Arc::clone(&gate),
    );
    for (peer, user, token) in requests {
        let mut req = axum::http::Request::builder()
            .uri("/api/x")
            .header("x-boss-user", format!(r#"{{"id":"{user}"}}"#));
        if let Some(t) = token {
            req = req.header(boss_core::machine_token::HEADER, t);
        }
        let mut req = req.body(Body::empty()).unwrap();
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from((peer, 40000))));
        app.clone().oneshot(req).await.unwrap();
    }
    (service.to_string(), GateRead::Answered(gate.misses()))
}

/// The control port, with a tally that saw nothing.
async fn dispatcher() -> (String, GateRead<Misses>) {
    tally_after("dispatcher", []).await
}

const NOISY: [u8; 4] = [10, 20, 0, 66];

/// One LAN caller varying `x-boss-user`: 5,000 users from one address.
fn spray(peer: [u8; 4]) -> impl Iterator<Item = Request> {
    (0..5000).map(move |i| (peer, format!("spray-{i}"), None))
}

fn day() -> chrono::Duration {
    chrono::Duration::days(1)
}

/// None of one source's noise is `previous`, so none of it can hide a
/// caller still on the old value: the revoke goes ahead, and its record
/// names the source and how much it sent.
#[tokio::test]
async fn one_noisy_source_does_not_hold_the_revoke_and_is_named_on_it() {
    let jobs = tally_after("jobs", spray(NOISY)).await;
    let text = judge_drain(&[dispatcher().await, jobs], Utc::now(), day(), &[])
        .expect("one source's none cannot hide a previous");
    let past = 5000 - MAX_KEYS_PER_SOURCE;
    assert!(
        text.contains("10.20.0.66") && text.contains(&format!("{past} none")),
        "the noisy source is named on the revoke: {text}"
    );
}

/// The sprayer and a caller still on the old value share ONE address —
/// the shape of every in-pod caller, and of any LAN host with a noisy
/// neighbour process (review ebc7b1cc, B1). The lagging caller is keyed
/// by name however full its source's share is, so the revoke holds
/// naming it while it sends, and releases once it has been quiet for the
/// window. Counted past the share, it had no time: reproduced still held
/// two days later on a one-day window, saying it "still presents".
#[tokio::test]
async fn a_lagging_caller_sharing_the_sprayers_address_holds_then_ages_out() {
    let requests = spray(NOISY).chain([(NOISY, "a-real-caller".to_string(), Some("prev"))]);
    let jobs = tally_after("jobs", requests).await;
    let control = dispatcher().await;
    let held = judge_drain(&[control.clone(), jobs.clone()], Utc::now(), day(), &[])
        .expect_err("a caller still on previous holds");
    assert!(
        held.iter().any(|h| h.contains("10.20.0.66")
            && h.contains("a-real-caller")
            && h.contains("still presents previous")),
        "{held:?}"
    );
    let later = Utc::now() + chrono::Duration::days(2);
    let text = judge_drain(&[control, jobs], later, day(), &[])
        .expect("a caller quiet for the whole window no longer holds");
    assert!(
        text.contains("10.20.0.66"),
        "the source's noise is still named: {text}"
    );
}

/// The real caller behind the spray keeps its row, so the hold names it
/// by address and user — before, it was one more `overflow`.
#[tokio::test]
async fn a_real_caller_after_one_noisy_source_is_named_on_the_hold() {
    let requests = spray(NOISY).chain([(
        [10, 20, 0, 30],
        "automation:forge-converge".to_string(),
        Some("prev"),
    )]);
    let jobs = tally_after("jobs", requests).await;
    let held = judge_drain(&[dispatcher().await, jobs], Utc::now(), day(), &[])
        .expect_err("a caller still on previous holds");
    assert!(
        held.iter().any(|h| h.contains("10.20.0.30")
            && h.contains("automation:forge-converge")
            && h.contains("still presents previous")),
        "{held:?}"
    );
    assert!(
        !held.iter().any(|h| h.contains("overflowed")),
        "one noisy source is not an overflow: {held:?}"
    );
}

/// Many sources are not one noisy caller. Sixteen full shares are the
/// whole tally, what comes after is unattributed `overflow`, and the
/// drain holds on it exactly as before.
#[tokio::test]
async fn a_many_source_spray_still_holds_the_revoke_as_before() {
    let requests =
        (0..100).flat_map(|i| (0..64u8).map(move |s| ([10, 20, 1, s], format!("spray-{i}"), None)));
    let jobs = tally_after("jobs", requests).await;
    let held = judge_drain(&[dispatcher().await, jobs], Utc::now(), day(), &[])
        .expect_err("an overflow no one source can be charged with holds");
    assert!(held.iter().any(|h| h.contains("overflowed")), "{held:?}");
}
