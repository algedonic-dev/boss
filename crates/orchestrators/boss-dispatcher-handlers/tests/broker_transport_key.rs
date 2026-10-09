//! Broker-owned transport preparation, not receiver enrollment or rotation.
use async_trait::async_trait;
use boss_dispatcher_handlers::handlers::broker_transport_key::{
    IssuedKey, KeyIssuer, PreparedKey, RsaSshIssuer, prepare,
};
use boss_dispatcher_handlers::handlers::credential_issuer::{SecretData, SecretStore, WriteAt};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Memory {
    held: Mutex<SecretData>,
    moved: bool,
    drop_public: bool,
    refuse_access: bool,
    normalize_read: bool,
    corrupt_readback: Option<(&'static str, &'static str)>,
}

#[async_trait]
impl SecretStore for Memory {
    async fn read_key(&self, _: &str, _: &str, key: &str) -> Result<Option<String>, String> {
        Ok(self.held.lock().unwrap().data.get(key).cloned())
    }
    async fn write_key(&self, _: &str, _: &str, _: &str, _: &str) -> Result<(), String> {
        panic!("unconditional writes must never initialize the broker key")
    }
    async fn read_secret(&self, ns: &str, name: &str) -> Result<Option<SecretData>, String> {
        assert!(
            !self.refuse_access,
            "unauthorized scope reached the Secret store"
        );
        assert_eq!((ns, name), ("boss", "runner-deposit-key"));
        let mut held = self.held.lock().unwrap().clone();
        if self.normalize_read {
            held.data = held
                .data
                .into_iter()
                .map(|(k, v)| (k, v.trim().to_owned()))
                .collect();
        }
        Ok(Some(held))
    }
    async fn write_keys_at(
        &self,
        _: &str,
        _: &str,
        entries: &[(&str, &str)],
        version: &str,
    ) -> Result<WriteAt, String> {
        if self.moved {
            return Ok(WriteAt::Moved);
        }
        let mut held = self.held.lock().unwrap();
        assert_eq!(version, held.version);
        held.data
            .extend(entries.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        if self.drop_public {
            held.data.remove("public_key");
        }
        if let Some((key, value)) = self.corrupt_readback {
            held.data.insert(key.into(), value.into());
        }
        held.version = "next".into();
        Ok(WriteAt::Written)
    }
}

#[tokio::test]
async fn exact_readback_still_refuses_internal_bytes_public_key_and_provenance_changes() {
    for change in [
        ("private_key", "private-test-\r\nonly"),
        ("private_key", "private-test-only-extra"),
        ("public_key", "ssh-ed25519 different"),
        ("minted_for", "6c9183de-7558-45cc-a344-85db97523420"),
    ] {
        let store = Memory {
            normalize_read: true,
            corrupt_readback: Some(change),
            ..Memory::default()
        };
        let result = prepare(
            &store,
            &Issuer(Mutex::new(0)),
            "boss",
            "runner-deposit-key",
            JOB,
        )
        .await;
        assert!(result.is_err(), "readback accepted changed {}", change.0);
    }
}

#[tokio::test]
async fn issuer_pair_round_trips_exactly_through_production_read_normalization() {
    let memory = Memory {
        normalize_read: true,
        ..Memory::default()
    };
    let result = prepare(
        &memory,
        &RsaSshIssuer,
        "boss",
        "runner-deposit-key",
        "4214697b-caf1-4c99-bb11-dbb26fccb513",
    )
    .await;
    assert!(
        matches!(result, Ok(PreparedKey::AwaitingEnrollment { .. })),
        "{result:?}"
    );
    let stored = memory.held.lock().unwrap().clone();
    let observed = memory
        .read_secret("boss", "runner-deposit-key")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.data, observed.data,
        "readback preserves the exact stored pair and provenance"
    );
    use rsa::pkcs1::DecodeRsaPrivateKey;
    rsa::RsaPrivateKey::from_pkcs1_pem(&stored.data["private_key"]).unwrap();
}

struct NativeApi {
    url: String,
    packet: Arc<Mutex<serde_json::Value>>,
    writes: Arc<Mutex<Vec<(String, serde_json::Value)>>>,
    task: tokio::task::JoinHandle<()>,
    fail_issue_once: Arc<Mutex<bool>>,
    registry: Arc<Mutex<serde_json::Value>>,
    completions: Arc<Mutex<Vec<String>>>,
}
impl Drop for NativeApi {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn native_api(packet: serde_json::Value) -> NativeApi {
    use axum::{Json, Router, extract::Request, response::IntoResponse};
    let held = Arc::new(Mutex::new(packet));
    let writes = Arc::new(Mutex::new(Vec::new()));
    let p = held.clone();
    let w = writes.clone();
    let fail_issue_once = Arc::new(Mutex::new(false));
    let failing = fail_issue_once.clone();
    let registry = Arc::new(Mutex::new(
        serde_json::json!({"id": "runner-deposit-key", "kind": "ssh-transport-key", "storage_location": "Secret boss/runner-deposit-key"}),
    ));
    let declared = registry.clone();
    let completions = Arc::new(Mutex::new(Vec::new()));
    let events = completions.clone();
    let app = Router::new().fallback(move |request: Request| {
        let p = p.clone();
        let w = w.clone();
        let failing = failing.clone();
        let declared = declared.clone();
        let events = events.clone();
        async move {
            let path = request.uri().path().to_string();
            if request.method() == axum::http::Method::GET {
                if path.starts_with("/api/credentials/") {
                    return Json(declared.lock().unwrap().clone()).into_response();
                }
                return Json(p.lock().unwrap().clone()).into_response();
            }
            let actor: serde_json::Value = serde_json::from_str(
                request
                    .headers()
                    .get("x-boss-user")
                    .unwrap()
                    .to_str()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(actor["id"], format!("rule:{}", preparation_rule_name()));
            let bytes = axum::body::to_bytes(request.into_body(), 65536)
                .await
                .unwrap();
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            w.lock().unwrap().push((path.clone(), body.clone()));
            if path.ends_with("/steps/issue-id") && *failing.lock().unwrap() {
                *failing.lock().unwrap() = false;
                return (
                    axum::http::StatusCode::SERVICE_UNAVAILABLE,
                    "fixture: step completion unavailable",
                )
                    .into_response();
            }
            for step in p.lock().unwrap()["steps"].as_array_mut().unwrap() {
                if path.ends_with(&format!("/steps/{}/metadata", step["id"].as_str().unwrap())) {
                    step["metadata"]
                        .as_object_mut()
                        .unwrap()
                        .extend(body.as_object().unwrap().clone());
                }
                if path.ends_with(&format!("/steps/{}", step["id"].as_str().unwrap())) {
                    if step["status"] != "completed" && body["status"] == "completed" {
                        events
                            .lock()
                            .unwrap()
                            .push(step["id"].as_str().unwrap().to_string());
                    }
                    step["status"] = body["status"].clone();
                }
            }
            Json(serde_json::json!({"ok": true})).into_response()
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    NativeApi {
        url,
        packet: held,
        writes,
        task,
        fail_issue_once,
        registry,
        completions,
    }
}

fn preparation_rule_name() -> String {
    let raw = std::fs::read_to_string(
        boss_testing::repo_root()
            .join("infra/dispatcher/rules/broker-prepares-the-runner-deposit-key.toml"),
    )
    .unwrap();
    let value: toml::Value = toml::from_str(&raw).unwrap();
    value["rule"][0]["name"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn retry_after_partial_failure_conserves_native_preparation_and_never_calls_rotation() {
    let api = native_api(preparation_packet()).await;
    *api.fail_issue_once.lock().unwrap() = true;
    let store = Arc::new(Memory::default());
    let issuer = Arc::new(Issuer(Mutex::new(0)));
    assert!(
        invoke_preparation(&api, store.clone(), issuer.clone())
            .await
            .is_err()
    );
    invoke_preparation(&api, store, issuer.clone())
        .await
        .unwrap();
    assert_eq!(*issuer.0.lock().unwrap(), 1, "the stored pair is conserved");
    assert_eq!(*api.completions.lock().unwrap(), ["issue-id", "install-id"]);
    let writes = api.writes.lock().unwrap();
    assert!(!writes.iter().any(|(path, _)| path.contains("/rotation/")));
    for id in ["issue-id", "install-id", "enroll-id"] {
        assert_eq!(
            writes
                .iter()
                .filter(|(path, _)| path.ends_with(&format!("/steps/{id}/metadata")))
                .count(),
            1,
            "public evidence must not be rewritten after partial failure"
        );
    }
}

#[tokio::test]
async fn recorded_preparation_never_mints_again_after_storage_loss_or_drift() {
    for recorded in ["issue", "install", "enroll"] {
        for damage in ["lost", "private-changed", "public-changed", "owner-changed"] {
            let api = native_api(preparation_packet()).await;
            let store = Arc::new(Memory::default());
            let issuer = Arc::new(Issuer(Mutex::new(0)));
            invoke_preparation(&api, store.clone(), issuer.clone())
                .await
                .unwrap();
            for step in api.packet.lock().unwrap()["steps"].as_array_mut().unwrap() {
                step["status"] = serde_json::json!(if step["spec_slug"] == recorded
                    || step["spec_slug"] == "scope"
                {
                    "completed"
                } else {
                    "pending"
                });
            }
            let proposal_before = api.packet.lock().unwrap()["steps"][3]["metadata"].clone();
            let writes_before = api.writes.lock().unwrap().len();
            {
                let mut held = store.held.lock().unwrap();
                match damage {
                    "lost" => held.data.clear(),
                    "private-changed" => {
                        held.data
                            .insert("private_key".into(), "corrupted-private-test-only".into());
                    }
                    "public-changed" => {
                        held.data
                            .insert("public_key".into(), "ssh-rsa changed-test-only".into());
                    }
                    "owner-changed" => {
                        held.data
                            .insert("minted_for".into(), "another-packet".into());
                    }
                    _ => unreachable!(),
                }
            }
            *issuer.0.lock().unwrap() = 0;
            let held_before = store.held.lock().unwrap().clone();
            assert!(
                invoke_preparation(&api, store.clone(), issuer.clone())
                    .await
                    .is_err(),
                "accepted {recorded}/{damage}"
            );
            assert_eq!(
                *issuer.0.lock().unwrap(),
                0,
                "minted after {recorded}/{damage}"
            );
            assert_eq!(
                *store.held.lock().unwrap(),
                held_before,
                "modified after recorded pair drift"
            );
            assert_eq!(
                api.writes.lock().unwrap().len(),
                writes_before,
                "wrote native evidence after drift"
            );
            assert_eq!(
                api.packet.lock().unwrap()["steps"][3]["metadata"],
                proposal_before
            );
        }
    }
}

#[tokio::test]
async fn real_pair_replay_refuses_corruption_and_a_different_valid_private_key_without_writes() {
    let first = Memory::default();
    let second = Memory::default();
    prepare(&first, &RsaSshIssuer, "boss", "runner-deposit-key", JOB)
        .await
        .unwrap();
    prepare(&second, &RsaSshIssuer, "boss", "runner-deposit-key", JOB)
        .await
        .unwrap();
    let first_pair = first.held.lock().unwrap().clone();
    let different_private = second.held.lock().unwrap().data["private_key"].clone();
    let mut accepted = Vec::new();
    for damage in ["corrupt", "different-valid", "trailing-extra"] {
        let api = native_api(preparation_packet()).await;
        let store = Arc::new(Memory::default());
        *store.held.lock().unwrap() = first_pair.clone();
        invoke_preparation(&api, store.clone(), Arc::new(RsaSshIssuer))
            .await
            .unwrap();
        for step in api.packet.lock().unwrap()["steps"].as_array_mut().unwrap() {
            if step["spec_slug"] == "install" {
                step["status"] = serde_json::json!("pending");
            }
        }
        let writes_before = api.writes.lock().unwrap().len();
        store.held.lock().unwrap().data.insert(
            "private_key".into(),
            if damage == "corrupt" {
                "corrupted-private-test-only".into()
            } else if damage == "trailing-extra" {
                format!("{}\nextra-test-only", first_pair.data["private_key"])
            } else {
                different_private.clone()
            },
        );
        let held_before = store.held.lock().unwrap().clone();
        if invoke_preparation(&api, store.clone(), Arc::new(RsaSshIssuer))
            .await
            .is_ok()
        {
            accepted.push(damage);
            continue;
        }
        assert_eq!(
            *store.held.lock().unwrap(),
            held_before,
            "replay replaced damaged storage"
        );
        assert_eq!(
            api.writes.lock().unwrap().len(),
            writes_before,
            "replay advanced native evidence"
        );
    }
    assert!(
        accepted.is_empty(),
        "accepted real pair damage: {accepted:?}"
    );
}

async fn invoke_preparation(
    api: &NativeApi,
    store: Arc<Memory>,
    issuer: Arc<dyn KeyIssuer>,
) -> Result<(), String> {
    invoke_preparation_with(api, store, issuer, &[]).await
}

/// The runner rule's five args, plus whatever a second transport's rule
/// row adds (`purpose`, `forced_command`; backlog 88379df3).
async fn invoke_preparation_with(
    api: &NativeApi,
    store: Arc<Memory>,
    issuer: Arc<dyn KeyIssuer>,
    extra: &[(&str, &str)],
) -> Result<(), String> {
    use boss_dispatcher::rules::{
        expr::Value,
        handler::{Handler, InvocationContext},
    };
    use boss_dispatcher_handlers::handlers::broker_transport_key::CredentialPrepareSshDeposit;
    let mut args = vec![
        ("secret_namespace".into(), Value::String("boss".into())),
        (
            "secret_name".into(),
            Value::String("runner-deposit-key".into()),
        ),
        (
            "credential_id".into(),
            Value::String("runner-deposit-key".into()),
        ),
        (
            "protocol_kind".into(),
            Value::String("prepare-a-deposit-key".into()),
        ),
        ("receiver_host".into(), Value::String("boss-gcp".into())),
    ];
    for (name, value) in extra {
        args.push(((*name).into(), Value::String((*value).into())));
    }
    CredentialPrepareSshDeposit::new(&api.url, store, issuer).invoke(&args, &InvocationContext {
        rule_name: preparation_rule_name(),
        triggering_event_id: "event-id".into(), triggering_topic: "step.done.credential-rotation".into(),
        event_payload: serde_json::json!({"job_id": JOB, "step_id": "scope-id", "kind": "credential-rotation", "subject_kind": "custom", "subject_id": "runner-deposit-key", "metadata": {}}),
        event_timestamp: None,
    }).await.map_err(|error| error.to_string())
}

#[tokio::test]
async fn the_handler_refuses_native_machine_scope_before_any_secret_or_issuer_access() {
    let mut packet = preparation_packet();
    packet["steps"][0]["completed_by"] = serde_json::json!("automation:dispatcher");
    let api = native_api(packet).await;
    let issuer = Arc::new(Issuer(Mutex::new(0)));
    assert!(
        invoke_preparation(
            &api,
            Arc::new(Memory {
                refuse_access: true,
                ..Memory::default()
            }),
            issuer.clone()
        )
        .await
        .is_err()
    );
    assert_eq!(*issuer.0.lock().unwrap(), 0);
    assert!(api.writes.lock().unwrap().is_empty());
}

#[tokio::test]
async fn the_handler_refuses_unread_or_other_registry_declarations_before_minting() {
    for bad in [
        serde_json::Value::Null,
        serde_json::json!({"id": "other", "kind": "ssh-transport-key", "storage_location": "Secret boss/runner-deposit-key"}),
        serde_json::json!({"id": "runner-deposit-key", "kind": "kubeconfig", "storage_location": "Secret boss/runner-deposit-key"}),
        serde_json::json!({"id": "runner-deposit-key", "kind": "ssh-transport-key", "storage_location": "Secret boss/break-glass-key"}),
    ] {
        let api = native_api(preparation_packet()).await;
        *api.registry.lock().unwrap() = bad;
        let issuer = Arc::new(Issuer(Mutex::new(0)));
        assert!(
            invoke_preparation(
                &api,
                Arc::new(Memory {
                    refuse_access: true,
                    ..Memory::default()
                }),
                issuer.clone()
            )
            .await
            .is_err()
        );
        assert_eq!(*issuer.0.lock().unwrap(), 0);
        assert!(api.writes.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn the_handler_records_public_preparation_only_and_replay_does_not_remint() {
    let api = native_api(preparation_packet()).await;
    let store = Arc::new(Memory::default());
    let issuer = Arc::new(Issuer(Mutex::new(0)));
    invoke_preparation(&api, store.clone(), issuer.clone())
        .await
        .unwrap();
    invoke_preparation(&api, store, issuer.clone())
        .await
        .unwrap();
    assert_eq!(*issuer.0.lock().unwrap(), 1);
    let writes = api.writes.lock().unwrap();
    let serialized = serde_json::to_string(&*writes).unwrap();
    assert!(!serialized.contains("private-test-only"));
    assert!(serialized.contains("public-test-only"));
    assert!(serialized.contains("boss-gcp"));
    assert!(
        writes
            .iter()
            .any(|(path, body)| path.ends_with("/steps/enroll-id/metadata")
                && body["public_key"] == "ssh-ed25519 public-test-only"
                && body["receiver_host"] == "boss-gcp"
                && body["purpose"] == "ops-runner credential deposit"),
        "the human enrollment step must hold the exact public proposal"
    );
    assert!(
        writes
            .iter()
            .any(|(path, body)| path.ends_with("/steps/issue-id") && body["status"] == "completed")
    );
    assert!(
        writes.iter().any(
            |(path, body)| path.ends_with("/steps/install-id") && body["status"] == "completed"
        )
    );
    assert!(
        !writes
            .iter()
            .any(|(path, _)| path.ends_with("/steps/enroll-id")
                || path.ends_with("/verified")
                || path.ends_with("/revoked"))
    );
    assert_eq!(api.packet.lock().unwrap()["steps"][3]["status"], "pending");
    assert!(!writes.iter().any(|(path, _)| path.contains("/rotation/")));
    assert_eq!(*api.completions.lock().unwrap(), ["issue-id", "install-id"]);
}

/// A SECOND TRANSPORT NAMES ITS OWN PURPOSE AND ITS OWN FORCED COMMAND
/// (backlog 88379df3; David, design-doc bdc60b65 question gcp-push). The
/// handler said `ops-runner credential deposit` whatever its rule row
/// declared, so a key prepared to carry the estate machine token to
/// boss-gcp would have put the runner's purpose under David's passkey.
/// The rule row's `purpose` reaches the enroll step and both receipts,
/// and its `forced_command` reaches the enroll step joined to the public
/// key the broker prepared — the one authorized_keys line he is asked to
/// sign, never a line he assembles. A rule that names neither says what
/// the runner's always said, and carries no line.
#[tokio::test]
async fn a_rule_row_names_the_purpose_and_the_forced_command_the_enrollment_signs() {
    let forced = "exec sudo -n /usr/local/libexec/boss/ops-credential-recv machine-token";
    let api = native_api(preparation_packet()).await;
    invoke_preparation_with(
        &api,
        Arc::new(Memory::default()),
        Arc::new(Issuer(Mutex::new(0))),
        &[
            ("purpose", "estate machine token deposit"),
            ("forced_command", forced),
        ],
    )
    .await
    .unwrap();
    {
        let writes = api.writes.lock().unwrap();
        let enroll = writes
            .iter()
            .find(|(path, _)| path.ends_with("/steps/enroll-id/metadata"))
            .map(|(_, body)| body.clone())
            .expect("the enrollment proposal is written");
        assert_eq!(enroll["purpose"], "estate machine token deposit");
        assert_eq!(
            enroll["authorized_keys_line"],
            format!("command=\"{forced}\",restrict ssh-ed25519 public-test-only"),
            "the step carries the whole line, the forced command and the prepared public key"
        );
        // AND NO BARE KEY BESIDE IT (backlog dea2236f; packet 96a0f7bb,
        // 2026-10-07): the bare key was the easier of the two to copy
        // into authorized_keys, and there it is a login, not a deposit
        // key. The public half stays on the two machine receipts, which
        // is where the procedure sends the signer to compare.
        assert!(
            enroll.get("public_key").is_none(),
            "a step that carries the line to place carries no bare key: {enroll}"
        );
        for step in ["issue-id", "install-id"] {
            let receipt = writes
                .iter()
                .find(|(path, _)| path.ends_with(&format!("/steps/{step}/metadata")))
                .map(|(_, body)| body.clone())
                .expect("the machine receipts are written");
            assert_eq!(receipt["purpose"], "estate machine token deposit");
        }
        assert!(
            !serde_json::to_string(&*writes)
                .unwrap()
                .contains("ops-runner credential deposit"),
            "the runner's purpose must not ride a second transport's packet"
        );
    }

    // The runner's own row, which names neither, is unchanged.
    let api = native_api(preparation_packet()).await;
    invoke_preparation(
        &api,
        Arc::new(Memory::default()),
        Arc::new(Issuer(Mutex::new(0))),
    )
    .await
    .unwrap();
    let enroll = api
        .writes
        .lock()
        .unwrap()
        .iter()
        .find(|(path, _)| path.ends_with("/steps/enroll-id/metadata"))
        .map(|(_, body)| body.clone())
        .unwrap();
    assert_eq!(enroll["purpose"], "ops-runner credential deposit");
    assert!(enroll.get("authorized_keys_line").is_none());

    // A forced command that could close its own quoting, or carry a
    // second line, is refused before anything is minted.
    for hostile in [
        "true\",restrict ssh-ed25519 AAAA attacker\ncommand=\"x",
        "true\" ssh-ed25519 AAAA attacker",
        "exec sudo -n /x\nssh-ed25519 AAAA attacker",
        "a\\\"b",
        "",
    ] {
        let api = native_api(preparation_packet()).await;
        let issuer = Arc::new(Issuer(Mutex::new(0)));
        let refused = invoke_preparation_with(
            &api,
            Arc::new(Memory::default()),
            issuer.clone(),
            &[("forced_command", hostile)],
        )
        .await;
        assert!(refused.is_err(), "accepted forced_command {hostile:?}");
        assert_eq!(*issuer.0.lock().unwrap(), 0, "minted for {hostile:?}");
    }
}

struct Issuer(Mutex<usize>);
#[async_trait]
impl KeyIssuer for Issuer {
    fn validate_pair(&self, private_key: &str, public_key: &str) -> Result<(), String> {
        if private_key == "private-test-only" && public_key == "ssh-ed25519 public-test-only" {
            Ok(())
        } else {
            Err("fixture pair mismatch".into())
        }
    }
    async fn mint(&self) -> Result<IssuedKey, String> {
        *self.0.lock().unwrap() += 1;
        Ok(IssuedKey::new(
            "private-test-only".into(),
            "ssh-ed25519 public-test-only".into(),
        ))
    }
}
const JOB: &str = "1d31a6a5-1c9e-47bb-bff2-b0421e6960b7";

fn preparation_packet() -> serde_json::Value {
    serde_json::json!({
        "id": JOB, "kind": "prepare-a-deposit-key", "status": "open", "partition": "real",
        // As the jobs API serves it (boss_core::job::Subject). This
        // fixture spelled it `{"kind": …}`, the handler's own private
        // mirror, which is how a handler that could read no real packet
        // kept 25 green tests (delta review 76249509, B1).
        "subject": {"subject_kind": "custom", "id": "runner-deposit-key"},
        "steps": [
            {"id": "scope-id", "spec_slug": "scope", "kind": "credential-rotation",
             "status": "completed", "completed_by": "emp-david",
             "metadata": {"human_only": true, "credential": "runner-deposit-key"}},
            {"id": "issue-id", "spec_slug": "issue", "kind": "task", "status": "ready", "metadata": {}},
            {"id": "install-id", "spec_slug": "install", "kind": "task", "status": "pending", "metadata": {}},
            {"id": "enroll-id", "spec_slug": "enroll", "kind": "sign-off", "status": "pending", "assurance_required": "presence", "sign_offs_required": ["platform-admin"], "metadata": {"human_only": true}}
        ]
    })
}

#[test]
fn only_the_native_completed_human_scope_authorizes_preparation() {
    use boss_dispatcher_handlers::handlers::broker_transport_key::authorize_preparation;
    let good = preparation_packet();
    assert!(
        authorize_preparation(
            &good,
            JOB,
            "scope-id",
            "prepare-a-deposit-key",
            "runner-deposit-key"
        )
        .is_ok()
    );
    for (field, value) in [
        ("status", serde_json::json!("ready")),
        ("completed_by", serde_json::json!("automation:dispatcher")),
        ("completed_by", serde_json::Value::Null),
        ("kind", serde_json::json!("task")),
    ] {
        let mut packet = good.clone();
        packet["steps"][0][field] = value;
        assert!(
            authorize_preparation(
                &packet,
                JOB,
                "scope-id",
                "prepare-a-deposit-key",
                "runner-deposit-key"
            )
            .is_err(),
            "accepted invalid scope field {field}"
        );
    }
    let mut packet = good.clone();
    packet["steps"][0]["metadata"]["human_only"] = serde_json::json!(false);
    assert!(
        authorize_preparation(
            &packet,
            JOB,
            "scope-id",
            "prepare-a-deposit-key",
            "runner-deposit-key"
        )
        .is_err()
    );
    let mut packet = good.clone();
    packet["steps"][0]["metadata"]["credential"] = serde_json::json!("another-key");
    assert!(
        authorize_preparation(
            &packet,
            JOB,
            "scope-id",
            "prepare-a-deposit-key",
            "runner-deposit-key"
        )
        .is_err()
    );
}

#[test]
fn malformed_or_other_packet_identity_cannot_authorize_a_key() {
    use boss_dispatcher_handlers::handlers::broker_transport_key::authorize_preparation;
    let good = preparation_packet();
    for field in ["id", "kind", "subject", "steps", "status"] {
        let mut packet = good.clone();
        packet.as_object_mut().unwrap().remove(field);
        assert!(
            authorize_preparation(
                &packet,
                JOB,
                "scope-id",
                "prepare-a-deposit-key",
                "runner-deposit-key"
            )
            .is_err(),
            "accepted missing {field}"
        );
    }
    for (field, value) in [
        ("kind", "rotate-a-credential"),
        ("status", "closed"),
        ("id", "another-job"),
    ] {
        let mut packet = good.clone();
        packet[field] = serde_json::json!(value);
        assert!(
            authorize_preparation(
                &packet,
                JOB,
                "scope-id",
                "prepare-a-deposit-key",
                "runner-deposit-key"
            )
            .is_err()
        );
    }
    let mut duplicate = good.clone();
    duplicate["steps"]
        .as_array_mut()
        .unwrap()
        .push(good["steps"][0].clone());
    assert!(
        authorize_preparation(
            &duplicate,
            JOB,
            "scope-id",
            "prepare-a-deposit-key",
            "runner-deposit-key"
        )
        .is_err()
    );
}

#[test]
fn machine_steps_cannot_hide_a_human_contract_or_weaken_receiver_assurance() {
    use boss_dispatcher_handlers::handlers::broker_transport_key::authorize_preparation;
    let good = preparation_packet();
    for index in [1, 2] {
        let mut packet = good.clone();
        packet["steps"][index]["kind"] = serde_json::json!("sign-off");
        assert!(
            authorize_preparation(
                &packet,
                JOB,
                "scope-id",
                "prepare-a-deposit-key",
                "runner-deposit-key"
            )
            .is_err()
        );
    }
    for (field, value) in [
        ("assurance_required", serde_json::json!("session")),
        ("sign_offs_required", serde_json::json!([])),
    ] {
        let mut packet = good.clone();
        packet["steps"][3][field] = value;
        assert!(
            authorize_preparation(
                &packet,
                JOB,
                "scope-id",
                "prepare-a-deposit-key",
                "runner-deposit-key"
            )
            .is_err(),
            "accepted weakened {field}"
        );
    }
    let mut packet = good.clone();
    packet["steps"][2]["id"] = packet["steps"][1]["id"].clone();
    assert!(
        authorize_preparation(
            &packet,
            JOB,
            "scope-id",
            "prepare-a-deposit-key",
            "runner-deposit-key"
        )
        .is_err()
    );
}

#[test]
fn simulated_or_unread_partition_cannot_prepare_real_transport_storage() {
    use boss_dispatcher_handlers::handlers::broker_transport_key::authorize_preparation;
    for partition in [
        serde_json::json!("simulated"),
        serde_json::json!("shadow"),
        serde_json::Value::Null,
    ] {
        let mut packet = preparation_packet();
        packet["partition"] = partition;
        assert!(
            authorize_preparation(
                &packet,
                JOB,
                "scope-id",
                "prepare-a-deposit-key",
                "runner-deposit-key"
            )
            .is_err()
        );
    }
}

#[test]
fn the_published_preparation_shape_keeps_scope_and_enrollment_human_and_distinct() {
    let root = boss_testing::repo_root();
    let source =
        std::fs::read_to_string(root.join("infra/platform/workflows/prepare-a-deposit-key.toml"))
            .unwrap();
    let data: toml::Value = toml::from_str(&source).unwrap();
    let workflow = &data["workflow"][0];
    assert_eq!(workflow["kind"].as_str(), Some("prepare-a-deposit-key"));
    let steps = workflow["step"].as_array().unwrap();
    let step = |slug: &str| {
        steps
            .iter()
            .find(|step| step["title"].as_str() == Some(slug))
            .unwrap()
    };
    assert_eq!(step("scope")["kind"].as_str(), Some("credential-rotation"));
    assert_eq!(
        step("scope")["metadata_defaults"]["human_only"].as_bool(),
        Some(true)
    );
    assert_eq!(step("enroll")["kind"].as_str(), Some("sign-off"));
    assert_eq!(
        step("enroll")["assurance_required"].as_str(),
        Some("presence")
    );
    assert_eq!(
        step("enroll")["metadata_defaults"]["human_only"].as_bool(),
        Some(true)
    );
    for field in ["public_key", "receiver_host", "purpose"] {
        assert!(
            step("enroll")["fields"]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value["name"].as_str() == Some(field)
                    && value["filled_by"].as_str() == Some("executor")
                    && value["required"].as_bool() == Some(true)),
            "human must review {field} before enrollment"
        );
    }
    assert!(
        step("verify")["ready_when"]
            .as_str()
            .unwrap()
            .contains("steps.enroll.done")
    );
    assert!(
        step("revoke")["ready_when"]
            .as_str()
            .unwrap()
            .contains("steps.verify.done")
    );
}

#[tokio::test]
async fn preparing_a_key_installs_one_atomic_pair_and_does_not_enroll_it() {
    let store = Arc::new(Memory::default());
    let issuer = Issuer(Mutex::new(0));
    let result = prepare(store.as_ref(), &issuer, "boss", "runner-deposit-key", JOB)
        .await
        .unwrap();
    assert_eq!(
        result,
        PreparedKey::AwaitingEnrollment {
            public_key: "ssh-ed25519 public-test-only".into()
        }
    );
    assert_eq!(
        store.held.lock().unwrap().data,
        BTreeMap::from([
            ("private_key".into(), "private-test-only".into()),
            ("public_key".into(), "ssh-ed25519 public-test-only".into()),
            ("minted_for".into(), JOB.into()),
        ])
    );
    assert!(!format!("{result:?}").contains("private-test-only"));
}

#[tokio::test]
async fn replay_reuses_the_pair_without_minting_or_claiming_enrollment() {
    let store = Memory::default();
    let issuer = Issuer(Mutex::new(0));
    let first = prepare(&store, &issuer, "boss", "runner-deposit-key", JOB)
        .await
        .unwrap();
    let second = prepare(&store, &issuer, "boss", "runner-deposit-key", JOB)
        .await
        .unwrap();
    assert_eq!(first, second);
    assert_eq!(*issuer.0.lock().unwrap(), 1);
}

#[tokio::test]
async fn another_packet_cannot_replace_a_prepared_pair() {
    let store = Memory::default();
    let issuer = Issuer(Mutex::new(0));
    prepare(&store, &issuer, "boss", "runner-deposit-key", JOB)
        .await
        .unwrap();
    let before = store.held.lock().unwrap().clone();
    assert!(
        prepare(
            &store,
            &issuer,
            "boss",
            "runner-deposit-key",
            "6c9183de-7558-45cc-a344-85db97523420"
        )
        .await
        .is_err()
    );
    assert_eq!(*store.held.lock().unwrap(), before);
    assert_eq!(*issuer.0.lock().unwrap(), 1);
}

#[tokio::test]
async fn a_racing_secret_write_defers_without_overwriting_the_winner() {
    let store = Memory {
        moved: true,
        ..Memory::default()
    };
    let issuer = Issuer(Mutex::new(0));
    assert_eq!(
        prepare(&store, &issuer, "boss", "runner-deposit-key", JOB)
            .await
            .unwrap(),
        PreparedKey::Moved
    );
    assert!(store.held.lock().unwrap().data.is_empty());
}

#[tokio::test]
async fn a_partial_existing_pair_is_refused_without_disclosing_private_bytes() {
    let store = Memory::default();
    store
        .held
        .lock()
        .unwrap()
        .data
        .insert("private_key".into(), "private-test-only".into());
    let issuer = Issuer(Mutex::new(0));
    let error = prepare(&store, &issuer, "boss", "runner-deposit-key", JOB)
        .await
        .unwrap_err();
    assert!(!error.contains("private-test-only"));
    assert_eq!(*issuer.0.lock().unwrap(), 0);
    assert_eq!(store.held.lock().unwrap().data.len(), 1);
}

#[tokio::test]
async fn unrelated_secret_keys_survive_preparation() {
    let store = Memory::default();
    store
        .held
        .lock()
        .unwrap()
        .data
        .insert("unrelated".into(), "retained".into());
    prepare(
        &store,
        &Issuer(Mutex::new(0)),
        "boss",
        "runner-deposit-key",
        JOB,
    )
    .await
    .unwrap();
    assert_eq!(store.held.lock().unwrap().data["unrelated"], "retained");
}

#[test]
fn debugging_the_issuer_result_never_exposes_the_private_half() {
    let issued = IssuedKey::new(
        "private-test-only".into(),
        "ssh-ed25519 public-test-only".into(),
    );
    assert!(!format!("{issued:?}").contains("private-test-only"));
}

#[tokio::test]
async fn the_real_issuer_generates_no_private_files() {
    let scratch = boss_testing::scratch_dir("broker-transport-key-issuer");
    let key = RsaSshIssuer.mint().await.unwrap();
    assert!(format!("{key:?}").contains("ssh-rsa "));
    assert!(!format!("{key:?}").contains("PRIVATE"));
    assert_eq!(std::fs::read_dir(&scratch).unwrap().count(), 0);
}

#[tokio::test]
async fn a_store_acknowledgment_without_the_full_pair_is_not_preparation_evidence() {
    let store = Memory {
        drop_public: true,
        ..Memory::default()
    };
    let result = prepare(
        &store,
        &Issuer(Mutex::new(0)),
        "boss",
        "runner-deposit-key",
        JOB,
    )
    .await;
    assert!(
        result.is_err(),
        "the broker must read back the entire pair and provenance"
    );
}

#[tokio::test]
async fn canceling_issuance_leaves_no_private_file_on_disk() {
    let scratch = boss_testing::scratch_dir("broker-transport-key-cancel");
    let issuer = RsaSshIssuer;
    // Drop the future while its CPU worker is pending. That worker owns
    // memory only; there is no private file to strand after cancellation.
    let mut issuance = Box::pin(issuer.mint());
    std::future::poll_fn(|cx| {
        assert!(std::future::Future::poll(issuance.as_mut(), cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    drop(issuance);
    assert_eq!(std::fs::read_dir(&scratch).unwrap().count(), 0);
}

#[tokio::test]
async fn openssh_derives_the_exact_broker_public_key_from_the_generated_private_half() {
    use rsa::{pkcs1::DecodeRsaPrivateKey, traits::PublicKeyParts};
    use std::os::unix::fs::PermissionsExt;
    let store = Memory::default();
    let prepared = prepare(&store, &RsaSshIssuer, "boss", "runner-deposit-key", JOB)
        .await
        .unwrap();
    let PreparedKey::AwaitingEnrollment { public_key } = prepared else {
        panic!("pair not prepared")
    };
    let held = store.held.lock().unwrap().clone();
    let private = &held.data["private_key"];
    let parsed = rsa::RsaPrivateKey::from_pkcs1_pem(private).unwrap();
    assert!(parsed.n().bits() >= 3072);
    let scratch = boss_testing::scratch_dir("broker-transport-ssh-compatibility");
    let path = scratch.join("test-key");
    std::fs::write(&path, private).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let output = tokio::process::Command::new("ssh-keygen")
        .arg("-y")
        .arg("-f")
        .arg(&path)
        .output()
        .await
        .unwrap();
    std::fs::remove_file(path).unwrap();
    assert!(
        output.status.success(),
        "ssh-keygen failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim_end(),
        public_key
    );
}
