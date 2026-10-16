//! Accepted001 private collector TLS lifecycle. This inert port boundary
//! neither registers a rule nor installs grants, a listener or a Secret.
use super::credential_issuer::{SecretData, SecretStore, WriteAt};
use async_trait::async_trait;
use chrono::{DateTime, Utc};

pub const NAMESPACE: &str = "boss";
pub const SECRET: &str = "boss-managed-cache-server-tls";
pub const HOST: &str = "boss-managed-cache-collection.boss.svc";
pub const OVERLAP_SECONDS: i64 = 600;
pub const CERTIFICATE_SECONDS: i64 = 86_400;
/// At the audit bound the lifecycle refuses; it never forgets a generation.
pub const MAX_RETIRED_GENERATIONS: usize = 1024;

#[path = "managed_collector_tls/installation.rs"]
pub mod installation;
#[path = "managed_collector_tls/observer.rs"]
pub mod observer;

/// Installation identity and request reservations live outside the replaceable
/// Secret. A plain SecretStore cannot run this lifecycle.
#[async_trait]
pub trait BoundStore: SecretStore {
    async fn original_uid(&self) -> Result<String, Refused>;
    async fn reserve(&self, request: uuid::Uuid) -> Result<bool, Refused>;
    async fn reserved(&self, request: uuid::Uuid) -> Result<bool, Refused>;
    async fn attest_effect(&self, effect: &Effect) -> Result<(), Refused>;
}

/// Private material has no serialization or value-bearing Debug implementation.
pub struct Certificate {
    key: String,
    pem: String,
    trust: String,
    expires: DateTime<Utc>,
}
impl std::fmt::Debug for Certificate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Certificate")
            .field("expires", &self.expires)
            .finish_non_exhaustive()
    }
}
#[async_trait]
pub trait Issuer: Send + Sync {
    async fn mint(&self, now: DateTime<Utc>) -> Result<Certificate, Refused>;
    fn validate(&self, certificate: &Certificate, now: DateTime<Utc>) -> Result<(), Refused>;
}

/// Returned only by the protected installer after independent native reads.
/// Exact requirements are carried in the adjacent adapter contract.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Effect {
    pub secret_uid: String,
    pub secret_version: String,
    pub generation: uuid::Uuid,
    pub certificate_sha256: String,
    pub observed_at: DateTime<Utc>,
    pub installed_trust_generation: uuid::Uuid,
    pub public_trust_sha256: String,
    pub public_trust_uid: String,
    pub retired_generation: Option<uuid::Uuid>,
    pub old_collectors_absent: bool,
    pub last_old_token_expiry: Option<DateTime<Utc>>,
}
#[async_trait]
pub trait ProtectedInstallation: Send + Sync {
    /// Broker-local clock sampled after observe returns, not a remote or
    /// request-supplied timestamp. It fences the complete native operation.
    fn completed_at(&self) -> DateTime<Utc>;
    async fn observe(
        &self,
        observed: &SecretData,
        generation: uuid::Uuid,
        public_certificate: &str,
    ) -> Result<Effect, Refused>;
}
#[derive(Debug, PartialEq, Eq)]
pub enum Progress {
    Staged {
        generation: uuid::Uuid,
        public_certificate: String,
    },
    Promoted,
    Revoked,
    Moved,
}
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("private collector TLS lifecycle refused")]
pub struct Refused;

pub async fn prepare(
    store: &dyn BoundStore,
    issuer: &dyn Issuer,
    request: uuid::Uuid,
    now: DateTime<Utc>,
) -> Result<Progress, Refused> {
    if request.is_nil() {
        return Err(Refused);
    }
    let observed = read(store).await?;
    if retired(&observed)?.contains(&request) {
        return Err(Refused);
    }
    let next = slot(&observed, "next")?;
    let current = slot(&observed, "current")?;
    if let Some(held) = next
        .as_ref()
        .or(current.as_ref().filter(|s| s.generation == request))
    {
        if held.generation != request {
            return Err(Refused);
        }
        if !store.reserved(request).await? {
            return Err(Refused);
        }
        validate(issuer, held, now)?;
        return Ok(held.progress());
    }
    if slot(&observed, "previous")?.is_some() {
        return Err(Refused);
    }
    if let Some(current) = current {
        validate(issuer, &current, now)?;
    }
    // Intent is durable before minting. An interrupted attempt with no retained
    // slot is uncertain, not permission to mint a different pair on retry.
    if !store.reserve(request).await? {
        return Err(Refused);
    }
    let pair = issuer.mint(now).await.map_err(|_| Refused)?;
    let next = Slot {
        secret_uid: observed.uid.clone(),
        generation: request,
        key: pair.key,
        pem: pair.pem,
        trust: pair.trust,
        expires: pair.expires,
    };
    validate(issuer, &next, now)?;
    let mut expected = observed.clone();
    expected.data.insert(
        "tls.next".into(),
        serde_json::to_string(&next).map_err(|_| Refused)?,
    );
    match write(store, &observed, &expected).await? {
        WriteAt::Moved => Ok(Progress::Moved),
        WriteAt::Written => Ok(next.progress()),
    }
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Slot {
    // Persist the original object identity through committed-acknowledgement loss.
    secret_uid: String,
    generation: uuid::Uuid,
    key: String,
    pem: String,
    trust: String,
    expires: DateTime<Utc>,
}
impl Slot {
    fn certificate(&self) -> Certificate {
        Certificate {
            key: self.key.clone(),
            pem: self.pem.clone(),
            trust: self.trust.clone(),
            expires: self.expires,
        }
    }
    fn progress(&self) -> Progress {
        Progress::Staged {
            generation: self.generation,
            public_certificate: self.trust.clone(),
        }
    }
}
fn slot(s: &SecretData, name: &str) -> Result<Option<Slot>, Refused> {
    match s.data.get(&format!("tls.{name}")).filter(|v| !v.is_empty()) {
        Some(text) => {
            let value: Slot = serde_json::from_str(text).map_err(|_| Refused)?;
            if value.secret_uid != s.uid
                || value.generation.is_nil()
                || value.key.is_empty()
                || value.pem.is_empty()
                || value.trust.is_empty()
            {
                return Err(Refused);
            }
            Ok(Some(value))
        }
        None => Ok(None),
    }
}
fn validate(issuer: &dyn Issuer, slot: &Slot, now: DateTime<Utc>) -> Result<(), Refused> {
    if slot.expires <= now || slot.expires > now + chrono::Duration::seconds(CERTIFICATE_SECONDS) {
        return Err(Refused);
    }
    issuer
        .validate(&slot.certificate(), now)
        .map_err(|_| Refused)
}
async fn read(store: &dyn BoundStore) -> Result<SecretData, Refused> {
    let original_uid = store.original_uid().await?;
    let observed = store
        .read_secret(NAMESPACE, SECRET)
        .await
        .map_err(|_| Refused)?
        .ok_or(Refused)?;
    if observed.uid != original_uid
        || uuid::Uuid::parse_str(&observed.uid)
            .map_err(|_| Refused)?
            .is_nil()
        || observed.version.is_empty()
        || observed.data.keys().any(|key| {
            !matches!(
                key.as_str(),
                "tls.next"
                    | "tls.current"
                    | "tls.previous"
                    | "tls.promoted-at"
                    | "tls.effect"
                    | "tls.retired"
            )
        })
    {
        return Err(Refused);
    }
    let past = retired(&observed)?;
    let mut active = std::collections::BTreeSet::new();
    for name in ["next", "current", "previous"] {
        if let Some(slot) = slot(&observed, name)?
            && (past.contains(&slot.generation)
                || !active.insert(slot.generation)
                || !store.reserved(slot.generation).await?)
        {
            return Err(Refused);
        }
    }
    if slot(&observed, "current")?.is_none()
        && (!past.is_empty() || slot(&observed, "previous")?.is_some())
    {
        return Err(Refused);
    }
    Ok(observed)
}
fn retired(s: &SecretData) -> Result<std::collections::BTreeSet<uuid::Uuid>, Refused> {
    let values: Vec<uuid::Uuid> = match s.data.get("tls.retired").filter(|v| !v.is_empty()) {
        Some(text) => serde_json::from_str(text).map_err(|_| Refused)?,
        None => Vec::new(),
    };
    let unique: std::collections::BTreeSet<_> = values.iter().copied().collect();
    if values.len() > MAX_RETIRED_GENERATIONS
        || values.len() != unique.len()
        || unique.iter().any(uuid::Uuid::is_nil)
    {
        return Err(Refused);
    }
    Ok(unique)
}
async fn write(
    store: &dyn BoundStore,
    observed: &SecretData,
    expected: &SecretData,
) -> Result<WriteAt, Refused> {
    let entries: Vec<_> = expected
        .data
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let result = store
        .write_keys_if(NAMESPACE, SECRET, &entries, observed)
        .await
        .map_err(|_| Refused)?;
    if result == WriteAt::Written {
        let actual = read(store).await?;
        if actual.uid != observed.uid
            || actual.version == observed.version
            || actual.data != expected.data
        {
            return Err(Refused);
        }
    }
    Ok(result)
}
pub async fn promote(
    store: &dyn BoundStore,
    issuer: &dyn Issuer,
    installer: &dyn ProtectedInstallation,
    request: uuid::Uuid,
    now: DateTime<Utc>,
) -> Result<Progress, Refused> {
    if request.is_nil() {
        return Err(Refused);
    }
    let observed = read(store).await?;
    let current = slot(&observed, "current")?;
    let next = slot(&observed, "next")?;
    let selected = next
        .as_ref()
        .or(current.as_ref().filter(|s| s.generation == request))
        .ok_or(Refused)?;
    if selected.generation != request {
        return Err(Refused);
    }
    validate(issuer, selected, now)?;
    let effect = effect(installer, &observed, selected, now).await?;
    store.attest_effect(&effect).await?;
    if read(store).await? != observed {
        return Err(Refused);
    }
    if next.is_none() {
        return Ok(Progress::Promoted);
    }
    if slot(&observed, "previous")?.is_some() {
        return Err(Refused);
    }
    if current.as_ref().is_some_and(|old| {
        old.generation == request || old.pem == selected.pem || old.trust == selected.trust
    }) {
        return Err(Refused);
    }
    let mut expected = observed.clone();
    expected.data.insert(
        "tls.current".into(),
        serde_json::to_string(selected).map_err(|_| Refused)?,
    );
    expected.data.insert("tls.next".into(), String::new());
    expected.data.insert(
        "tls.previous".into(),
        current
            .map(|old| serde_json::to_string(&old))
            .transpose()
            .map_err(|_| Refused)?
            .unwrap_or_default(),
    );
    expected
        .data
        .insert("tls.promoted-at".into(), now.to_rfc3339());
    expected.data.insert(
        "tls.effect".into(),
        serde_json::to_string(&effect).map_err(|_| Refused)?,
    );
    match write(store, &observed, &expected).await? {
        WriteAt::Written => Ok(Progress::Promoted),
        WriteAt::Moved => Ok(Progress::Moved),
    }
}
async fn effect(
    installer: &dyn ProtectedInstallation,
    observed: &SecretData,
    selected: &Slot,
    now: DateTime<Utc>,
) -> Result<Effect, Refused> {
    use rsa::sha2::{Digest, Sha256};
    let effect = installer
        .observe(observed, selected.generation, &selected.pem)
        .await
        .map_err(|_| Refused)?;
    let completed = installer.completed_at();
    if effect.secret_uid != observed.uid
        || effect.secret_version != observed.version
        || effect.generation != selected.generation
        || effect.installed_trust_generation != selected.generation
        || effect.public_trust_sha256 != format!("{:x}", Sha256::digest(selected.trust.as_bytes()))
        || uuid::Uuid::parse_str(&effect.public_trust_uid).is_err()
        || uuid::Uuid::parse_str(&effect.public_trust_uid).is_ok_and(|id| id.is_nil())
        || effect.certificate_sha256 != format!("{:x}", Sha256::digest(selected.pem.as_bytes()))
        || completed < now
        || completed - now > chrono::Duration::seconds(60)
        || effect.observed_at > completed
        || effect.observed_at < now
        || selected.expires <= completed
    {
        return Err(Refused);
    }
    Ok(effect)
}
pub async fn revoke(
    store: &dyn BoundStore,
    issuer: &dyn Issuer,
    installer: &dyn ProtectedInstallation,
    request: uuid::Uuid,
    now: DateTime<Utc>,
) -> Result<Progress, Refused> {
    if request.is_nil() {
        return Err(Refused);
    }
    let observed = read(store).await?;
    if slot(&observed, "next")?.is_some() {
        return Err(Refused);
    }
    let current = slot(&observed, "current")?.ok_or(Refused)?;
    if current.generation != request {
        return Err(Refused);
    }
    validate(issuer, &current, now)?;
    let effect = effect(installer, &observed, &current, now).await?;
    store.attest_effect(&effect).await?;
    if read(store).await? != observed {
        return Err(Refused);
    }
    let Some(previous) = slot(&observed, "previous")? else {
        return Ok(Progress::Revoked);
    };
    let promoted =
        DateTime::parse_from_rfc3339(observed.data.get("tls.promoted-at").ok_or(Refused)?)
            .map_err(|_| Refused)?
            .with_timezone(&Utc);
    if promoted > now
        || now - promoted < chrono::Duration::seconds(OVERLAP_SECONDS)
        || effect.retired_generation != Some(previous.generation)
        || !effect.old_collectors_absent
        || effect
            .last_old_token_expiry
            .is_none_or(|expiry| expiry > now)
    {
        return Err(Refused);
    }
    let mut expected = observed.clone();
    expected.data.insert("tls.previous".into(), String::new());
    let mut history = retired(&observed)?;
    if history.len() == MAX_RETIRED_GENERATIONS || !history.insert(previous.generation) {
        return Err(Refused);
    }
    expected.data.insert(
        "tls.retired".into(),
        serde_json::to_string(&history).map_err(|_| Refused)?,
    );
    expected.data.insert(
        "tls.effect".into(),
        serde_json::to_string(&effect).map_err(|_| Refused)?,
    );
    match write(store, &observed, &expected).await? {
        WriteAt::Written => Ok(Progress::Revoked),
        WriteAt::Moved => Ok(Progress::Moved),
    }
}

pub struct OpenSslIssuer;
#[path = "managed_collector_tls/issuer.rs"]
mod issuer;

#[cfg(test)]
#[path = "managed_collector_tls/tests.rs"]
mod tests;
