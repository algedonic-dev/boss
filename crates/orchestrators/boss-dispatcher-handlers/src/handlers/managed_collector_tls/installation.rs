//! Protected originals outside the replaceable TLS Secret. Nothing here is
//! installed or wired to a dispatcher rule by constructing this source.
use super::*;
use rustix::fs::{Mode, OFlags, ResolveFlags, openat, openat2};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs::File,
    io::{Read, Write},
    os::unix::fs::MetadataExt,
    path::Path,
    sync::Arc,
};

const ORIGINAL: &str = "tls-installation.json";
const LIMIT: u64 = 256 * 1024;

/// Supplied by the protected installer from its original create/read receipts,
/// never by an enrollment, collection or lifecycle HTTP request.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Original {
    pub installation: uuid::Uuid,
    pub secret_creation: Value,
    pub pod: Value,
    pub container: String,
}
impl Original {
    pub(super) fn uid(&self) -> Result<String, Refused> {
        if self.installation.is_nil()
            || self.secret_creation.get("kind").and_then(Value::as_str) != Some("Secret")
            || self
                .secret_creation
                .pointer("/metadata/namespace")
                .and_then(Value::as_str)
                != Some(NAMESPACE)
            || self
                .secret_creation
                .pointer("/metadata/name")
                .and_then(Value::as_str)
                != Some(SECRET)
            || self
                .secret_creation
                .get("data")
                .is_some_and(|d| !d.as_object().is_some_and(|v| v.is_empty()))
            || self.secret_creation.get("stringData").is_some()
        {
            return Err(Refused);
        }
        let uid = field(&self.secret_creation, "/metadata/uid")?;
        if uuid::Uuid::parse_str(uid).map_err(|_| Refused)?.is_nil() {
            return Err(Refused);
        }
        field(&self.secret_creation, "/metadata/resourceVersion")?;
        pod_identity(&self.pod, &self.container)?;
        Ok(uid.into())
    }
}
pub(super) fn field<'a>(value: &'a Value, path: &str) -> Result<&'a str, Refused> {
    value
        .pointer(path)
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .ok_or(Refused)
}
pub(super) fn pod_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 253
        && name.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}
pub(super) fn pod_identity(pod: &Value, container: &str) -> Result<Value, Refused> {
    if !pod_name(field(pod, "/metadata/name")?) {
        return Err(Refused);
    }
    if pod.get("kind").and_then(Value::as_str) != Some("Pod")
        || pod.pointer("/metadata/namespace").and_then(Value::as_str) != Some(NAMESPACE)
        || pod
            .pointer("/metadata/deletionTimestamp")
            .is_some_and(|v| !v.is_null())
        || pod
            .pointer("/spec/serviceAccountName")
            .and_then(Value::as_str)
            != Some("boss")
        || pod.pointer("/status/phase").and_then(Value::as_str) != Some("Running")
        || !pod
            .pointer("/status/conditions")
            .and_then(Value::as_array)
            .is_some_and(|rows| {
                rows.iter()
                    .any(|r| r["type"] == "Ready" && r["status"] == "True")
            })
    {
        return Err(Refused);
    }
    let uid = field(pod, "/metadata/uid")?;
    if uuid::Uuid::parse_str(uid).map_err(|_| Refused)?.is_nil() {
        return Err(Refused);
    }
    let containers = pod
        .pointer("/spec/containers")
        .and_then(Value::as_array)
        .ok_or(Refused)?;
    let chosen: Vec<_> = containers
        .iter()
        .filter(|c| c["name"].as_str() == Some(container))
        .collect();
    let [chosen] = chosen.as_slice() else {
        return Err(Refused);
    };
    let volumes = pod
        .pointer("/spec/volumes")
        .and_then(Value::as_array)
        .ok_or(Refused)?;
    let tls: Vec<_> = volumes
        .iter()
        .filter(|v| v.pointer("/secret/secretName").and_then(Value::as_str) == Some(SECRET))
        .collect();
    let [tls] = tls.as_slice() else {
        return Err(Refused);
    };
    if tls.pointer("/secret/items").is_some()
        || tls.pointer("/secret/optional").and_then(Value::as_bool) == Some(true)
    {
        return Err(Refused);
    }
    let mounts: Vec<_> = chosen
        .get("volumeMounts")
        .and_then(Value::as_array)
        .ok_or(Refused)?
        .iter()
        .filter(|m| m["name"] == tls["name"])
        .collect();
    let [mount] = mounts.as_slice() else {
        return Err(Refused);
    };
    if mount["readOnly"].as_bool() != Some(true)
        || mount.get("subPath").is_some()
        || mount.get("subPathExpr").is_some()
        || !field(mount, "/mountPath")?.starts_with('/')
    {
        return Err(Refused);
    }
    let statuses: Vec<_> = pod
        .pointer("/status/containerStatuses")
        .and_then(Value::as_array)
        .ok_or(Refused)?
        .iter()
        .filter(|s| s["name"].as_str() == Some(container))
        .collect();
    let [status] = statuses.as_slice() else {
        return Err(Refused);
    };
    if status["ready"].as_bool() != Some(true)
        || status["restartCount"].as_u64() != Some(0)
        || status
            .pointer("/state/running/startedAt")
            .and_then(Value::as_str)
            .is_none()
    {
        return Err(Refused);
    }
    Ok(
        serde_json::json!({"uid":uid,"name":field(pod,"/metadata/name")?,"spec":pod.get("spec").ok_or(Refused)?,"container_id":field(status,"/containerID")?,"image_id":field(status,"/imageID")?,"started":field(status,"/state/running/startedAt")?}),
    )
}

#[async_trait]
pub trait PodReader: Send + Sync {
    async fn read(&self, name: &str) -> Result<Value, Refused>;
}

/// Fixed cluster target, no redirects/proxies or ambient certificate roots.
/// Token/CA are passed only by a future protected installer; no file is read
/// and no reader is configured by this car. A missing grant answers refusal.
pub struct HttpPodReader {
    pub(super) client: reqwest::Client,
    pub(super) token: String,
}
impl HttpPodReader {
    pub fn new(ca: reqwest::Certificate, token: String) -> Result<Self, Refused> {
        if token.is_empty() {
            return Err(Refused);
        }
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .tls_built_in_root_certs(false)
            .add_root_certificate(ca)
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|_| Refused)?;
        Ok(Self { client, token })
    }
}
#[async_trait]
impl PodReader for HttpPodReader {
    async fn read(&self, name: &str) -> Result<Value, Refused> {
        use futures::StreamExt;
        if !pod_name(name) {
            return Err(Refused);
        }
        let response = self
            .client
            .get(format!(
                "https://kubernetes.default.svc/api/v1/namespaces/{NAMESPACE}/pods/{name}"
            ))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|_| Refused)?;
        if response.status() != reqwest::StatusCode::OK {
            return Err(Refused);
        }
        let mut stream = response.bytes_stream();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| Refused)?;
            if bytes.len().saturating_add(chunk.len()) > LIMIT as usize {
                return Err(Refused);
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(|_| Refused)
    }
}

/// Only public trust metadata; the complete protected installer observation is
/// retained beside it, not replaced by a hash or a caller's ready boolean.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustPlan {
    pub installation: uuid::Uuid,
    pub generation: uuid::Uuid,
    pub public_trust_sha256: String,
    pub original: Value,
}
pub(super) fn collector_contract() -> Result<Value, Refused> {
    serde_json::from_str(include_str!(
        "../../../../../../infra/agent/private-collector-contract.json"
    ))
    .map_err(|_| Refused)
}
pub(super) fn trust_name() -> Result<String, Refused> {
    let contract = collector_contract()?;
    let name = contract["trust"].as_str().ok_or(Refused)?;
    if !pod_name(name) {
        return Err(Refused);
    }
    Ok(name.into())
}
impl TrustPlan {
    pub(super) fn validate(&self) -> Result<(), Refused> {
        use rsa::sha2::{Digest, Sha256};
        let contract = collector_contract()?;
        if self.installation.is_nil()
            || self.generation.is_nil()
            || self.original.get("kind").and_then(Value::as_str) != Some("ConfigMap")
            || self.original.pointer("/metadata/name") != contract.get("trust")
            || self
                .original
                .pointer("/metadata/namespace")
                .and_then(Value::as_str)
                != Some("boss-dev")
            || self
                .original
                .pointer("/metadata/deletionTimestamp")
                .is_some_and(|v| !v.is_null())
            || uuid::Uuid::parse_str(field(&self.original, "/metadata/uid")?)
                .map_err(|_| Refused)?
                .is_nil()
            || field(&self.original, "/metadata/resourceVersion")?.is_empty()
            || self.public_trust_sha256
                != format!(
                    "{:x}",
                    Sha256::digest(field(&self.original, "/data/ca.crt")?.as_bytes())
                )
        {
            return Err(Refused);
        }
        Ok(())
    }
}

pub struct FileBoundStore {
    root: Arc<File>,
    owner: u32,
    store: Arc<dyn SecretStore>,
    reader: Arc<dyn PodReader>,
}
impl FileBoundStore {
    pub fn open(
        path: &Path,
        owner: u32,
        store: Arc<dyn SecretStore>,
        reader: Arc<dyn PodReader>,
    ) -> Result<Self, Refused> {
        let root = File::from(
            openat2(
                rustix::fs::CWD,
                path,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
                Mode::empty(),
                ResolveFlags::NO_SYMLINKS,
            )
            .map_err(|_| Refused)?,
        );
        let meta = root.metadata().map_err(|_| Refused)?;
        if meta.uid() != owner || meta.mode() & 0o777 != 0o700 {
            return Err(Refused);
        }
        Ok(Self {
            root: Arc::new(root),
            owner,
            store,
            reader,
        })
    }
    pub(super) async fn filesystem<T: Send + 'static>(
        &self,
        f: impl FnOnce(&File, u32) -> Result<T, Refused> + Send + 'static,
    ) -> Result<T, Refused> {
        let root = self.root.clone();
        let owner = self.owner;
        tokio::task::spawn_blocking(move || {
            let meta = root.metadata().map_err(|_| Refused)?;
            if meta.uid() != owner || meta.mode() & 0o777 != 0o700 {
                return Err(Refused);
            }
            f(&root, owner)
        })
        .await
        .map_err(|_| Refused)?
    }
    pub(super) async fn original(&self) -> Result<Original, Refused> {
        self.filesystem(|root, owner| {
            serde_json::from_slice(&read(root, ORIGINAL, owner, LIMIT)?).map_err(|_| Refused)
        })
        .await
    }
    async fn validated_original(&self) -> Result<Original, Refused> {
        let original = self.original().await?;
        original.uid()?;
        let observed = self
            .reader
            .read(field(&original.pod, "/metadata/name")?)
            .await?;
        if pod_identity(&observed, &original.container)?
            != pod_identity(&original.pod, &original.container)?
        {
            return Err(Refused);
        }
        Ok(original)
    }
    /// Called only by the protected installer with original native receipts.
    /// Rebinding, implicit bootstrap on read and replacement recovery refuse.
    pub async fn bind(&self, original: Original) -> Result<(), Refused> {
        let uid = original.uid()?;
        let current = self
            .reader
            .read(field(&original.pod, "/metadata/name")?)
            .await?;
        if pod_identity(&current, &original.container)?
            != pod_identity(&original.pod, &original.container)?
        {
            return Err(Refused);
        }
        let secret = self
            .store
            .read_secret(NAMESPACE, SECRET)
            .await
            .map_err(|_| Refused)?
            .ok_or(Refused)?;
        if secret.uid != uid
            || secret.version != field(&original.secret_creation, "/metadata/resourceVersion")?
            || !secret.data.is_empty()
        {
            return Err(Refused);
        }
        let bytes = serde_json::to_vec(&original).map_err(|_| Refused)?;
        self.filesystem(move |root, owner| {
            retain(root, ORIGINAL, &bytes, owner)?;
            Ok(())
        })
        .await
    }
    pub async fn retain_trust(&self, plan: TrustPlan) -> Result<(), Refused> {
        let original = self.original().await?;
        plan.validate()?;
        if plan.installation != original.installation || !self.reserved(plan.generation).await? {
            return Err(Refused);
        }
        self.original_uid().await?;
        let name = format!("trust-{}.json", plan.generation);
        let bytes = serde_json::to_vec(&plan).map_err(|_| Refused)?;
        self.filesystem(move |root, owner| {
            retain(root, &name, &bytes, owner)?;
            Ok(())
        })
        .await
    }
    pub(super) async fn trust_plan(&self, generation: uuid::Uuid) -> Result<TrustPlan, Refused> {
        let name = format!("trust-{generation}.json");
        let plan: TrustPlan = self
            .filesystem(move |root, owner| {
                serde_json::from_slice(&read(root, &name, owner, LIMIT)?).map_err(|_| Refused)
            })
            .await?;
        plan.validate()?;
        Ok(plan)
    }
    pub(super) async fn check_effect(&self, effect: &Effect) -> Result<(), Refused> {
        let original = self.original().await?;
        if self.original_uid().await? != effect.secret_uid {
            return Err(Refused);
        }
        let plan = self.trust_plan(effect.generation).await?;
        if plan.installation != original.installation
            || plan.generation != effect.generation
            || plan.public_trust_sha256 != effect.public_trust_sha256
            || field(&plan.original, "/metadata/uid")? != effect.public_trust_uid
        {
            return Err(Refused);
        }
        Ok(())
    }
    async fn request(&self, request: uuid::Uuid, create: bool) -> Result<bool, Refused> {
        if request.is_nil() {
            return Err(Refused);
        }
        let original = self.original().await?;
        let name = format!("request-{request}.json");
        use rsa::sha2::{Digest, Sha256};
        let original_hash = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&original).map_err(|_| Refused)?)
        );
        let bytes=serde_json::to_vec(&serde_json::json!({"installation":original.installation,"original_sha256":original_hash,"original_uid":original.uid()?,"request":request})).map_err(|_|Refused)?;
        self.filesystem(move |root, owner| {
            if create {
                retain(root, &name, &bytes, owner)
            } else {
                match open(root, &name, OFlags::RDONLY) {
                    Ok(_) => {
                        if read(root, &name, owner, 4096)? != bytes {
                            return Err(Refused);
                        }
                        Ok(true)
                    }
                    Err(e) if e == rustix::io::Errno::NOENT => Ok(false),
                    Err(_) => Err(Refused),
                }
            }
        })
        .await
    }
}
#[async_trait]
impl BoundStore for FileBoundStore {
    async fn original_uid(&self) -> Result<String, Refused> {
        self.validated_original().await?.uid()
    }
    async fn reserve(&self, request: uuid::Uuid) -> Result<bool, Refused> {
        self.original_uid().await?;
        self.request(request, true).await
    }
    async fn reserved(&self, request: uuid::Uuid) -> Result<bool, Refused> {
        self.request(request, false).await
    }
    async fn attest_effect(&self, effect: &Effect) -> Result<(), Refused> {
        self.check_effect(effect).await
    }
}
#[async_trait]
impl SecretStore for FileBoundStore {
    async fn read_key(&self, _: &str, _: &str, _: &str) -> Result<Option<String>, String> {
        Err("private TLS lifecycle requires whole-object reads".into())
    }
    async fn write_key(&self, _: &str, _: &str, _: &str, _: &str) -> Result<(), String> {
        Err("private TLS lifecycle requires original-bound CAS".into())
    }
    async fn read_secret(&self, ns: &str, name: &str) -> Result<Option<SecretData>, String> {
        if (ns, name) != (NAMESPACE, SECRET) {
            return Err(Refused.to_string());
        }
        let original = self.validated_original().await.map_err(|e| e.to_string())?;
        let uid = original.uid().map_err(|e| e.to_string())?;
        let birth_version = field(&original.secret_creation, "/metadata/resourceVersion")
            .map_err(|e| e.to_string())?;
        let value = self
            .store
            .read_secret(ns, name)
            .await
            .map_err(|_| Refused.to_string())?
            .ok_or_else(|| Refused.to_string())?;
        if value.uid != uid {
            return Err(Refused.to_string());
        }
        // Lifecycle absence is about active slots, not map keys: cleared values
        // and leftover effect metadata cannot make a later object an original.
        let mut has_active_slot = false;
        for name in ["next", "current", "previous"] {
            has_active_slot |= slot(&value, name)
                .map_err(|_| Refused.to_string())?
                .is_some();
        }
        if !has_active_slot && value.version != birth_version {
            return Err(Refused.to_string());
        }
        Ok(Some(value))
    }
    async fn write_keys_if(
        &self,
        ns: &str,
        name: &str,
        entries: &[(&str, &str)],
        observed: &SecretData,
    ) -> Result<WriteAt, String> {
        if (ns, name) != (NAMESPACE, SECRET)
            || self.original_uid().await.map_err(|e| e.to_string())? != observed.uid
        {
            return Err(Refused.to_string());
        }
        self.store
            .write_keys_if(ns, name, entries, observed)
            .await
            .map_err(|_| Refused.to_string())
    }
}
fn open(root: &File, name: &str, flags: OFlags) -> Result<File, rustix::io::Errno> {
    openat(
        root,
        name,
        flags | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::from_bits_truncate(0o600),
    )
    .map(File::from)
}
pub(super) fn read(root: &File, name: &str, owner: u32, limit: u64) -> Result<Vec<u8>, Refused> {
    let file = open(root, name, OFlags::RDONLY).map_err(|_| Refused)?;
    let meta = file.metadata().map_err(|_| Refused)?;
    if !meta.is_file()
        || meta.uid() != owner
        || meta.mode() & 0o777 != 0o600
        || meta.nlink() != 1
        || meta.len() > limit
    {
        return Err(Refused);
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Refused)?;
    if bytes.len() as u64 != meta.len() || bytes.len() as u64 > limit {
        return Err(Refused);
    }
    Ok(bytes)
}
pub(super) fn retain(root: &File, name: &str, bytes: &[u8], owner: u32) -> Result<bool, Refused> {
    if bytes.len() as u64 > LIMIT {
        return Err(Refused);
    }
    match open(root, name, OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL) {
        Ok(mut file) => {
            file.write_all(bytes).map_err(|_| Refused)?;
            file.sync_all().map_err(|_| Refused)?;
            root.sync_all().map_err(|_| Refused)?;
            Ok(true)
        }
        Err(e) if e == rustix::io::Errno::EXIST => {
            if read(root, name, owner, LIMIT)? != bytes {
                return Err(Refused);
            }
            open(root, name, OFlags::RDONLY)
                .map_err(|_| Refused)?
                .sync_all()
                .map_err(|_| Refused)?;
            root.sync_all().map_err(|_| Refused)?;
            Ok(false)
        }
        Err(_) => Err(Refused),
    }
}
