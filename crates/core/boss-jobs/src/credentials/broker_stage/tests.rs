//! The broker-stage verifier's controls (design 6e28ed42's list). A test
//! key is GENERATED per test binary, never checked in: a private key in
//! the tree would trip the public mirror's secret scanning.

use super::*;
use rsa::signature::{SignatureEncoding as _, Signer as _};
use serde_json::json;
use std::sync::OnceLock;

pub(crate) const NOW: i64 = 1_800_000_000;
pub(crate) const KID: &str = "cluster-key-1";

pub(crate) struct Signer {
    private: rsa::RsaPrivateKey,
}

impl Signer {
    fn generate(bits: usize) -> Self {
        Self {
            private: rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, bits)
                .expect("generate a test key"),
        }
    }

    pub(crate) fn trusted(&self, kid: &str) -> TrustedKey {
        TrustedKey {
            kid: kid.to_owned(),
            key: self.private.to_public_key(),
        }
    }

    /// A compact RS256 token over `claims`, with `header` as given.
    pub(crate) fn sign_with(&self, header: &Value, claims: &Value) -> String {
        let enc = |v: &Value| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(v).unwrap())
        };
        let signed = format!("{}.{}", enc(header), enc(claims));
        let signature = rsa::pkcs1v15::SigningKey::<rsa::sha2::Sha256>::new(self.private.clone())
            .sign(signed.as_bytes());
        format!(
            "{signed}.{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signature.to_bytes())
        )
    }

    pub(crate) fn sign(&self, claims: &Value) -> String {
        self.sign_with(&json!({"alg":"RS256","kid":KID,"typ":"JWT"}), claims)
    }

    fn jwk(&self, kid: &str) -> Value {
        let enc = |n: &rsa::BigUint| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(n.to_bytes_be())
        };
        json!({"use":"sig","kty":"RSA","kid":kid,"alg":"RS256",
               "n":enc(self.private.n()),"e":enc(self.private.e())})
    }
}

/// The cluster issuer's key, and one the cluster never issued.
pub(crate) fn cluster() -> &'static Signer {
    static KEY: OnceLock<Signer> = OnceLock::new();
    KEY.get_or_init(|| Signer::generate(2048))
}

pub(crate) fn stranger() -> &'static Signer {
    static KEY: OnceLock<Signer> = OnceLock::new();
    KEY.get_or_init(|| Signer::generate(2048))
}

pub(crate) struct Fixed(pub Vec<TrustedKey>);

impl StageKeys for Fixed {
    fn trusted(&self) -> Result<Vec<TrustedKey>, String> {
        Ok(self.0.clone())
    }
}

struct Dark;

impl StageKeys for Dark {
    fn trusted(&self) -> Result<Vec<TrustedKey>, String> {
        Err("the key set is not mounted".into())
    }
}

pub(crate) fn terms() -> Terms {
    Terms {
        issuer: "https://cluster.test:6443".into(),
        audience: "boss-broker-stage".into(),
        namespace: "boss".into(),
        service_account: "boss".into(),
    }
}

pub(crate) fn keys() -> Fixed {
    Fixed(vec![cluster().trusted(KID)])
}

/// What the cluster signs for the projected volume: ten minutes, one
/// audience, the pod's namespace and ServiceAccount.
pub(crate) fn claims_at(now: i64) -> Value {
    json!({
        "iss": "https://cluster.test:6443",
        "aud": ["boss-broker-stage"],
        "sub": "system:serviceaccount:boss:boss",
        "iat": now - 60, "nbf": now - 60, "exp": now + 540,
        "kubernetes.io": {"namespace":"boss","serviceaccount":{"name":"boss","uid":"u-1"}},
    })
}

fn claims() -> Value {
    claims_at(NOW)
}

fn with(patch: Value) -> Value {
    let mut all = claims();
    for (key, value) in patch.as_object().unwrap() {
        all[key] = value.clone();
    }
    all
}

fn judged(claims: &Value) -> Result<Verified, Refusal> {
    verify(&cluster().sign(claims), &terms(), &keys(), NOW)
}

#[test]
fn a_ten_minute_token_for_the_stage_audience_verifies() {
    assert_eq!(
        judged(&claims()),
        Ok(Verified {
            expires_at: NOW + 540,
            lifetime_secs: 600
        })
    );
    // The audience as a bare string is the same claim.
    assert!(judged(&with(json!({"aud":"boss-broker-stage"}))).is_ok());
}

#[test]
fn another_audience_is_refused_and_so_is_a_shared_one() {
    for aud in [
        json!(["https://cluster.test:6443"]),
        json!("boss"),
        json!(["boss-broker-stage", "https://cluster.test:6443"]),
        json!([]),
        Value::Null,
    ] {
        assert_eq!(judged(&with(json!({"aud": aud}))), Err(Refusal::Audience));
    }
}

#[test]
fn an_expired_token_is_refused_at_its_last_second() {
    assert_eq!(
        judged(&with(
            json!({"iat": NOW - 600, "nbf": NOW - 600, "exp": NOW})
        )),
        Err(Refusal::Expired)
    );
    assert_eq!(
        judged(&with(
            json!({"iat": NOW - 900, "nbf": NOW - 900, "exp": NOW - 300})
        )),
        Err(Refusal::Expired)
    );
}

#[test]
fn a_lifetime_past_ten_minutes_is_refused_however_fresh() {
    for exp in [NOW + 541, NOW + 3540, NOW + 31_536_000] {
        assert_eq!(judged(&with(json!({"exp": exp}))), Err(Refusal::Lifetime));
    }
    // An expiry at or before issue is no lifetime at all.
    assert_eq!(
        judged(&with(
            json!({"iat": NOW + 10, "nbf": NOW - 60, "exp": NOW + 10})
        )),
        Err(Refusal::Lifetime)
    );
}

#[test]
fn a_token_without_its_instants_is_malformed_not_unbounded() {
    for missing in ["exp", "iat"] {
        let mut all = claims();
        all.as_object_mut().unwrap().remove(missing);
        assert_eq!(judged(&all), Err(Refusal::Malformed), "{missing}");
    }
    assert_eq!(
        judged(&with(json!({"exp": "soon"}))),
        Err(Refusal::Malformed)
    );
}

#[test]
fn a_token_from_the_future_is_not_valid_yet() {
    assert_eq!(
        judged(&with(
            json!({"iat": NOW + 120, "nbf": NOW + 120, "exp": NOW + 720})
        )),
        Err(Refusal::NotYetValid)
    );
    // A clock a few seconds apart is not the future.
    assert!(
        judged(&with(
            json!({"iat": NOW + 5, "nbf": NOW + 5, "exp": NOW + 605})
        ))
        .is_ok()
    );
}

#[test]
fn another_namespace_or_serviceaccount_is_refused() {
    let bound = |ns: &str, sa: &str| json!({"namespace":ns,"serviceaccount":{"name":sa}});
    for claims in [
        // Another namespace's `boss`, and this namespace's `default`.
        with(
            json!({"sub":"system:serviceaccount:playground:boss","kubernetes.io":bound("playground","boss")}),
        ),
        with(
            json!({"sub":"system:serviceaccount:boss:default","kubernetes.io":bound("boss","default")}),
        ),
        // The two statements of the workload must AGREE.
        with(json!({"kubernetes.io":bound("playground","boss")})),
        with(json!({"kubernetes.io":bound("boss","default")})),
        with(json!({"sub":"system:serviceaccount:boss:default"})),
        with(json!({"kubernetes.io":Value::Null})),
        with(json!({"sub":Value::Null})),
    ] {
        assert_eq!(judged(&claims), Err(Refusal::Workload), "{claims}");
    }
}

#[test]
fn another_issuer_is_refused() {
    assert_eq!(
        judged(&with(json!({"iss":"https://other.test:6443"}))),
        Err(Refusal::Issuer)
    );
}

#[test]
fn a_key_the_cluster_does_not_publish_is_refused() {
    // Unknown key id.
    let token = stranger().sign_with(&json!({"alg":"RS256","kid":"stranger"}), &claims());
    assert_eq!(
        verify(&token, &terms(), &keys(), NOW),
        Err(Refusal::UnknownKey)
    );
    // The trusted key's id, another key's signature.
    let token = stranger().sign(&claims());
    assert_eq!(
        verify(&token, &terms(), &keys(), NOW),
        Err(Refusal::Signature)
    );
}

#[test]
fn claims_changed_after_signing_are_refused() {
    let honest = cluster().sign(&with(json!({"aud":["somewhere-else"]})));
    let forged = cluster().sign(&claims());
    let (h, rest) = honest.split_once('.').unwrap();
    let signature = rest.split_once('.').unwrap().1;
    let wanted = forged.split('.').nth(1).unwrap();
    let token = format!("{h}.{wanted}.{signature}");
    assert_eq!(
        verify(&token, &terms(), &keys(), NOW),
        Err(Refusal::Signature)
    );
}

#[test]
fn only_rs256_is_read_and_before_any_key() {
    for alg in ["none", "HS256", "RS512", "ES256"] {
        let token = cluster().sign_with(&json!({"alg":alg,"kid":KID}), &claims());
        // `Dark` would answer KeysUnavailable: the algorithm is judged first.
        assert_eq!(
            verify(&token, &terms(), &Dark, NOW),
            Err(Refusal::Algorithm)
        );
    }
    let token = cluster().sign_with(&json!({"alg":"RS256"}), &claims());
    assert_eq!(
        verify(&token, &terms(), &keys(), NOW),
        Err(Refusal::Malformed)
    );
}

#[test]
fn unavailable_or_weak_keys_refuse() {
    let token = cluster().sign(&claims());
    assert_eq!(
        verify(&token, &terms(), &Dark, NOW),
        Err(Refusal::KeysUnavailable)
    );
    let weak = Signer::generate(1024);
    assert_eq!(
        verify(
            &weak.sign(&claims()),
            &terms(),
            &Fixed(vec![weak.trusted(KID)]),
            NOW
        ),
        Err(Refusal::WeakKey)
    );
}

#[test]
fn a_shape_that_is_not_a_signed_token_is_malformed() {
    let good = cluster().sign(&claims());
    let two_parts = good.rsplit_once('.').unwrap().0.to_owned();
    for token in [
        String::new(),
        "   ".into(),
        "not-a-token".into(),
        two_parts,
        format!("{good}.extra"),
        "a".repeat(MAX_TOKEN_BYTES + 1),
    ] {
        assert_eq!(
            verify(&token, &terms(), &keys(), NOW),
            Err(Refusal::Malformed)
        );
    }
}

#[test]
fn no_refusal_carries_a_byte_of_the_token() {
    let token = stranger().sign(&with(json!({"aud":["leaky-audience-value"]})));
    let segments: Vec<&str> = token.split('.').collect();
    let all = [
        Refusal::NotConfigured,
        Refusal::Misconfigured("X unset".into()),
        Refusal::NonePresented,
        Refusal::MultiplePresented,
        Refusal::Malformed,
        Refusal::Algorithm,
        Refusal::KeysUnavailable,
        Refusal::UnknownKey,
        Refusal::WeakKey,
        Refusal::Signature,
        Refusal::Issuer,
        Refusal::Audience,
        Refusal::Workload,
        Refusal::Expired,
        Refusal::NotYetValid,
        Refusal::Lifetime,
        Refusal::PacketUnavailable,
        Refusal::Packet,
    ];
    for refusal in all {
        let said = refusal.to_string();
        assert!(!said.is_empty());
        assert!(segments.iter().all(|s| !said.contains(s)), "{said}");
        assert!(!said.contains("leaky-audience-value"), "{said}");
    }
}

#[test]
fn the_key_set_file_is_read_whole_or_refused() {
    let dir = boss_testing::scratch_dir("broker-stage-key-set");
    let path = dir.join("jwks.json");
    let source = KeySetFile(path.clone());
    assert!(source.trusted().is_err(), "an absent file is unavailable");

    let set = json!({"keys":[
        cluster().jwk(KID),
        {"kty":"EC","kid":"ec-1","crv":"P-256","x":"AA","y":"AA"},
        {"kty":"RSA","kid":"enc-1","use":"enc","n":"AQAB","e":"AQAB"},
    ]});
    std::fs::write(&path, set.to_string()).unwrap();
    let trusted = source.trusted().expect("a key set");
    assert_eq!(trusted.len(), 1, "only the RS256 signing key is trusted");
    assert_eq!(trusted[0].kid, KID);
    assert!(verify(&cluster().sign(&claims()), &terms(), &source, NOW).is_ok());

    for bad in ["", "{}", "{\"keys\":[]}", "[]", "not json"] {
        std::fs::write(&path, bad).unwrap();
        let why = source.trusted().err().expect(bad);
        assert!(why.contains("jwks.json"), "{why}");
        assert_eq!(
            verify(&cluster().sign(&claims()), &terms(), &source, NOW),
            Err(Refusal::KeysUnavailable)
        );
    }
    assert!(
        KeySetFile(dir.clone()).trusted().is_err(),
        "a directory is not a key set"
    );
}

fn vars(set: &[(&str, &str)]) -> Door {
    let set: Vec<(String, String)> = set
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    Door::from_vars(move |name| set.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()))
}

const ALL: [(&str, &str); 5] = [
    (ISSUER_ENV, "https://cluster.test:6443"),
    (AUDIENCE_ENV, "boss-broker-stage"),
    (NAMESPACE_ENV, "boss"),
    (SERVICE_ACCOUNT_ENV, "boss"),
    (KEY_SET_ENV, "/nonexistent/jwks.json"),
];

#[test]
fn the_door_is_off_unconfigured_and_never_partly_on() {
    assert!(matches!(vars(&[]), Door::Off));
    assert!(matches!(vars(&[(AUDIENCE_ENV, "  ")]), Door::Off));
    match vars(&ALL) {
        Door::On { terms: got, .. } => assert_eq!(got, terms()),
        other => panic!("{}", other.describe()),
    }
    for (skipped, (name, _)) in ALL.iter().enumerate() {
        let partial: Vec<_> = ALL
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != skipped)
            .map(|(_, kv)| *kv)
            .collect();
        match vars(&partial) {
            Door::Broken(why) => assert!(why.contains(name), "{why}"),
            other => panic!("partly configured must be broken: {}", other.describe()),
        }
    }
    // The cluster's own audience is the token every pod already holds.
    let mut shared = ALL;
    shared[1] = (AUDIENCE_ENV, "https://cluster.test:6443");
    assert!(matches!(vars(&shared), Door::Broken(why) if why.contains("distinct")));
}

#[test]
fn the_judgement_touches_runner_rows_only() {
    let on = Door::On {
        terms: terms(),
        keys: Arc::new(keys()),
    };
    let broken = Door::Broken("X unset".into());
    let verified = Presented::Verified(Verified {
        expires_at: NOW + 540,
        lifetime_secs: 600,
    });
    let refused = Presented::Refused(Refusal::Audience);

    // Any other credential: as before, whatever the door or the token.
    for door in [&Door::Off, &broken, &on] {
        for presented in [None, Some(&verified), Some(&refused)] {
            assert_eq!(judge(door, "forgejo-access-token", presented), Ok(None));
        }
    }
    // A runner row, door off, nothing presented: today's behaviour.
    assert_eq!(judge(&Door::Off, RUNNER_KIND, None), Ok(None));
    // A runner row otherwise: verified or refused by name.
    assert!(matches!(
        judge(&on, RUNNER_KIND, Some(&verified)),
        Ok(Some(_))
    ));
    assert_eq!(judge(&on, RUNNER_KIND, None), Err(Refusal::NonePresented));
    assert_eq!(
        judge(&on, RUNNER_KIND, Some(&refused)),
        Err(Refusal::Audience)
    );
    assert_eq!(
        judge(&Door::Off, RUNNER_KIND, Some(&refused)),
        Err(Refusal::Audience)
    );
    assert_eq!(
        judge(&Door::Off, RUNNER_KIND, Some(&verified)),
        Err(Refusal::NotConfigured)
    );
    assert!(matches!(
        judge(&broken, RUNNER_KIND, None),
        Err(Refusal::Misconfigured(_))
    ));
}

pub(crate) struct Packets(pub Vec<(uuid::Uuid, StagePacket)>);

#[async_trait::async_trait]
impl StagePackets for Packets {
    async fn packet(&self, id: uuid::Uuid) -> Result<Option<StagePacket>, String> {
        Ok(self
            .0
            .iter()
            .find(|(k, _)| *k == id)
            .map(|(_, p)| p.clone()))
    }
}

struct DarkPackets;

#[async_trait::async_trait]
impl StagePackets for DarkPackets {
    async fn packet(&self, _: uuid::Uuid) -> Result<Option<StagePacket>, String> {
        Err("the jobs store did not answer".into())
    }
}

pub(crate) fn rotation(subject: &str, open: bool) -> StagePacket {
    StagePacket {
        kind: ROTATION_KIND.into(),
        subject_id: subject.into(),
        open,
    }
}

#[tokio::test]
async fn a_verified_stage_belongs_to_the_open_rotation_packet_on_its_credential() {
    let (mine, other, closed, foreign) = (
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
    );
    let id = "ops-runner-credential-forge";
    let mut ops_request = rotation(id, true);
    ops_request.kind = "ops-request".into();
    let packets = Packets(vec![
        (mine, rotation(id, true)),
        (other, rotation("ops-runner-credential-boss-gcp", true)),
        (closed, rotation(id, false)),
        (foreign, ops_request),
    ]);
    let bind = |job: Value| {
        let packets = &packets;
        async move { bind_packet(Some(packets), id, &json!({"job_id": job})).await }
    };
    assert_eq!(bind(json!(mine.to_string())).await, Ok(()));
    for job in [
        json!(other.to_string()),
        json!(closed.to_string()),
        json!(foreign.to_string()),
        json!(uuid::Uuid::new_v4().to_string()),
        json!("not-a-uuid"),
        Value::Null,
    ] {
        assert_eq!(bind(job).await, Err(Refusal::Packet));
    }
    let evidence = json!({"job_id": mine.to_string()});
    assert_eq!(
        bind_packet(None, id, &evidence).await,
        Err(Refusal::PacketUnavailable)
    );
    assert_eq!(
        bind_packet(Some(&DarkPackets), id, &evidence).await,
        Err(Refusal::PacketUnavailable)
    );
}

// ----- the key set fetched from the cluster (backlog 25fff647; design
// 20058482 `stage-keys`: the jobs API fetches it itself, with its own pod
// ServiceAccount token, over the cluster's verified TLS) -----

/// A PUBLIC trust root whose private key was discarded the moment it was
/// generated (2026-10-09): a CA no server can ever present a chain for, so
/// a test that trusts it can only ever watch a handshake fail.
const TEST_ROOT: &str = include_str!("test-root.pem");
/// What the pod token file holds in these tests. No refusal, log line or
/// byte on the wire before a verified handshake may carry it.
const POD_TOKEN: &str = "pod-token-SENTINEL-never-in-an-error";

fn pod_files(name: &str, token: Option<&str>, ca: Option<&str>) -> (PathBuf, PathBuf) {
    let dir = boss_testing::scratch_dir(name);
    let (token_file, ca_file) = (dir.join("token"), dir.join("ca.crt"));
    if let Some(token) = token {
        std::fs::write(&token_file, token).unwrap();
    }
    if let Some(ca) = ca {
        std::fs::write(&ca_file, ca).unwrap();
    }
    (token_file, ca_file)
}

fn with_key_set(value: &str) -> Door {
    let mut all = ALL;
    all[4] = (KEY_SET_ENV, value);
    vars(&all)
}

#[test]
fn a_key_set_is_a_file_or_a_url_over_verified_tls_and_never_plain() {
    for plain in [
        "http://kubernetes.default.svc/openid/v1/jwks",
        "HTTP://kubernetes.default.svc/openid/v1/jwks",
        "ftp://kubernetes.default.svc/openid/v1/jwks",
    ] {
        match with_key_set(plain) {
            Door::Broken(why) => {
                assert!(why.contains(KEY_SET_ENV) && why.contains("TLS"), "{why}")
            }
            other => panic!("{plain} must be refused: {}", other.describe()),
        }
    }
    let url = "https://kubernetes.default.svc/openid/v1/jwks";
    match with_key_set(url) {
        Door::On { keys, .. } => {
            let said = keys.source();
            assert!(
                said.contains(url) && said.contains(CLUSTER_CA_FILE),
                "{said}"
            );
        }
        other => panic!("{}", other.describe()),
    }
    // The boot line names where the keys come from.
    assert!(with_key_set(url).describe().contains(url));
    match with_key_set("/etc/boss/jwks.json") {
        Door::On { keys, .. } => assert!(keys.source().contains("/etc/boss/jwks.json")),
        other => panic!("{}", other.describe()),
    }
}

#[test]
fn the_cluster_key_set_needs_its_pod_identity_and_its_trust_root_and_names_neither_value() {
    for (name, token, ca, named) in [
        ("cluster-keys-no-ca", Some(POD_TOKEN), None, "ca.crt"),
        (
            "cluster-keys-bad-ca",
            Some(POD_TOKEN),
            Some("not a certificate"),
            "ca.crt",
        ),
        ("cluster-keys-no-token", None, Some(TEST_ROOT), "token"),
        (
            "cluster-keys-blank-token",
            Some("  \n"),
            Some(TEST_ROOT),
            "token",
        ),
    ] {
        let (token_file, ca_file) = pod_files(name, token, ca);
        let source = ClusterKeySet {
            // Port 9 is never dialled: each case fails before a connection.
            url: "https://127.0.0.1:9/openid/v1/jwks".into(),
            token_file,
            ca_file,
        };
        let why = source.trusted().err().expect(name);
        assert!(why.contains(named), "{name}: {why}");
        assert!(!why.contains(POD_TOKEN), "{name}: {why}");
        assert_eq!(
            verify(&cluster().sign(&claims()), &terms(), &source, NOW),
            Err(Refusal::KeysUnavailable),
            "{name}"
        );
    }
}

/// A listener that answers one connection with a perfectly good key set
/// in plain HTTP, and hands back every byte the client sent it.
fn plain_key_server() -> (u16, std::thread::JoinHandle<Vec<u8>>) {
    use std::io::{Read as _, Write as _};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let body = json!({"keys":[cluster().jwk(KID)]}).to_string();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut heard = vec![0u8; 16 * 1024];
        let n = stream.read(&mut heard).unwrap_or(0);
        heard.truncate(n);
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
            body.len()
        );
        heard
    });
    (port, server)
}

#[test]
fn the_pod_token_never_crosses_a_connection_that_did_not_verify() {
    let (port, server) = plain_key_server();
    let (token_file, ca_file) = pod_files(
        "cluster-keys-plain-server",
        Some(POD_TOKEN),
        Some(TEST_ROOT),
    );
    let source = ClusterKeySet {
        url: format!("https://127.0.0.1:{port}/openid/v1/jwks"),
        token_file,
        ca_file,
    };
    let why = source
        .trusted()
        .err()
        .expect("a server the cluster CA did not certify holds no key set");
    assert!(!why.contains(POD_TOKEN), "{why}");
    let heard = server.join().unwrap();
    assert!(!heard.is_empty(), "the client dialled, and spoke TLS first");
    let heard = String::from_utf8_lossy(&heard).to_ascii_lowercase();
    assert!(
        !heard.contains(&POD_TOKEN.to_ascii_lowercase()) && !heard.contains("authorization"),
        "a bearer crossed before the handshake verified: {heard}"
    );
}

#[test]
fn a_plain_url_handed_to_the_cluster_key_set_is_never_dialled() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let (token_file, ca_file) =
        pod_files("cluster-keys-plain-url", Some(POD_TOKEN), Some(TEST_ROOT));
    let source = ClusterKeySet {
        url: format!("http://127.0.0.1:{port}/openid/v1/jwks"),
        token_file,
        ca_file,
    };
    let why = source.trusted().err().expect("plain HTTP is refused");
    assert!(!why.contains(POD_TOKEN), "{why}");
    assert!(
        matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock),
        "the plain URL was dialled"
    );
}

#[test]
fn the_boot_reads_the_key_set_once_and_says_what_it_found() {
    assert_eq!(Door::Off.check_keys(), None);
    assert_eq!(Door::Broken("X unset".into()).check_keys(), None);
    let on = |keys: Arc<dyn StageKeys>| Door::On {
        terms: terms(),
        keys,
    };
    assert_eq!(on(Arc::new(keys())).check_keys(), Some(Ok(1)));
    assert!(matches!(
        on(Arc::new(Dark)).check_keys(),
        Some(Err(why)) if why.contains("not mounted")
    ));
}

#[test]
fn the_stage_actor_is_a_registered_automation_of_its_own() {
    use crate::agents::automations::{
        load_automations_dir, platform_automations_path, row_of_record,
    };
    let rows = load_automations_dir(std::path::Path::new(platform_automations_path()))
        .expect("the in-tree automations bundle loads");
    let row = row_of_record(&rows, ACTOR).expect("a row answers for the broker-stage actor");
    // Its OWN row, not a family's: the actor is assigned from a verified
    // workload, so no signer's prefix may answer for it.
    assert_eq!(row.id, ACTOR);
    assert_eq!(row.signs_for, None);
    // The role the stage command already carries (the dispatcher rule's
    // `platform-admin`); the door replaces the id and keeps the role.
    assert_eq!(row.role, "platform-admin");
}
