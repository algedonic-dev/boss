use super::*;
use ipc::ScopePort;
use std::sync::Mutex;
const GENERATION: &str = "2026-10-14T00:00:00+00:00";
struct Fixture {
    record: Mutex<Option<Record>>,
    run: Value,
    objects: Vec<(Resource, String, Value)>,
    credential: Value,
}
#[async_trait]
impl BrokerRecords for Fixture {
    async fn read(&self, _: uuid::Uuid) -> Result<Record, CacheError> {
        self.record
            .lock()
            .unwrap()
            .clone()
            .ok_or(CacheError::Unavailable)
    }
}
#[async_trait]
impl Metadata for Fixture {
    async fn run(&self, _: uuid::Uuid) -> Result<Value, CacheError> {
        Ok(self.run.clone())
    }
    async fn credential(&self, _: &str) -> Result<Value, CacheError> {
        Ok(self.credential.clone())
    }
    async fn native(&self, resource: Resource, name: &str) -> Result<Value, CacheError> {
        self.objects
            .iter()
            .find(|(r, n, _)| *r == resource && n == name)
            .map(|(_, _, v)| v.clone())
            .ok_or(CacheError::Unavailable)
    }
}
pub(crate) fn credential() -> Value {
    json!({"id":"managed-login","kind":"managed-codex-login","issuer":"codex-managed","principal":"david@algedonic.dev","scopes":["harness-login"],"rotated_at":GENERATION,"storage_location":"Secret boss-dev/fixture-login/auth.json","consumers":[{"kind":"harness","location":"codex-exec"}],"rotation_policy":"on-demand","notes":"synthetic metadata; no value"})
}
#[tokio::test]
async fn a_native_reader_retains_known_http_refusal_and_full_body_before_judgement() {
    use axum::{http::StatusCode, routing::get};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let router = axum::Router::new()
        .route(
            "/positive",
            get(|| async { axum::Json(json!({"known":"positive"})) }),
        )
        .route(
            "/refused",
            get(|| async {
                (
                    StatusCode::FORBIDDEN,
                    "synthetic complete private native refusal",
                )
            }),
        );
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
            .unwrap();
    });
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    assert_eq!(
        bounded_json(client.get(format!("http://{address}/positive")))
            .await
            .unwrap(),
        json!({"known":"positive"})
    );
    let observation = observed_request(client.get(format!("http://{address}/refused"))).await;
    stop.send(()).unwrap();
    server.await.unwrap();
    assert!(observation.result.is_err());
    assert_eq!(observation.original["status"], 403);
    assert_eq!(observation.original["complete"], true);
    use base64::Engine;
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(observation.original["body_base64"].as_str().unwrap())
            .unwrap(),
        b"synthetic complete private native refusal"
    );
}
#[tokio::test]
async fn named_native_absence_requires_actual404_complete_status_and_exact_resource() {
    use axum::{http::StatusCode, routing::get};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let status = |name: &str, kind: &str| json!({"apiVersion":"v1","kind":"Status","reason":"NotFound","code":404,"details":{"name":name,"kind":kind}});
    let good = status("original-job", "jobs");
    let same = good.clone();
    let foreign = status("other-job", "jobs");
    let wrong = status("original-job", "pods");
    let router = axum::Router::new()
        .route(
            "/positive",
            get(|| async { axum::Json(json!({"known":"positive"})) }),
        )
        .route(
            "/valid",
            get(move || {
                let value = good.clone();
                async move { (StatusCode::NOT_FOUND, axum::Json(value)) }
            }),
        )
        .route(
            "/fake200",
            get(move || {
                let value = same.clone();
                async move { axum::Json(value) }
            }),
        )
        .route(
            "/foreign",
            get(move || {
                let value = foreign.clone();
                async move { (StatusCode::NOT_FOUND, axum::Json(value)) }
            }),
        )
        .route(
            "/wrong",
            get(move || {
                let value = wrong.clone();
                async move { (StatusCode::NOT_FOUND, axum::Json(value)) }
            }),
        )
        .route(
            "/unrouted",
            get(|| async { (StatusCode::NOT_FOUND, "unrouted synthetic404") }),
        )
        .route(
            "/forbidden",
            get(|| async { (StatusCode::FORBIDDEN, "synthetic403") }),
        );
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
            .unwrap();
    });
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    assert_eq!(
        bounded_json(client.get(format!("http://{address}/positive")))
            .await
            .unwrap(),
        json!({"known":"positive"})
    );
    for path in [
        "valid",
        "fake200",
        "foreign",
        "wrong",
        "unrouted",
        "forbidden",
    ] {
        let result = bounded_absence(
            client.get(format!("http://{address}/{path}")),
            "original-job",
            "jobs",
        )
        .await;
        if path == "valid" {
            assert!(result.is_ok());
        } else {
            assert!(result.is_err(), "{path}");
        }
    }
    stop.send(()).unwrap();
    server.await.unwrap();
}
#[tokio::test]
async fn native_partial_body_loss_keeps_known_status_and_received_original() {
    use base64::Engine;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    for status in [200, 403, 503] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 2048];
            let _ = socket.read(&mut request).await.unwrap();
            let response = format!(
                "HTTP/1.1 {status} Synthetic\r\nContent-Length: 100\r\nConnection: close\r\n\r\nsynthetic private partial"
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            socket.shutdown().await.unwrap();
        });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let observed = observed_request(client.get(format!("http://{address}/native"))).await;
        server.await.unwrap();
        assert!(observed.result.is_err());
        assert_eq!(observed.original["status"], status);
        assert_eq!(observed.original["complete"], false);
        assert!(observed.original["error"].is_string());
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(observed.original["body_base64"].as_str().unwrap())
                .unwrap(),
            b"synthetic private partial"
        );
    }
}
#[tokio::test]
async fn an_unparseable_registry_install_generation_never_resolves_scope() {
    let (mut record, run, objects, c, mut q) = fixture();
    record.bindings.storage_generation = "not-a-time".into();
    record.installed.storage_generation = "not-a-time".into();
    q.phase = ipc::Phase::Acquire {
        generation: "not-a-time".into(),
    };
    let mut value = credential();
    value["rotated_at"] = json!("not-a-time");
    let port = Arc::new(Fixture {
        record: Mutex::new(Some(record)),
        run,
        objects,
        credential: value,
    });
    assert!(
        Resolver {
            records: port.clone(),
            metadata: port
        }
        .resolve(&c, &q)
        .await
        .is_err()
    );
}
pub(crate) fn fixture() -> (
    Record,
    Value,
    Vec<(Resource, String, Value)>,
    ipc::Caller,
    ipc::Request,
) {
    let mut p = crate::launch_controller::tests::prepared();
    p.login_file = "auth.json".into();
    let (pvc, job, pods) = crate::launch_controller::tests::objects(&p);
    let b = crate::launch_cluster::Bindings {
        schema: 1,
        generation: "install-one".into(),
        harness: "codex-exec".into(),
        harness_sha256: "a".repeat(64),
        image: p.image.clone(),
        image_platform: "linux/amd64".into(),
        binary: "/usr/local/bin/codex".into(),
        binary_version: "0.159.3".into(),
        argv_help_sha256: "b".repeat(64),
        helper_sha256: "c".repeat(64),
        kubectl_sha256: "d".repeat(64),
        credential: "managed-login".into(),
        issuer: "codex-managed".into(),
        principal: "david@algedonic.dev".into(),
        scopes: vec!["harness-login".into()],
        storage_generation: GENERATION.into(),
        login_file: "auth.json".into(),
        secret: p.login.clone(),
        renewal_owner: "broker".into(),
    };
    let caller = ipc::Caller {
        pod_uid: uuid::Uuid::from_u128(1),
        service_account_uid: uuid::Uuid::from_u128(2),
        expires_at: boss_clock_client::wall_now().timestamp() + 60,
    };
    let controller = json!({"apiVersion":"v1","kind":"Pod","metadata":{"namespace":"boss-dev","name":"controller","uid":caller.pod_uid,"resourceVersion":"1"},"spec":{"serviceAccountName":"dev-session"},"status":{"phase":"Running"}});
    let sa = json!({"apiVersion":"v1","kind":"ServiceAccount","metadata":{"namespace":"boss-dev","name":"dev-session","uid":caller.service_account_uid,"resourceVersion":"1"}});
    let objects = vec![
        (Resource::Pod, "controller".into(), controller),
        (Resource::ControllerAccount, "dev-session".into(), sa),
        (
            Resource::Job,
            job["metadata"]["name"].as_str().unwrap().into(),
            job.clone(),
        ),
        (Resource::Pod, "pod".into(), pods["items"][0].clone()),
        (
            Resource::Workspace,
            pvc["metadata"]["name"].as_str().unwrap().into(),
            pvc.clone(),
        ),
    ];
    let request = ipc::Request {
        schema: 1,
        run: uuid::Uuid::parse_str(&p.run).unwrap(),
        credential: b.credential.clone(),
        nonce: uuid::Uuid::from_u128(3),
        phase: ipc::Phase::Acquire {
            generation: b.storage_generation.clone(),
        },
    };
    let run = json!({"id":p.run,"kind":"agent-run","status":"open","steps":[{"spec_slug":"building","status":"active","assignee_id":p.actor,"metadata":{"worker_start_expectation":{"schema":2,"observer":p.observer,"actor":p.actor,"snapshot_sha256":p.snapshot.sha256,"prompt_sha256":crate::launch_controller::hash(&p.prompt)}}}]});
    let prompt_sha256 = crate::launch_controller::hash(&p.prompt);
    (
        Record {
            prepared: p,
            bindings: b.clone(),
            installed: b,
            controller_pod: "controller".into(),
            controller_pod_uid: caller.pod_uid,
            controller_sa_uid: caller.service_account_uid,
            prompt_sha256,
            original_job: job,
            original_pod: pods["items"][0].clone(),
            original_workspace: pvc,
            store_root: PathBuf::from("/broker/private/managed"),
            owner: 0,
        },
        run,
        objects,
        caller,
        request,
    )
}
async fn resolve(
    r: Record,
    run: Value,
    objects: Vec<(Resource, String, Value)>,
    c: ipc::Caller,
    q: ipc::Request,
) -> Result<ipc::Scope<MetadataOnly>, CacheError> {
    let port = Arc::new(Fixture {
        record: Mutex::new(Some(r)),
        run,
        objects,
        credential: credential(),
    });
    Resolver {
        records: port.clone(),
        metadata: port,
    }
    .resolve(&c, &q)
    .await
}
#[tokio::test]
async fn independently_observed_active_scope_resolves_without_private_bytes() {
    let (r, run, o, c, q) = fixture();
    let s = resolve(r, run, o, c, q).await.unwrap();
    assert!(s.running.is_some());
    assert!(s.retained.is_none());
    assert!(s.terminal.is_none());
}
#[tokio::test]
async fn acquire_precedes_compute_creation_but_bind_requires_originals() {
    for phase in [
        ipc::Phase::Acquire {
            generation: GENERATION.into(),
        },
        ipc::Phase::Bind,
    ] {
        let (mut r, run, mut objects, c, mut q) = fixture();
        r.original_job = Value::Null;
        r.original_pod = Value::Null;
        r.original_workspace = Value::Null;
        objects.truncate(2);
        q.phase = phase;
        let acquiring = matches!(q.phase, ipc::Phase::Acquire { .. });
        let result = resolve(r, run, objects, c, q).await;
        if acquiring {
            assert!(result.unwrap().running.is_none())
        } else {
            assert!(result.is_err())
        }
    }
}
#[tokio::test]
async fn changes_to_registry_installation_controller_or_original_incarnation_refuse() {
    for target in 0..12 {
        let (mut r, mut run, mut o, mut c, mut q) = fixture();
        match target {
            0 => run["status"] = json!("closed"),
            1 => run["steps"][0]["assignee_id"] = json!("other"),
            2 => {
                run["steps"][0]["metadata"]["worker_start_expectation"]["observer"] = json!("other")
            }
            3 => r.installed.storage_generation = "replacement".into(),
            4 => o[0].2["metadata"]["uid"] = json!(uuid::Uuid::from_u128(4)),
            5 => o[1].2["metadata"]["uid"] = json!(uuid::Uuid::from_u128(4)),
            6 => o[2].2["metadata"]["uid"] = json!("replacement"),
            7 => o[3].2["status"]["containerStatuses"][0]["containerID"] = json!("replacement"),
            8 => o[4].2["metadata"]["uid"] = json!("replacement"),
            9 => c.expires_at = 0,
            10 => q.credential = "foreign".into(),
            _ => o[3].2["spec"]["volumes"]
                .as_array_mut()
                .unwrap()
                .push(json!({"name":"extra","hostPath":{"path":"/host"}})),
        }
        assert!(resolve(r, run, o, c, q).await.is_err(), "case {target}");
    }
}
#[tokio::test]
async fn http_metadata_uses_only_exact_named_gets_and_refuses_redirect_or_bad_json()
-> anyhow::Result<()> {
    use axum::{Router, http::Uri, routing::get};
    let seen = Arc::new(Mutex::new(Vec::new()));
    let copy = seen.clone();
    let app = Router::new().fallback(get(move |uri: Uri| {
        let copy = copy.clone();
        async move {
            copy.lock().unwrap().push(uri.path().to_string());
            if uri.path().ends_with("/redirect") {
                (
                    axum::http::StatusCode::FOUND,
                    [("location", "/private")],
                    "{}",
                )
                    .into_response()
            } else if uri.path().ends_with("/bad") {
                "{} {}".into_response()
            } else if uri.path().ends_with("/oversized") {
                format!(
                    "{{\"synthetic-private-marker\":\"{}\"}}",
                    "x".repeat(256 * 1024)
                )
                .into_response()
            } else {
                axum::Json(json!({"native":"metadata"})).into_response()
            }
        }
    }));
    use axum::response::IntoResponse;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let http = HttpMetadata {
        jobs: crate::sor_client::machine_client()?,
        native: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .unwrap(),
        jobs_base: base.parse().unwrap(),
        native_base: base.parse().unwrap(),
        actor: "fixture-broker".into(),
    };
    assert_eq!(
        http.native(Resource::Pod, "exact-pod").await.unwrap(),
        json!({"native":"metadata"})
    );
    http.run(uuid::Uuid::from_u128(1)).await.unwrap();
    http.credential("managed-login").await.unwrap();
    http.native(Resource::CollectorAccount, "boss-managed-cache-collector")
        .await
        .unwrap();
    assert!(
        http.native(Resource::CollectorAccount, "default")
            .await
            .is_err()
    );
    assert!(http.native(Resource::Pod, "../private").await.is_err());
    assert!(http.native(Resource::Pod, "redirect").await.is_err());
    assert!(http.native(Resource::Pod, "bad").await.is_err());
    assert!(matches!(
        http.native(Resource::Pod, "oversized").await,
        Err(CacheError::Unavailable)
    ));
    assert_eq!(
        *seen.lock().unwrap(),
        vec![
            "/api/v1/namespaces/boss-dev/pods/exact-pod",
            "/api/jobs/00000000-0000-0000-0000-000000000001",
            "/api/credentials/managed-login",
            "/api/v1/namespaces/boss-dev/serviceaccounts/boss-managed-cache-collector",
            "/api/v1/namespaces/boss-dev/pods/redirect",
            "/api/v1/namespaces/boss-dev/pods/bad",
            "/api/v1/namespaces/boss-dev/pods/oversized"
        ]
    );
    server.abort();
    Ok(())
}
#[tokio::test]
async fn protected_records_read_installation_independently_and_confine_metadata_names() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let root = boss_testing::scratch_dir("private-scope-records");
    // mode-bits-ok: this is the broker metadata directory; traversal is required, no executable is written.
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let owner = std::fs::metadata(&root).unwrap().uid();
    let (r, _, _, _, q) = fixture();
    let record = root.join(format!("{}.json", q.run));
    let install = root.join("installation.json");
    std::fs::write(&record, serde_json::to_vec(&r).unwrap()).unwrap();
    std::fs::write(
        &install,
        serde_json::to_vec(&Installation {
            schema: 1,
            bindings: r.installed.clone(),
            store_root: root.join("fixed-store"),
            owner,
        })
        .unwrap(),
    )
    .unwrap();
    for path in [&record, &install] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let files = FileRecords {
        root: root.clone(),
        owner,
    };
    let read = files.read(q.run).await.unwrap();
    assert_eq!(read.installed, r.installed);
    assert_eq!(read.store_root, root.join("fixed-store"));
    std::fs::set_permissions(&install, std::fs::Permissions::from_mode(0o666)).unwrap();
    assert!(files.read(q.run).await.is_err());
    std::fs::remove_file(&install).unwrap();
    std::os::unix::fs::symlink(&record, &install).unwrap();
    assert!(files.read(q.run).await.is_err());
    std::fs::remove_file(&install).unwrap();
    std::fs::write(&install, vec![b' '; 256 * 1024 + 1]).unwrap();
    std::fs::set_permissions(&install, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(files.read(q.run).await.is_err());
}
#[test]
fn native_http_configuration_requires_protected_https_endpoint() -> anyhow::Result<()> {
    assert!(
        HttpMetadata::new(
            crate::sor_client::machine_client()?,
            "http://jobs.invalid/".parse().unwrap(),
            "https://cluster.invalid/".parse().unwrap(),
            reqwest::Client::builder(),
            "broker".into()
        )
        .is_ok()
    );
    for target in [
        "http://cluster.invalid/",
        "https://user:password@cluster.invalid/",
        "https://cluster.invalid/?endpoint=other",
        "https://cluster.invalid/arbitrary",
    ] {
        assert!(
            HttpMetadata::new(
                crate::sor_client::machine_client()?,
                "http://jobs.invalid/".parse().unwrap(),
                target.parse().unwrap(),
                reqwest::Client::builder(),
                "broker".into()
            )
            .is_err()
        );
    }
    Ok(())
}
#[tokio::test]
async fn metadata_collection_refuses_before_reading_any_record_or_private_path() {
    struct NoReads;
    #[async_trait]
    impl BrokerRecords for NoReads {
        async fn read(&self, _: uuid::Uuid) -> Result<Record, CacheError> {
            panic!("collection attempted protected record read")
        }
    }
    for phase in [ipc::Phase::Collect, ipc::Phase::Reconcile] {
        let (_, run, objects, c, mut q) = fixture();
        q.phase = phase;
        let port = Arc::new(Fixture {
            record: Mutex::new(None),
            run,
            objects,
            credential: credential(),
        });
        assert!(matches!(
            Resolver {
                records: Arc::new(NoReads),
                metadata: port
            }
            .resolve(&c, &q)
            .await,
            Err(CacheError::Unavailable)
        ));
    }
}
#[tokio::test]
async fn bind_uses_exact_credential_metadata_and_accepts_native_api_envelopes() {
    for mutation in [
        None,
        Some("issuer"),
        Some("principal"),
        Some("scopes"),
        Some("rotated_at"),
        Some("storage_location"),
        Some("consumers"),
    ] {
        let (r, run, objects, c, mut q) = fixture();
        q.phase = ipc::Phase::Bind;
        let mut value = credential();
        if let Some(key) = mutation {
            value[key] = json!("foreign")
        }
        let port = Arc::new(Fixture {
            record: Mutex::new(Some(r)),
            run: json!({"data":run}),
            objects,
            credential: json!({"data":value}),
        });
        let result = Resolver {
            records: port.clone(),
            metadata: port,
        }
        .resolve(&c, &q)
        .await;
        if mutation.is_none() {
            assert!(result.is_ok())
        } else {
            assert!(result.is_err(), "{mutation:?}")
        }
    }
}
#[tokio::test]
async fn a_generation_changed_during_native_resolution_is_not_returned_as_scope() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Changing {
        record: Record,
        reads: AtomicUsize,
    }
    #[async_trait]
    impl BrokerRecords for Changing {
        async fn read(&self, _: uuid::Uuid) -> Result<Record, CacheError> {
            let mut record = self.record.clone();
            if self.reads.fetch_add(1, Ordering::SeqCst) > 0 {
                record.installed.storage_generation = "changed".into()
            }
            Ok(record)
        }
    }
    let (record, run, objects, c, q) = fixture();
    let records = Arc::new(Changing {
        record,
        reads: AtomicUsize::new(0),
    });
    let metadata = Arc::new(Fixture {
        record: Mutex::new(None),
        run,
        objects,
        credential: credential(),
    });
    let result = Resolver {
        records: records.clone(),
        metadata,
    }
    .resolve(&c, &q)
    .await;
    assert!(matches!(result, Err(CacheError::InvalidBinding)));
    assert_eq!(records.reads.load(Ordering::SeqCst), 2);
}
