//! Branch code runs in a private Job, never beside conductor credentials.
use super::*;
use std::io::Write;
use std::process::Stdio;

const CLONE: &str = include_str!("consist_clone.sh");
const PRIVATE_TMP: &str = include_str!("../../../../../infra/consist-worker/private-tmp.sh");
const TMP_SUBPATH: &str = "private";
const WORKER: &str = include_str!("consist_worker.py");
const JOB_TEMPLATE: &str = include_str!("../../../../../infra/consist-worker/job.json");
const GATE_RUNNER: &str = include_str!("../../../../../infra/gate-runner/gate-runner.yaml");
const SERVICE_ACCOUNT_ROOT: &str = "/var/run/secrets/kubernetes.io/serviceaccount";
const KUBECTL: &str = "/usr/local/bin/kubectl";
// Held equal to the deployment by the owning source contract test. Keep
// the controller's runtime image independent of a candidate checkout.
pub(super) const CONTROLLER_IDENTITY: &str = "system:serviceaccount:boss-dev:boss-conductor";

fn session_config(host: &str, port: &str, account_root: &Path) -> Result<Value> {
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
        "clusters":[{"name":"existing-cluster","cluster":{"server":format!("https://{authority}:{port}"),"certificate-authority":account_root.join("ca.crt")}}],
        "users":[{"name":"existing-session","user":{"tokenFile":account_root.join("token")}}],
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

/// Where the controller's kubectl and its mounted session live. Production
/// reads the pod's own mount and service env; a test hands a stand-in, so
/// what a session journals can be measured rather than assumed (daab12f4).
pub(super) struct Native {
    program: std::path::PathBuf,
    account_root: std::path::PathBuf,
    host: String,
    port: String,
}

impl Native {
    fn mounted() -> Result<Self> {
        Ok(Self {
            program: KUBECTL.into(),
            account_root: SERVICE_ACCOUNT_ROOT.into(),
            host: std::env::var("KUBERNETES_SERVICE_HOST")
                .context("cluster service host is absent; no default target")?,
            port: std::env::var("KUBERNETES_SERVICE_PORT_HTTPS")
                .context("cluster HTTPS port is absent; no default target")?,
        })
    }
}

pub(super) struct ConsistControl {
    config: tempfile::NamedTempFile,
    recording: Option<std::path::PathBuf>,
    program: std::path::PathBuf,
}

impl ConsistControl {
    fn existing_session() -> Result<Self> {
        Self::session(&Native::mounted()?, None)
    }

    pub(super) fn recorded_session(path: &Path) -> Result<Self> {
        let recording = super::diagnostic_journal::private_directory(path)?;
        Self::session(&Native::mounted()?, Some(recording))
    }

    fn session(native: &Native, recording: Option<std::path::PathBuf>) -> Result<Self> {
        let config = session_config(&native.host, &native.port, &native.account_root)?;
        for file in ["token", "ca.crt"] {
            if !native.account_root.join(file).is_file() {
                bail!("existing mounted ServiceAccount {file} is unavailable");
            }
        }
        // Metadata only: kubectl reads its existing mount, including a
        // rotated token. No credential value enters argv, env or receipts.
        let mut file = tempfile::NamedTempFile::new().context("private controller config")?;
        file.write_all(&serde_json::to_vec(&config)?)?;
        file.flush()?;
        file.as_file().sync_all()?;
        let control = Self {
            config: file,
            recording,
            program: native.program.clone(),
        };
        let identity = control.json(&["auth", "whoami", "-o", "json"])?;
        check_session_identity(&identity)?;
        let allowed = control.captured(&["auth", "can-i", "create", "jobs"], None)?;
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
        let mut command = std::process::Command::new(&self.program);
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

    pub(super) fn json(&self, args: &[&str]) -> Result<Value> {
        let out = self.captured(args, None)?;
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

    pub(super) fn captured(
        &self,
        args: &[&str],
        input: Option<&[u8]>,
    ) -> Result<std::process::Output> {
        if let Some(path) = &self.recording {
            super::diagnostic_journal::keep(
                path,
                &json!({"phase":"native-invocation",
                "program":"/usr/local/bin/kubectl","namespace":"boss-dev","args":args,"request_timeout_seconds":15,
                "input_b64":input.map(|bytes|base64::Engine::encode(&base64::engine::general_purpose::STANDARD,bytes))}),
            )?;
        }
        let mut command = self.command();
        command.args(args).arg("--request-timeout=15s");
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        if let Some(input) = input {
            child
                .stdin
                .take()
                .context("controller stdin absent")?
                .write_all(input)?;
        }
        let output = child.wait_with_output()?;
        if let Some(path) = &self.recording {
            super::diagnostic_journal::command(path, args, input, &output)?;
        }
        Ok(output)
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
///
/// `edit_level` is the edit-level door's answer as the conductor read
/// it, handed to the worker as its last argument (the door's own object,
/// compact, or empty for none): the worker runs branch code and holds no
/// machine token (design c395e62c), so the lint that judges the level
/// is handed it rather than reading the door from this pod (backlog
/// 37742794; the gate Job's twin is backlog 934ccad1).
pub(super) fn validate_clone_url(url: &str) -> Result<()> {
    let parsed = reqwest::Url::parse(url).context("forge repository URL")?;
    if !matches!(parsed.scheme(), "http" | "https")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        bail!("consist clone requires a credential-free HTTP repository URL");
    }
    Ok(())
}

fn manifest(
    name: &str,
    url: &str,
    reference: &str,
    head: &str,
    baseline: &str,
    budget: u64,
    edit_level: Option<&Value>,
) -> Result<Value> {
    validate_clone_url(url)?;
    let image = worker_image()?;
    let edit_level = crate::gate::handable_edit_level(edit_level).unwrap_or_default();
    let bootstrap = format!("{PRIVATE_TMP}\n{CLONE}");
    let bindings = json!({"$NAME":name,"$IMAGE":image,"$CLONE":bootstrap,"$WORKER":WORKER,
        "$URL":url,"$REFERENCE":reference,"$HEAD":head,"$BASELINE":baseline,
        "$BUDGET":budget.to_string(),"$DEADLINE":budget.saturating_add(180),
        "$EDIT_LEVEL":edit_level,"$TMP_SUBPATH":TMP_SUBPATH});
    let mut job: Value =
        serde_json::from_str(JOB_TEMPLATE).context("parse owning consist Job template")?;
    fill_job_template(&mut job, &bindings)?;
    Ok(job)
}

pub(super) fn diagnostic_manifest(
    name: &str,
    url: &str,
    head: &str,
    baseline: &str,
    budget: u64,
    edit_level: &Value,
) -> Result<Value> {
    let reference = format!("refs/heads/{name}");
    let mut job = manifest(
        name,
        url,
        &reference,
        head,
        baseline,
        budget,
        Some(edit_level),
    )?;
    // A terminal worker must remain until its full originals are durably
    // retained. Existing boarding TTL and execution fences are unchanged.
    job["spec"]
        .as_object_mut()
        .context("diagnostic Job spec absent")?
        .remove("ttlSecondsAfterFinished");
    job["metadata"]["labels"]["boss.pipeline/diagnostic"] = json!("conductor");
    job["metadata"]["annotations"]["boss.pipeline/assembled"] = json!(head);
    job["metadata"]["annotations"]["boss.pipeline/baseline"] = json!(baseline);
    Ok(job)
}

pub(super) fn diagnostic_identity(path: &Path, check_capacity: bool) -> Result<String> {
    diagnostic_identity_on(path, check_capacity, Native::mounted)
}

fn diagnostic_identity_on(
    path: &Path,
    check_capacity: bool,
    native: impl FnOnce() -> Result<Native>,
) -> Result<String> {
    // The request's journal must still be a private owned directory, but
    // this per-pass gate is not journaled into it: every pass recorded its
    // kubectl rows INTO the request it served, so a request open long
    // enough spent its whole complete-read budget on elapsed time and went
    // UNKNOWN (4096 rows on assessment 28e96fbc, 2026-10-17, backlog
    // daab12f4). Each observation is logged in full by json(); the sessions
    // that carry evidence (create, observe, the native census) still journal
    // their own identity beside it.
    super::diagnostic_journal::private_directory(path)?;
    let control = ConsistControl::session(&native()?, None)?;
    let identity = control.json(&["auth", "whoami", "-o", "json"])?;
    check_session_identity(&identity)?;
    if check_capacity {
        let retained = control.json(&[
            "get",
            "jobs",
            "-l",
            "boss.pipeline/diagnostic=conductor",
            "-o",
            "json",
        ])?;
        if !diagnostic_capacity_free(&retained)? {
            bail!(
                "diagnostic capacity occupied or unknown; request waits without launch intent or new compute"
            );
        }
    }
    Ok(identity["status"]["userInfo"]["username"]
        .as_str()
        .context("observed diagnostic executor identity absent")?
        .into())
}

pub(super) fn diagnostic_create(
    path: &Path,
    job: &Value,
    assembled: &str,
) -> Result<super::diagnostics::Launch> {
    let control = ConsistControl::recorded_session(path)?;
    let retained = control.json(&[
        "get",
        "jobs",
        "-l",
        "boss.pipeline/diagnostic=conductor",
        "-o",
        "json",
    ])?;
    if !diagnostic_capacity_free(&retained)? {
        bail!(
            "diagnostic compute capacity is occupied or unknown; retain and report, never grant or delete by name"
        );
    }
    let input = serde_json::to_vec(job)?;
    let output = control.captured(&["create", "-f", "-", "-o", "json"], Some(&input))?;
    if !output.status.success() {
        bail!("diagnostic Job creation refused or unknown; full original output retained");
    }
    let name = job["metadata"]["name"]
        .as_str()
        .context("diagnostic manifest name absent")?;
    let baseline = job["metadata"]["annotations"]["boss.pipeline/baseline"]
        .as_str()
        .context("diagnostic manifest baseline absent")?;
    let launch = super::diagnostic_journal::recovered_launch(path, name, baseline)?
        .context("successful native create has no retained complete acknowledgement")?;
    if launch.assembled != assembled || launch.manifest != *job {
        bail!("native acknowledgement does not bind this exact assembled manifest");
    }
    Ok(launch)
}

fn diagnostic_capacity_free(retained: &Value) -> Result<bool> {
    let jobs = retained["items"]
        .as_array()
        .context("diagnostic capacity census has no items")?;
    Ok(jobs.iter().all(|job| {
        job["status"]["conditions"]
            .as_array()
            .is_some_and(|conditions| {
                conditions.iter().any(|condition| {
                    condition["status"] == "True"
                        && (condition["type"] == "Complete" || condition["type"] == "Failed")
                })
            })
    }))
}

pub(super) fn diagnostic_observe(
    path: &Path,
    name: &str,
    baseline: &str,
    launch: &super::diagnostics::Launch,
) -> Result<Option<super::diagnostics::Observation>> {
    let control = ConsistControl::recorded_session(path)?;
    let job = control.json(&["get", "job", name, "-o", "json"])?;
    if job["metadata"]["uid"] != launch.job_uid {
        bail!("diagnostic native Job incarnation changed; original response retained");
    }
    let terminal = job["status"]["conditions"]
        .as_array()
        .is_some_and(|conditions| {
            conditions.iter().any(|condition| {
                condition["status"] == "True"
                    && (condition["type"] == "Complete" || condition["type"] == "Failed")
            })
        });
    if !terminal {
        return Ok(None);
    }
    let pods = control.json(&[
        "get",
        "pods",
        "-l",
        &format!("job-name={name}"),
        "-o",
        "json",
    ])?;
    let rows = pods["items"]
        .as_array()
        .context("diagnostic pod census missing")?;
    if rows.len() != 1 {
        bail!("diagnostic requires exactly one native pod");
    }
    let pod = &rows[0];
    if !pod["metadata"]["ownerReferences"]
        .as_array()
        .is_some_and(|owners| {
            owners
                .iter()
                .any(|owner| owner["uid"] == launch.job_uid && owner["controller"] == true)
        })
    {
        bail!("diagnostic pod does not belong to the acknowledged Job UID");
    }
    let pod_name = pod["metadata"]["name"]
        .as_str()
        .context("diagnostic pod name absent")?;
    let mut receipt = String::new();
    let mut observer_refusal = None;
    for container in ["clone", "consist"] {
        let output = control.captured(&["logs", pod_name, "-c", container], None)?;
        if container == "consist" && output.status.success() {
            match String::from_utf8(output.stdout) {
                Ok(original) => {
                    receipt = original;
                    let named = serde_json::from_str::<Value>(&receipt)
                        .ok()
                        .and_then(|value| value["runs"].as_array().map(Vec::len))
                        .unwrap_or(0);
                    // Use the exact worker's own receipt parser as well as the
                    // diagnostic's required-check judgement; retain before both.
                    match read_receipt(receipt.as_bytes(), name, &launch.assembled, baseline, named)
                    {
                        Ok(observed) => receipt = observed.original,
                        Err(error) => {
                            observer_refusal =
                                Some(format!("complete worker receipt refused: {error:#}"))
                        }
                    }
                }
                Err(_) => {
                    observer_refusal =
                        Some("worker receipt is not UTF-8; original bytes retained".into())
                }
            }
        } else if !output.status.success() {
            observer_refusal = Some(format!(
                "{container} log observation failed; complete output retained"
            ));
        }
    }
    let exit = |key: &str, name: &str| -> Result<Option<i32>> {
        let Some(statuses) = pod["status"][key].as_array() else {
            return Ok(None);
        };
        let Some(status) = statuses.iter().find(|status| status["name"] == name) else {
            return Ok(None);
        };
        if status["restartCount"].as_u64() != Some(0) {
            bail!("diagnostic container incarnation restarted");
        }
        status["state"]["terminated"]["exitCode"]
            .as_i64()
            .map(i32::try_from)
            .transpose()
            .map_err(Into::into)
    };
    Ok(Some(super::diagnostics::Observation {
        job_uid: launch.job_uid.clone(),
        pod_uid: pod["metadata"]["uid"]
            .as_str()
            .filter(|uid| !uid.is_empty())
            .context("diagnostic pod UID absent")?
            .into(),
        original_output: json!({"commands":super::diagnostic_journal::originals(path)?,"job":job,"pods":pods}),
        original_receipt: receipt,
        init_exit: exit("initContainerStatuses", "clone")?,
        worker_exit: exit("containerStatuses", "consist")?,
        observer_refusal,
    }))
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

#[derive(Debug)]
struct ReceiptObservation {
    verdict: ConsistVerdict,
    original: String,
}

impl std::ops::Deref for ReceiptObservation {
    type Target = ConsistVerdict;
    fn deref(&self) -> &Self::Target {
        &self.verdict
    }
}

fn read_receipt(
    log: &[u8],
    name: &str,
    head: &str,
    baseline: &str,
    files_named: usize,
) -> Result<ReceiptObservation> {
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
    Ok(ReceiptObservation {
        verdict,
        original: String::from_utf8(log.to_vec()).context("original receipt is not UTF-8")?,
    })
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
            )
            .map(|observation| observation.verdict);
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
    edit_level: Option<Value>,
) -> ConsistVerdict {
    let clone = clone.to_string();
    let url = url.to_string();
    let policy = policy.clone();
    let result = tokio::task::spawn_blocking(move || -> Result<ConsistVerdict> {
        validate_clone_url(&url)?;
        let head = sh(&["git","-C",&clone,"rev-parse","HEAD"])?;
        let baseline = sh(&["git","-C",&clone,"rev-parse","origin/main"])?;
        let head_text = String::from_utf8(head.stdout).context("assembled head is not text")?;
        let baseline_text = String::from_utf8(baseline.stdout).context("baseline head is not text")?;
        let head = head_text.trim();
        let baseline = baseline_text.trim();
        let name = format!("consist-{}", uuid::Uuid::new_v4().simple());
        let reference = format!("refs/heads/{name}");
        sh(&["git","-C",&clone,"push","fork",&format!("{head}:{reference}")])?;
        let job = manifest(&name,&url,&reference,head,baseline,policy.consist_budget.as_secs(),edit_level.as_ref())?;
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
    #[test]
    fn diagnostic_manifest_preserves_the_clone_url_boundary() {
        let head = "a".repeat(40);
        let baseline = "b".repeat(40);
        let level = serde_json::json!({"edit_level":"source"});
        for safe in [
            "http://forge.invalid/example/repository.git",
            "https://forge.invalid/example/repository.git",
        ] {
            let job = super::diagnostic_manifest(
                "consist-url-control",
                safe,
                &head,
                &baseline,
                30,
                &level,
            )
            .unwrap();
            assert!(job.to_string().contains(safe));
        }
        let mut dummy_userinfo =
            reqwest::Url::parse("https://forge.invalid/example/repository.git").unwrap();
        dummy_userinfo.set_username("review-user").unwrap();
        dummy_userinfo.set_password(Some("not-a-secret")).unwrap();
        for refused in [
            dummy_userinfo.as_str(),
            "https://review-user@forge.invalid/example/repository.git",
            "https://forge.invalid/example/repository.git?ordinary=fixture",
            "https://forge.invalid/example/repository.git#ordinary-fixture",
            "ssh://forge.invalid/example/repository.git",
            "file:///ordinary-fixture/repository.git",
        ] {
            assert!(
                super::diagnostic_manifest(
                    "consist-url-refusal",
                    refused,
                    &head,
                    &baseline,
                    30,
                    &level
                )
                .is_err(),
                "diagnostic manifest bypassed the ordinary clone URL boundary"
            );
        }
    }
    #[test]
    fn retained_terminal_originals_do_not_consume_compute_capacity() {
        assert!(
            super::diagnostic_capacity_free(&serde_json::json!({"items":[
                {"status":{"conditions":[{"type":"Complete","status":"True"}]}},
                {"status":{"conditions":[{"type":"Failed","status":"True"}]}}
            ]}))
            .unwrap()
        );
        assert!(
            !super::diagnostic_capacity_free(&serde_json::json!({"items":[
                {"status":{"active":1}}
            ]}))
            .unwrap()
        );
        assert!(
            !super::diagnostic_capacity_free(&serde_json::json!({"items":[
                {"status":{"conditions":[{"type":"Complete","status":"False"}]}}
            ]}))
            .unwrap()
        );
        assert!(super::diagnostic_capacity_free(&serde_json::json!({})).is_err());
    }
    use super::*;

    #[test]
    fn the_consist_client_binds_validation_and_authentication_to_one_context() {
        let config = session_config(
            "kubernetes.default.svc",
            "443",
            Path::new(SERVICE_ACCOUNT_ROOT),
        )
        .unwrap();
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
                session_config(host, port, Path::new(SERVICE_ACCOUNT_ROOT)).is_err(),
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
        let control = ConsistControl {
            config: file,
            recording: None,
            program: KUBECTL.into(),
        };
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

    /// A stand-in cluster session: a kubectl that answers the controller's
    /// identity, its authority and an empty capacity census, and a mount
    /// holding the two files a session requires. Nothing else answers.
    fn stand_in_native(name: &str) -> Native {
        let dir = boss_testing::scratch_dir(name);
        let account_root = dir.join("serviceaccount");
        std::fs::create_dir_all(&account_root).unwrap();
        for file in ["token", "ca.crt"] {
            std::fs::write(account_root.join(file), b"stand-in").unwrap();
        }
        let program = dir.join("kubectl");
        boss_testing::write_exec(
            &program,
            &format!(
                "#!/bin/sh\ncase \"$*\" in\n*'auth whoami'*) printf '%s\\n' '{{\"status\":{{\"userInfo\":{{\"username\":\"{CONTROLLER_IDENTITY}\"}}}}}}' ;;\n*'auth can-i create jobs'*) printf 'yes\\n' ;;\n*'get jobs'*) printf '%s\\n' '{{\"items\":[]}}' ;;\n*) exit 9 ;;\nesac\n"
            ),
        );
        Native {
            program,
            account_root,
            host: "10.0.0.1".into(),
            port: "443".into(),
        }
    }

    fn journal_files(path: &Path) -> (usize, usize) {
        let pending = path.join("pending");
        let count = |dir: &Path| {
            std::fs::read_dir(dir).map_or(0, |entries| {
                entries
                    .filter(|entry| entry.as_ref().unwrap().path().is_file())
                    .count()
            })
        };
        (count(path), count(&pending))
    }

    /// Backlog daab12f4 (measured 2026-10-17 on assessment 28e96fbc): every
    /// pass's identity check journaled its kubectl invocations INTO the
    /// request it served, so a request open long enough filled its own
    /// complete-read budget from elapsed time alone and went UNKNOWN. The
    /// per-pass gate is observed and logged, never journaled into the
    /// request: however many passes run, the request's files do not move.
    #[test]
    fn a_per_pass_identity_check_spends_nothing_of_the_request_it_serves() {
        let root = tempfile::tempdir().unwrap();
        let journal =
            super::super::diagnostic_journal::private_directory(&root.path().join("request"))
                .unwrap();
        super::super::diagnostic_journal::keep(&journal, &json!({"original":"prior evidence"}))
            .unwrap();
        let before = journal_files(&journal);
        let native = || Ok(stand_in_native("per-pass-identity"));
        for check_capacity in [false, true, false, true] {
            assert_eq!(
                diagnostic_identity_on(&journal, check_capacity, native).unwrap(),
                CONTROLLER_IDENTITY
            );
        }
        assert_eq!(
            journal_files(&journal),
            before,
            "per-pass identity checks wrote into the served request's journal"
        );
        assert_eq!(
            super::super::diagnostic_journal::originals(&journal)
                .unwrap()
                .len(),
            1
        );
    }

    fn receipt() -> Value {
        json!({"version":1,"identity":"consist-test","head":"head","baseline":"main","exclusions":{"exit":0,"stdout":"","stderr":""},"runs":[{"name":"real-check","exit":0,"stdout":"","stderr":""}],"warnings":[]})
    }

    #[test]
    fn diagnostic_observation_keeps_successful_original_output_before_judging() {
        let mut original = receipt();
        original["runs"][0]["stdout"] = json!("complete ordinary successful output");
        original["runs"][0]["stderr"] = json!("ordinary successful diagnostic");
        let bytes = serde_json::to_vec(&original).unwrap();
        let verdict = read_receipt(&bytes, "consist-test", "head", "main", 10).unwrap();
        assert_eq!(verdict.original.as_bytes(), bytes);
        assert!(
            format!("{verdict:?}").contains("complete ordinary successful output"),
            "a request executor cannot persist successful stdout after the controller discards it: {verdict:?}"
        );
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
                .arg("")
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
        let ConsistVerdict::Refuse { failed, ran, .. } = verdict.verdict else {
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
        boss_testing::write_exec(
            &tree.join("infra/lint/real-failure.sh"),
            "#!/usr/bin/env bash\nprintf '\\377\\376'\nprintf '\\375' >&2\nexit 0\n",
        );
        let binary = invoke(head);
        assert!(binary.status.success());
        let original: Value = serde_json::from_slice(&binary.stdout).unwrap();
        assert_eq!(
            original["runs"][0]["stdout_b64"], "//4=",
            "original stdout bytes were discarded before storage"
        );
        assert_eq!(
            original["runs"][0]["stderr_b64"], "/Q==",
            "original stderr bytes were discarded before storage"
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
            Some(&json!({"edit_level": null, "manifest": "/opt/boss/tenant/seeds/tenant.toml"})),
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
    fn retained_worker_generations_preserve_exact_isolation_fences() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let code = r#"import copy, importlib.util, json, pathlib, sys
root = pathlib.Path(sys.argv[1])
spec = importlib.util.spec_from_file_location('declaration', root/'infra/consist-worker/declaration.py')
d = importlib.util.module_from_spec(spec); spec.loader.exec_module(d)
t = json.loads((root/'infra/consist-worker/job.json').read_text())
values={'$NAME':'consist-'+'a'*32,'$IMAGE':'example.invalid/worker:fixed','$CLONE':'old clone',
'$WORKER':'old worker','$URL':'https://example.invalid/repo.git','$REFERENCE':'refs/heads/test',
'$HEAD':'1'*40,'$BASELINE':'2'*40,'$BUDGET':'120','$DEADLINE':300,'$EDIT_LEVEL':'','$TMP_SUBPATH':'private'}
def fill(v):
    if isinstance(v,str): return values.get(v,v)
    if isinstance(v,list): return [fill(x) for x in v]
    if isinstance(v,dict): return {k:fill(x) for k,x in v.items()}
    return v
current=fill(t); assert d.recognized(t,current)
old=copy.deepcopy(current); p=old['spec']['template']['spec']
p['initContainers'][0]['command'].pop()
p['initContainers'][0]['volumeMounts'].pop()
del p['containers'][0]['volumeMounts'][1]['subPath']
assert d.recognized(t,old), 'retained pre-private-scratch job must remain recognized'
for candidate in [current,old]:
    earlier=copy.deepcopy(candidate); earlier['spec']['template']['spec']['containers'][0]['command'].pop()
    assert d.recognized(t,earlier), 'existing EDIT_LEVEL generation remains recognized'
    for bad in [candidate,earlier]:
        for field in ['hostPID','hostIPC','hostNetwork','automountServiceAccountToken','shareProcessNamespace']:
            altered=copy.deepcopy(bad); altered['spec']['template']['spec'][field]=True
            assert not d.recognized(t,altered), field
        altered=copy.deepcopy(bad); altered['spec']['template']['spec']['containers'][0]['securityContext']['readOnlyRootFilesystem']=False
        assert not d.recognized(t,altered)
hybrid=copy.deepcopy(old); hybrid['spec']['template']['spec']['containers'][0]['volumeMounts'][1]['subPath']='private'
assert not d.recognized(t,hybrid), 'partial historical shape is not an admitted generation'
"#;
        let result = std::process::Command::new("python3")
            .arg("-c")
            .arg(code)
            .arg(root)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "retained-generation contract: {result:?}"
        );
    }

    #[test]
    fn worker_tmp_is_the_owned_child_prepared_by_the_trusted_bootstrap() {
        let job = manifest(
            "consist-test",
            "https://example.invalid/repo.git",
            "refs/heads/test",
            "head",
            "main",
            60,
            None,
        )
        .unwrap();
        let pod = &job["spec"]["template"]["spec"];
        let mounts = pod["containers"][0]["volumeMounts"].as_array().unwrap();
        let scratch = mounts.iter().find(|m| m["mountPath"] == "/tmp").unwrap();
        assert_eq!(scratch["name"], "worker-tmp");
        assert_eq!(
            scratch["subPath"], "private",
            "mount the prepared child, never the kubelet-owned volume root"
        );
        let clone = &pod["initContainers"][0];
        assert!(
            clone["volumeMounts"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["name"] == "worker-tmp" && m["mountPath"] == "/consist-tmp")
        );
        let bootstrap = clone["command"][2].as_str().unwrap();
        let prepare = bootstrap
            .find("prepare_consist_tmp /consist-tmp \"$6\"")
            .unwrap();
        assert!(prepare < bootstrap.find("git init").unwrap());
        assert!(
            bootstrap.contains("prepare_consist_tmp()"),
            "the exact trusted bootstrap carries the producer"
        );
        assert_eq!(
            clone["command"].as_array().unwrap().last().unwrap(),
            &scratch["subPath"],
            "producer and mounted child derive from one definition"
        );
    }

    #[test]
    fn worker_scratch_recognition_refuses_unbound_or_traversing_children() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let code = r#"import importlib.util, json, pathlib, sys
root = pathlib.Path(sys.argv[1])
spec = importlib.util.spec_from_file_location('declaration', root/'infra/consist-worker/declaration.py')
d = importlib.util.module_from_spec(spec); spec.loader.exec_module(d)
for bad in ['', '.', '..', '../private', '/private', 'a/b', 'x'*65]:
    assert not d.binding('$TMP_SUBPATH', bad), bad
assert d.binding('$TMP_SUBPATH', 'private')
template = json.loads((root/'infra/consist-worker/job.json').read_text())
d.validate_template(template)
bindings = {}
assert d.matches('$TMP_SUBPATH', 'private', bindings)
assert not d.matches('$TMP_SUBPATH', 'different', bindings)
"#;
        let result = std::process::Command::new("python3")
            .arg("-c")
            .arg(code)
            .arg(root)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "unsafe or inconsistent child was recognized: {result:?}"
        );
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
            None,
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
            4
        );
        let worker = &pod["containers"][0];
        assert_eq!(worker["securityContext"]["readOnlyRootFilesystem"], true);
        assert_eq!(
            worker["volumeMounts"],
            json!([{"name":"checkout","mountPath":"/work","readOnly":true},{"name":"worker-tmp","mountPath":"/tmp","subPath":TMP_SUBPATH}])
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
        let ConsistVerdict::Refuse { failed, .. } = verdict.verdict else {
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

    /// THE CONSIST'S LINTS ARE HANDED THE EDIT LEVEL (backlog 37742794).
    /// The worker runs the assembled tree, so it holds no machine token
    /// (design c395e62c) — and until this, the edit-level lint read the
    /// door itself from every consist pod: a tokenless GET signed
    /// automation:gate-runner, 27 would-refuse facts (one per consist pod)
    /// in the 24 h to 2026-10-09T05:30Z, and under enforce a refusal the
    /// lint answers with exit 3, which this reader counts as a FAILED
    /// lint, so every train would be refused at its consist. The
    /// conductor holds the token and runs no branch code, so it reads
    /// the door ONCE and hands the answer down, as `boss gate` and the
    /// train's gate launch already do (backlog 934ccad1).
    #[test]
    fn the_consist_lints_are_handed_the_edit_level_and_read_no_door() {
        // The rendered Job carries the door's own object, compact, as the
        // worker's last argument — and nothing for no answer or another
        // shape, exactly the set `boss gate` refuses to hand.
        let command = |answer: Option<Value>| {
            let job = manifest(
                "consist-test",
                "https://example.invalid/repo.git",
                "refs/heads/test",
                "head",
                "main",
                60,
                answer.as_ref(),
            )
            .unwrap();
            job["spec"]["template"]["spec"]["containers"][0]["command"]
                .as_array()
                .unwrap()
                .clone()
        };
        let answer =
            json!({"edit_level": "data", "manifest": "/opt/boss/tenant/seeds/tenant.toml"});
        let handed = command(Some(answer.clone()));
        assert_eq!(handed[handed.len() - 2], "/work/repo");
        assert_eq!(handed[handed.len() - 1], json!(answer.to_string()));
        for not in [None, Some(json!("data")), Some(json!({"error": "denied"}))] {
            assert_eq!(command(not.clone()).last(), Some(&json!("")), "{not:?}");
        }

        // The fixed worker puts a handed answer where the lint reads it,
        // and an empty one nowhere: the lint then says none was handed.
        let tree = boss_testing::scratch_dir("consist-worker-edit-level");
        fs::create_dir_all(tree.join("infra/lint")).unwrap();
        boss_testing::write_exec(&tree.join("infra/gate.sh"), "#!/usr/bin/env bash\nexit 0\n");
        boss_testing::write_exec(
            &tree.join("infra/lint/reads-the-level.sh"),
            "#!/usr/bin/env bash\nprintf 'level=%s\\n' \"${BOSS_EDIT_LEVEL_ANSWER-unset}\"\n",
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
        let head = String::from_utf8(out.stdout).unwrap().trim().to_string();
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&tree)
                .args(["update-ref", "refs/remotes/origin/main", &head])
                .status()
                .unwrap()
                .success()
        );
        let seen = |handed: &str| {
            let result = Command::new("python3")
                .args(["-c", WORKER, &head, &head, "10", "consist-test"])
                .arg(&tree)
                .arg(handed)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            let receipt: Value = serde_json::from_slice(&result.stdout).unwrap();
            receipt["runs"][0]["stdout"].as_str().unwrap().to_string()
        };
        let compact = answer.to_string();
        assert_eq!(seen(&compact), format!("level={compact}\n"));
        assert_eq!(seen(""), "level=unset\n");
    }
}
