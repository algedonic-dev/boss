//! The launcher runs a registered step and records native execution, rather
//! than turning printed prompts or submitted controls into observed facts.
use std::{path::Path, process::Stdio, sync::Arc};

use boss_core::{
    job::{Job, JobId, JobStatus, Priority, Step, StepStatus, Subject},
    port::EventBus,
    publisher::DomainPublisher,
};
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, JobsRepository, WorkflowRegistry};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use serde_json::{Value, json};

const ACTOR: &str = "agent-codex";
const THREAD: &str = "bbbbbbbb-1111-4111-8111-111111111111";

struct World {
    root: tempfile::TempDir,
    jobs: Arc<InMemoryJobs>,
    base: String,
    run: JobId,
    server: tokio::task::JoinHandle<()>,
    fallback: Option<std::path::PathBuf>,
}

impl Drop for World {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn git(path: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

impl World {
    async fn new() -> Self {
        Self::with_admission("normal").await
    }
    async fn with_admission(mode: &'static str) -> Self {
        let root = tempfile::tempdir().unwrap();
        git(root.path(), &["init", "-q"]);
        git(
            root.path(),
            &["config", "user.email", "fixture@example.test"],
        );
        git(root.path(), &["config", "user.name", "Fixture"]);
        std::fs::write(root.path().join("README"), "isolated launcher fixture\n").unwrap();
        git(root.path(), &["add", "README"]);
        git(root.path(), &["commit", "-qm", "Seed fixture"]);
        let bin = root.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        // A missing fake interpreter makes execvp search the next PATH entry.
        // Keep this fixture's entire executable roster private so it can never
        // fall through to an installed, paid harness.
        #[cfg(unix)]
        for tool in ["git", "cat", "python3", "sleep"] {
            std::os::unix::fs::symlink(format!("/usr/bin/{tool}"), bin.join(tool)).unwrap();
        }
        let native = bin.join("codex");
        boss_testing::write_exec(
            &native,
            &format!(
                r#"#!/bin/sh
set -eu
printf '%s\n' "$BOSS_ACTOR" "$BOSS_AGENT_RUN" "$PWD" > "$FIXTURE_CAPTURE/env"
printf '%s\n' "$BOSS_AGENT_RUN" >> "$FIXTURE_CAPTURE/calls"
printf '%s\n' "$@" > "$FIXTURE_CAPTURE/argv"
cat > "$FIXTURE_CAPTURE/prompt"
printf '%s\n' '{{"type":"thread.started","thread_id":"{THREAD}"}}'
printf '%s\n' '{{"type":"turn.started"}}'
printf '%s\n' '{{"type":"item.completed","item":{{"id":"item_1","type":"agent_message","text":"Fixture result"}}}}'
printf '%s\n' '{{"type":"turn.completed","usage":{{"input_tokens":20,"cached_input_tokens":10,"output_tokens":5}}}}'
"#
            ),
        );
        let jobs = Arc::new(InMemoryJobs::new());
        let today = chrono::Utc::now().date_naive();
        let mut packet = Job::new(
            "backlog-item",
            Subject::new("custom", "launcher-fixture"),
            "A registered step",
            ACTOR,
            Priority::Standard,
            today,
        );
        packet.status = JobStatus::Open;
        let mut source = Step::new(packet.id, "task", "measure", 0).with_assignee(ACTOR);
        source.spec_slug = Some("measure".into());
        source.status = StepStatus::Active;
        let mut run = Job::new(
            "agent-run",
            Subject::new("custom", packet.id.to_string()),
            "A dispatched analyst",
            ACTOR,
            Priority::Standard,
            today,
        );
        run.status = JobStatus::Open;
        run.metadata = json!({"packet":packet.id.to_string(),"step":"measure","agent":ACTOR,"profile":"analyst","model":"gpt-6.1-sol","effort":"high","budget_usd":5,"brief":"The exact persisted dispatch brief."});
        source.metadata = json!({"agent_run":run.id.to_string()});
        let mut building = Step::new(run.id, "task", "building", 0).with_assignee(ACTOR);
        building.spec_slug = Some("building".into());
        building.status = StepStatus::Ready;
        jobs.create_job(&packet).await.unwrap();
        jobs.add_step(&source).await.unwrap();
        jobs.create_job(&run).await.unwrap();
        jobs.add_step(&building).await.unwrap();
        let kinds = Arc::new(InMemoryWorkflows::new());
        let spec = boss_jobs::seed_loader::load_workflows(
            boss_testing::repo_root().join("infra/platform/workflows"),
        )
        .unwrap()
        .into_iter()
        .find(|s| s.kind == "agent-execution")
        .expect("the actual bundled observation protocol");
        kinds.seed(spec).unwrap();
        let policy: Arc<dyn PolicyClient> = Arc::new(
            FakePolicyClient::builder()
                .allow("platform-admin", Action::Read, Resource::job(), Scope::All)
                .allow(
                    "platform-admin",
                    Action::Create,
                    Resource::job(),
                    Scope::All,
                )
                .allow(
                    "platform-admin",
                    Action::Update,
                    Resource::step(),
                    Scope::All,
                )
                .allow(
                    "platform-admin",
                    Action::Update,
                    Resource::job(),
                    Scope::All,
                )
                .build(),
        );
        let bus = boss_testing::RecordingEventBus::new();
        let bus_dyn: Arc<dyn EventBus> = bus.clone();
        let state = JobsApiState {
            kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
            ..JobsApiState::minimal(
                jobs.clone(),
                bus,
                DomainPublisher::new(bus_dyn, "jobs"),
                policy,
                Arc::new(boss_clock_client::WallClockClient),
            )
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let patches = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let run_id = run.id;
        let drift_jobs = jobs.clone();
        let app = router(state).layer(axum::middleware::from_fn(
            move |req: axum::extract::Request, next: axum::middleware::Next| {
                let patches = patches.clone();
                let jobs = drift_jobs.clone();
                let mut source = source.clone();
                let mut run = run.clone();
                let mut building = building.clone();
                async move {
                    let admission =
                        req.method() == axum::http::Method::POST && req.uri().path() == "/api/jobs";
                    let first_observation = matches!(mode, "start-refused" | "start-stalled")
                        && req.method() == axum::http::Method::PATCH
                        && req.uri().path().ends_with("/metadata")
                        && !req.uri().path().contains("/steps/")
                        && patches.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0;
                    let mut response = next.run(req).await;
                    if first_observation {
                        if mode == "start-stalled" {
                            // The server received and stored the native start,
                            // but its response never reaches the paired IO leg.
                            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                        }
                        *response.status_mut() = axum::http::StatusCode::CONFLICT;
                        *response.body_mut() =
                            axum::body::Body::from("fixture observation write refused");
                    }
                    if admission && response.status() == axum::http::StatusCode::CREATED {
                        match mode {
                            "duplicate" => *response.status_mut() = axum::http::StatusCode::OK,
                            "wrong-id" => {
                                *response.body_mut() =
                                    axum::body::Body::from("{\"id\":\"another-packet\"}")
                            }
                            "unverified" => {
                                *response.body_mut() =
                                    axum::body::Body::from("a lost admission body")
                            }
                            "drift-claim" | "drift-link" => {
                                if mode == "drift-claim" {
                                    source.assignee_id = Some("another-agent".into());
                                } else {
                                    source.metadata["agent_run"] = json!("another-run");
                                }
                                let (_, version) =
                                    jobs.get_step_versioned(&source.id).await.unwrap().unwrap();
                                jobs.update_step_if_unchanged_at(
                                    &source,
                                    version,
                                    chrono::Utc::now(),
                                    &[],
                                )
                                .await
                                .unwrap();
                            }
                            "drift-model" | "drift-brief" | "drift-run" => {
                                match mode {
                                    "drift-model" => run.metadata["model"] = json!("gpt-6-sol"),
                                    "drift-brief" => {
                                        run.metadata["brief"] = json!("A replacement brief.")
                                    }
                                    _ => run.status = JobStatus::Closed,
                                }
                                jobs.update_job(&run).await.unwrap();
                            }
                            "drift-building" => {
                                building.status = StepStatus::Completed;
                                let (_, version) = jobs
                                    .get_step_versioned(&building.id)
                                    .await
                                    .unwrap()
                                    .unwrap();
                                jobs.update_step_if_unchanged_at(
                                    &building,
                                    version,
                                    chrono::Utc::now(),
                                    &[],
                                )
                                .await
                                .unwrap();
                            }
                            _ => {}
                        }
                    }
                    response
                }
            },
        ));
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            root,
            jobs,
            base,
            run: run_id,
            server,
            fallback: None,
        }
    }
    async fn launch(&self) -> std::process::Output {
        tokio::process::Command::new(env!("CARGO_BIN_EXE_boss"))
            .current_dir(self.root.path())
            .args([
                "launch",
                &self.run.to_string(),
                "--harness",
                "codex",
                "--scratch",
            ])
            .arg(self.root.path().join("attempt"))
            .env("BOSS_JOBS_URL", &self.base)
            .env("BOSS_ACTOR", ACTOR)
            .env("FIXTURE_CAPTURE", self.root.path())
            .env(
                "PATH",
                std::env::join_paths(
                    std::iter::once(self.root.path().join("bin")).chain(self.fallback.clone()),
                )
                .unwrap(),
            )
            .env(
                "BOSS_MACHINE_TOKEN_DIR",
                self.root.path().join("empty-token"),
            )
            .kill_on_drop(true)
            .stdin(Stdio::null())
            .output()
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn source_and_run_drift_after_admission_never_start_an_executor() {
    for mode in [
        "drift-claim",
        "drift-link",
        "drift-model",
        "drift-brief",
        "drift-run",
        "drift-building",
    ] {
        let world = World::with_admission(mode).await;
        let out = world.launch().await;
        assert!(
            !out.status.success(),
            "{mode}: stale authorization must refuse: {out:?}"
        );
        assert!(
            !world.root.path().join("calls").exists(),
            "{mode}: zero process starts"
        );
        let receipt: Value =
            serde_json::from_slice(&out.stdout).expect("a stale admitted attempt remains recorded");
        assert_eq!(receipt["status"], "unknown");
        assert!(receipt["runtime_id"].is_null());
        assert!(
            receipt["launcher_error"]
                .as_str()
                .unwrap()
                .contains("before process start")
        );
        let id = JobId::from_uuid(
            uuid::Uuid::parse_str(receipt["execution"].as_str().unwrap()).unwrap(),
        );
        let held = world.jobs.get_job(&id).await.unwrap().unwrap();
        assert_eq!(held.metadata["observation"], receipt);
        assert_eq!(held.status, JobStatus::Closed);
        assert!(
            !world.launch().await.status.success(),
            "{mode}: no retry permission"
        );
    }
}

#[tokio::test]
async fn a_stalled_observation_response_stops_and_reaps_the_executor_with_unknown_evidence() {
    let world = World::with_admission("start-stalled").await;
    let mut run = world.jobs.get_job(&world.run).await.unwrap().unwrap();
    run.metadata["brief"] = json!("a".repeat(256 * 1024));
    world.jobs.update_job(&run).await.unwrap();
    boss_testing::write_exec(
        &world.root.path().join("bin/codex"),
        &format!(
            r#"#!/bin/sh
exec python3 -c 'import json, os, pathlib, sys; pathlib.Path(os.environ["FIXTURE_CAPTURE"], "pid").write_text(str(os.getpid())); print(json.dumps({{"type":"thread.started","thread_id":"{THREAD}"}}), flush=True); print(json.dumps({{"type":"item.completed","item":{{"type":"agent_message","text":"a"*262144}}}}), flush=True); sys.stdin.read()'
"#
        ),
    );
    let out=tokio::time::timeout(std::time::Duration::from_secs(18), world.launch()).await.expect("a withheld HTTP response must hit its own deadline, stop the child and preserve evidence");
    assert!(!out.status.success(), "{out:?}");
    let receipt: Value = serde_json::from_slice(&out.stdout)
        .expect("unknown outcome retained durably after transport timeout");
    assert_eq!(receipt["status"], "unknown");
    assert_eq!(receipt["runtime_id"], THREAD);
    assert!(
        receipt["launcher_error"]
            .as_str()
            .unwrap()
            .contains("timed out")
    );
    let local: Value = serde_json::from_slice(
        &std::fs::read(world.root.path().join("attempt/receipt.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(local, receipt);
    let id =
        JobId::from_uuid(uuid::Uuid::parse_str(receipt["execution"].as_str().unwrap()).unwrap());
    assert_eq!(
        world.jobs.get_job(&id).await.unwrap().unwrap().metadata["observation"],
        receipt
    );
    let pid = std::fs::read_to_string(world.root.path().join("pid")).unwrap();
    assert!(
        !Path::new("/proc").join(pid.trim()).exists(),
        "private native child was stopped and reaped"
    );
}

#[tokio::test]
async fn a_refused_start_observation_cannot_deadlock_against_prompt_input() {
    let world = World::with_admission("start-refused").await;
    let mut run = world.jobs.get_job(&world.run).await.unwrap().unwrap();
    run.metadata["brief"] = json!("a".repeat(256 * 1024));
    world.jobs.update_job(&run).await.unwrap();
    boss_testing::write_exec(
        &world.root.path().join("bin/codex"),
        &format!(
            r#"#!/bin/sh
set -eu
printf '%s\n' '{{"type":"thread.started","thread_id":"{THREAD}"}}'
python3 -c 'import json; print(json.dumps({{"type":"item.completed","item":{{"type":"agent_message","text":"a"*262144}}}}))'
cat > "$FIXTURE_CAPTURE/prompt"
"#
        ),
    );
    let out = tokio::time::timeout(std::time::Duration::from_secs(3), world.launch())
        .await
        .expect("a refused output write must unblock a pending prompt write");
    assert!(!out.status.success(), "{out:?}");
    let receipt: Value =
        serde_json::from_slice(&out.stdout).expect("the actual API refusal remains durable");
    assert_eq!(receipt["status"], "unknown");
    assert_eq!(
        receipt["runtime_id"], THREAD,
        "the already copied native start is retained"
    );
    assert!(
        receipt["launcher_error"]
            .as_str()
            .unwrap()
            .contains("fixture observation write refused")
    );
}

#[tokio::test]
async fn a_broken_first_executable_cannot_fall_through_to_another_harness() {
    let mut world = World::new().await;
    let first = world.root.path().join("bin/codex");
    let fallback = world.root.path().join("fallback-bin");
    std::fs::create_dir(&fallback).unwrap();
    boss_testing::copy_exec(&first, &fallback.join("codex"));
    world.fallback = Some(fallback);
    boss_testing::write_exec(&first, "#!/a-missing-fixture-interpreter\n");
    let out = world.launch().await;
    assert!(
        !out.status.success(),
        "a different PATH executable must not replace the selected harness: {out:?}"
    );
    assert!(
        !world.root.path().join("calls").exists(),
        "the second fixture must never execute"
    );
    let receipt: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(receipt["source"], "os.process");
    assert_eq!(receipt["status"], "failed");
}

#[tokio::test]
async fn duplicate_or_unverified_admission_never_starts_a_process() {
    for mode in ["duplicate", "wrong-id", "unverified"] {
        let world = World::with_admission(mode).await;
        let out = world.launch().await;
        assert!(!out.status.success(), "{mode}");
        assert!(
            !world.root.path().join("calls").exists(),
            "{mode}: no executor"
        );
        let (rows, total) = world
            .jobs
            .list_jobs(
                &boss_jobs::JobFilter {
                    kind: Some("agent-execution".into()),
                    ..Default::default()
                },
                10,
                0,
            )
            .await
            .unwrap();
        assert_eq!(total, 1, "{mode}: the actual server admitted a reservation");
        assert_eq!(rows[0].metadata["status"], "reserved");
        assert!(
            !world.launch().await.status.success(),
            "{mode}: uncertainty cannot authorize a retry"
        );
        assert_eq!(
            world
                .jobs
                .recorded_events()
                .iter()
                .filter(|e| e.kind == boss_jobs::events::JOB_CREATED)
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn invalid_run_controls_and_a_lost_source_claim_refuse_before_reservation() {
    for (key, value) in [
        ("profile", json!("builder")),
        ("model", json!("opus-5[1m]")),
        ("effort", json!("ultra")),
        ("brief", json!(null)),
        ("step", json!("another-step")),
    ] {
        let world = World::new().await;
        let mut run = world.jobs.get_job(&world.run).await.unwrap().unwrap();
        run.metadata[key] = value;
        world.jobs.update_job(&run).await.unwrap();
        let out = world.launch().await;
        assert!(!out.status.success(), "{key}");
        assert!(!world.root.path().join("calls").exists());
        assert!(
            world
                .jobs
                .recorded_events()
                .iter()
                .all(|e| e.kind != boss_jobs::events::JOB_CREATED)
        );
    }
}

#[tokio::test]
async fn preparation_and_spawn_refusals_record_the_attempt_without_runtime_invention() {
    for mode in ["existing-directory", "spawn"] {
        let world = World::new().await;
        if mode == "existing-directory" {
            std::fs::create_dir(world.root.path().join("attempt")).unwrap();
            std::fs::write(
                world.root.path().join("attempt/receipt.json"),
                "keep this file",
            )
            .unwrap();
        } else {
            boss_testing::write_exec(
                &world.root.path().join("bin/codex"),
                "#!/a-missing-fixture-interpreter\n",
            );
        }
        let out = world.launch().await;
        assert!(!out.status.success(), "{mode}");
        let receipt: Value = serde_json::from_slice(&out.stdout)
            .expect("a refused preparation or spawn retains its cause");
        assert!(receipt["runtime_id"].is_null());
        assert!(receipt["observed_model"].is_null());
        assert!(
            receipt["launcher_error"]
                .as_str()
                .is_some_and(|s| !s.is_empty())
        );
        let id = JobId::from_uuid(
            uuid::Uuid::parse_str(receipt["execution"].as_str().unwrap()).unwrap(),
        );
        let held = world.jobs.get_job(&id).await.unwrap().unwrap();
        assert_eq!(held.status, JobStatus::Closed);
        assert_eq!(held.metadata["observation"], receipt);
        assert!(!world.root.path().join("calls").exists());
        if mode == "existing-directory" {
            assert_eq!(
                std::fs::read_to_string(world.root.path().join("attempt/receipt.json")).unwrap(),
                "keep this file"
            );
        }
    }
}

#[tokio::test]
async fn prompt_input_and_native_output_are_drained_concurrently() {
    let world = World::new().await;
    let mut run = world.jobs.get_job(&world.run).await.unwrap().unwrap();
    run.metadata["brief"] = json!("a".repeat(256 * 1024));
    world.jobs.update_job(&run).await.unwrap();
    boss_testing::write_exec(
        &world.root.path().join("bin/codex"),
        &format!(
            r#"#!/bin/sh
set -eu
printf '%s\n' '{{"type":"thread.started","thread_id":"{THREAD}"}}'
printf '%s\n' '{{"type":"turn.started"}}'
python3 -c 'import json; print(json.dumps({{"type":"item.completed","item":{{"type":"agent_message","text":"a"*262144}}}}))'
cat > "$FIXTURE_CAPTURE/prompt"
printf '%s\n' '{{"type":"turn.completed"}}'
"#
        ),
    );
    let out = tokio::time::timeout(std::time::Duration::from_secs(3), world.launch())
        .await
        .expect("a native process writing before reading the prompt must not deadlock");
    assert!(out.status.success(), "{out:?}");
    assert!(
        std::fs::read(world.root.path().join("attempt/native.jsonl"))
            .unwrap()
            .len()
            > 256 * 1024
    );
    assert!(
        std::fs::read(world.root.path().join("prompt"))
            .unwrap()
            .len()
            > 256 * 1024
    );
}

#[tokio::test]
async fn a_native_success_cannot_hide_a_refused_prompt_input() {
    let world = World::new().await;
    let mut run = world.jobs.get_job(&world.run).await.unwrap().unwrap();
    run.metadata["brief"] = json!("a".repeat(256 * 1024));
    world.jobs.update_job(&run).await.unwrap();
    boss_testing::write_exec(
        &world.root.path().join("bin/codex"),
        &format!(
            r#"#!/bin/sh
exec 0<&-
printf '%s\n' '{{"type":"thread.started","thread_id":"{THREAD}"}}'
printf '%s\n' '{{"type":"turn.started"}}'
printf '%s\n' '{{"type":"turn.completed"}}'
"#
        ),
    );
    let out = world.launch().await;
    assert!(!out.status.success());
    let receipt: Value =
        serde_json::from_slice(&out.stdout).expect("prompt refusal leaves a durable observation");
    assert_eq!(receipt["status"], "unknown");
    assert!(
        receipt["launcher_error"]
            .as_str()
            .is_some_and(|s| s.contains("complete brief"))
    );
    let id =
        JobId::from_uuid(uuid::Uuid::parse_str(receipt["execution"].as_str().unwrap()).unwrap());
    assert_eq!(
        world.jobs.get_job(&id).await.unwrap().unwrap().metadata["observation"],
        receipt
    );
}

#[tokio::test]
async fn an_actual_native_thread_is_recorded_without_inventing_its_model_or_heartbeat() {
    let world = World::new().await;
    let out = world.launch().await;
    assert!(
        out.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let captured = std::fs::read_to_string(world.root.path().join("env")).unwrap();
    assert!(captured.starts_with(&format!("{ACTOR}\n{}\n", world.run)));
    let worktree = Path::new(captured.lines().nth(2).unwrap());
    assert_ne!(worktree, world.root.path());
    assert!(worktree.join("README").is_file());
    let prompt = std::fs::read_to_string(world.root.path().join("prompt")).unwrap();
    assert!(prompt.contains("The exact persisted dispatch brief."));
    assert!(prompt.contains(&format!("agent-run {}", world.run)));
    let argv = std::fs::read_to_string(world.root.path().join("argv")).unwrap();
    assert!(argv.contains("exec\n--json\n"));
    assert!(argv.contains("gpt-6.1-sol"));
    assert!(!argv.contains("dangerously-bypass"));
    let job: Value = serde_json::from_slice(&out.stdout).expect("one execution receipt");
    assert_eq!(job["source"], "codex.exec.json");
    assert_eq!(job["runtime_id"], THREAD);
    assert_eq!(job["agent_run"], world.run.to_string());
    assert_eq!(job["status"], "completed");
    assert!(job.get("observed_model").is_some_and(Value::is_null));
    assert!(job.get("heartbeat_at").is_none());
    assert!(job.get("spend_usd").is_none());
    let id = JobId::from_uuid(uuid::Uuid::parse_str(job["execution"].as_str().unwrap()).unwrap());
    let held = world.jobs.get_job(&id).await.unwrap().unwrap();
    assert_eq!(
        held.subject.id,
        world.run.to_string(),
        "the reservation subject and registered-run link identify the same run"
    );
    assert_eq!(held.metadata["requested"]["model"], "gpt-6.1-sol");
    assert_eq!(held.metadata["requested"]["sandbox"], "read-only");
    assert_eq!(
        held.metadata["observation"], job,
        "the durable record must retain the copied receipt"
    );
    assert_eq!(
        held.status,
        JobStatus::Closed,
        "the observation packet reaches its own terminal"
    );
    assert!(
        world
            .jobs
            .list_steps(&id)
            .await
            .unwrap()
            .iter()
            .any(|s| s.spec_slug.as_deref() == Some("observing")
                && s.status == StepStatus::Completed)
    );
    let replay = world.launch().await;
    assert!(!replay.status.success(), "a replay cannot launch again");
    assert_eq!(
        world
            .jobs
            .recorded_events()
            .iter()
            .filter(|e| e.kind == boss_jobs::events::JOB_CREATED)
            .count(),
        1,
        "one reservation admission"
    );
}

#[tokio::test]
async fn two_concurrent_launchers_cannot_execute_one_registered_run_twice() {
    let world = World::new().await;
    let (first, second) = tokio::join!(world.launch(), world.launch());
    assert_ne!(
        first.status.success(),
        second.status.success(),
        "one launch wins: {first:?} {second:?}"
    );
    assert_eq!(
        std::fs::read_to_string(world.root.path().join("calls"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    assert_eq!(
        world
            .jobs
            .recorded_events()
            .iter()
            .filter(|e| e.kind == boss_jobs::events::JOB_CREATED)
            .count(),
        1
    );
}

#[tokio::test]
async fn a_run_owned_by_someone_else_cannot_be_launched_under_the_callers_identity() {
    let world = World::new().await;
    let mut run = world.jobs.get_job(&world.run).await.unwrap().unwrap();
    run.metadata["agent"] = json!("another-agent");
    world.jobs.update_job(&run).await.unwrap();
    let out = world.launch().await;
    assert!(!out.status.success());
    assert!(!world.root.path().join("calls").exists());
    assert!(
        world
            .jobs
            .recorded_events()
            .iter()
            .all(|e| e.kind != boss_jobs::events::JOB_CREATED),
        "refuse before reservation"
    );
}

#[tokio::test]
async fn silent_malformed_failed_or_contradictory_native_output_never_becomes_success() {
    for (name, replace, exit) in [
        ("silence", "", 0),
        ("malformed", "not JSON", 0),
        (
            "wrong-thread",
            "{\"type\":\"thread.started\",\"thread_id\":\"\"}",
            0,
        ),
        ("unstarted", "{\"type\":\"turn.completed\"}", 0),
        (
            "native-error",
            "{\"type\":\"error\",\"message\":\"fixture refusal\"}",
            0,
        ),
        (
            "exit-failed",
            "{\"type\":\"thread.started\",\"thread_id\":\"bbbbbbbb-1111-4111-8111-111111111111\"}",
            7,
        ),
    ] {
        let world = World::new().await;
        let path = world.root.path().join("bin/codex");
        boss_testing::write_exec(
            &path,
            &format!(
                "#!/bin/sh\ncat > \"$FIXTURE_CAPTURE/prompt\"\nprintf '%s\\n' '{replace}'\nexit {exit}\n"
            ),
        );
        let out = world.launch().await;
        assert!(
            !out.status.success(),
            "{name} cannot attest a completed execution"
        );
        let receipt: Value = serde_json::from_slice(&out.stdout)
            .expect("a refused native turn still leaves its observation");
        assert_ne!(receipt["status"], "completed", "{name}");
        assert!(receipt["observed_model"].is_null());
        let id = JobId::from_uuid(
            uuid::Uuid::parse_str(receipt["execution"].as_str().unwrap()).unwrap(),
        );
        let held = world.jobs.get_job(&id).await.unwrap().unwrap();
        assert_eq!(
            held.status,
            JobStatus::Closed,
            "{name}: the observation terminal is recorded"
        );
        assert_eq!(held.metadata["observation"], receipt, "{name}");
        if name == "native-error" {
            assert_eq!(
                receipt["native_events"][0]["message"], "fixture refusal",
                "the failure cause must survive in the durable receipt"
            );
        }
        assert!(world.root.path().join("attempt/native.jsonl").is_file());
        assert!(
            !world.launch().await.status.success(),
            "{name}: a failed or uncertain native attempt is not retried"
        );
    }
}

#[tokio::test]
async fn a_native_thread_start_is_recorded_while_the_executor_is_still_running() {
    let world = World::new().await;
    let path = world.root.path().join("bin/codex");
    let script = std::fs::read_to_string(&path).unwrap().replace(
        "printf '%s\\n' '{\"type\":\"turn.completed\"",
        "sleep 2\nprintf '%s\\n' '{\"type\":\"turn.completed\"",
    );
    boss_testing::write_exec(&path, &script);
    let observe = async {
        for _ in 0..50 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            let (rows, total) = world
                .jobs
                .list_jobs(
                    &boss_jobs::JobFilter {
                        kind: Some("agent-execution".into()),
                        ..Default::default()
                    },
                    10,
                    0,
                )
                .await
                .unwrap();
            if total == 1 && rows[0].metadata["status"] == "accepted" {
                let r = rows[0].metadata["observation"].clone();
                assert_eq!(r["runtime_id"], THREAD);
                assert_eq!(r["source"], "codex.exec.json");
                assert!(r["observed_model"].is_null());
                assert!(r.get("heartbeat_at").is_none());
                return true;
            }
        }
        false
    };
    let (out, observed) = tokio::join!(world.launch(), observe);
    assert!(out.status.success(), "{out:?}");
    assert!(
        observed,
        "a parent must read a native start before the final return"
    );
}
