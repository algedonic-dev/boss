//! Native Kubernetes adapter and private durable controller journal.
//! No effect is performed by parsing a row; launch selection must resolve
//! a published generation, activation and issuer-owned login first.
use crate::launch_controller::{Effect, Journal, Native, Transport};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub(crate) const ROWS: &str = "infra/platform/agent-controllers";

fn replay_patch(record: &Value, receipt: Option<&Value>, ending: &Value) -> Result<Value, String> {
    if ending["backend"] != "cluster"
        || !matches!(ending["status"].as_str(), Some("finished" | "died"))
    {
        return Err("durable terminal is not a cluster outcome".into());
    }
    let mut patch = serde_json::Map::new();
    for (key, value) in [("worker_started", receipt), ("launch", Some(ending))] {
        if let Some(value) = value {
            match record["metadata"].get(key) {
                Some(existing) if existing != value => {
                    return Err(format!("replay refuses different {key}"));
                }
                Some(_) => {}
                None => {
                    patch.insert(key.into(), value.clone());
                }
            }
        }
    }
    Ok(Value::Object(patch))
}

#[cfg(test)]
mod replay_tests {
    use super::*;
    #[tokio::test]
    async fn every_native_effect_uses_the_private_pinned_config_and_conserves_uid_delete_input() {
        let root = boss_testing::scratch_dir("launch-native-config");
        let executable = root.join("native-client");
        boss_testing::write_exec(
            &executable,
            "#!/usr/bin/python3 -I\nimport json,os,sys\nprint(json.dumps({'config':os.environ.get('KUBECONFIG'),'argv':sys.argv[1:],'body':sys.stdin.read()}))\nsys.stderr.buffer.write(b'original-native-error\\x00\\xff')\n",
        );
        let config = root.join("pinned.json");
        std::fs::write(&config, b"fixture config; no credential").unwrap();
        let mut native = KubeTransport::new(executable, root.clone()).unwrap();
        native.kubeconfig = Some(config.clone());
        let options = json!({"apiVersion":"v1","kind":"DeleteOptions","preconditions":{"uid":"original-native-uid"},"propagationPolicy":"Foreground"});
        let result = native
            .perform(
                Effect::DeleteJob {
                    name: "agent-original".into(),
                    options: options.clone(),
                },
                Duration::from_secs(5),
            )
            .await
            .unwrap();
        assert_eq!(result.exit, Some(0));
        assert_eq!(result.stderr, b"original-native-error\x00\xff");
        let response: Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(
            response["config"],
            config.to_str().unwrap(),
            "all effects must use the private actual kubeconfig, not a config override riding in-cluster fallback"
        );
        assert!(
            !response["argv"]
                .as_array()
                .unwrap()
                .iter()
                .any(|a| matches!(
                    a.as_str()
                        .unwrap()
                        .trim_start_matches('-')
                        .split('=')
                        .next(),
                    Some("kubeconfig" | "context")
                ))
        );
        assert_eq!(
            serde_json::from_str::<Value>(response["body"].as_str().unwrap()).unwrap(),
            options
        );
        assert_eq!(
            std::fs::read(root.join("native-00000001.stderr")).unwrap(),
            result.stderr
        );
        assert_eq!(
            std::fs::read(root.join("native-00000001.request-body")).unwrap(),
            serde_json::to_vec(&options).unwrap()
        );
    }
    #[test]
    fn replay_keeps_the_original_terminal_and_refuses_conflicting_receipts() {
        let ending = json!({"backend":"cluster","status":"died","outcome":{"stop_observed":true}});
        let receipt = json!({"schema":2,"run":"original"});
        let record = json!({"metadata":{}});
        assert_eq!(
            replay_patch(&record, Some(&receipt), &ending).unwrap(),
            json!({"worker_started":receipt,"launch":ending})
        );
        let existing = json!({"metadata":{"worker_started":receipt,"launch":ending}});
        assert_eq!(
            replay_patch(&existing, Some(&receipt), &ending).unwrap(),
            json!({})
        );
        for bad in [
            json!({"metadata":{"launch":{"status":"finished"}}}),
            json!({"metadata":{"worker_started":{"schema":1}}}),
        ] {
            assert!(replay_patch(&bad, Some(&receipt), &ending).is_err());
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Bindings {
    pub schema: u64,
    pub generation: String,
    pub harness: String,
    pub harness_sha256: String,
    pub image: String,
    pub image_platform: String,
    pub binary: String,
    pub binary_version: String,
    pub argv_help_sha256: String,
    pub helper_sha256: String,
    pub kubectl_sha256: String,
    pub credential: String,
    pub issuer: String,
    pub principal: String,
    pub scopes: Vec<String>,
    pub storage_generation: String,
    pub login_file: String,
    pub secret: String,
    pub renewal_owner: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Row {
    schema: u64,
    enabled: bool,
    required_binary_version: String,
}

fn protected_bytes(path: &Path) -> Result<Vec<u8>, String> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let metadata = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.uid() != 0 || metadata.permissions().mode() & 0o022 != 0 {
        return Err("published binding is not a root-owned non-writable regular artifact".into());
    }
    std::fs::read(path).map_err(|e| e.to_string())
}

/// Missing published/issuer inputs refuse BEFORE dispatch claims a step.
pub(crate) async fn resolve(
    http: &crate::machine_http::MachineClient,
    base: &str,
    repo: &Path,
    observer: &str,
    harness: &crate::harness::Harness,
) -> anyhow::Result<Bindings> {
    use anyhow::{Context, bail};
    let row: Row = toml::from_str(&std::fs::read_to_string(
        repo.join(ROWS).join(format!("{}.toml", harness.id)),
    )?)?;
    if row.schema != 1 || !row.enabled {
        bail!("cluster backend is source-prepared and activation remains held");
    }
    let generation = std::fs::canonicalize("/work/tools/image-cli/current")?;
    let binding: Bindings = serde_json::from_slice(
        &protected_bytes(&generation.join(format!("agent-controller-{}.json", harness.id)))
            .map_err(anyhow::Error::msg)?,
    )?;
    let actual = crate::launch_controller::hash(&std::fs::read(
        repo.join(crate::harness::DIR)
            .join(format!("{}.toml", harness.id)),
    )?);
    if binding.schema != 1
        || binding.harness != harness.id
        || binding.harness_sha256 != actual
        || generation.file_name().and_then(|s| s.to_str()) != Some(binding.generation.as_str())
        || binding.generation.len() != 40
        || !binding.generation.bytes().all(|b| b.is_ascii_hexdigit())
        || binding.image_platform != "linux/amd64"
        || !crate::launch_job::is_pinned(&binding.image)
        || !crate::launch_job::is_name(&binding.secret)
        || !binding.binary.starts_with("/usr/local/bin/")
        || Path::new(&binding.binary)
            .file_name()
            .and_then(|s| s.to_str())
            != harness.argv.first().map(String::as_str)
        || binding.binary_version != row.required_binary_version
        || [
            &binding.argv_help_sha256,
            &binding.helper_sha256,
            &binding.kubectl_sha256,
        ]
        .iter()
        .any(|s| !is_digest(s))
        || binding.login_file.is_empty()
        || binding.login_file.contains('/')
        || binding.renewal_owner.is_empty()
        || binding.scopes.is_empty()
        || binding.issuer.is_empty()
        || binding.principal.is_empty()
    {
        bail!("published generation/harness/image/login witness is absent, stale or ambiguous");
    }
    let api = |path: String| async move {
        crate::sor_client::api_at_signed(
            http,
            base,
            reqwest::Method::GET,
            &path,
            None,
            crate::identity::Signature::As(observer.into()),
        )
        .await?
        .context("authoritative launch input read returned no body")
    };
    let hold = api("/api/jobs/b3071bf4-5996-4cf8-96db-e4b23b2f67a1".into()).await?;
    let hold = hold.get("data").unwrap_or(&hold);
    if hold["id"] != "b3071bf4-5996-4cf8-96db-e4b23b2f67a1"
        || hold["metadata"].get("held_by_david_20261009").is_some()
    {
        bail!("David's isolation activation hold is still present or cannot be verified");
    }
    // Consume the existing supported activation's MACHINE-RUN artifact,
    // not a new guessed approval protocol or merely a source-file flag.
    let selected = std::fs::canonicalize("/work/tools/dev-control/enabled")?;
    let control = Path::new("/work/tools/dev-control/generations").join(&binding.generation);
    if selected != control {
        bail!("published controller generation is not the actual active selector");
    }
    let contract: Value = serde_json::from_slice(
        &protected_bytes(&control.join("contract-check.json")).map_err(anyhow::Error::msg)?,
    )?;
    use base64::Engine;
    let stdout = base64::engine::general_purpose::STANDARD.decode(
        contract["stdout_base64"]
            .as_str()
            .context("activation contract has no original stdout")?,
    )?;
    let expected = std::fs::read(control.join("infra/dev/control-contract.txt"))?;
    if contract["commit"] != binding.generation
        || contract["completion"] != "observed"
        || contract["exit"] != 0
        || stdout != expected
    {
        bail!(
            "existing activation has no successful complete machine-run contract for this generation"
        );
    }
    let credential = api(format!("/api/credentials/{}", binding.credential)).await?;
    let credential = credential.get("data").unwrap_or(&credential);
    let row: boss_jobs::credentials::CredentialRow = serde_json::from_value(credential.clone())?;
    if !credential_matches(&row, &binding) {
        bail!(
            "issuer-owned login registry/install generation or consumer does not match; no Secret data was read"
        );
    }
    let kubectl = control_kubectl(&binding);
    if crate::launch_controller::hash(&protected_bytes(&kubectl).map_err(anyhow::Error::msg)?)
        != binding.kubectl_sha256
    {
        bail!("published native transport binary differs from its verified generation");
    }
    git_bytes(
        repo,
        &[
            "merge-base",
            "--is-ancestor",
            &binding.generation,
            "refs/remotes/origin/main",
        ],
    )
    .await?;
    let helper = git_bytes(
        repo,
        &[
            "show",
            &format!("{}:infra/agent/agent-io.py", binding.generation),
        ],
    )
    .await?;
    if crate::launch_controller::hash(&helper) != binding.helper_sha256 {
        bail!("published image helper witness differs from the landed source generation");
    }
    Ok(binding)
}

/// One registry/install predicate for launcher and private broker metadata.
/// Parsing the typed row happens before this judgement; no value is read.
pub(crate) fn credential_matches(
    row: &boss_jobs::credentials::CredentialRow,
    binding: &Bindings,
) -> bool {
    let expected_storage = format!("Secret boss-dev/{}/{}", binding.secret, binding.login_file);
    row.id == binding.credential
        && row.issuer == binding.issuer
        && row.principal == binding.principal
        && row.scopes == json!(binding.scopes)
        && row.storage_location == expected_storage
        && row.consumers.as_array().is_some_and(|items| {
            items
                .iter()
                .any(|c| c["kind"] == "harness" && c["location"] == binding.harness)
        })
        && row.rotated_at.map(|t| t.to_rfc3339()).as_deref()
            == Some(binding.storage_generation.as_str())
        && !crate::launch_job::DEV_POD_SECRETS.contains(&binding.secret.as_str())
}

fn is_digest(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn control_kubectl(binding: &Bindings) -> PathBuf {
    Path::new("/work/tools/dev-control/generations")
        .join(&binding.generation)
        .join("bin/kubectl")
}

async fn git_bytes(repo: &Path, args: &[&str]) -> anyhow::Result<Vec<u8>> {
    use anyhow::{Context, bail};
    let output = tokio::process::Command::new("/usr/bin/git")
        .arg("-C")
        .arg(repo)
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .await
        .context("reading the exact source snapshot")?;
    if !output.status.success() {
        bail!(
            "git snapshot {} failed: stdout={} stderr={}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(output.stdout)
}

pub(crate) async fn snapshot(
    repo: &Path,
) -> anyhow::Result<(crate::launch_controller::Snapshot, Vec<u8>, Vec<u8>)> {
    use anyhow::{Context, bail};
    if !git_bytes(repo, &["status", "--porcelain", "--untracked-files=all"])
        .await?
        .is_empty()
    {
        bail!(
            "cluster input requires a clean named source head; dirty/untracked source is not guessed"
        );
    }
    let commit = String::from_utf8(git_bytes(repo, &["rev-parse", "HEAD"]).await?)?
        .trim()
        .to_string();
    let main =
        String::from_utf8(git_bytes(repo, &["rev-parse", "refs/remotes/origin/main"]).await?)?
            .trim()
            .to_string();
    let tree = String::from_utf8(git_bytes(repo, &["rev-parse", "HEAD^{tree}"]).await?)?
        .trim()
        .to_string();
    let members = git_bytes(repo, &["ls-tree", "-r", "-z", "--full-tree", "HEAD"]).await?;
    if members.is_empty() {
        bail!("exact source snapshot has no member manifest");
    }
    let temp = tempfile::tempdir().context("making the private source snapshot")?;
    let path = temp.path().join("source.bundle");
    git_bytes(
        repo,
        &[
            "bundle",
            "create",
            path.to_str().context("snapshot path is not UTF8")?,
            "HEAD",
            "refs/remotes/origin/main",
        ],
    )
    .await?;
    let bundle = tokio::fs::read(&path).await?;
    if bundle.len() > 128 * 1024 * 1024 {
        bail!("complete source bundle exceeds the declared 128MiB transport bound");
    }
    // Re-read after bundling; moving source never becomes an exact snapshot.
    if String::from_utf8(git_bytes(repo, &["rev-parse", "HEAD"]).await?)?.trim() != commit
        || String::from_utf8(git_bytes(repo, &["rev-parse", "refs/remotes/origin/main"]).await?)?
            .trim()
            != main
        || !git_bytes(repo, &["status", "--porcelain", "--untracked-files=all"])
            .await?
            .is_empty()
    {
        bail!("source changed while its immutable bundle was prepared");
    }
    let snapshot = crate::launch_controller::Snapshot {
        commit,
        main,
        tree,
        sha256: crate::launch_controller::hash(&bundle),
        bytes: bundle.len() as u64,
        members_sha256: crate::launch_controller::hash(&members),
        members_bytes: members.len() as u64,
    };
    Ok((snapshot, bundle, members))
}

/// The source path is real but dormant. No process fallback exists after
/// selecting Cluster; missing bindings refuse before the source claim.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn launch_at(
    http: &crate::machine_http::MachineClient,
    base: &str,
    observer: &str,
    run: &str,
    prompt: &str,
    settings: &crate::dispatch::Settings,
    plan: &crate::dispatch_launch::Plan,
    request: &crate::dispatch_launch::Request,
) -> anyhow::Result<Value> {
    use crate::launch_controller::{Journal, Prepared};
    use anyhow::{Context, bail};
    let binding = plan
        .cluster
        .as_ref()
        .context("selected cluster backend has no resolved authoritative bindings")?;
    let snapshot = plan
        .snapshot
        .clone()
        .context("selected cluster backend has no exact source snapshot")?;
    let last = Path::new("/workspace/artifacts/last-message.txt");
    let mut argv = plan
        .harness
        .argv_for(&settings.model, &settings.effort, &plan.sandbox, last);
    argv[0] = binding.binary.clone();
    let prompt = format!(
        "{prompt}\n== THE CREDENTIAL-FREE LAUNCHER ==\nThis run executes in a private cluster Job. You hold no system-of-record or forge credential and must not invoke --started, report, gate, approval or release. The trusted launcher records observations under its own signature. Read the ENTIRE START, then emit one first assistant message beginning BOSS_START_ACK followed by a JSON object with schema=1, run={run:?}, start_read=true, snapshot_sha256={:?}, and prompt_sha256 equal to the SHA256 of the exact file /workspace/prompt.txt. A thread id alone does not acknowledge START. Your source is /workspace/tree, last message is /workspace/artifacts/last-message.txt. Every runtime/activation hold remains.\n",
        snapshot.sha256
    );
    let prepared = Prepared {
        run: run.into(),
        observer: observer.into(),
        actor: plan.harness.actor.clone(),
        image: binding.image.clone(),
        login: binding.secret.clone(),
        bound: request.bound,
        grace: request.grace,
        snapshot,
        bundle: plan.bundle.clone(),
        prompt: prompt.into_bytes(),
        argv,
        home_env: plan.harness.transcript.home_env.clone(),
        login_file: binding.login_file.clone(),
        runtime_event: plan
            .harness
            .started
            .as_ref()
            .context("cluster harness has no native runtime event")?
            .event
            .clone(),
        runtime_key: plan
            .harness
            .started
            .as_ref()
            .and_then(|e| e.key.clone())
            .context("cluster harness has no runtime identity key")?,
        completed_event: plan
            .harness
            .completed
            .as_ref()
            .context("cluster harness has no completed-turn event")?
            .event
            .clone(),
        transcript_pattern: plan
            .harness
            .transcript
            .runtime_glob
            .clone()
            .context("cluster harness cannot uniquely bind its transcript")?,
    };
    let api = |method: reqwest::Method, path: String, body: Option<Value>| async move {
        crate::sor_client::api_at_signed(
            http,
            base,
            method,
            &path,
            body,
            crate::identity::Signature::As(observer.into()),
        )
        .await
    };
    let before = api(reqwest::Method::GET, format!("/api/jobs/{run}"), None)
        .await?
        .context("fresh run is unreadable")?;
    let before = before.get("data").unwrap_or(&before);
    let building = crate::envelope::steps(before)
        .into_iter()
        .find(|s| s["spec_slug"] == "building")
        .context("run has no building step")?;
    if before["kind"] != "agent-run"
        || before["status"] != "open"
        || building["assignee_id"] != prepared.actor
        || building["status"] != "ready"
    {
        bail!("observer requires this newly dispatched run's exact ready actor nomination");
    }
    let step = building["id"].as_str().context("building has no id")?;
    let expectation = json!({"schema":2,"minutes":15,"observer":observer,"actor":prepared.actor,
        "prompt_sha256":crate::launch_controller::hash(&prepared.prompt),"snapshot_sha256":prepared.snapshot.sha256});
    let handed = plan
        .harness
        .argv
        .iter()
        .any(|w| w.contains("{model}"))
        .then_some(settings.model.as_str());
    let model =
        crate::dispatch_launch::launched_model(handed, Some(&plan.harness.id), "argv", observer);
    crate::dispatch_launch::record_launched_model(&api, run, &model)
        .await
        .map_err(|e| anyhow::anyhow!("cluster model first record refused before claim: {e}"))?;
    api(
        reqwest::Method::PATCH,
        format!("/api/jobs/{run}/steps/{step}/metadata"),
        Some(json!({"worker_start_expectation":expectation})),
    )
    .await?;
    api(
        reqwest::Method::POST,
        format!(
            "/api/jobs/{run}/steps/{step}/claim?claimed_for={}",
            prepared.actor
        ),
        None,
    )
    .await?;
    let intent = json!({"schema":1,"backend":"cluster","status":"starting","harness":binding.harness,"generation":binding.generation,
        "actor":prepared.actor,"launched_by":observer,"declared_model":settings.model,"observed_model":null,"snapshot":prepared.snapshot,
        "prompt_sha256":crate::launch_controller::hash(&prepared.prompt),"bound_seconds":prepared.bound.as_secs(),"dir":request.dir});
    api(
        reqwest::Method::PATCH,
        format!("/api/jobs/{run}/metadata"),
        Some(json!({"cluster_launch_intent":intent})),
    )
    .await?;
    let readback = api(reqwest::Method::GET, format!("/api/jobs/{run}"), None)
        .await?
        .context("launch intent readback unavailable")?;
    let readback = readback.get("data").unwrap_or(&readback);
    let building = crate::envelope::steps(readback)
        .into_iter()
        .find(|s| s["spec_slug"] == "building")
        .context("building readback missing")?;
    if readback["metadata"]["cluster_launch_intent"] != intent
        || building["metadata"]["worker_start_expectation"] != expectation
        || building["status"] != "active"
        || building["assignee_id"] != prepared.actor
    {
        bail!("native intent/actor expectation failed exact readback; no Job was created");
    }
    let mut journal = FileJournal::create(&request.dir)
        .await
        .map_err(anyhow::Error::msg)?;
    journal
        .record("published_bindings", &serde_json::to_value(binding)?)
        .await
        .map_err(anyhow::Error::msg)?;
    journal
        .record("prepared_inputs", &serde_json::to_value(&prepared)?)
        .await
        .map_err(anyhow::Error::msg)?;
    journal
        .append("source_members", &plan.members)
        .await
        .map_err(anyhow::Error::msg)?;
    journal
        .append("source_bundle", &prepared.bundle)
        .await
        .map_err(anyhow::Error::msg)?;
    journal
        .append("prompt", &prepared.prompt)
        .await
        .map_err(anyhow::Error::msg)?;
    let captures = journal.root().to_path_buf();
    let mut journal = ObserverJournal {
        files: journal,
        http: http.clone(),
        base: base.into(),
        observer: observer.into(),
        run: run.into(),
        pending: Vec::new(),
    };
    let mut transport =
        KubeTransport::new(control_kubectl(binding), captures).map_err(anyhow::Error::msg)?;
    let mut stops = crate::dispatch_launch::Stops::listen()?;
    let outcome = tokio::select! {
        outcome=crate::launch_controller::run(&prepared,&mut transport,&mut journal)=>outcome,
        signal=stops.next()=>Err(format!("launcher interrupted by {signal}; independent cluster deadline and durable recovery remain")),
    };
    // A cancelled controller future never claims stop. Recovery uses only
    // its trusted durable UID record; the cluster bound owes it nothing.
    let terminal = match outcome {
        Ok(outcome) => serde_json::to_value(outcome)?,
        Err(error) => {
            let original = tokio::fs::read(journal.files.root().join("job_binding.json"))
                .await
                .map_err(|e| e.to_string())
                .and_then(|bytes| {
                    serde_json::from_slice::<Value>(&bytes).map_err(|e| e.to_string())
                });
            let stopped = match original {
                Ok(created) => {
                    crate::launch_controller::stop_known(
                        &prepared,
                        &created,
                        &mut transport,
                        &mut journal,
                    )
                    .await
                }
                Err(why) => Err(format!("no complete durable creation identity: {why}")),
            };
            json!({"ending":error,"stop_observed":stopped.as_ref().is_ok_and(|s|s.stop_observed),"evidence_complete":false,"workspace_retained":true,"cleanup":format!("{stopped:?}")})
        }
    };
    journal
        .files
        .record("controller_terminal", &terminal)
        .await
        .map_err(anyhow::Error::msg)?;
    journal.settle().await;
    let mut ending = json!({"schema":1,"backend":"cluster","status":if terminal["evidence_complete"]==true&&terminal["stop_observed"]==true {"finished"}else{"died"},
        "launched_by":observer,"declared_model":settings.model,"observed_model":null,"outcome":terminal,"dir":request.dir});
    for (kind, key) in [
        ("transcript", "transcript"),
        ("last_message", "last_message_file"),
        ("stdout", "stdout_file"),
        ("stderr", "stderr_file"),
    ] {
        if let Some(path) = journal.files.last.get(kind) {
            ending[key] = json!(path);
        }
    }
    if let Some(path) = journal.files.last.get("transcript") {
        match std::fs::read_to_string(path).map_err(|e|e.to_string()).and_then(|text| {
            let model=crate::transcript_usage::read_models(&text).recorded().ok_or("native transcript has no billed model")?;
            let usage=crate::transcript_usage::sum_usage(&text).map(|u|json!({"input":u.input,"cache_read":u.cache_read,"cache_write":u.cache_write,"output":u.output,"turns":u.turns}));
            Ok((model,usage))
        }) {
            Ok((model,usage))=>{ending["observed_model"]=json!(model);ending["transcript_usage_at_exit"]=json!(usage);}
            Err(why)=>ending["observed_model_unread"]=json!(why),
        }
    }
    journal
        .files
        .record("launch_ending", &ending)
        .await
        .map_err(anyhow::Error::msg)?;
    // Failure is replayable, never a reason to keep compute alive. Complete
    // original artifacts and the terminal remain in this private directory.
    let delivery = tokio::time::timeout(
        Duration::from_secs(5),
        api(
            reqwest::Method::PATCH,
            format!("/api/jobs/{run}/metadata"),
            Some(json!({"launch":ending})),
        ),
    )
    .await;
    journal
        .files
        .record(
            "terminal_delivery",
            &json!({"result":format!("{delivery:?}")}),
        )
        .await
        .map_err(anyhow::Error::msg)?;
    if ending["status"] != "finished" {
        let death = async {
            let run_body = api(reqwest::Method::GET, format!("/api/jobs/{run}"), None)
                .await?
                .context("death readback missing")?;
            let building = crate::envelope::steps(&run_body)
                .into_iter()
                .find(|s| s["spec_slug"] == "building")
                .context("death has no building step")?;
            if building["status"] == "active" && building["metadata"].get("result").is_none() {
                let sid = building["id"]
                    .as_str()
                    .context("death building has no id")?;
                api(
                    reqwest::Method::PATCH,
                    format!("/api/jobs/{run}/steps/{sid}/metadata"),
                    Some(json!({"result":"died","launch_died":{"by":observer,"why":terminal}})),
                )
                .await?;
                api(
                    reqwest::Method::PUT,
                    format!("/api/jobs/{run}/steps/{sid}"),
                    Some(crate::dispatch::completed()),
                )
                .await?;
                let readback = api(reqwest::Method::GET, format!("/api/jobs/{run}"), None)
                    .await?
                    .context("death terminal readback missing")?;
                if !crate::envelope::steps(&readback).iter().any(|s| {
                    s["id"] == sid
                        && s["status"] == "completed"
                        && s["metadata"]["result"] == "died"
                }) {
                    bail!("death terminal did not read back");
                }
            }
            Ok::<_, anyhow::Error>(())
        };
        let result = tokio::time::timeout(Duration::from_secs(5), death).await;
        journal
            .files
            .record("death_delivery", &json!({"result":format!("{result:?}")}))
            .await
            .map_err(anyhow::Error::msg)?;
        bail!(
            "cluster launch refused/interrupted; durable original evidence and stop/retention status are in {}",
            request.dir.display()
        );
    }
    Ok(ending)
}

struct ObserverJournal {
    files: FileJournal,
    http: crate::machine_http::MachineClient,
    base: String,
    observer: String,
    run: String,
    pending: Vec<tokio::task::JoinHandle<Result<(), String>>>,
}
impl Drop for ObserverJournal {
    fn drop(&mut self) {
        for task in &self.pending {
            task.abort();
        }
    }
}
#[async_trait]
impl Journal for ObserverJournal {
    async fn append(&mut self, kind: &str, bytes: &[u8]) -> Result<(), String> {
        self.files.append(kind, bytes).await
    }
    async fn record(&mut self, key: &str, value: &Value) -> Result<(), String> {
        self.files.record(key, value).await?;
        if key == "observer_started" {
            let http = self.http.clone();
            let base = self.base.clone();
            let observer = self.observer.clone();
            let run = self.run.clone();
            let receipt = value.clone();
            let root = self.files.root.clone();
            self.pending.push(tokio::spawn(async move {
                let delivery = async {
                    let call = |method: reqwest::Method, body: Option<Value>| async {
                        crate::sor_client::api_at_signed(
                            &http,
                            &base,
                            method,
                            &format!(
                                "/api/jobs/{run}{}",
                                if body.is_some() { "/metadata" } else { "" }
                            ),
                            body,
                            crate::identity::Signature::As(observer.clone()),
                        )
                        .await
                        .map_err(|e| e.to_string())
                    };
                    let before = call(reqwest::Method::GET, None)
                        .await?
                        .ok_or("observer run read returned no body")?;
                    let before = before.get("data").unwrap_or(&before);
                    let step = crate::envelope::steps(before)
                        .into_iter()
                        .find(|s| s["spec_slug"] == "building")
                        .ok_or("observer building missing")?;
                    if before["status"] != "open"
                        || step["status"] != "active"
                        || !boss_jobs::agent_budget::observer_start_receipt_matches(
                            &step["metadata"]["worker_start_expectation"],
                            &receipt,
                            &run,
                            step["assignee_id"].as_str().unwrap_or(""),
                        )
                    {
                        return Err("observer identity/expectation no longer matches".into());
                    }
                    if let Some(existing) = before["metadata"].get("worker_started") {
                        if existing != &receipt {
                            return Err("observer refuses to replace a different receipt".into());
                        }
                    } else {
                        call(
                            reqwest::Method::PATCH,
                            Some(json!({"worker_started":receipt})),
                        )
                        .await?;
                    }
                    let after = call(reqwest::Method::GET, None)
                        .await?
                        .ok_or("observer receipt readback missing")?;
                    if after.get("data").unwrap_or(&after)["metadata"]["worker_started"] != receipt
                    {
                        return Err("observer receipt did not read back".into());
                    }
                    Ok(())
                };
                let result = tokio::time::timeout(Duration::from_secs(5), delivery)
                    .await
                    .unwrap_or_else(|_| {
                        Err(
                            "observer API deadline; receipt retained for idempotent replay"
                                .to_string(),
                        )
                    });
                durable_file(
                    &root.join("observer_delivery.json"),
                    &serde_json::to_vec(&json!({"result":result})).map_err(|e| e.to_string())?,
                )
                .await?;
                result
            }));
        }
        Ok(())
    }
}
impl ObserverJournal {
    async fn settle(&mut self) {
        for mut task in self.pending.drain(..) {
            if tokio::time::timeout(Duration::from_secs(6), &mut task)
                .await
                .is_err()
            {
                task.abort();
                let _=self.files.append("observer_delivery_interrupted",b"API delivery task could not finish; receipt is durable and remains pending replay").await;
            }
        }
    }
}

pub(crate) async fn recover_recorded(original: String, replay_only: bool) -> anyhow::Result<()> {
    use anyhow::{Context, bail};
    let id =
        uuid::Uuid::parse_str(&original).context("recovery requires the original full run UUID")?;
    if id.to_string() != original {
        bail!("recovery requires a canonical original UUID");
    }
    let caller = std::env::var("BOSS_AGENT_RUN")
        .context("recovery requires a fresh registered own-run marker")?;
    if caller == original {
        bail!("recovery does not reuse its original run marker");
    }
    let observer = crate::identity::sign(&reqwest::Method::PATCH, "/api/jobs")?;
    let http = crate::sor_client::machine_client()?;
    let base = crate::sor_client::resolve_jobs_base(None)?;
    let api = |method: reqwest::Method, path: String, body: Option<Value>| {
        let http = &http;
        let base = &base;
        let observer = &observer;
        async move {
            crate::sor_client::api_at_signed(
                http,
                base,
                method,
                &path,
                body,
                crate::identity::Signature::As(observer.clone()),
            )
            .await
        }
    };
    let own = api(reqwest::Method::GET, format!("/api/jobs/{caller}"), None)
        .await?
        .context("recovery caller run is unreadable")?;
    let own = own.get("data").unwrap_or(&own);
    let registry = crate::dispatch_launch::agents_at(&http, &base, &observer).await?;
    let actor = crate::dispatch::resolve_agent(&registry, &observer)
        .context("recovery principal has no unique actor")?;
    if own["kind"] != "agent-run"
        || own["status"] != "open"
        || !crate::envelope::steps(own).iter().any(|s| {
            s["spec_slug"] == "building" && s["status"] == "active" && s["assignee_id"] == actor
        })
    {
        bail!("recovery requires its fresh caller's actual active assigned run");
    }
    let record = api(reqwest::Method::GET, format!("/api/jobs/{original}"), None)
        .await?
        .context("original cluster run is unreadable")?;
    let record = record.get("data").unwrap_or(&record);
    if record["kind"] != "agent-run"
        || record["metadata"]["cluster_launch_intent"]["backend"] != "cluster"
    {
        bail!("target has no admitted cluster lifecycle");
    }
    let directory = Path::new(
        record["metadata"]["cluster_launch_intent"]["dir"]
            .as_str()
            .context("original has no trusted durable artifact locator")?,
    );
    if !directory.is_absolute() {
        bail!("original artifact locator is not absolute");
    }
    let prepared: crate::launch_controller::Prepared = serde_json::from_slice(
        &protected_bytes(&directory.join("prepared_inputs.json")).map_err(anyhow::Error::msg)?,
    )?;
    let binding: Bindings = serde_json::from_slice(
        &protected_bytes(&directory.join("published_bindings.json")).map_err(anyhow::Error::msg)?,
    )?;
    if replay_only {
        if prepared.run != original || prepared.observer != observer {
            bail!("replay requires the original registered observer; no borrowed signature");
        }
        let ending: Value = serde_json::from_slice(
            &protected_bytes(&directory.join("launch_ending.json")).map_err(anyhow::Error::msg)?,
        )?;
        let receipt_path = directory.join("observer_started.json");
        let receipt: Option<Value> = if receipt_path.exists() {
            Some(serde_json::from_slice(
                &protected_bytes(&receipt_path).map_err(anyhow::Error::msg)?,
            )?)
        } else {
            None
        };
        if let Some(receipt) = &receipt {
            let step = crate::envelope::steps(record)
                .into_iter()
                .find(|s| s["spec_slug"] == "building")
                .context("replay building missing")?;
            if !boss_jobs::agent_budget::observer_start_receipt_matches(
                &step["metadata"]["worker_start_expectation"],
                receipt,
                &original,
                step["assignee_id"].as_str().unwrap_or(""),
            ) {
                bail!("retained observer receipt does not bind original expectation");
            }
        }
        let delivery = async {
            let patch =
                replay_patch(record, receipt.as_ref(), &ending).map_err(anyhow::Error::msg)?;
            if patch.as_object().is_some_and(|p| !p.is_empty()) {
                api(
                    reqwest::Method::PATCH,
                    format!("/api/jobs/{original}/metadata"),
                    Some(patch),
                )
                .await?;
            }
            let after = api(reqwest::Method::GET, format!("/api/jobs/{original}"), None)
                .await?
                .context("replay readback missing")?;
            let after = after.get("data").unwrap_or(&after);
            if !replay_patch(after, receipt.as_ref(), &ending)
                .map_err(anyhow::Error::msg)?
                .as_object()
                .is_some_and(|p| p.is_empty())
            {
                bail!("replay exact readback failed");
            }
            if ending["status"] == "died" {
                let step = crate::envelope::steps(after)
                    .into_iter()
                    .find(|s| s["spec_slug"] == "building")
                    .context("replay building missing")?;
                if step["status"] == "active" {
                    if step["metadata"].get("result").is_some_and(|v| v != "died") {
                        bail!("replay refuses different building result");
                    }
                    let sid = step["id"].as_str().context("replay building id missing")?;
                    api(reqwest::Method::PATCH,format!("/api/jobs/{original}/steps/{sid}/metadata"),Some(json!({"result":"died","launch_died":{"by":observer,"why":ending["outcome"]}}))).await?;
                    api(
                        reqwest::Method::PUT,
                        format!("/api/jobs/{original}/steps/{sid}"),
                        Some(crate::dispatch::completed()),
                    )
                    .await?;
                    let final_record =
                        api(reqwest::Method::GET, format!("/api/jobs/{original}"), None)
                            .await?
                            .context("replay death readback missing")?;
                    if !crate::envelope::steps(&final_record).iter().any(|s| {
                        s["id"] == sid
                            && s["status"] == "completed"
                            && s["metadata"]["result"] == "died"
                    }) {
                        bail!("replay death not observed");
                    }
                } else if step["status"] != "completed" || step["metadata"]["result"] != "died" {
                    bail!("replay refuses different building terminal");
                }
            }
            Ok::<_, anyhow::Error>(())
        };
        let result = tokio::time::timeout(Duration::from_secs(5), delivery).await;
        durable_file(
            &directory.join(format!("replay-{caller}.json")),
            &serde_json::to_vec(&json!({"result":format!("{result:?}")}))?,
        )
        .await
        .map_err(anyhow::Error::msg)?;
        result.context("bounded replay deadline; durable receipts remain")??;
        println!(
            "original observer and terminal replayed with exact readback; no compute or workspace effect"
        );
        return Ok(());
    }
    if prepared.run != original || prepared.image != binding.image {
        bail!("durable inputs do not bind the original run/image");
    }
    if crate::launch_controller::hash(
        &protected_bytes(&control_kubectl(&binding)).map_err(anyhow::Error::msg)?,
    ) != binding.kubectl_sha256
    {
        bail!("original published transport generation is unavailable or changed");
    }
    let created: Value = serde_json::from_slice(
        &protected_bytes(&directory.join("job_binding.json")).map_err(anyhow::Error::msg)?,
    )?;
    let workspace: Value = serde_json::from_slice(
        &protected_bytes(&directory.join("workspace_binding.json")).map_err(anyhow::Error::msg)?,
    )?;
    let root = directory.join(format!("recovery-{caller}"));
    let mut journal = FileJournal::create(&root)
        .await
        .map_err(anyhow::Error::msg)?;
    let mut transport =
        KubeTransport::new(control_kubectl(&binding), root.clone()).map_err(anyhow::Error::msg)?;
    let receipt = crate::launch_controller::recover(
        &prepared,
        &created,
        &workspace,
        &mut transport,
        &mut journal,
    )
    .await
    .map_err(anyhow::Error::msg)?;
    let key = format!("cluster_recovery_{caller}");
    let patch = Value::Object(serde_json::Map::from_iter([(key.clone(), receipt.clone())]));
    api(
        reqwest::Method::PATCH,
        format!("/api/jobs/{original}/metadata"),
        Some(patch),
    )
    .await?;
    let after = api(reqwest::Method::GET, format!("/api/jobs/{original}"), None)
        .await?
        .context("recovery receipt did not read back")?;
    if after.get("data").unwrap_or(&after)["metadata"][&key] != receipt {
        bail!(
            "recovery receipt differs on native readback; originals remain in {}",
            root.display()
        );
    }
    println!(
        "retained original evidence recovered into {}; original terminal and PVC retention unchanged",
        root.display()
    );
    Ok(())
}

pub(crate) struct FileJournal {
    root: PathBuf,
    sequence: u64,
    last: std::collections::BTreeMap<String, PathBuf>,
}

impl FileJournal {
    pub(crate) async fn create(root: &Path) -> Result<Self, String> {
        let mut builder = tokio::fs::DirBuilder::new();
        // mode-bits-ok: a new controller-owned private durable run journal
        builder.mode(0o700);
        builder.create(root).await.map_err(|e| e.to_string())?;
        Ok(Self {
            root: root.to_path_buf(),
            sequence: 0,
            last: std::collections::BTreeMap::new(),
        })
    }
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }
    async fn original(&mut self, name: &str, bytes: &[u8]) -> Result<(), String> {
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or("journal sequence overflow")?;
        let path = self.root.join(format!("{:08}-{name}", self.sequence));
        durable_file(&path, bytes).await?;
        self.last.insert(name.into(), path);
        Ok(())
    }
}

async fn durable_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut options = tokio::fs::OpenOptions::new();
    // mode-bits-ok: exact original evidence private to the controller
    options.write(true).create_new(true).mode(0o600);
    let mut file = options.open(path).await.map_err(|e| e.to_string())?;
    file.write_all(bytes).await.map_err(|e| e.to_string())?;
    file.sync_all().await.map_err(|e| e.to_string())?;
    if let Some(parent) = path.parent() {
        tokio::fs::File::open(parent)
            .await
            .map_err(|e| e.to_string())?
            .sync_all()
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[async_trait]
impl Journal for FileJournal {
    async fn append(&mut self, kind: &str, bytes: &[u8]) -> Result<(), String> {
        if !kind
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        {
            return Err("invalid journal kind".into());
        }
        self.original(kind, bytes).await
    }
    async fn record(&mut self, key: &str, value: &Value) -> Result<(), String> {
        if !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return Err("invalid monotonic journal key".into());
        }
        let path = self.root.join(format!("{key}.json"));
        let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
        if path.exists() {
            if tokio::fs::read(&path).await.map_err(|e| e.to_string())? == bytes {
                return Ok(());
            }
            return Err(format!("journal refuses to replace {key}"));
        }
        durable_file(&path, &bytes).await
    }
}

/// This adapter takes only the already-verified published control binary
/// path. No caller kubecontext, endpoint, namespace or command is accepted.
pub(crate) struct KubeTransport {
    executable: PathBuf,
    captures: PathBuf,
    sequence: u64,
    running: Option<OwnedNativeTask>,
    native_streams: Option<Native>,
    kubeconfig: Option<PathBuf>,
}

struct OwnedNativeTask(tokio::task::JoinHandle<Result<Native, String>>);
impl Drop for OwnedNativeTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl KubeTransport {
    pub(crate) fn new(executable: PathBuf, captures: PathBuf) -> Result<Self, String> {
        if !executable.is_absolute() || !executable.is_file() {
            return Err("published kubectl is absent".into());
        }
        Ok(Self {
            executable,
            captures,
            sequence: 0,
            running: None,
            native_streams: None,
            kubeconfig: None,
        })
    }
    fn arguments(effect: Effect) -> Result<(Vec<String>, Vec<u8>), String> {
        let mut args = vec!["--namespace=boss-dev".into()];
        let mut input = Vec::new();
        match effect {
            Effect::CreateWorkspace(body) | Effect::CreateJob(body) => {
                args.extend(["create".into(), "-f".into(), "-".into(), "-o=json".into()]);
                input = serde_json::to_vec(&body).map_err(|e| e.to_string())?;
            }
            Effect::ReadJob(name) => {
                name_safe(&name)?;
                args.extend(["get".into(),format!("--raw=/apis/batch/v1/namespaces/boss-dev/jobs?fieldSelector=metadata.name%3D{name}&limit=2")]);
            }
            Effect::ReadWorkspace(name) => {
                name_safe(&name)?;
                args.extend(["get".into(), "pvc".into(), name, "-o=json".into()]);
            }
            Effect::ListPods(uid) => {
                name_safe(&uid)?;
                args.extend(["get".into(),format!("--raw=/api/v1/namespaces/boss-dev/pods?labelSelector=batch.kubernetes.io%2Fcontroller-uid%3D{uid}&limit=2")]);
            }
            Effect::Exec {
                pod,
                operation,
                input: bytes,
                fence,
            } => {
                name_safe(&pod)?;
                if ![
                    "identity", "bundle", "prompt", "start", "collect", "recover",
                ]
                .contains(&operation.as_str())
                {
                    return Err("unknown typed agent operation".into());
                }
                args.extend([
                    "exec".into(),
                    "-i".into(),
                    pod,
                    "-c=agent".into(),
                    "--".into(),
                    "/usr/local/bin/boss-agent-io".into(),
                    operation,
                ]);
                if let Some(fence) = fence {
                    args.extend([
                        "--fence".into(),
                        serde_json::to_string(&fence).map_err(|e| e.to_string())?,
                    ]);
                }
                input = bytes;
            }
            Effect::DeleteJob { name, options } => {
                name_safe(&name)?;
                args.extend([
                    "delete".into(),
                    format!("--raw=/apis/batch/v1/namespaces/boss-dev/jobs/{name}"),
                    "-f".into(),
                    "-".into(),
                ]);
                input = serde_json::to_vec(&options).map_err(|e| e.to_string())?;
            }
        }
        Ok((args, input))
    }
}

fn name_safe(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.len() > 253
        || !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err("native target is not a bound Kubernetes name/UID".into());
    }
    Ok(())
}

/// Retain chunks while reading, including a cancelled command's partial
/// output. The index names stream and order; no UTF8 decoding or tail.
async fn capture<R: tokio::io::AsyncRead + Unpin>(
    mut reader: R,
    path: PathBuf,
    mut acknowledgment: Option<tokio::sync::oneshot::Sender<Native>>,
) -> Result<Vec<u8>, String> {
    let mut options = tokio::fs::OpenOptions::new();
    // mode-bits-ok: private original native transport stream
    options.write(true).create_new(true).mode(0o600);
    let mut file = options.open(&path).await.map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 8192];
    let mut line = Vec::new();
    loop {
        let count = reader.read(&mut buffer).await.map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        file.write_all(&buffer[..count])
            .await
            .map_err(|e| e.to_string())?;
        file.sync_data().await.map_err(|e| e.to_string())?;
        bytes.extend_from_slice(&buffer[..count]);
        if acknowledgment.is_some() {
            for byte in &buffer[..count] {
                if *byte == b'\n' {
                    if let Ok(frame) = serde_json::from_slice::<Value>(&line)
                        && frame["kind"] == "start-ack"
                        && let Some(sender) = acknowledgment.take()
                    {
                        let _ = sender.send(Native {
                            stdout: serde_json::to_vec(&frame["value"])
                                .map_err(|e| e.to_string())?,
                            stderr: Vec::new(),
                            exit: Some(0),
                        });
                    }
                    line.clear();
                } else {
                    line.push(*byte);
                }
                if line.len() > 4 * 1024 * 1024 {
                    return Err("native frame exceeds its byte bound".into());
                }
            }
        }
        if bytes.len() > 512 * 1024 * 1024 {
            return Err("native output exceeded its retained 512MiB bound".into());
        }
    }
    file.sync_all().await.map_err(|e| e.to_string())?;
    Ok(bytes)
}

#[async_trait]
impl Transport for KubeTransport {
    async fn perform(&mut self, effect: Effect, remaining: Duration) -> Result<Native, String> {
        if self.kubeconfig.is_none() {
            self.initialize(remaining).await?;
        }
        let starting = matches!(&effect,Effect::Exec {operation,..} if operation=="start");
        let collecting = matches!(&effect,Effect::Exec {operation,..} if operation=="collect");
        if starting && self.running.is_some() {
            return Err("one launch attempt cannot start a second harness".into());
        }
        if collecting {
            let mut task = self
                .running
                .take()
                .ok_or("collection has no bound native execution")?;
            let result = tokio::time::timeout(remaining, &mut task.0)
                .await
                .map_err(|_| "native execution still running at collection deadline")?
                .map_err(|e| e.to_string())??;
            if result.exit != Some(0) {
                return Err(
                    "native helper/transport did not exit successfully; originals retained".into(),
                );
            }
            self.native_streams = Some(result);
        }
        let (args, input) = Self::arguments(effect)?;
        let mut args = args;
        let config = self
            .kubeconfig
            .as_ref()
            .ok_or("existing session was not bound")?;
        if args.iter().any(|a| a == "--fence") {
            args.extend(["--seconds".into(), remaining.as_secs_f64().to_string()]);
        }
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or("native capture sequence overflow")?;
        let prefix = self.captures.join(format!("native-{:08}", self.sequence));
        durable_file(&prefix.with_extension("request.json"),&serde_json::to_vec(&json!({"argv":args,"input_sha256":crate::launch_controller::hash(&input),"input_bytes":input.len()})).map_err(|e|e.to_string())?).await?;
        durable_file(&prefix.with_extension("request-body"), &input).await?;
        let mut command = tokio::process::Command::new(&self.executable);
        command
            .args(&args)
            .env_clear()
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .env("HOME", &self.captures)
            .env("KUBECONFIG", config)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        // The trusted in-cluster client uses its existing ServiceAccount.
        // No worker receives these variables or the mounted token.
        for name in ["KUBERNETES_SERVICE_HOST", "KUBERNETES_SERVICE_PORT"] {
            if let Ok(value) = std::env::var(name) {
                command.env(name, value);
            }
        }
        let mut child = command.spawn().map_err(|e| e.to_string())?;
        let mut stdin = child.stdin.take().ok_or("native stdin absent")?;
        let stdout = child.stdout.take().ok_or("native stdout absent")?;
        let stderr = child.stderr.take().ok_or("native stderr absent")?;
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let acknowledgment = starting.then_some(sender);
        let execution = async move {
            let write = async {
                stdin.write_all(&input).await.map_err(|e| e.to_string())?;
                stdin.shutdown().await.map_err(|e| e.to_string())?;
                // Unix pipe shutdown is not an EOF: close the owned writer
                // before waiting for a native helper that reads its envelope.
                drop(stdin);
                Ok::<_, String>(())
            };
            let out = capture(stdout, prefix.with_extension("stdout"), acknowledgment);
            let err = capture(stderr, prefix.with_extension("stderr"), None);
            let wait = async {
                let status = child.wait().await.map_err(|e| e.to_string())?;
                durable_file(
                    &prefix.with_extension("exit.json"),
                    &serde_json::to_vec(&json!({"exit":status.code()}))
                        .map_err(|e| e.to_string())?,
                )
                .await?;
                Ok::<_, String>(status)
            };
            let (w, o, e, status) = tokio::join!(write, out, err, wait);
            w?;
            let status = status?;
            Ok(Native {
                stdout: o?,
                stderr: e?,
                exit: status.code(),
            })
        };
        // Killing this kubectl client on expiry does NOT claim Pod stop.
        // The controller still UID-deletes and observes the Job separately.
        if starting {
            self.running = Some(OwnedNativeTask(tokio::spawn(async move {
                tokio::time::timeout(remaining, execution)
                    .await
                    .map_err(|_| "native client deadline; partial streams retained".to_string())?
            })));
            return tokio::time::timeout(remaining, receiver)
                .await
                .map_err(|_| "native START acknowledgment deadline")?
                .map_err(|_| "native helper ended without START acknowledgment".into());
        }
        let result = tokio::time::timeout(remaining, execution)
            .await
            .map_err(|_| "native client deadline; partial streams retained".to_string())??;
        if collecting {
            self.verify_streamed_archive(&result)?;
        }
        Ok(result)
    }
}

impl KubeTransport {
    async fn observe_client(
        &mut self,
        args: &[&str],
        remaining: Duration,
    ) -> Result<Native, String> {
        self.sequence += 1;
        let prefix = self
            .captures
            .join(format!("connection-{:08}", self.sequence));
        let mut command = tokio::process::Command::new(&self.executable);
        command
            .args(args)
            .env_clear()
            .env("HOME", "/work/home")
            .env("PATH", "/usr/bin:/bin")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|e| e.to_string())?;
        let stdout = child.stdout.take().ok_or("connection stdout absent")?;
        let stderr = child.stderr.take().ok_or("connection stderr absent")?;
        let reading = async {
            let (out, err, status) = tokio::join!(
                capture(stdout, prefix.with_extension("stdout"), None),
                capture(stderr, prefix.with_extension("stderr"), None),
                child.wait()
            );
            let exit = status.map_err(|e| e.to_string())?.code();
            durable_file(
                &prefix.with_extension("exit.json"),
                &serde_json::to_vec(&json!({"exit":exit,"argv":args}))
                    .map_err(|e| e.to_string())?,
            )
            .await?;
            Ok(Native {
                stdout: out?,
                stderr: err?,
                exit,
            })
        };
        tokio::time::timeout(remaining, reading)
            .await
            .map_err(|_| "existing session observation timed out; originals retained".to_string())?
    }
    async fn initialize(&mut self, remaining: Duration) -> Result<(), String> {
        let deadline = tokio::time::Instant::now()
            .checked_add(remaining)
            .ok_or("connection bound overflow")?;
        let left = || deadline.saturating_duration_since(tokio::time::Instant::now());
        let context = self
            .observe_client(&["config", "current-context"], left())
            .await?;
        if context.exit != Some(0) || context.stdout != b"in-cluster\n" {
            return Err("native controller requires the existing in-cluster context".into());
        }
        let endpoint = self
            .observe_client(
                &[
                    "config",
                    "view",
                    "--minify",
                    "-o",
                    "jsonpath={.clusters[0].cluster.server}",
                ],
                left(),
            )
            .await?;
        if endpoint.exit != Some(0) {
            return Err("existing Kubernetes endpoint could not be read".into());
        }
        let endpoint = std::str::from_utf8(&endpoint.stdout).map_err(|e| e.to_string())?;
        let url = reqwest::Url::parse(endpoint).map_err(|e| e.to_string())?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
        {
            return Err("existing Kubernetes endpoint is not a bounded HTTPS authority".into());
        }
        let identity = self
            .observe_client(&["auth", "whoami", "-o=json"], left())
            .await?;
        if identity.exit != Some(0) {
            return Err("existing native identity query failed; original output retained".into());
        }
        let identity: Value =
            serde_json::from_slice(&identity.stdout).map_err(|e| e.to_string())?;
        if identity["status"]["userInfo"]["username"]
            != "system:serviceaccount:boss-dev:dev-session"
        {
            return Err(
                "native controller identity is not the existing dev-session ServiceAccount".into(),
            );
        }
        // Pin the observed endpoint and existing mounted credential refs.
        // No value is copied; mutable host config cannot retarget a later exec.
        let config = json!({"apiVersion":"v1","kind":"Config","current-context":"in-cluster",
            "clusters":[{"name":"existing","cluster":{"server":endpoint,"certificate-authority":"/var/run/secrets/kubernetes.io/serviceaccount/ca.crt"}}],
            "users":[{"name":"existing","user":{"tokenFile":"/var/run/secrets/kubernetes.io/serviceaccount/token"}}],
            "contexts":[{"name":"in-cluster","context":{"cluster":"existing","user":"existing","namespace":"boss-dev"}}]});
        let path = self.captures.join("native-kubeconfig.json");
        durable_file(
            &path,
            &serde_json::to_vec(&config).map_err(|e| e.to_string())?,
        )
        .await?;
        self.kubeconfig = Some(path);
        Ok(())
    }
    fn verify_streamed_archive(&self, collection: &Native) -> Result<(), String> {
        use base64::Engine;
        let original = self
            .native_streams
            .as_ref()
            .ok_or("no trusted native stream capture")?;
        let archive: Value =
            serde_json::from_slice(&collection.stdout).map_err(|e| e.to_string())?;
        let mut streams =
            std::collections::BTreeMap::from([("stdout", Vec::new()), ("stderr", Vec::new())]);
        let mut expected = 1_u64;
        for line in original
            .stdout
            .split(|b| *b == b'\n')
            .filter(|l| !l.is_empty())
        {
            let frame: Value = serde_json::from_slice(line).map_err(|e| e.to_string())?;
            if frame["kind"] == "native-stream" {
                if frame["sequence"].as_u64() != Some(expected) {
                    return Err("native stream ordering is incomplete".into());
                }
                expected += 1;
                let target = streams
                    .get_mut(
                        frame["stream"]
                            .as_str()
                            .ok_or("native stream has no channel")?,
                    )
                    .ok_or("unexpected native stream channel")?;
                target.extend(
                    base64::engine::general_purpose::STANDARD
                        .decode(
                            frame["data"]
                                .as_str()
                                .ok_or("native stream has no original data")?,
                        )
                        .map_err(|e| e.to_string())?,
                );
            }
        }
        for (kind, bytes) in streams {
            let retained = base64::engine::general_purpose::STANDARD
                .decode(
                    archive["artifacts"][kind]["data"]
                        .as_str()
                        .ok_or("archive lacks original stream")?,
                )
                .map_err(|e| e.to_string())?;
            if retained != bytes {
                return Err(format!(
                    "worker archive {kind} differs from trusted original native stream"
                ));
            }
        }
        Ok(())
    }
}
