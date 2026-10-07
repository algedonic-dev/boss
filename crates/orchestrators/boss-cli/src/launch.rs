//! An existing run's read-only executor adapters (2f7b8c00, approved design 59459712).
//! The first admission of an execution packet reserves one process across
//! hosts. An uncertain response never starts or retries an executor. Native
//! stdout supplies thread identity and outcome, not an observed model, spend,
//! a child heartbeat, or evidence that the assigned work was delivered.
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const KIND: &str = "agent-execution";
const SOURCE: &str = "codex.exec.json";

#[derive(clap::Subcommand)]
pub enum Cmd {
    /// Launch an already dispatched analyst or reviewer through a native adapter.
    ///
    /// Uses an isolated worktree and harness-specific read-only controls. Only a fresh,
    /// confirmed reservation starts a process; reruns and uncertain admission
    /// refuse. The receipt records native identity/outcome, with model unknown
    /// until a separate native transcript supplies it. It completes no work
    /// step and prices no usage. Builder execution remains unsupported.
    Launch {
        run: String,
        #[arg(long, value_parser = ["codex", "gemini"])]
        harness: String,
        /// A new private directory for this attempt's worktree and full logs.
        #[arg(long)]
        scratch: PathBuf,
    },
}

pub async fn dispatch(cmd: Cmd) -> Result<()> {
    let Cmd::Launch {
        run,
        harness,
        scratch,
    } = cmd;
    run_at(&run, &scratch, &harness).await
}

fn text<'a>(v: &'a Value, field: &str) -> Result<&'a str> {
    v.get(field)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .with_context(|| format!("missing or malformed {field}"))
}

fn execution_id(run: &str) -> String {
    let digest = Sha256::digest(format!("BOSS agent-execution v1:{run}"));
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(bytes).to_string()
}

async fn api(
    http: &boss_core::machine_token::Client,
    base: &str,
    actor: &str,
    method: reqwest::Method,
    path: &str,
    body: Option<Value>,
) -> Result<Option<Value>> {
    crate::gate::api_at_signed(
        http,
        base,
        method,
        path,
        body,
        crate::identity::Signature::As(actor.into()),
    )
    .await
}

async fn get(
    http: &boss_core::machine_token::Client,
    base: &str,
    actor: &str,
    id: &str,
) -> Result<Value> {
    let v = api(
        http,
        base,
        actor,
        reqwest::Method::GET,
        &format!("/api/jobs/{id}"),
        None,
    )
    .await?
    .context("the packet read returned no body")?;
    if v.get("id").and_then(Value::as_str) != Some(id) {
        bail!("the read did not identify the requested packet {id}");
    }
    Ok(v)
}

async fn patch(
    http: &boss_core::machine_token::Client,
    base: &str,
    actor: &str,
    id: &str,
    body: Value,
) -> Result<Value> {
    api(
        http,
        base,
        actor,
        reqwest::Method::PATCH,
        &format!("/api/jobs/{id}/metadata"),
        Some(body.clone()),
    )
    .await?;
    let held = get(http, base, actor, id).await?;
    let (_, took) = crate::job::confirm_patch(&held["metadata"], &body);
    if !took {
        bail!(
            "execution {id} did not retain its observation; full local evidence remains in the attempt directory"
        );
    }
    Ok(held)
}

fn one_step<'a>(job: &'a Value, slug: &str) -> Result<&'a Value> {
    let found = crate::envelope::steps(job)
        .into_iter()
        .filter(|s| s["spec_slug"] == slug)
        .collect::<Vec<_>>();
    if found.len() != 1 {
        bail!("execution has no unique {slug} step");
    }
    Ok(found[0])
}

async fn finish(
    http: &boss_core::machine_token::Client,
    base: &str,
    actor: &str,
    id: &str,
    result: &str,
) -> Result<()> {
    let job = get(http, base, actor, id).await?;
    let sid = text(one_step(&job, "observing")?, "id")?;
    let path = format!("/api/jobs/{id}/steps/{sid}");
    api(
        http,
        base,
        actor,
        reqwest::Method::PATCH,
        &format!("{path}/metadata"),
        Some(json!({"result":result})),
    )
    .await?;
    let held = get(http, base, actor, id).await?;
    if one_step(&held, "observing")?["metadata"]["result"] != result {
        bail!("execution result did not read back; its observation is preserved");
    }
    api(
        http,
        base,
        actor,
        reqwest::Method::PUT,
        &path,
        Some(json!({"status":"completed"})),
    )
    .await?;
    let held = get(http, base, actor, id).await?;
    if one_step(&held, "observing")?["status"] != "completed" {
        bail!("execution observation completion did not read back");
    }
    let terminal = one_step(&held, result)?;
    if terminal["status"] != "completed" {
        let tid = text(terminal, "id")?;
        api(
            http,
            base,
            actor,
            reqwest::Method::PUT,
            &format!("/api/jobs/{id}/steps/{tid}"),
            Some(json!({"status":"completed"})),
        )
        .await?;
    }
    let closed = get(http, base, actor, id).await?;
    if closed["status"] != "closed" || one_step(&closed, result)?["status"] != "completed" {
        bail!("execution terminal did not read back");
    }
    Ok(())
}

async fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let output = tokio::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .await?;
    if !output.status.success() {
        bail!("git refused preparing the isolated executor worktree");
    }
    Ok(String::from_utf8(output.stdout)?.trim().into())
}

/// Select once, then spawn by absolute path. execvp would otherwise skip a
/// selected executable with a broken interpreter and run a later harness.
async fn executable(name: &str) -> Result<PathBuf> {
    let paths = std::env::var_os("PATH").context("the executor has no executable search path")?;
    for directory in std::env::split_paths(&paths) {
        let candidate = directory.join(name);
        let Ok(metadata) = tokio::fs::metadata(&candidate).await else {
            continue;
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o111 == 0 {
                continue;
            }
        }
        if metadata.is_file() {
            return Ok(tokio::fs::canonicalize(&candidate).await?);
        }
    }
    bail!("no executable {name} adapter was found")
}

/// Admission and worktree preparation can outlast a source claim. Read it
/// again immediately before spawn; separate packet reads are not an atomic
/// ownership lock for the executor's lifetime.
async fn revalidate_start(
    http: &boss_core::machine_token::Client,
    base: &str,
    actor: &str,
    run: &str,
    registered: &Value,
    packet: &Value,
) -> Result<()> {
    let fresh = get(http, base, actor, run).await?;
    if ["kind", "partition", "owner_id"]
        .iter()
        .any(|key| fresh[key] != registered[key])
        || fresh["status"] != "open"
        || [
            "agent", "model", "effort", "profile", "brief", "packet", "step",
        ]
        .iter()
        .any(|key| fresh["metadata"][key] != registered["metadata"][key])
    {
        bail!("registered run or submitted controls changed");
    }
    let building = one_step(&fresh, "building")?;
    let original = one_step(registered, "building")?;
    if !matches!(building["status"].as_str(), Some("ready" | "active"))
        || building["id"] != original["id"]
        || building["assignee_id"] != actor
    {
        bail!("registered building step no longer belongs to this run and actor");
    }
    let packet_id = text(&registered["metadata"], "packet")?;
    let slug = text(&registered["metadata"], "step")?;
    let fresh = get(http, base, actor, packet_id).await?;
    let source = one_step(&fresh, slug)?;
    if fresh["status"] != "open"
        || fresh["kind"] != packet["kind"]
        || fresh["partition"] != packet["partition"]
        || source["id"] != one_step(packet, slug)?["id"]
        || source["status"] != "active"
        || source["assignee_id"] != actor
        || source["metadata"]["agent_run"] != run
    {
        bail!("source step no longer has this actor's exact active run claim");
    }
    Ok(())
}

/// Exactly one native thread and one complete turn are needed for success.
/// Unknown event types carry no extra claims. The complete raw stream is kept
/// on disk; only native control events enter the durable observation.
#[derive(Default)]
struct Native {
    gemini: bool,
    configured_model: Option<String>,
    thread: Option<String>,
    started: bool,
    completed: bool,
    result_seen: bool,
    failed: bool,
    malformed: bool,
    launcher_error: Option<String>,
    capture_incomplete: bool,
    events: Vec<Value>,
}
impl Native {
    fn read(&mut self, line: &[u8]) {
        let Ok(v) = serde_json::from_slice::<Value>(line) else {
            self.malformed = true;
            return;
        };
        let Some(kind) = v.get("type").and_then(Value::as_str) else {
            self.malformed = true;
            return;
        };
        if self.gemini {
            match kind {
                "init" => {
                    let id = v
                        .get("session_id")
                        .and_then(Value::as_str)
                        .filter(|s| uuid::Uuid::parse_str(s).is_ok());
                    let model = v
                        .get("model")
                        .and_then(Value::as_str)
                        .filter(|s| !s.trim().is_empty());
                    if let (Some(id), Some(model)) = (id.filter(|_| self.thread.is_none()), model) {
                        self.thread = Some(id.into());
                        self.configured_model = Some(model.into());
                        self.started = true;
                    } else {
                        self.malformed = true;
                    }
                    self.events.push(v);
                }
                "result" => {
                    if !self.started
                        || self.result_seen
                        || !v
                            .get("stats")
                            .is_some_and(crate::gemini_execution::complete_stats)
                    {
                        self.malformed = true;
                    }
                    match v.get("status").and_then(Value::as_str) {
                        Some("success") if !self.failed => self.completed = true,
                        Some("error") => self.failed = true,
                        _ => self.malformed = true,
                    }
                    self.result_seen = true;
                    self.events.push(v);
                }
                "error" => {
                    if self.result_seen {
                        self.malformed = true;
                    }
                    match v.get("severity").and_then(Value::as_str) {
                        Some("warning") => {}
                        Some("error") => self.failed = true,
                        _ => self.malformed = true,
                    }
                    self.events.push(v);
                }
                "message" | "tool_use" | "tool_result" => {
                    if !self.started || self.result_seen {
                        self.malformed = true;
                    }
                }
                _ => self.malformed = true,
            }
            return;
        }
        match kind {
            "thread.started" => {
                let id = v
                    .get("thread_id")
                    .and_then(Value::as_str)
                    .filter(|s| uuid::Uuid::parse_str(s).is_ok());
                if let Some(id) = id.filter(|_| self.thread.is_none()) {
                    self.thread = Some(id.into());
                    self.events.push(json!({"type":kind,"thread_id":id}));
                } else {
                    self.malformed = true;
                }
            }
            "turn.started" => {
                if self.thread.is_none() || self.started {
                    self.malformed = true;
                }
                self.started = true;
                self.events.push(json!({"type":kind}));
            }
            "turn.completed" => {
                if !self.started || self.completed || self.failed {
                    self.malformed = true;
                }
                self.completed = true;
                self.events.push(json!({"type":kind}));
            }
            "turn.failed" | "error" => {
                self.failed = true;
                self.events.push(v);
            }
            _ => {}
        }
    }
    fn receipt(&self, run: &str, execution: &str, code: Option<i32>, scratch: &Path) -> Value {
        let status = if self.malformed
            || self.thread.is_none()
            || (!self.completed && !self.failed && code == Some(0))
        {
            "unknown"
        } else if self.failed || code != Some(0) {
            "failed"
        } else {
            "completed"
        };
        let mut receipt = json!({"source":if self.gemini { "gemini.stream-json" } else { SOURCE },"agent_run":run,"execution":execution,"runtime_id":self.thread,
            "parent_runtime_id":null,"status":status,"observed_model":null,
            "observed_at":boss_clock_client::wall_now().to_rfc3339(),"exit_code":code,
            "native_events":self.events,"launcher_error":self.launcher_error,"capture_incomplete":self.capture_incomplete,
            "stdout_file":scratch.join("native.jsonl"),"stderr_file":scratch.join("native.stderr")});
        if self.gemini {
            receipt["configured_model"] = json!(self.configured_model);
        }
        receipt
    }
}

async fn run_at(run: &str, scratch: &Path, harness: &str) -> Result<()> {
    uuid::Uuid::parse_str(run).context("launch requires the full registered agent-run UUID")?;
    let actor = crate::identity::sign(&reqwest::Method::POST, "/api/jobs")?;
    let base = crate::gate::resolve_jobs_base(None)?;
    // Bound each connection and response body, not the healthy executor's
    // lifetime. A silent observation request must unblock paired native IO.
    let http = crate::gate::machine_client_with(
        reqwest::Client::builder().timeout(std::time::Duration::from_secs(10)),
    )?;
    let job = get(&http, &base, &actor, run).await?;
    if job["kind"] != "agent-run" || job["status"] != "open" || job["partition"] != "real" {
        bail!("launch requires an open real registered agent-run");
    }
    let md = &job["metadata"];
    if text(md, "agent")? != actor {
        bail!("the run belongs to another actor; launch changes no identity");
    }
    let profile = text(md, "profile")?;
    if !matches!(profile, "analyst" | "reviewer") {
        bail!("native adapters support analyst/reviewer read-only runs");
    }
    let model = text(md, "model")?;
    if !(if harness == "gemini" {
        model == "gemini-2.5-pro"
    } else {
        model.starts_with("gpt-")
    }) || !boss_jobs::agent_spec::known_models()
        .iter()
        .any(|m| m == model)
    {
        bail!("the requested model is not admitted for this native adapter");
    }
    let effort = text(md, "effort")?;
    if !matches!(effort, "low" | "medium" | "high" | "xhigh") {
        bail!("the requested effort is unsupported by this native adapter");
    }
    let brief = text(md, "brief")?;
    let packet_id = text(md, "packet")?;
    let slug = text(md, "step")?;
    let packet = get(&http, &base, &actor, packet_id).await?;
    let steps = crate::envelope::steps(&packet)
        .into_iter()
        .filter(|s| s["spec_slug"] == slug)
        .collect::<Vec<_>>();
    if packet["status"] != "open"
        || steps.len() != 1
        || steps[0]["status"] != "active"
        || steps[0]["assignee_id"] != actor
        || steps[0]["metadata"]["agent_run"] != run
    {
        bail!("the source step no longer has this actor's exact active run claim");
    }
    let building = crate::envelope::steps(&job)
        .into_iter()
        .filter(|s| s["spec_slug"] == "building")
        .collect::<Vec<_>>();
    if building.len() != 1 || !matches!(building[0]["status"].as_str(), Some("ready" | "active")) {
        bail!("the registered run is no longer building");
    }
    let repo =
        PathBuf::from(git(&std::env::current_dir()?, &["rev-parse", "--show-toplevel"]).await?);
    let head = git(&repo, &["rev-parse", "HEAD"]).await?;
    let execution = execution_id(run);
    let requested = json!({"model":model,"effort":effort,"profile":profile,"sandbox":if harness=="gemini" {"native-file-read-policy"} else {"read-only"}});
    let body = json!({"id":execution,"kind":KIND,"subject":{"subject_kind":"custom","id":run},
        "title":format!("{harness} execution of {run}"),"owner_id":job["owner_id"],"priority":"standard","status":"open","tags":["executor-adapter"],
        "metadata":{"agent_run":run,"agent":actor,"harness":if harness == "gemini" {"gemini.stream-json"} else {"codex.exec"},"requested":requested,"status":"reserved","source_head":head}});
    // The shared API helper erases HTTP status. This single admission MUST
    // retain it: a duplicate200 is not permission to start a second process.
    let response = http
        .post(format!("{base}/api/jobs"))
        .header("x-boss-user", crate::identity::header(&actor))
        .json(&body)
        .send()
        .await
        .context(
            "execution admission uncertain; no process started; never retry this as a new launch",
        )?;
    if response.status() != reqwest::StatusCode::CREATED {
        bail!(
            "execution {execution} was not freshly admitted (HTTP {}); no process started",
            response.status()
        );
    }
    let answer: Value = response
        .json()
        .await
        .context("admission body unverified; no process started")?;
    if answer["id"] != execution || answer.get("already_admitted").is_some() {
        bail!("admission identity unverified; no process started");
    }
    let admitted = get(&http, &base, &actor, &execution).await?;
    if admitted["kind"] != KIND || admitted["metadata"] != body["metadata"] {
        // Admission stamps opened_by. Confirm only the keys we submitted.
        let (_, took) = crate::job::confirm_patch(&admitted["metadata"], &body["metadata"]);
        if admitted["kind"] != KIND || !took {
            bail!("execution reservation did not read back; no process started");
        }
    }
    let observer = text(one_step(&admitted, "observing")?, "id")?;
    api(
        &http,
        &base,
        &actor,
        reqwest::Method::POST,
        &format!("/api/jobs/{execution}/steps/{observer}/claim"),
        None,
    )
    .await?;
    let claimed = get(&http, &base, &actor, &execution).await?;
    let observing = one_step(&claimed, "observing")?;
    if observing["status"] != "active" || observing["assignee_id"] != actor {
        bail!("the execution observer claim did not read back; no process started");
    }
    let mut created = false;
    let attempt=async {
    let mut directory=tokio::fs::DirBuilder::new();
    #[cfg(unix)]
    // mode-bits-ok: only the private attempt directory is made traversable
    directory.mode(0o700);
    directory.create(scratch).await.context("attempt directory must be new; reservation remains visible, no process started")?;
    created=true;
    let scratch=tokio::fs::canonicalize(scratch).await?;
    let workspace=scratch.join("worktree");
    let workspace_text=workspace.to_str().context("worktree path is not UTF-8")?;
    git(&repo,&["worktree","add","--detach","--lock",workspace_text,&head]).await?;
    tokio::fs::write(scratch.join("reservation.json"),serde_json::to_vec_pretty(&admitted)?).await?;
    let prompt=format!("{brief}\n\n== EXECUTOR ADAPTER ==\nYour run is agent-run {run}.\nRequested model {model}, effort {effort}; submitted controls are not observed execution.\nBOSS_ACTOR={actor} and BOSS_AGENT_RUN={run}.\n{}\nThis adapter uses {harness} in an isolated worktree with read-only controls. Preserve the brief's safety and evidence requirements using your harness's own tools. Do not start unregistered paid executors. The launcher records no heartbeat, delivery or usage on your behalf.\n",crate::dispatch::marker_line(run));
    let stderr=tokio::fs::OpenOptions::new().write(true).create_new(true).open(scratch.join("native.stderr")).await?;
    let mut log=tokio::fs::OpenOptions::new().write(true).create_new(true).open(scratch.join("native.jsonl")).await?;
    let executable=executable(harness).await?;
    let mut command=tokio::process::Command::new(&executable);
    if harness == "gemini" {
        let key=std::env::var_os("GEMINI_API_KEY").filter(|value| !value.is_empty()).context("Gemini environment API key is unavailable; no ambient credential fallback")?;
        let prepared=crate::gemini_execution::prepare(&scratch,&workspace,effort).await?;
        crate::gemini_execution::verify_version(&executable,&scratch,&workspace,&prepared).await?;
        prepared.environment(&mut command);
        command.env("GEMINI_API_KEY",key);
        command.args(["--prompt","Execute the persisted BOSS brief supplied on standard input.","--output-format","stream-json","--model",model,"--approval-mode","plan","--extensions","none","--admin-policy"])
            .arg(prepared.policy);
    } else {
        command.args(["exec","--json","--model",model,"-c",&format!("model_reasoning_effort=\"{effort}\""),"--sandbox","read-only","-"]);
    }
    revalidate_start(&http,&base,&actor,run,&job,&packet).await.context("run and source claim must still match before process start")?;
    let command=command
        .current_dir(&workspace).env("BOSS_ACTOR",&actor).env("BOSS_AGENT_RUN",run)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::from(stderr.into_std().await)).kill_on_drop(true).spawn();
    let mut child=match command {
        Ok(child)=>child,
        Err(error)=>{
            return Ok(json!({"source":"os.process","agent_run":run,"execution":execution,
                "runtime_id":null,"parent_runtime_id":null,"observed_model":null,"status":"failed",
                "observed_at":boss_clock_client::wall_now().to_rfc3339(),"launcher_error":format!("{harness} process could not start: {error}"),
                "stdout_file":scratch.join("native.jsonl"),"stderr_file":scratch.join("native.stderr")}));
        }
    };
    let mut stdin=child.stdin.take().context("native process has no prompt input")?;
    let stdout=child.stdout.take().context("native process has no event stream")?;
    let mut reader=BufReader::new(stdout);
    let mut native=Native { gemini:harness=="gemini", ..Default::default() };
    let mut line=Vec::new();
    // Either pipe can fill before the child consumes the other. Drain output
    // while writing the brief; a refused input still retains the native stream.
    let input=async {
        stdin.write_all(prompt.as_bytes()).await.context("native process did not receive its complete brief")?;
        stdin.shutdown().await.context("native prompt input could not close")?;
        drop(stdin);
        Ok::<(),anyhow::Error>(())
    };
    let output=async {
      loop {
        line.clear();
        if reader.read_until(b'\n',&mut line).await?==0 {break;}
        log.write_all(&line).await?;
        let before=native.thread.is_some();
        native.read(&line);
        if !before && native.thread.is_some() {
            log.flush().await?;
            let started=json!({"source":if native.gemini {"gemini.stream-json"} else {SOURCE},"agent_run":run,"execution":execution,
                "runtime_id":native.thread,"parent_runtime_id":null,"status":"accepted","observed_model":null,
                "observed_at":boss_clock_client::wall_now().to_rfc3339(),"native_events":native.events});
            tokio::fs::write(scratch.join("native-start.json"),serde_json::to_vec_pretty(&started)?).await?;
            patch(&http,&base,&actor,&execution,json!({"status":"accepted","observation":started})).await?;
        }
      }
      Ok::<(),anyhow::Error>(())
    };
    if let Err(error)=tokio::try_join!(input,output) {
        // Once either IO leg refuses, cancel the other and stop the process.
        // Retain buffered output; descendants holding the pipe cannot hide the
        // refusal forever. Any incomplete capture is explicit in the receipt.
        child.start_kill().context("the refused native process could not be stopped")?;
        native.malformed=true;
        native.launcher_error=Some(format!("{error:#}"));
        match tokio::time::timeout(std::time::Duration::from_secs(5),tokio::io::copy(&mut reader,&mut log)).await {
            Ok(Ok(_))=>{},
            result=>{
                native.capture_incomplete=true;
                native.launcher_error=Some(format!("{error:#}; remaining native output could not be fully captured: {result:?}"));
            }
        }
    }
    log.flush().await?;
    let status=child.wait().await?;
    Ok::<Value,anyhow::Error>(native.receipt(run,&execution,status.code(),&scratch))
    }.await;
    let receipt = match attempt {
        Ok(receipt) => receipt,
        Err(error) => {
            // Keep any native start already copied before the launcher failed.
            // An existing caller directory is never read or overwritten.
            let started = if created {
                tokio::fs::read(scratch.join("native-start.json"))
                    .await
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            } else {
                None
            };
            json!({"source":"launcher","agent_run":run,"execution":execution,
                "runtime_id":started.as_ref().map(|s|s["runtime_id"].clone()),"parent_runtime_id":null,
                "observed_model":null,"status":"unknown","observed_at":boss_clock_client::wall_now().to_rfc3339(),
                "launcher_error":format!("{error:#}"),"native_start":started})
        }
    };
    if created {
        tokio::fs::write(
            scratch.join("receipt.json"),
            serde_json::to_vec_pretty(&receipt)?,
        )
        .await?;
    }
    patch(
        &http,
        &base,
        &actor,
        &execution,
        json!({"status":receipt["status"],"observation":receipt}),
    )
    .await?;
    finish(
        &http,
        &base,
        &actor,
        &execution,
        receipt["status"]
            .as_str()
            .context("the observation has no result")?,
    )
    .await?;
    println!("{}", serde_json::to_string(&receipt)?);
    if receipt["status"] != "completed" {
        bail!(
            "execution {execution} did not produce an unambiguous successful native turn; attempt evidence retained at {}",
            scratch.display()
        );
    }
    Ok(())
}
