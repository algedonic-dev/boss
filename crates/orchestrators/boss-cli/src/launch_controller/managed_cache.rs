//! Source checkpoint for6f3ef63c: cache bytes stay inside a private broker port.
//! Confined file persistence and metadata IPC are source adapters; no provider,
//! Secret, native mount/scope adapter or launch selector is installed here.

use super::{Outcome, Prepared};
use crate::{launch_binding::RunningBinding, launch_cluster::Bindings};
use serde_json::Value;

pub(crate) mod collection;
pub(crate) mod collector;
pub(crate) mod files;
pub(crate) mod ipc;
pub(crate) mod scope;
pub(crate) mod verification;

/// Metadata only. The private port retains installed/staged bytes separately;
/// cache bytes have no value type, serialization path or journal here.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Installed {
    credential: String,
    issuer: String,
    principal: String,
    generation: String,
    renewal_owner: String,
}
impl Installed {
    fn from_binding(b: &Bindings) -> Result<Self, CacheError> {
        if b.harness != "codex-exec"
            || b.login_file != "auth.json"
            || [
                &b.credential,
                &b.issuer,
                &b.principal,
                &b.storage_generation,
                &b.renewal_owner,
            ]
            .iter()
            .any(|s| s.trim().is_empty())
        {
            return Err(CacheError::InvalidBinding);
        }
        Ok(Self {
            credential: b.credential.clone(),
            issuer: b.issuer.clone(),
            principal: b.principal.clone(),
            generation: b.storage_generation.clone(),
            renewal_owner: b.renewal_owner.clone(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Lease {
    installed: Installed,
    run: String,
    nonce: uuid::Uuid,
    binding: Option<Value>,
}
impl Lease {
    /// Only a broker-acknowledged Running lease can supply private worker
    /// preparation metadata. This never includes a path, token or cache bytes.
    pub(crate) fn worker_plan(&self) -> Result<Value, CacheError> {
        let binding = self.binding.as_ref().ok_or(CacheError::InvalidBinding)?;
        if self.nonce.is_nil() || !binding.is_object() {
            return Err(CacheError::InvalidBinding);
        }
        Ok(
            serde_json::json!({"schema":1,"run":self.run,"nonce":self.nonce,
            "credential":self.installed.credential,"issuer":self.installed.issuer,
            "principal":self.installed.principal,"generation":self.installed.generation,"binding":binding}),
        )
    }
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum State {
    Ready(Installed),
    Leased(Lease),
    // Validated next-generation metadata, still unavailable. Exact durable
    // private publication/recovery is a future adapter obligation, not an
    // acknowledgement inferred from a possibly committed CAS error (review962f).
    Collected(Lease, Installed),
    Quarantined(Lease),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum CacheError {
    InvalidBinding,
    Unavailable,
    Conflict,
    Quarantined,
}

/// A trusted collector's metadata about privately retained/staged bytes.
/// `validated` means strict bounded format AND supported same-account metadata
/// were verified by that collector, never claims read out of worker output.
#[derive(Clone)]
pub(crate) struct Renewed {
    issuer: String,
    principal: String,
    generation: String,
    binding: Value,
    validated: bool,
}

/// The existing controller's future private broker port, not a second launcher.
/// CAS is atomic/durable and returns true only after exact readback. Ready->lease
/// also checks installed/expiry/revocation/consumer metadata in that transaction.
/// Collected commits only the exact privately staged collection for this
/// lease/generation. It remains unavailable even if the CAS commits and then
/// loses its acknowledgement. This kernel supplies no Collected->Ready door:
/// durable private publication/recovery must establish that separate effect.
/// Collection checks the current bound lease and native incarnation BEFORE any
/// private read, retains full originals privately, and exposes no cache bytes or
/// raw private diagnostics here. There is no force, expiry-unlock or release door.
pub(crate) trait Store {
    fn compare_swap(&mut self, expected: &State, next: &State) -> Result<bool, CacheError>;
    fn collect(&mut self, lease: &Lease) -> Result<Renewed, CacheError>;
}

pub(crate) fn acquire<S: Store>(
    p: &Prepared,
    b: &Bindings,
    nonce: uuid::Uuid,
    store: &mut S,
) -> Result<Lease, CacheError> {
    let installed = Installed::from_binding(b)?;
    if nonce.is_nil()
        || p.login != b.secret
        || p.login_file != b.login_file
        || p.home_env != "CODEX_HOME"
        || uuid::Uuid::parse_str(&p.run)
            .ok()
            .is_none_or(|id| id.is_nil() || id.to_string() != p.run)
    {
        return Err(CacheError::InvalidBinding);
    }
    let lease = Lease {
        installed: installed.clone(),
        run: p.run.clone(),
        nonce,
        binding: None,
    };
    if !store.compare_swap(&State::Ready(installed), &State::Leased(lease.clone()))? {
        return Err(CacheError::Conflict);
    }
    Ok(lease)
}

pub(crate) fn bind<S: Store>(
    lease: &Lease,
    running: &RunningBinding,
    store: &mut S,
) -> Result<Lease, CacheError> {
    if lease.binding.is_some() {
        return Err(CacheError::InvalidBinding);
    }
    if !running.matches_run(&lease.run) {
        return quarantine(lease, store);
    }
    let mut bound = lease.clone();
    bound.binding = Some(running.identity());
    if !store.compare_swap(&State::Leased(lease.clone()), &State::Leased(bound.clone()))? {
        return Err(CacheError::Conflict);
    }
    Ok(bound)
}

pub(crate) fn quarantine<S: Store, T>(lease: &Lease, store: &mut S) -> Result<T, CacheError> {
    // Even a failed quarantine leaves an unavailable held lease; no old token
    // is retried or marked ready. A changed state belongs to its actual owner.
    if store.compare_swap(
        &State::Leased(lease.clone()),
        &State::Quarantined(lease.clone()),
    )? {
        Err(CacheError::Quarantined)
    } else {
        Err(CacheError::Conflict)
    }
}

pub(crate) fn finish<S: Store>(
    lease: &Lease,
    outcome: &Outcome,
    store: &mut S,
) -> Result<Installed, CacheError> {
    let held = State::Leased(lease.clone());
    // A stale receipt/replay never opens a cache owned by another generation.
    if !store.compare_swap(&held, &held)? {
        return Err(CacheError::Conflict);
    }
    let Some(binding) = &lease.binding else {
        return quarantine(lease, store);
    };
    if outcome.ending != "native_exited"
        || !outcome.stop_observed
        || !outcome.evidence_complete
        || outcome.job_uid.as_deref() != binding["job_uid"].as_str()
    {
        return quarantine(lease, store);
    }
    let renewed = match store.collect(lease) {
        Ok(value) => value,
        Err(_) => return quarantine(lease, store),
    };
    if !renewed.validated
        || renewed.issuer != lease.installed.issuer
        || renewed.principal != lease.installed.principal
        || renewed.binding != *binding
        || renewed.generation.trim().is_empty()
        || renewed.generation == lease.installed.generation
    {
        return quarantine(lease, store);
    }
    let next = Installed {
        generation: renewed.generation,
        ..lease.installed.clone()
    };
    // A successful finish means collection was staged, not publication. Never
    // expose Ready here: a failed acknowledgement cannot undo a durable save.
    match store.compare_swap(&held, &State::Collected(lease.clone(), next.clone())) {
        Ok(true) => Ok(next),
        _ => quarantine(lease, store),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{launch_binding::JobBinding, launch_cluster::Bindings};

    pub(super) struct MemoryStore {
        state: State,
        admissible: bool,
        collection: Result<Renewed, CacheError>,
        collections: usize,
        refuse_publish: bool,
        refuse_quarantine: bool,
    }
    impl Store for MemoryStore {
        fn compare_swap(&mut self, expected: &State, next: &State) -> Result<bool, CacheError> {
            if self.refuse_quarantine && matches!(next, State::Quarantined(_)) {
                return Ok(false);
            }
            if matches!(next, State::Leased(_)) && !self.admissible {
                return Err(CacheError::Unavailable);
            }
            if self.refuse_publish && matches!(next, State::Collected(_, _)) {
                return Ok(false);
            }
            if self.state != *expected {
                return Ok(false);
            }
            self.state = next.clone();
            Ok(true)
        }
        fn collect(&mut self, _: &Lease) -> Result<Renewed, CacheError> {
            self.collections += 1;
            self.collection.clone()
        }
    }
    pub(super) fn fixture() -> (
        super::super::Prepared,
        Bindings,
        RunningBinding,
        MemoryStore,
    ) {
        let mut p = super::super::tests::prepared();
        p.login_file = "auth.json".into();
        let b = Bindings {
            schema: 1,
            generation: "a".repeat(40),
            harness: "codex-exec".into(),
            harness_sha256: "b".repeat(64),
            image: p.image.clone(),
            image_platform: "linux/amd64".into(),
            binary: "/usr/local/bin/codex".into(),
            binary_version: "codex-cli 0.159.3".into(),
            argv_help_sha256: "c".repeat(64),
            helper_sha256: "d".repeat(64),
            kubectl_sha256: "e".repeat(64),
            credential: "dedicated-managed-login".into(),
            issuer: "dedicated-issuer".into(),
            principal: "authorized-fixture-principal".into(),
            scopes: vec!["managed-exec".into()],
            storage_generation: "installed-one".into(),
            login_file: "auth.json".into(),
            secret: p.login.clone(),
            renewal_owner: "trusted-broker".into(),
        };
        let (_, job, pods) = super::super::tests::objects(&p);
        let spec = crate::launch_job::Spec {
            run_id: &p.run,
            image: &p.image,
            login_secret: &p.login,
            bound: p.bound,
            grace: p.grace,
        };
        let native = JobBinding::from_created(&job, &spec)
            .unwrap()
            .observe_running(&job, &pods)
            .unwrap();
        let installed = Installed::from_binding(&b).unwrap();
        let store = MemoryStore {
            state: State::Ready(installed),
            admissible: true,
            collections: 0,
            refuse_publish: false,
            refuse_quarantine: false,
            collection: Ok(Renewed {
                issuer: b.issuer.clone(),
                principal: b.principal.clone(),
                generation: "installed-two".into(),
                binding: native.identity(),
                validated: true,
            }),
        };
        (p, b, native, store)
    }
    fn terminal() -> super::super::Outcome {
        super::super::Outcome {
            ending: "native_exited".into(),
            job_uid: Some("job-uid".into()),
            stop_observed: true,
            evidence_complete: true,
            workspace_retained: true,
            observations: vec![],
        }
    }
    fn owned(
        p: &super::super::Prepared,
        b: &Bindings,
        native: &RunningBinding,
        store: &mut MemoryStore,
    ) -> Lease {
        let lease = acquire(p, b, uuid::Uuid::new_v4(), store).unwrap();
        bind(&lease, native, store).unwrap()
    }

    #[test]
    fn one_generation_has_one_durable_owner_before_any_private_collection() {
        let (p, b, native, mut store) = fixture();
        let first = acquire(&p, &b, uuid::Uuid::new_v4(), &mut store).unwrap();
        assert_eq!(store.collections, 0);
        let mut second = p.clone();
        second.run = uuid::Uuid::new_v4().to_string();
        assert_eq!(
            acquire(&second, &b, uuid::Uuid::new_v4(), &mut store),
            Err(CacheError::Conflict)
        );
        assert!(matches!(&store.state, State::Leased(lease) if lease == &first));
        let bound = bind(&first, &native, &mut store).unwrap();
        assert_eq!(bound.binding, Some(native.identity()));
    }

    #[test]
    fn only_bound_successful_terminal_and_same_principal_stage_the_next_generation() {
        let (p, b, native, mut store) = fixture();
        let lease = owned(&p, &b, &native, &mut store);
        let next = finish(&lease, &terminal(), &mut store).unwrap();
        assert_eq!(next.generation, "installed-two");
        assert_eq!(next.principal, b.principal);
        assert_eq!(store.state, State::Collected(lease.clone(), next));
        assert_eq!(store.collections, 1);
        assert_eq!(
            finish(&lease, &terminal(), &mut store),
            Err(CacheError::Conflict)
        );
    }

    #[test]
    fn interrupted_unknown_and_replaced_terminals_quarantine_without_collecting() {
        for bad in 0..5 {
            let (p, b, native, mut store) = fixture();
            let lease = owned(&p, &b, &native, &mut store);
            let mut outcome = terminal();
            match bad {
                0 => outcome.ending = "interrupted".into(),
                1 => outcome.stop_observed = false,
                2 => outcome.evidence_complete = false,
                3 => outcome.job_uid = Some("replacement".into()),
                _ => outcome.job_uid = None,
            }
            assert_eq!(
                finish(&lease, &outcome, &mut store),
                Err(CacheError::Quarantined)
            );
            assert_eq!(store.collections, 0);
            assert_eq!(store.state, State::Quarantined(lease));
            assert_eq!(
                acquire(&p, &b, uuid::Uuid::new_v4(), &mut store),
                Err(CacheError::Conflict)
            );
        }
    }

    #[test]
    fn collection_identity_format_failure_and_same_generation_never_release_the_stream() {
        for bad in 0..6 {
            let (p, b, native, mut store) = fixture();
            let lease = owned(&p, &b, &native, &mut store);
            let mut collected = store.collection.clone().unwrap();
            match bad {
                0 => collected.principal = "foreign".into(),
                1 => collected.issuer = "foreign".into(),
                2 => collected.binding["pod_uid"] = serde_json::json!("replacement"),
                3 => collected.validated = false,
                4 => collected.generation = b.storage_generation.clone(),
                _ => collected.generation.clear(),
            }
            store.collection = Ok(collected);
            assert_eq!(
                finish(&lease, &terminal(), &mut store),
                Err(CacheError::Quarantined)
            );
            assert_eq!(store.state, State::Quarantined(lease));
        }
        let (p, b, native, mut store) = fixture();
        let lease = owned(&p, &b, &native, &mut store);
        store.collection = Err(CacheError::Unavailable);
        assert_eq!(
            finish(&lease, &terminal(), &mut store),
            Err(CacheError::Quarantined)
        );
        assert_eq!(store.state, State::Quarantined(lease));
    }

    #[test]
    fn unavailable_expired_revoked_or_conflicting_store_never_admits_a_run() {
        let (p, b, _, mut store) = fixture();
        store.admissible = false;
        assert_eq!(
            acquire(&p, &b, uuid::Uuid::new_v4(), &mut store),
            Err(CacheError::Unavailable)
        );
        assert!(matches!(store.state, State::Ready(_)));
        assert_eq!(store.collections, 0);
    }

    #[test]
    fn refused_quarantine_is_not_claimed_as_a_recorded_quarantine() {
        let (p, b, native, mut store) = fixture();
        let lease = owned(&p, &b, &native, &mut store);
        store.refuse_quarantine = true;
        let mut bad = terminal();
        bad.stop_observed = false;
        assert_eq!(finish(&lease, &bad, &mut store), Err(CacheError::Conflict));
        assert_eq!(store.state, State::Leased(lease));
        assert_eq!(store.collections, 0);
    }

    #[test]
    fn a_foreign_run_binding_quarantines_before_private_materialization() {
        let (mut p, b, _, mut store) = fixture();
        p.run = uuid::Uuid::new_v4().to_string();
        let lease = acquire(&p, &b, uuid::Uuid::new_v4(), &mut store).unwrap();
        let (_, _, foreign, _) = fixture();
        assert_eq!(
            bind(&lease, &foreign, &mut store),
            Err(CacheError::Quarantined)
        );
        assert_eq!(store.state, State::Quarantined(lease));
        assert_eq!(store.collections, 0);
    }

    #[test]
    fn generation_cas_refusal_keeps_the_lease_and_never_claims_collection() {
        let (p, b, native, mut store) = fixture();
        let lease = owned(&p, &b, &native, &mut store);
        store.refuse_publish = true;
        assert_eq!(
            finish(&lease, &terminal(), &mut store),
            Err(CacheError::Quarantined)
        );
        assert_eq!(store.state, State::Quarantined(lease));
        assert_eq!(store.collections, 1);
    }

    #[test]
    fn reviewer_uncertain_publication_cannot_admit_a_second_run() {
        struct AmbiguousStore(MemoryStore);
        impl Store for AmbiguousStore {
            fn compare_swap(&mut self, expected: &State, next: &State) -> Result<bool, CacheError> {
                let saved = self.0.compare_swap(expected, next)?;
                if saved && matches!(next, State::Collected(_, _)) {
                    // Durable commit succeeded, but its exact readback failed.
                    return Err(CacheError::Unavailable);
                }
                Ok(saved)
            }
            fn collect(&mut self, lease: &Lease) -> Result<Renewed, CacheError> {
                self.0.collect(lease)
            }
        }
        let (p, b, native, mut initial) = fixture();
        let lease = owned(&p, &b, &native, &mut initial);
        let mut store = AmbiguousStore(initial);
        assert!(finish(&lease, &terminal(), &mut store).is_err());
        assert!(matches!(&store.0.state, State::Collected(owner, next)
            if owner == &lease && next.generation == "installed-two"
                && next.principal == lease.installed.principal));
        assert_eq!(store.0.collections, 1);
        let mut renewed_binding = b.clone();
        renewed_binding.storage_generation = "installed-two".into();
        let mut second = p.clone();
        second.run = uuid::Uuid::new_v4().to_string();
        assert!(
            acquire(&second, &renewed_binding, uuid::Uuid::new_v4(), &mut store).is_err(),
            "uncertain publication returned the stream to Ready and admitted a second run"
        );
        assert_eq!(
            finish(&lease, &terminal(), &mut store),
            Err(CacheError::Conflict)
        );
        assert_eq!(store.0.collections, 1);
    }

    #[test]
    fn reviewer_invalid_owner_nonce_and_principal_never_read_private_cache() {
        for bad in 0..5 {
            let (mut p, mut b, _, mut store) = fixture();
            let mut nonce = uuid::Uuid::new_v4();
            match bad {
                0 => nonce = uuid::Uuid::nil(),
                1 => p.run = uuid::Uuid::nil().to_string(),
                2 => p.login = "other-issuer-input".into(),
                3 => b.principal = "   ".into(),
                _ => b.renewal_owner.clear(),
            }
            assert_eq!(
                acquire(&p, &b, nonce, &mut store),
                Err(CacheError::InvalidBinding)
            );
            assert!(matches!(store.state, State::Ready(_)));
            assert_eq!(store.collections, 0);
        }
    }

    #[test]
    fn reviewer_stale_unbound_lease_cannot_collect_or_change_current_owner() {
        let (p, b, native, mut store) = fixture();
        let unbound = acquire(&p, &b, uuid::Uuid::new_v4(), &mut store).unwrap();
        let current = bind(&unbound, &native, &mut store).unwrap();
        assert_eq!(
            finish(&unbound, &terminal(), &mut store),
            Err(CacheError::Conflict)
        );
        assert_eq!(
            bind(&unbound, &native, &mut store),
            Err(CacheError::Conflict)
        );
        assert_eq!(store.state, State::Leased(current.clone()));
        assert_eq!(store.collections, 0);
        let mut wrong = store.collection.clone().unwrap();
        wrong.binding["container_id"] = serde_json::json!("containerd://replacement");
        store.collection = Ok(wrong);
        assert_eq!(
            finish(&current, &terminal(), &mut store),
            Err(CacheError::Quarantined)
        );
        assert_eq!(store.collections, 1);
        assert_eq!(store.state, State::Quarantined(current));
        assert!(acquire(&p, &b, uuid::Uuid::new_v4(), &mut store).is_err());
    }

    #[test]
    fn successful_collection_stays_unavailable_until_exact_private_recovery() {
        let (p, b, native, mut store) = fixture();
        let lease = owned(&p, &b, &native, &mut store);
        let next = finish(&lease, &terminal(), &mut store).unwrap();
        let mut next_binding = b.clone();
        next_binding.storage_generation = next.generation;
        assert!(acquire(&p, &next_binding, uuid::Uuid::new_v4(), &mut store).is_err());
    }

    #[test]
    fn uncertain_acquire_and_running_bind_conserve_the_unavailable_owner() {
        struct CommittedError {
            inner: MemoryStore,
            binding: bool,
        }
        impl Store for CommittedError {
            fn compare_swap(&mut self, expected: &State, next: &State) -> Result<bool, CacheError> {
                let saved = self.inner.compare_swap(expected, next)?;
                if saved && matches!(next, State::Leased(l) if l.binding.is_some() == self.binding)
                {
                    return Err(CacheError::Unavailable);
                }
                Ok(saved)
            }
            fn collect(&mut self, lease: &Lease) -> Result<Renewed, CacheError> {
                self.inner.collect(lease)
            }
        }
        for binding in [false, true] {
            let (p, b, native, mut initial) = fixture();
            let lease = if binding {
                Some(acquire(&p, &b, uuid::Uuid::new_v4(), &mut initial).unwrap())
            } else {
                None
            };
            let mut store = CommittedError {
                inner: initial,
                binding,
            };
            let result = if let Some(lease) = lease {
                bind(&lease, &native, &mut store)
            } else {
                acquire(&p, &b, uuid::Uuid::new_v4(), &mut store)
            };
            assert_eq!(result, Err(CacheError::Unavailable));
            assert!(matches!(store.inner.state, State::Leased(_)));
            assert!(acquire(&p, &b, uuid::Uuid::new_v4(), &mut store).is_err());
            assert_eq!(store.inner.collections, 0);
        }
    }
    #[test]
    fn private_worker_plan_requires_the_exact_running_lease_and_contains_only_metadata() {
        let (prepared, binding, running, mut store) = fixture();
        let lease = acquire(&prepared, &binding, uuid::Uuid::new_v4(), &mut store).unwrap();
        assert_eq!(lease.worker_plan(), Err(CacheError::InvalidBinding));
        let bound = bind(&lease, &running, &mut store).unwrap();
        let plan = bound.worker_plan().unwrap();
        assert_eq!(
            plan,
            serde_json::json!({"schema":1,"run":bound.run,"nonce":bound.nonce,
            "credential":binding.credential,"issuer":binding.issuer,"principal":binding.principal,
            "generation":binding.storage_generation,"binding":running.identity()})
        );
    }
}
