use super::*;
use std::io::{Read, Write};

async fn synthetic_leaf(trust_override: bool) -> Result<Vec<u8>, Refused> {
    let pair = OpenSslIssuer.mint(Utc::now()).await.unwrap();
    let trust = if trust_override {
        OpenSslIssuer.mint(Utc::now()).await.unwrap().trust
    } else {
        pair.trust.clone()
    };
    let (address, server) = serve(&pair);
    // The injection is test-only. Production has only the fixed cluster target.
    let result = ServedLeaf.observe_test(&trust, address).await;
    server.join().unwrap();
    if !trust_override {
        assert_eq!(result.as_ref().unwrap(), pair.pem.as_bytes());
    }
    result
}
fn serve(pair: &Certificate) -> (std::net::SocketAddr, std::thread::JoinHandle<()>) {
    let mut builder =
        openssl::ssl::SslAcceptor::mozilla_intermediate_v5(openssl::ssl::SslMethod::tls()).unwrap();
    builder
        .set_private_key(&openssl::pkey::PKey::private_key_from_pem(pair.key.as_bytes()).unwrap())
        .unwrap();
    builder
        .set_certificate(&openssl::x509::X509::from_pem(pair.pem.as_bytes()).unwrap())
        .unwrap();
    let acceptor = builder.build();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(4);
        let socket = loop {
            match listener.accept() {
                Ok((s, _)) => break Some(s),
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        && std::time::Instant::now() < until =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(10))
                }
                _ => break None,
            }
        };
        let Some(socket) = socket else {
            return;
        };
        socket
            .set_read_timeout(Some(std::time::Duration::from_secs(3)))
            .unwrap();
        socket
            .set_write_timeout(Some(std::time::Duration::from_secs(3)))
            .unwrap();
        if let Ok(mut stream) = acceptor.accept(socket) {
            let mut request = [0; 4096];
            let _ = stream.read(&mut request);
            let _=stream.write_all(b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        }
    });
    (address, server)
}

#[tokio::test]
async fn a_real_tls_peer_returns_its_canonical_leaf_with_only_selected_trust() {
    assert!(synthetic_leaf(false).await.is_ok());
}

#[tokio::test]
async fn a_foreign_public_root_cannot_qualify_a_served_leaf() {
    assert!(synthetic_leaf(true).await.is_err());
}

#[tokio::test]
async fn a_trusted_peer_still_requires_the_fixed_hostname_and_current_certificate() {
    use openssl::{
        asn1::Asn1Time,
        hash::MessageDigest,
        pkey::PKey,
        x509::{X509, X509NameBuilder, extension::SubjectAlternativeName},
    };
    for (host, expired, accepted) in [
        (HOST, false, true),
        ("other.invalid", false, false),
        (HOST, true, false),
    ] {
        let mut pair = OpenSslIssuer.mint(Utc::now()).await.unwrap();
        let key = PKey::private_key_from_pem(pair.key.as_bytes()).unwrap();
        let mut name = X509NameBuilder::new().unwrap();
        name.append_entry_by_text("CN", host).unwrap();
        let name = name.build();
        let mut certificate = X509::builder().unwrap();
        certificate.set_version(2).unwrap();
        certificate
            .set_serial_number(
                &openssl::bn::BigNum::from_u32(17)
                    .unwrap()
                    .to_asn1_integer()
                    .unwrap(),
            )
            .unwrap();
        certificate.set_subject_name(&name).unwrap();
        certificate.set_issuer_name(&name).unwrap();
        certificate.set_pubkey(&key).unwrap();
        certificate
            .set_not_before(&Asn1Time::from_unix(Utc::now().timestamp() - 86400).unwrap())
            .unwrap();
        certificate
            .set_not_after(
                &Asn1Time::from_unix(Utc::now().timestamp() + if expired { -1 } else { 86400 })
                    .unwrap(),
            )
            .unwrap();
        let san = SubjectAlternativeName::new()
            .dns(host)
            .build(&certificate.x509v3_context(None, None))
            .unwrap();
        certificate.append_extension(san).unwrap();
        certificate.sign(&key, MessageDigest::sha256()).unwrap();
        pair.pem = String::from_utf8(certificate.build().to_pem().unwrap()).unwrap();
        pair.trust = pair.pem.clone();
        let (address, server) = serve(&pair);
        assert_eq!(
            ServedLeaf.observe_test(&pair.trust, address).await.is_ok(),
            accepted,
            "host {host}, expired {expired}"
        );
        server.join().unwrap();
    }
}

#[tokio::test]
#[ignore = "invoked in an isolated environment by the owning subprocess test"]
async fn isolated_tls_environment_child() {
    let trust =
        std::fs::read_to_string(std::env::var_os("BOSS_TLS_TEST_SELECTED_PUBLIC_CA").unwrap())
            .unwrap();
    let address = std::env::var("BOSS_TLS_TEST_ADDRESS")
        .unwrap()
        .parse()
        .unwrap();
    let expected = std::env::var("BOSS_TLS_TEST_EXPECT").unwrap() == "accepted";
    assert_eq!(
        ServedLeaf.observe_test(&trust, address).await.is_ok(),
        expected
    );
}

#[tokio::test]
async fn inherited_keylogging_ambient_ca_and_proxy_settings_cannot_change_the_tls_observer() {
    for foreign in [false, true] {
        let pair = OpenSslIssuer.mint(Utc::now()).await.unwrap();
        let selected = if foreign {
            OpenSslIssuer.mint(Utc::now()).await.unwrap().trust
        } else {
            pair.trust.clone()
        };
        let root = boss_testing::scratch_dir("tls-observer-isolated-environment");
        let ambient = root.join("ambient-public-ca.pem");
        let selected_path = root.join("selected-public-ca.pem");
        let keylog = root.join("must-not-write-session-keys");
        std::fs::write(&ambient, pair.trust.as_bytes()).unwrap();
        std::fs::write(&selected_path, selected.as_bytes()).unwrap();
        let (address, server) = serve(&pair);
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "handlers::managed_collector_tls::observer::tests::isolated_tls_environment_child",
                "--ignored",
                "--nocapture",
            ])
            .env("BOSS_TLS_TEST_SELECTED_PUBLIC_CA", &selected_path)
            .env("BOSS_TLS_TEST_ADDRESS", address.to_string())
            .env(
                "BOSS_TLS_TEST_EXPECT",
                if foreign { "refused" } else { "accepted" },
            )
            .env("SSLKEYLOGFILE", &keylog)
            .env("SSL_CERT_FILE", &ambient)
            .env("SSL_CERT_DIR", &root)
            .env("HTTPS_PROXY", "http://127.0.0.1:1")
            .env("ALL_PROXY", "http://127.0.0.1:1")
            .output()
            .unwrap();
        server.join().unwrap();
        std::fs::write(root.join("child.stdout"), &output.stdout).unwrap();
        std::fs::write(root.join("child.stderr"), &output.stderr).unwrap();
        assert!(
            output.status.success(),
            "isolated synthetic child failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !keylog.exists() || std::fs::metadata(keylog).unwrap().len() == 0,
            "the observer must not export session keys"
        );
    }
}

use super::evidence::LeafReader;
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Native {
    secret: Value,
    pod: Value,
    trust: Value,
}
#[async_trait]
impl NativeReads for Native {
    async fn read(&self, target: Target) -> Result<NativeReply, Refused> {
        let value = match &target {
            Target::Secret => &self.secret,
            Target::BrokerPod(name) if self.pod["metadata"]["name"] == *name => &self.pod,
            Target::PublicTrust => &self.trust,
            _ => return Err(Refused),
        };
        Ok(NativeReply {
            target,
            status: Some(200),
            headers: vec![("synthetic-original".into(), vec![255, 0, 1])],
            body: serde_json::to_vec(value).unwrap(),
            error: None,
        })
    }
}
struct FreshTrust(Arc<Native>);
#[async_trait]
impl CurrentTrustReader for FreshTrust {
    async fn observe(&self, invocation: uuid::Uuid) -> Result<CurrentTrustObservation, Refused> {
        Ok(CurrentTrustObservation {
            invocation,
            observation: uuid::Uuid::new_v4(),
            observed_at: Utc::now(),
            native: self.0.read(Target::PublicTrust).await?,
        })
    }
}
struct Leaf(Vec<u8>);
#[async_trait]
impl LeafReader for Leaf {
    async fn observe(&self, _: &str) -> TlsObservation {
        TlsObservation {
            started_at: Utc::now(),
            completed_at: Utc::now(),
            peer_der: openssl::x509::X509::from_pem(&self.0)
                .unwrap()
                .to_der()
                .unwrap(),
            peer_pem: self.0.clone(),
            error: None,
        }
    }
}
async fn observed_fixture() -> (
    ProtectedObserver,
    SecretData,
    uuid::Uuid,
    std::path::PathBuf,
) {
    observed_rotation_fixture(false).await
}
async fn observed_rotation_fixture(
    rotation: bool,
) -> (
    ProtectedObserver,
    SecretData,
    uuid::Uuid,
    std::path::PathBuf,
) {
    use base64::Engine;
    use rsa::sha2::{Digest, Sha256};
    let (store, observed, pod, root, generation, _) =
        super::super::tests::observer_fixture(rotation).await;
    let selected = slot(&observed, "next")
        .unwrap()
        .or(slot(&observed, "current").unwrap())
        .unwrap();
    let original: installation::Original =
        serde_json::from_slice(&std::fs::read(root.join("tls-installation.json")).unwrap())
            .unwrap();
    let trust = json!({"kind":"ConfigMap","metadata":{"namespace":"boss-dev","name":"boss-managed-cache-public-trust","uid":uuid::Uuid::new_v4(),"resourceVersion":"trust:1"},"data":{"ca.crt":selected.trust}});
    store
        .retain_trust(installation::TrustPlan {
            installation: original.installation,
            generation,
            public_trust_sha256: format!("{:x}", Sha256::digest(selected.trust.as_bytes())),
            original: trust.clone(),
        })
        .await
        .unwrap();
    let data = observed
        .data
        .iter()
        .map(|(key, value)| {
            (
                key.clone(),
                json!(base64::engine::general_purpose::STANDARD.encode(value)),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    let native = Arc::new(Native {
        secret: json!({"kind":"Secret","metadata":{"namespace":NAMESPACE,"name":SECRET,"uid":observed.uid,"resourceVersion":observed.version},"data":data}),
        pod,
        trust,
    });
    let mut observer = ProtectedObserver::new(
        store,
        native.clone(),
        Some(Arc::new(FreshTrust(native))),
        None,
    );
    observer.leaf = Arc::new(Leaf(selected.pem.into_bytes()));
    (observer, observed, generation, root)
}

#[tokio::test]
async fn fresh_independent_native_trust_and_tls_observations_can_qualify_initial_promotion() {
    let (observer, observed, generation, root) = observed_fixture().await;
    let selected = slot(&observed, "next").unwrap().unwrap();
    let effect = observer
        .observe(&observed, generation, &selected.pem)
        .await
        .unwrap();
    assert_eq!(effect.secret_uid, observed.uid);
    assert_eq!(effect.secret_version, observed.version);
    assert_eq!(effect.installed_trust_generation, generation);
    assert!(!effect.old_collectors_absent);
    assert!(effect.retired_generation.is_none());
    let records = std::fs::read_dir(&root)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("tls-observation-")
        })
        .count();
    assert!(
        records >= 7,
        "whole originals and final manifest must be retained"
    );
}

#[tokio::test]
async fn the_production_peer_adapter_conserves_actual_der_and_full_error() {
    let pair = OpenSslIssuer.mint(Utc::now()).await.unwrap();
    let pem = pair.pem.into_bytes();
    let wanted = openssl::x509::X509::from_pem(&pem)
        .unwrap()
        .to_der()
        .unwrap();
    let observation = TlsObservation::from_peer(Utc::now(), Ok(pem.clone()));
    assert_eq!(observation.peer_der, wanted);
    assert_eq!(observation.peer_pem, pem);
    let diagnostic =
        "the whole synthetic TLS diagnostic, including the first failure and final error";
    assert_eq!(
        TlsObservation::from_peer(Utc::now(), Err(diagnostic.into()))
            .error
            .as_deref(),
        Some(diagnostic)
    );
}

#[tokio::test]
async fn a_current_trust_producer_performs_a_fresh_named_read_instead_of_replaying_a_plan() {
    let (observer, _, _, _) = observed_fixture().await;
    let reader = HttpCurrentTrust::new(observer.native.clone());
    let invocation = uuid::Uuid::new_v4();
    let first = reader.observe(invocation).await.unwrap();
    let second = reader.observe(invocation).await.unwrap();
    assert_eq!(first.invocation, invocation);
    assert_ne!(first.observation, second.observation);
    assert_eq!(first.native.body, second.native.body);
    assert!(matches!(first.native.target, Target::PublicTrust));
}

#[tokio::test]
async fn native_http_retains_raw_body_status_and_headers_without_following_redirects() {
    use axum::{Router, response::IntoResponse};
    let raw = vec![0, 255, 1, 2, 3, 10];
    for status in [200_u16, 302, 403, 404] {
        let response = raw.clone();
        let app = Router::new().fallback(move |request: axum::extract::Request| {
            let response = response.clone();
            async move {
                assert_eq!(request.method(), axum::http::Method::GET);
                assert_eq!(
                    request.uri().path(),
                    "/api/v1/namespaces/boss/secrets/boss-managed-cache-server-tls"
                );
                assert_eq!(
                    request.headers()["authorization"],
                    "Bearer synthetic-observer"
                );
                (
                    axum::http::StatusCode::from_u16(status).unwrap(),
                    [("location", "/must-not-follow"), ("x-original", "whole")],
                    response,
                )
                    .into_response()
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let public = OpenSslIssuer.mint(Utc::now()).await.unwrap().trust;
        let reader = installation::HttpPodReader::new(
            reqwest::Certificate::from_pem(public.as_bytes()).unwrap(),
            "synthetic-observer".into(),
        )
        .unwrap();
        let original = reader.read_at(Target::Secret, &base).await.unwrap();
        assert_eq!(original.status, Some(status));
        assert_eq!(original.body, raw);
        assert!(original.error.is_none());
        assert!(
            original
                .headers
                .iter()
                .any(|(name, value)| name == "x-original" && value == b"whole")
        );
        server.abort();
    }
}

struct CorruptNative {
    inner: Arc<dyn NativeReads>,
    fault: usize,
    calls: AtomicUsize,
}
#[async_trait]
impl NativeReads for CorruptNative {
    async fn read(&self, target: Target) -> Result<NativeReply, Refused> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let mut reply = self.inner.read(target.clone()).await?;
        let mut body: Value = serde_json::from_slice(&reply.body).unwrap();
        match self.fault {
            0 if matches!(target, Target::Secret) => {
                body["metadata"]["uid"] = json!(uuid::Uuid::new_v4())
            }
            1 if matches!(target, Target::Secret) => {
                body["metadata"]["resourceVersion"] = json!("changed")
            }
            2 if matches!(target, Target::Secret) => body["kind"] = json!("Status"),
            3 if matches!(target, Target::Secret) => {
                reply.body.clear();
                return Ok(reply);
            }
            4 => reply.status = Some(403),
            5 => reply.status = Some(302),
            6 if matches!(target, Target::Secret) => body["data"] = json!({"tls.next":"foreign"}),
            7 if matches!(target, Target::BrokerPod(_)) => {
                body["metadata"]["uid"] = json!(uuid::Uuid::new_v4())
            }
            8 if matches!(target, Target::BrokerPod(_)) => {
                body["status"]["containerStatuses"][0]["restartCount"] = json!(1)
            }
            9 if matches!(target, Target::BrokerPod(_)) => {
                body["spec"]["containers"][0]["volumeMounts"][0]["readOnly"] = json!(false)
            }
            10 if matches!(target, Target::BrokerPod(_)) => {
                body["spec"]["containers"][0]["volumeMounts"][0]["subPath"] = json!("part")
            }
            11 if matches!(target, Target::BrokerPod(_)) => reply.target = Target::Secret,
            12 if call >= 3 && matches!(target, Target::Secret) => {
                body["metadata"]["resourceVersion"] = json!("changed-after-tls")
            }
            13 if call >= 2 && matches!(target, Target::BrokerPod(_)) => {
                body["status"]["containerStatuses"][0]["containerID"] =
                    json!("replacement-after-tls")
            }
            _ => {}
        }
        reply.body = serde_json::to_vec(&body).unwrap();
        Ok(reply)
    }
}

#[tokio::test]
async fn every_native_identity_target_permission_and_recheck_control_refuses() {
    for fault in 0..14 {
        let (mut observer, observed, generation, root) = observed_fixture().await;
        observer.native = Arc::new(CorruptNative {
            inner: observer.native.clone(),
            fault,
            calls: AtomicUsize::new(0),
        });
        let selected = slot(&observed, "next").unwrap().unwrap();
        assert!(
            observer
                .observe(&observed, generation, &selected.pem)
                .await
                .is_err(),
            "fault {fault}"
        );
        let manifests = std::fs::read_dir(root)
            .unwrap()
            .filter_map(Result::ok)
            .filter_map(|entry| {
                serde_json::from_slice::<Value>(&std::fs::read(entry.path()).ok()?).ok()
            })
            .filter(|value| value.get("qualified").is_some())
            .collect::<Vec<_>>();
        assert_eq!(manifests.len(), 1);
        assert_eq!(manifests[0]["qualified"], false);
    }
}

struct CorruptTrust {
    inner: Arc<dyn CurrentTrustReader>,
    fault: usize,
}
#[async_trait]
impl CurrentTrustReader for CorruptTrust {
    async fn observe(&self, invocation: uuid::Uuid) -> Result<CurrentTrustObservation, Refused> {
        let mut current = self.inner.observe(invocation).await?;
        match self.fault {
            0 => current.invocation = uuid::Uuid::new_v4(),
            1 => current.observed_at -= chrono::Duration::seconds(61),
            2 => current.observed_at += chrono::Duration::seconds(61),
            3 => current.observation = uuid::Uuid::nil(),
            4 => current.observation = uuid::Uuid::from_u128(33),
            5 => current.native.status = Some(403),
            6 => current.native.body.clear(),
            7 => current.native.target = Target::Secret,
            8 | 9 => {
                let mut value: Value = serde_json::from_slice(&current.native.body).unwrap();
                if self.fault == 8 {
                    value["metadata"]["uid"] = json!(uuid::Uuid::new_v4());
                } else {
                    value["data"]["ca.crt"] = json!("replaced trust");
                }
                current.native.body = serde_json::to_vec(&value).unwrap();
            }
            _ => return Err(Refused),
        }
        Ok(current)
    }
}

#[tokio::test]
async fn desired_trust_originals_missing_authority_and_replayed_current_observations_are_not_proof()
{
    for fault in 0..11 {
        let (mut observer, observed, generation, _) = observed_fixture().await;
        observer.trust = if fault == 10 {
            None
        } else {
            Some(Arc::new(CorruptTrust {
                inner: observer.trust.clone().unwrap(),
                fault,
            }))
        };
        let selected = slot(&observed, "next").unwrap().unwrap();
        assert!(
            observer
                .observe(&observed, generation, &selected.pem)
                .await
                .is_err(),
            "trust fault {fault}"
        );
    }
}

#[tokio::test]
async fn matching_reobservations_retain_new_originals_without_mutating_lifecycle_state() {
    let (observer, observed, generation, root) = observed_fixture().await;
    let installation = std::fs::read(root.join("tls-installation.json")).unwrap();
    let selected = slot(&observed, "next").unwrap().unwrap();
    for _ in 0..2 {
        assert!(
            observer
                .observe(&observed, generation, &selected.pem)
                .await
                .is_ok()
        );
    }
    assert_eq!(
        std::fs::read(root.join("tls-installation.json")).unwrap(),
        installation
    );
    assert_eq!(
        observer.store.read_secret(NAMESPACE, SECRET).await.unwrap(),
        Some(observed)
    );
    let manifests = std::fs::read_dir(root)
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            serde_json::from_slice::<Value>(&std::fs::read(entry.path()).ok()?).ok()
        })
        .filter(|value| value.get("qualified").is_some())
        .collect::<Vec<_>>();
    assert_eq!(manifests.len(), 2);
    assert_ne!(manifests[0]["invocation"], manifests[1]["invocation"]);
}

struct Census {
    original: Value,
}
#[async_trait]
impl RegisteredCensus for Census {
    async fn observe(
        &self,
        invocation: uuid::Uuid,
        _: uuid::Uuid,
    ) -> Result<CensusObservation, Refused> {
        Ok(CensusObservation {
            invocation,
            observed_at: Utc::now(),
            original: self.original.clone(),
        })
    }
}
struct AbsentCollectors {
    inner: Arc<dyn NativeReads>,
}
#[async_trait]
impl NativeReads for AbsentCollectors {
    async fn read(&self, target: Target) -> Result<NativeReply, Refused> {
        let (name, resource, group) = match &target {
            Target::CollectorJob(name) => (name, "jobs", "batch"),
            Target::CollectorPod(name) => (name, "pods", ""),
            _ => return self.inner.read(target).await,
        };
        let mut original = json!({"kind":"Status","apiVersion":"v1","status":"Failure","reason":"NotFound","code":404,"details":{"name":name,"kind":resource,"group":group}});
        // Kubernetes omits an empty core API group in StatusDetails.
        if group.is_empty() {
            original["details"].as_object_mut().unwrap().remove("group");
        }
        let body = serde_json::to_vec(&original).unwrap();
        Ok(NativeReply {
            target,
            status: Some(404),
            headers: vec![],
            body,
            error: None,
        })
    }
}
async fn retirement_fixture() -> (
    ProtectedObserver,
    SecretData,
    uuid::Uuid,
    std::path::PathBuf,
    Value,
) {
    let (mut observer, observed, generation, root) = observed_rotation_fixture(true).await;
    let original: installation::Original =
        serde_json::from_slice(&std::fs::read(root.join("tls-installation.json")).unwrap())
            .unwrap();
    let previous = slot(&observed, "previous").unwrap().unwrap();
    let closed = Utc::now() - chrono::Duration::seconds(650);
    let job_uid = uuid::Uuid::new_v4();
    let pod_uid = uuid::Uuid::new_v4();
    let spec = json!({"audiences":["boss-managed-cache-collection"],"expirationSeconds":600,"boundObjectRef":{"apiVersion":"v1","kind":"Pod","name":"collector-original-pod","uid":pod_uid}});
    let request = json!({"apiVersion":"authentication.k8s.io/v1","kind":"TokenRequest","metadata":{"namespace":"boss-dev","name":"boss-managed-cache-collector"},"spec":spec});
    let mut response = request.clone();
    response["status"] = json!({"token":"synthetic-retained-token","expirationTimestamp":(closed+chrono::Duration::seconds(590)).to_rfc3339()});
    let census = json!({"schema":1,"installation":original.installation,"generation":previous.generation,"producer_pod_uid":original.pod["metadata"]["uid"],"revision":uuid::Uuid::new_v4(),"closed_at":closed,"unresolved_operations":[],"collectors":[{
        "job":{"apiVersion":"batch/v1","kind":"Job","metadata":{"namespace":"boss-dev","name":"collector-original-job","uid":job_uid}},
        "pod":{"apiVersion":"v1","kind":"Pod","metadata":{"namespace":"boss-dev","name":"collector-original-pod","uid":pod_uid,"ownerReferences":[{"apiVersion":"batch/v1","kind":"Job","name":"collector-original-job","uid":job_uid,"controller":true}]},"spec":{"serviceAccountName":"boss-managed-cache-collector"}},
        "tokens":[{"issued_at":closed,"request":request,"response":response}]
    }]});
    observer.native = Arc::new(AbsentCollectors {
        inner: observer.native.clone(),
    });
    observer.census = Some(Arc::new(Census {
        original: census.clone(),
    }));
    (observer, observed, generation, root, census)
}

#[tokio::test]
async fn a_sealed_protected_generation_census_and_exact_original_absences_can_qualify_retirement() {
    let (observer, observed, generation, _, census) = retirement_fixture().await;
    let current = slot(&observed, "current").unwrap().unwrap();
    let previous = slot(&observed, "previous").unwrap().unwrap();
    let effect = observer
        .observe(&observed, generation, &current.pem)
        .await
        .unwrap();
    assert_eq!(effect.retired_generation, Some(previous.generation));
    assert!(effect.old_collectors_absent);
    let closed = DateTime::parse_from_rfc3339(census["closed_at"].as_str().unwrap())
        .unwrap()
        .with_timezone(&Utc);
    assert_eq!(
        effect.last_old_token_expiry,
        Some(closed + chrono::Duration::seconds(600))
    );
    assert!(effect.last_old_token_expiry.unwrap() < Utc::now());
}

struct FaultyCensus {
    original: Value,
    fault: usize,
    calls: AtomicUsize,
}
#[async_trait]
impl RegisteredCensus for FaultyCensus {
    async fn observe(
        &self,
        invocation: uuid::Uuid,
        generation: uuid::Uuid,
    ) -> Result<CensusObservation, Refused> {
        let mut observation = Census {
            original: self.original.clone(),
        }
        .observe(invocation, generation)
        .await?;
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let value = &mut observation.original;
        match self.fault {
            0 => value["installation"] = json!(uuid::Uuid::new_v4()),
            1 => value["generation"] = json!(uuid::Uuid::new_v4()),
            2 => value["producer_pod_uid"] = json!(uuid::Uuid::new_v4()),
            3 => value["revision"] = json!(uuid::Uuid::nil()),
            4 => value["closed_at"] = json!(Utc::now() + chrono::Duration::seconds(1)),
            5 => value["unresolved_operations"] = json!([{"create":"unacknowledged"}]),
            6 => value["caller_complete"] = json!(true),
            7 => {
                value["collectors"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("pod");
            }
            8 => value["collectors"][0]["pod"]["metadata"]["uid"] = json!(uuid::Uuid::new_v4()),
            9 => {
                value["collectors"][0]["pod"]["metadata"]["ownerReferences"][0]["uid"] =
                    json!(uuid::Uuid::new_v4())
            }
            10 => value["collectors"][0]["pod"]["spec"]["serviceAccountName"] = json!("unrelated"),
            11 => {
                value["collectors"][0]["tokens"][0]["response"]["status"]
                    .as_object_mut()
                    .unwrap()
                    .remove("expirationTimestamp");
            }
            12 => {
                value["collectors"][0]["tokens"][0]["response"]["status"]["expirationTimestamp"] =
                    json!(Utc::now() + chrono::Duration::seconds(900))
            }
            13 => value["collectors"][0]["tokens"][0]["issued_at"] = json!(Utc::now()),
            14 => {
                value["collectors"][0]["tokens"][0]["request"]["spec"]["audiences"] =
                    json!(["foreign"])
            }
            15 => {
                value["collectors"][0]["tokens"][0]["request"]["spec"]["boundObjectRef"]["kind"] =
                    json!("Job")
            }
            16 => {
                value["collectors"][0]["tokens"][0]["response"]["spec"]["expirationSeconds"] =
                    json!(3600)
            }
            17 => value["collectors"][0]["tokens"] = json!([]),
            18 => {
                let duplicate = value["collectors"][0].clone();
                value["collectors"].as_array_mut().unwrap().push(duplicate);
            }
            19 if call > 0 => value["revision"] = json!(uuid::Uuid::new_v4()),
            20 => observation.invocation = uuid::Uuid::new_v4(),
            21 => observation.observed_at -= chrono::Duration::seconds(61),
            22 => observation.observed_at += chrono::Duration::seconds(61),
            23 => return Err(Refused),
            24 => value["schema"] = json!(0),
            25 => value["collectors"][0]["job"]["metadata"]["name"] = json!("../outside"),
            26 => value["collectors"][0]["tokens"][0]["response"]["status"]["token"] = json!(""),
            27 => {
                value["collectors"][0]["tokens"][0]["response"]["status"]["expirationTimestamp"] =
                    json!(Utc::now() - chrono::Duration::seconds(900))
            }
            28 => value["closed_at"] = json!(Utc::now() - chrono::Duration::seconds(1)),
            _ => {}
        }
        Ok(observation)
    }
}

#[tokio::test]
async fn generation_closure_identity_completeness_replay_and_every_token_binding_are_required() {
    for fault in 0..30 {
        let (mut observer, observed, generation, _, original) = retirement_fixture().await;
        observer.census = if fault == 29 {
            None
        } else {
            Some(Arc::new(FaultyCensus {
                original,
                fault,
                calls: AtomicUsize::new(0),
            }))
        };
        let current = slot(&observed, "current").unwrap().unwrap();
        assert!(
            observer
                .observe(&observed, generation, &current.pem)
                .await
                .is_err(),
            "census fault {fault}"
        );
    }
}

struct FaultyAbsence {
    inner: Arc<dyn NativeReads>,
    fault: usize,
    calls: AtomicUsize,
}
#[async_trait]
impl NativeReads for FaultyAbsence {
    async fn read(&self, target: Target) -> Result<NativeReply, Refused> {
        let mut reply = self.inner.read(target.clone()).await?;
        if !matches!(target, Target::CollectorJob(_) | Target::CollectorPod(_)) {
            return Ok(reply);
        }
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let mut value: Value = serde_json::from_slice(&reply.body).unwrap();
        match self.fault {
            0 => reply.status = Some(403),
            1 => reply.status = Some(200),
            2 => value["reason"] = json!("Forbidden"),
            3 => value["details"]["name"] = json!("wrong-original"),
            4 => value["details"]["kind"] = json!("namespaces"),
            5 => value["details"]["group"] = json!("foreign"),
            6 => value["code"] = json!(403),
            7 => {
                reply.body.clear();
                return Ok(reply);
            }
            8 => reply.error = Some("whole transport failure after partial input".into()),
            9 => reply.target = Target::Secret,
            10 if call >= 2 => reply.status = Some(200),
            11 => value["kind"] = json!("Pod"),
            _ => {}
        }
        reply.body = serde_json::to_vec(&value).unwrap();
        Ok(reply)
    }
}
#[tokio::test]
async fn only_two_exact_original_not_found_reads_are_absence_never_forbidden_empty_or_replacement()
{
    for fault in 0..12 {
        let (mut observer, observed, generation, _, _) = retirement_fixture().await;
        observer.native = Arc::new(FaultyAbsence {
            inner: observer.native.clone(),
            fault,
            calls: AtomicUsize::new(0),
        });
        let current = slot(&observed, "current").unwrap().unwrap();
        assert!(
            observer
                .observe(&observed, generation, &current.pem)
                .await
                .is_err(),
            "absence fault {fault}"
        );
    }
}

struct ChangesDuringCensus {
    inner: Arc<dyn NativeReads>,
    collector_reads: AtomicUsize,
}
#[async_trait]
impl NativeReads for ChangesDuringCensus {
    async fn read(&self, target: Target) -> Result<NativeReply, Refused> {
        let mut reply = self.inner.read(target.clone()).await?;
        if matches!(target, Target::CollectorJob(_) | Target::CollectorPod(_)) {
            self.collector_reads.fetch_add(1, Ordering::SeqCst);
        }
        if matches!(target, Target::Secret) && self.collector_reads.load(Ordering::SeqCst) > 0 {
            let mut value: Value = serde_json::from_slice(&reply.body).unwrap();
            value["metadata"]["resourceVersion"] = json!("changed-during-census");
            reply.body = serde_json::to_vec(&value).unwrap();
        }
        Ok(reply)
    }
}
#[tokio::test]
async fn lifecycle_state_is_rechecked_after_the_retirement_census() {
    let (mut observer, observed, generation, _, _) = retirement_fixture().await;
    observer.native = Arc::new(ChangesDuringCensus {
        inner: observer.native.clone(),
        collector_reads: AtomicUsize::new(0),
    });
    let current = slot(&observed, "current").unwrap().unwrap();
    assert!(
        observer
            .observe(&observed, generation, &current.pem)
            .await
            .is_err(),
        "a census cannot extend an earlier Secret read indefinitely"
    );
}

#[tokio::test]
async fn a_protected_sealed_empty_generation_has_a_conservative_expiry_bound() {
    let (mut observer, observed, generation, _, mut original) = retirement_fixture().await;
    original["collectors"] = json!([]);
    observer.census = Some(Arc::new(Census { original }));
    let current = slot(&observed, "current").unwrap().unwrap();
    assert!(
        observer
            .observe(&observed, generation, &current.pem)
            .await
            .unwrap()
            .old_collectors_absent
    );
}

#[tokio::test]
async fn a_refusal_manifest_names_the_measured_stage_and_original_installation() {
    let (mut observer, observed, generation, root) = observed_fixture().await;
    observer.trust = None;
    let selected = slot(&observed, "next").unwrap().unwrap();
    assert!(
        observer
            .observe(&observed, generation, &selected.pem)
            .await
            .is_err()
    );
    let manifest = std::fs::read_dir(&root)
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            serde_json::from_slice::<Value>(&std::fs::read(entry.path()).ok()?).ok()
        })
        .find(|value| value.get("qualified").is_some())
        .unwrap();
    let original: Value =
        serde_json::from_slice(&std::fs::read(root.join("tls-installation.json")).unwrap())
            .unwrap();
    assert_eq!(manifest["installation"], original["installation"]);
    assert_eq!(manifest["failure_stage"], "current-trust-before");
    assert_eq!(manifest["qualified"], false);
}

struct FaultyLeaf {
    pem: Vec<u8>,
    fault: usize,
}
#[async_trait]
impl LeafReader for FaultyLeaf {
    async fn observe(&self, trust: &str) -> TlsObservation {
        let mut leaf = Leaf(self.pem.clone()).observe(trust).await;
        match self.fault {
            0 => leaf.error = Some("whole synthetic TLS refusal".into()),
            1 => leaf.peer_pem = b"foreign served leaf".to_vec(),
            2 => leaf.peer_der = vec![1, 2, 3],
            3 => leaf.started_at -= chrono::Duration::seconds(61),
            4 => leaf.completed_at += chrono::Duration::seconds(61),
            5 => leaf.completed_at = leaf.started_at - chrono::Duration::seconds(1),
            _ => {}
        }
        leaf
    }
}
#[tokio::test]
async fn actual_leaf_bytes_der_errors_and_own_time_window_are_all_required() {
    for fault in 0..6 {
        let (mut observer, observed, generation, _) = observed_fixture().await;
        let selected = slot(&observed, "next").unwrap().unwrap();
        observer.leaf = Arc::new(FaultyLeaf {
            pem: selected.pem.as_bytes().to_vec(),
            fault,
        });
        assert!(
            observer
                .observe(&observed, generation, &selected.pem)
                .await
                .is_err(),
            "leaf fault {fault}"
        );
    }
}

struct JumpingClock {
    delta: i64,
    calls: AtomicUsize,
}
impl evidence::Clock for JumpingClock {
    fn now(&self) -> DateTime<Utc> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        Utc::now() + chrono::Duration::seconds(if call == 0 { 0 } else { self.delta })
    }
}
#[tokio::test]
async fn the_observers_own_clock_refuses_backwards_or_overlong_windows() {
    for delta in [-61, 61] {
        let (mut observer, observed, generation, root) = observed_fixture().await;
        observer.clock = Arc::new(JumpingClock {
            delta,
            calls: AtomicUsize::new(0),
        });
        let selected = slot(&observed, "next").unwrap().unwrap();
        assert!(
            observer
                .observe(&observed, generation, &selected.pem)
                .await
                .is_err()
        );
        let manifest = std::fs::read_dir(root)
            .unwrap()
            .filter_map(Result::ok)
            .filter_map(|entry| {
                serde_json::from_slice::<Value>(&std::fs::read(entry.path()).ok()?).ok()
            })
            .find(|value| value.get("qualified").is_some())
            .unwrap();
        assert_eq!(manifest["failure_stage"], "observation-clock-window");
    }
}

#[tokio::test]
async fn fixed_native_paths_refuse_traversal_and_conserve_bounded_incomplete_originals() {
    use axum::{Router, response::IntoResponse};
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    let app = Router::new().fallback(move |request: axum::extract::Request| {
        counted.fetch_add(1, Ordering::SeqCst);
        async move {
            let path = request.uri().path();
            assert!(matches!(
                path,
                "/api/v1/namespaces/boss/pods/original-broker"
                    | "/api/v1/namespaces/boss-dev/configmaps/boss-managed-cache-public-trust"
                    | "/apis/batch/v1/namespaces/boss-dev/jobs/original-job"
                    | "/api/v1/namespaces/boss-dev/pods/original-pod"
            ));
            (axum::http::StatusCode::OK, vec![b'x'; 128 * 1024 + 10]).into_response()
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let public = OpenSslIssuer.mint(Utc::now()).await.unwrap().trust;
    let reader = installation::HttpPodReader::new(
        reqwest::Certificate::from_pem(public.as_bytes()).unwrap(),
        "synthetic-observer".into(),
    )
    .unwrap();
    for target in [
        Target::BrokerPod("original-broker".into()),
        Target::PublicTrust,
        Target::CollectorJob("original-job".into()),
        Target::CollectorPod("original-pod".into()),
    ] {
        let original = reader.read_at(target, &base).await.unwrap();
        assert_eq!(original.status, Some(200));
        assert_eq!(original.body.len(), 128 * 1024 + 1);
        assert!(original.error.as_ref().unwrap().contains("incomplete"));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    for target in [
        Target::BrokerPod("../outside".into()),
        Target::CollectorJob("a?b".into()),
        Target::CollectorPod("a/b".into()),
    ] {
        assert!(reader.read_at(target, &base).await.is_err());
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        4,
        "invalid names must refuse before any HTTP request"
    );
    server.abort();
}
