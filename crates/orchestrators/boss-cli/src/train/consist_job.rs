//! Branch code runs in a private Job, never beside conductor credentials.
use super::*;
use std::io::Write;
use std::process::Stdio;

const CLONE: &str = include_str!("consist_clone.sh");
const WORKER: &str = include_str!("consist_worker.py");
const JOB_TEMPLATE: &str = include_str!("../../../../../infra/consist-worker/job.json");
const GATE_RUNNER: &str = include_str!("../../../../../infra/gate-runner/gate-runner.yaml");
const SERVICE_ACCOUNT_ROOT: &str = "/var/run/secrets/kubernetes.io/serviceaccount";
// Held equal to the deployment by the owning source contract test. Keep
// the controller's runtime image independent of a candidate checkout.
const CONTROLLER_IDENTITY: &str = "system:serviceaccount:boss-dev:boss-conductor";

fn session_config(host: &str, port: &str) -> Result<Value> {
    let port: u16 = port
        .parse()
        .context("cluster HTTPS port is absent or invalid")?;
    if port == 0 || host.is_empty() || host == "localhost" || host.ends_with(".localhost") {
        bail!("cluster endpoint is absent or loopback");
    }
    let authority = match host.parse::<std::net::IpAddr>() {
        Ok(ip) => {
            if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
                bail!("cluster endpoint is not a remote unicast address");
            }
            match ip {
                std::net::IpAddr::V4(_) => ip.to_string(),
                std::net::IpAddr::V6(_) => format!("[{ip}]"),
            }
        }
        Err(_) => {
            if !host.split('.').all(|label| {
                !label.is_empty()
                    && !label.starts_with('-')
                    && !label.ends_with('-')
                    && label
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || c == b'-')
            }) {
                bail!("cluster endpoint hostname is invalid");
            }
            host.to_string()
        }
    };
    Ok(
        json!({"apiVersion":"v1","kind":"Config","current-context":"consist-controller",
        "clusters":[{"name":"existing-cluster","cluster":{"server":format!("https://{authority}:{port}"),"certificate-authority":format!("{SERVICE_ACCOUNT_ROOT}/ca.crt")}}],
        "users":[{"name":"existing-session","user":{"tokenFile":format!("{SERVICE_ACCOUNT_ROOT}/token")}}],
        "contexts":[{"name":"consist-controller","context":{"cluster":"existing-cluster","user":"existing-session","namespace":"boss-dev"}}]}),
    )
}

fn check_session_identity(identity: &Value) -> Result<()> {
    // A copied alias or a mounted token is not a proof of this identity.
    if identity["status"]["userInfo"]["username"].as_str() != Some(CONTROLLER_IDENTITY) {
        bail!("observed cluster identity is not the declared conductor ServiceAccount");
    }
    Ok(())
}

struct ConsistControl {
    config: tempfile::NamedTempFile,
}

impl ConsistControl {
    fn existing_session() -> Result<Self> {
        let host = std::env::var("KUBERNETES_SERVICE_HOST")
            .context("cluster service host is absent; no default target")?;
        let port = std::env::var("KUBERNETES_SERVICE_PORT_HTTPS")
            .context("cluster HTTPS port is absent; no default target")?;
        let config = session_config(&host, &port)?;
        for file in ["token", "ca.crt"] {
            if !Path::new(SERVICE_ACCOUNT_ROOT).join(file).is_file() {
                bail!("existing mounted ServiceAccount {file} is unavailable");
            }
        }
        // Metadata only: kubectl reads its existing mount, including a
        // rotated token. No credential value enters argv, env or receipts.
        let mut file = tempfile::NamedTempFile::new().context("private controller config")?;
        file.write_all(&serde_json::to_vec(&config)?)?;
        file.flush()?;
        file.as_file().sync_all()?;
        let control = Self { config: file };
        let identity = control.json(&["auth", "whoami", "-o", "json"])?;
        check_session_identity(&identity)?;
        let allowed = control
            .command()
            .args(["auth", "can-i", "create", "jobs", "--request-timeout=15s"])
            .output()?;
        log(format!(
            "consist controller namespace authority full output:\n{}{}",
            String::from_utf8_lossy(&allowed.stdout),
            String::from_utf8_lossy(&allowed.stderr)
        ));
        if !allowed.status.success() || allowed.stdout != b"yes\n" {
            bail!("declared conductor identity cannot create namespace Jobs");
        }
        Ok(control)
    }

    fn command(&self) -> std::process::Command {
        let mut command = std::process::Command::new("/usr/local/bin/kubectl");
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", "/nonexistent");
        command
            .arg("--kubeconfig")
            .arg(self.config.path())
            .args(["-n", "boss-dev"]);
        command
    }

    fn json(&self, args: &[&str]) -> Result<Value> {
        let out = self
            .command()
            .args(args)
            .arg("--request-timeout=15s")
            .output()?;
        log(format!(
            "consist controller kubectl {} full output:\n{}{}",
            args.join(" "),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ));
        if !out.status.success() {
            bail!(
                "configured kubectl {} failed; full observation retained",
                args.join(" ")
            );
        }
        serde_json::from_slice(&out.stdout).context("configured Kubernetes returned invalid JSON")
    }
}

fn worker_image() -> Result<&'static str> {
    let mut in_gate = false;
    for line in GATE_RUNNER.lines().map(str::trim) {
        if line == "- name: gate" {
            in_gate = true;
        } else if in_gate && line.starts_with("- name:") {
            break;
        } else if in_gate && let Some(image) = line.strip_prefix("image:") {
            return Ok(image.trim());
        }
    }
    bail!("shipped gate manifest has no execution image");
}

fn unchecked(why: impl Into<String>) -> ConsistVerdict {
    ConsistVerdict::Proceed {
        ran: 0,
        warnings: vec![why.into()],
    }
}

/// One fresh Job has one private checkout and no credentials after init.
fn manifest(
    name: &str,
    url: &str,
    reference: &str,
    head: &str,
    baseline: &str,
    budget: u64,
) -> Result<Value> {
    let image = worker_image()?;
    let bindings = json!({"$NAME":name,"$IMAGE":image,"$CLONE":CLONE,"$WORKER":WORKER,
        "$URL":url,"$REFERENCE":reference,"$HEAD":head,"$BASELINE":baseline,
        "$BUDGET":budget.to_string(),"$DEADLINE":budget.saturating_add(180)});
    let mut job: Value =
        serde_json::from_str(JOB_TEMPLATE).context("parse owning consist Job template")?;
    fill_job_template(&mut job, &bindings)?;
    Ok(job)
}

fn fill_job_template(value: &mut Value, bindings: &Value) -> Result<()> {
    match value {
        Value::String(s) if s.starts_with('$') => {
            *value = bindings
                .get(s.as_str())
                .with_context(|| format!("unknown consist template field {s}"))?
                .clone();
        }
        Value::Array(items) => {
            for item in items {
                fill_job_template(item, bindings)?;
            }
        }
        Value::Object(fields) => {
            for field in fields.values_mut() {
                fill_job_template(field, bindings)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Kubernetes termination evidence, not a branch's JSON, proves execution.
fn worker_terminated(pods: &Value, uid: &str) -> Result<String> {
    let rows = pods["items"].as_array().context("pod list missing items")?;
    if rows.len() != 1 {
        bail!("expected exactly one consist pod, got {}", rows.len());
    }
    let pod = &rows[0];
    let owned = pod["metadata"]["ownerReferences"]
        .as_array()
        .is_some_and(|owners| {
            owners
                .iter()
                .any(|o| o["uid"].as_str() == Some(uid) && o["controller"] == true)
        });
    if !owned {
        bail!("consist pod does not belong to the launched Job uid");
    }
    let statuses = pod["status"]["containerStatuses"]
        .as_array()
        .context("worker status absent")?;
    let status = statuses
        .iter()
        .find(|s| s["name"] == "consist")
        .context("consist status absent")?;
    if status["restartCount"].as_u64() != Some(0)
        || status["state"]["terminated"]["exitCode"].as_i64() != Some(0)
    {
        bail!(
            "consist worker has no successful zero-restart termination: {}",
            status["state"]
        );
    }
    pod["metadata"]["name"]
        .as_str()
        .map(str::to_string)
        .context("pod name absent")
}

fn read_receipt(
    log: &[u8],
    name: &str,
    head: &str,
    baseline: &str,
    files_named: usize,
) -> Result<ConsistVerdict> {
    let receipt: Value =
        serde_json::from_slice(log).context("worker log is not one complete receipt")?;
    if receipt["version"] != 1
        || receipt["identity"] != name
        || receipt["head"] != head
        || receipt["baseline"] != baseline
    {
        bail!("consist receipt binding disagrees with the launched worker");
    }
    let rows = receipt["runs"]
        .as_array()
        .context("consist receipt has no runs")?;
    if receipt["exclusions"]["exit"].as_i64() != Some(0) && !rows.is_empty() {
        bail!("lint runs reported without a successful branch roster");
    }
    let mut runs = Vec::new();
    let mut seen = BTreeSet::new();
    for row in rows {
        let lint = row["name"].as_str().context("lint name absent")?;
        if lint.is_empty() || !seen.insert(lint.to_string()) {
            bail!("missing or duplicate lint identity");
        }
        let stdout = row["stdout"].as_str().context("lint stdout absent")?;
        let stderr = row["stderr"].as_str().context("lint stderr absent")?;
        let output = format!("{stdout}{stderr}");
        let result = match row["exit"].as_i64() {
            Some(0) => LintResult::Passed,
            Some(126 | 127) => LintResult::Unrunnable(output),
            Some(_) => LintResult::Failed(output),
            None => LintResult::Unrunnable(format!(
                "{}: {output}",
                row["unavailable"]
                    .as_str()
                    .context("exit and unavailable reason absent")?
            )),
        };
        runs.push(LintRun {
            name: lint.into(),
            result,
        });
    }
    let mut verdict = consist_verdict(&runs, files_named);
    let warnings = receipt["warnings"]
        .as_array()
        .context("worker warnings absent")?;
    let target = match &mut verdict {
        ConsistVerdict::Proceed { warnings, .. } | ConsistVerdict::Refuse { warnings, .. } => {
            warnings
        }
    };
    for warning in warnings {
        target.push(warning.as_str().context("worker warning not text")?.into());
    }
    Ok(verdict)
}

fn run_job(
    job: &Value,
    head: &str,
    baseline: &str,
    policy: &DeliveryPolicy,
) -> Result<ConsistVerdict> {
    let control = ConsistControl::existing_session()?;
    let name = job["metadata"]["name"]
        .as_str()
        .context("job name absent")?;
    let mut child = control
        .command()
        .args(["create", "-f", "-", "-o", "json", "--request-timeout=15s"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .context("kubectl stdin absent")?
        .write_all(&serde_json::to_vec(job)?)?;
    let out = child.wait_with_output()?;
    log(format!(
        "consist controller Job create full output:\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    ));
    if !out.status.success() {
        bail!(
            "consist Job create failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let created: Value = serde_json::from_slice(&out.stdout)?;
    let uid = created["metadata"]["uid"]
        .as_str()
        .context("Job create acknowledgement has no uid")?;
    if created["metadata"]["name"] != name {
        bail!("Job create acknowledged another worker");
    }
    log(format!(
        "consist worker: Job {name} uid {uid} head {head} baseline {baseline}"
    ));
    let started = std::time::Instant::now();
    loop {
        let current = control.json(&["get", "job", name, "-o", "json"])?;
        if current["metadata"]["uid"] != uid {
            bail!("consist Job identity changed");
        }
        let done = current["status"]["conditions"]
            .as_array()
            .is_some_and(|conditions| {
                conditions.iter().any(|c| {
                    c["status"] == "True" && (c["type"] == "Complete" || c["type"] == "Failed")
                })
            });
        if done {
            let pods = control.json(&[
                "get",
                "pods",
                "-l",
                &format!("job-name={name}"),
                "-o",
                "json",
            ])?;
            // Capture full init and worker diagnostics even when execution failed.
            for container in ["clone", "consist"] {
                let output = control
                    .command()
                    .args([
                        "logs",
                        &format!("job/{name}"),
                        "-c",
                        container,
                        "--request-timeout=15s",
                    ])
                    .output()?;
                log(format!(
                    "consist worker {name}/{container} full log:\n{}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                ));
            }
            let pod = worker_terminated(&pods, uid)?;
            let output = control
                .command()
                .args(["logs", &pod, "-c", "consist", "--request-timeout=15s"])
                .output()?;
            if !output.status.success() {
                bail!("consist worker log unavailable");
            }
            return read_receipt(
                &output.stdout,
                name,
                head,
                baseline,
                policy.consist_files_named,
            );
        }
        if started.elapsed() > policy.consist_budget + Duration::from_secs(180) {
            bail!("consist worker deadline elapsed; no execution verdict");
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

pub(super) async fn isolated_consist_check(
    clone: &str,
    url: &str,
    policy: &DeliveryPolicy,
) -> ConsistVerdict {
    let clone = clone.to_string();
    let url = url.to_string();
    let policy = policy.clone();
    let result = tokio::task::spawn_blocking(move || -> Result<ConsistVerdict> {
        let parsed = reqwest::Url::parse(&url).context("forge repository URL")?;
        if !matches!(parsed.scheme(), "http" | "https") || !parsed.username().is_empty() || parsed.password().is_some() || parsed.query().is_some() || parsed.fragment().is_some() {
            bail!("consist clone requires a credential-free HTTP repository URL");
        }
        let head = sh(&["git","-C",&clone,"rev-parse","HEAD"])?;
        let baseline = sh(&["git","-C",&clone,"rev-parse","origin/main"])?;
        let head_text = String::from_utf8(head.stdout).context("assembled head is not text")?;
        let baseline_text = String::from_utf8(baseline.stdout).context("baseline head is not text")?;
        let head = head_text.trim();
        let baseline = baseline_text.trim();
        let name = format!("consist-{}", uuid::Uuid::new_v4().simple());
        let reference = format!("refs/heads/{name}");
        sh(&["git","-C",&clone,"push","fork",&format!("{head}:{reference}")])?;
        let job = manifest(&name,&url,&reference,head,baseline,policy.consist_budget.as_secs())?;
        let verdict = run_job(&job,head,baseline,&policy);
        // The private immutable ref is ours. Refuse to delete another writer's tip.
        match sh(&["git","-C",&clone,"ls-remote","fork",&reference]) {
            Ok(current) if String::from_utf8_lossy(&current.stdout).split_whitespace().next() == Some(head) => {
                if let Err(error) = sh(&["git","-C",&clone,"push","fork","--delete",&name]) { log(format!("consist worker: private ref cleanup failed: {error:#}")); }
            }
            _ => log(format!("consist worker: private ref {reference} cleanup held; remote head not observed as ours")),
        }
        verdict
    }).await;
    match result {
        Ok(Ok(verdict)) => verdict,
        Ok(Err(error)) => unchecked(format!(
            "isolated consist unavailable: {error:#}; no branch code executed in conductor"
        )),
        Err(error) => unchecked(format!(
            "isolated consist observer failed: {error}; no branch code executed in conductor"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_consist_client_binds_validation_and_authentication_to_one_context() {
        let config = session_config("kubernetes.default.svc", "443").unwrap();
        assert_eq!(config["current-context"], "consist-controller");
        assert_eq!(
            config["clusters"][0]["cluster"]["server"],
            "https://kubernetes.default.svc:443"
        );
        assert_eq!(
            config["clusters"][0]["cluster"]["certificate-authority"],
            "/var/run/secrets/kubernetes.io/serviceaccount/ca.crt"
        );
        assert_eq!(
            config["users"][0]["user"]["tokenFile"],
            "/var/run/secrets/kubernetes.io/serviceaccount/token"
        );
        assert!(config["users"][0]["user"].get("token").is_none());
        assert_eq!(config["contexts"][0]["context"]["namespace"], "boss-dev");
        for (host, port) in [
            ("", "443"),
            ("localhost", "443"),
            ("127.0.0.1", "443"),
            ("::1", "443"),
            ("https://evil.invalid/x", "443"),
            ("kubernetes.default.svc", "0"),
            ("kubernetes.default.svc", "65536"),
            ("kubernetes.default.svc", "443/path"),
        ] {
            assert!(
                session_config(host, port).is_err(),
                "unsafe endpoint {host}:{port}"
            );
        }
    }

    #[test]
    fn the_consist_identity_is_observed_and_never_inferred_from_a_mount() {
        let declaration = fs::read_to_string(
            boss_testing::repo_root().join("infra/cluster/manifests/boss-conductor.yaml"),
        )
        .unwrap();
        let accounts = declaration
            .lines()
            .filter_map(|line| line.trim().strip_prefix("serviceAccountName:"))
            .collect::<Vec<_>>();
        assert_eq!(
            accounts.len(),
            1,
            "one deployed conductor account must be named"
        );
        assert_eq!(
            CONTROLLER_IDENTITY,
            format!("system:serviceaccount:boss-dev:{}", accounts[0].trim()),
            "controller identity drifted from its deployment"
        );
        let good = json!({"status":{"userInfo":{"username":"system:serviceaccount:boss-dev:boss-conductor"}}});
        assert!(check_session_identity(&good).is_ok());
        for bad in [
            json!({}),
            json!({"status":{"userInfo":{"username":"system:anonymous"}}}),
            json!({"status":{"userInfo":{"username":"system:serviceaccount:boss-dev:dev-session"}}}),
        ] {
            assert!(check_session_identity(&bad).is_err());
        }
    }

    #[test]
    fn every_controller_operation_uses_the_private_context_and_a_clean_environment() {
        use std::os::unix::fs::PermissionsExt;
        let file = tempfile::NamedTempFile::new().unwrap();
        assert_eq!(
            file.as_file().metadata().unwrap().permissions().mode() & 0o777,
            0o600
        );
        let control = ConsistControl { config: file };
        let command = control.command();
        assert_eq!(command.get_program(), "/usr/local/bin/kubectl");
        let args = command.get_args().collect::<Vec<_>>();
        assert_eq!(
            args,
            vec![
                std::ffi::OsStr::new("--kubeconfig"),
                control.config.path().as_os_str(),
                std::ffi::OsStr::new("-n"),
                std::ffi::OsStr::new("boss-dev")
            ]
        );
        let environment = command.get_envs().collect::<Vec<_>>();
        assert_eq!(environment.len(), 2);
        assert!(environment.contains(&(
            std::ffi::OsStr::new("HOME"),
            Some(std::ffi::OsStr::new("/nonexistent"))
        )));
        assert!(environment.contains(&(
            std::ffi::OsStr::new("PATH"),
            Some(std::ffi::OsStr::new("/usr/bin:/bin"))
        )));
    }

    fn receipt() -> Value {
        json!({"version":1,"identity":"consist-test","head":"head","baseline":"main","exclusions":{"exit":0,"stdout":"","stderr":""},"runs":[{"name":"real-check","exit":0,"stdout":"","stderr":""}],"warnings":[]})
    }

    #[test]
    fn the_real_fixed_worker_captures_branch_output_as_data_and_checks_both_heads() {
        let tree = boss_testing::scratch_dir("consist-worker-effect");
        fs::create_dir_all(tree.join("infra/lint")).unwrap();
        boss_testing::write_exec(&tree.join("infra/gate.sh"), "#!/usr/bin/env bash\nexit 0\n");
        boss_testing::write_exec(
            &tree.join("infra/lint/real-failure.sh"),
            "#!/usr/bin/env bash\nprintf '{\"version\":1,\"identity\":\"invented-receipt\"}\\ncausal failing line\\n'\nprintf 'stderr explains failure\\n' >&2\nexit 1\n",
        );
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "fixture@example.invalid"],
            vec!["config", "user.name", "Fixture"],
            vec!["add", "."],
            vec!["commit", "-qm", "fixture"],
        ] {
            assert!(
                Command::new("git")
                    .arg("-C")
                    .arg(&tree)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let out = Command::new("git")
            .arg("-C")
            .arg(&tree)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        let text = String::from_utf8(out.stdout).unwrap();
        let head = text.trim();
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&tree)
                .args(["update-ref", "refs/remotes/origin/main", head])
                .status()
                .unwrap()
                .success()
        );
        let invoke = |expected: &str| {
            Command::new("python3")
                .args(["-c", WORKER, expected, head, "10", "consist-test"])
                .arg(&tree)
                .output()
                .unwrap()
        };
        let result = invoke(head);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let verdict = read_receipt(&result.stdout, "consist-test", head, head, 10).unwrap();
        let ConsistVerdict::Refuse { failed, ran, .. } = verdict else {
            panic!("forged lint stdout changed the verdict")
        };
        assert_eq!(ran, 1);
        assert!(failed[0].output.contains("invented-receipt"));
        assert!(failed[0].output.contains("causal failing line"));
        assert!(failed[0].output.contains("stderr explains failure"));
        let bad = invoke("wrong-head");
        assert!(!bad.status.success());
        assert!(
            bad.stdout.is_empty(),
            "a mismatched tree printed an invented verdict"
        );
        boss_testing::write_exec(
            &tree.join("infra/lint/real-failure.sh"),
            "#!/usr/bin/env bash\nprintf 'exact positive worker\\n'\nexit 0\n",
        );
        let good = invoke(head);
        assert!(good.status.success());
        assert_eq!(
            read_receipt(&good.stdout, "consist-test", head, head, 10)
                .unwrap()
                .ran(),
            1
        );
    }

    #[test]
    fn actual_generator_output_is_recognized_from_its_owning_template() {
        let name = "consist-0123456789abcdef0123456789abcdef";
        let job = manifest(
            name,
            "https://example.invalid/repo.git",
            "refs/heads/test",
            "0123456789abcdef0123456789abcdef01234567",
            "fedcba9876543210fedcba9876543210fedcba98",
            120,
        )
        .unwrap();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let scratch = boss_testing::scratch_dir("consist-generated-declaration");
        let live = scratch.join("live.json");
        std::fs::write(&live, serde_json::to_vec(&json!({"items":[job]})).unwrap()).unwrap();
        let result = std::process::Command::new("python3")
            .arg(root.join("infra/consist-worker/declaration.py"))
            .arg("--names")
            .arg(root.join("infra/consist-worker/job.json"))
            .arg(live)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(String::from_utf8(result.stdout).unwrap().trim(), name);
        let mut unknown = json!("$UNKNOWN");
        assert!(fill_job_template(&mut unknown, &json!({})).is_err());
    }

    #[test]
    fn no_execution_mount_or_namespace_can_reach_the_conductor_credentials() {
        let job = manifest(
            "consist-test",
            "https://example.invalid/repo.git",
            "refs/heads/test",
            "head",
            "main",
            60,
        )
        .unwrap();
        let pod = &job["spec"]["template"]["spec"];
        assert_eq!(pod["automountServiceAccountToken"], false);
        for key in ["shareProcessNamespace", "hostPID", "hostIPC", "hostNetwork"] {
            assert_eq!(pod[key], false);
        }
        assert_eq!(
            pod["initContainers"][0]["volumeMounts"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        let worker = &pod["containers"][0];
        assert_eq!(worker["securityContext"]["readOnlyRootFilesystem"], true);
        assert_eq!(
            worker["volumeMounts"],
            json!([{"name":"checkout","mountPath":"/work","readOnly":true},{"name":"worker-tmp","mountPath":"/tmp"}])
        );
        assert!(worker.get("envFrom").is_none());
        assert!(worker.get("env").is_none());
        assert_eq!(
            worker["securityContext"]["capabilities"]["drop"],
            json!(["ALL"])
        );
        assert_eq!(pod["volumes"][0]["emptyDir"]["sizeLimit"], "1Gi");
        assert_eq!(job["spec"]["backoffLimit"], 0);
        assert_eq!(job["spec"]["activeDeadlineSeconds"], 240);
        assert!(!CLONE.contains("config --global"));
        assert!(!CLONE.contains("infra/gate.sh"));
        assert!(WORKER.contains("[\"bash\", \"infra/gate.sh\", \"--exclusions\"]"));
    }

    #[test]
    fn invented_missing_or_misbound_receipts_never_claim_checked_work() {
        let good = receipt();
        let bytes = serde_json::to_vec(&good).unwrap();
        assert_eq!(
            read_receipt(&bytes, "consist-test", "head", "main", 10)
                .unwrap()
                .ran(),
            1
        );
        for field in [
            "version", "identity", "head", "baseline", "runs", "warnings",
        ] {
            let mut bad = good.clone();
            bad[field] = json!("forged");
            assert!(
                read_receipt(
                    &serde_json::to_vec(&bad).unwrap(),
                    "consist-test",
                    "head",
                    "main",
                    10
                )
                .is_err(),
                "{field}"
            );
        }
        let mut duplicate = bytes.clone();
        duplicate.extend_from_slice(&bytes);
        assert!(read_receipt(&duplicate, "consist-test", "head", "main", 10).is_err());
        assert!(read_receipt(b"{}", "consist-test", "head", "main", 10).is_err());
        let mut failed_roster = good.clone();
        failed_roster["exclusions"]["exit"] = json!(1);
        assert!(
            read_receipt(
                &serde_json::to_vec(&failed_roster).unwrap(),
                "consist-test",
                "head",
                "main",
                10
            )
            .is_err()
        );
        let mut repeated = good.clone();
        repeated["runs"]
            .as_array_mut()
            .unwrap()
            .push(good["runs"][0].clone());
        assert!(
            read_receipt(
                &serde_json::to_vec(&repeated).unwrap(),
                "consist-test",
                "head",
                "main",
                10
            )
            .is_err()
        );
    }

    #[test]
    fn a_lint_failure_retains_all_causal_output() {
        let mut value = receipt();
        let text = "causal failure\n".repeat(3000);
        value["runs"][0]["exit"] = json!(1);
        value["runs"][0]["stdout"] = json!(text);
        value["runs"][0]["stderr"] = json!("last diagnostic");
        let verdict = read_receipt(
            &serde_json::to_vec(&value).unwrap(),
            "consist-test",
            "head",
            "main",
            10,
        )
        .unwrap();
        let ConsistVerdict::Refuse { failed, .. } = verdict else {
            panic!("failure silently passed")
        };
        assert_eq!(failed[0].output, format!("{text}last diagnostic"));
    }

    #[test]
    fn only_the_actual_owned_zero_exit_worker_is_evidence() {
        let good = json!({"items":[{"metadata":{"name":"pod","ownerReferences":[{"uid":"actual-uid","controller":true}]},"status":{"containerStatuses":[{"name":"consist","restartCount":0,"state":{"terminated":{"exitCode":0}}}]}}]});
        assert_eq!(worker_terminated(&good, "actual-uid").unwrap(), "pod");
        assert!(worker_terminated(&good, "invented-uid").is_err());
        for code in [1, 137] {
            let mut bad = good.clone();
            bad["items"][0]["status"]["containerStatuses"][0]["state"]["terminated"]["exitCode"] =
                json!(code);
            assert!(worker_terminated(&bad, "actual-uid").is_err());
        }
        let mut restarted = good.clone();
        restarted["items"][0]["status"]["containerStatuses"][0]["restartCount"] = json!(1);
        assert!(worker_terminated(&restarted, "actual-uid").is_err());
        assert!(worker_terminated(&json!({"items":[]}), "actual-uid").is_err());
    }
}
