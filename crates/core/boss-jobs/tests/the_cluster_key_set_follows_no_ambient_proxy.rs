//! The in-pod credential verifier dials the cluster API server where its
//! URL says, whatever proxy the process's environment names (backlog
//! bf5f21b2).
//!
//! **This file holds exactly one test, and that is the point.** It sets
//! the process's proxy variables, which every other test in a binary
//! would read too, and reqwest reads them when a client is BUILT — so the
//! variables are set before each client below, alone in its process.
//! (The same shape as boss-core's machine-client proxy pin and the
//! dispatcher's estate-credential one.)
//!
//! WHAT WAS MEASURED (builder run fcc8c58b, 2026-10-18, re-read on
//! origin/main 0566d48a by run 585ce961). `ClusterKeySet` presents the
//! pod's OWN ServiceAccount token to fetch the issuer's key set, and built
//! its pinned-CA, https-only client with no `.no_proxy()`, so reqwest
//! honoured `HTTPS_PROXY`/`ALL_PROXY` and sent its CONNECT to the proxy.
//! With the six variables pointed at a dark loopback port, two of its unit
//! tests in `credentials::broker_stage` hung past 60 s each: their fake
//! API server sat on `accept` for a dial that went to the proxy instead.
//! In a pod with a proxy exported, every stage verification would have
//! answered `KeysUnavailable` — and a proxy that answers learns which
//! cluster the pod asks, and holds the tunnel its token rides in. A
//! connection to the in-cluster API has no business crossing one. The
//! control shows what a client that honours the environment does with the
//! same URL.

use std::io::Read;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use boss_jobs::credentials::broker_stage::{ClusterKeySet, StageKeys};

const POD_TOKEN: &str = "pod-token-SENTINEL-never-through-a-proxy";

/// Accept one connection, record the first bytes it sends (a TLS
/// ClientHello from a client that came direct, a `CONNECT` head from one
/// that came as a proxy's client), then close it, which fails the client
/// at once rather than leaving it to time out. A listener nobody reaches
/// leaves its thread parked on `accept` until the process exits, which is
/// the test's own lifetime.
fn hear_once(listener: std::net::TcpListener) -> Receiver<Vec<u8>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let Ok((mut sock, _)) = listener.accept() else {
            return;
        };
        let _ = sock.set_read_timeout(Some(Duration::from_secs(5)));
        let mut heard = vec![0u8; 16 * 1024];
        let n = sock.read(&mut heard).unwrap_or(0);
        heard.truncate(n);
        let _ = tx.send(heard);
    });
    rx
}

/// What the origin and the proxy each heard of one fetch, made by `send`
/// to the origin's https URL after the environment names the proxy. The
/// proxy is `127.0.0.2`, a different host from the origin's `127.0.0.1`
/// and loopback on every Linux network namespace.
fn leg(send: impl FnOnce(String)) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
    let origin = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let origin_at = origin.local_addr().unwrap();
    let origin_heard = hear_once(origin);
    let proxy = std::net::TcpListener::bind("127.0.0.2:0").unwrap();
    let proxy_url = format!("http://{}", proxy.local_addr().unwrap());
    let proxy_heard = hear_once(proxy);
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
    send(format!("https://{origin_at}/openid/v1/jwks"));
    let wait = Duration::from_millis(500);
    (
        origin_heard.recv_timeout(wait).ok(),
        proxy_heard.recv_timeout(wait).ok(),
    )
}

#[test]
fn the_cluster_key_set_follows_no_proxy_the_environment_names() {
    // The control: a plain https-only client, built under the same
    // environment, tunnels through the proxy — so the fixture is one
    // reqwest reads.
    let (origin, proxy) = leg(|url| {
        let refused = reqwest::blocking::Client::builder()
            .https_only(true)
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap()
            .get(url)
            .send();
        assert!(refused.is_err(), "neither listener answers a request");
    });
    assert_eq!(
        origin, None,
        "the control went direct: the fixture proxies nothing"
    );
    assert!(
        proxy.is_some_and(|h| String::from_utf8_lossy(&h).starts_with("CONNECT 127.0.0.1:")),
        "the control's proxy heard no CONNECT, so this fixture cannot catch a proxied fetch"
    );

    // The verifier itself: its pod files in a scratch directory, the
    // origin a peer the trust root never certified, so the fetch fails at
    // the handshake — after the dial this test is about.
    let dir = boss_testing::scratch_dir("cluster-key-set-no-proxy");
    let (token_file, ca_file) = (dir.join("token"), dir.join("ca.crt"));
    boss_testing::write_file(&token_file, POD_TOKEN);
    boss_testing::write_file(
        &ca_file,
        include_str!("../src/credentials/broker_stage/test-root.pem"),
    );
    let mut why = String::new();
    let (origin, proxy) = leg(|url| {
        why = ClusterKeySet {
            url,
            token_file,
            ca_file,
        }
        .trusted()
        .err()
        .expect("an uncertified peer cannot supply trusted signing keys");
    });
    assert!(!why.contains(POD_TOKEN), "{why}");
    assert_eq!(
        proxy.map(|h| String::from_utf8_lossy(&h).into_owned()),
        None,
        "the cluster key-set fetch dialled the proxy; client refusal: {why}"
    );
    let origin = origin.unwrap_or_else(|| panic!("the origin was never dialled: {why}"));
    assert_eq!(
        origin.get(..2),
        Some(&[22u8, 3][..]),
        "the fetch reached the origin direct and spoke TLS first: {why}"
    );
    let origin = String::from_utf8_lossy(&origin).to_ascii_lowercase();
    assert!(
        !origin.contains(&POD_TOKEN.to_ascii_lowercase()) && !origin.contains("authorization"),
        "a bearer crossed before the handshake verified"
    );
}
