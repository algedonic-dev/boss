//! Private managed renewal is an explicit, conserved effect, never a read-only
//! cache check. Native installation/scope/provider use remain unavailable.
use super::CacheError;
use async_trait::async_trait;
use base64::Engine;
use boss_jobs::credentials::broker_stage::TrustedKey;
use rsa::signature::Verifier;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub(super) const ISSUER: &str = "https://auth.openai.com";
const CLIENT: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const LIMIT: usize = 256 * 1024;
const RESPONSE_LIMIT: usize = 64 * 1024;

/// Trusted broker installation/lease context, not caller or cached claims.
pub(crate) struct Expected {
    pub run: String,
    pub nonce: uuid::Uuid,
    pub subject: String,
    pub workspace: String,
}

#[async_trait]
pub(crate) trait Provider: Send + Sync {
    async fn keys(&self) -> Result<Vec<TrustedKey>, CacheError>;
    async fn userinfo(&self, access: &str) -> Result<PrivateResponse, CacheError>;
    /// Full private response; missing/uncertain replacements never qualify.
    async fn refresh(&self, refresh: &str) -> Result<PrivateResponse, CacheError>;
}

/// Private status/body/EOF originals. Nothing here implements Debug or a public
/// serialization/diagnostic interface; retain() writes only to private storage.
pub(crate) struct PrivateResponse {
    pub status: u16,
    pub body: Vec<u8>,
    pub complete: bool,
}
impl PrivateResponse {
    fn original(&self) -> Result<Vec<u8>, CacheError> {
        serde_json::to_vec(&json!({"status":self.status,"complete":self.complete,
            "body_base64":base64::engine::general_purpose::STANDARD.encode(&self.body)}))
        .map_err(|_| CacheError::Unavailable)
    }
    fn object(&self) -> Result<Value, CacheError> {
        if self.status != 200 || !self.complete || self.body.len() > RESPONSE_LIMIT {
            return Err(CacheError::Unavailable);
        }
        serde_json::from_slice(&self.body).map_err(|_| CacheError::Unavailable)
    }
}

/// A native adapter must attest exact held lease/incarnation/stop/mount BEFORE
/// input is read. Every record is durable, exact-read-back, private-only.
#[async_trait]
pub(crate) trait PrivateRenewal: Send {
    async fn attest(&mut self, expected: &Expected) -> Result<(), CacheError>;
    async fn input(&mut self) -> Result<Vec<u8>, CacheError>;
    async fn retain(&mut self, phase: &'static str, bytes: &[u8]) -> Result<(), CacheError>;
    async fn quarantine(&mut self) -> Result<(), CacheError>;
}

/// No Debug/Serialize or public bytes accessor: only the private FileStore
/// callback may consume this exact original/replacement binding.
pub(crate) struct Verified {
    run: String,
    nonce: uuid::Uuid,
    original: String,
    replacement: Vec<u8>,
    subject: String,
}
impl super::files::PrivateVerifier for Verified {
    fn verify(&mut self, bytes: &[u8], principal: &str) -> Result<Vec<u8>, CacheError> {
        let digest = crate::launch_controller::hash(bytes);
        if principal != self.subject
            || (digest != self.original
                && digest != crate::launch_controller::hash(&self.replacement))
        {
            return Err(CacheError::InvalidBinding);
        }
        Ok(self.replacement.clone())
    }
}
impl Verified {
    pub(super) fn matches(&self, lease: &super::Lease) -> bool {
        self.run == lease.run
            && self.nonce == lease.nonce
            && self.subject == lease.installed.principal
    }
    pub(super) fn original_hash(&self) -> &str {
        &self.original
    }
    pub(super) fn replacement(&self) -> &[u8] {
        &self.replacement
    }
}

pub(crate) async fn renew_store<P: Provider>(
    expected: &Expected,
    provider: &P,
    store: super::files::FileStore<super::files::AwaitingVerification>,
    lease: super::Lease,
) -> Result<super::files::FileStore<Verified>, CacheError> {
    let mut ledger = super::files::FileRenewal::new(store, lease);
    let proof = renew(expected, provider, &mut ledger).await?;
    ledger.seal(proof).await
}

/// Fixed provider adapter; no caller-supplied endpoint, issuer, client, proxy,
/// redirect or private diagnostic. Native callers are still uninstalled.
pub(crate) struct OpenAiProvider {
    client: reqwest::Client,
}
enum Operation {
    Keys,
    Userinfo,
    Refresh,
}
impl OpenAiProvider {
    pub(crate) fn new() -> Result<Self, CacheError> {
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .map_err(|_| CacheError::Unavailable)?;
        Ok(Self { client })
    }
    fn request(
        &self,
        operation: Operation,
        credential: &str,
    ) -> Result<reqwest::Request, CacheError> {
        let request=match operation {
            Operation::Keys=>self.client.get("https://auth.openai.com/.well-known/jwks.json"),
            Operation::Userinfo=>self.client.get("https://auth.openai.com/api/accounts/oauth/userinfo").bearer_auth(credential),
            // Exact pinned0.159.3 native refresh operation, not the different
            // token-sharing registration or a guessed discovery alias.
            Operation::Refresh=>self.client.post("https://auth.openai.com/oauth/token")
                .json(&json!({"client_id":CLIENT,"grant_type":"refresh_token","refresh_token":credential})),
        };
        request
            .header("accept", "application/json")
            .build()
            .map_err(|_| CacheError::Unavailable)
    }
    async fn exchange(
        &self,
        operation: Operation,
        credential: &str,
    ) -> Result<PrivateResponse, CacheError> {
        let mut response = self
            .client
            .execute(self.request(operation, credential)?)
            .await
            .map_err(|_| CacheError::Unavailable)?;
        let status = response.status().as_u16();
        let mut body = Vec::new();
        loop {
            match response.chunk().await {
                Ok(Some(chunk)) => {
                    let take = (RESPONSE_LIMIT + 1)
                        .saturating_sub(body.len())
                        .min(chunk.len());
                    body.extend_from_slice(&chunk[..take]);
                    if body.len() > RESPONSE_LIMIT {
                        return Ok(PrivateResponse {
                            status,
                            body,
                            complete: false,
                        });
                    }
                }
                Ok(None) => {
                    return Ok(PrivateResponse {
                        status,
                        body,
                        complete: true,
                    });
                }
                Err(_) => {
                    return Ok(PrivateResponse {
                        status,
                        body,
                        complete: false,
                    });
                }
            }
        }
    }
}
#[async_trait]
impl Provider for OpenAiProvider {
    async fn keys(&self) -> Result<Vec<TrustedKey>, CacheError> {
        let response = self.exchange(Operation::Keys, "").await?;
        if response.status != 200 || !response.complete || response.body.len() > RESPONSE_LIMIT {
            return Err(CacheError::Unavailable);
        }
        let text = std::str::from_utf8(&response.body).map_err(|_| CacheError::Unavailable)?;
        boss_jobs::credentials::broker_stage::parse_key_set(text)
            .filter(|keys| !keys.is_empty())
            .ok_or(CacheError::Unavailable)
    }
    async fn userinfo(&self, access: &str) -> Result<PrivateResponse, CacheError> {
        self.exchange(Operation::Userinfo, access).await
    }
    async fn refresh(&self, refresh: &str) -> Result<PrivateResponse, CacheError> {
        self.exchange(Operation::Refresh, refresh).await
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Tokens {
    id_token: String,
    access_token: String,
    refresh_token: String,
    account_id: String,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Cache {
    auth_mode: String,
    #[serde(rename = "OPENAI_API_KEY")]
    api_key: Option<String>,
    tokens: Tokens,
    last_refresh: String,
}
fn nonempty(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= LIMIT
        && value.trim() == value
        && !value.chars().any(char::is_control)
}
fn cache(bytes: &[u8], expected: &Expected) -> Result<Cache, CacheError> {
    if bytes.len() > LIMIT {
        return Err(CacheError::InvalidBinding);
    }
    let cache: Cache = serde_json::from_slice(bytes).map_err(|_| CacheError::InvalidBinding)?;
    if cache.auth_mode != "chatgpt"
        || cache.api_key.is_some()
        || cache.tokens.account_id != expected.workspace
        || [
            &cache.tokens.id_token,
            &cache.tokens.access_token,
            &cache.tokens.refresh_token,
        ]
        .iter()
        .any(|s| !nonempty(s))
        || chrono::DateTime::parse_from_rfc3339(&cache.last_refresh).is_err()
    {
        return Err(CacheError::InvalidBinding);
    }
    Ok(cache)
}

fn identity(
    token: &str,
    keys: &[TrustedKey],
    expected: &Expected,
    now: i64,
) -> Result<i64, CacheError> {
    let parts = token.split('.').collect::<Vec<_>>();
    let [header, payload, signature] = parts.as_slice() else {
        return Err(CacheError::InvalidBinding);
    };
    let decode = |part: &str| {
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(part)
            .map_err(|_| CacheError::InvalidBinding)
    };
    let header: Value =
        serde_json::from_slice(&decode(header)?).map_err(|_| CacheError::InvalidBinding)?;
    if header["alg"] != "RS256"
        || ["crit", "jku", "jwk"]
            .iter()
            .any(|key| header.get(key).is_some())
    {
        return Err(CacheError::InvalidBinding);
    }
    let key = keys
        .iter()
        .filter(|key| Some(key.kid.as_str()) == header["kid"].as_str())
        .collect::<Vec<_>>();
    let [key] = key.as_slice() else {
        return Err(CacheError::Unavailable);
    };
    let sig = rsa::pkcs1v15::Signature::try_from(decode(signature)?.as_slice())
        .map_err(|_| CacheError::InvalidBinding)?;
    rsa::pkcs1v15::VerifyingKey::<rsa::sha2::Sha256>::new(key.key.clone())
        .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &sig)
        .map_err(|_| CacheError::InvalidBinding)?;
    let claims: Value =
        serde_json::from_slice(&decode(payload)?).map_err(|_| CacheError::InvalidBinding)?;
    let audience = claims["aud"] == CLIENT || claims["aud"] == json!([CLIENT]);
    let issued = claims["iat"].as_i64().ok_or(CacheError::InvalidBinding)?;
    let expires = claims["exp"].as_i64().ok_or(CacheError::InvalidBinding)?;
    if claims["iss"] != ISSUER
        || !audience
        || claims["sub"] != expected.subject
        || claims["https://api.openai.com/auth"]["chatgpt_account_id"] != expected.workspace
        || claims.get("azp").is_some_and(|value| value != CLIENT)
        || issued < 0
        || issued > now.saturating_add(5)
        || expires <= now
        || expires <= issued
        || claims
            .get("nbf")
            .is_some_and(|value| value.as_i64().is_none_or(|n| n > now.saturating_add(5)))
    {
        return Err(CacheError::InvalidBinding);
    }
    Ok(issued)
}

async fn userinfo<P: Provider, L: PrivateRenewal>(
    provider: &P,
    ledger: &mut L,
    access: &str,
    expected: &Expected,
    phase: &'static str,
) -> Result<(), CacheError> {
    let response = provider.userinfo(access).await?;
    ledger.retain(phase, &response.original()?).await?;
    let response = response.object()?;
    if response["sub"].as_str() != Some(expected.subject.as_str()) {
        return Err(CacheError::InvalidBinding);
    }
    Ok(())
}

async fn attempt<P: Provider, L: PrivateRenewal>(
    expected: &Expected,
    provider: &P,
    ledger: &mut L,
) -> Result<Verified, CacheError> {
    let input = ledger.input().await?;
    ledger.retain("original", &input).await?;
    let original = cache(&input, expected)?;
    let keys = provider.keys().await?;
    identity(
        &original.tokens.id_token,
        &keys,
        expected,
        boss_clock_client::wall_now().timestamp(),
    )?;
    userinfo(
        provider,
        ledger,
        &original.tokens.access_token,
        expected,
        "access-before",
    )
    .await?;
    let began = boss_clock_client::wall_now().timestamp();
    ledger
        .retain(
            "refresh-intent",
            &serde_json::to_vec(&json!({"run":expected.run,"nonce":expected.nonce,
        "issuer":ISSUER,"client":CLIENT,"operation":"managed-refresh","began":began,
        "input_sha256":crate::launch_controller::hash(&input)}))
            .map_err(|_| CacheError::Unavailable)?,
        )
        .await?;
    // Only after the durable intent may the rotating effect be attempted.
    let response = provider.refresh(&original.tokens.refresh_token).await?;
    ledger
        .retain("refresh-response", &response.original()?)
        .await?;
    let response = response.object()?;
    let field = |name| {
        response
            .get(name)
            .and_then(Value::as_str)
            .filter(|s| nonempty(s))
            .map(str::to_owned)
            .ok_or(CacheError::InvalidBinding)
    };
    let id_token = field("id_token")?;
    let access_token = field("access_token")?;
    let refresh_token = field("refresh_token")?;
    let keys = provider.keys().await?;
    let issued = identity(
        &id_token,
        &keys,
        expected,
        boss_clock_client::wall_now().timestamp(),
    )?;
    if id_token == original.tokens.id_token
        || access_token == original.tokens.access_token
        || refresh_token == original.tokens.refresh_token
        || issued < began.saturating_sub(5)
    {
        return Err(CacheError::InvalidBinding);
    }
    userinfo(provider, ledger, &access_token, expected, "access-after").await?;
    let replacement = serde_json::to_vec(&Cache {
        auth_mode: "chatgpt".into(),
        api_key: None,
        tokens: Tokens {
            id_token,
            access_token,
            refresh_token,
            account_id: expected.workspace.clone(),
        },
        last_refresh: boss_clock_client::wall_now().to_rfc3339(),
    })
    .map_err(|_| CacheError::Unavailable)?;
    if replacement.len() > LIMIT {
        return Err(CacheError::InvalidBinding);
    }
    ledger.retain("replacement", &replacement).await?;
    Ok(Verified {
        run: expected.run.clone(),
        nonce: expected.nonce,
        original: crate::launch_controller::hash(&input),
        replacement,
        subject: expected.subject.clone(),
    })
}

pub(crate) async fn renew<P: Provider, L: PrivateRenewal>(
    expected: &Expected,
    provider: &P,
    ledger: &mut L,
) -> Result<Verified, CacheError> {
    // Caller assertions do not let an invalid request read or quarantine a
    // stream. Once attested, every uncertain phase stays unavailable.
    ledger.attest(expected).await?;
    if uuid::Uuid::parse_str(&expected.run)
        .ok()
        .is_none_or(|run| run.is_nil())
        || expected.nonce.is_nil()
        || !nonempty(&expected.subject)
        || !nonempty(&expected.workspace)
    {
        return Err(CacheError::InvalidBinding);
    }
    match attempt(expected, provider, ledger).await {
        Ok(proof) => Ok(proof),
        Err(reason) => {
            let _ = ledger.quarantine().await;
            Err(reason)
        }
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use base64::Engine;
    use rsa::signature::{SignatureEncoding, Signer};
    use serde_json::{Value, json};
    use std::sync::Mutex;

    pub(crate) struct FakeProvider {
        key: rsa::RsaPrivateKey,
        old: String,
        before: String,
        after: String,
        /// None answers the refresh the way a real issuer does: a
        /// replacement minted at the moment of the refresh, recorded in
        /// `minted`. A fixed body is for the refusal cases, never the
        /// qualifying one, because `attempt` judges a replacement's `iat`
        /// against the refresh it answers (backlog 252491a8).
        response: Option<Vec<u8>>,
        minted: Mutex<Option<String>>,
        failed: bool,
        calls: Mutex<Vec<String>>,
    }
    #[async_trait]
    impl Provider for FakeProvider {
        async fn keys(&self) -> Result<Vec<TrustedKey>, CacheError> {
            Ok(vec![TrustedKey {
                kid: "synthetic".into(),
                key: self.key.to_public_key(),
            }])
        }
        async fn userinfo(&self, access: &str) -> Result<PrivateResponse, CacheError> {
            self.calls.lock().unwrap().push("userinfo".into());
            let subject = match access {
                "SYNTHETIC_OLD_ACCESS" => self.before.clone(),
                "SYNTHETIC_NEW_ACCESS" => self.after.clone(),
                _ => return Err(CacheError::Unavailable),
            };
            Ok(PrivateResponse {
                status: 200,
                complete: true,
                body: serde_json::to_vec(&json!({"sub":subject})).unwrap(),
            })
        }
        async fn refresh(&self, refresh: &str) -> Result<PrivateResponse, CacheError> {
            assert_eq!(refresh, "SYNTHETIC_OLD_REFRESH");
            self.calls.lock().unwrap().push("refresh".into());
            if self.failed {
                return Err(CacheError::Unavailable);
            }
            let body = match &self.response {
                Some(body) => body.clone(),
                None => {
                    let id_token = signed(
                        &self.key,
                        &claims(boss_clock_client::wall_now().timestamp(), "replacement"),
                    );
                    *self.minted.lock().unwrap() = Some(id_token.clone());
                    serde_json::to_vec(&json!({"id_token":id_token,"access_token":"SYNTHETIC_NEW_ACCESS", "refresh_token":"SYNTHETIC_NEW_REFRESH"})).unwrap()
                }
            };
            Ok(PrivateResponse {
                status: 200,
                complete: true,
                body,
            })
        }
    }
    struct Ledger {
        scope: bool,
        bytes: Vec<u8>,
        reads: usize,
        records: Vec<(&'static str, Vec<u8>)>,
        fail: Option<&'static str>,
        quarantined: bool,
    }
    #[async_trait]
    impl PrivateRenewal for Ledger {
        async fn attest(&mut self, _: &Expected) -> Result<(), CacheError> {
            if self.scope {
                Ok(())
            } else {
                Err(CacheError::InvalidBinding)
            }
        }
        async fn input(&mut self) -> Result<Vec<u8>, CacheError> {
            self.reads += 1;
            Ok(self.bytes.clone())
        }
        async fn retain(&mut self, phase: &'static str, bytes: &[u8]) -> Result<(), CacheError> {
            // Simulate a durable write that committed before acknowledgment loss.
            self.records.push((phase, bytes.to_vec()));
            if self.fail == Some(phase) {
                Err(CacheError::Unavailable)
            } else {
                Ok(())
            }
        }
        async fn quarantine(&mut self) -> Result<(), CacheError> {
            self.quarantined = true;
            Ok(())
        }
    }
    fn signed(key: &rsa::RsaPrivateKey, claims: &Value) -> String {
        let enc = |bytes: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
        let input = format!(
            "{}.{}",
            enc(br#"{"alg":"RS256","kid":"synthetic"}"#),
            enc(&serde_json::to_vec(claims).unwrap())
        );
        let sig =
            rsa::pkcs1v15::SigningKey::<rsa::sha2::Sha256>::new(key.clone()).sign(input.as_bytes());
        format!("{input}.{}", enc(&sig.to_vec()))
    }
    fn claims(now: i64, serial: &str) -> Value {
        json!({"iss":ISSUER,"aud":CLIENT,"sub":"synthetic-subject", "iat":now,"exp":now+3600,"jti":serial,
            "https://api.openai.com/auth":{"chatgpt_account_id":"synthetic-workspace"}})
    }
    fn input(id: &str) -> Vec<u8> {
        serde_json::to_vec(&json!({"auth_mode":"chatgpt","OPENAI_API_KEY":null,
            "tokens":{"id_token":id,"access_token":"SYNTHETIC_OLD_ACCESS", "refresh_token":"SYNTHETIC_OLD_REFRESH","account_id":"synthetic-workspace"},
            "last_refresh":"2026-01-01T00:00:00Z"})).unwrap()
    }
    fn fixture() -> (Expected, FakeProvider, Ledger) {
        let key = rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap();
        let now = boss_clock_client::wall_now().timestamp();
        let old = signed(&key, &claims(now - 30, "original"));
        (
            Expected {
                run: uuid::Uuid::new_v4().to_string(),
                nonce: uuid::Uuid::new_v4(),
                subject: "synthetic-subject".into(),
                workspace: "synthetic-workspace".into(),
            },
            FakeProvider {
                key,
                old: old.clone(),
                before: "synthetic-subject".into(),
                after: "synthetic-subject".into(),
                response: None,
                minted: Mutex::new(None),
                failed: false,
                calls: Mutex::new(vec![]),
            },
            Ledger {
                scope: true,
                bytes: input(&old),
                reads: 0,
                records: vec![],
                fail: None,
                quarantined: false,
            },
        )
    }
    pub(crate) fn provider_fixture() -> (Expected, FakeProvider, Vec<u8>) {
        let (expected, provider, ledger) = fixture();
        (expected, provider, ledger.bytes)
    }
    #[tokio::test]
    async fn a_full_authenticated_replacement_is_retained_before_qualification() {
        let (expected, provider, mut ledger) = fixture();
        let proof = renew(&expected, &provider, &mut ledger).await.unwrap();
        assert_eq!(
            proof.original,
            crate::launch_controller::hash(&ledger.bytes)
        );
        assert_eq!(proof.subject, expected.subject);
        let replaced: Value = serde_json::from_slice(&proof.replacement).unwrap();
        assert_eq!(replaced["tokens"]["refresh_token"], "SYNTHETIC_NEW_REFRESH");
        assert_eq!(
            replaced["tokens"]["id_token"].as_str(),
            provider.minted.lock().unwrap().as_deref()
        );
        assert_eq!(
            ledger.records.iter().map(|r| r.0).collect::<Vec<_>>(),
            [
                "original",
                "access-before",
                "refresh-intent",
                "refresh-response",
                "access-after",
                "replacement"
            ]
        );
        assert!(!ledger.quarantined);
    }
    /// Backlog 252491a8: gate 7f5e3cfd went red at files.rs:756 with
    /// InvalidBinding because this fixture minted the replacement id_token
    /// when the FIXTURE was built, and a loaded node paused the test for
    /// longer than the 5 s grace `attempt` allows between the refresh
    /// intent and the replacement's `iat`. A real issuer mints at refresh
    /// time, so the fake does too. 6 s is the smallest pause that crosses
    /// the grace whatever the second boundaries (`iat` and `began` are
    /// whole seconds), so this is red on the old fixture every time.
    #[tokio::test]
    async fn a_pause_between_fixture_and_refresh_still_qualifies_a_fresh_replacement() {
        let (expected, provider, mut ledger) = fixture();
        tokio::time::sleep(std::time::Duration::from_secs(6)).await;
        assert_eq!(
            renew(&expected, &provider, &mut ledger).await.err(),
            None,
            "a pause before the refresh is not a refusal"
        );
    }
    #[tokio::test]
    async fn scope_refusal_precedes_every_private_read_or_provider_effect() {
        let (expected, provider, mut ledger) = fixture();
        ledger.scope = false;
        assert!(renew(&expected, &provider, &mut ledger).await.is_err());
        assert_eq!(ledger.reads, 0);
        assert!(provider.calls.lock().unwrap().is_empty());
    }
    #[tokio::test]
    async fn signed_identity_cannot_attest_a_foreign_access_or_refresh_principal() {
        let (expected, mut provider, mut ledger) = fixture();
        provider.before = "foreign-access-subject".into();
        assert!(renew(&expected, &provider, &mut ledger).await.is_err());
        assert!(!provider.calls.lock().unwrap().contains(&"refresh".into()));
        provider.before = expected.subject.clone();
        let mut foreign = claims(boss_clock_client::wall_now().timestamp(), "foreign");
        foreign["sub"] = json!("foreign-refresh-subject");
        let response = serde_json::to_vec(&json!({"id_token":signed(&provider.key,&foreign),"access_token":"SYNTHETIC_NEW_ACCESS", "refresh_token":"SYNTHETIC_NEW_REFRESH"})).unwrap();
        provider.response = Some(response.clone());
        assert!(renew(&expected, &provider, &mut ledger).await.is_err());
        assert!(ledger.quarantined);
        assert!(ledger.records.iter().any(|r| r.0 == "refresh-response"
            && serde_json::from_slice::<Value>(&r.1).unwrap()["body_base64"]
                == base64::engine::general_purpose::STANDARD.encode(&response)));
    }
    #[tokio::test]
    async fn incomplete_or_old_refresh_responses_never_qualify() {
        let (expected, mut provider, mut ledger) = fixture();
        let new = signed(
            &provider.key,
            &claims(boss_clock_client::wall_now().timestamp(), "replacement"),
        );
        for response in [
            json!({}),
            json!({"id_token":provider.old,"access_token":"SYNTHETIC_NEW_ACCESS", "refresh_token":"SYNTHETIC_NEW_REFRESH"}),
            json!({"id_token":new,"access_token":"SYNTHETIC_NEW_ACCESS", "refresh_token":"SYNTHETIC_OLD_REFRESH"}),
        ] {
            provider.response = Some(serde_json::to_vec(&response).unwrap());
            assert!(renew(&expected, &provider, &mut ledger).await.is_err());
            assert!(ledger.quarantined);
        }
    }
    /// Backlog f12a6717: the case above sends the ORIGINAL id_token as its
    /// stale one, so the equality check refuses it first and the 5 s
    /// freshness guard was never the reason anything failed; disabling that
    /// guard left all 61 managed_cache tests green (review of car 50893346).
    /// This replacement is distinct in all three tokens and correctly signed
    /// by the trusted key, so `identity` accepts it (asserted, so the test
    /// names its own cause) and only its `iat`, 60 s before the refresh
    /// intent, can refuse it, after the response is retained and before
    /// the new access token is ever presented.
    #[tokio::test]
    async fn a_distinct_signed_replacement_issued_before_the_refresh_never_qualifies() {
        let (expected, mut provider, mut ledger) = fixture();
        let now = boss_clock_client::wall_now().timestamp();
        let stale = signed(&provider.key, &claims(now - 60, "replacement"));
        assert_ne!(stale, provider.old);
        assert_eq!(
            identity(&stale, &provider.keys().await.unwrap(), &expected, now),
            Ok(now - 60),
            "the stale replacement must pass every identity check, or this test pins the wrong refusal"
        );
        provider.response = Some(serde_json::to_vec(&json!({"id_token":stale,"access_token":"SYNTHETIC_NEW_ACCESS", "refresh_token":"SYNTHETIC_NEW_REFRESH"})).unwrap());
        assert_eq!(
            renew(&expected, &provider, &mut ledger).await.err(),
            Some(CacheError::InvalidBinding)
        );
        assert!(ledger.quarantined);
        assert_eq!(
            ledger.records.iter().map(|r| r.0).collect::<Vec<_>>(),
            [
                "original",
                "access-before",
                "refresh-intent",
                "refresh-response"
            ],
            "refused on the replacement's age: retained, never presented"
        );
    }
    #[tokio::test]
    async fn committed_write_error_or_uncertain_refresh_keeps_the_stream_unavailable() {
        let (expected, mut provider, mut ledger) = fixture();
        for phase in [
            "original",
            "refresh-intent",
            "refresh-response",
            "replacement",
        ] {
            ledger.fail = Some(phase);
            assert!(renew(&expected, &provider, &mut ledger).await.is_err());
            assert!(ledger.quarantined);
        }
        ledger.fail = None;
        provider.failed = true;
        assert!(renew(&expected, &provider, &mut ledger).await.is_err());
        assert!(ledger.quarantined);
    }
    #[tokio::test]
    async fn invalid_signed_claims_or_signature_refuse_before_refresh() {
        let (expected, provider, mut ledger) = fixture();
        let base = claims(boss_clock_client::wall_now().timestamp(), "bad");
        for (field, value) in [
            ("iss", json!("https://foreign.invalid")),
            ("aud", json!("foreign-client")),
            ("exp", json!(1)),
            ("sub", json!("foreign-sub")),
            ("iat", json!(4102444800_i64)),
        ] {
            let mut bad = base.clone();
            bad[field] = value;
            ledger.bytes = input(&signed(&provider.key, &bad));
            assert!(renew(&expected, &provider, &mut ledger).await.is_err());
        }
        let foreign = rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap();
        ledger.bytes = input(&signed(&foreign, &base));
        assert!(renew(&expected, &provider, &mut ledger).await.is_err());
        assert!(!provider.calls.lock().unwrap().contains(&"refresh".into()));
    }
    #[tokio::test]
    async fn changed_access_after_refresh_and_unbounded_or_ambiguous_bodies_quarantine() {
        let (expected, mut provider, mut ledger) = fixture();
        provider.after = "foreign-new-access".into();
        assert!(renew(&expected, &provider, &mut ledger).await.is_err());
        assert!(ledger.quarantined);
        provider.after = expected.subject.clone();
        for body in [
            b"private non-JSON failure".to_vec(),
            vec![b'x'; RESPONSE_LIMIT + 1],
        ] {
            provider.response = Some(body);
            assert!(renew(&expected, &provider, &mut ledger).await.is_err());
        }
        assert!(ledger.quarantined);
        let incomplete = PrivateResponse {
            status: 200,
            body: br#"{"sub":"synthetic-subject"}"#.to_vec(),
            complete: false,
        };
        assert!(incomplete.object().is_err());
        let rejected = PrivateResponse {
            status: 401,
            body: br#"{"sub":"synthetic-subject"}"#.to_vec(),
            complete: true,
        };
        assert!(rejected.object().is_err());
    }
    #[tokio::test]
    async fn foreign_mode_unknown_fields_and_malformed_cache_refuse_before_provider() {
        let (expected, provider, mut ledger) = fixture();
        let original: Value = serde_json::from_slice(&ledger.bytes).unwrap();
        for (field, value) in [
            ("auth_mode", json!("chatgptAuthTokens")),
            ("OPENAI_API_KEY", json!("SYNTHETIC_OTHER_KEY")),
            ("last_refresh", json!(true)),
            ("unknown", json!("SYNTHETIC_EXTRA")),
        ] {
            let mut cache = original.clone();
            cache[field] = value;
            ledger.bytes = serde_json::to_vec(&cache).unwrap();
            assert!(renew(&expected, &provider, &mut ledger).await.is_err());
        }
        ledger.bytes = b"not a managed cache".to_vec();
        assert!(renew(&expected, &provider, &mut ledger).await.is_err());
        assert!(provider.calls.lock().unwrap().is_empty());
    }
    #[test]
    fn the_native_provider_request_shape_has_fixed_targets_and_private_credentials() {
        let provider = OpenAiProvider::new().unwrap();
        let request = provider
            .request(Operation::Userinfo, "SYNTHETIC_ACCESS")
            .unwrap();
        assert_eq!(
            request.url().as_str(),
            "https://auth.openai.com/api/accounts/oauth/userinfo"
        );
        assert_eq!(
            request.headers()["authorization"],
            "Bearer SYNTHETIC_ACCESS"
        );
        let request = provider
            .request(Operation::Refresh, "SYNTHETIC_REFRESH")
            .unwrap();
        assert_eq!(
            request.url().as_str(),
            "https://auth.openai.com/oauth/token"
        );
        assert_eq!(request.method(), reqwest::Method::POST);
        let body: Value =
            serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).unwrap();
        assert_eq!(
            body,
            json!({"client_id":CLIENT,"grant_type":"refresh_token","refresh_token":"SYNTHETIC_REFRESH"})
        );
        assert!(request.headers().get("authorization").is_none());
        assert_eq!(
            provider
                .request(Operation::Keys, "")
                .unwrap()
                .url()
                .as_str(),
            "https://auth.openai.com/.well-known/jwks.json"
        );
    }
}
