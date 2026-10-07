//! The broker-stage door: a runner credential's stage commands are
//! believed from a VERIFIED workload token, never from the rule label the
//! caller types (design 6e28ed42, decided by David 2026-10-05 and
//! confirmed 2026-10-06; backlog 1e50e66b, for the urgent 6c9183de).
//!
//! WHY. `POST /api/credentials/{id}/rotation/{phase}` took any
//! operator-tier caller: the dispatcher's asserted `x-boss-user` plus the
//! estate machine token, which every agent's door, the conductor and each
//! host also hold. Design 6805c764 calls that token membership, not
//! identity — so anyone holding it could record that an ops runner's
//! credential had been minted, installed, verified or revoked. The host's
//! delivery acknowledgment was authenticated in car 0451f958; the
//! broker's own stages were the half left on an asserted label.
//!
//! WHAT IS VERIFIED, AND WHAT IS NOT. The caller presents, in [`HEADER`],
//! a Kubernetes ServiceAccount token projected for ONE audience that is
//! not the cluster's own. The door checks, in this order: RS256 and
//! nothing else; a signing key the trusted key set names; the signature;
//! then the issuer, the audience (that one and no other), the namespace
//! and ServiceAccount (the `sub` AND the `kubernetes.io` claim, which must
//! agree), the expiry, and a lifetime of at most [`MAX_LIFETIME_SECS`].
//! That proves WORKLOAD MEMBERSHIP: some process in the pod that holds
//! that ServiceAccount asked. It does not prove which process, which rule
//! fired, or that the pod still exists — verification is offline, so a
//! retired workload's token is believed until it expires, ten minutes at
//! most. David accepted exactly that limit; the alternative, a
//! dispatcher-exclusive workload, was not chosen.
//!
//! WHAT IT AUTHORIZES. Only the phases of a registry row whose kind is
//! [`RUNNER_KIND`], and only for the open [`ROTATION_KIND`] packet whose
//! subject is that row ([`StagePackets`]). Every other credential's
//! phases are judged exactly as before and a token changes nothing about
//! them. A host's runner credential cannot stage (the rotation door
//! refuses a credentialed caller first), and this token is not consulted
//! by the delivery door at all.
//!
//! IT IS OFF UNTIL CONFIGURED, AND NOTHING CONFIGURES IT. The decision
//! authorizes no deployment, no grant and no enforcement flip, so the
//! five variables of [`Door::from_env`] are set nowhere and no manifest
//! projects the token. Off, a runner row's phases are recorded as they
//! always were. Partly configured is not off: it refuses runner stages
//! by name. A token presented to a door that cannot verify it is refused
//! too — a presented identity is never quietly ignored.
//!
//! NO TokenReview. The decision grants none, so nothing here asks the
//! cluster whether a token is still live; that is the ten-minute limit
//! above, stated rather than hidden. The trusted keys arrive through the
//! [`StageKeys`] port, whose one adapter reads a key-set file on every
//! verification. Who fills that file — and with what proof of the
//! cluster's TLS trust — belongs to the activation packet, not this car.
//!
//! NO VALUE LEAVES THIS MODULE: a refusal is one of a closed set of
//! fixed sentences, and a verified token is logged as its audience, its
//! expiry and its lifetime.

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use base64::Engine as _;
use rsa::signature::Verifier as _;
use rsa::traits::PublicKeyParts as _;
use serde_json::Value;

/// The header the workload presents its token in. An `x-boss-` name, so
/// the gateway strips it at the edge like every other machine header.
pub const HEADER: &str = "x-boss-broker-stage-token";

/// The actor a verified stage is recorded as. Assigned here, from the
/// verified workload — never read from the caller's `x-boss-user`.
pub const ACTOR: &str = "automation:credential-broker-stage";

/// The registry kind whose phases this door guards, and the protocol
/// whose packet a stage must belong to.
pub const RUNNER_KIND: &str = "ops-runner-credential";
pub const ROTATION_KIND: &str = "rotate-a-credential";

/// David's bound (6e28ed42): requested and accepted lifetime, seconds.
pub const MAX_LIFETIME_SECS: i64 = 600;

/// Clock difference tolerated on `iat` / `nbf` only. Expiry has none.
const NOT_BEFORE_LEEWAY_SECS: i64 = 30;
const MAX_TOKEN_BYTES: usize = 8 * 1024;
const MAX_KEY_SET_BYTES: u64 = 64 * 1024;
const MIN_MODULUS_BITS: usize = 2048;

pub const ISSUER_ENV: &str = "BOSS_BROKER_STAGE_ISSUER";
pub const AUDIENCE_ENV: &str = "BOSS_BROKER_STAGE_AUDIENCE";
pub const NAMESPACE_ENV: &str = "BOSS_BROKER_STAGE_NAMESPACE";
pub const SERVICE_ACCOUNT_ENV: &str = "BOSS_BROKER_STAGE_SERVICE_ACCOUNT";
pub const KEY_SET_ENV: &str = "BOSS_BROKER_STAGE_KEY_SET";
const VARS: [&str; 5] = [
    ISSUER_ENV,
    AUDIENCE_ENV,
    NAMESPACE_ENV,
    SERVICE_ACCOUNT_ENV,
    KEY_SET_ENV,
];

/// What a token must say to be this door's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Terms {
    pub issuer: String,
    pub audience: String,
    pub namespace: String,
    pub service_account: String,
}

/// Why a stage was not believed. A closed set of fixed sentences: none
/// carries a byte of the token, a claim, or a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    NotConfigured,
    Misconfigured(String),
    NonePresented,
    MultiplePresented,
    Malformed,
    Algorithm,
    KeysUnavailable,
    UnknownKey,
    WeakKey,
    Signature,
    Issuer,
    Audience,
    Workload,
    Expired,
    NotYetValid,
    Lifetime,
    PacketUnavailable,
    Packet,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Refusal::NotConfigured => {
                "a broker-stage token was presented, and this jobs API has no verification configured for one"
            }
            Refusal::Misconfigured(why) => {
                return write!(f, "broker-stage verification is misconfigured: {why}");
            }
            Refusal::NonePresented => "no broker-stage token was presented",
            Refusal::MultiplePresented => "more than one broker-stage token was presented",
            Refusal::Malformed => "the broker-stage token is not a well-formed signed token",
            Refusal::Algorithm => "the broker-stage token is not signed with RS256",
            Refusal::KeysUnavailable => "the trusted signing keys are unavailable",
            Refusal::UnknownKey => "the broker-stage token names a signing key that is not trusted",
            Refusal::WeakKey => "the trusted signing key is shorter than 2048 bits",
            Refusal::Signature => "the broker-stage token's signature does not verify",
            Refusal::Issuer => "the broker-stage token is from another issuer",
            Refusal::Audience => "the broker-stage token is not for exactly the broker-stage audience",
            Refusal::Workload => {
                "the broker-stage token belongs to another namespace or ServiceAccount"
            }
            Refusal::Expired => "the broker-stage token has expired",
            Refusal::NotYetValid => "the broker-stage token is not valid yet",
            Refusal::Lifetime => "the broker-stage token's lifetime exceeds ten minutes",
            Refusal::PacketUnavailable => "the rotation packet cannot be read",
            Refusal::Packet => {
                "the stage names no open rotate-a-credential packet on this credential"
            }
        })
    }
}

/// One key the cluster issuer signs with.
#[derive(Clone)]
pub struct TrustedKey {
    pub kid: String,
    pub key: rsa::RsaPublicKey,
}

/// Where the trusted keys come from. Blocking: the door calls it on the
/// blocking pool. An error is a refusal, never an empty key set.
pub trait StageKeys: Send + Sync {
    fn trusted(&self) -> Result<Vec<TrustedKey>, String>;
}

/// A key set (the issuer's `/openid/v1/jwks` document) in a file, read on
/// every verification — a rotated file is the next request's truth, and
/// there is no cache here to go stale (the runner credential door's
/// posture). The error names the path and the fault, never the content.
pub struct KeySetFile(pub PathBuf);

impl StageKeys for KeySetFile {
    fn trusted(&self) -> Result<Vec<TrustedKey>, String> {
        let path = self.0.display();
        let meta = std::fs::metadata(&self.0).map_err(|e| format!("{path}: {e}"))?;
        if !meta.is_file() || meta.len() > MAX_KEY_SET_BYTES {
            return Err(format!("{path}: not a regular key-set file within bounds"));
        }
        let text = std::fs::read_to_string(&self.0).map_err(|e| format!("{path}: {e}"))?;
        let keys = parse_key_set(&text).ok_or_else(|| format!("{path}: not a key set"))?;
        if keys.is_empty() {
            return Err(format!("{path}: holds no RS256 signing key"));
        }
        Ok(keys)
    }
}

fn b64(part: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(part)
        .ok()
}

/// The RS256 signing keys of a JWKS document; `None` when it is not one.
/// A key of another type, use or algorithm is not this door's and is
/// left out — so a token naming it meets [`Refusal::UnknownKey`].
pub fn parse_key_set(text: &str) -> Option<Vec<TrustedKey>> {
    let doc: Value = serde_json::from_str(text).ok()?;
    let keys = doc.get("keys")?.as_array()?;
    let field = |k: &Value, name: &str| k.get(name).and_then(Value::as_str).map(str::to_owned);
    Some(
        keys.iter()
            .filter(|k| {
                field(k, "kty").as_deref() == Some("RSA")
                    && field(k, "alg").is_none_or(|alg| alg == "RS256")
                    && field(k, "use").is_none_or(|u| u == "sig")
            })
            .filter_map(|k| {
                let kid = field(k, "kid").filter(|kid| !kid.is_empty())?;
                let n = rsa::BigUint::from_bytes_be(&b64(&field(k, "n")?)?);
                let e = rsa::BigUint::from_bytes_be(&b64(&field(k, "e")?)?);
                Some(TrustedKey {
                    kid,
                    key: rsa::RsaPublicKey::new(n, e).ok()?,
                })
            })
            .collect(),
    )
}

/// What a verified token established — and all it established.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verified {
    pub expires_at: i64,
    pub lifetime_secs: i64,
}

/// Verify `token` against `terms` and the trusted keys at `now` (unix
/// seconds). Pure but for the key read. No claim is read before the
/// signature holds.
pub fn verify(
    token: &str,
    terms: &Terms,
    keys: &dyn StageKeys,
    now: i64,
) -> Result<Verified, Refusal> {
    let token = token.trim();
    if token.is_empty() || token.len() > MAX_TOKEN_BYTES {
        return Err(Refusal::Malformed);
    }
    let parts: Vec<&str> = token.split('.').collect();
    let [header, claims, signature] = parts.as_slice() else {
        return Err(Refusal::Malformed);
    };
    let json = |part: &str| -> Result<Value, Refusal> {
        b64(part)
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .filter(Value::is_object)
            .ok_or(Refusal::Malformed)
    };
    let header_json = json(header)?;
    if header_json.get("alg").and_then(Value::as_str) != Some("RS256") {
        return Err(Refusal::Algorithm);
    }
    let kid = header_json
        .get("kid")
        .and_then(Value::as_str)
        .ok_or(Refusal::Malformed)?;
    let trusted = keys.trusted().map_err(|why| {
        tracing::error!(%why, "the broker-stage trusted keys are unavailable; the stage is refused");
        Refusal::KeysUnavailable
    })?;
    let key = trusted
        .iter()
        .find(|k| k.kid == kid)
        .ok_or(Refusal::UnknownKey)?;
    if key.key.n().bits() < MIN_MODULUS_BITS {
        return Err(Refusal::WeakKey);
    }
    let signature = b64(signature)
        .and_then(|bytes| rsa::pkcs1v15::Signature::try_from(bytes.as_slice()).ok())
        .ok_or(Refusal::Malformed)?;
    let signed = &token[..header.len() + 1 + claims.len()];
    rsa::pkcs1v15::VerifyingKey::<rsa::sha2::Sha256>::new(key.key.clone())
        .verify(signed.as_bytes(), &signature)
        .map_err(|_| Refusal::Signature)?;

    let claims = json(claims)?;
    let text = |v: &Value, name: &str| v.get(name).and_then(Value::as_str).map(str::to_owned);
    if text(&claims, "iss").as_deref() != Some(terms.issuer.as_str()) {
        return Err(Refusal::Issuer);
    }
    // ONE audience and it is ours: a token minted for this door and for
    // the cluster's own API at once is the shared token this door exists
    // not to accept.
    let audience_holds = match claims.get("aud") {
        Some(Value::String(one)) => *one == terms.audience,
        Some(Value::Array(all)) => {
            matches!(all.as_slice(), [Value::String(one)] if *one == terms.audience)
        }
        _ => false,
    };
    if !audience_holds {
        return Err(Refusal::Audience);
    }
    let bound = claims.get("kubernetes.io").cloned().unwrap_or(Value::Null);
    let subject = format!(
        "system:serviceaccount:{}:{}",
        terms.namespace, terms.service_account
    );
    if text(&claims, "sub").as_deref() != Some(subject.as_str())
        || text(&bound, "namespace").as_deref() != Some(terms.namespace.as_str())
        || text(&bound["serviceaccount"], "name").as_deref() != Some(terms.service_account.as_str())
    {
        return Err(Refusal::Workload);
    }
    let instant = |name: &str| claims.get(name).and_then(Value::as_i64);
    let (Some(expires_at), Some(issued_at)) = (instant("exp"), instant("iat")) else {
        return Err(Refusal::Malformed);
    };
    if expires_at <= now {
        return Err(Refusal::Expired);
    }
    let not_before = match claims.get("nbf") {
        None => issued_at,
        Some(v) => v.as_i64().ok_or(Refusal::Malformed)?.max(issued_at),
    };
    if not_before > now.saturating_add(NOT_BEFORE_LEEWAY_SECS) {
        return Err(Refusal::NotYetValid);
    }
    let lifetime_secs = expires_at.saturating_sub(issued_at);
    if lifetime_secs <= 0 || lifetime_secs > MAX_LIFETIME_SECS {
        return Err(Refusal::Lifetime);
    }
    Ok(Verified {
        expires_at,
        lifetime_secs,
    })
}

/// The door's configuration: off, on, or named-broken. Never "partly on".
#[derive(Clone)]
pub enum Door {
    Off,
    Broken(String),
    On {
        terms: Terms,
        keys: Arc<dyn StageKeys>,
    },
}

impl Door {
    pub fn from_env() -> Self {
        Self::from_vars(|name| std::env::var(name).ok())
    }

    /// All five variables, or none. An audience equal to the issuer is
    /// the cluster's own default audience — the token every pod already
    /// holds — so it is refused as not distinct.
    pub fn from_vars(get: impl Fn(&str) -> Option<String>) -> Self {
        let values: Vec<Option<String>> = VARS
            .iter()
            .map(|name| {
                get(name)
                    .map(|v| v.trim().to_owned())
                    .filter(|v| !v.is_empty())
            })
            .collect();
        if values.iter().all(Option::is_none) {
            return Door::Off;
        }
        let missing: Vec<&str> = VARS
            .iter()
            .zip(&values)
            .filter(|(_, v)| v.is_none())
            .map(|(name, _)| *name)
            .collect();
        let [
            Some(issuer),
            Some(audience),
            Some(namespace),
            Some(service_account),
            Some(key_set),
        ] = values.as_slice()
        else {
            return Door::Broken(format!("{} unset", missing.join(", ")));
        };
        if audience == issuer {
            return Door::Broken(format!(
                "{AUDIENCE_ENV} is the issuer itself, which is the cluster's own audience and not a distinct one"
            ));
        }
        Door::On {
            terms: Terms {
                issuer: issuer.clone(),
                audience: audience.clone(),
                namespace: namespace.clone(),
                service_account: service_account.clone(),
            },
            keys: Arc::new(KeySetFile(PathBuf::from(key_set))),
        }
    }

    /// One line for the boot log: the state and the terms, never a key.
    pub fn describe(&self) -> String {
        match self {
            Door::Off => {
                "off (unconfigured): runner credential stages are recorded as before".into()
            }
            Door::Broken(why) => {
                format!("REFUSING runner credential stages — misconfigured: {why}")
            }
            Door::On { terms, .. } => format!(
                "on: issuer {}, audience {}, workload {}/{}, lifetime at most {MAX_LIFETIME_SECS}s",
                terms.issuer, terms.audience, terms.namespace, terms.service_account
            ),
        }
    }
}

/// What the middleware made of the header, for the rotation door to judge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Presented {
    Verified(Verified),
    Refused(Refusal),
}

/// The middleware: take [`HEADER`] off the request — so nothing
/// downstream holds the token — verify it, and leave the answer as a
/// request extension. It refuses nothing itself: only the rotation door,
/// for a runner row, acts on the answer, so no other route changes.
pub async fn present(State(door): State<Arc<Door>>, mut req: Request, next: Next) -> Response {
    let tokens: Vec<Option<String>> = req
        .headers()
        .get_all(HEADER)
        .iter()
        .map(|v| v.to_str().ok().map(str::to_owned))
        .collect();
    req.headers_mut().remove(HEADER);
    let presented = match (tokens.as_slice(), door.as_ref()) {
        ([], _) => return next.run(req).await,
        ([_, _, ..], _) => Presented::Refused(Refusal::MultiplePresented),
        ([None], _) => Presented::Refused(Refusal::Malformed),
        ([Some(_)], Door::Off) => Presented::Refused(Refusal::NotConfigured),
        ([Some(_)], Door::Broken(why)) => Presented::Refused(Refusal::Misconfigured(why.clone())),
        ([Some(token)], Door::On { terms, keys }) => {
            let (token, terms, keys) = (token.clone(), terms.clone(), keys.clone());
            // The ONE real clock, on purpose: `exp` was signed by a real
            // cluster against real time, and the deploy-mode clock answers
            // what day it is in the company's timeline — a simulated day
            // must neither revive an expired token nor expire a live one.
            let now = boss_clock_client::wall_now().timestamp();
            let audience = terms.audience.clone();
            match tokio::task::spawn_blocking(move || verify(&token, &terms, keys.as_ref(), now))
                .await
            {
                Ok(Ok(verified)) => {
                    tracing::info!(
                        %audience,
                        expires_at = verified.expires_at,
                        lifetime_secs = verified.lifetime_secs,
                        "a broker-stage token verified"
                    );
                    Presented::Verified(verified)
                }
                Ok(Err(refusal)) => Presented::Refused(refusal),
                Err(e) => {
                    tracing::error!(error = %e, "broker-stage verification did not finish; the token is refused");
                    Presented::Refused(Refusal::KeysUnavailable)
                }
            }
        }
    };
    if let Presented::Refused(why) = &presented {
        tracing::warn!(path = %req.uri().path(), %why, "a presented broker-stage token was not believed");
    }
    req.extensions_mut().insert(presented);
    next.run(req).await
}

/// Mount the presenting layer around every route `app` holds.
pub fn mount(app: axum::Router, door: Arc<Door>) -> axum::Router {
    app.layer(axum::middleware::from_fn_with_state(door, present))
}

/// The rotation door's question, for a row of `kind`: `Ok(None)` records
/// as before (not a runner row, or the door is off and nothing was
/// presented); `Ok(Some(_))` is a verified stage; `Err` is the refusal.
pub fn judge(
    door: &Door,
    kind: &str,
    presented: Option<&Presented>,
) -> Result<Option<Verified>, Refusal> {
    if kind != RUNNER_KIND {
        return Ok(None);
    }
    match (presented, door) {
        (Some(Presented::Refused(why)), _) => Err(why.clone()),
        (Some(Presented::Verified(verified)), Door::On { .. }) => Ok(Some(verified.clone())),
        (Some(Presented::Verified(_)), _) => Err(Refusal::NotConfigured),
        (None, Door::Off) => Ok(None),
        (None, Door::Broken(why)) => Err(Refusal::Misconfigured(why.clone())),
        (None, Door::On { .. }) => Err(Refusal::NonePresented),
    }
}

/// What the rotation door needs to know of the packet a stage names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagePacket {
    pub kind: String,
    pub subject_id: String,
    pub open: bool,
}

/// The read of that packet — the jobs repository, behind a port so the
/// credentials door owns no job storage.
#[async_trait::async_trait]
pub trait StagePackets: Send + Sync {
    async fn packet(&self, id: uuid::Uuid) -> Result<Option<StagePacket>, String>;
}

/// [`StagePackets`] over the jobs repository — the jobs API's own adapter.
pub struct JobsStagePackets(pub Arc<dyn crate::port::JobsRepository>);

#[async_trait::async_trait]
impl StagePackets for JobsStagePackets {
    async fn packet(&self, id: uuid::Uuid) -> Result<Option<StagePacket>, String> {
        let job = self
            .0
            .get_job(&boss_core::job::JobId::from_uuid(id))
            .await
            .map_err(|e| e.to_string())?;
        Ok(job.map(|job| StagePacket {
            open: job.status == boss_core::job::JobStatus::Open,
            kind: job.kind,
            subject_id: job.subject.id,
        }))
    }
}

/// A verified stage belongs to the open rotation packet on THIS
/// credential, named by the command's own `job_id`. No reader, an
/// unreadable packet, another protocol, another credential's packet, a
/// closed one: each is a refusal.
pub async fn bind_packet(
    packets: Option<&dyn StagePackets>,
    credential_id: &str,
    evidence: &Value,
) -> Result<(), Refusal> {
    let packets = packets.ok_or(Refusal::PacketUnavailable)?;
    let job_id = evidence
        .get("job_id")
        .and_then(Value::as_str)
        .and_then(|id| uuid::Uuid::parse_str(id).ok())
        .ok_or(Refusal::Packet)?;
    let packet = packets.packet(job_id).await.map_err(|why| {
        tracing::error!(%job_id, %why, "the rotation packet of a verified stage cannot be read");
        Refusal::PacketUnavailable
    })?;
    match packet {
        Some(p) if p.kind == ROTATION_KIND && p.subject_id == credential_id && p.open => Ok(()),
        _ => Err(Refusal::Packet),
    }
}

#[cfg(test)]
pub(crate) mod tests;
