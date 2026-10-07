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
