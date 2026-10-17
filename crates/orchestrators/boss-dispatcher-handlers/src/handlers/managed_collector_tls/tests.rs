use super::*;
#[tokio::test]
async fn missing_active_slots_refuse_every_later_original_representation() {
    for birth in ["1", "opaque:original-version"] {
        for case in 0..6 {
            let (bound, store, issuer, _, _, request, now) = file_fixture(birth).await;
            let first = prepare(&bound, &issuer, request, now).await.unwrap();
            assert_eq!(prepare(&bound, &issuer, request, now).await.unwrap(), first);
            assert_eq!(issuer.0.load(Ordering::SeqCst), 1);
            {
                let mut native = store.0.lock().unwrap();
                for value in native.data.values_mut() {
                    value.clear();
                }
                match case {
                    0 => native.data.clear(),
                    1 => {}
                    2 => {
                        native.data.insert("tls.current".into(), String::new());
                        native.data.insert("tls.previous".into(), String::new());
                    }
                    3 => {
                        native.data.clear();
                        native.data.insert("tls.effect".into(), "{}".into());
                        native
                            .data
                            .insert("tls.promoted-at".into(), now.to_rfc3339());
                    }
                    4 => {
                        native.data.insert("tls.retired".into(), "[]".into());
                    }
                    _ => {
                        native
                            .data
                            .insert("unknown".into(), "retained metadata".into());
                    }
                }
                native.version.push('1');
            }
            let lost = store.0.lock().unwrap().clone();
            for attempted in [request, uuid::Uuid::new_v4()] {
                let result = prepare(&bound, &issuer, attempted, now).await;
                assert!(result.is_err(), "case {case}, birth {birth}: {result:?}");
                assert_eq!(issuer.0.load(Ordering::SeqCst), 1);
                assert_eq!(*store.0.lock().unwrap(), lost);
            }
        }
    }
}
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

#[tokio::test]
async fn an_empty_replacement_cannot_restart_an_original_request() {
    let (store, issuer, request, now) = fixture();
    prepare(&store, &issuer, request, now).await.unwrap();
    {
        let mut value = store.0.lock().unwrap();
        value.uid = uuid::Uuid::new_v4().to_string();
        value.version = "1".into();
        value.data.clear();
    }
    assert!(prepare(&store, &issuer, request, now).await.is_err());
    assert_eq!(issuer.0.load(Ordering::SeqCst), 1);
}
#[derive(Default)]
struct StoreFault {
    lost_ack: bool,
    replacement: bool,
    moved: bool,
    reads: usize,
}
struct Store(Mutex<SecretData>, Mutex<StoreFault>, Mutex<Binding>);
struct Binding {
    uid: String,
    requests: std::collections::BTreeSet<uuid::Uuid>,
}
#[async_trait]
impl BoundStore for Store {
    async fn original_uid(&self) -> Result<String, Refused> {
        Ok(self.2.lock().unwrap().uid.clone())
    }
    async fn reserve(&self, request: uuid::Uuid) -> Result<bool, Refused> {
        Ok(self.2.lock().unwrap().requests.insert(request))
    }
    async fn reserved(&self, request: uuid::Uuid) -> Result<bool, Refused> {
        Ok(self.2.lock().unwrap().requests.contains(&request))
    }
    async fn attest_effect(&self, _: &Effect) -> Result<(), Refused> {
        Ok(())
    }
}
#[async_trait]
impl SecretStore for Store {
    async fn read_key(&self, _: &str, _: &str, _: &str) -> Result<Option<String>, String> {
        panic!("whole object only")
    }
    async fn write_key(&self, _: &str, _: &str, _: &str, _: &str) -> Result<(), String> {
        panic!("CAS only")
    }
    async fn read_secret(&self, ns: &str, name: &str) -> Result<Option<SecretData>, String> {
        assert_eq!((ns, name), (NAMESPACE, SECRET));
        self.1.lock().unwrap().reads += 1;
        Ok(Some(self.0.lock().unwrap().clone()))
    }
    async fn write_keys_if(
        &self,
        ns: &str,
        name: &str,
        entries: &[(&str, &str)],
        observed: &SecretData,
    ) -> Result<WriteAt, String> {
        assert_eq!((ns, name), (NAMESPACE, SECRET));
        let mut value = self.0.lock().unwrap();
        let mut fault = self.1.lock().unwrap();
        if fault.moved {
            fault.moved = false;
            value.version.push('2');
            return Ok(WriteAt::Moved);
        }
        if value.uid != observed.uid || value.version != observed.version {
            return Ok(WriteAt::Moved);
        }
        for (key, entry) in entries {
            value.data.insert((*key).into(), (*entry).into());
        }
        value.version.push('1');
        if fault.replacement {
            fault.replacement = false;
            value.uid = uuid::Uuid::new_v4().to_string();
        }
        if fault.lost_ack {
            fault.lost_ack = false;
            return Err("synthetic lost acknowledgement".into());
        }
        Ok(WriteAt::Written)
    }
}
struct FakeIssuer(AtomicUsize);
#[async_trait]
impl Issuer for FakeIssuer {
    async fn mint(&self, now: DateTime<Utc>) -> Result<Certificate, Refused> {
        let n = self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Certificate {
            key: format!("synthetic-private-key-{n}"),
            pem: format!("synthetic-public-certificate-{n}"),
            trust: format!("synthetic-public-trust-{n}"),
            expires: now + chrono::Duration::seconds(CERTIFICATE_SECONDS),
        })
    }
    fn validate(&self, c: &Certificate, _: DateTime<Utc>) -> Result<(), Refused> {
        if c.key.strip_prefix("synthetic-private-key-")
            == c.pem.strip_prefix("synthetic-public-certificate-")
            && c.key.starts_with("synthetic-private-key-")
        {
            Ok(())
        } else {
            Err(Refused)
        }
    }
}
#[derive(Clone, Copy)]
enum FaultEffect {
    None,
    Trust,
    TrustUid,
    Retired,
    Uid,
    Version,
    Generation,
    Leaf,
    Stale,
    Future,
}
struct Installer {
    ready: bool,
    old_absent: bool,
    last_expiry: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    fault: FaultEffect,
}
#[async_trait]
impl ProtectedInstallation for Installer {
    fn completed_at(&self) -> DateTime<Utc> {
        self.now
    }
    async fn observe(
        &self,
        s: &SecretData,
        generation: uuid::Uuid,
        public: &str,
    ) -> Result<Effect, Refused> {
        use rsa::sha2::{Digest, Sha256};
        if !self.ready {
            return Err(Refused);
        }
        let selected = slot(s, "next")
            .unwrap()
            .or_else(|| slot(s, "current").unwrap())
            .unwrap();
        let mut value = Effect {
            secret_uid: s.uid.clone(),
            secret_version: s.version.clone(),
            generation,
            certificate_sha256: format!("{:x}", Sha256::digest(public.as_bytes())),
            observed_at: self.now,
            installed_trust_generation: generation,
            public_trust_sha256: format!("{:x}", Sha256::digest(selected.trust.as_bytes())),
            public_trust_uid: uuid::Uuid::from_u128(7).to_string(),
            retired_generation: slot(s, "previous").unwrap().map(|p| p.generation),
            old_collectors_absent: self.old_absent,
            last_old_token_expiry: self.last_expiry,
        };
        match self.fault {
            FaultEffect::None => {}
            FaultEffect::Trust => value.public_trust_sha256 = "foreign trust".into(),
            FaultEffect::TrustUid => value.public_trust_uid = uuid::Uuid::new_v4().to_string(),
            FaultEffect::Retired => value.retired_generation = Some(uuid::Uuid::new_v4()),
            FaultEffect::Uid => value.secret_uid = uuid::Uuid::new_v4().to_string(),
            FaultEffect::Version => value.secret_version.push('2'),
            FaultEffect::Generation => value.installed_trust_generation = uuid::Uuid::new_v4(),
            FaultEffect::Leaf => value.certificate_sha256 = "foreign leaf".into(),
            FaultEffect::Stale => value.observed_at -= chrono::Duration::seconds(61),
            FaultEffect::Future => value.observed_at += chrono::Duration::seconds(1),
        }
        Ok(value)
    }
}
#[tokio::test]
async fn rotation_requires_served_trust_and_drained_previous_before_revocation() {
    let (store, issuer, first, now) = fixture();
    prepare(&store, &issuer, first, now).await.unwrap();
    let mut install = Installer {
        ready: false,
        old_absent: false,
        last_expiry: None,
        now,
        fault: FaultEffect::None,
    };
    assert!(
        promote(&store, &issuer, &install, first, now)
            .await
            .is_err()
    );
    install.ready = true;
    assert_eq!(
        promote(&store, &issuer, &install, first, now)
            .await
            .unwrap(),
        Progress::Promoted
    );
    let second = uuid::Uuid::new_v4();
    prepare(&store, &issuer, second, now).await.unwrap();
    assert_eq!(
        promote(&store, &issuer, &install, second, now)
            .await
            .unwrap(),
        Progress::Promoted
    );
    assert!(
        revoke(&store, &issuer, &install, second, now)
            .await
            .is_err()
    );
    let later = now + chrono::Duration::seconds(OVERLAP_SECONDS + 1);
    install.now = later;
    install.old_absent = true;
    assert!(
        revoke(&store, &issuer, &install, second, later)
            .await
            .is_err(),
        "no expiry witness is not a pass"
    );
    install.last_expiry = Some(later + chrono::Duration::seconds(1));
    assert!(
        revoke(&store, &issuer, &install, second, later)
            .await
            .is_err()
    );
    install.last_expiry = Some(now);
    assert_eq!(
        revoke(&store, &issuer, &install, second, later)
            .await
            .unwrap(),
        Progress::Revoked
    );
    assert_eq!(
        revoke(&store, &issuer, &install, second, later)
            .await
            .unwrap(),
        Progress::Revoked
    );
}
#[tokio::test]
async fn promotion_refuses_an_asserted_generation_with_foreign_trust_or_native_identity() {
    let (store, issuer, request, now) = fixture();
    prepare(&store, &issuer, request, now).await.unwrap();
    let mut install = Installer {
        ready: true,
        old_absent: true,
        last_expiry: Some(now),
        now,
        fault: FaultEffect::None,
    };
    for fault in [
        FaultEffect::Trust,
        FaultEffect::Uid,
        FaultEffect::Version,
        FaultEffect::Generation,
        FaultEffect::Leaf,
        FaultEffect::Stale,
        FaultEffect::Future,
    ] {
        install.fault = fault;
        assert!(
            promote(&store, &issuer, &install, request, now)
                .await
                .is_err()
        );
        assert!(slot(&store.0.lock().unwrap(), "current").unwrap().is_none());
    }
    install.fault = FaultEffect::None;
    assert_eq!(
        promote(&store, &issuer, &install, request, now)
            .await
            .unwrap(),
        Progress::Promoted
    );
}
#[tokio::test]
async fn another_previous_generation_cannot_qualify_revocation() {
    let (store, issuer, request, now) = fixture();
    let mut install = Installer {
        ready: true,
        old_absent: true,
        last_expiry: Some(now),
        now,
        fault: FaultEffect::None,
    };
    prepare(&store, &issuer, request, now).await.unwrap();
    promote(&store, &issuer, &install, request, now)
        .await
        .unwrap();
    let second = uuid::Uuid::new_v4();
    prepare(&store, &issuer, second, now).await.unwrap();
    promote(&store, &issuer, &install, second, now)
        .await
        .unwrap();
    let later = now + chrono::Duration::seconds(OVERLAP_SECONDS + 1);
    install.now = later;
    install.fault = FaultEffect::Retired;
    assert!(
        revoke(&store, &issuer, &install, second, later)
            .await
            .is_err()
    );
    assert!(
        slot(&store.0.lock().unwrap(), "previous")
            .unwrap()
            .is_some()
    );
    install.fault = FaultEffect::None;
    assert_eq!(
        revoke(&store, &issuer, &install, second, later)
            .await
            .unwrap(),
        Progress::Revoked
    );
}
fn fixture() -> (Store, FakeIssuer, uuid::Uuid, DateTime<Utc>) {
    let uid = uuid::Uuid::new_v4().to_string();
    (
        Store(
            Mutex::new(SecretData {
                uid: uid.clone(),
                version: "1".into(),
                data: Default::default(),
            }),
            Mutex::new(StoreFault::default()),
            Mutex::new(Binding {
                uid,
                requests: Default::default(),
            }),
        ),
        FakeIssuer(AtomicUsize::new(0)),
        uuid::Uuid::new_v4(),
        Utc::now(),
    )
}
#[tokio::test]
async fn lost_staging_acknowledgement_replays_the_committed_original() {
    let (store, issuer, request, now) = fixture();
    store.1.lock().unwrap().lost_ack = true;
    assert!(prepare(&store, &issuer, request, now).await.is_err());
    let original = store.0.lock().unwrap().clone();
    assert!(matches!(
        prepare(&store, &issuer, request, now).await.unwrap(),
        Progress::Staged { .. }
    ));
    assert_eq!(issuer.0.load(Ordering::SeqCst), 1);
    assert_eq!(store.0.lock().unwrap().data, original.data);
}
#[tokio::test]
async fn conditional_movement_and_object_replacement_never_return_staged_success() {
    let (store, issuer, request, now) = fixture();
    store.1.lock().unwrap().moved = true;
    assert_eq!(
        prepare(&store, &issuer, request, now).await.unwrap(),
        Progress::Moved
    );
    assert!(store.0.lock().unwrap().data.is_empty());
    store.1.lock().unwrap().replacement = true;
    assert!(prepare(&store, &issuer, request, now).await.is_err());
}
#[tokio::test]
async fn malformed_unknown_and_foreign_slots_refuse_before_minting() {
    for data in [
        std::collections::BTreeMap::from([(
            "tls.next".into(),
            "partial synthetic-private-canary".into(),
        )]),
        std::collections::BTreeMap::from([("unknown".into(), "synthetic-private-canary".into())]),
    ] {
        let (store, issuer, request, now) = fixture();
        store.0.lock().unwrap().data = data;
        assert!(prepare(&store, &issuer, request, now).await.is_err());
        assert_eq!(issuer.0.load(Ordering::SeqCst), 0);
    }
}
#[tokio::test]
async fn a_retired_generation_request_cannot_mint_another_identity() {
    let (store, issuer, first, now) = fixture();
    let mut install = Installer {
        ready: true,
        old_absent: true,
        last_expiry: Some(now),
        now,
        fault: FaultEffect::None,
    };
    prepare(&store, &issuer, first, now).await.unwrap();
    promote(&store, &issuer, &install, first, now)
        .await
        .unwrap();
    let second = uuid::Uuid::new_v4();
    prepare(&store, &issuer, second, now).await.unwrap();
    promote(&store, &issuer, &install, second, now)
        .await
        .unwrap();
    let later = now + chrono::Duration::seconds(OVERLAP_SECONDS + 1);
    install.now = later;
    revoke(&store, &issuer, &install, second, later)
        .await
        .unwrap();
    assert!(prepare(&store, &issuer, first, later).await.is_err());
    assert_eq!(issuer.0.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn preparation_conserves_one_private_pair_and_replays_without_reminting() {
    let (store, issuer, request, now) = fixture();
    let first = prepare(&store, &issuer, request, now).await.unwrap();
    assert!(matches!(first, Progress::Staged { .. }));
    assert_eq!(prepare(&store, &issuer, request, now).await.unwrap(), first);
    assert_eq!(issuer.0.load(Ordering::SeqCst), 1);
    assert!(!format!("{first:?}").contains("synthetic-private-key"));
    assert!(
        prepare(&store, &issuer, uuid::Uuid::new_v4(), now)
            .await
            .is_err()
    );
    assert_eq!(issuer.0.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn real_issuer_proves_fixed_hostname_pair_trust_expiry_and_private_redaction() {
    let now = Utc::now();
    let issuer = OpenSslIssuer;
    let first = issuer.mint(now).await.unwrap();
    let second = issuer.mint(now).await.unwrap();
    issuer.validate(&first, now).unwrap();
    assert_ne!(first.pem, second.pem);
    assert_ne!(first.trust, second.trust);
    assert!(!format!("{first:?}").contains(&first.key));
    let wrong_key = Certificate {
        key: second.key.clone(),
        pem: first.pem.clone(),
        trust: first.trust.clone(),
        expires: first.expires,
    };
    assert!(issuer.validate(&wrong_key, now).is_err());
    let wrong_trust = Certificate {
        key: first.key.clone(),
        pem: first.pem.clone(),
        trust: second.trust.clone(),
        expires: first.expires,
    };
    assert!(issuer.validate(&wrong_trust, now).is_err());
    assert!(
        issuer
            .validate(&first, first.expires + chrono::Duration::seconds(1))
            .is_err()
    );
    let malformed = Certificate {
        key: "synthetic-private-canary".into(),
        pem: "malformed".into(),
        trust: first.trust.clone(),
        expires: first.expires,
    };
    assert!(issuer.validate(&malformed, now).is_err());
    let leaf = openssl::x509::X509::from_pem(first.pem.as_bytes()).unwrap();
    let names = leaf.subject_alt_names().unwrap();
    assert_eq!(names.len(), 1);
    assert_eq!(names[0].dnsname(), Some(HOST));
}
struct NativeTiming(Installer);
#[async_trait]
impl ProtectedInstallation for NativeTiming {
    fn completed_at(&self) -> DateTime<Utc> {
        Utc::now()
    }
    async fn observe(&self, s: &SecretData, g: uuid::Uuid, p: &str) -> Result<Effect, Refused> {
        let mut effect = self.0.observe(s, g, p).await?;
        effect.observed_at = Utc::now();
        Ok(effect)
    }
}
#[tokio::test]
async fn a_genuine_observation_after_invocation_start_can_qualify_initial_installation() {
    let (store, issuer, request, now) = fixture();
    prepare(&store, &issuer, request, now).await.unwrap();
    let install = NativeTiming(Installer {
        ready: true,
        old_absent: true,
        last_expiry: Some(now),
        now,
        fault: FaultEffect::None,
    });
    assert_eq!(
        promote(&store, &issuer, &install, request, now)
            .await
            .unwrap(),
        Progress::Promoted
    );
}
#[tokio::test]
async fn the_issued_pair_serves_real_tls_only_under_its_own_public_trust() {
    use std::io::{Read, Write};
    let issuer = OpenSslIssuer;
    let pair = issuer.mint(Utc::now()).await.unwrap();
    let other = issuer.mint(Utc::now()).await.unwrap();
    for (trust, accepted) in [(&pair.trust, true), (&other.trust, false)] {
        let key = openssl::pkey::PKey::private_key_from_pem(pair.key.as_bytes()).unwrap();
        let certificate = openssl::x509::X509::from_pem(pair.pem.as_bytes()).unwrap();
        let mut builder =
            openssl::ssl::SslAcceptor::mozilla_intermediate_v5(openssl::ssl::SslMethod::tls())
                .unwrap();
        builder.set_private_key(&key).unwrap();
        builder.set_certificate(&certificate).unwrap();
        builder.check_private_key().unwrap();
        let acceptor = builder.build();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let Ok(mut stream) = acceptor.accept(socket) else {
                return false;
            };
            let mut request = [0_u8; 4096];
            if stream.read(&mut request).is_err() {
                return false;
            }
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .is_ok()
        });
        let client = reqwest::Client::builder()
            .no_proxy()
            .tls_built_in_root_certs(false)
            .add_root_certificate(reqwest::Certificate::from_pem(trust.as_bytes()).unwrap())
            .resolve(HOST, address)
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();
        let result = client
            .get(format!("https://{HOST}:{}/", address.port()))
            .send()
            .await;
        assert_eq!(
            result.is_ok(),
            accepted,
            "TLS request with expected acceptance {accepted} failed: {:?}",
            result.as_ref().err()
        );
        if let Ok(response) = result {
            assert_eq!(response.status(), reqwest::StatusCode::OK);
        }
        assert_eq!(server.join().unwrap(), accepted);
    }
}
#[test]
fn overlap_outlasts_the_authoritative_collector_token_lifetime() {
    let root = boss_testing::repo_root();
    let contract: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root.join("infra/agent/private-collector-contract.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(OVERLAP_SECONDS, contract["token_seconds"].as_i64().unwrap());
    assert_eq!(HOST, contract["host"].as_str().unwrap());
}

#[tokio::test]
async fn a_copied_slot_keeps_its_original_uid_on_every_retry() {
    let (store, issuer, request, now) = fixture();
    store.1.lock().unwrap().replacement = true;
    assert!(prepare(&store, &issuer, request, now).await.is_err());
    for _ in 0..2 {
        assert!(
            prepare(&store, &issuer, request, now).await.is_err(),
            "a copied slot under a replacement UID is not the original"
        );
    }
    assert_eq!(issuer.0.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn copied_current_and_previous_slots_refuse_all_lifecycle_entries() {
    for phase in 0..2 {
        let (store, issuer, request, now) = fixture();
        let install = Installer {
            ready: true,
            old_absent: true,
            last_expiry: Some(now),
            now,
            fault: FaultEffect::None,
        };
        prepare(&store, &issuer, request, now).await.unwrap();
        promote(&store, &issuer, &install, request, now)
            .await
            .unwrap();
        let selected = if phase == 1 {
            let second = uuid::Uuid::new_v4();
            prepare(&store, &issuer, second, now).await.unwrap();
            promote(&store, &issuer, &install, second, now)
                .await
                .unwrap();
            second
        } else {
            request
        };
        store.0.lock().unwrap().uid = uuid::Uuid::new_v4().to_string();
        let copied = store.0.lock().unwrap().clone();
        assert!(prepare(&store, &issuer, selected, now).await.is_err());
        assert!(
            promote(&store, &issuer, &install, selected, now)
                .await
                .is_err()
        );
        assert!(
            revoke(&store, &issuer, &install, selected, now)
                .await
                .is_err()
        );
        assert_eq!(*store.0.lock().unwrap(), copied);
    }
}

#[tokio::test]
async fn lost_promotion_and_revocation_acknowledgements_keep_the_same_object_identity() {
    let (store, issuer, first, now) = fixture();
    let mut install = Installer {
        ready: true,
        old_absent: true,
        last_expiry: Some(now),
        now,
        fault: FaultEffect::None,
    };
    prepare(&store, &issuer, first, now).await.unwrap();
    store.1.lock().unwrap().lost_ack = true;
    assert!(
        promote(&store, &issuer, &install, first, now)
            .await
            .is_err()
    );
    assert_eq!(
        promote(&store, &issuer, &install, first, now)
            .await
            .unwrap(),
        Progress::Promoted
    );
    let second = uuid::Uuid::new_v4();
    prepare(&store, &issuer, second, now).await.unwrap();
    promote(&store, &issuer, &install, second, now)
        .await
        .unwrap();
    let later = now + chrono::Duration::seconds(OVERLAP_SECONDS + 1);
    install.now = later;
    store.1.lock().unwrap().lost_ack = true;
    assert!(
        revoke(&store, &issuer, &install, second, later)
            .await
            .is_err()
    );
    assert_eq!(
        revoke(&store, &issuer, &install, second, later)
            .await
            .unwrap(),
        Progress::Revoked
    );
    assert_eq!(issuer.0.load(Ordering::SeqCst), 2);
}

struct NativePod(Mutex<serde_json::Value>);
#[async_trait]
impl installation::PodReader for NativePod {
    async fn read(&self, name: &str) -> Result<serde_json::Value, Refused> {
        let pod = self.0.lock().unwrap().clone();
        assert_eq!(pod["metadata"]["name"], name);
        Ok(pod)
    }
}
async fn file_fixture(
    initial_version: &str,
) -> (
    installation::FileBoundStore,
    std::sync::Arc<Store>,
    FakeIssuer,
    std::sync::Arc<NativePod>,
    std::path::PathBuf,
    uuid::Uuid,
    DateTime<Utc>,
) {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let (store, issuer, request, now) = fixture();
    store.0.lock().unwrap().version = initial_version.into();
    let store = std::sync::Arc::new(store);
    let pod = serde_json::json!({
        "kind":"Pod", "metadata":{"namespace":NAMESPACE,"name":"broker-fixture","uid":uuid::Uuid::new_v4().to_string()},
        "spec":{"serviceAccountName":"boss","volumes":[{"name":"tls","secret":{"secretName":SECRET}}],
            "containers":[{"name":"boss","image":"synthetic@sha256:abc","volumeMounts":[{"name":"tls","mountPath":"/private-tls","readOnly":true}]}]},
        "status":{"phase":"Running","conditions":[{"type":"Ready","status":"True"}],"containerStatuses":[{"name":"boss","ready":true,"restartCount":0,"containerID":"synthetic://original",
            "imageID":"synthetic@sha256:abc","state":{"running":{"startedAt":now.to_rfc3339()}}}]}
    });
    let reader = std::sync::Arc::new(NativePod(Mutex::new(pod.clone())));
    let root = boss_testing::scratch_dir("private-tls-original-binding");
    // mode-bits-ok: search permissions on the confined fixture directory, never an executable file.
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let owner = std::fs::metadata(&root).unwrap().uid();
    let bound =
        installation::FileBoundStore::open(&root, owner, store.clone(), reader.clone()).unwrap();
    let original = installation::Original {
        installation: uuid::Uuid::new_v4(),
        secret_creation: serde_json::json!({"kind":"Secret","metadata":{"namespace":NAMESPACE,"name":SECRET,"uid":store.0.lock().unwrap().uid,"resourceVersion":initial_version},"data":{}}),
        pod,
        container: "boss".into(),
    };
    bound.bind(original).await.unwrap();
    (bound, store, issuer, reader, root, request, now)
}
/// The observer owns its behavior tests; reuse the original-bound store fixture.
pub(super) async fn observer_fixture(
    rotation: bool,
) -> (
    std::sync::Arc<installation::FileBoundStore>,
    SecretData,
    serde_json::Value,
    std::path::PathBuf,
    uuid::Uuid,
    DateTime<Utc>,
) {
    let (bound, store, _, pod, root, request, now) = file_fixture("birth:original").await;
    let mint_at = if rotation {
        now - chrono::Duration::seconds(701)
    } else {
        now
    };
    prepare(&bound, &OpenSslIssuer, request, mint_at)
        .await
        .unwrap();
    let request = if rotation {
        {
            let mut value = store.0.lock().unwrap();
            let first = value.data["tls.next"].clone();
            value.data.insert("tls.current".into(), first);
            value.data.insert("tls.next".into(), String::new());
            value.version = "rotation:current".into();
        }
        let next = uuid::Uuid::new_v4();
        prepare(&bound, &OpenSslIssuer, next, mint_at)
            .await
            .unwrap();
        {
            let mut value = store.0.lock().unwrap();
            let old = value.data["tls.current"].clone();
            let new = value.data["tls.next"].clone();
            value.data.insert("tls.previous".into(), old);
            value.data.insert("tls.current".into(), new);
            value.data.insert("tls.next".into(), String::new());
            value
                .data
                .insert("tls.promoted-at".into(), mint_at.to_rfc3339());
            value.version = "rotation:promoted".into();
        }
        next
    } else {
        request
    };
    let observed = store.0.lock().unwrap().clone();
    let native_pod = pod.0.lock().unwrap().clone();
    (
        std::sync::Arc::new(bound),
        observed,
        native_pod,
        root,
        request,
        now,
    )
}
#[tokio::test]
async fn file_binding_refuses_empty_replacement_and_same_uid_state_loss() {
    for replace in [false, true] {
        let (bound, store, issuer, _, _, request, now) = file_fixture("1").await;
        let first = prepare(&bound, &issuer, request, now).await.unwrap();
        assert_eq!(prepare(&bound, &issuer, request, now).await.unwrap(), first);
        {
            let mut value = store.0.lock().unwrap();
            value.data.clear();
            if replace {
                value.uid = uuid::Uuid::new_v4().to_string();
            }
        }
        assert!(prepare(&bound, &issuer, request, now).await.is_err());
        assert_eq!(issuer.0.load(Ordering::SeqCst), 1);
    }
}
#[tokio::test]
async fn native_workload_and_projection_changes_refuse_before_secret_read() {
    for case in 0..8 {
        let (bound, store, issuer, reader, _, request, now) = file_fixture("1").await;
        let reads = store.1.lock().unwrap().reads;
        {
            let mut pod = reader.0.lock().unwrap();
            match case {
                0 => pod["metadata"]["uid"] = serde_json::json!(uuid::Uuid::new_v4()),
                1 => {
                    pod["status"]["containerStatuses"][0]["containerID"] =
                        serde_json::json!("synthetic://replacement")
                }
                2 => pod["status"]["containerStatuses"][0]["restartCount"] = serde_json::json!(1),
                3 => {
                    pod["spec"]["containers"][0]["volumeMounts"][0]["readOnly"] =
                        serde_json::json!(false)
                }
                4 => {
                    pod["spec"]["containers"][0]["volumeMounts"][0]["subPath"] =
                        serde_json::json!("tls.current")
                }
                5 => {
                    pod["spec"]["volumes"][0]["secret"]["secretName"] = serde_json::json!("foreign")
                }
                6 => pod["metadata"]["deletionTimestamp"] = serde_json::json!(now.to_rfc3339()),
                _ => {
                    pod["status"]["containerStatuses"][0]["imageID"] =
                        serde_json::json!("synthetic@sha256:replacement")
                }
            }
        }
        assert!(
            prepare(&bound, &issuer, request, now).await.is_err(),
            "case {case}"
        );
        assert_eq!(issuer.0.load(Ordering::SeqCst), 0);
        assert_eq!(store.1.lock().unwrap().reads, reads);
    }
}
#[tokio::test]
async fn arbitrary_trust_prose_is_not_an_installed_plan_original() {
    use rsa::sha2::{Digest, Sha256};
    let (bound, _, issuer, _, root, request, now) = file_fixture("1").await;
    prepare(&bound, &issuer, request, now).await.unwrap();
    let original: installation::Original =
        serde_json::from_slice(&std::fs::read(root.join("tls-installation.json")).unwrap())
            .unwrap();
    assert!(
        bound
            .retain_trust(installation::TrustPlan {
                installation: original.installation,
                generation: request,
                public_trust_sha256: format!("{:x}", Sha256::digest(b"synthetic public trust")),
                original: serde_json::json!({"ready":true}),
            })
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_durable_request_survives_reopen_and_never_restarts_missing_state() {
    use std::os::unix::fs::MetadataExt;
    let (bound, store, issuer, reader, root, request, now) = file_fixture("1").await;
    assert!(bound.reserve(request).await.unwrap());
    let reopened = installation::FileBoundStore::open(
        &root,
        std::fs::metadata(&root).unwrap().uid(),
        store,
        reader,
    )
    .unwrap();
    assert!(prepare(&reopened, &issuer, request, now).await.is_err());
    assert_eq!(issuer.0.load(Ordering::SeqCst), 0);
    assert!(reopened.reserved(request).await.unwrap());
}

#[tokio::test]
async fn promotion_joins_original_trust_uid_hash_generation_and_workload() {
    use rsa::sha2::{Digest, Sha256};
    let (bound, store, issuer, _, root, request, now) = file_fixture("1").await;
    prepare(&bound, &issuer, request, now).await.unwrap();
    let selected = slot(&store.0.lock().unwrap(), "next").unwrap().unwrap();
    let original: installation::Original =
        serde_json::from_slice(&std::fs::read(root.join("tls-installation.json")).unwrap())
            .unwrap();
    let config = serde_json::json!({"kind":"ConfigMap","metadata":{"name":"boss-managed-cache-public-trust","namespace":"boss-dev","uid":uuid::Uuid::from_u128(7).to_string(),"resourceVersion":"4"},"data":{"ca.crt":selected.trust}});
    let mut install = Installer {
        ready: true,
        old_absent: true,
        last_expiry: Some(now),
        now,
        fault: FaultEffect::None,
    };
    assert!(
        promote(&bound, &issuer, &install, request, now)
            .await
            .is_err(),
        "missing installed trust original is not a pass"
    );
    for bad in [
        serde_json::json!({"ready":true}),
        {
            let mut v = config.clone();
            v["data"]["ca.crt"] = serde_json::json!("another trust");
            v
        },
        {
            let mut v = config.clone();
            v["metadata"]["name"] = serde_json::json!("another-plan");
            v
        },
        {
            let mut v = config.clone();
            v["metadata"]["namespace"] = serde_json::json!("boss-playground");
            v
        },
    ] {
        assert!(
            bound
                .retain_trust(installation::TrustPlan {
                    installation: original.installation,
                    generation: request,
                    public_trust_sha256: format!("{:x}", Sha256::digest(selected.trust.as_bytes())),
                    original: bad
                })
                .await
                .is_err()
        );
    }
    bound
        .retain_trust(installation::TrustPlan {
            installation: original.installation,
            generation: request,
            public_trust_sha256: format!("{:x}", Sha256::digest(selected.trust.as_bytes())),
            original: config,
        })
        .await
        .unwrap();
    install.fault = FaultEffect::TrustUid;
    assert!(
        promote(&bound, &issuer, &install, request, now)
            .await
            .is_err()
    );
    install.fault = FaultEffect::None;
    assert_eq!(
        promote(&bound, &issuer, &install, request, now)
            .await
            .unwrap(),
        Progress::Promoted
    );
    let rotation = uuid::Uuid::new_v4();
    let staged = prepare(&bound, &issuer, rotation, now).await.unwrap();
    assert!(matches!(staged, Progress::Staged { .. }));
    assert_eq!(
        prepare(&bound, &issuer, rotation, now).await.unwrap(),
        staged
    );
    assert_eq!(issuer.0.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn protected_files_refuse_symlink_hardlink_permissions_and_partial_records() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    for case in 0..5 {
        let (bound, _, issuer, _, root, request, now) = file_fixture("1").await;
        let path = root.join("tls-installation.json");
        let bytes = std::fs::read(&path).unwrap();
        match case {
            0 => {
                std::fs::rename(&path, root.join("original.saved")).unwrap();
                symlink(root.join("original.saved"), &path).unwrap();
            }
            1 => std::fs::hard_link(&path, root.join("second-link")).unwrap(),
            2 => std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap(),
            3 => std::fs::write(&path, &bytes[..bytes.len() / 2]).unwrap(),
            // mode-bits-ok: deliberate permission drift of the fixture directory, never an executable file.
            _ => std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap(),
        }
        assert!(
            prepare(&bound, &issuer, request, now).await.is_err(),
            "case {case}"
        );
        assert_eq!(issuer.0.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn an_original_cannot_name_a_relative_native_api_target() {
    let (bound, _, _, reader, root, _, _) = file_fixture("1").await;
    let path = root.join("tls-installation.json");
    let mut original: installation::Original =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    std::fs::remove_file(path).unwrap();
    original.pod["metadata"]["name"] = serde_json::json!("..");
    reader.0.lock().unwrap()["metadata"]["name"] = serde_json::json!("..");
    assert!(bound.bind(original).await.is_err());
}

#[tokio::test]
async fn fresh_requests_cannot_restart_lost_state_or_replace_the_original() {
    for birth in ["1", "opaque:original-version"] {
        let (bound, store, issuer, _, root, request, now) = file_fixture(birth).await;
        let path = root.join("tls-installation.json");
        let saved = std::fs::read(&path).unwrap();
        let original: installation::Original = serde_json::from_slice(&saved).unwrap();
        let first = prepare(&bound, &issuer, request, now).await.unwrap();
        assert_eq!(prepare(&bound, &issuer, request, now).await.unwrap(), first);
        assert_eq!(issuer.0.load(Ordering::SeqCst), 1);
        let later_version = {
            let mut native = store.0.lock().unwrap();
            native.data.clear();
            native.version.push('1');
            native.version.clone()
        };
        for attempted in [request, uuid::Uuid::new_v4()] {
            assert!(prepare(&bound, &issuer, attempted, now).await.is_err());
            assert_eq!(issuer.0.load(Ordering::SeqCst), 1);
        }
        let mut recaptured = original;
        recaptured.secret_creation["metadata"]["resourceVersion"] =
            serde_json::json!(later_version);
        assert!(bound.bind(recaptured).await.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), saved);
        let mut missing: installation::Original = serde_json::from_slice(&saved).unwrap();
        missing.secret_creation["metadata"]
            .as_object_mut()
            .unwrap()
            .remove("resourceVersion");
        let reads = store.1.lock().unwrap().reads;
        assert!(bound.bind(missing).await.is_err());
        assert_eq!(store.1.lock().unwrap().reads, reads);
    }
}
