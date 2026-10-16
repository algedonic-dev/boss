//! Metadata-only adapter for the EXISTING private broker, not another launcher.
//! It is not mounted or configured natively. Signature membership is followed
//! by independent registered-run/credential/native scope checks in the broker.
//! No actor assertion, filesystem path, cache bytes or release verb exists here.
use super::CacheError;
use async_trait::async_trait;
use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
};
use boss_jobs::credentials::broker_stage::{StageKeys, Terms, TrustedKey};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub(crate) const HEADER: &str = "x-boss-managed-cache-workload";
const LIMIT: usize = 32 * 1024;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    pub schema: u64,
    pub run: uuid::Uuid,
    pub credential: String,
    pub nonce: uuid::Uuid,
    pub phase: Phase,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub(crate) enum Phase {
    Acquire { generation: String },
    Bind,
    Collect,
    Reconcile,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub(crate) enum Reply {
    LeaseHeld {
        nonce: uuid::Uuid,
    },
    CollectedUnavailable {
        nonce: uuid::Uuid,
        generation: String,
    },
    ReconciledUnavailable {
        nonce: uuid::Uuid,
        generation: String,
    },
    Refused {
        reason: CacheError,
    },
}

pub(crate) struct Caller {
    pub pod_uid: uuid::Uuid,
    pub service_account_uid: uuid::Uuid,
    pub expires_at: i64,
}

/// The broker must independently resolve the registered active run, its real
/// observer, installed issuer/principal/consumer and current native bindings.
/// Bind/collect use its own native reader; no caller terminal boolean is taken.
/// Missing registry, scope, mount or account verifier refuses BEFORE bytes.
/// Neither an opaque enrollment receipt nor a verified token supplies scope.
#[async_trait]
pub(crate) trait ScopedBroker: Send + Sync {
    async fn apply(&self, caller: Caller, request: Request) -> Result<Reply, CacheError>;
}
/// Resolved exclusively by the broker's registered-run/native observer port.
/// Paths and verifier come from trusted broker installation, never from IPC.
/// The native port remains uninstalled; no receipt UUID constructs this value.
pub(crate) struct Scope<V: super::files::PrivateVerifier> {
    pub prepared: super::super::Prepared,
    pub bindings: crate::launch_cluster::Bindings,
    pub running: Option<crate::launch_binding::RunningBinding>,
    pub terminal: Option<super::super::Outcome>,
    pub store_root: std::path::PathBuf,
    pub owner: u32,
    pub verifier: V,
    pub retained: Option<super::files::RetainedCache>,
    pub controller_pod_uid: uuid::Uuid,
    pub controller_service_account_uid: uuid::Uuid,
}
#[async_trait]
pub(crate) trait ScopePort<V: super::files::PrivateVerifier>: Send + Sync {
    async fn resolve(&self, caller: &Caller, request: &Request) -> Result<Scope<V>, CacheError>;
}
/// Same one-store kernel, reached through metadata IPC. Each operation opens
/// the private OS lock and reads durable owner state; no in-memory lease table.
pub(crate) struct FileBroker<V: super::files::PrivateVerifier> {
    pub scope: Arc<dyn ScopePort<V>>,
}
#[async_trait]
impl<V: super::files::PrivateVerifier + 'static> ScopedBroker for FileBroker<V> {
    async fn apply(&self, caller: Caller, request: Request) -> Result<Reply, CacheError> {
        let scope = self.scope.resolve(&caller, &request).await?;
        if scope.prepared.run != request.run.to_string()
            || scope.bindings.credential != request.credential
            || scope.controller_pod_uid != caller.pod_uid
            || scope.controller_service_account_uid != caller.service_account_uid
            || caller.expires_at <= boss_clock_client::wall_now().timestamp()
        {
            return Err(CacheError::InvalidBinding);
        }
        tokio::task::spawn_blocking(move || {
            let mut store =
                super::files::FileStore::open(&scope.store_root, scope.owner, scope.verifier)?;
            match request.phase {
                Phase::Acquire { generation } => {
                    if generation != scope.bindings.storage_generation {
                        return Err(CacheError::InvalidBinding);
                    }
                    let lease = super::acquire(
                        &scope.prepared,
                        &scope.bindings,
                        request.nonce,
                        &mut store,
                    )?;
                    Ok(Reply::LeaseHeld { nonce: lease.nonce })
                }
                phase => {
                    let lease = match store.state()? {
                        super::State::Leased(lease) | super::State::Collected(lease, _) => lease,
                        _ => return Err(CacheError::Conflict),
                    };
                    if lease.run != scope.prepared.run
                        || lease.nonce != request.nonce
                        || lease.installed != super::Installed::from_binding(&scope.bindings)?
                    {
                        return Err(CacheError::InvalidBinding);
                    }
                    match phase {
                        Phase::Bind => {
                            let running = scope.running.ok_or(CacheError::Unavailable)?;
                            let bound = super::bind(&lease, &running, &mut store)?;
                            Ok(Reply::LeaseHeld { nonce: bound.nonce })
                        }
                        Phase::Collect => {
                            let terminal = scope.terminal.ok_or(CacheError::Unavailable)?;
                            let running = scope.running.ok_or(CacheError::Unavailable)?;
                            if lease.binding.as_ref() != Some(&running.identity())
                                || !terminal.workspace_retained
                            {
                                return Err(CacheError::InvalidBinding);
                            }
                            store.attach_retained(scope.retained.ok_or(CacheError::Unavailable)?);
                            let next = super::finish(&lease, &terminal, &mut store)?;
                            Ok(Reply::CollectedUnavailable {
                                nonce: lease.nonce,
                                generation: next.generation,
                            })
                        }
                        Phase::Reconcile => {
                            let generation = store.reconcile_collected(&lease)?;
                            Ok(Reply::ReconciledUnavailable {
                                nonce: lease.nonce,
                                generation,
                            })
                        }
                        Phase::Acquire { .. } => Err(CacheError::InvalidBinding),
                    }
                }
            }
        })
        .await
        .map_err(|_| CacheError::Unavailable)?
    }
}

pub(crate) struct Authentication {
    pub terms: Terms,
    pub service_account_uid: uuid::Uuid,
    pub keys: Arc<dyn StageKeys>,
}
struct SafeKeys(Arc<dyn StageKeys>);
impl StageKeys for SafeKeys {
    fn trusted(&self) -> Result<Vec<TrustedKey>, String> {
        self.0
            .trusted()
            .map_err(|_| "private broker trusted keys unavailable".into())
    }
}
pub(crate) struct Context {
    pub authentication: Option<Arc<Authentication>>,
    pub broker: Arc<dyn ScopedBroker>,
}

pub(super) fn authenticate(
    token: &str,
    configured: &Authentication,
    now: i64,
) -> Result<Caller, CacheError> {
    use base64::Engine;
    let verified = boss_jobs::credentials::broker_stage::verify(
        token,
        &configured.terms,
        &SafeKeys(configured.keys.clone()),
        now,
    )
    .map_err(|_| CacheError::Unavailable)?;
    let payload = token.split('.').nth(1).ok_or(CacheError::Unavailable)?;
    let claims: serde_json::Value = serde_json::from_slice(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload)
            .map_err(|_| CacheError::Unavailable)?,
    )
    .map_err(|_| CacheError::Unavailable)?;
    let identity = |value: &serde_json::Value| -> Result<uuid::Uuid, CacheError> {
        value
            .as_str()
            .and_then(|s| uuid::Uuid::parse_str(s).ok())
            .filter(|id| !id.is_nil())
            .ok_or(CacheError::Unavailable)
    };
    let service_account_uid = identity(&claims["kubernetes.io"]["serviceaccount"]["uid"])?;
    if service_account_uid != configured.service_account_uid {
        return Err(CacheError::Unavailable);
    }
    Ok(Caller {
        service_account_uid,
        pod_uid: identity(&claims["kubernetes.io"]["pod"]["uid"])?,
        expires_at: verified.expires_at,
    })
}

async fn handle(
    State(context): State<Arc<Context>>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let refused = |reason| (StatusCode::FORBIDDEN, axum::Json(Reply::Refused { reason }));
    let Some(authentication) = context.authentication.clone() else {
        return refused(CacheError::Unavailable);
    };
    let values = headers.get_all(HEADER).iter().collect::<Vec<_>>();
    let [header] = values.as_slice() else {
        return refused(CacheError::Unavailable);
    };
    let Ok(token) = header.to_str().map(str::to_owned) else {
        return refused(CacheError::Unavailable);
    };
    let now = boss_clock_client::wall_now().timestamp();
    let caller =
        tokio::task::spawn_blocking(move || authenticate(&token, &authentication, now)).await;
    let Ok(Ok(caller)) = caller else {
        return refused(CacheError::Unavailable);
    };
    if body.len() > LIMIT {
        return refused(CacheError::InvalidBinding);
    }
    // Deserializer errors may quote a private unknown field. Never expose them.
    let Ok(request) = serde_json::from_slice::<Request>(&body) else {
        return refused(CacheError::InvalidBinding);
    };
    if request.schema != 1
        || request.run.is_nil()
        || request.nonce.is_nil()
        || !crate::launch_job::is_name(&request.credential)
    {
        return refused(CacheError::InvalidBinding);
    }
    match context.broker.apply(caller, request).await {
        Ok(reply) => (StatusCode::OK, axum::Json(reply)),
        Err(reason) => refused(reason),
    }
}

/// Source router only. No listener or manifest mounts it; unconfigured is
/// always a refusal, unlike the older runner-stage door's compatibility mode.
pub(crate) fn router(context: Context) -> axum::Router {
    axum::Router::new()
        .route("/private/managed-cache", post(handle))
        .layer(axum::extract::DefaultBodyLimit::max(LIMIT))
        .with_state(Arc::new(context))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use rsa::signature::{SignatureEncoding, Signer};
    struct Keys(rsa::RsaPublicKey);
    impl StageKeys for Keys {
        fn trusted(&self) -> Result<Vec<TrustedKey>, String> {
            Ok(vec![TrustedKey {
                kid: "fixture".into(),
                key: self.0.clone(),
            }])
        }
    }
    fn fixture() -> (Authentication, String, uuid::Uuid) {
        let key = rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap();
        let uid = uuid::Uuid::new_v4();
        let pod = uuid::Uuid::new_v4();
        let terms = Terms {
            issuer: "https://fixture.invalid".into(),
            audience: "private-cache-fixture".into(),
            namespace: "fixture-namespace".into(),
            service_account: "fixture-controller".into(),
        };
        let now = chrono::Utc::now().timestamp();
        let claims = serde_json::json!({"iss":terms.issuer,"aud":[terms.audience],
            "sub":"system:serviceaccount:fixture-namespace:fixture-controller", "iat":now,"exp":now+60,
            "kubernetes.io":{"namespace":terms.namespace,"serviceaccount":{"name":terms.service_account,"uid":uid},"pod":{"uid":pod}}});
        let encode = |raw: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw);
        let body = format!(
            "{}.{}",
            encode(br#"{"alg":"RS256","kid":"fixture"}"#),
            encode(&serde_json::to_vec(&claims).unwrap())
        );
        let signature =
            rsa::pkcs1v15::SigningKey::<rsa::sha2::Sha256>::new(key.clone()).sign(body.as_bytes());
        let token = format!("{body}.{}", encode(&signature.to_vec()));
        (
            Authentication {
                terms,
                service_account_uid: uid,
                keys: Arc::new(Keys(key.to_public_key())),
            },
            token,
            pod,
        )
    }
    #[test]
    fn an_authenticated_scoped_metadata_request_has_a_broker_ipc_adapter() {
        let (configured, token, pod) = fixture();
        let caller = authenticate(&token, &configured, chrono::Utc::now().timestamp()).unwrap();
        assert_eq!(caller.pod_uid, pod);
        assert_eq!(caller.service_account_uid, configured.service_account_uid);
        assert!(caller.expires_at > chrono::Utc::now().timestamp());
    }
    #[test]
    fn forged_wrong_uid_wrong_audience_and_expired_workload_tokens_refuse() {
        let (mut configured, token, _) = fixture();
        let now = chrono::Utc::now().timestamp();
        assert!(authenticate("synthetic-private-material", &configured, now).is_err());
        assert!(authenticate(&token, &configured, now + 600).is_err());
        configured.service_account_uid = uuid::Uuid::new_v4();
        assert!(authenticate(&token, &configured, now).is_err());
        let (mut configured, token, _) = fixture();
        configured.terms.audience = "wrong".into();
        assert!(authenticate(&token, &configured, now).is_err());
    }
    struct RefusingBroker;
    #[async_trait]
    impl ScopedBroker for RefusingBroker {
        async fn apply(&self, _: Caller, _: Request) -> Result<Reply, CacheError> {
            Err(CacheError::InvalidBinding)
        }
    }
    #[tokio::test]
    async fn uninstalled_router_refuses_at_the_http_boundary() {
        use tower::ServiceExt;
        let service = router(Context {
            authentication: None,
            broker: Arc::new(RefusingBroker),
        });
        let response = service
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/private/managed-cache")
                    .body(axum::body::Body::from("synthetic private body"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let body = axum::body::to_bytes(response.into_body(), LIMIT)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
            serde_json::json!({"kind":"Refused","reason":"Unavailable"})
        );
    }
    #[tokio::test]
    async fn authenticated_membership_never_supplies_run_scope_and_diagnostics_are_fixed() {
        let (authentication, token, _) = fixture();
        let context = Arc::new(Context {
            authentication: Some(Arc::new(authentication)),
            broker: Arc::new(RefusingBroker),
        });
        let body = serde_json::to_vec(&Request {
            schema: 1,
            run: uuid::Uuid::new_v4(),
            credential: "fixture-login".into(),
            nonce: uuid::Uuid::new_v4(),
            phase: Phase::Bind,
        })
        .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(HEADER, token.parse().unwrap());
        let response = handle(
            State(context.clone()),
            headers.clone(),
            Bytes::from(body.clone()),
        )
        .await
        .into_response();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let bytes = axum::body::to_bytes(response.into_body(), LIMIT)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
            serde_json::json!({"kind":"Refused","reason":"InvalidBinding"})
        );
        let mut private = serde_json::from_slice::<serde_json::Value>(&body).unwrap();
        private["synthetic-private-unknown-field"] = serde_json::json!("synthetic-private-value");
        let response = handle(
            State(context.clone()),
            headers.clone(),
            Bytes::from(serde_json::to_vec(&private).unwrap()),
        )
        .await
        .into_response();
        let bytes = axum::body::to_bytes(response.into_body(), LIMIT)
            .await
            .unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("synthetic-private"));
        headers.append(HEADER, token.parse().unwrap());
        assert_eq!(
            handle(State(context), headers, Bytes::from(body))
                .await
                .into_response()
                .status(),
            StatusCode::FORBIDDEN
        );
        let unavailable = Arc::new(Context {
            authentication: None,
            broker: Arc::new(RefusingBroker),
        });
        assert_eq!(
            handle(State(unavailable), HeaderMap::new(), Bytes::new())
                .await
                .into_response()
                .status(),
            StatusCode::FORBIDDEN
        );
    }

    struct ScopeVerifier;
    impl super::super::files::PrivateVerifier for ScopeVerifier {
        fn verify(&mut self, bytes: &[u8], principal: &str) -> Result<Vec<u8>, CacheError> {
            if bytes == b"synthetic private cache" && principal == "authorized-fixture-principal" {
                Ok(bytes.to_vec())
            } else {
                Err(CacheError::InvalidBinding)
            }
        }
    }
    struct FixtureScope {
        root: std::path::PathBuf,
        pod: uuid::Uuid,
        account: uuid::Uuid,
    }
    #[async_trait]
    impl ScopePort<ScopeVerifier> for FixtureScope {
        async fn resolve(
            &self,
            _: &Caller,
            _: &Request,
        ) -> Result<Scope<ScopeVerifier>, CacheError> {
            let (prepared, bindings, running, _) = super::super::tests::fixture();
            Ok(Scope {
                prepared,
                bindings,
                running: Some(running),
                terminal: None,
                store_root: self.root.clone(),
                owner: rustix::process::geteuid().as_raw(),
                verifier: ScopeVerifier,
                retained: None,
                controller_pod_uid: self.pod,
                controller_service_account_uid: self.account,
            })
        }
    }
    #[tokio::test]
    async fn file_broker_checks_scope_before_files_and_preserves_one_durable_owner() {
        use std::os::unix::fs::PermissionsExt;
        let root = boss_testing::scratch_dir("managed-ipc-store");
        // mode-bits-ok: private broker fixture directory, never an executable file
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let (prepared, bindings, _, _) = super::super::tests::fixture();
        for (name, bytes) in [
            (
                "state.json",
                serde_json::to_vec(&super::super::State::Ready(
                    super::super::Installed::from_binding(&bindings).unwrap(),
                ))
                .unwrap(),
            ),
            ("installed.auth", b"synthetic private cache".to_vec()),
        ] {
            std::fs::write(root.join(name), bytes).unwrap();
            std::fs::set_permissions(root.join(name), std::fs::Permissions::from_mode(0o600))
                .unwrap();
        }
        let pod = uuid::Uuid::new_v4();
        let account = uuid::Uuid::new_v4();
        let broker = FileBroker {
            scope: Arc::new(FixtureScope { root, pod, account }),
        };
        let caller = || Caller {
            pod_uid: pod,
            service_account_uid: account,
            expires_at: chrono::Utc::now().timestamp() + 60,
        };
        let request = Request {
            schema: 1,
            run: uuid::Uuid::parse_str(&prepared.run).unwrap(),
            credential: bindings.credential.clone(),
            nonce: uuid::Uuid::new_v4(),
            phase: Phase::Acquire {
                generation: bindings.storage_generation.clone(),
            },
        };
        let mut foreign = caller();
        foreign.pod_uid = uuid::Uuid::new_v4();
        assert!(broker.apply(foreign, request.clone()).await.is_err());
        let mut foreign_account = caller();
        foreign_account.service_account_uid = uuid::Uuid::new_v4();
        assert!(
            broker
                .apply(foreign_account, request.clone())
                .await
                .is_err()
        );
        assert!(
            matches!(broker.apply(caller(), request.clone()).await.unwrap(), Reply::LeaseHeld{nonce} if nonce==request.nonce)
        );
        let mut competing = request.clone();
        competing.nonce = uuid::Uuid::new_v4();
        assert!(broker.apply(caller(), competing).await.is_err());
        let mut bound = request;
        bound.phase = Phase::Bind;
        assert!(matches!(
            broker.apply(caller(), bound.clone()).await.unwrap(),
            Reply::LeaseHeld { .. }
        ));
        bound.phase = Phase::Collect;
        assert!(
            broker.apply(caller(), bound).await.is_err(),
            "no native terminal/mount proof is not collection"
        );
    }
}
