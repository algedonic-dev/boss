//! `boss dispatch <packet> --step <slug> --launch --launch-dir <DIR>` —
//! start the harness the step's model names, and witness how it ends
//! (backlog 2f7b8c00, narrowed to Codex 2026-10-07).
//!
//! WHAT WAS MISSING. `boss dispatch` printed a prompt; whoever read it
//! ran it on their own harness. A step whose block named `gpt-6.1-sol`
//! therefore ran on the coordinator's Opus unless a Codex session
//! dispatched it to itself — 86 of the newest 200 runs that day — and
//! each of those opened with its Building nominated to the wrong actor,
//! recorded the DECLARED model as the one that ran, and (85 of 86)
//! carried no token count.
//!
//! WHAT THIS DOES, in order, and what each step leaves on the record:
//!
//! 1. [`plan_at`], BEFORE the claim: the agents registry and the harness
//!    roster name the harness ([`crate::harness::for_model`]); a harness
//!    its coordinator starts, a profile it has no sandbox for, a missing
//!    executable and an existing `--launch-dir` each refuse here, while
//!    nothing is claimed or filed.
//! 2. Dispatch claims the step FOR the harness's actor (`?claimed_for=`,
//!    review b149c579 N2 — so the claim door's concurrency cap and budget
//!    reading judge that actor's agents row, not the coordinator's), files
//!    the run with `agent` = the harness's actor and nominates the run's
//!    Building to it (see `dispatch::nominate_building`).
//! 3. [`run_at`]: a new 0700 directory, a locked detached worktree at
//!    main as just fetched from the forge (`base_sha` on the receipt;
//!    a fetch that fails ends the run — [`fetched_main`]), the
//!    prompt on stdin, the process in a process group of its own, and an
//!    environment that is cleared and then holds the harness file's pass
//!    list, `BOSS_ACTOR` = the harness's actor, `BOSS_AGENT_RUN`, and —
//!    for a harness whose file says `home = "own"` — a HOME that is an
//!    empty directory made for this launch, with the harness's own home
//!    variable pointing at its real login directory. See WHAT THE
//!    PROCESS CAN STILL REACH below: this narrows, it does not isolate.
//!    The receipt is written to the run as `launch` BEFORE the spawn
//!    (`status = starting`): a launcher that dies leaves a run that says
//!    it was being started, and the silence clock ends it.
//! 4. At exit: the transcript is found by the runtime id the process
//!    printed, and the model it names is recorded as `observed_model` —
//!    beside `declared_model`, never from it. Unreadable is `null` with
//!    the reason.
//! 5. The verdict is the CHILD's exit (review f7f0b689, B2): its stdout
//!    is drained for a short grace after it, never until EOF, because
//!    anything it started may hold the pipe open long after it finished.
//!    Then the whole process group is stopped — TERM, a grace, KILL —
//!    at the bound AND after an ordinary exit (B3), so nothing the
//!    process started goes on acting as the run once its ending is
//!    recorded. A clean worktree is removed after a finished turn; after
//!    a death it stays, locked, in the launch directory for the rescue.
//! 6. A process that could not start, exited non-zero, was stopped at the
//!    bound, or never printed a completed turn ends the run `died`, with
//!    `launch_died` saying which — the two writes the hourly clock makes,
//!    made now, by the witness.
//!
//! EVERY WRITE HERE IS SIGNED BY THE LAUNCHING ACTOR. The launcher is
//! not the harness and never signs as it: what the launched process does
//! under its own `BOSS_ACTOR` (its claim of Building, its `--started`)
//! is its own.
//!
//! WHAT THE PROCESS CAN STILL REACH (review f7f0b689, B4 — earlier text
//! here claimed a separation this verb does not make). The launched
//! process runs as the SAME UNIX ACCOUNT as the session that launched
//! it. Clearing the environment and giving it a HOME of its own means
//! the launching session's variables are not handed to it and that the
//! account's actor file, the other harness's login directory and the git
//! credential helper are not under its HOME. It does not make them
//! unreadable: any file this account can read — the real home by its
//! path, the machine-token mount, the other harness's login — is still
//! reachable by a process that goes looking, and `BOSS_ACTOR` is a
//! default it is told to keep, not a bound: the actor header is asserted
//! and nothing authenticates it. Only a separate uid (or a container)
//! closes that, and this verb has neither. Design 21062f95 chose the
//! container (2026-10-10): [`crate::launch_job`] is that Job's contract,
//! and nothing here creates it yet.
//!
//! STOPPING THE VERB IS AN ENDING TOO (review e9514316, B5). The process
//! leads a group of its own, so a Ctrl-C, a tool's timeout or a session
//! ending signals the VERB's group and no longer reaches it — and the
//! bound, the only thing that bounds a launched run, lives in the verb.
//! Three things answer that:
//! - SIGINT, SIGTERM and SIGHUP to the verb are handled as an ending:
//!   the group is stopped exactly as at the bound, the run is ended
//!   `died` with `launch_died.why` naming the signal, and the verb exits
//!   non-zero.
//! - A verb that is KILLED handles nothing, so the kernel tells the
//!   process: it is started with a parent-death signal (SIGTERM). That
//!   is SENT to the ONE process the verb spawned, and it is a request:
//!   a process that ignores TERM outlives a killed verb, and so does
//!   everything it started (review b149c579, N20 — this text and the
//!   verb's help used to say a killed verb takes that process with it).
//!   It does not reach a descendant (the setting is not inherited
//!   across fork), a process that cleared it, or — not the case for any
//!   shipped harness — a setuid executable, for which the kernel clears
//!   it. A killed verb also writes nothing: the run keeps its `launch`
//!   receipt at `starting`, beside the `launch_process` that says the
//!   process was spawned, until the silence clock ends it.
//! - So the pid and process group are ON THE PACKET as soon as the
//!   process exists (`launch_process.pid`, `launch_process.pgid`, under
//!   a key of their own so that write arriving late can never replace
//!   the ending — review 0facb916, B1) and
//!   in `<launch-dir>/pid`: what a killed verb leaves can be found and
//!   stopped by hand — `kill -TERM -<pgid>`, then `kill -KILL -<pgid>`
//!   for whatever is left, which is what the verb itself does.
//!
//! WHAT THE VERB'S OWN ENDINGS DO NOT WAIT FOR (review b149c579, N18 and
//! N19). The bound is counted from the spawn and the receipt that
//! carries the pid is written BESIDE the wait, never before it: a system
//! of record that is dark or stalled delays that receipt, not the bound
//! and not the verb's hearing of its own stop — and it is not waited for
//! after the stop either, because it names only the process, on its own
//! key. The lines the verb says, and the retry line of the jobs-API
//! client every write here goes through (review 0facb916, N1), are
//! written without panicking, so a verb whose stderr has gone still
//! stops the group and writes `died`; and whatever unwinds the launch —
//! a panic, a dropped future — KILLs the group on the way out
//! ([`StopOnDrop`]). The writes made AFTER the group is stopped (the
//! ending, the `died` terminal) are still awaited with the client's own
//! patience: a stalled record then holds the VERB, not the process.
//!
//! NOT DONE HERE, said so the record is not read as more than it is: the
//! budget is not enforced while the process runs (only the time bound
//! is); the stop reaches the process group, so a descendant that put
//! itself in ANOTHER group or session (it called setsid or setpgid)
//! escapes it and keeps the environment it was born with; and `launch`
//! is the launcher's observation — the run's spend is what `--report`
//! meters. `boss launch` (launch.rs) is the older, separate launcher: it
//! refuses a run that belongs to another actor, inherits the whole
//! environment and reads no harness file. It is NOT folded into the
//! roster by this car (review N11); the two should become one.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// The run packet key the launcher's receipt lives under.
pub(crate) const RECEIPT_KEY: &str = "launch";

/// The run packet key the process's pid and group are written under the
/// moment it exists — its own key, never [`RECEIPT_KEY`], so a write that
/// the record applies late cannot replace the ending (review 0facb916,
/// B1). It holds only facts that stay true once the process is gone.
pub(crate) const PROCESS_KEY: &str = "launch_process";

/// The Building key a launcher-witnessed death is explained under.
pub(crate) const DIED_KEY: &str = "launch_died";

/// What `--launch` was asked for.
#[derive(Debug, Clone)]
pub(crate) struct Request {
    /// A directory that does not exist yet: the worktree and every file
    /// of this attempt.
    pub dir: PathBuf,
    /// How long the process may run before it is stopped.
    pub bound: std::time::Duration,
    /// Where the harness's executable is looked for: the launching
    /// process's PATH, read once at the CLI boundary.
    pub search_path: Option<OsString>,
    /// How long each wait AFTER the verdict may take: draining stdout
    /// once the process has exited, and between TERM and KILL.
    pub grace: std::time::Duration,
    /// Take SIGINT, SIGTERM and SIGHUP as an ending. Always true for the
    /// verb. A flag because listening is for the life of the PROCESS —
    /// once heard here, those signals no longer end it by default — and
    /// a test binary that launches in-process must stay stoppable.
    pub heed_stops: bool,
}

/// [`Request::grace`] for the verb.
pub(crate) const GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// What [`plan_at`] resolved, before anything was claimed.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Plan {
    pub harness: crate::harness::Harness,
    pub sandbox: String,
    /// The harness's argv[0], resolved once to an absolute path — a
    /// later PATH entry can then never be what runs.
    pub executable: PathBuf,
    pub cluster: Option<crate::launch_cluster::Bindings>,
    pub snapshot: Option<crate::launch_controller::Snapshot>,
    pub bundle: Vec<u8>,
    pub members: Vec<u8>,
}

/// The first executable regular file named `name` on `path`.
pub(crate) fn find_executable(name: &str, path: &OsString) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    std::env::split_paths(path)
        .map(|dir| dir.join(name))
        .find(|candidate| {
            std::fs::metadata(candidate)
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
        .and_then(|found| std::fs::canonicalize(found).ok())
}

/// The complete agents registry, or the refusal: a page that is not the
/// whole table cannot say that exactly one row runs a model.
pub(crate) async fn agents_at(
    http: &boss_core::machine_token::Client,
    base: &str,
    actor: &str,
) -> Result<Vec<Value>> {
    let body = crate::sor_client::api_at_signed(
        http,
        base,
        reqwest::Method::GET,
        "/api/agents",
        None,
        crate::identity::Signature::As(actor.to_string()),
    )
    .await?
    .context("the agents registry returned no body")?;
    let total = body
        .get("total")
        .and_then(Value::as_u64)
        .context("the agents registry returned no total")?;
    let rows = crate::train::rows(Some(body))?;
    if rows.len() as u64 != total {
        bail!(
            "the agents registry answered {} of {total} rows — which one runs a model cannot \
             be read from part of it",
            rows.len()
        );
    }
    Ok(rows)
}

/// Resolve the launch before the claim. Pure but for the registry rows
/// and the filesystem looks it is handed.
pub(crate) fn plan(
    harnesses: &[crate::harness::Harness],
    agents: &[Value],
    settings: &crate::dispatch::Settings,
    request: &Request,
    search_path: Option<&OsString>,
    // The registered id of the actor running the verb, when the registry
    // names exactly one.
    dispatcher: Option<&str>,
) -> std::result::Result<Plan, String> {
    let harness = crate::harness::for_model(harnesses, agents, &settings.model)?;
    // A PROCESS HARNESS IS ANOTHER ACTOR (review f7f0b689, B1). A file
    // saying `actor = "<the coordinator>"` on a process harness would
    // have every launched process sign as the session that launched it —
    // the one identity a launch exists to keep apart. The file cannot
    // know who will dispatch, so it is judged here, before the claim.
    if harness.launch != crate::harness::Launch::HostTool
        && dispatcher == Some(harness.actor.as_str())
    {
        return Err(format!(
            "harness {} is a process that signs as {}, the actor running this verb — a \
             launched process is a different actor from the one that launches it",
            harness.id, harness.actor
        ));
    }
    if harness.launch == crate::harness::Launch::HostTool {
        return Err(format!(
            "`{}` is run by harness {} ({}), which its coordinator's own session starts — \
             there is no process for --launch to spawn. Dispatch without --launch and hand \
             the printed prompt to that harness",
            settings.model, harness.id, harness.actor
        ));
    }
    let sandbox = harness.sandbox_for(&settings.profile)?.to_string();
    if harness.launch == crate::harness::Launch::Cluster && !request.dir.is_absolute() {
        return Err("cluster --launch-dir is an absolute private durable locator".into());
    }
    let name = harness.argv.first().ok_or("the harness declares no argv")?;
    let executable = if harness.launch == crate::harness::Launch::Cluster {
        PathBuf::from(name)
    } else {
        search_path
            .and_then(|p| find_executable(name, p))
            .ok_or_else(|| {
                format!(
                    "no executable `{name}` on PATH to start harness {} with",
                    harness.id
                )
            })?
    };
    if request.dir.exists() {
        return Err(format!(
            "--launch-dir {} already exists — an attempt's directory is new, so its files are \
             this attempt's and nothing else's",
            request.dir.display()
        ));
    }
    Ok(Plan {
        harness: harness.clone(),
        sandbox,
        executable,
        cluster: None,
        snapshot: None,
        bundle: Vec::new(),
        members: Vec::new(),
    })
}

/// [`plan`] over the live registry and the tree's roster.
pub(crate) async fn plan_at(
    http: &boss_core::machine_token::Client,
    base: &str,
    repo: &Path,
    actor: &str,
    settings: &crate::dispatch::Settings,
    request: &Request,
) -> Result<Plan> {
    let harnesses = crate::harness::read_all(repo)?;
    let agents = agents_at(http, base, actor).await?;
    let dispatcher = crate::dispatch::resolve_agent(&agents, actor);
    let mut selected = plan(
        &harnesses,
        &agents,
        settings,
        request,
        request.search_path.as_ref(),
        dispatcher.as_deref(),
    )
    .map_err(|e| anyhow::anyhow!("boss dispatch --launch: {e}"))?;
    if selected.harness.launch == crate::harness::Launch::Cluster {
        let binding =
            crate::launch_cluster::resolve(http, base, repo, actor, &selected.harness).await?;
        let (snapshot, bundle, members) = crate::launch_cluster::snapshot(repo).await?;
        selected.executable = PathBuf::from(&binding.binary);
        selected.cluster = Some(binding);
        selected.snapshot = Some(snapshot);
        selected.bundle = bundle;
        selected.members = members;
    }
    Ok(selected)
}

/// THE ROSTER A LAUNCH READS IS MAIN'S (review f7f0b689, N6). The
/// harness files come from the checkout the verb stands in, so a
/// coordinator standing in a branch worktree would launch whatever THAT
/// branch's file says — an argv, a sandbox, an actor no review has seen.
/// Refused unless [`crate::harness::DIR`] in that checkout is exactly
/// `origin/main`'s, as this clone last fetched it: no uncommitted,
/// untracked or IGNORED file there (the roster reader reads every
/// `*.toml` in the directory whether git tracks it or not, and the
/// tree's `.gitignore` hides `.env.*` from a plain status — review
/// e9514316, N14), and no difference from the ref.
///
/// WHAT IT IS: a guard against standing in the wrong checkout. It is
/// not a seal. It asks git, so an edit hidden by assume-unchanged or
/// skip-worktree, a local branch named `origin/main`, or a ref moved by
/// hand answers Ok; whoever can do those can replace the executable on
/// PATH. The review of a harness file is the hold's, not this check's.
///
/// WHY NOT "the tree the binary was built from": that path
/// (`trust_boundary::BUILT_FROM`) exists only on a pod build, is itself a
/// checkout that can sit on a branch, and would make the roster a second
/// thing to keep equal to the one the verb reads everything else from.
/// Comparing the directory against the converged ref asks the question
/// that matters — has this roster been through the train — of the files
/// that will actually be used. It reads the local ref and does not
/// fetch: a clone that has not fetched compares against an older main,
/// which is still a main.
pub(crate) fn roster_is_mains(repo: &Path) -> std::result::Result<(), String> {
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .map_err(|e| format!("could not run git: {e}"))
    };
    for dir in [crate::harness::DIR, crate::launch_cluster::ROWS] {
        let status = git(&["status", "--porcelain", "--ignored", "--", dir])?;
        if !status.status.success() {
            return Err(format!(
                "whether {dir} is origin/main's could not be read (git status: {}) — no evidence \
             is not a pass, and --launch reads the roster from this checkout",
                String::from_utf8_lossy(&status.stderr).trim()
            ));
        }
        let dirty = String::from_utf8_lossy(&status.stdout).trim().to_string();
        if !dirty.is_empty() {
            return Err(format!(
                "{dir} in {} holds uncommitted, untracked or ignored files ({}) — --launch starts \
             what the roster says, so it reads only a roster that is origin/main's",
                repo.display(),
                dirty.lines().collect::<Vec<_>>().join("; ")
            ));
        }
        let diff = git(&["diff", "--quiet", "origin/main", "--", dir])?;
        match diff.status.code() {
            Some(0) => {}
            Some(1) => {
                return Err(format!(
                    "{dir} in {} differs from origin/main — --launch starts what the roster says, so \
             it reads only a roster that has landed. Run it from a checkout of main",
                    repo.display()
                ));
            }
            _ => {
                return Err(format!(
                    "whether {dir} is origin/main's could not be read (git diff: {}) — no evidence is \
             not a pass. If this clone has no origin/main yet, `git fetch origin` gives it one",
                    String::from_utf8_lossy(&diff.stderr).trim()
                ));
            }
        }
    }
    Ok(())
}

/// What the launcher appends to the dispatched prompt. It states what
/// the launcher did and where a launched run DIFFERS from what the rules
/// document above it says (review f7f0b689, N10): those rules are
/// written for a worker that holds its own step.
pub(crate) fn launcher_section(
    plan: &Plan,
    dispatcher: &str,
    run_id: &str,
    last_message: &Path,
) -> String {
    format!(
        "\n== THE LAUNCHER ==\nYou were started by `boss dispatch --launch` as harness `{id}`. \
         Your working directory is a git worktree made for this run alone; it is removed when \
         you finish cleanly, so leave nothing in it that you need. Your environment carries \
         BOSS_ACTOR={actor} and BOSS_AGENT_RUN={run_id}: sign as that actor and no other, and \
         set neither.\n\
         WHERE THIS RUN DIFFERS FROM THE RULES ABOVE. The step you were dispatched for is \
         held by {actor}, claimed FOR you by {dispatcher}, who launched you: it counts against \
         your own run cap and budget, but {dispatcher} records its outcome. Do not claim it, \
         complete it, or record a verdict on it. Write your verdict and your findings as your \
         FINAL message: it is kept at {last}, and {dispatcher} records it on the step and as \
         this run's report.\n\
         Your own run's building step IS yours, and the worker receipt above takes it. If your \
         sandbox does not let a command reach the jobs API, do not work around it: say so in \
         your final message and go on with the work. Where the rules name a tool of another \
         harness, use your own.\n",
        id = plan.harness.id,
        actor = plan.harness.actor,
        last = last_message.display()
    )
}

/// What the process's stdout said, by the harness file's two events.
#[derive(Debug, Default)]
struct Native {
    runtime: Option<String>,
    completed: bool,
    lines: u64,
}

impl Native {
    fn read(&mut self, harness: &crate::harness::Harness, line: &[u8]) {
        self.lines += 1;
        let Ok(v) = serde_json::from_slice::<Value>(line) else {
            return;
        };
        let kind = v.get("type").and_then(Value::as_str);
        if let Some(started) = &harness.started
            && kind == Some(started.event.as_str())
            && self.runtime.is_none()
        {
            self.runtime = started
                .key
                .as_deref()
                .and_then(|k| v.get(k))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
        }
        if harness
            .completed
            .as_ref()
            .is_some_and(|c| kind == Some(c.event.as_str()))
        {
            self.completed = true;
        }
    }
}

/// How the process ended, as the launcher saw it.
#[derive(Debug, Default)]
struct Ending {
    spawn_error: Option<String>,
    exit_code: Option<i32>,
    timed_out: bool,
    input_error: Option<String>,
    native: Native,
    /// Stdout was still open when the grace ended and its reader was
    /// stopped: the kept file may be short of what the process wrote.
    capture_incomplete: bool,
    /// The signal that stopped the VERB while the process ran.
    stopped_by: Option<&'static str>,
}

/// The three signals that stop the verb, as one stream of names.
pub(crate) struct Stops {
    interrupt: tokio::signal::unix::Signal,
    terminate: tokio::signal::unix::Signal,
    hangup: tokio::signal::unix::Signal,
}

impl Stops {
    /// From here on these signals no longer end the process by default:
    /// they are an ending this verb carries out.
    pub(crate) fn listen() -> std::io::Result<Stops> {
        use tokio::signal::unix::{SignalKind, signal};
        Ok(Stops {
            interrupt: signal(SignalKind::interrupt())?,
            terminate: signal(SignalKind::terminate())?,
            hangup: signal(SignalKind::hangup())?,
        })
    }

    pub(crate) async fn next(&mut self) -> &'static str {
        tokio::select! {
            _ = self.interrupt.recv() => "SIGINT",
            _ = self.terminate.recv() => "SIGTERM",
            _ = self.hangup.recv() => "SIGHUP",
        }
    }
}

impl Ending {
    /// Why this ending is a death, or `None` for a process that exited 0
    /// after printing a completed turn. Silence is refused like failure:
    /// an exit 0 that never said it finished a turn is not a success.
    fn died(&self) -> Option<String> {
        if let Some(e) = &self.spawn_error {
            return Some(format!("the process could not be started: {e}"));
        }
        if let Some(signal) = self.stopped_by {
            return Some(format!(
                "the launcher was stopped by {signal} while the process ran, and stopped the \
                 process with it"
            ));
        }
        if self.timed_out {
            return Some("the process was still running at its time bound and was stopped".into());
        }
        match self.exit_code {
            Some(0) if self.native.completed => None,
            Some(0) => Some(
                "the process exited 0 without printing a completed turn — it has not shown \
                 that it did anything"
                    .into(),
            ),
            Some(code) => Some(format!("the process exited {code}")),
            None => Some("the process was ended by a signal".into()),
        }
    }
}

/// The model the harness's own transcript names for this runtime, the
/// counts it holds, and where it is — or why none of that could be read.
/// Never the declared model: this function is not handed it.
fn observe(
    harness: &crate::harness::Harness,
    env: &(dyn Fn(&str) -> Option<OsString> + Sync),
    runtime: Option<&str>,
) -> (
    Option<PathBuf>,
    std::result::Result<(String, Value), String>,
) {
    let Some(runtime) = runtime else {
        return (
            None,
            Err("the process printed no runtime id, so its transcript cannot be named".into()),
        );
    };
    let Some(home) = harness.home(env) else {
        return (
            None,
            Err(format!(
                "neither {} nor HOME is set, so the harness's transcripts cannot be found",
                harness.transcript.home_env
            )),
        );
    };
    let path = match harness.transcript_of(&home, runtime) {
        Ok(p) => p,
        Err(why) => {
            return (
                None,
                // The home is NAMED, never printed: its path is the value
                // of a variable of the launching session (review
                // e9514316, N12), and this sentence goes onto the packet.
                Err(format!(
                    "no transcript for runtime {runtime}: {why} under ${} (else $HOME/{})",
                    harness.transcript.home_env, harness.transcript.home_default
                )),
            );
        }
    };
    // THE FILE IS NAMED BY ITS VARIABLE TOO (review b149c579, N21): a
    // transcript that was found and could not be read, or names no
    // model, was explained by its whole path — the home's, as above —
    // and that sentence goes onto the packet twice (`observed_model_unread`
    // and `model`). The path itself is `launch.transcript`, the one key
    // the report reads the file by.
    let named = format!(
        "{} under ${} (else $HOME/{})",
        path.strip_prefix(&home)
            .map(|within| within.display().to_string())
            .unwrap_or_else(|_| "the transcript".into()),
        harness.transcript.home_env,
        harness.transcript.home_default
    );
    let read = std::fs::read_to_string(&path)
        .map_err(|e| format!("could not read {named}: {e}"))
        .and_then(|text| {
            let model = crate::transcript_usage::read_models(&text)
                .recorded()
                .ok_or_else(|| format!("{named} names no model on any billed turn"))?;
            let seen = match crate::transcript_usage::sum_usage(&text) {
                Some(u) => json!({
                    "input": u.input, "cache_read": u.cache_read,
                    "cache_write": u.cache_write, "output": u.output, "turns": u.turns,
                }),
                None => Value::Null,
            };
            Ok((model, seen))
        });
    (Some(path), read)
}

/// The sentence the receipt and the terminal carry about the model. One
/// copy, in core (backlog ed8e5c85).
pub(crate) use boss_jobs::car_release::model_line;

/// The record a launcher writes about the model it started: `model` is
/// EXACTLY the name it handed the harness (`null` when it handed none),
/// `harness` the roster file it started when it started one by name, and
/// `via` the control the name went through. Nothing here is inferred:
/// the reader judges whether the name pins a model ([`reviewer_model`]).
///
/// [`reviewer_model`]: boss_jobs::car_release::reviewer_model
pub(crate) fn launched_model(
    model: Option<&str>,
    harness: Option<&str>,
    via: &str,
    launched_by: &str,
) -> Value {
    let mut record = json!({ "model": model, "via": via, "launched_by": launched_by });
    if let Some(h) = harness {
        record["harness"] = json!(h);
    }
    record
}

/// THE LAUNCHER'S RECORD OF THE MODEL IT STARTED (backlog e6b05dd6, D2
/// of design fb9ba887, David 2026-10-09: "the dispatcher or harness that
/// launches a run writes the model onto the agent-run, and only that
/// record counts").
///
/// WHY A FIRST RECORD. The release used to read the model the run's own
/// `--report` observed in a transcript on its own host — the agent
/// describing itself. This is written by whatever STARTS the run, through
/// the first-record door (`POST …/steps/{building}/metadata/records`,
/// `expected_absence`), on the run's Building step: the first write
/// stands, its writer and time are stamped by the server, and no later
/// write can replace it. Written before the run exists — the hook writes
/// it before it hands the Agent call back, `--launch` before the spawn —
/// so the run cannot have written it first.
///
/// ONLY A RUN THAT HAS NOT BEGUN. A Building step that is already active
/// or finished belongs to a run that is running or ran: a record written
/// now would describe a launch this caller did not make (a prompt that
/// merely names another run, read by the hook as its own). Refused here,
/// and the run reads `launched unknown`, which is true.
///
/// `Err` is every way it was not written, said whole. The callers carry
/// on: a launch that cannot record its model is still a launch, and its
/// release reads `launched unknown` rather than nothing at all — except
/// [`NotRecorded::Contradicted`], which the hook refuses the call on.
pub(crate) async fn record_launched_model<F, Fut>(
    api: &F,
    run_id: &str,
    record: &Value,
) -> std::result::Result<(), NotRecorded>
where
    F: Fn(reqwest::Method, String, Option<Value>) -> Fut,
    Fut: std::future::Future<Output = Result<Option<Value>>>,
{
    use boss_jobs::car_release::{BUILDING_SLUG, LAUNCHED_MODEL_KEY};
    use boss_jobs::records_before_claim::CLAIMED_KEY;
    let other = NotRecorded::Other;
    let run = api(reqwest::Method::GET, format!("/api/jobs/{run_id}"), None)
        .await
        .map_err(|e| other(format!("reading run {run_id}: {e:#}")))?
        .ok_or_else(|| other(format!("run {run_id} read back empty")))?;
    let building = run
        .get("steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|s| s["spec_slug"] == BUILDING_SLUG)
        .ok_or_else(|| other(format!("run {run_id} has no {BUILDING_SLUG} step")))?;
    let status = building["status"].as_str().unwrap_or_default();
    // Claimed once is begun, even released back to Ready (re-review
    // 211cbae4 B1): the server refuses the record then too, and this
    // says why in the launcher's words.
    let claimed = building["metadata"].get(CLAIMED_KEY).is_some();
    if claimed || !matches!(status, "pending" | "ready") {
        return Err(other(format!(
            "run {run_id}'s {BUILDING_SLUG} step is {status:?}{}: the run has begun, so this \
             is not its launch",
            if claimed { " and has been claimed" } else { "" }
        )));
    }
    // ANOTHER LAUNCH ALREADY STANDS (re-review 211cbae4 N2). A first
    // record is never replaced, so a different value already there is
    // either a launch this caller did not make or a write by someone
    // signing as the launcher before it — and the release would believe
    // it. Not a quiet journal line: the hook refuses the call on it.
    if let Some(stored) = building["metadata"].get(LAUNCHED_MODEL_KEY)
        && stored != record
    {
        return Err(NotRecorded::Contradicted {
            stored: stored.clone(),
        });
    }
    let step = building["id"].as_str().unwrap_or_default();
    let answer = api(
        reqwest::Method::POST,
        format!("/api/jobs/{run_id}/steps/{step}/metadata/records"),
        Some(json!({ "key": LAUNCHED_MODEL_KEY, "value": record, "expected_absence": true })),
    )
    .await
    .map_err(|e| other(format!("the first-record door refused it: {e:#}")))?;
    match answer
        .as_ref()
        .and_then(|a| a.get("result"))
        .and_then(Value::as_str)
    {
        Some("recorded" | "replayed") => Ok(()),
        found => Err(other(format!(
            "the first-record door answered {found:?}, not recorded — nothing stands for it"
        ))),
    }
}

/// Why [`record_launched_model`] wrote nothing.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum NotRecorded {
    /// The run's Building already holds a DIFFERENT `launch.model`.
    Contradicted { stored: Value },
    /// Every other way, said whole.
    Other(String),
}

impl std::fmt::Display for NotRecorded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Contradicted { stored } => write!(
                f,
                "the run's launch is already on the record as {stored}, which is not this \
                 launch: a first record is never replaced, so either this run was launched \
                 before or someone signing as its launcher wrote it first"
            ),
            Self::Other(why) => f.write_str(why),
        }
    }
}

/// Signal the process group `leader` leads. `Ok(false)` is "there is no
/// such group" — every member is gone and reaped.
fn signal_group(leader: u32, signal: rustix::process::Signal) -> std::result::Result<bool, String> {
    let pid = i32::try_from(leader)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
        .ok_or_else(|| format!("{leader} is not a process id"))?;
    match rustix::process::kill_process_group(pid, signal) {
        Ok(()) => Ok(true),
        Err(rustix::io::Errno::SRCH) => Ok(false),
        Err(e) => Err(e.to_string()),
    }
}

/// Does the group still have a member? A member that has exited and not
/// been reaped still counts, so `true` can outlast the last live one.
fn group_has_members(leader: u32) -> bool {
    i32::try_from(leader)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
        .is_some_and(|pid| rustix::process::test_kill_process_group(pid).is_ok())
}

/// STOP EVERYTHING THE LAUNCH STARTED (review f7f0b689, B3): TERM to the
/// process group, `grace` for it to go, then KILL. The leader is reaped
/// here as it exits — an unreaped leader keeps its group alive in the
/// kernel's eyes.
///
/// It used to be one `start_kill` on one pid, and an ordinary background
/// child survived in the attempt's worktree, still carrying BOSS_ACTOR
/// and BOSS_AGENT_RUN, after its run was recorded `died`.
///
/// WHAT ESCAPES: a descendant that left the group (setsid, setpgid).
/// Nothing here can name it, and the receipt does not claim otherwise.
///
/// The group is signalled by the leader's pid after the leader may have
/// been reaped. A group id cannot be reused while any member remains,
/// and with none remaining the signal answers "no such group"; the id
/// being handed to an unrelated new group leader in the milliseconds
/// between would need the pid space to wrap in that time.
async fn stop_group(
    child: &mut tokio::process::Child,
    leader: Option<u32>,
    grace: std::time::Duration,
) -> Value {
    let Some(leader) = leader else {
        return json!({ "signalled": [], "why": "the process had no pid to lead a group" });
    };
    let mut signalled = Vec::new();
    let mut failed = None;
    for (name, signal) in [
        ("TERM", rustix::process::Signal::TERM),
        ("KILL", rustix::process::Signal::KILL),
    ] {
        match signal_group(leader, signal) {
            Ok(true) => signalled.push(name),
            Ok(false) => break,
            Err(e) => {
                failed = Some(e);
                break;
            }
        }
        let until = tokio::time::Instant::now() + grace;
        loop {
            let _ = child.try_wait();
            if !group_has_members(leader) || tokio::time::Instant::now() >= until {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        if !group_has_members(leader) {
            break;
        }
    }
    json!({
        "group": leader,
        "signalled": signalled,
        "signal_error": failed,
        // After KILL a member can only be exiting or unreaped.
        "members_left_unreaped": group_has_members(leader),
    })
}

/// WHATEVER UNWINDS THE LAUNCH STOPS THE GROUP (review b149c579, N19).
/// [`stop_group`] is the stop; this is for the launch that never reaches
/// it — a panic, a future that is dropped. Until it existed that path
/// was `kill_on_drop` alone, which kills the ONE process spawned: a
/// wrapper-shaped harness (the real `codex` on PATH is a node script
/// that spawns the native program) lost its wrapper and the program ran
/// on with no bound, on a packet that read `running`.
///
/// KILL, not TERM then a grace: a drop cannot wait, and a signal that
/// may be ignored is not a stop. Disarmed once [`stop_group`] has run —
/// the group is then gone, and its id is not signalled again. It is
/// declared AFTER the child, so it drops first, while the unreaped
/// leader still holds the group's id. What escapes it is what escapes
/// [`stop_group`], and a verb that is KILLED drops nothing.
struct StopOnDrop(Option<u32>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        if let Some(leader) = self.0 {
            let _ = signal_group(leader, rustix::process::Signal::KILL);
        }
    }
}

/// A line on the verb's stderr, written WITHOUT `eprintln!` (review
/// b149c579, N19). `eprintln!` panics when the write fails, and a verb
/// whose terminal has closed is told so by SIGHUP — the moment it must
/// stop the group and write `died`. Measured: with the reader of stderr
/// gone and then SIGHUP, the group was stopped, the next line panicked,
/// the verb exited 101 and Building's `died` was never written. A line
/// nobody can read is lost; the ending it announced is not.
macro_rules! say {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr(), $($arg)*);
    }};
}

/// Remove the attempt's worktree after a FINISHED turn, when it holds no
/// change (review f7f0b689, N8: each launch left a locked worktree
/// registered in the dispatching checkout for ever). A worktree with
/// changes, or one git will not remove, stays and the reason is the
/// answer; after a death nothing calls this — the worktree is evidence.
async fn remove_clean_worktree(repo: &Path, worktree: &Path) -> std::result::Result<(), String> {
    let text = worktree.to_str().ok_or("the worktree path is not UTF-8")?;
    let changes = git(worktree, &["status", "--porcelain"])
        .await
        .map_err(|e| format!("{e:#}"))?;
    if !changes.is_empty() {
        return Err(format!(
            "it holds changes ({} path(s)), so it is kept",
            changes.lines().count()
        ));
    }
    git(repo, &["worktree", "unlock", text])
        .await
        .map_err(|e| format!("{e:#}"))?;
    git(repo, &["worktree", "remove", text])
        .await
        .map_err(|e| format!("{e:#}"))?;
    Ok(())
}

async fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let out = tokio::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .await
        .context("running git")?;
    if !out.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// How long the fetch of main may take before the launch refuses.
pub(crate) const FETCH_BOUND: std::time::Duration = std::time::Duration::from_secs(120);

/// THE BASE IS MAIN AS THE FORGE HAS IT NOW (backlog 62ebae10). Until
/// 2026-10-15 the worktree was made at the dispatching checkout's HEAD,
/// which the pod fast-forwards only hourly and which may carry a commit
/// of its own: a builder could start up to an hour stale, and a car built
/// on an old base earns "gated, then changed" re-gates (re-gated cars
/// waited 149 minutes against 42). So main is fetched — precisely that
/// one branch — and its sha is the base. The checkout's HEAD, branch,
/// index and files are untouched, because other sessions read them.
///
/// INTO A REF OF THE RUN'S OWN, NOT `refs/remotes/origin/main` (review
/// fae63341, B1). git updates a ref by compare and swap, so when main
/// has moved and several fetches of the SHARED ref overlap — a
/// coordinator refilling after a train lands, `boss gate`'s freshness
/// read, the hourly sidecar, an operator — all but one fail "cannot
/// lock ref", and a claimed run ended `died` though its sibling had
/// written exactly the ref it wanted (2 at once: 30 of 60 failed). No
/// other writer names `into` ([`launch_ref`]), so there is nothing to
/// contend. `--refmap=` is load-bearing: without it git ALSO updates
/// the configured remote-tracking ref "opportunistically" for a branch
/// named on the command line — measured on git 2.39.5, it moved
/// `refs/remotes/origin/main` though the refspec named another ref — and
/// the race is back. `--no-tags` keeps auto-followed tags out of the
/// shared `refs/tags`; `--no-write-fetch-head` leaves the checkout's
/// `FETCH_HEAD` alone (N5). The caller deletes `into` once the worktree
/// exists, or once it is known that it never will.
///
/// A fetch that fails, does not finish within `bound`, or is overtaken
/// by `stop` (the verb's own stop, N4) is an error — never a fall-back to
/// HEAD, which is the staleness this removes. The bound exists because
/// nothing else bounds this wait: the run is already claimed. Whatever
/// ends the wait early ends the WHOLE fetch: it runs in a process group
/// of its own, killed on the way out (N1), because `kill_on_drop` kills
/// git alone, and the transport it started — a `git-remote-http` on a
/// stalled connection, with no bound of its own — ran on, reparented.
async fn fetched_main(
    repo: &Path,
    into: &str,
    bound: std::time::Duration,
    stop: impl std::future::Future<Output = &'static str>,
) -> Result<String> {
    use tokio::io::AsyncReadExt as _;
    let refspec = format!("+refs/heads/main:{into}");
    let mut child = tokio::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "fetch",
            "--quiet",
            "--no-tags",
            "--no-write-fetch-head",
            "--refmap=",
            "origin",
            &refspec,
        ])
        // A credential prompt would wait for a terminal nobody watches.
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .context("running git fetch")?;
    // Declared after the child, so it drops first: the group is KILLED
    // while its unreaped leader still holds the group's id.
    let mut group = StopOnDrop(child.id());
    let mut stderr = child.stderr.take();
    let fetched = async {
        let mut said = String::new();
        let (status, _) = tokio::join!(child.wait(), async {
            if let Some(pipe) = stderr.as_mut() {
                let _ = pipe.read_to_string(&mut said).await;
            }
        });
        (status, said)
    };
    let (status, said) = tokio::select! {
        done = fetched => done,
        _ = tokio::time::sleep(bound) => bail!(
            "git fetch origin main did not finish within {}s — the worktree is based on \
             main as the forge has it, and the checkout's own HEAD is not a substitute",
            bound.as_secs_f32()
        ),
        signal = stop => bail!(
            "the launcher was stopped by {signal} while it fetched main, before anything \
             was started"
        ),
    };
    // git waited for what it started; the group is gone and its id is
    // not signalled again.
    group.0 = None;
    if !status.context("waiting for git fetch")?.success() {
        bail!(
            "git fetch origin main failed: {} — the worktree is based on main as the forge \
             has it, and the checkout's own HEAD is not a substitute",
            said.trim()
        );
    }
    git(
        repo,
        &["rev-parse", "--verify", &format!("{into}^{{commit}}")],
    )
    .await
}

/// The ref one launch fetches main into: private to its run, so no other
/// fetch writes it (review fae63341, B1), and deleted once the worktree
/// stands on what it held.
fn launch_ref(run_id: &str) -> String {
    format!("refs/boss-launch/{run_id}")
}

/// Launch the planned harness for `run_id` and stay until it ends.
/// `Ok` carries the receipt of a process that finished a turn; a death
/// is recorded on the run and then returned as the error.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_at(
    http: &boss_core::machine_token::Client,
    base: &str,
    repo: &Path,
    actor: &str,
    run_id: &str,
    prompt: &str,
    settings: &crate::dispatch::Settings,
    plan: &Plan,
    request: &Request,
    env: &(dyn Fn(&str) -> Option<OsString> + Sync),
) -> Result<Value> {
    if plan.harness.launch == crate::harness::Launch::Cluster {
        return crate::launch_cluster::launch_at(
            http, base, actor, run_id, prompt, settings, plan, request,
        )
        .await;
    }
    let api = |method: reqwest::Method, path: String, body: Option<Value>| {
        let signature = crate::identity::Signature::As(actor.to_string());
        async move { crate::sor_client::api_at_signed(http, base, method, &path, body, signature).await }
    };
    let short = &run_id[..8.min(run_id.len())];
    let stamp = || boss_clock_client::wall_now().to_rfc3339();

    // PREPARE. A failure here is a death like any other: the run is
    // filed and its step claimed, so it must not be left open and silent.
    let mut receipt = json!({
        "harness": plan.harness.id,
        "actor": plan.harness.actor,
        "launched_by": actor,
        "declared_model": settings.model,
        "observed_model": null,
        "effort": settings.effort,
        "sandbox": plan.sandbox,
        "bound_seconds": request.bound.as_secs(),
        "status": "starting",
        "started_at": stamp(),
    });
    let mut ending = Ending::default();
    // Heard from here to the end: a signal that arrives while the
    // worktree is being made is still waiting when the wait begins.
    let mut stops = match request.heed_stops.then(Stops::listen).transpose() {
        Ok(stops) => stops,
        Err(e) => {
            ending.spawn_error = Some(format!("the verb could not listen for its own stop: {e}"));
            return finish(&api, run_id, receipt, &ending, None, settings).await;
        }
    };
    let prepared: Result<(PathBuf, PathBuf, PathBuf, PathBuf)> = async {
        let mut builder = tokio::fs::DirBuilder::new();
        // mode-bits-ok: the attempt's own directory, private to this account
        builder.mode(0o700);
        builder
            .create(&request.dir)
            .await
            .with_context(|| format!("creating --launch-dir {}", request.dir.display()))?;
        let dir = tokio::fs::canonicalize(&request.dir).await?;
        // What the dispatching checkout stood on, kept as provenance; the
        // worktree is based on `base`, never on it (backlog 62ebae10).
        let head = git(repo, &["rev-parse", "HEAD"]).await?;
        let worktree = dir.join("worktree");
        let worktree_text = worktree
            .to_str()
            .context("the worktree path is not UTF-8")?;
        let private = launch_ref(run_id);
        let stop = async {
            match stops.as_mut() {
                Some(stops) => stops.next().await,
                None => std::future::pending().await,
            }
        };
        let based = async {
            let base = fetched_main(repo, &private, FETCH_BOUND, stop).await?;
            git(
                repo,
                &[
                    "worktree",
                    "add",
                    "--detach",
                    "--lock",
                    worktree_text,
                    &base,
                ],
            )
            .await?;
            anyhow::Ok(base)
        }
        .await;
        // The run's own ref has done its work whichever way that went: the
        // worktree now holds the commit, or never will.
        let dropped = git(repo, &["update-ref", "-d", &private]).await;
        let base = based?;
        if let Err(e) = dropped {
            receipt["base_ref_left"] = json!(format!("{private}: {e:#}"));
        }
        // NOT `source_head` (review fae63341, N3): until 62ebae10 that key
        // was the sha the worktree was made from, and a receipt read beside
        // an older one must not mean something else under the same name.
        receipt["dispatching_head"] = json!(head);
        receipt["base_ref"] = json!("origin/main");
        receipt["base_sha"] = json!(base);
        receipt["dir"] = json!(dir);
        receipt["worktree"] = json!(worktree);
        // A HOME OF ITS OWN (review f7f0b689, B4): empty, 0700, made for
        // this launch. Made whatever the file says, used only when it
        // says `home = "own"`.
        let home = dir.join("home");
        let mut private = tokio::fs::DirBuilder::new();
        // mode-bits-ok: the launched process's own empty home
        private.mode(0o700);
        private.create(&home).await?;
        let last = dir.join("last-message.txt");
        Ok((dir, worktree, last, home))
    }
    .await;
    let (dir, worktree, last, own_home) = match prepared {
        Ok(p) => p,
        Err(e) => {
            ending.spawn_error = Some(format!("{e:#}"));
            return finish(&api, run_id, receipt, &ending, None, settings).await;
        }
    };
    let argv = plan
        .harness
        .argv_for(&settings.model, &settings.effort, &plan.sandbox, &last);
    let environment = plan.harness.environment(env, run_id, &own_home);
    receipt["home"] = json!(match plan.harness.home_kind() {
        crate::harness::Home::Own => json!({ "kind": "own", "dir": own_home }),
        crate::harness::Home::Inherit => json!({ "kind": "inherit" }),
    });
    receipt["argv"] = json!(argv);
    receipt["executable"] = json!(plan.executable);
    // Names only: a value in the environment may be a secret.
    receipt["env_names"] = json!(environment.iter().map(|(n, _)| n).collect::<Vec<_>>());
    receipt["stdout_file"] = json!(dir.join("native.jsonl"));
    receipt["stderr_file"] = json!(dir.join("native.stderr"));
    receipt["last_message_file"] = json!(last);
    let full_prompt = format!("{prompt}{}", launcher_section(plan, actor, run_id, &last));
    receipt["prompt_bytes"] = json!(full_prompt.len());

    // THE RECEIPT BEFORE THE PROCESS. Unwritten, nothing is started: a
    // process the record does not know about is the one thing this verb
    // may not leave behind.
    let opened: Result<(tokio::fs::File, std::fs::File)> = async {
        tokio::fs::write(dir.join("prompt.txt"), &full_prompt).await?;
        api(
            reqwest::Method::PATCH,
            format!("/api/jobs/{run_id}/metadata"),
            Some(json!({ RECEIPT_KEY: receipt.clone() })),
        )
        .await
        .context("writing the launch receipt onto the run before the process starts")?;
        let log = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dir.join("native.jsonl"))
            .await?;
        let stderr = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dir.join("native.stderr"))?;
        Ok((log, stderr))
    }
    .await;
    let (mut log, stderr) = match opened {
        Ok(o) => o,
        Err(e) => {
            ending.spawn_error = Some(format!("{e:#}"));
            return finish(&api, run_id, receipt, &ending, None, settings).await;
        }
    };
    // THE MODEL IT STARTS, AS A FIRST RECORD, BEFORE THE PROCESS EXISTS
    // (backlog e6b05dd6): the name exactly as the argv hands it to the
    // harness, or none when the harness file's argv carries no model.
    // Not written is not a reason to start nothing — the release then
    // reads `launched unknown` — but it is said, and kept on the receipt.
    // Read off the harness file's argv TEMPLATE, where `{model}` is the
    // launcher's own placeholder, never by finding the name in the
    // rendered words, which any other word could contain (review fd7f1030
    // F4).
    let handed = plan
        .harness
        .argv
        .iter()
        .any(|w| w.contains("{model}"))
        .then_some(settings.model.as_str());
    let launch_model = launched_model(handed, Some(&plan.harness.id), "argv", actor);
    if let Err(why) = record_launched_model(&api, run_id, &launch_model).await {
        say!("boss dispatch: run {short} — the model it starts is not on the record: {why}");
        receipt["launch_model_unrecorded"] = json!(why.to_string());
    }

    let mut command = tokio::process::Command::new(&plan.executable);
    command
        .args(&argv[1..])
        .env_clear()
        .envs(environment)
        .current_dir(&worktree)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(stderr))
        // A group of its own, so the stop below reaches what it starts.
        .process_group(0)
        .kill_on_drop(true);
    // IT DIES WITH ITS PARENT (review e9514316, B5): a verb that is
    // KILLED runs no handler, so the kernel is asked to send the process
    // SIGTERM when its parent goes. The parent may already be gone by
    // the time the request is made — the re-check of getppid is the
    // standard guard for that — and the request is tied to the THREAD
    // that spawns: here the one that runs the verb to its end.
    let parent = rustix::process::getpid();
    // SAFETY: the closure runs in the forked child before exec and makes
    // two system calls through rustix (prctl, getppid). It allocates
    // nothing, takes no lock and touches no state of the parent.
    unsafe {
        command.pre_exec(move || {
            rustix::process::set_parent_process_death_signal(Some(rustix::process::Signal::TERM))?;
            if rustix::process::getppid() != Some(parent) {
                return Err(rustix::io::Errno::SRCH.into());
            }
            Ok(())
        });
    }
    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => {
            ending.spawn_error = Some(e.to_string());
            return finish(&api, run_id, receipt, &ending, None, settings).await;
        }
    };
    // Read before any wait: a reaped child no longer answers its id.
    let leader = child.id();
    // From here to the stop below, nothing that unwinds this function
    // leaves the group running.
    let mut on_unwind = StopOnDrop(leader);
    // THE BOUND IS COUNTED FROM THE SPAWN (review b149c579, N18), not
    // from whenever the record next answers.
    let bound = tokio::time::sleep(request.bound);
    tokio::pin!(bound);
    receipt["pid"] = json!(leader);
    // It leads its own group, so the group's id is its pid.
    receipt["pgid"] = json!(leader);
    // THE PROCESS ON THE PACKET, NOW THAT THERE IS A PID (review
    // e9514316, B5): the receipt written before the spawn could not carry
    // it, and a verb that dies without a word must leave a process that
    // can be found. In the launch directory too, for a packet that cannot
    // be written.
    //
    // UNDER ITS OWN KEY (review 0facb916, B1). It was the whole `launch`
    // receipt again, with status `running`. Once this write rode beside
    // the wait (N18, below) it could still be in flight when the ending
    // was written, and the record applies a metadata write as a
    // top-level merge: measured, a pid write held 4 s and then delivered
    // turned a `died` receipt back into `running` and took ended_at,
    // exit_code, stopped and transcript with it. Waiting for its answer
    // after the stop would close that only while the connection answers
    // at all — a client with no timeout on a stalled connection would
    // hold the ending for ever. On a key of its own, holding only what
    // stays true once the process is gone, its arriving late changes
    // nothing it should not, in whatever order the record applies it.
    let process = json!({ "pid": leader, "pgid": leader, "spawned_at": stamp() });
    let who = leader.map_or("?".to_string(), |p| p.to_string());
    let _ = tokio::fs::write(dir.join("pid"), format!("{who}\n")).await;
    //
    // THE WRITE IS ONE OF THE THINGS WAITED FOR, NEVER THE THING WAITED
    // ON (review b149c579, N18). It was awaited here, inline, before the
    // wait that enforces the bound, on a client with no timeout that
    // retries a refused connect for 120 s. Measured: a 0.5 s bound with
    // this write held 4 s let the process live 4.54 s, and a SIGTERM
    // during a 2.5 s hold was acted on 2.53 s later. A dark system of
    // record stretched the bound by up to two minutes; a stalled
    // connection removed it and left the verb deaf to its own stop. So
    // it is polled in the wait below, beside the exit, the bound and the
    // signals, and whichever ending comes first is not kept waiting.
    let pid_receipt = api(
        reqwest::Method::PATCH,
        format!("/api/jobs/{run_id}/metadata"),
        Some(json!({ PROCESS_KEY: process })),
    );
    tokio::pin!(pid_receipt);
    let mut pid_receipt_answered = false;
    let pid_unwritten = |e: &anyhow::Error| {
        say!(
            "boss dispatch: run {short}'s pid could not be written onto the run ({e:#}); it is \
             in {}",
            dir.join("pid").display()
        );
    };
    // N20 (review b149c579): TERM THEN KILL. The line used to name TERM
    // alone, which a process that ignores it — and its child — outlives.
    say!(
        "boss dispatch: run {short} launched on harness {} as {} (pid {who}, process group \
         {who}), bound {} s, files in {}. Stopping THIS verb (Ctrl-C, TERM, HUP) stops it and \
         ends the run died; if this verb is killed instead, the process is sent TERM and may \
         ignore it, so stop the group by hand with `kill -TERM -{who}` and then, for whatever \
         is left, `kill -KILL -{who}` — the pid is {PROCESS_KEY}.pid on the run and in {}",
        plan.harness.id,
        plan.harness.actor,
        request.bound.as_secs(),
        dir.display(),
        dir.join("pid").display()
    );
    // Both pipes are served by tasks of their own, so neither can stall
    // the other and neither can stall the VERDICT: that is the child's
    // exit and nothing else (review f7f0b689, B2).
    let native = std::sync::Arc::new(std::sync::Mutex::new(Native::default()));
    let mut reader = {
        let native = native.clone();
        let harness = plan.harness.clone();
        let stdout = child.stdout.take();
        tokio::spawn(async move {
            let Some(out) = stdout else { return };
            let mut reader = BufReader::new(out);
            let mut line = Vec::new();
            loop {
                line.clear();
                match reader.read_until(b'\n', &mut line).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                // Flushed per line: a reader stopped at the grace has
                // already kept everything it read.
                let _ = log.write_all(&line).await;
                let _ = log.flush().await;
                if let Ok(mut seen) = native.lock() {
                    seen.read(&harness, &line);
                }
            }
        })
    };
    let mut writer = {
        let stdin = child.stdin.take();
        let bytes = full_prompt.into_bytes();
        tokio::spawn(async move {
            let Some(mut pipe) = stdin else {
                return Ok(());
            };
            pipe.write_all(&bytes).await?;
            pipe.shutdown().await
        })
    };
    // THREE ENDINGS: the process exits, the bound passes, or the verb
    // itself is told to stop. The last two stop the group below.
    // The receipt's answer is not an ending: it is noted and the wait
    // goes on.
    loop {
        tokio::select! {
            exited = child.wait() => {
                match exited {
                    Ok(status) => ending.exit_code = status.code(),
                    Err(e) => ending.spawn_error = Some(format!("waiting for the process: {e}")),
                }
                break;
            }
            _ = &mut bound => {
                ending.timed_out = true;
                break;
            }
            signal = async {
                match stops.as_mut() {
                    Some(stops) => stops.next().await,
                    None => std::future::pending().await,
                }
            } => {
                ending.stopped_by = Some(signal);
                break;
            }
            written = &mut pid_receipt, if !pid_receipt_answered => {
                pid_receipt_answered = true;
                if let Err(e) = written {
                    pid_unwritten(&e);
                }
            }
        }
    }
    // A process that exited gets a grace for what it wrote last to be
    // read; one stopped at the bound or with the verb gets none before
    // the stop.
    let mut drained = !ending.timed_out
        && ending.stopped_by.is_none()
        && tokio::time::timeout(request.grace, &mut reader)
            .await
            .is_ok();
    // Nothing the launch started outlives its ending, however it ended.
    receipt["stopped"] = stop_group(&mut child, leader, request.grace).await;
    on_unwind.0 = None;
    let _ = tokio::time::timeout(request.grace, child.wait()).await;
    // With the group stopped, a pid receipt still in flight is not waited
    // for: the ending carries the pid, and that write names only the
    // process under its own key, so if the record applies it after the
    // ending it replaces nothing the ending says (review 0facb916, B1).
    if !pid_receipt_answered {
        say!(
            "boss dispatch: run {short}'s pid receipt ({PROCESS_KEY}) was still unanswered when \
             the process was stopped; the ending is written with the pid, and that receipt, if \
             it lands, names only the process"
        );
    }
    if !drained {
        // With the group gone the pipe has no writer and this returns
        // at once; a holder that escaped the group is not waited for.
        drained = tokio::time::timeout(request.grace, &mut reader)
            .await
            .is_ok();
        if !drained {
            reader.abort();
        }
    }
    ending.capture_incomplete = !drained;
    ending.input_error = match tokio::time::timeout(request.grace, &mut writer).await {
        Ok(Ok(Ok(()))) => None,
        Ok(Ok(Err(e))) => Some(e.to_string()),
        Ok(Err(e)) => Some(format!("the prompt writer failed: {e}")),
        Err(_) => {
            writer.abort();
            Some("the process never took its whole prompt".into())
        }
    };
    ending.native = native
        .lock()
        .map(|mut seen| std::mem::take(&mut *seen))
        .unwrap_or_default();
    // A finished turn leaves no worktree behind; a death leaves it,
    // locked, as evidence, and the receipt names where.
    receipt["worktree_kept"] = match ending.died() {
        Some(_) => json!("the run died: kept, locked, for the rescue"),
        None => match remove_clean_worktree(repo, &worktree).await {
            Ok(()) => Value::Null,
            Err(why) => json!(why),
        },
    };
    finish(
        &api,
        run_id,
        receipt,
        &ending,
        Some((&plan.harness, env)),
        settings,
    )
    .await
}

/// Write what was seen and, for a death, the run's terminal.
async fn finish<F, Fut>(
    api: &F,
    run_id: &str,
    mut receipt: Value,
    ending: &Ending,
    observed_from: Option<(
        &crate::harness::Harness,
        &(dyn Fn(&str) -> Option<OsString> + Sync),
    )>,
    settings: &crate::dispatch::Settings,
) -> Result<Value>
where
    F: Fn(reqwest::Method, String, Option<Value>) -> Fut,
    Fut: std::future::Future<Output = Result<Option<Value>>>,
{
    let short = &run_id[..8.min(run_id.len())];
    let (transcript, observed) = match observed_from {
        Some((harness, env)) => observe(harness, env, ending.native.runtime.as_deref()),
        None => (None, Err("the process never started".to_string())),
    };
    let died = ending.died();
    receipt["status"] = json!(if died.is_some() { "died" } else { "exited" });
    receipt["ended_at"] = json!(boss_clock_client::wall_now().to_rfc3339());
    receipt["exit_code"] = json!(ending.exit_code);
    receipt["timed_out"] = json!(ending.timed_out);
    receipt["runtime_id"] = json!(ending.native.runtime);
    receipt["turn_completed"] = json!(ending.native.completed);
    receipt["stdout_lines"] = json!(ending.native.lines);
    receipt["input_error"] = json!(ending.input_error);
    receipt["capture_incomplete"] = json!(ending.capture_incomplete);
    receipt["stopped_by"] = json!(ending.stopped_by);
    receipt["transcript"] = json!(transcript);
    let (model, why) = match &observed {
        Ok((model, seen)) => {
            receipt["observed_model"] = json!(model);
            receipt["transcript_usage_at_exit"] = seen.clone();
            (Some(model.as_str()), None)
        }
        Err(why) => {
            receipt["observed_model"] = Value::Null;
            receipt["observed_model_unread"] = json!(why);
            (None, Some(why.as_str()))
        }
    };
    let line = model_line(&settings.model, model, why);
    receipt["model"] = json!(line);
    if let Some(dir) = receipt.get("dir").and_then(Value::as_str) {
        let _ = tokio::fs::write(
            Path::new(dir).join("receipt.json"),
            serde_json::to_vec_pretty(&receipt)?,
        )
        .await;
    }
    let recorded = api(
        reqwest::Method::PATCH,
        format!("/api/jobs/{run_id}/metadata"),
        Some(json!({ RECEIPT_KEY: receipt.clone() })),
    )
    .await;
    say!("boss dispatch: run {short} — {line}");
    let Some(why) = died else {
        recorded.context("the process finished a turn and its receipt could not be written")?;
        // WHO WRITES THE TERMINAL (review e9514316, N17). This run has
        // no gate, so no green ends it, and the launcher ends only a
        // death. The step it worked on is held by the harness's actor,
        // claimed for it by the actor who launched it (N2), and the
        // launcher completes it — a status-only completion, admitted by
        // the launcher's scope; completing THAT step is what ends the
        // run, through the rule that follows the step's `agent_run` edge.
        say!(
            "boss dispatch: run {short}'s process exited 0 after a completed turn; its handback \
             is {}. The run is still OPEN and nothing here ends it. To end it: read the \
             handback, complete the step it was dispatched for (held by {}, claimed for it by \
             you) — the rule \
             agent-run-delivers-when-its-step-is-done then completes this run's building \
             `delivered` — and record the report with `boss dispatch --report {run_id} \
             --summary-file <that file>`. If that step is not being completed, end the run \
             yourself: `boss step complete {run_id} --step building --field result=delivered` \
             (or result=refused)",
            receipt["last_message_file"].as_str().unwrap_or("not kept"),
            receipt["actor"].as_str().unwrap_or("the harness's actor")
        );
        return Ok(receipt);
    };
    // THE TERMINAL (the existing `died` path): the field through the
    // merge door, then the status alone — only onto a Building that is
    // still open and says nothing, so a result another hand wrote stands.
    let run = api(reqwest::Method::GET, format!("/api/jobs/{run_id}"), None)
        .await?
        .context("the run read returned no body")?;
    let building = crate::envelope::steps(&run)
        .into_iter()
        .find(|s| {
            s.get("spec_slug").and_then(Value::as_str) == Some(crate::dispatch::BUILDING_SLUG)
        })
        .cloned()
        .with_context(|| format!("run {run_id} has no building step to end"))?;
    let open = matches!(
        building.get("status").and_then(Value::as_str),
        Some("pending" | "ready" | "active")
    );
    let unsaid = building.pointer("/metadata/result").is_none();
    if open && unsaid {
        let sid = building
            .get("id")
            .and_then(Value::as_str)
            .context("the building step has no id")?;
        api(
            reqwest::Method::PATCH,
            format!("/api/jobs/{run_id}/steps/{sid}/metadata"),
            Some(json!({
                "result": "died",
                DIED_KEY: {
                    "why": why,
                    "exit_code": ending.exit_code,
                    "timed_out": ending.timed_out,
                    "harness": receipt["harness"],
                    "by": receipt["launched_by"],
                    "at": receipt["ended_at"],
                },
            })),
        )
        .await
        .with_context(|| format!("recording `died` on run {short}'s building step"))?;
        api(
            reqwest::Method::PUT,
            format!("/api/jobs/{run_id}/steps/{sid}"),
            Some(crate::dispatch::completed()),
        )
        .await
        .with_context(|| format!("completing run {short}'s building step as died"))?;
        let after = api(reqwest::Method::GET, format!("/api/jobs/{run_id}"), None)
            .await?
            .context("the run read returned no body")?;
        let held = crate::envelope::steps(&after)
            .into_iter()
            .find(|s| s.get("id").and_then(Value::as_str) == Some(sid))
            .is_some_and(|s| {
                s.get("status").and_then(Value::as_str) == Some("completed")
                    && s.pointer("/metadata/result").and_then(Value::as_str) == Some("died")
            });
        if !held {
            bail!(
                "run {short}'s process died ({why}) and its `died` terminal did not read back — \
                 the run is still open; end it with `boss step complete {run_id} --step \
                 building --field result=died`"
            );
        }
    }
    if let Err(e) = recorded {
        say!("boss dispatch: run {short}'s launch receipt could not be written ({e:#})");
    }
    bail!(
        "run {short} DIED — {why}. Its building step is ended `died`{}; the step it was \
         dispatched for is still held by {} and carries this run's edge: release it with \
         `boss step release` before dispatching it again. Evidence, and the locked worktree \
         (`git worktree unlock` then `git worktree remove` once it is read): {}",
        if open && unsaid {
            ""
        } else {
            " by another hand"
        },
        receipt["launched_by"].as_str().unwrap_or("its dispatcher"),
        receipt["dir"].as_str().unwrap_or("no directory was made")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::wire_tests::{
        PACKET, RUN, keeping_stub, live_agents, packet_without_projection, repo,
    };
    use crate::dispatch::{BriefSource, Overrides, dispatch_to};

    /// A STUB `codex`, never the real one: it records its argv, its whole
    /// environment, its cwd and its stdin under the fixture's `capture`
    /// directory, and then behaves as the fixture's `mode` file says. Bash builtins only — the gate image
    /// has no procps and need not have coreutils on the PATH this
    /// process is handed.
    const STUB: &str = r#"#!/bin/bash
root='__ROOT__'
cap="$root/home/capture"
echo $$ > "$cap/pid"
CODEX_HOME=${CODEX_HOME:-$HOME/.codex}
printf '%s\n' "$@" > "$cap/argv"
export -p > "$cap/env"
pwd > "$cap/cwd"
if [ -f README ]; then echo yes > "$cap/readme"; fi
# What the HOME it was handed holds (review f7f0b689, B4).
if [ -r "$HOME/.config/boss/actor" ]; then echo readable > "$cap/actorfile"; else echo absent > "$cap/actorfile"; fi
entries=("$HOME"/* "$HOME"/.[!.]*)
n=0; for e in "${entries[@]}"; do if [ -e "$e" ]; then n=$((n+1)); fi; done
echo "$n" > "$cap/home-entries"
prompt=$(</dev/stdin)
printf '%s' "$prompt" > "$cap/stdin"
last=
while [ $# -gt 0 ]; do
  if [ "$1" = --output-last-message ]; then last=$2; fi
  shift
done
mode=$(<"$root/home/mode")
started='{"type":"thread.started","thread_id":"01a1-thread"}'
case "$mode" in
  ok|no-transcript)
    printf '%s\n' "$started" '{"type":"turn.started"}'
    if [ "$mode" = ok ]; then
      printf '%s\n' "$(<"$root/home/fixture.jsonl")" > "$CODEX_HOME/sessions/2026/10/07/rollout-2026-10-07T10-00-00-01a1-thread.jsonl"
    fi
    printf '%s' 'VERDICT: approve. One finding, none blocking.' > "$last"
    printf '%s\n' '{"type":"turn.completed"}'
    ;;
  linger)
    # A tool child that outlives the CLI and still holds its stdout
    # (review f7f0b689, B2).
    sleep 30 &
    echo $! > "$cap/childpid"
    printf '%s\n' "$started" '{"type":"turn.started"}'
    printf '%s\n' "$(<"$root/home/fixture.jsonl")" > "$CODEX_HOME/sessions/2026/10/07/rollout-2026-10-07T10-00-00-01a1-thread.jsonl"
    printf '%s' 'VERDICT: approve.' > "$last"
    printf '%s\n' '{"type":"turn.completed"}'
    exit 0
    ;;
  hang-with-child)
    # An ordinary child, NOT detached: same session, same process group
    # (review f7f0b689, B3).
    sleep 30 &
    echo $! > "$cap/childpid"
    printf '%s\n' "$started"
    echo yes > "$cap/spinning"
    while :; do :; done
    ;;
  deaf)
    # A process, and a child, that ignore TERM: only KILL ends them.
    trap '' TERM
    sleep 30 &
    echo $! > "$cap/childpid"
    printf '%s\n' "$started"
    while :; do :; done
    ;;
  fail) printf '%s\n' "$started"; echo 'not signed in' >&2; exit 3 ;;
  silent) exit 0 ;;
  hang) printf '%s\n' "$started"; echo yes > "$cap/spinning"; while :; do :; done ;;
esac
"#;

    /// One Codex rollout of ONE run, in the record shapes the adapter's
    /// own tests use (`transcript_usage::tests::codex_rollout`): the
    /// model on the turn is deliberately NOT the declared one.
    fn rollout(model: &str) -> String {
        [
            json!({"type":"session_meta","payload":{"id":"01a1-thread"}}),
            json!({"type":"event_msg","timestamp":"2026-10-07T10:00:00Z","payload":{"type":"task_started","turn_id":"t-a","started_at":1791367200}}),
            json!({"type":"turn_context","payload":{"turn_id":"t-a","model":model}}),
            json!({"type":"response_item","timestamp":"2026-10-07T10:00:01Z","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":format!("Your run is agent-run {RUN}\n{}", crate::dispatch::marker_line(RUN))}]}}),
            json!({"type":"token_usage_record","timestamp":"2026-10-07T10:00:02Z","payload":{"thread_id":"01a1-thread","turn_id":"t-a","response_id":"a","usage":{"input_tokens":1000,"cached_input_tokens":700,"cache_write_input_tokens":0,"output_tokens":200,"reasoning_output_tokens":50}}}),
            json!({"type":"event_msg","timestamp":"2026-10-07T10:00:03Z","payload":{"type":"task_complete","turn_id":"t-a","started_at":1791367200,"completed_at":1791367203}}),
        ]
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n")
    }

    fn git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }

    fn git_out(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    struct World {
        root: PathBuf,
        home: PathBuf,
        source: PathBuf,
        request: Request,
    }

    impl World {
        fn new(name: &str, mode: &str, bound_ms: u64) -> World {
            let root = boss_testing::scratch_dir(name);
            let home = root.join("home");
            std::fs::create_dir_all(home.join("capture")).unwrap();
            std::fs::create_dir_all(home.join(".codex/sessions/2026/10/07")).unwrap();
            std::fs::write(home.join("mode"), mode).unwrap();
            // The launching account's own actor file, where the CLI's
            // identity falls back to when BOSS_ACTOR is unset.
            std::fs::create_dir_all(home.join(".config/boss")).unwrap();
            std::fs::write(home.join(".config/boss/actor"), "claude@algedonic.dev\n").unwrap();
            std::fs::write(home.join("fixture.jsonl"), rollout("gpt-6-astra")).unwrap();
            std::fs::create_dir_all(root.join("bin")).unwrap();
            // The stub finds its fixtures by a path written into it: the
            // launched process is handed no variable to find them by.
            boss_testing::write_exec(
                &root.join("bin/codex"),
                &STUB.replace("__ROOT__", root.to_str().unwrap()),
            );
            // The forge, and the dispatching checkout as a clone of it:
            // the launch bases its worktree on the forge's main as it
            // fetches it, never on the checkout's own HEAD (backlog
            // 62ebae10).
            let forge = root.join("forge");
            std::fs::create_dir_all(&forge).unwrap();
            git(&forge, &["init", "-q", "-b", "main"]);
            git(&forge, &["config", "user.email", "fixture@example.invalid"]);
            git(&forge, &["config", "user.name", "Fixture"]);
            std::fs::write(forge.join("README"), "fixture\n").unwrap();
            git(&forge, &["add", "README"]);
            git(&forge, &["commit", "-qm", "Seed fixture"]);
            let source = root.join("source");
            git(
                &root,
                &[
                    "clone",
                    "-q",
                    forge.to_str().unwrap(),
                    source.to_str().unwrap(),
                ],
            );
            let request = Request {
                dir: root.join("attempt"),
                bound: std::time::Duration::from_millis(bound_ms),
                // The stub's directory ALONE: no test can reach a real CLI.
                search_path: Some(root.join("bin").into_os_string()),
                grace: std::time::Duration::from_millis(1_500),
                heed_stops: false,
            };
            World {
                root,
                home,
                source,
                request,
            }
        }

        /// The launching session's environment: its own actor, its own
        /// run, its own harness's variables — none of which may arrive.
        fn coordinator_env(&self) -> impl Fn(&str) -> Option<OsString> + Sync + use<> {
            let home = self.home.clone();
            move |name: &str| match name {
                "HOME" => Some(home.clone().into_os_string()),
                "PATH" => Some("/usr/bin:/bin".into()),
                // On the shipped pass list: its VALUE reaches the process
                // and must reach nothing else (review e9514316, N12).
                "LANG" => Some("marker-value-of-a-passed-variable".into()),
                "BOSS_ACTOR" => Some("claude@algedonic.dev".into()),
                "BOSS_ACTOR_FILE" => Some("/coordinator/actor".into()),
                "BOSS_AGENT_RUN" => Some("the-coordinators-own-run".into()),
                "BOSS_JOBS_URL" => Some("http://sor.invalid".into()),
                "CLAUDE_CODE_SESSION_ID"
                | "CLAUDECODE"
                | "CLAUDE_CONFIG_DIR"
                | "CLAUDE_CODE_MESSAGING_TOKEN"
                | "ANTHROPIC_API_KEY" => Some("coordinator-session".into()),
                _ => None,
            }
        }

        /// A commit lands on the forge's main that the dispatching
        /// checkout has not fetched; its sha.
        fn land(&self, file: &str) -> String {
            let forge = self.root.join("forge");
            std::fs::write(forge.join(file), "landed\n").unwrap();
            git(&forge, &["add", file]);
            git(&forge, &["commit", "-qm", "Land on the forge"]);
            git_out(&forge, &["rev-parse", "HEAD"])
        }

        fn captured(&self, name: &str) -> String {
            std::fs::read_to_string(self.home.join("capture").join(name))
                .unwrap_or_else(|e| panic!("the stub wrote no {name}: {e}"))
        }
    }

    /// A step whose block is a reviewer's and declares the default model;
    /// the dispatch names the Codex model, as the operator would.
    fn reviewer_row() -> Value {
        json!({
            "kind": "backlog-item",
            "version": 7,
            "steps": [
                { "title": "triage", "kind": "task" },
                { "title": "build", "kind": "task",
                  "agent": { "profile": "reviewer", "model": "opus-5[1m]", "budget_usd": 2, "effort": "medium" } },
            ],
        })
    }

    async fn dispatch(
        base: &str,
        world: &World,
        model: Option<&str>,
        row_profile_launch: bool,
    ) -> Result<crate::dispatch::Dispatched> {
        dispatch_to(
            &crate::sor_client::machine_client().unwrap(),
            base,
            &repo(),
            PACKET,
            None,
            None,
            &Overrides {
                model: model.map(str::to_string),
                ..Overrides::default()
            },
            false,
            "claude@algedonic.dev",
            "emp-david",
            "boss-dev-0",
            BriefSource::Rendered,
            row_profile_launch.then_some(&world.request),
        )
        .await
    }

    async fn launch(base: &str, world: &World, d: &crate::dispatch::Dispatched) -> Result<Value> {
        run_at(
            &crate::sor_client::machine_client().unwrap(),
            base,
            &world.source,
            "claude@algedonic.dev",
            &d.run_id,
            &d.prompt,
            &d.settings,
            d.plan.as_ref().expect("a launch was planned"),
            &world.request,
            &world.coordinator_env(),
        )
        .await
    }

    /// (a), (b), (c) and (d) of backlog 2f7b8c00 on ONE dispatched step,
    /// against a stub CLI and a jobs API that keeps what it is sent.
    #[tokio::test]
    async fn a_step_whose_model_the_registry_gives_to_codex_is_launched_as_codex_and_only_codex() {
        let world = World::new("dispatch-launch-ok", "ok", 20_000);
        let (base, log, run) =
            keeping_stub(packet_without_projection(), reviewer_row(), live_agents()).await;
        let d = dispatch(&base, &world, Some("gpt-6.1-sol"), true)
            .await
            .unwrap();

        // (a) THE RUN IS THE HARNESS ACTOR'S, and so is its Building —
        // placed while pending, by nobody's hand.
        {
            let filed = run.lock().unwrap().clone().unwrap();
            assert_eq!(filed["metadata"]["agent"], "agent-codex");
            assert_eq!(filed["metadata"]["harness"], "codex-exec");
            assert_eq!(filed["metadata"]["dispatched_by"], "claude@algedonic.dev");
            assert_eq!(filed["metadata"]["model"], "gpt-6.1-sol");
            assert_eq!(filed["steps"][2]["assignee_id"], "agent-codex");
            assert_eq!(filed["steps"][2]["status"], "ready");
        }
        assert_eq!(d.building_assignee.as_deref(), Some("agent-codex"));
        // N2 (review b149c579): THE STEP IS CLAIMED FOR THE HARNESS'S
        // ACTOR, so the claim door reserves the run's budget against
        // agent-codex's hour and counts it under agent-codex's
        // concurrency cap — the actor the run is filed under — and never
        // under the coordinator's row. One claim, sent once.
        assert_eq!(
            packet_claims(&log),
            [format!(
                "/api/jobs/{PACKET}/steps/s-build/claim?claimed_for=agent-codex"
            )]
        );

        let receipt = launch(&base, &world, &d)
            .await
            .expect("the stub finished a turn");

        // THE LAUNCHER'S RECORD (backlog e6b05dd6): the model the argv
        // hands the harness, as a first record on the run's Building,
        // written before the process existed — so before the pid is.
        {
            let filed = run.lock().unwrap().clone().unwrap();
            assert_eq!(
                filed["steps"][2]["metadata"][boss_jobs::car_release::LAUNCHED_MODEL_KEY],
                json!({"model": "gpt-6.1-sol", "harness": "codex-exec", "via": "argv",
                       "launched_by": "claude@algedonic.dev"}),
                "{filed}"
            );
            assert!(
                receipt.get("launch_model_unrecorded").is_none(),
                "{receipt}"
            );
            let calls = log.calls.lock().unwrap();
            let at = |want: &dyn Fn(&(String, String, Value)) -> bool| {
                calls.iter().position(want).expect("the call was made")
            };
            let recorded = at(&|(m, p, _)| m == "POST" && p.ends_with("/metadata/records"));
            let spawned = at(&|(m, _, b)| m == "PATCH" && b.get(PROCESS_KEY).is_some());
            assert!(recorded < spawned, "recorded before the process existed");
        }

        // (a) THE PROCESS: the argv `codex exec --help` spells, in the
        // attempt's own worktree, with the dispatched prompt on stdin.
        let dir = std::fs::canonicalize(&world.request.dir).unwrap();
        assert_eq!(
            world.captured("argv").lines().collect::<Vec<_>>(),
            [
                "exec",
                "--json",
                "--ignore-user-config",
                "--model",
                "gpt-6.1-sol",
                "-c",
                "model_reasoning_effort=\"medium\"",
                "--sandbox",
                // The shipped file's word for a reviewer (design
                // 99a71246): the pod cannot start `read-only`.
                "danger-full-access",
                "--output-last-message",
                dir.join("last-message.txt").to_str().unwrap(),
                "-",
            ]
        );
        assert_eq!(
            world.captured("cwd").trim(),
            dir.join("worktree").to_str().unwrap()
        );
        assert_eq!(
            world.captured("readme").trim(),
            "yes",
            "a real worktree of the source"
        );
        // N8: a finished turn leaves no worktree behind — not on disk,
        // and not registered in the checkout it was made from.
        assert!(!dir.join("worktree").exists(), "removed after a clean exit");
        assert_eq!(receipt["worktree_kept"], Value::Null);
        let listed = std::process::Command::new("git")
            .arg("-C")
            .arg(&world.source)
            .args(["worktree", "list", "--porcelain"])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&listed.stdout)
                .matches("worktree ")
                .count(),
            1,
            "only the source checkout itself is registered"
        );
        let stdin = world.captured("stdin");
        assert!(
            stdin.starts_with(d.prompt.trim_end()),
            "the dispatched prompt, unedited"
        );
        assert!(stdin.contains(&format!("agent-run {RUN}")));
        assert!(stdin.contains("== THE LAUNCHER =="));
        // N10: the launched worker is told who holds the step it works on
        // — itself, since N2 — and who records the verdict on it.
        assert!(
            stdin.contains(
                "The step you were dispatched for is held by agent-codex, claimed FOR you by \
                 claude@algedonic.dev"
            ),
            "the launcher section says who holds the source step:\n{stdin}"
        );

        // (a) THE IDENTITY: its own actor and run, and nothing of the
        // session that launched it.
        let env = world.captured("env");
        assert!(
            env.contains("declare -x BOSS_ACTOR=\"agent-codex\""),
            "{env}"
        );
        assert!(
            env.contains(&format!("declare -x BOSS_AGENT_RUN=\"{RUN}\"")),
            "{env}"
        );
        for absent in [
            "claude@algedonic.dev",
            "the-coordinators-own-run",
            "coordinator-session",
            "BOSS_ACTOR_FILE",
            "CLAUDE",
            "ANTHROPIC",
        ] {
            assert!(
                !env.contains(absent),
                "the process was handed {absent}:\n{env}"
            );
        }
        // B4: a HOME OF ITS OWN — the attempt's empty directory, with the
        // harness's login found through its own variable. The account's
        // actor file is not under it. (It is still READABLE by its real
        // path: same account. That is the residual, and no test here
        // pretends otherwise.)
        assert!(
            env.contains(&format!(
                "declare -x HOME=\"{}\"",
                dir.join("home").display()
            )),
            "{env}"
        );
        assert!(
            env.contains(&format!(
                "declare -x CODEX_HOME=\"{}\"",
                world.home.join(".codex").display()
            )),
            "{env}"
        );
        assert_eq!(world.captured("actorfile").trim(), "absent");
        assert_eq!(world.captured("home-entries").trim(), "0", "an empty home");
        assert_eq!(receipt["home"]["kind"], "own");
        assert!(
            world.home.join(".config/boss/actor").is_file(),
            "the fixture did plant one in the launching account's home"
        );

        // (b) THE MODEL THAT RAN is the transcript's, beside the one
        // declared — here they differ, and the record says so.
        assert_eq!(receipt["status"], "exited");
        assert_eq!(receipt["declared_model"], "gpt-6.1-sol");
        assert_eq!(receipt["observed_model"], "gpt-6-astra");
        assert_eq!(
            receipt["model"],
            "declared gpt-6.1-sol, observed gpt-6-astra — they DIFFER"
        );
        assert_eq!(receipt["runtime_id"], "01a1-thread");
        assert_eq!(receipt["launched_by"], "claude@algedonic.dev");
        assert_eq!(receipt["actor"], "agent-codex");
        let filed = run.lock().unwrap().clone().unwrap();
        assert_eq!(
            filed["metadata"][RECEIPT_KEY], receipt,
            "the receipt is on the run"
        );
        assert_eq!(
            filed["steps"][2]["status"], "ready",
            "a finished turn ends nothing"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("last-message.txt")).unwrap(),
            "VERDICT: approve. One finding, none blocking."
        );

        // (c) THE TOKENS: the report finds the transcript from the
        // launcher's record — no search, no --transcript — and the
        // existing adapter counts it.
        let (recorded, _) = crate::dispatch::transcript_source_at(
            &crate::sor_client::machine_client().unwrap(),
            &base,
            "claude@algedonic.dev",
            RUN,
            &world.coordinator_env(),
        )
        .await;
        let transcript = world
            .home
            .join(".codex/sessions/2026/10/07/rollout-2026-10-07T10-00-00-01a1-thread.jsonl");
        assert_eq!(recorded.as_deref(), Some(transcript.as_path()));
        let located = crate::transcript_usage::locate(
            None,
            recorded.as_deref(),
            Err("unused"),
            RUN,
            std::time::SystemTime::UNIX_EPOCH,
        )
        .unwrap();
        let metered = crate::transcript_usage::meter(
            &located,
            RUN,
            &crate::transcript_usage::Slice::default(),
        )
        .expect("the Codex rollout is metered");
        assert_eq!(
            (
                metered.usage.input,
                metered.usage.cache_read,
                metered.usage.output
            ),
            (300, 700, 200)
        );
        assert_eq!(metered.models.recorded().as_deref(), Some("gpt-6-astra"));
        // N7: a recorded path that is NOT under the harness's transcript
        // home is not believed — here, the same file seen from an
        // environment whose Codex home is elsewhere.
        let elsewhere = world.root.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let (outside, _) = crate::dispatch::transcript_source_at(
            &crate::sor_client::machine_client().unwrap(),
            &base,
            "claude@algedonic.dev",
            RUN,
            &move |name: &str| (name == "CODEX_HOME").then(|| elsewhere.clone().into_os_string()),
        )
        .await;
        assert_eq!(
            outside, None,
            "a path outside the harness's home is dropped"
        );
        let report = crate::dispatch::Report {
            summary: "s".into(),
            spend_usd: None,
            tokens: None,
            meter: Some(metered),
        };
        assert_eq!(crate::dispatch::report_patch(&report)["tokens"], 1200);
        let observed = crate::dispatch::model_observed(&filed, &report);
        assert_eq!(observed["declared"], "gpt-6.1-sol");
        assert_eq!(observed["observed"], "gpt-6-astra");

        // (d) `--started`, as the launched worker: its environment's
        // actor claims the Building it was nominated for, then the
        // receipt. The coordinator cannot: the step is not its own.
        let client = crate::sor_client::machine_client().unwrap();
        let refused = crate::dispatch_started::started_at(
            &client,
            &base,
            RUN,
            "claude@algedonic.dev",
            Some(RUN),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(
            refused.contains("active assigned building step"),
            "{refused}"
        );
        assert!(
            !log.calls
                .lock()
                .unwrap()
                .iter()
                .any(|(m, p, _)| m == "POST" && p.ends_with("/steps/run-building/claim")),
            "the coordinator made no claim on a run that is not its own"
        );
        crate::dispatch_started::started_at(&client, &base, RUN, "agent-codex", Some(RUN))
            .await
            .expect("the worker's own receipt");
        let filed = run.lock().unwrap().clone().unwrap();
        assert_eq!(filed["steps"][2]["status"], "active");
        assert_eq!(
            filed["metadata"]["worker_started"],
            json!({"schema":1,"run":RUN,"actor":"agent-codex"})
        );
        let _ = &world.root;
    }

    /// (b), the other half: a transcript that cannot be read is
    /// `observed unknown` with the reason — never the declared model.
    #[tokio::test]
    async fn an_unreadable_transcript_is_observed_unknown_and_never_the_declared_model() {
        let world = World::new("dispatch-launch-no-transcript", "no-transcript", 20_000);
        let (base, _log, run) =
            keeping_stub(packet_without_projection(), reviewer_row(), live_agents()).await;
        let d = dispatch(&base, &world, Some("gpt-6.1-sol"), true)
            .await
            .unwrap();
        let receipt = launch(&base, &world, &d).await.unwrap();
        assert_eq!(receipt["observed_model"], Value::Null);
        assert_eq!(receipt["declared_model"], "gpt-6.1-sol");
        let said = receipt["model"].as_str().unwrap();
        assert!(
            said.starts_with("declared gpt-6.1-sol, observed unknown (no transcript for runtime 01a1-thread: 0 files match")
                && said.ends_with("under $CODEX_HOME (else $HOME/.codex))"),
            "{said}"
        );
        assert_eq!(receipt["transcript"], Value::Null);
        let filed = run.lock().unwrap().clone().unwrap();
        assert_eq!(
            filed["metadata"][RECEIPT_KEY]["observed_model"],
            Value::Null
        );
        // And at report time, unmetered: the same answer, from no source.
        let report = crate::dispatch::Report {
            summary: "s".into(),
            spend_usd: None,
            tokens: Some(crate::dispatch::Tokens::Total(5)),
            meter: None,
        };
        let observed = crate::dispatch::model_observed(&filed, &report);
        assert_eq!(observed["observed"], Value::Null);
        assert_eq!(
            observed["says"],
            "declared gpt-6.1-sol, observed unknown (the run was not metered: no transcript was read)"
        );
    }

    /// (e) A process that exits non-zero, exits 0 having said nothing,
    /// or outlives its bound ends the run `died`, with the reason — and
    /// the verb fails, naming the step still held.
    #[tokio::test]
    async fn a_process_that_fails_says_nothing_or_outlives_its_bound_ends_the_run_died() {
        for (mode, bound_ms, why) in [
            ("fail", 20_000, "the process exited 3"),
            (
                "silent",
                20_000,
                "exited 0 without printing a completed turn",
            ),
            ("hang", 400, "still running at its time bound"),
        ] {
            let world = World::new(&format!("dispatch-launch-{mode}"), mode, bound_ms);
            let (base, _log, run) =
                keeping_stub(packet_without_projection(), reviewer_row(), live_agents()).await;
            let d = dispatch(&base, &world, Some("gpt-6.1-sol"), true)
                .await
                .unwrap();
            let refused = launch(&base, &world, &d).await.unwrap_err().to_string();
            assert!(
                refused.contains("DIED") && refused.contains(why),
                "{mode}: {refused}"
            );
            assert!(refused.contains("boss step release"), "{mode}: {refused}");
            let filed = run.lock().unwrap().clone().unwrap();
            let building = &filed["steps"][2];
            assert_eq!(building["status"], "completed", "{mode}");
            assert_eq!(building["metadata"]["result"], "died", "{mode}");
            let died = &building["metadata"][DIED_KEY];
            assert!(
                died["why"].as_str().unwrap().contains(why),
                "{mode}: {died}"
            );
            assert_eq!(
                died["by"], "claude@algedonic.dev",
                "the witness signs as itself"
            );
            assert_eq!(died["timed_out"], mode == "hang", "{mode}");
            let receipt = &filed["metadata"][RECEIPT_KEY];
            assert_eq!(receipt["status"], "died", "{mode}");
            // N8: after a death the worktree stays, locked, and says so.
            assert!(
                world.request.dir.join("worktree/README").is_file(),
                "{mode}"
            );
            assert!(
                receipt["worktree_kept"]
                    .as_str()
                    .unwrap()
                    .contains("kept, locked"),
                "{mode}"
            );
            assert!(refused.contains("git worktree unlock"), "{mode}: {refused}");
            assert_eq!(receipt["observed_model"], Value::Null, "{mode}");
            assert_eq!(receipt["declared_model"], "gpt-6.1-sol", "{mode}");
        }
    }

    /// THE WORKTREE IS MAIN AS THE FORGE HAS IT (backlog 62ebae10). The
    /// dispatching checkout is fast-forwarded hourly and may carry a
    /// commit of its own, so its HEAD is neither: a builder based there
    /// started up to an hour stale and earned "gated, then changed"
    /// re-gates. The launch fetches main, bases the worktree on what it
    /// fetched, records that sha on the run — and leaves the checkout's
    /// HEAD, branch and working tree exactly as they were, because other
    /// sessions read it. A death (`fail`) keeps the worktree to read.
    #[tokio::test]
    async fn the_worktree_is_based_on_a_freshly_fetched_origin_main_not_the_checkouts_head() {
        let world = World::new("dispatch-launch-base", "fail", 20_000);
        let source = &world.source;
        git(source, &["config", "user.email", "fixture@example.invalid"]);
        git(source, &["config", "user.name", "Fixture"]);
        std::fs::write(source.join("LOCAL"), "unpushed\n").unwrap();
        git(source, &["add", "LOCAL"]);
        git(
            source,
            &["commit", "-qm", "A commit only this checkout has"],
        );
        let landed = world.land("LANDED");
        let head_before = git_out(source, &["rev-parse", "HEAD"]);
        let branch_before = git_out(source, &["symbolic-ref", "HEAD"]);
        let tracking_before = git_out(source, &["rev-parse", "refs/remotes/origin/main"]);
        assert_ne!(head_before, landed);
        assert_ne!(
            git_out(source, &["rev-parse", "refs/remotes/origin/main"]),
            landed,
            "the checkout has not fetched the landed commit yet"
        );

        let (base, _log, run) =
            keeping_stub(packet_without_projection(), reviewer_row(), live_agents()).await;
        let d = dispatch(&base, &world, Some("gpt-6.1-sol"), true)
            .await
            .unwrap();
        launch(&base, &world, &d).await.unwrap_err();

        let kept = world.request.dir.join("worktree");
        assert_eq!(git_out(&kept, &["rev-parse", "HEAD"]), landed);
        assert!(kept.join("LANDED").is_file(), "the forge's commit is there");
        assert!(!kept.join("LOCAL").exists(), "the checkout's own is not");
        let receipt = run.lock().unwrap().clone().unwrap()["metadata"][RECEIPT_KEY].clone();
        assert_eq!(receipt["base_ref"], "origin/main", "{receipt}");
        assert_eq!(receipt["base_sha"], landed.as_str(), "{receipt}");
        assert_eq!(
            receipt["dispatching_head"],
            head_before.as_str(),
            "{receipt}"
        );
        assert!(receipt.get("source_head").is_none(), "{receipt}");
        assert!(receipt.get("base_ref_left").is_none(), "{receipt}");
        // The dispatching checkout did not move, and the fetch wrote no
        // ref another reader shares (review fae63341, B1): not the
        // remote-tracking main, and not the run's own, which is gone.
        assert_eq!(
            git_out(source, &["rev-parse", "refs/remotes/origin/main"]),
            tracking_before
        );
        assert_eq!(git_out(source, &["for-each-ref", "refs/boss-launch"]), "");
        assert_eq!(git_out(source, &["rev-parse", "HEAD"]), head_before);
        assert_eq!(git_out(source, &["symbolic-ref", "HEAD"]), branch_before);
        assert_eq!(git_out(source, &["status", "--porcelain"]), "");
        assert!(!source.join("LANDED").exists());
    }

    /// A FETCH THAT FAILS REFUSES — it never falls back to the checkout's
    /// HEAD, which is the staleness this exists to remove. The run ends
    /// `died` naming the fetch, and no worktree is made.
    #[tokio::test]
    async fn a_fetch_that_fails_ends_the_run_died_and_makes_no_worktree() {
        let world = World::new("dispatch-launch-nofetch", "ok", 20_000);
        let gone = world.root.join("no-such-forge");
        git(
            &world.source,
            &["remote", "set-url", "origin", gone.to_str().unwrap()],
        );
        let (base, _log, run) =
            keeping_stub(packet_without_projection(), reviewer_row(), live_agents()).await;
        let d = dispatch(&base, &world, Some("gpt-6.1-sol"), true)
            .await
            .unwrap();
        let refused = launch(&base, &world, &d).await.unwrap_err().to_string();
        assert!(refused.contains("DIED"), "{refused}");
        let filed = run.lock().unwrap().clone().unwrap();
        let died = &filed["steps"][2]["metadata"][DIED_KEY];
        assert!(
            died["why"].as_str().unwrap().contains("git fetch"),
            "{died}"
        );
        assert!(!world.request.dir.join("worktree").exists());
        assert_eq!(
            git_out(&world.source, &["for-each-ref", "refs/boss-launch"]),
            ""
        );
        assert!(
            !world.home.join("capture/argv").exists(),
            "nothing was started"
        );
    }

    /// The fetch is BOUNDED: a forge that never answers ends the wait at
    /// the bound with a refusal that says so, instead of holding a claimed
    /// run silent before its process exists (a stop to the verb is heard
    /// only once the process is waited on).
    ///
    /// And NOTHING THE FETCH STARTED OUTLIVES THE REFUSAL (review
    /// fae63341, N1): it used to be `kill_on_drop`, which kills git alone,
    /// and the forge side it had started ran on, reparented. Over HTTP
    /// that is a `git-remote-http` on a stalled connection with no bound
    /// of its own. The stand-in forge here writes its own pid, then
    /// becomes the sleep.
    #[tokio::test]
    async fn a_fetch_that_never_answers_is_refused_at_its_bound() {
        let world = World::new("dispatch-launch-slowfetch", "ok", 20_000);
        let pid_file = world.root.join("forge-side.pid");
        git(
            &world.source,
            &[
                "config",
                "remote.origin.uploadpack",
                // A function, because git appends the repository path to
                // this text as an argument, which `f` ignores.
                &format!(
                    "f() {{ echo $$ > {}; exec sleep 20; }}; f",
                    pid_file.display()
                ),
            ],
        );
        let started = std::time::Instant::now();
        let refused = fetched_main(
            &world.source,
            &launch_ref("bound"),
            std::time::Duration::from_millis(500),
            std::future::pending(),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "{refused}"
        );
        assert!(refused.contains("did not finish within"), "{refused}");
        let forge_side = std::fs::read_to_string(&pid_file).expect("the forge side started");
        assert!(
            gone_within(&forge_side, std::time::Duration::from_secs(5)),
            "the fetch's own child {forge_side} outlived the refusal"
        );
    }

    /// LAUNCHES THAT OVERLAP DO NOT KILL EACH OTHER (review fae63341,
    /// B1). Every fetch used to write the SHARED checkout's
    /// `refs/remotes/origin/main`, and git updates a ref by compare and
    /// swap: when main had moved, all but one of the overlapping fetches
    /// failed "cannot lock ref" and its claimed run ended `died` — though
    /// the ref it wanted was there, written by its sibling. Measured on
    /// the car's own command: 2 at once, 30 of 60 failed; 8 at once, 210
    /// of 240. A coordinator refilling after a train lands launches
    /// several at once, exactly when main has just moved.
    ///
    /// Each round races four launches AND a plain `git fetch origin` —
    /// the sidecar's, an operator's — that writes the shared ref.
    #[tokio::test]
    async fn launches_that_fetch_at_once_after_main_moved_all_get_main() {
        let world = World::new("dispatch-launch-contend", "ok", 20_000);
        for round in 0..5 {
            let landed = world.land(&format!("LANDED-{round}"));
            let refs = (0..4)
                .map(|n| launch_ref(&format!("run-{round}-{n}")))
                .collect::<Vec<_>>();
            let fetches = refs.iter().map(|private| {
                fetched_main(&world.source, private, FETCH_BOUND, std::future::pending())
            });
            let shared = tokio::process::Command::new("git")
                .arg("-C")
                .arg(&world.source)
                .args(["fetch", "--quiet", "origin"])
                .output();
            let (got, _) = tokio::join!(futures::future::join_all(fetches), shared);
            for got in got {
                assert_eq!(got.expect("every overlapping fetch gets main"), landed);
            }
        }
    }

    /// A STOP DURING THE FETCH ENDS IT THEN (review fae63341, N4). The
    /// verb's own stop used to be heard only in the wait for the harness,
    /// so a stop sent while the fetch hung was held up to the bound — and
    /// then a harness was started only to be stopped. The fetch's group
    /// goes with it.
    #[tokio::test]
    async fn a_stop_during_the_fetch_ends_it_at_once() {
        let world = World::new("dispatch-launch-stopfetch", "ok", 20_000);
        let pid_file = world.root.join("forge-side.pid");
        git(
            &world.source,
            &[
                "config",
                "remote.origin.uploadpack",
                &format!(
                    "f() {{ echo $$ > {}; exec sleep 20; }}; f",
                    pid_file.display()
                ),
            ],
        );
        let started = std::time::Instant::now();
        let stop = async {
            while !std::fs::read_to_string(&pid_file).is_ok_and(|p| !p.trim().is_empty()) {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            "SIGTERM"
        };
        let refused = fetched_main(&world.source, &launch_ref("stopped"), FETCH_BOUND, stop)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "{refused}"
        );
        assert!(refused.contains("stopped by SIGTERM"), "{refused}");
        let forge_side = std::fs::read_to_string(&pid_file).unwrap();
        assert!(
            gone_within(&forge_side, std::time::Duration::from_secs(5)),
            "the fetch's own child {forge_side} outlived the stop"
        );
    }

    /// Is `pid` a process that can still act? A zombie cannot, and an
    /// orphan in a pod may stay one: its state is read, not only its
    /// presence. No external tool — the gate image has no procps.
    fn alive(pid: &str) -> bool {
        std::fs::read_to_string(Path::new("/proc").join(pid.trim()).join("stat"))
            .ok()
            .and_then(|stat| {
                // "<pid> (<comm>) <state> …": the state follows the last ')'.
                stat.rsplit_once(')')
                    .and_then(|(_, rest)| rest.trim_start().chars().next())
            })
            .is_some_and(|state| state != 'Z' && state != 'X')
    }

    /// B2 (review f7f0b689). A process that prints a completed turn and
    /// exits 0 has finished, whatever it left holding its stdout: the
    /// verdict is the CHILD's exit. It used to wait for EOF on the pipe,
    /// so this run was recorded `died`, `timed_out`, after the whole
    /// bound — onto a terminal step.
    #[tokio::test]
    async fn a_finished_turn_is_judged_on_the_exit_though_a_child_still_holds_stdout() {
        let world = World::new("dispatch-launch-linger", "linger", 20_000);
        let (base, _log, run) =
            keeping_stub(packet_without_projection(), reviewer_row(), live_agents()).await;
        let d = dispatch(&base, &world, Some("gpt-6.1-sol"), true)
            .await
            .unwrap();
        let receipt = launch(&base, &world, &d)
            .await
            .expect("an exit 0 after a completed turn is not a death");
        // Gate873 took 28.9s including worktree and receipt I/O even
        // though the child exited correctly. The verdict, its unchanged
        // 20s process bound, and the stopped descendant prove the behavior;
        // total test duration is not the process's exit observation.
        assert_eq!(receipt["status"], "exited");
        assert_eq!(receipt["exit_code"], 0);
        assert_eq!(receipt["timed_out"], false);
        assert_eq!(receipt["turn_completed"], true);
        assert_eq!(receipt["observed_model"], "gpt-6-astra");
        let filed = run.lock().unwrap().clone().unwrap();
        assert_eq!(
            filed["steps"][2]["status"], "ready",
            "no terminal was written"
        );
        assert!(filed["steps"][2]["metadata"].get("result").is_none());
        // And what it left behind does not go on acting as the run.
        assert!(
            !alive(&world.captured("childpid")),
            "the lingering child was stopped"
        );
    }

    /// B3 (review f7f0b689). The bound stops the launched process AND
    /// what it started: an ordinary background child used to survive in
    /// the attempt's worktree, still carrying BOSS_ACTOR and
    /// BOSS_AGENT_RUN, after the run was recorded `died`.
    #[tokio::test]
    async fn the_bound_stops_every_process_of_the_launched_group() {
        let world = World::new("dispatch-launch-hang-child", "hang-with-child", 500);
        let (base, _log, run) =
            keeping_stub(packet_without_projection(), reviewer_row(), live_agents()).await;
        let d = dispatch(&base, &world, Some("gpt-6.1-sol"), true)
            .await
            .unwrap();
        let refused = launch(&base, &world, &d).await.unwrap_err().to_string();
        assert!(
            refused.contains("still running at its time bound"),
            "{refused}"
        );
        let filed = run.lock().unwrap().clone().unwrap();
        let spawned = filed["metadata"][RECEIPT_KEY]["pid"].to_string();
        assert!(!alive(&spawned), "the spawned process is gone");
        assert!(
            !alive(&world.captured("childpid")),
            "its ordinary child is gone too"
        );
        assert_eq!(filed["steps"][2]["metadata"]["result"], "died");
        assert_eq!(
            filed["metadata"][RECEIPT_KEY]["stopped"]["signalled"],
            json!(["TERM"]),
            "an ordinary group goes on TERM"
        );
    }

    /// B1: a harness file that makes a PROCESS sign as the actor
    /// launching it is refused before the claim — the file cannot know
    /// who dispatches, so the plan judges it.
    #[test]
    fn a_process_harness_that_signs_as_its_launcher_is_refused() {
        let world = World::new("dispatch-launch-self", "ok", 1_000);
        let harnesses = crate::harness::read_all(&repo()).unwrap();
        let agents = live_agents()["data"].as_array().unwrap().clone();
        let settings = crate::dispatch::Settings {
            profile: "reviewer".into(),
            model: "gpt-6.1-sol".into(),
            budget_usd: 2.0,
            effort: "medium".into(),
            executor_provenance: Default::default(),
        };
        let path = world.request.search_path.as_ref();
        let refused = plan(
            &harnesses,
            &agents,
            &settings,
            &world.request,
            path,
            Some("agent-codex"),
        )
        .unwrap_err();
        assert!(
            refused.contains("signs as agent-codex, the actor running this verb"),
            "{refused}"
        );
        assert!(
            plan(
                &harnesses,
                &agents,
                &settings,
                &world.request,
                path,
                Some("agent-claude")
            )
            .is_ok()
        );
        assert!(plan(&harnesses, &agents, &settings, &world.request, path, None).is_ok());
    }

    /// N6: `--launch` reads the roster only from a checkout whose copy
    /// is origin/main's — committed, tracked, and equal to the ref.
    #[test]
    fn a_roster_that_is_not_origin_mains_is_refused() {
        let root = boss_testing::scratch_dir("dispatch-launch-roster");
        git(&root, &["init", "-q"]);
        git(&root, &["config", "user.email", "fixture@example.invalid"]);
        git(&root, &["config", "user.name", "Fixture"]);
        let dir = root.join(crate::harness::DIR);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("one.toml"), "id = \"one\"\n").unwrap();
        git(&root, &["add", "."]);
        git(&root, &["commit", "-qm", "roster"]);
        // No origin/main at all: unread, and unread is refused.
        let unread = roster_is_mains(&root).unwrap_err();
        assert!(unread.contains("could not be read"), "{unread}");
        assert!(
            unread.contains("git fetch origin"),
            "it says what to do: {unread}"
        );
        git(&root, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        assert_eq!(roster_is_mains(&root), Ok(()));
        // An untracked file, an uncommitted edit, a committed difference.
        std::fs::write(dir.join("two.toml"), "id = \"two\"\n").unwrap();
        let untracked = roster_is_mains(&root).unwrap_err();
        assert!(
            untracked.contains("uncommitted") && untracked.contains("two.toml"),
            "{untracked}"
        );
        std::fs::remove_file(dir.join("two.toml")).unwrap();
        std::fs::write(dir.join("one.toml"), "id = \"one\"\nargv = [\"bash\"]\n").unwrap();
        assert!(roster_is_mains(&root).unwrap_err().contains("uncommitted"));
        git(&root, &["commit", "-qam", "a branch's roster"]);
        let differs = roster_is_mains(&root).unwrap_err();
        assert!(differs.contains("differs from origin/main"), "{differs}");
        // N14 (review e9514316): a file git IGNORES is still a file the
        // roster reader reads. The tree's own .gitignore has `.env.*`.
        git(&root, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        std::fs::write(root.join(".gitignore"), ".env.*\n").unwrap();
        git(&root, &["add", ".gitignore"]);
        git(&root, &["commit", "-qm", "ignore"]);
        std::fs::write(dir.join(".env.toml"), "id = \".env\"\n").unwrap();
        let ignored = roster_is_mains(&root).unwrap_err();
        assert!(ignored.contains(".env.toml"), "{ignored}");
        std::fs::remove_file(dir.join(".env.toml")).unwrap();
        // A change elsewhere in the tree is not the roster's.
        git(&root, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        std::fs::write(root.join("README"), "x\n").unwrap();
        assert_eq!(roster_is_mains(&root), Ok(()));
    }

    /// The variable a test names to make [`the_verb_in_its_own_process`]
    /// run; unset, that test is a no-op.
    const HELPER: &str = "BOSS_TEST_LAUNCH_VERB";

    /// THE VERB, FOR REAL, IN A PROCESS OF ITS OWN (review e9514316, B5):
    /// the same `run_at`, a stub that spins, a bound far away. The tests
    /// below start this test binary again with [`HELPER`] set and then
    /// signal or kill THAT process — the only way to ask what a stopped
    /// verb leaves behind. It tells its parent where its fixture is, and
    /// when `run_at` returns, what the run looks like.
    #[tokio::test]
    async fn the_verb_in_its_own_process() {
        let Some(tell) = std::env::var_os(HELPER).map(PathBuf::from) else {
            return;
        };
        let mut world = World::new("dispatch-launch-verb", "hang", 60_000);
        world.request.grace = std::time::Duration::from_millis(500);
        // This process IS the verb: it takes its own stop, as the verb does.
        world.request.heed_stops = true;
        let (base, log, run) =
            keeping_stub(packet_without_projection(), reviewer_row(), live_agents()).await;
        let d = dispatch(&base, &world, Some("gpt-6.1-sol"), true)
            .await
            .unwrap();
        std::fs::write(tell.join("root"), world.root.to_str().unwrap()).unwrap();
        let out = launch(&base, &world, &d).await;
        let ended = json!({
            "error": out.err().map(|e| format!("{e:#}")),
            "run": run.lock().unwrap().clone(),
            "writes": log.calls.lock().unwrap().iter()
                .filter(|(m, _, _)| m != "GET")
                .map(|(m, p, b)| json!([m, p, b]))
                .collect::<Vec<_>>(),
        });
        std::fs::write(tell.join("ended"), ended.to_string()).unwrap();
    }

    /// Start [`the_verb_in_its_own_process`] and wait until its launched
    /// stub is spinning. Returns the verb, where it tells, and its world.
    fn verb_running(name: &str) -> (std::process::Child, PathBuf, PathBuf) {
        let tell = boss_testing::scratch_dir(name);
        let stderr = Stdio::from(std::fs::File::create(tell.join("stderr")).unwrap());
        verb_running_saying_to(tell, stderr)
    }

    /// [`verb_running`], with the verb's stderr chosen by the caller.
    fn verb_running_saying_to(
        tell: PathBuf,
        stderr: Stdio,
    ) -> (std::process::Child, PathBuf, PathBuf) {
        let verb = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "dispatch_launch::tests::the_verb_in_its_own_process",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(HELPER, &tell)
            // A file, not /dev/null: dispatch refuses to print a prompt
            // into nothing (d268b260).
            .stdout(Stdio::from(
                std::fs::File::create(tell.join("stdout")).unwrap(),
            ))
            .stderr(stderr)
            .spawn()
            .unwrap();
        let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let root = loop {
            assert!(
                std::time::Instant::now() < until,
                "the verb never launched its stub"
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
            if let Ok(root) = std::fs::read_to_string(tell.join("root")) {
                let root = PathBuf::from(root);
                if root.join("home/capture/spinning").is_file() {
                    break root;
                }
            }
        };
        (verb, tell, root)
    }

    /// Is `pid` gone within `wait`? When it is not, it is KILLED here
    /// before the answer is given: a failing assertion must not leave a
    /// stub spinning on the box (the first red run of these tests did).
    fn gone_within(pid: &str, wait: std::time::Duration) -> bool {
        let until = std::time::Instant::now() + wait;
        while alive(pid) {
            if std::time::Instant::now() >= until {
                if let Some(pid) = pid
                    .trim()
                    .parse()
                    .ok()
                    .and_then(rustix::process::Pid::from_raw)
                {
                    let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
                }
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        true
    }

    /// B5 (1). SIGINT, SIGTERM and SIGHUP to the verb are an ENDING: the
    /// launched group is stopped, the run is ended `died` naming the
    /// signal, and the verb says so. Before, the signal ended the verb
    /// and nothing else — the process was in a group of its own (the B3
    /// fix), so nothing reached it, and it went on with no bound and no
    /// witness, signing as the harness actor on a run still `starting`.
    #[test]
    fn a_signal_to_the_verb_stops_the_launched_group_and_ends_the_run_died() {
        for (name, signal) in [
            ("SIGTERM", rustix::process::Signal::TERM),
            ("SIGINT", rustix::process::Signal::INT),
            ("SIGHUP", rustix::process::Signal::HUP),
        ] {
            let (mut verb, tell, root) = verb_running(&format!("dispatch-launch-stopped-{name}"));
            let launched = std::fs::read_to_string(root.join("home/capture/pid")).unwrap();
            assert!(alive(&launched), "{name}: the stub is running");
            let pid = rustix::process::Pid::from_raw(verb.id() as i32).unwrap();
            rustix::process::kill_process(pid, signal).unwrap();
            let _ = verb.wait().unwrap();
            assert!(
                gone_within(&launched, std::time::Duration::from_secs(5)),
                "{name}: the launched process outlived the verb that was stopped"
            );
            let ended: Value = serde_json::from_str(
                &std::fs::read_to_string(tell.join("ended"))
                    .unwrap_or_else(|_| panic!("{name}: the verb ended without ending the run")),
            )
            .unwrap();
            let error = ended["error"].as_str().unwrap_or_default();
            assert!(
                error.contains("DIED") && error.contains(name),
                "{name}: {error}"
            );
            let building = &ended["run"]["steps"][2];
            assert_eq!(building["status"], "completed", "{name}");
            assert_eq!(building["metadata"]["result"], "died", "{name}");
            let why = building["metadata"][DIED_KEY]["why"].as_str().unwrap();
            assert!(
                why.contains(name) && why.contains("launcher was stopped"),
                "{name}: {why}"
            );
            let receipt = &ended["run"]["metadata"][RECEIPT_KEY];
            assert_eq!(receipt["status"], "died", "{name}");
            assert_eq!(receipt["stopped_by"], name, "{name}");
            assert_eq!(receipt["stopped"]["signalled"], json!(["TERM"]), "{name}");
            // N12: no VALUE of a variable in anything written or said.
            let said = std::fs::read_to_string(tell.join("stderr")).unwrap();
            assert!(said.contains("launched on harness codex-exec"), "{said}");
            // N20 (review b149c579): the by-hand line is TERM THEN KILL.
            // TERM alone does not end a process that ignores it.
            let group = launched.trim();
            assert!(
                said.contains(&format!("`kill -TERM -{group}`"))
                    && said.contains(&format!("`kill -KILL -{group}`")),
                "{name}: the by-hand stop names both signals: {said}"
            );
            for (place, text) in [
                ("stderr", said.as_str()),
                ("the writes", &ended["writes"].to_string()),
            ] {
                for value in ["marker-value-of-a-passed-variable", "coordinator-session"] {
                    assert!(!text.contains(value), "{name}: {place} carries {value}");
                }
            }
        }
    }

    /// B5 (2). A verb that is KILLED handles nothing — so the launched
    /// process is told by the kernel: a parent-death signal, set before
    /// it execs.
    #[test]
    fn a_killed_verb_takes_the_launched_process_with_it() {
        let (mut verb, tell, root) = verb_running("dispatch-launch-killed");
        let launched = std::fs::read_to_string(root.join("home/capture/pid")).unwrap();
        assert!(alive(&launched));
        verb.kill().unwrap();
        let _ = verb.wait().unwrap();
        assert!(
            gone_within(&launched, std::time::Duration::from_secs(5)),
            "the launched process outlived a killed verb"
        );
        assert!(
            !tell.join("ended").exists(),
            "a killed verb wrote no ending"
        );
    }

    /// B5 (3). The pid and the process group are on the packet as soon as
    /// the process exists — a second write, right after the spawn — so
    /// a verb that dies without a word leaves a process that can be
    /// found. The pre-spawn receipt cannot carry them: there is no pid.
    /// That write is under its own key, never the receipt's (review
    /// 0facb916, B1): the receipt is written twice, before the spawn and
    /// at the end, and nothing else writes it.
    #[tokio::test]
    async fn the_receipt_carries_the_pid_as_soon_as_the_process_exists() {
        let world = World::new("dispatch-launch-pid", "hang", 500);
        let (base, log, _run) =
            keeping_stub(packet_without_projection(), reviewer_row(), live_agents()).await;
        let d = dispatch(&base, &world, Some("gpt-6.1-sol"), true)
            .await
            .unwrap();
        launch(&base, &world, &d).await.unwrap_err();
        let launched = world.captured("pid").trim().to_string();
        let writes: Vec<Value> = log
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, p, _)| m == "PATCH" && p == &format!("/api/jobs/{RUN}/metadata"))
            .map(|(_, _, b)| b.clone())
            .collect();
        let keys: Vec<Vec<&str>> = writes
            .iter()
            .map(|b| b.as_object().unwrap().keys().map(String::as_str).collect())
            .collect();
        assert_eq!(
            keys,
            [[RECEIPT_KEY], [PROCESS_KEY], [RECEIPT_KEY]],
            "the receipt, the process, the receipt — each write one key"
        );
        let states: Vec<&str> = [&writes[0], &writes[2]]
            .iter()
            .map(|b| b[RECEIPT_KEY]["status"].as_str().unwrap())
            .collect();
        assert_eq!(states, ["starting", "died"]);
        assert_eq!(
            writes[0][RECEIPT_KEY].get("pid"),
            None,
            "before the spawn there is none"
        );
        let process = &writes[1][PROCESS_KEY];
        assert_eq!(process["pid"].to_string(), launched);
        assert_eq!(
            process["pgid"].to_string(),
            launched,
            "it leads its own group"
        );
        assert_eq!(
            process.get("status"),
            None,
            "it says nothing that stops being true when the process ends"
        );
        assert_eq!(
            writes[2][RECEIPT_KEY]["pid"].to_string(),
            launched,
            "the ending carries the pid too"
        );
        assert_eq!(
            std::fs::read_to_string(world.request.dir.join("pid"))
                .unwrap()
                .trim(),
            launched,
            "and in the launch directory, for a packet that could not be written"
        );
    }

    /// N12 (review e9514316): nothing the launcher writes or records
    /// carries the VALUE of an environment variable. A missing transcript
    /// used to be explained "…under <the home's path>", which is the
    /// value of whatever variable the file's `home_env` names.
    #[tokio::test]
    async fn no_receipt_or_packet_write_carries_the_value_of_a_variable() {
        let world = World::new("dispatch-launch-values", "no-transcript", 20_000);
        let (base, log, run) =
            keeping_stub(packet_without_projection(), reviewer_row(), live_agents()).await;
        let d = dispatch(&base, &world, Some("gpt-6.1-sol"), true)
            .await
            .unwrap();
        let receipt = launch(&base, &world, &d).await.unwrap();
        let unread = receipt["observed_model_unread"].as_str().unwrap();
        assert!(
            unread.contains("$CODEX_HOME"),
            "the variable is NAMED: {unread}"
        );
        let home = world.home.to_str().unwrap();
        let writes = log
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, _, _)| m != "GET")
            .map(|(_, p, b)| format!("{p} {b}"))
            .collect::<Vec<_>>()
            .join("\n");
        let filed = run.lock().unwrap().clone().unwrap()["metadata"][RECEIPT_KEY].to_string();
        for (place, text) in [
            ("the receipt", receipt.to_string()),
            ("the packet", filed),
            ("the writes", writes),
        ] {
            for value in [
                home,
                "marker-value-of-a-passed-variable",
                "coordinator-session",
                "http://sor.invalid",
            ] {
                assert!(!text.contains(value), "{place} carries the value {value}");
            }
        }
    }

    /// B3, the second signal: a group that ignores TERM is ended by
    /// KILL after the grace, and the receipt says both were sent.
    #[tokio::test]
    async fn a_group_that_ignores_term_is_killed_after_the_grace() {
        let mut world = World::new("dispatch-launch-deaf", "deaf", 400);
        world.request.grace = std::time::Duration::from_millis(400);
        let (base, _log, run) =
            keeping_stub(packet_without_projection(), reviewer_row(), live_agents()).await;
        let d = dispatch(&base, &world, Some("gpt-6.1-sol"), true)
            .await
            .unwrap();
        launch(&base, &world, &d).await.unwrap_err();
        let filed = run.lock().unwrap().clone().unwrap();
        let receipt = &filed["metadata"][RECEIPT_KEY];
        assert_eq!(receipt["stopped"]["signalled"], json!(["TERM", "KILL"]));
        assert!(
            !alive(&receipt["pid"].to_string()),
            "the deaf process is gone"
        );
        assert!(!alive(&world.captured("childpid")), "and its deaf child");
    }

    /// What a system of record in front of the launcher does with the
    /// SECOND metadata write — the one that carries the pid, made right
    /// after the spawn — once it has held it for a while.
    #[derive(Clone, Copy, PartialEq)]
    enum Held {
        /// Dropped unanswered: the stalled connection of review
        /// b149c579, N18.
        Dropped,
        /// Delivered late, so the record applies it after whatever was
        /// written meanwhile: a server that took the request and applied
        /// it slowly (review 0facb916, B1).
        DeliveredLate,
    }

    /// A system of record in front of `base` whose SECOND metadata write
    /// is held for `hold` and then `held`. Every other request passes
    /// through untouched.
    async fn stalling_the_pid_receipt(base: &str, hold: std::time::Duration, held: Held) -> String {
        let upstream = base.trim_start_matches("http://").to_string();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let receipt = format!("PATCH /api/jobs/{RUN}/metadata ");
        tokio::spawn(async move {
            let mut receipts = 0;
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                // The request line, looked at and left in the socket.
                let mut head = [0u8; 256];
                let mut seen = 0;
                for _ in 0..400 {
                    seen = sock.peek(&mut head).await.unwrap_or(0);
                    if seen == 0 || head[..seen].contains(&b'\n') {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
                if head[..seen].starts_with(receipt.as_bytes()) {
                    receipts += 1;
                }
                let stall = receipts == 2 && head[..seen].starts_with(receipt.as_bytes());
                let upstream = upstream.clone();
                tokio::spawn(async move {
                    if stall {
                        tokio::time::sleep(hold).await;
                        if held == Held::Dropped {
                            return;
                        }
                    }
                    if let Ok(mut up) = tokio::net::TcpStream::connect(&upstream).await {
                        let _ = tokio::io::copy_bidirectional(&mut sock, &mut up).await;
                    }
                });
            }
        });
        format!("http://{addr}")
    }

    /// N18 (review b149c579). THE BOUND IS COUNTED FROM THE SPAWN,
    /// WHATEVER THE RECORD IS DOING. The receipt that carries the pid
    /// was awaited inline, before the wait that enforces the bound: with
    /// that write held 4 s and a bound of 0.5 s the process lived 4.54 s,
    /// and a signal during the hold was not heard until it ended. A dark
    /// system of record stretched the bound; a stalled one removed it.
    #[tokio::test]
    async fn the_bound_is_kept_while_the_pid_receipt_cannot_be_written() {
        let mut world = World::new("dispatch-launch-stalled-receipt", "hang", 500);
        world.request.grace = std::time::Duration::from_millis(400);
        let (base, _log, run) =
            keeping_stub(packet_without_projection(), reviewer_row(), live_agents()).await;
        let d = dispatch(&base, &world, Some("gpt-6.1-sol"), true)
            .await
            .unwrap();
        let hold = std::time::Duration::from_secs(4);
        let stalled = stalling_the_pid_receipt(&base, hold, Held::Dropped).await;
        let began = std::time::Instant::now();
        let refused = launch(&stalled, &world, &d).await.unwrap_err().to_string();
        let took = began.elapsed();
        assert!(
            refused.contains("still running at its time bound"),
            "{refused}"
        );
        assert!(
            took < std::time::Duration::from_secs(3),
            "a 0.5 s bound was enforced {took:?} after the launch began: the stalled receipt \
             write ({hold:?}) held it"
        );
        let filed = run.lock().unwrap().clone().unwrap();
        let receipt = &filed["metadata"][RECEIPT_KEY];
        assert_eq!(receipt["status"], "died");
        assert_eq!(receipt["timed_out"], true);
        // The ending carries the pid the stalled write could not.
        assert_eq!(receipt["pid"].to_string(), world.captured("pid").trim());
        assert!(!alive(&receipt["pid"].to_string()), "the process is gone");
        assert_eq!(filed["steps"][2]["metadata"]["result"], "died");
    }

    /// B1 (review 0facb916). A PID RECEIPT THAT LANDS LATE CANNOT REPLACE
    /// THE ENDING. Once the pid write rode beside the wait instead of
    /// before it, it could still be in flight when the ending was
    /// written — and both wrote the whole `launch` receipt through a
    /// top-level merge, so whichever the record applied LAST was the
    /// receipt. Measured on 90597863 with this proxy: at return the run
    /// read `died`; five seconds later it read `running`, with ended_at,
    /// exit_code, stopped and transcript gone, while Building read died.
    #[tokio::test]
    async fn a_pid_receipt_that_lands_late_cannot_replace_the_ending() {
        let mut world = World::new("dispatch-launch-late-receipt", "hang", 500);
        world.request.grace = std::time::Duration::from_millis(400);
        let (base, _log, run) =
            keeping_stub(packet_without_projection(), reviewer_row(), live_agents()).await;
        let d = dispatch(&base, &world, Some("gpt-6.1-sol"), true)
            .await
            .unwrap();
        let hold = std::time::Duration::from_secs(4);
        let late = stalling_the_pid_receipt(&base, hold, Held::DeliveredLate).await;
        let refused = launch(&late, &world, &d).await.unwrap_err().to_string();
        assert!(
            refused.contains("still running at its time bound"),
            "{refused}"
        );
        let at_return = run.lock().unwrap().clone().unwrap()["metadata"][RECEIPT_KEY].clone();
        assert_eq!(at_return["status"], "died");
        // Long enough for the held write to reach the record.
        tokio::time::sleep(hold + std::time::Duration::from_secs(1)).await;
        let later = run.lock().unwrap().clone().unwrap();
        let receipt = &later["metadata"][RECEIPT_KEY];
        assert_eq!(
            receipt["status"], "died",
            "after the late pid write landed the receipt reads {}, building reads {}",
            receipt["status"], later["steps"][2]["metadata"]["result"]
        );
        assert_eq!(receipt, &at_return, "the ending is the receipt, untouched");
        // The control: the late write DID land, so the case was run.
        let launched = world.captured("pid").trim().to_string();
        assert_eq!(
            later["metadata"][PROCESS_KEY]["pid"].to_string(),
            launched,
            "the held write was applied after the ending"
        );
        assert_eq!(later["steps"][2]["metadata"]["result"], "died");
    }

    /// N19 (review b149c579), the first half. A VERB WHOSE STDERR HAS
    /// GONE STILL ENDS THE RUN. With the reader of its stderr gone and
    /// then SIGHUP — what a closed terminal does — the group was stopped
    /// but the next `eprintln!` panicked: the verb exited 101 and
    /// Building's `died` was never written.
    #[test]
    fn a_verb_whose_stderr_has_gone_still_ends_the_run_died() {
        let tell = boss_testing::scratch_dir("dispatch-launch-no-stderr");
        let (reader, writer) = std::io::pipe().unwrap();
        let (mut verb, tell, root) = verb_running_saying_to(tell, Stdio::from(writer));
        let launched = std::fs::read_to_string(root.join("home/capture/pid")).unwrap();
        assert!(alive(&launched), "the stub is running");
        // Nobody reads the verb's stderr from here: its next write fails.
        drop(reader);
        let pid = rustix::process::Pid::from_raw(verb.id() as i32).unwrap();
        rustix::process::kill_process(pid, rustix::process::Signal::HUP).unwrap();
        let status = verb.wait().unwrap();
        assert!(
            gone_within(&launched, std::time::Duration::from_secs(5)),
            "the launched process outlived the verb"
        );
        let ended: Value = serde_json::from_str(
            &std::fs::read_to_string(tell.join("ended")).unwrap_or_else(|_| {
                panic!("the verb ended ({status}) without ending the run: a write to its stderr panicked")
            }),
        )
        .unwrap();
        let building = &ended["run"]["steps"][2];
        assert_eq!(building["status"], "completed");
        assert_eq!(building["metadata"]["result"], "died");
        assert!(
            building["metadata"][DIED_KEY]["why"]
                .as_str()
                .unwrap()
                .contains("SIGHUP")
        );
        assert_eq!(ended["run"]["metadata"][RECEIPT_KEY]["status"], "died");
        assert_eq!(status.code(), Some(0), "the verb's process did not panic");
    }

    /// N19, the second half. WHATEVER UNWINDS THE LAUNCH STOPS THE GROUP.
    /// A launch that was dropped — a panic, a cancelled future — left the
    /// stop to `kill_on_drop`, which kills the ONE process spawned. For a
    /// wrapper-shaped harness (the real `codex` on PATH is a node script
    /// that spawns the native program) the wrapper died and its child ran
    /// on with no bound, on a packet that read `running`.
    #[tokio::test]
    async fn a_launch_that_is_dropped_stops_the_whole_group() {
        let world = World::new("dispatch-launch-dropped", "hang-with-child", 60_000);
        let (base, _log, _run) =
            keeping_stub(packet_without_projection(), reviewer_row(), live_agents()).await;
        let d = dispatch(&base, &world, Some("gpt-6.1-sol"), true)
            .await
            .unwrap();
        let mut launching = Box::pin(launch(&base, &world, &d));
        let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !world.home.join("capture/spinning").is_file() {
            assert!(std::time::Instant::now() < until, "the stub never started");
            tokio::select! {
                out = &mut launching => panic!("the launch ended by itself: {out:?}"),
                _ = tokio::time::sleep(std::time::Duration::from_millis(25)) => {}
            }
        }
        let (leader, child) = (world.captured("pid"), world.captured("childpid"));
        assert!(alive(&leader) && alive(&child), "both are running");
        drop(launching);
        let wait = std::time::Duration::from_secs(5);
        let (leader_gone, child_gone) = (gone_within(&leader, wait), gone_within(&child, wait));
        assert!(leader_gone, "the spawned process outlived the launch");
        assert!(
            child_gone,
            "what the spawned process started outlived the launch"
        );
    }

    /// N21 (review b149c579). A transcript that is FOUND and names no
    /// model was explained by its whole path — under the harness's home,
    /// the value of a variable of the launching session — in
    /// `observed_model_unread` and in `model`. Those two are sentences;
    /// they name the variable, as the missing-transcript sentence does
    /// (N12). `launch.transcript` is the one key that IS the path: the
    /// report reads the file by it.
    #[tokio::test]
    async fn a_transcript_that_names_no_model_is_explained_without_the_homes_path() {
        let world = World::new("dispatch-launch-no-model", "ok", 20_000);
        let without: String = rollout("gpt-6-astra")
            .lines()
            .filter(|l| !l.contains("turn_context"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(world.home.join("fixture.jsonl"), without).unwrap();
        let (base, _log, _run) =
            keeping_stub(packet_without_projection(), reviewer_row(), live_agents()).await;
        let d = dispatch(&base, &world, Some("gpt-6.1-sol"), true)
            .await
            .unwrap();
        let receipt = launch(&base, &world, &d).await.unwrap();
        assert_eq!(receipt["observed_model"], Value::Null);
        let home = world.home.to_str().unwrap();
        assert!(
            receipt["transcript"].as_str().unwrap().starts_with(home),
            "the fixture: the transcript was found, under the home"
        );
        for key in ["observed_model_unread", "model"] {
            let said = receipt[key].as_str().unwrap();
            assert!(said.contains("names no model"), "{key}: {said}");
            assert!(
                !said.contains(home),
                "{key} carries the home's path: {said}"
            );
            assert!(
                said.contains(
                    "sessions/2026/10/07/rollout-2026-10-07T10-00-00-01a1-thread.jsonl under \
                     $CODEX_HOME (else $HOME/.codex)"
                ),
                "{key} names the file by its variable: {said}"
            );
        }
    }

    /// Every claim made on the dispatched PACKET's steps, as sent — the
    /// query string included, because who the claim is for rides there.
    fn packet_claims(log: &crate::dispatch::wire_tests::Log) -> Vec<String> {
        log.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, p, _)| {
                m == "POST"
                    && p.starts_with(&format!("/api/jobs/{PACKET}/steps/"))
                    && p.split('?')
                        .next()
                        .is_some_and(|path| path.ends_with("/claim"))
            })
            .map(|(_, p, _)| p.clone())
            .collect()
    }

    fn writes_of(log: &crate::dispatch::wire_tests::Log) -> usize {
        log.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, _, _)| m != "GET")
            .count()
    }

    /// A launch that cannot be made is refused BEFORE the claim, by what
    /// the registry and the harness files say — and nothing is written.
    #[tokio::test]
    async fn a_launch_that_cannot_be_made_is_refused_before_anything_is_claimed() {
        // The model the block declares is the coordinator's own harness's.
        let world = World::new("dispatch-launch-host-tool", "ok", 20_000);
        let (base, log, _) =
            keeping_stub(packet_without_projection(), reviewer_row(), live_agents()).await;
        let refused = dispatch(&base, &world, None, true)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            refused.contains("harness claude-code") && refused.contains("no process for --launch"),
            "{refused}"
        );
        assert_eq!(writes_of(&log), 0);

        // A profile the harness has no sandbox for (a builder's block).
        let builder = crate::dispatch::wire_tests::row_with_block();
        let (base, log, _) =
            keeping_stub(packet_without_projection(), builder, live_agents()).await;
        let refused = dispatch(&base, &world, Some("gpt-6.1-sol"), true)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            refused.contains("no sandbox for profile `builder`"),
            "{refused}"
        );
        assert_eq!(writes_of(&log), 0);

        // A model no registry row runs.
        let (base, log, _) =
            keeping_stub(packet_without_projection(), reviewer_row(), live_agents()).await;
        let refused = dispatch(&base, &world, Some("gemini-2.5-pro"), true)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            refused.contains("no registered agent runs `gemini-2.5-pro`"),
            "{refused}"
        );
        assert_eq!(writes_of(&log), 0);

        // No executable, and a directory that already exists.
        let mut bare = World::new("dispatch-launch-no-exe", "ok", 20_000);
        bare.request.search_path = Some(bare.root.join("nowhere").into_os_string());
        let refused = dispatch(&base, &bare, Some("gpt-6.1-sol"), true)
            .await
            .unwrap_err()
            .to_string();
        assert!(refused.contains("no executable `codex`"), "{refused}");
        std::fs::create_dir_all(&world.request.dir).unwrap();
        let refused = dispatch(&base, &world, Some("gpt-6.1-sol"), true)
            .await
            .unwrap_err()
            .to_string();
        assert!(refused.contains("already exists"), "{refused}");
        assert_eq!(writes_of(&log), 0);
        assert!(!world.home.join("capture/argv").exists(), "no process ran");
    }

    /// Without `--launch` the same dispatch is the one it always was:
    /// the run is the dispatching actor's, no harness is named on it, and
    /// nothing is spawned — whatever model the flag names.
    #[tokio::test]
    async fn without_launch_the_run_is_the_dispatching_actors_and_nothing_is_spawned() {
        let world = World::new("dispatch-no-launch", "ok", 20_000);
        let (base, log, run) =
            keeping_stub(packet_without_projection(), reviewer_row(), live_agents()).await;
        let d = dispatch(&base, &world, Some("gpt-6.1-sol"), false)
            .await
            .unwrap();
        assert!(d.plan.is_none());
        // N2's other half: a printed prompt's step is the dispatching
        // actor's own claim, naming no one else.
        assert_eq!(
            packet_claims(&log),
            [format!("/api/jobs/{PACKET}/steps/s-build/claim")]
        );
        let filed = run.lock().unwrap().clone().unwrap();
        assert_eq!(filed["metadata"]["agent"], "claude@algedonic.dev");
        assert!(filed["metadata"].get("harness").is_none());
        assert!(filed["metadata"].get(RECEIPT_KEY).is_none());
        assert_eq!(filed["steps"][2]["assignee_id"], "agent-claude");
        assert!(!world.request.dir.exists());
        assert!(!world.home.join("capture/argv").exists());
    }
}
