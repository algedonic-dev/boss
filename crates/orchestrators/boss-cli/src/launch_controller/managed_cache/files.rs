//! Descriptor-confined persistence under the existing private cache kernel.
//! No native broker location, collector mount or release authority is selected.
use super::{CacheError, Lease, Renewed, State, Store};
use rustix::fs::{Mode, OFlags, ResolveFlags, openat, openat2, renameat};
use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

const CACHE_LIMIT: usize = 256 * 1024;
const STATE_LIMIT: usize = 32 * 1024;

/// Supported account/format verification stays inside the private broker;
/// private provider output must never become a diagnostic. No native verifier
/// or enrollment selector is installed by this source checkpoint.
pub(crate) trait PrivateVerifier: Send {
    /// Return the qualified private generation. A rotating verification effect
    /// cannot publish the input's now-spent refresh token in its place.
    fn verify(&mut self, bytes: &[u8], principal: &str) -> Result<Vec<u8>, CacheError>;
}

/// The filesystem never verifies an account by itself or calls a provider on
/// its synchronous callback. Async renewal must first supply an exact witness.
pub(crate) struct AwaitingVerification;
impl PrivateVerifier for AwaitingVerification {
    fn verify(&mut self, _bytes: &[u8], _principal: &str) -> Result<Vec<u8>, CacheError> {
        Err(CacheError::Unavailable)
    }
}

/// An owned descriptor/OS-lock adapter for the async private renewal engine.
/// Native ScopePort/mount installation remains owed. No path or bytes arrive
/// from IPC, and every filesystem operation runs on the bounded blocking pool.
pub(crate) struct FileRenewal {
    store: std::sync::Arc<std::sync::Mutex<FileStore<AwaitingVerification>>>,
    lease: Lease,
}
impl FileRenewal {
    pub(super) fn new(store: FileStore<AwaitingVerification>, lease: Lease) -> Self {
        Self {
            store: std::sync::Arc::new(std::sync::Mutex::new(store)),
            lease,
        }
    }
    async fn filesystem<T: Send + 'static>(
        &self,
        op: impl FnOnce(&mut FileStore<AwaitingVerification>, &Lease) -> Result<T, CacheError>
        + Send
        + 'static,
    ) -> Result<T, CacheError> {
        let store = self.store.clone();
        let lease = self.lease.clone();
        tokio::task::spawn_blocking(move || {
            let mut store = store.lock().map_err(|_| CacheError::Unavailable)?;
            op(&mut store, &lease)
        })
        .await
        .map_err(|_| CacheError::Unavailable)?
    }
    pub(super) async fn seal(
        self,
        proof: super::verification::Verified,
    ) -> Result<FileStore<super::verification::Verified>, CacheError> {
        tokio::task::spawn_blocking(move || {
            let store = std::sync::Arc::try_unwrap(self.store)
                .map_err(|_| CacheError::Unavailable)?
                .into_inner()
                .map_err(|_| CacheError::Unavailable)?;
            if store.state() != Ok(State::Leased(self.lease.clone()))
                || !proof.matches(&self.lease)
                || crate::launch_controller::hash(&read_private(
                    &store.root,
                    &format!("renew-{}-original.private", self.lease.nonce),
                    store.owner,
                    CACHE_LIMIT,
                )?) != proof.original_hash()
                || read_private(
                    &store.root,
                    &format!("renew-{}-replacement.private", self.lease.nonce),
                    store.owner,
                    CACHE_LIMIT,
                )? != proof.replacement()
            {
                return Err(CacheError::InvalidBinding);
            }
            Ok(store.with_verifier(proof))
        })
        .await
        .map_err(|_| CacheError::Unavailable)?
    }
}
#[async_trait::async_trait]
impl super::verification::PrivateRenewal for FileRenewal {
    async fn attest(&mut self, expected: &super::verification::Expected) -> Result<(), CacheError> {
        let run = expected.run.clone();
        let nonce = expected.nonce;
        let subject = expected.subject.clone();
        self.filesystem(move |store, lease| {
            if store.state()? != State::Leased(lease.clone())
                || lease.run != run
                || lease.nonce != nonce
                || lease.installed.principal != subject
                || lease.installed.issuer != super::verification::ISSUER
                || lease.binding.is_none()
                || store.retained.as_ref().is_none_or(|r| r.lease != *lease)
            {
                return Err(CacheError::InvalidBinding);
            }
            let name = format!("renew-{}-refresh-intent.private", lease.nonce);
            match rustix::fs::statat(&store.root, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
                Err(rustix::io::Errno::NOENT) => Ok(()),
                _ => Err(CacheError::Unavailable),
            }
        })
        .await
    }
    async fn input(&mut self) -> Result<Vec<u8>, CacheError> {
        self.filesystem(|store, lease| {
            if store.state()? != State::Leased(lease.clone()) {
                return Err(CacheError::Conflict);
            }
            let retained = store
                .retained
                .as_ref()
                .filter(|r| r.lease == *lease)
                .ok_or(CacheError::Unavailable)?;
            read_private(&retained.root, "auth.json", retained.owner, CACHE_LIMIT)
        })
        .await
    }
    async fn retain(&mut self, phase: &'static str, bytes: &[u8]) -> Result<(), CacheError> {
        if ![
            "original",
            "access-before",
            "refresh-intent",
            "refresh-response",
            "access-after",
            "replacement",
        ]
        .contains(&phase)
            || bytes.len() > CACHE_LIMIT
        {
            return Err(CacheError::InvalidBinding);
        }
        let bytes = bytes.to_vec();
        self.filesystem(move |store, lease| {
            if store.state()? != State::Leased(lease.clone()) {
                return Err(CacheError::Conflict);
            }
            retain_once(
                &store.root,
                &format!("renew-{}-{phase}.private", lease.nonce),
                &bytes,
                store.owner,
            )
        })
        .await
    }
    async fn quarantine(&mut self) -> Result<(), CacheError> {
        self.filesystem(|store, lease| {
            if store.state()? == State::Quarantined(lease.clone()) {
                return Ok(());
            }
            let _: Result<(), CacheError> = super::quarantine(lease, store);
            if store.state()? == State::Quarantined(lease.clone()) {
                Ok(())
            } else {
                Err(CacheError::Unavailable)
            }
        })
        .await
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CollectionWitness {
    lease: Lease,
    generation: String,
    sha256: String,
    bytes: u64,
}

pub(super) fn private_directory(path: &Path, owner: u32) -> Result<File, CacheError> {
    let fd = openat2(
        rustix::fs::CWD,
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
        ResolveFlags::NO_SYMLINKS,
    )
    .map_err(|_| CacheError::Unavailable)?;
    let file = File::from(fd);
    let metadata = file.metadata().map_err(|_| CacheError::Unavailable)?;
    if !metadata.is_dir() || metadata.uid() != owner || metadata.mode() & 0o777 != 0o700 {
        return Err(CacheError::Unavailable);
    }
    Ok(file)
}
fn open_file(root: &File, name: &str, flags: OFlags) -> Result<File, CacheError> {
    if name.is_empty() || name.contains('/') || matches!(name, "." | "..") {
        return Err(CacheError::InvalidBinding);
    }
    let fd = openat(
        root,
        name,
        flags | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        // mode-bits-ok: private broker state/originals, never worker access
        Mode::from_bits_truncate(0o600),
    )
    .map_err(|_| CacheError::Unavailable)?;
    Ok(File::from(fd))
}
fn read_private(root: &File, name: &str, owner: u32, limit: usize) -> Result<Vec<u8>, CacheError> {
    let mut file = open_file(root, name, OFlags::RDONLY)?;
    let metadata = file.metadata().map_err(|_| CacheError::Unavailable)?;
    if !metadata.is_file()
        || metadata.uid() != owner
        || metadata.nlink() != 1
        || metadata.mode() & 0o777 != 0o600
        || metadata.len() > limit as u64
    {
        return Err(CacheError::Unavailable);
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| CacheError::Unavailable)?;
    let after = file.metadata().map_err(|_| CacheError::Unavailable)?;
    if bytes.len() > limit
        || bytes.len() as u64 != metadata.len()
        || after.len() != metadata.len()
        || after.mtime() != metadata.mtime()
        || after.mtime_nsec() != metadata.mtime_nsec()
        || after.ctime() != metadata.ctime()
        || after.ctime_nsec() != metadata.ctime_nsec()
    {
        return Err(CacheError::Unavailable);
    }
    Ok(bytes)
}
fn retain_once(root: &File, name: &str, bytes: &[u8], owner: u32) -> Result<(), CacheError> {
    match open_file(root, name, OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL) {
        Ok(mut file) => {
            file.write_all(bytes).map_err(|_| CacheError::Unavailable)?;
            file.sync_all().map_err(|_| CacheError::Unavailable)?;
            root.sync_all().map_err(|_| CacheError::Unavailable)
        }
        Err(_) if read_private(root, name, owner, CACHE_LIMIT)? == bytes => Ok(()),
        Err(_) => Err(CacheError::Conflict),
    }
}

/// The trusted collector opens a fixed retained mount after the controller
/// established exact run/Job/Pod/container and terminal evidence. This pins
/// its directory descriptor; no worker-supplied pathname or metadata proof
/// is accepted. The native private mount/authority transport remains owed.
pub(crate) struct RetainedCache {
    root: File,
    owner: u32,
    lease: Lease,
}
impl RetainedCache {
    pub(crate) fn open(
        path: &Path,
        owner: u32,
        lease: &Lease,
        running: &crate::launch_binding::RunningBinding,
        terminal: &crate::launch_controller::Outcome,
    ) -> Result<Self, CacheError> {
        if lease.binding.as_ref() != Some(&running.identity())
            || !running.matches_run(&lease.run)
            || terminal.ending != "native_exited"
            || !terminal.stop_observed
            || !terminal.evidence_complete
            || !terminal.workspace_retained
            || terminal.job_uid.as_deref() != running.identity()["job_uid"].as_str()
        {
            return Err(CacheError::InvalidBinding);
        }
        Ok(Self {
            root: private_directory(path, owner)?,
            owner,
            lease: lease.clone(),
        })
    }
}

/// Native retained-mount contract, derived from original controller originals.
/// No UID or authority is learned from the worker or an IPC request. A native
/// installer must still attest that its private local mount is THIS volume.
pub(crate) struct RetainedMount {
    pub workspace_uid: String,
    pub collector: crate::launch_binding::JobBinding,
}
impl RetainedMount {
    pub(crate) fn from_observations(
        spec: &crate::launch_job::Spec<'_>,
        original: &serde_json::Value,
        observed: &serde_json::Value,
        collector: &serde_json::Value,
    ) -> Result<Self, CacheError> {
        let intent = crate::launch_job::workspace_manifest(spec);
        super::super::workspace_identity(original, &intent)
            .map_err(|_| CacheError::InvalidBinding)?;
        super::super::workspace_identity(observed, &intent)
            .map_err(|_| CacheError::InvalidBinding)?;
        if original["metadata"]["uid"] != observed["metadata"]["uid"] {
            return Err(CacheError::InvalidBinding);
        }
        let collector = crate::launch_binding::JobBinding::from_recovery_created(collector, spec)
            .map_err(|_| CacheError::InvalidBinding)?;
        Ok(Self {
            workspace_uid: original["metadata"]["uid"]
                .as_str()
                .ok_or(CacheError::InvalidBinding)?
                .into(),
            collector,
        })
    }
}

/// One exclusive OS lock and durable generation CAS; no expiry unlock.
/// The IPC adapter runs filesystem operations on the blocking pool.
pub(crate) struct FileStore<V: PrivateVerifier> {
    root: File,
    owner: u32,
    _lock: File,
    retained: Option<RetainedCache>,
    verifier: V,
    #[cfg(test)]
    fail_after_rename: bool,
}
impl<V: PrivateVerifier + 'static> FileStore<V> {
    fn with_verifier<W: PrivateVerifier>(self, verifier: W) -> FileStore<W> {
        FileStore {
            root: self.root,
            owner: self.owner,
            _lock: self._lock,
            retained: self.retained,
            verifier,
            #[cfg(test)]
            fail_after_rename: self.fail_after_rename,
        }
    }
    pub(crate) fn open(path: &Path, owner: u32, verifier: V) -> Result<Self, CacheError> {
        let root = private_directory(path, owner)?;
        let lock = open_file(&root, "lease.lock", OFlags::RDWR | OFlags::CREATE)?;
        let metadata = lock.metadata().map_err(|_| CacheError::Unavailable)?;
        if !metadata.is_file()
            || metadata.uid() != owner
            || metadata.nlink() != 1
            || metadata.mode() & 0o777 != 0o600
        {
            return Err(CacheError::Unavailable);
        }
        lock.try_lock().map_err(|_| CacheError::Unavailable)?;
        Ok(Self {
            root,
            owner,
            _lock: lock,
            retained: None,
            verifier,
            #[cfg(test)]
            fail_after_rename: false,
        })
    }
    pub(crate) fn attach_retained(&mut self, retained: RetainedCache) {
        self.retained = Some(retained);
    }
    pub(super) fn state(&self) -> Result<State, CacheError> {
        serde_json::from_slice(&read_private(
            &self.root,
            "state.json",
            self.owner,
            STATE_LIMIT,
        )?)
        .map_err(|_| CacheError::Unavailable)
    }
    /// Exact upload operation closure belongs to the held stream, never to
    /// the lifetime of its bearer token. Any prior intent blocks another POST.
    pub(super) fn upload_available(&self, lease: &Lease) -> Result<(), CacheError> {
        if self.state()? != State::Leased(lease.clone()) {
            return Err(CacheError::Conflict);
        }
        match rustix::fs::statat(
            &self.root,
            format!("upload-{}-intent.private", lease.nonce),
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        ) {
            Err(rustix::io::Errno::NOENT) => Ok(()),
            Ok(_) => Err(CacheError::Conflict),
            Err(_) => Err(CacheError::Unavailable),
        }
    }
    pub(super) fn begin_upload(
        &self,
        lease: &Lease,
        before: &serde_json::Value,
    ) -> Result<(), CacheError> {
        self.upload_available(lease)?;
        self.retain_upload_record(
            &format!("upload-{}-intent.private", lease.nonce),
            &serde_json::to_vec(before).map_err(|_| CacheError::Unavailable)?,
        )
    }
    pub(super) fn retain_upload_guard(
        &self,
        lease: &Lease,
        phase: &str,
        originals: &serde_json::Value,
    ) -> Result<(), CacheError> {
        if !matches!(phase, "authorize" | "before" | "after")
            || self.state()? != State::Leased(lease.clone())
        {
            return Err(CacheError::Conflict);
        }
        self.retain_upload_record(
            &format!("upload-{}-{phase}-native.private", lease.nonce),
            &serde_json::to_vec(originals).map_err(|_| CacheError::Unavailable)?,
        )
    }
    fn retain_upload_record(&self, name: &str, bytes: &[u8]) -> Result<(), CacheError> {
        // Whole private originals are bounded, never reduced into summaries.
        if bytes.len() > 16 * 1024 * 1024 {
            return Err(CacheError::Unavailable);
        }
        retain_once(&self.root, name, bytes, self.owner)?;
        if read_private(&self.root, name, self.owner, 16 * 1024 * 1024)? != bytes {
            return Err(CacheError::Unavailable);
        }
        Ok(())
    }
    pub(super) fn retain_upload(
        &self,
        lease: &Lease,
        bytes: &[u8],
        receipt: &serde_json::Value,
    ) -> Result<(), CacheError> {
        if bytes.is_empty()
            || bytes.len() > CACHE_LIMIT
            || self.state()? != State::Leased(lease.clone())
        {
            return Err(CacheError::InvalidBinding);
        }
        let intent = format!("upload-{}-intent.private", lease.nonce);
        read_private(&self.root, &intent, self.owner, 16 * 1024 * 1024)?;
        self.retain_upload_record(&format!("upload-{}-original.private", lease.nonce), bytes)?;
        self.retain_upload_record(
            &format!("upload-{}-receipt.private", lease.nonce),
            &serde_json::to_vec(receipt).map_err(|_| CacheError::Unavailable)?,
        )
    }
    pub(super) fn stage_upload(
        &self,
        lease: &Lease,
        bytes: &[u8],
        transport: &serde_json::Value,
    ) -> Result<(), CacheError> {
        if bytes.len() > CACHE_LIMIT || self.state()? != State::Leased(lease.clone()) {
            return Err(CacheError::InvalidBinding);
        }
        read_private(
            &self.root,
            &format!("upload-{}-intent.private", lease.nonce),
            self.owner,
            16 * 1024 * 1024,
        )?;
        self.retain_upload_record(&format!("upload-{}-original.private", lease.nonce), bytes)?;
        self.retain_upload_record(
            &format!("upload-{}-transport.private", lease.nonce),
            &serde_json::to_vec(transport).map_err(|_| CacheError::Unavailable)?,
        )
    }
    fn original_collection(&self, lease: &Lease) -> Result<Vec<u8>, CacheError> {
        let witness: CollectionWitness = serde_json::from_slice(&read_private(
            &self.root,
            &format!("collected-{}.json", lease.nonce),
            self.owner,
            STATE_LIMIT,
        )?)
        .map_err(|_| CacheError::Unavailable)?;
        if witness.lease != *lease || witness.generation != lease.nonce.to_string() {
            return Err(CacheError::Conflict);
        }
        let bytes = read_private(
            &self.root,
            &format!("collected-{}.auth", lease.nonce),
            self.owner,
            CACHE_LIMIT,
        )?;
        if bytes.len() as u64 != witness.bytes
            || crate::launch_controller::hash(&bytes) != witness.sha256
        {
            return Err(CacheError::Unavailable);
        }
        Ok(bytes)
    }
    /// Reconcile the exact privately retained collection, keeping the original
    /// state unavailable. This is not publication or a Collected->Ready door.
    pub(crate) fn reconcile_collected(&mut self, lease: &Lease) -> Result<String, CacheError> {
        let State::Collected(owner, next) = self.state()? else {
            return Err(CacheError::Conflict);
        };
        if &owner != lease {
            return Err(CacheError::Conflict);
        }
        let bytes = self.original_collection(lease)?;
        self.verifier.verify(&bytes, &next.principal)?;
        if self.state()? != State::Collected(owner, next.clone()) {
            return Err(CacheError::Conflict);
        }
        Ok(next.generation)
    }
}
impl<V: PrivateVerifier + 'static> Store for FileStore<V> {
    fn compare_swap(&mut self, expected: &State, next: &State) -> Result<bool, CacheError> {
        // No caller turns an uncertain stream Ready. Actual publication and
        // installation remain separate private broker effects, never inferred.
        if matches!(next, State::Ready(_)) {
            return Err(CacheError::InvalidBinding);
        }
        if self.state()? != *expected {
            return Ok(false);
        }
        // An ownership read CAS is not another state-changing write. In
        // particular it must not obscure a later committed collection failure.
        if expected == next {
            return Ok(true);
        }
        if let State::Ready(installed) = expected {
            let original = read_private(&self.root, "installed.auth", self.owner, CACHE_LIMIT)?;
            self.verifier.verify(&original, &installed.principal)?;
        }
        let raw = serde_json::to_vec(next).map_err(|_| CacheError::InvalidBinding)?;
        if raw.len() > STATE_LIMIT {
            return Err(CacheError::InvalidBinding);
        }
        let transaction = uuid::Uuid::new_v4();
        let witness = serde_json::to_vec(&serde_json::json!({"expected":expected,"next":next}))
            .map_err(|_| CacheError::InvalidBinding)?;
        retain_once(
            &self.root,
            &format!("transition-{transaction}.json"),
            &witness,
            self.owner,
        )?;
        let temporary = format!("state-{transaction}.pending");
        retain_once(&self.root, &temporary, &raw, self.owner)?;
        renameat(&self.root, &temporary, &self.root, "state.json")
            .map_err(|_| CacheError::Unavailable)?;
        self.root.sync_all().map_err(|_| CacheError::Unavailable)?;
        #[cfg(test)]
        if self.fail_after_rename {
            return Err(CacheError::Unavailable);
        }
        if self.state()? != *next {
            return Err(CacheError::Unavailable);
        }
        Ok(true)
    }
    fn collect(&mut self, lease: &Lease) -> Result<Renewed, CacheError> {
        if self.state()? != State::Leased(lease.clone()) {
            return Err(CacheError::Conflict);
        }
        let retained = self.retained.as_ref().ok_or(CacheError::Unavailable)?;
        if retained.lease != *lease {
            return Err(CacheError::Conflict);
        }
        let bytes = read_private(&retained.root, "auth.json", retained.owner, CACHE_LIMIT)?;
        let bytes = self.verifier.verify(&bytes, &lease.installed.principal)?;
        if bytes.len() > CACHE_LIMIT {
            return Err(CacheError::InvalidBinding);
        }
        retain_once(
            &self.root,
            &format!("collected-{}.auth", lease.nonce),
            &bytes,
            self.owner,
        )?;
        let witness = CollectionWitness {
            lease: lease.clone(),
            generation: lease.nonce.to_string(),
            sha256: crate::launch_controller::hash(&bytes),
            bytes: bytes.len() as u64,
        };
        retain_once(
            &self.root,
            &format!("collected-{}.json", lease.nonce),
            &serde_json::to_vec(&witness).map_err(|_| CacheError::InvalidBinding)?,
            self.owner,
        )?;
        Ok(Renewed {
            issuer: lease.installed.issuer.clone(),
            principal: lease.installed.principal.clone(),
            generation: lease.nonce.to_string(),
            binding: lease.binding.clone().ok_or(CacheError::InvalidBinding)?,
            validated: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    struct FixtureVerifier;
    impl PrivateVerifier for FixtureVerifier {
        fn verify(&mut self, bytes: &[u8], principal: &str) -> Result<Vec<u8>, CacheError> {
            if [
                b"synthetic private cache".as_slice(),
                b"synthetic private changed cache".as_slice(),
            ]
            .contains(&bytes)
                && principal == "authorized-fixture-principal"
            {
                Ok(bytes.to_vec())
            } else {
                Err(CacheError::InvalidBinding)
            }
        }
    }
    #[test]
    fn an_owned_private_directory_has_a_real_confined_store() {
        let root = boss_testing::scratch_dir("managed-cache-fs");
        // mode-bits-ok: private fixture directory traversal, never an executable file
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            FileStore::open(&root, rustix::process::geteuid().as_raw(), FixtureVerifier).is_ok()
        );
    }

    fn seeded() -> (std::path::PathBuf, FileStore<FixtureVerifier>) {
        let (_, binding, _, _) = super::super::tests::fixture();
        let root = boss_testing::scratch_dir("managed-cache-durable");
        // mode-bits-ok: private fixture directory traversal, never an executable file
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let store =
            FileStore::open(&root, rustix::process::geteuid().as_raw(), FixtureVerifier).unwrap();
        let initial = State::Ready(super::super::Installed::from_binding(&binding).unwrap());
        retain_once(
            &store.root,
            "state.json",
            &serde_json::to_vec(&initial).unwrap(),
            store.owner,
        )
        .unwrap();
        retain_once(
            &store.root,
            "installed.auth",
            b"synthetic private cache",
            store.owner,
        )
        .unwrap();
        (root, store)
    }
    fn collected(store: &mut FileStore<FixtureVerifier>) -> Lease {
        let (prepared, binding, running, _) = super::super::tests::fixture();
        let unbound =
            super::super::acquire(&prepared, &binding, uuid::Uuid::new_v4(), store).unwrap();
        let lease = super::super::bind(&unbound, &running, store).unwrap();
        let cache = boss_testing::scratch_dir("managed-cache-retained");
        // mode-bits-ok: private retained fixture directory, never an executable file
        std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o700)).unwrap();
        let directory = private_directory(&cache, store.owner).unwrap();
        retain_once(
            &directory,
            "auth.json",
            b"synthetic private cache",
            store.owner,
        )
        .unwrap();
        let terminal = terminal();
        store.attach_retained(
            RetainedCache::open(&cache, store.owner, &lease, &running, &terminal).unwrap(),
        );
        lease
    }
    fn terminal() -> crate::launch_controller::Outcome {
        crate::launch_controller::Outcome {
            ending: "native_exited".into(),
            job_uid: Some("job-uid".into()),
            stop_observed: true,
            evidence_complete: true,
            workspace_retained: true,
            observations: vec![],
        }
    }

    struct ReplacementVerifier;
    impl PrivateVerifier for ReplacementVerifier {
        fn verify(&mut self, _: &[u8], _: &str) -> Result<Vec<u8>, CacheError> {
            Ok(b"synthetic private changed cache".to_vec())
        }
    }
    #[test]
    fn collection_conserves_verified_replacement_instead_of_spent_input() {
        let (_, mut store) = seeded();
        let lease = collected(&mut store);
        let mut store = store.with_verifier(ReplacementVerifier);
        super::super::finish(&lease, &terminal(), &mut store).unwrap();
        assert_eq!(
            read_private(
                &store.root,
                &format!("collected-{}.auth", lease.nonce),
                store.owner,
                CACHE_LIMIT
            )
            .unwrap(),
            b"synthetic private changed cache"
        );
    }
    #[tokio::test]
    async fn async_renewal_uses_the_owned_store_and_leaves_a_durable_unavailable_collection() {
        let (mut expected, provider, bytes) = super::super::verification::tests::provider_fixture();
        let (prepared, mut binding, running, _) = super::super::tests::fixture();
        expected.run = prepared.run.clone();
        binding.issuer = super::super::verification::ISSUER.into();
        binding.principal = expected.subject.clone();
        let root = boss_testing::scratch_dir("managed-private-provider-store");
        // mode-bits-ok: private renewal fixture directory, never an executable file
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut store = FileStore::open(
            &root,
            rustix::process::geteuid().as_raw(),
            AwaitingVerification,
        )
        .unwrap();
        // This collection fixture begins after an already admitted run; it
        // does not invent initial issuer installation/admission verification.
        let lease = Lease {
            installed: super::super::Installed::from_binding(&binding).unwrap(),
            run: prepared.run.clone(),
            nonce: expected.nonce,
            binding: Some(running.identity()),
        };
        retain_once(
            &store.root,
            "state.json",
            &serde_json::to_vec(&State::Leased(lease.clone())).unwrap(),
            store.owner,
        )
        .unwrap();
        let mount = boss_testing::scratch_dir("managed-private-provider-retained");
        // mode-bits-ok: private retained-volume fixture directory, never an executable file
        std::fs::set_permissions(&mount, std::fs::Permissions::from_mode(0o700)).unwrap();
        let directory = private_directory(&mount, store.owner).unwrap();
        retain_once(&directory, "auth.json", &bytes, store.owner).unwrap();
        store.attach_retained(
            RetainedCache::open(&mount, store.owner, &lease, &running, &terminal()).unwrap(),
        );
        let mut store =
            super::super::verification::renew_store(&expected, &provider, store, lease.clone())
                .await
                .unwrap();
        super::super::finish(&lease, &terminal(), &mut store).unwrap();
        assert!(matches!(store.state().unwrap(), State::Collected(_, _)));
        let collected = read_private(
            &store.root,
            &format!("collected-{}.auth", lease.nonce),
            store.owner,
            CACHE_LIMIT,
        )
        .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&collected).unwrap()["tokens"]["refresh_token"],
            "SYNTHETIC_NEW_REFRESH"
        );
        assert_eq!(
            read_private(
                &store.root,
                &format!("renew-{}-original.private", lease.nonce),
                store.owner,
                CACHE_LIMIT
            )
            .unwrap(),
            bytes
        );
        assert_eq!(
            read_private(
                &store.root,
                &format!("renew-{}-replacement.private", lease.nonce),
                store.owner,
                CACHE_LIMIT
            )
            .unwrap(),
            collected
        );
        assert_eq!(
            store.reconcile_collected(&lease).unwrap(),
            lease.nonce.to_string()
        );
        assert!(matches!(store.state().unwrap(), State::Collected(_, _)));
        for phase in [
            "original",
            "access-before",
            "refresh-intent",
            "refresh-response",
            "access-after",
            "replacement",
        ] {
            assert!(
                read_private(
                    &store.root,
                    &format!("renew-{}-{phase}.private", lease.nonce),
                    store.owner,
                    CACHE_LIMIT
                )
                .is_ok()
            );
        }
    }

    #[test]
    fn committed_collection_error_remains_unavailable_after_reopen_and_exact_recovery() {
        let (root, mut store) = seeded();
        let lease = collected(&mut store);
        store.fail_after_rename = true;
        assert_eq!(
            super::super::finish(&lease, &terminal(), &mut store),
            Err(CacheError::Conflict)
        );
        assert!(matches!(store.state().unwrap(), State::Collected(_, _)));
        drop(store);
        let mut reopened =
            FileStore::open(&root, rustix::process::geteuid().as_raw(), FixtureVerifier).unwrap();
        assert_eq!(
            reopened.reconcile_collected(&lease).unwrap(),
            lease.nonce.to_string()
        );
        let (prepared, mut binding, _, _) = super::super::tests::fixture();
        binding.storage_generation = lease.nonce.to_string();
        assert_eq!(
            super::super::acquire(&prepared, &binding, uuid::Uuid::new_v4(), &mut reopened),
            Err(CacheError::Conflict)
        );
    }

    #[test]
    fn recovery_rejects_changed_private_original_even_when_account_verification_passes() {
        let (_, mut store) = seeded();
        let lease = collected(&mut store);
        super::super::finish(&lease, &terminal(), &mut store).unwrap();
        let mut file = open_file(
            &store.root,
            &format!("collected-{}.auth", lease.nonce),
            OFlags::WRONLY | OFlags::TRUNC,
        )
        .unwrap();
        file.write_all(b"synthetic private changed cache").unwrap();
        file.sync_all().unwrap();
        assert!(
            store.reconcile_collected(&lease).is_err(),
            "account equality does not prove the retained original"
        );
    }

    #[test]
    fn collection_requires_the_observed_workspace_to_remain_retained() {
        let (_, mut store) = seeded();
        let lease = collected(&mut store);
        let (_, _, running, _) = super::super::tests::fixture();
        let mut outcome = terminal();
        outcome.workspace_retained = false;
        assert!(
            RetainedCache::open(
                Path::new("/absent-private-mount"),
                store.owner,
                &lease,
                &running,
                &outcome
            )
            .is_err()
        );
        let root = boss_testing::scratch_dir("managed-cache-erased-retention");
        // mode-bits-ok: private retention-control directory, never an executable file
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            RetainedCache::open(&root, store.owner, &lease, &running, &outcome).is_err(),
            "a directory is not evidence that the original workspace survived"
        );
    }

    #[test]
    fn committed_acquire_and_running_errors_never_reopen_readiness() {
        for during_bind in [false, true] {
            let (root, mut store) = seeded();
            let (prepared, binding, running, _) = super::super::tests::fixture();
            let nonce = uuid::Uuid::new_v4();
            if during_bind {
                let lease = super::super::acquire(&prepared, &binding, nonce, &mut store).unwrap();
                store.fail_after_rename = true;
                assert!(super::super::bind(&lease, &running, &mut store).is_err());
            } else {
                store.fail_after_rename = true;
                assert!(super::super::acquire(&prepared, &binding, nonce, &mut store).is_err());
            }
            assert!(!matches!(store.state().unwrap(), State::Ready(_)));
            drop(store);
            let mut reopened =
                FileStore::open(&root, rustix::process::geteuid().as_raw(), FixtureVerifier)
                    .unwrap();
            assert!(
                super::super::acquire(&prepared, &binding, uuid::Uuid::new_v4(), &mut reopened)
                    .is_err()
            );
        }
    }

    #[test]
    fn retained_mount_conserves_original_pvc_uid_and_existing_readonly_collector_contract() {
        let (prepared, _, _, _) = super::super::tests::fixture();
        let spec = crate::launch_job::Spec {
            run_id: &prepared.run,
            image: &prepared.image,
            login_secret: &prepared.login,
            bound: prepared.bound,
            grace: prepared.grace,
        };
        let mut pvc = crate::launch_job::workspace_manifest(&spec);
        pvc["metadata"]["uid"] = serde_json::json!("original-volume-uid");
        pvc["metadata"]["resourceVersion"] = serde_json::json!("1");
        let mut job = crate::launch_job::recovery_manifest(&spec).unwrap();
        job["metadata"]["uid"] = serde_json::json!("trusted-collector-uid");
        job["metadata"]["resourceVersion"] = serde_json::json!("1");
        let mount = RetainedMount::from_observations(&spec, &pvc, &pvc, &job).unwrap();
        assert_eq!(mount.workspace_uid, "original-volume-uid");
        assert_eq!(mount.collector.uid(), "trusted-collector-uid");
        let mut replaced = pvc.clone();
        replaced["metadata"]["uid"] = serde_json::json!("replacement");
        assert!(RetainedMount::from_observations(&spec, &pvc, &replaced, &job).is_err());
        for mutation in [0, 1, 2] {
            let mut foreign = job.clone();
            match mutation {
                0 => {
                    foreign["spec"]["template"]["spec"]["containers"][0]["volumeMounts"][0]["readOnly"] =
                        serde_json::json!(false)
                }
                1 => foreign["metadata"]["namespace"] = serde_json::json!("boss"),
                _ => {
                    foreign["spec"]["template"]["spec"]["automountServiceAccountToken"] =
                        serde_json::json!(true)
                }
            }
            assert!(RetainedMount::from_observations(&spec, &pvc, &pvc, &foreign).is_err());
        }
    }

    #[test]
    fn exclusive_lock_links_permissions_and_readiness_overwrite_are_refused() {
        let (root, mut store) = seeded();
        assert!(FileStore::open(&root, store.owner, FixtureVerifier).is_err());
        let state = store.state().unwrap();
        assert_eq!(
            store.compare_swap(&state, &state),
            Err(CacheError::InvalidBinding)
        );
        drop(store);
        let linked = root.with_extension("linked");
        std::os::unix::fs::symlink(&root, &linked).unwrap();
        assert!(
            FileStore::open(
                &linked,
                rustix::process::geteuid().as_raw(),
                FixtureVerifier
            )
            .is_err()
        );
        // mode-bits-ok: deliberately non-private directory for a refusal control
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            FileStore::open(&root, rustix::process::geteuid().as_raw(), FixtureVerifier).is_err()
        );
    }
}
