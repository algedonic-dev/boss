//! Protected ports carry whole originals. Nothing installs or grants these readers.
use super::*;
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;
#[path = "retirement.rs"]
mod retirement;

#[derive(Clone, Serialize, PartialEq, Eq)]
pub enum Target {
    Secret,
    BrokerPod(String),
    PublicTrust,
    CollectorJob(String),
    CollectorPod(String),
}

/// Private evidence: deliberately no value-bearing Debug implementation.
#[derive(Clone, Serialize)]
pub struct NativeReply {
    pub target: Target,
    pub status: Option<u16>,
    pub headers: Vec<(String, Vec<u8>)>,
    #[serde(serialize_with = "bytes_base64")]
    pub body: Vec<u8>,
    pub error: Option<String>,
}
fn bytes_base64<S: serde::Serializer>(value: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
    use base64::Engine;
    serializer.serialize_str(&base64::engine::general_purpose::STANDARD.encode(value))
}
#[async_trait]
pub trait NativeReads: Send + Sync {
    async fn read(&self, target: Target) -> Result<NativeReply, Refused>;
}

#[derive(Serialize)]
pub struct CurrentTrustObservation {
    pub invocation: uuid::Uuid,
    pub observation: uuid::Uuid,
    pub observed_at: DateTime<Utc>,
    pub native: NativeReply,
}
#[async_trait]
pub trait CurrentTrustReader: Send + Sync {
    /// A fresh protected producer read, not a retained desired TrustPlan.
    async fn observe(&self, invocation: uuid::Uuid) -> Result<CurrentTrustObservation, Refused>;
}

#[derive(Serialize)]
pub struct CensusObservation {
    pub invocation: uuid::Uuid,
    pub observed_at: DateTime<Utc>,
    /// Complete immutable generation-closure record from the protected
    /// registered-launch producer, never a label query or caller boolean.
    pub original: Value,
}
#[async_trait]
pub trait RegisteredCensus: Send + Sync {
    async fn observe(
        &self,
        invocation: uuid::Uuid,
        generation: uuid::Uuid,
    ) -> Result<CensusObservation, Refused>;
}

#[derive(Serialize)]
pub struct TlsObservation {
    pub started_at: DateTime<Utc>,
    pub completed_at: DateTime<Utc>,
    pub peer_der: Vec<u8>,
    pub peer_pem: Vec<u8>,
    pub error: Option<String>,
}
impl TlsObservation {
    pub(super) fn from_peer(started_at: DateTime<Utc>, result: Result<Vec<u8>, String>) -> Self {
        match result {
            Ok(peer_pem) => {
                let decoded =
                    openssl::x509::X509::from_pem(&peer_pem).and_then(|cert| cert.to_der());
                match decoded {
                    Ok(peer_der) => Self {
                        started_at,
                        completed_at: wall_time(),
                        peer_der,
                        peer_pem,
                        error: None,
                    },
                    Err(error) => Self {
                        started_at,
                        completed_at: wall_time(),
                        peer_der: Vec::new(),
                        peer_pem,
                        error: Some(error.to_string()),
                    },
                }
            }
            Err(error) => Self {
                started_at,
                completed_at: wall_time(),
                peer_der: Vec::new(),
                peer_pem: Vec::new(),
                error: Some(error),
            },
        }
    }
}
#[async_trait]
pub(super) trait LeafReader: Send + Sync {
    async fn observe(&self, trust: &str) -> TlsObservation;
}
#[async_trait]
impl LeafReader for ServedLeaf {
    async fn observe(&self, trust: &str) -> TlsObservation {
        let started_at = wall_time();
        TlsObservation::from_peer(started_at, self.original(trust).await)
    }
}

pub(super) trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}
struct LocalClock;
// These private observations judge a live certificate/token and the actual
// adapter invocation, never a firing's historical time or an audit row stamp.
pub(super) fn wall_time() -> DateTime<Utc> {
    boss_clock_client::wall_now()
}
impl Clock for LocalClock {
    fn now(&self) -> DateTime<Utc> {
        wall_time()
    }
}

pub struct ProtectedObserver {
    pub(super) store: Arc<installation::FileBoundStore>,
    pub(super) native: Arc<dyn NativeReads>,
    pub(super) trust: Option<Arc<dyn CurrentTrustReader>>,
    pub(super) census: Option<Arc<dyn RegisteredCensus>>,
    pub(super) leaf: Arc<dyn LeafReader>,
    pub(super) clock: Arc<dyn Clock>,
}
fn hash(bytes: &[u8]) -> String {
    use rsa::sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}
fn native_value(reply: &NativeReply) -> Result<Value, Refused> {
    if reply.status != Some(200) || reply.error.is_some() {
        return Err(Refused);
    }
    serde_json::from_slice(&reply.body).map_err(|_| Refused)
}
fn same_secret(reply: &NativeReply, observed: &SecretData) -> Result<(), Refused> {
    use base64::Engine;
    let value = native_value(reply)?;
    let expected = observed
        .data
        .iter()
        .map(|(key, value)| {
            (
                key.clone(),
                Value::String(base64::engine::general_purpose::STANDARD.encode(value)),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    if !matches!(reply.target, Target::Secret)
        || value["kind"] != "Secret"
        || value["metadata"]["namespace"] != NAMESPACE
        || value["metadata"]["name"] != SECRET
        || value["metadata"]["uid"] != observed.uid
        || value["metadata"]["resourceVersion"] != observed.version
        || value
            .pointer("/metadata/deletionTimestamp")
            .is_some_and(|v| !v.is_null())
        || value.get("stringData").is_some()
        || value["data"] != Value::Object(expected)
    {
        return Err(Refused);
    }
    Ok(())
}
struct Capture<'a> {
    observer: &'a ProtectedObserver,
    invocation: uuid::Uuid,
    started: DateTime<Utc>,
    originals: Vec<Value>,
    installation: Option<uuid::Uuid>,
    stage: &'static str,
}
impl Capture<'_> {
    async fn retain(&mut self, kind: &str, value: &impl Serialize) -> Result<(), Refused> {
        let bytes = serde_json::to_vec(value).map_err(|_| Refused)?;
        let name = format!(
            "tls-observation-{}-{}.json",
            self.invocation,
            self.originals.len()
        );
        let reference =
            serde_json::json!({"kind":kind,"file":name,"bytes":bytes.len(),"sha256":hash(&bytes)});
        self.observer
            .store
            .filesystem(move |root, owner| {
                installation::retain(root, &name, &bytes, owner)?;
                if installation::read(root, &name, owner, 256 * 1024)? != bytes {
                    return Err(Refused);
                }
                Ok(())
            })
            .await?;
        self.originals.push(reference);
        Ok(())
    }
    async fn native(&mut self, target: Target) -> Result<NativeReply, Refused> {
        let reply = self.observer.native.read(target.clone()).await?;
        self.retain("native-http", &reply).await?;
        if reply.target != target {
            return Err(Refused);
        }
        Ok(reply)
    }
    async fn current_trust(
        &mut self,
        plan: &installation::TrustPlan,
    ) -> Result<CurrentTrustObservation, Refused> {
        let current = self
            .observer
            .trust
            .as_ref()
            .ok_or(Refused)?
            .observe(self.invocation)
            .await?;
        self.retain("current-trust", &current).await?;
        if current.invocation != self.invocation
            || current.observation.is_nil()
            || current.observed_at < self.started
            || current.observed_at > self.observer.clock.now()
            || !matches!(current.native.target, Target::PublicTrust)
            || native_value(&current.native)? != plan.original
        {
            return Err(Refused);
        }
        Ok(current)
    }
    async fn measure(
        &mut self,
        observed: &SecretData,
        generation: uuid::Uuid,
        certificate: &str,
    ) -> Result<Effect, Refused> {
        let original = self.observer.store.original().await?;
        self.installation = Some(original.installation);
        let uid = original.uid()?;
        if uid != observed.uid
            || generation.is_nil()
            || !self.observer.store.reserved(generation).await?
        {
            return Err(Refused);
        }
        let selected = slot(observed, "next")?
            .or(slot(observed, "current")?.filter(|s| s.generation == generation))
            .ok_or(Refused)?;
        if selected.generation != generation || selected.pem != certificate {
            return Err(Refused);
        }
        validate(&OpenSslIssuer, &selected, self.started)?;
        self.stage = "original-pod-before";
        let name = installation::field(&original.pod, "/metadata/name")?;
        let pod = self.native(Target::BrokerPod(name.into())).await?;
        if installation::pod_identity(&native_value(&pod)?, &original.container)?
            != installation::pod_identity(&original.pod, &original.container)?
        {
            return Err(Refused);
        }
        self.stage = "original-secret-before";
        same_secret(&self.native(Target::Secret).await?, observed)?;
        if self
            .observer
            .store
            .read_secret(NAMESPACE, SECRET)
            .await
            .map_err(|_| Refused)?
            .as_ref()
            != Some(observed)
        {
            return Err(Refused);
        }
        self.stage = "desired-trust-plan";
        let plan = self.observer.store.trust_plan(generation).await?;
        if plan.installation != original.installation
            || plan.generation != generation
            || plan.public_trust_sha256 != hash(selected.trust.as_bytes())
        {
            return Err(Refused);
        }
        self.stage = "current-trust-before";
        let before = self.current_trust(&plan).await?;
        self.stage = "served-leaf";
        let leaf = self.observer.leaf.observe(&selected.trust).await;
        self.retain("tls", &leaf).await?;
        if leaf.error.is_some()
            || leaf.started_at < self.started
            || leaf.completed_at < leaf.started_at
            || leaf.completed_at > self.observer.clock.now()
            || leaf.peer_pem != selected.pem.as_bytes()
            || leaf.peer_der
                != openssl::x509::X509::from_pem(&leaf.peer_pem)
                    .map_err(|_| Refused)?
                    .to_der()
                    .map_err(|_| Refused)?
        {
            return Err(Refused);
        }
        self.stage = "previous-generation-closure";
        let previous = slot(observed, "previous")?;
        let expiry = if let Some(previous) = &previous {
            Some(self.retirement(&original, previous.generation).await?)
        } else {
            None
        };
        self.stage = "current-trust-after";
        let after = self.current_trust(&plan).await?;
        if before.observation == after.observation || after.observed_at < before.observed_at {
            return Err(Refused);
        }
        self.stage = "original-pod-after";
        let again = self.native(Target::BrokerPod(name.into())).await?;
        if installation::pod_identity(&native_value(&again)?, &original.container)?
            != installation::pod_identity(&original.pod, &original.container)?
        {
            return Err(Refused);
        }
        self.stage = "original-secret-after";
        same_secret(&self.native(Target::Secret).await?, observed)?;
        if self
            .observer
            .store
            .read_secret(NAMESPACE, SECRET)
            .await
            .map_err(|_| Refused)?
            .as_ref()
            != Some(observed)
        {
            return Err(Refused);
        }
        Ok(Effect {
            secret_uid: uid,
            secret_version: observed.version.clone(),
            generation,
            certificate_sha256: hash(&leaf.peer_pem),
            observed_at: self.observer.clock.now(),
            installed_trust_generation: generation,
            public_trust_sha256: plan.public_trust_sha256,
            public_trust_uid: installation::field(&plan.original, "/metadata/uid")?.into(),
            retired_generation: previous.map(|p| p.generation),
            old_collectors_absent: expiry.is_some(),
            last_old_token_expiry: expiry,
        })
    }
}
impl ProtectedObserver {
    pub fn new(
        store: Arc<installation::FileBoundStore>,
        native: Arc<dyn NativeReads>,
        trust: Option<Arc<dyn CurrentTrustReader>>,
        census: Option<Arc<dyn RegisteredCensus>>,
    ) -> Self {
        Self {
            store,
            native,
            trust,
            census,
            leaf: Arc::new(ServedLeaf),
            clock: Arc::new(LocalClock),
        }
    }
}
#[async_trait]
impl ProtectedInstallation for ProtectedObserver {
    fn completed_at(&self) -> DateTime<Utc> {
        self.clock.now()
    }
    async fn observe(
        &self,
        observed: &SecretData,
        generation: uuid::Uuid,
        public_certificate: &str,
    ) -> Result<Effect, Refused> {
        let mut capture = Capture {
            observer: self,
            invocation: uuid::Uuid::new_v4(),
            started: self.clock.now(),
            originals: Vec::new(),
            installation: None,
            stage: "original-binding",
        };
        let mut result = match tokio::time::timeout(
            std::time::Duration::from_secs(60),
            capture.measure(observed, generation, public_certificate),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => {
                capture.stage = "observation-deadline";
                Err(Refused)
            }
        };
        let completed = self.clock.now();
        if completed < capture.started
            || completed - capture.started > chrono::Duration::seconds(60)
        {
            capture.stage = "observation-clock-window";
            result = Err(Refused);
        }
        let manifest = serde_json::json!({"schema":1,"invocation":capture.invocation,"installation":capture.installation,"original_binding":"tls-installation.json","started_at":capture.started,"completed_at":completed,"originals":capture.originals,"qualified":result.is_ok(),"failure_stage":if result.is_err(){Some(capture.stage)}else{None},"effect":result.as_ref().ok()});
        let bytes = serde_json::to_vec(&manifest).map_err(|_| Refused)?;
        let name = format!("tls-observation-{}.json", capture.invocation);
        self.store
            .filesystem(move |root, owner| {
                installation::retain(root, &name, &bytes, owner)?;
                if installation::read(root, &name, owner, 256 * 1024)? != bytes {
                    return Err(Refused);
                }
                Ok(())
            })
            .await?;
        result
    }
}
