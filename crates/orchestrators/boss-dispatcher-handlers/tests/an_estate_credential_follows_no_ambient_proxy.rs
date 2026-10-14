//! The credential broker's estate-host senders go where their URL says,
//! whatever proxy the process's environment names (backlog 6162aff2).
//!
//! **This file holds exactly one test, and that is the point.** It sets
//! the process's proxy variables, which every other test in a binary
//! would read too, and reqwest reads them when a client is BUILT — so the
//! variables are set before each client below, alone in its process.
//! (The same shape as boss-core's machine-client proxy pin, car d3fb7795.)
//!
//! WHAT WAS MEASURED (review bad26eb2, 2026-10-10, re-read on origin/main
//! 3bed869c7). `ForgejoAdmin` signs every request with the broker's forge
//! ADMIN ROOT token and was built with `reqwest::Client::new()`, which
//! honours `HTTP_PROXY`/`ALL_PROXY`; its production base is the estate
//! forge over PLAIN HTTP (`BOSS_BROKER_FORGE_URL`), so a proxy named in
//! the dispatcher's environment would have read the root token in
//! cleartext. `KubeSecretStore` carries the broker's ServiceAccount
//! bearer to the cluster API, an estate address no egress proxy has any
//! business relaying; its cluster CA is ADDED to the system roots, not
//! substituted for them, so TLS alone does not rule a reader out. Both
//! now build their client with `.no_proxy()`. The control shows what a
//! client that honours the environment does with the same header.

use std::io::{Read, Write};
use std::sync::mpsc::Receiver;
use std::time::Duration;

use boss_dispatcher_handlers::handlers::credential_issuer::{
    ForgeTokenIssuer, ForgejoAdmin, KubeSecretStore, SecretStore,
};

const ROOT: &str = "forge-admin-root-value";
const BEARER: &str = "cluster-bearer-value";

/// Accept one connection, record its request head (lowercased), answer
/// 200 with an empty JSON list. A listener nobody reaches leaves its
/// thread parked on `accept` until the process exits, which is the
/// test's own lifetime.
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
        let _ = sock.write_all(
            b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\nconnection: close\r\n\r\n[]",
        );
    });
    rx
}

/// What the origin and the proxy each saw of one request, sent by `send`
/// to the origin's base URL after the environment names the proxy. The
/// proxy is `127.0.0.2`, a different host from the origin's `127.0.0.1`
/// and loopback on every Linux network namespace.
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
    send(format!("http://{origin_at}"));
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

#[test]
fn the_brokers_estate_senders_follow_no_proxy_the_environment_names() {
    // The control: a plain client, built under the same environment and
    // handed the same header, goes to the proxy — so the fixture is one
    // reqwest reads, and the credential reaches whoever answers there.
    let (origin, proxy) = leg(|base| {
        block_on(async {
            reqwest::Client::new()
                .get(format!("{base}/api/v1/admin/users/david/tokens"))
                .header("authorization", format!("token {ROOT}"))
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
        proxy.is_some_and(|h| h.contains(ROOT)),
        "the control's proxy did not see the credential, so this fixture cannot catch a leak"
    );

    // The forge admin adapter, signing with the broker's root token.
    let (origin, proxy) = leg(|base| {
        let admin = ForgejoAdmin::new(base, ROOT).unwrap();
        block_on(async {
            let _ = admin.list_tokens("david").await;
        })
    });
    assert_eq!(
        proxy, None,
        "the forge admin adapter sent the root token to the proxy"
    );
    assert!(
        origin.is_some_and(|h| h.contains(ROOT)),
        "the forge did not receive the admin request directly"
    );

    // The cluster Secret store, signing with the ServiceAccount bearer.
    let (origin, proxy) = leg(|base| {
        let kube = KubeSecretStore::new(base, BEARER, None).unwrap();
        block_on(async {
            let _ = kube.read_key("boss", "some-secret", "token").await;
        })
    });
    assert_eq!(
        proxy, None,
        "the cluster Secret store sent its bearer to the proxy"
    );
    assert!(
        origin.is_some_and(|h| h.contains(BEARER)),
        "the cluster API did not receive the Secret read directly"
    );
}
