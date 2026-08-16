//! `boss train` — drive the pr-train Workflow.
//!
//! Ported from `infra/train/conductor.py` (directive 26d61c97: no
//! python runs the BOSS system — the conductor's logic now lives in
//! the same `boss` binary the box already ships). The semantics, the
//! journal lines, and the incident history below are the python
//! conductor's, carried over intact.
//!
//! The train is the cadence: changes accumulate on branches with their
//! ship-a-change Jobs parked at `review`, and twice a day this runs and
//! does the batching a person used to do by discipline. Two phases:
//!
//!  1. RECONCILE — for every OPEN pr-train Job, record whatever evidence
//!     arrived since the last run: the CI verdict (polled from the
//!     forge), the merge (observed, never assumed), and the deploys that
//!     carried the merge out. Steps close only when the conductor holds
//!     the evidence in hand; a train whose PR nobody merged just stays
//!     open, visibly. Once a train has arrived, the sweep deletes each
//!     landed car's branch from the forge — on the job record's
//!     evidence, because squash-merged trains leave no git ancestry to
//!     prove a landing (see `deletable_branches`), and only while the
//!     branch still points at the head that boarded (`sweep_guard`).
//!
//!  2. BOARD — open this window's train Job, collect the ship-a-change
//!     Jobs that are ready (review step ready/active, a branch pushed to
//!     the fork, not already on a train), assemble one train branch by
//!     merging each on top of origin/main, push it, open ONE batched PR.
//!     A branch that does not merge cleanly is skipped, named on the Job,
//!     and left for the next train. An empty window cancels the train via
//!     the `job.metadata.empty` marker rather than pretending.
//!
//! Two trees, deliberately:
//!   - assembly happens in a dedicated clone (BOSS_TRAIN_HOME/repo) —
//!     never in the dev working tree, which may hold a session's
//!     half-built work;
//!   - deploys run from the dev tree (/opt/boss) only when it is clean
//!     and on main; otherwise the deploy is left pending with the reason
//!     recorded, and the next run retries.
//!
//! Talks to jobs-api directly with an actor header (the gateway strips
//! inbound identity, same as boss-step.sh). Steps are addressed by
//! `spec_slug` with a title fallback for steps that predate the column.

use std::collections::BTreeSet;
use std::fs::{self, File, TryLockError};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use chrono::{DateTime, Timelike, Utc};
use reqwest::Method;
use serde_json::{Map, Value, json};

const ACTOR: &str = "automation:train-conductor";

pub(crate) fn boss_user() -> String {
    json!({
        "id": ACTOR, "role": "platform-admin", "access_tier": "operator",
        "territory_account_ids": [], "direct_report_ids": [], "department": "platform",
    })
    .to_string()
}

/// Which slice of the conductor to run. `Run` is the timer entry
/// (reconcile + board); the others are the standalone verbs the
/// python argv flags (`--preflight`, `--reconcile-only`) selected.
/// `Cancel` is the operator's judgment call on a train that will not
/// arrive — close the PR unmerged, release the cars, record why.
pub enum Phase {
    Preflight,
    Reconcile,
    Board,
    Run,
    Cancel { handle: String, reason: String },
}

pub(crate) fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

struct Config {
    jobs: String,
    gh_repo: String,
    head_owner: String,
    fork_url: String,
    upstream_url: String,
    home: String,
    clone: String,
    deploy_tree: String,
    /// The train protocol revision (directive 27ab7680): under the
    /// forge, CI-green trains merge themselves — GitHub was a 10-hour
    /// permission wall on an all-green train, and the human wall in
    /// this protocol is the car review at parking, not a mechanical
    /// click at landing.
    auto_merge: bool,
    /// The drift sentinel's deliberate escape hatch
    /// (BOSS_TRAIN_ALLOW_LOCAL_JOBS=1): accept a loopback jobs URL.
    /// Test harnesses and demo boxes only — on a real box the jobs
    /// system of record lives elsewhere (incident c4b4a6b0).
    allow_local_jobs: bool,
    /// Hours without a step completion before an open train counts
    /// stalled (BOSS_TRAIN_STALL_HOURS, default 6). An env knob for
    /// now — this threshold belongs in the cadence_rules registry as
    /// protocol data; follow-up, not this change.
    stall_hours: i64,
    /// Hours the PR may sit without CI producing ANY verdict before the
    /// conductor says so (BOSS_TRAIN_CI_HOURS, default 2). David's
    /// number, 2026-08-15: roughly twice the measured p90 of pr->ci.
    ci_hours: i64,
    /// Release a red train's consist automatically once it has stalled
    /// (BOSS_TRAIN_AUTO_CANCEL, default ON — set to `0` to disable).
    /// On by default because the failure it prevents is a pipeline that
    /// stops at the first red and stays stopped until a human looks;
    /// the kill switch exists so an operator debugging a consist can
    /// keep it on the rails without editing code.
    auto_cancel: bool,
    dry: bool,
}

impl Config {
    fn from_env(dry: bool) -> Self {
        let gh_repo = env_or("BOSS_TRAIN_GH_REPO", "algedonic-dev/boss");
        let home = env_or("BOSS_TRAIN_HOME", "/var/lib/boss-train");
        Config {
            jobs: env_or("BOSS_JOBS_URL", "http://127.0.0.1:7900"),
            head_owner: env_or("BOSS_TRAIN_HEAD_OWNER", "dauld"),
            fork_url: env_or(
                "BOSS_TRAIN_FORK_URL",
                "https://github.com/dauld/boss-fork.git",
            ),
            upstream_url: env_or(
                "BOSS_TRAIN_UPSTREAM_URL",
                &format!("https://github.com/{gh_repo}.git"),
            ),
            clone: format!("{home}/repo"),
            deploy_tree: env_or("BOSS_TRAIN_DEPLOY_TREE", "/opt/boss"),
            auto_merge: std::env::var("BOSS_TRAIN_AUTO_MERGE").as_deref() == Ok("1"),
            allow_local_jobs: std::env::var("BOSS_TRAIN_ALLOW_LOCAL_JOBS").as_deref() == Ok("1"),
            stall_hours: env_or("BOSS_TRAIN_STALL_HOURS", "6").parse().unwrap_or(6),
            ci_hours: env_or("BOSS_TRAIN_CI_HOURS", "2").parse().unwrap_or(2),
            auto_cancel: std::env::var("BOSS_TRAIN_AUTO_CANCEL").as_deref() != Ok("0"),
            gh_repo,
            home,
            dry,
        }
    }
}

fn log(msg: impl std::fmt::Display) {
    println!("conductor: {msg}");
}

/// Run a command capturing output; error on non-zero exit with the
/// same message shape the python `sh()` raised.
fn sh_in(cwd: Option<&Path>, check: bool, args: &[&str]) -> Result<Output> {
    let mut cmd = Command::new(args[0]);
    cmd.args(&args[1..]);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let out = cmd
        .output()
        .with_context(|| format!("spawning {}", args.join(" ")))?;
    if check && !out.status.success() {
        bail!(
            "{}: rc={}\n{}",
            args.join(" "),
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(out)
}

fn sh(args: &[&str]) -> Result<Output> {
    sh_in(None, true, args)
}

fn sh_unchecked(args: &[&str]) -> Result<Output> {
    sh_in(None, false, args)
}

fn stdout_str(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

// ---------------------------------------------------------------------------
// Phase 0 — pre-flight the locomotive
//
// The 2026-08-10 18:01 window crashed before boarding: a sudo probe had
// left root-owned objects in the clone, and the conductor's fetch died
// at the moment the window opened. The consist had been rehearsed; the
// locomotive had not. Every entry (including the 10-minute reconcile,
// which is thereby the early-warning cadence) proves the clone healthy
// before touching train state, and a sick locomotive exits 3 — loud in
// the unit's status — instead of surfacing at departure time.
// ---------------------------------------------------------------------------

/// The conductor's effective uid. std exposes no geteuid, and the
/// workspace carries no libc-level dependency worth adding for one
/// call; POSIX `id -u` prints exactly this.
fn euid() -> Result<u32> {
    let out = sh(&["id", "-u"])?;
    stdout_str(&out).trim().parse().context("parsing `id -u`")
}

/// Collect files under `dir` not owned by uid `me` — the recursive
/// half of python's os.walk. A directory that refuses a read is
/// skipped (os.walk's default); a file gone before lstat is skipped
/// too — gc'd mid-walk; ownership of what remains is what matters.
fn walk_foreign(dir: &Path, me: u32, foreign: &mut Vec<PathBuf>) -> Result<()> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Ok(());
    };
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        let meta = match path.symlink_metadata() {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e).context(format!("lstat {}", path.display())),
        };
        if meta.is_dir() {
            walk_foreign(&path, me, foreign)?;
        } else if meta.uid() != me {
            foreign.push(path);
        }
    }
    Ok(())
}

/// Host of an http(s) URL — scheme, userinfo, port, and path all
/// stripped. Enough to ask "is this loopback?" without a URL crate.
fn url_host(url: &str) -> &str {
    let rest = url.split("://").nth(1).unwrap_or(url);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority.rsplit('@').next().unwrap_or_default();
    match host.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or_default(),
        None => host.split(':').next().unwrap_or_default(),
    }
}

/// The drift sentinel (split-brain incident c4b4a6b0): BOSS_JOBS_URL
/// defaulted to localhost on a cutover box and the conductor silently
/// booked a whole window's trains on the wrong instance. A loopback
/// jobs URL is a preflight problem unless the box declares that a
/// local jobs-api is the point — BOSS_TRAIN_ALLOW_LOCAL_JOBS=1, set
/// deliberately by test harnesses and demo boxes.
pub(crate) fn local_jobs_problem(jobs_url: &str, allow_local: bool) -> Option<String> {
    if allow_local {
        return None;
    }
    let host = url_host(jobs_url);
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    loopback.then(|| {
        format!(
            "BOSS_JOBS_URL resolves to loopback ({jobs_url}) — bookkeeping must target \
             the jobs system of record, not this box (split-brain incident c4b4a6b0); \
             set BOSS_TRAIN_ALLOW_LOCAL_JOBS=1 only where a local jobs-api is the point"
        )
    })
}

/// Return the list of problems; empty means the locomotive is fit.
fn preflight(cfg: &Config) -> Result<Vec<String>> {
    let mut problems = Vec::new();
    // The drift sentinel runs first, clone or no clone: a conductor
    // whose bookkeeping would land on this box instead of the system
    // of record must not pull at all.
    if let Some(p) = local_jobs_problem(&cfg.jobs, cfg.allow_local_jobs) {
        problems.push(p);
    }
    // The invariant is OWNERSHIP, not uid zero: the conductor must run
    // as the clone's owner. The original flat refuse-root check said
    // the same thing only on the box where the service user is not
    // root — in a CI container every process IS root and the fixture
    // clone is root-owned, which is perfectly consistent. The
    // foreign-owned walk below enforces the real rule in both worlds:
    // root over the service user's clone still fails (every object is
    // foreign to euid 0), and the poisoning incident this guards
    // against stays guarded.
    let git_dir = Path::new(&cfg.clone).join(".git");
    if !git_dir.is_dir() {
        log("preflight: no clone yet — first boarding will create it");
        return Ok(problems);
    }
    let me = euid()?;
    let mut foreign = Vec::new();
    walk_foreign(&git_dir, me, &mut foreign)?;
    if !foreign.is_empty() {
        let shown = foreign
            .iter()
            .take(3)
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        problems.push(format!(
            "{} object(s) in the clone not owned by uid {me} (e.g. {shown}) — \
             a foreign-uid run has poisoned {}",
            foreign.len(),
            cfg.clone
        ));
    }
    for remote in ["origin", "fork"] {
        let r = sh_unchecked(&[
            "git",
            "-C",
            &cfg.clone,
            "fetch",
            remote,
            "--prune",
            "--dry-run",
        ])?;
        if !r.status.success() {
            let stderr = String::from_utf8_lossy(&r.stderr);
            let stderr = stderr.trim();
            let detail = if stderr.is_empty() {
                format!("rc={}", r.status.code().unwrap_or(-1))
            } else {
                stderr.lines().last().unwrap_or_default().to_string()
            };
            problems.push(format!("dry fetch of {remote} failed: {detail}"));
        }
    }
    Ok(problems)
}

// ---------------------------------------------------------------------------
// The jobs-API blip guard
//
// The cluster is the system of record, and it rolls. Twice on
// 2026-08-13 a reconcile hit `Connection refused` to the jobs API
// mid-converge and returned rc=1 for the whole verb — right to refuse
// to act blind, needlessly brittle about an outage that lasted
// seconds (the cadence loop's dock probe held the queue-depth rules
// for that tick on the same blip). A bounded retry covers the roll.
//
// Two rules keep it from papering over anything real: a 4xx is an
// ANSWER and is never retried, and every retry journals one line, so
// blips stay measurable instead of invisible.
// ---------------------------------------------------------------------------

/// What a failed jobs-API attempt was. The classifier reads this and
/// nothing else — pure, and pinned by tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Failure {
    /// The connection never established — refused, DNS, TLS. Proof
    /// that the request did not reach the system of record.
    Connect,
    /// A timeout, or a response that died mid-body: nothing usable
    /// came back, and whether the write happened is UNKNOWN.
    Ambiguous,
    /// The jobs API answered, with this status.
    Http(u16),
    /// The answer arrived and was unusable — an unparseable body.
    Malformed,
}

/// Retry, or surface? Two rules, and the second is the one that keeps
/// the retry honest:
///
///   - a 4xx is an ANSWER (a 422 is the SoR saying no, and asking the
///     same question three times does not change it); only transport
///     failures and 5xx are blips;
///   - a blip that leaves the write AMBIGUOUS may only be re-sent when
///     the call is idempotent. Re-POSTing an ambiguous create is how
///     one blip becomes two train Jobs. A refused connection is not
///     ambiguous — nothing was received — so anything may go again,
///     which is exactly the production case this exists for.
pub(crate) fn retryable(method: &Method, failure: &Failure) -> bool {
    let idempotent = matches!(
        *method,
        Method::GET | Method::PUT | Method::DELETE | Method::HEAD
    );
    match failure {
        Failure::Connect => true,
        Failure::Ambiguous => idempotent,
        Failure::Http(status) => idempotent && (500..600).contains(status),
        Failure::Malformed => false,
    }
}

/// A reqwest error, classified. Connect / timeout / mid-flight body
/// failures are the blips a rolling SoR produces; a builder or
/// redirect error is a misconfiguration, and retrying one just burns
/// the window three times over.
fn classify_transport(e: &reqwest::Error) -> Failure {
    if e.is_connect() {
        Failure::Connect
    } else if e.is_timeout() || e.is_request() || e.is_body() {
        Failure::Ambiguous
    } else {
        Failure::Malformed
    }
}

/// A jobs-API call that did not succeed: what it was (for the
/// classifier) and the error to surface once the retries run out.
pub(crate) struct ApiFailure {
    pub(crate) kind: Failure,
    pub(crate) cause: anyhow::Error,
}

impl ApiFailure {
    /// A reqwest failure — classified by what reqwest says went wrong.
    pub(crate) fn transport(e: reqwest::Error, context: String) -> Self {
        ApiFailure {
            kind: classify_transport(&e),
            cause: anyhow::Error::new(e).context(context),
        }
    }
}

/// The bounded retry: how many attempts in total, and the first wait
/// between them (each further wait doubles).
#[derive(Debug, Clone, Copy)]
pub(crate) struct RetryPolicy {
    pub(crate) attempts: u32,
    pub(crate) base: Duration,
}

/// The jobs-API policy: 3 attempts, 2s then 4s. A pod roll is over
/// inside that budget, and a jobs API still refusing after it is an
/// outage the verb should surface rather than paper over.
pub(crate) const JOBS_API_RETRY: RetryPolicy = RetryPolicy {
    attempts: 3,
    base: Duration::from_secs(2),
};

impl RetryPolicy {
    /// The wait before attempt `n + 1`, doubling from `base`.
    pub(crate) fn backoff(&self, attempt: u32) -> Duration {
        self.base * 2u32.pow(attempt.saturating_sub(1).min(16))
    }

    /// The same decisions with no waiting — the tests' policy, so the
    /// retry semantics get pinned without spending the backoff.
    #[cfg(test)]
    pub(crate) const fn immediate(attempts: u32) -> Self {
        RetryPolicy {
            attempts,
            base: Duration::ZERO,
        }
    }
}

/// Character budget for a blip's cause in the journal.
const BLIP_CAUSE_BUDGET: usize = 80;

/// The one-line cause of a blip: the INNERMOST error, which is where
/// the fact lives ("Connection refused (os error 61)") — the layers
/// above it just repeat the url the journal line already implies.
pub(crate) fn short_cause(err: &anyhow::Error) -> String {
    let innermost = err
        .chain()
        .last()
        .map(|c| c.to_string())
        .unwrap_or_default();
    let line = innermost.lines().next().unwrap_or_default().trim();
    if line.chars().count() <= BLIP_CAUSE_BUDGET {
        return line.to_string();
    }
    format!(
        "{}…",
        line.chars().take(BLIP_CAUSE_BUDGET).collect::<String>()
    )
}

/// Run `op` until it succeeds, its failure turns out to be an answer
/// rather than a blip, or the attempt budget runs out. Every retry
/// journals one line through `journal` — the caller's idiom, so the
/// conductor's blips read `conductor: ` and the cadence loop's read
/// `cadence: `.
pub(crate) async fn retrying<T, F, Fut>(
    policy: &RetryPolicy,
    method: &Method,
    journal: &dyn Fn(&str),
    mut op: F,
) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = std::result::Result<T, ApiFailure>>,
{
    let mut attempt = 1u32;
    loop {
        let failure = match op().await {
            Ok(v) => return Ok(v),
            Err(f) => f,
        };
        if attempt >= policy.attempts || !retryable(method, &failure.kind) {
            return Err(failure.cause);
        }
        journal(&format!(
            "jobs API blip ({attempt}/{}): {}",
            policy.attempts,
            short_cause(&failure.cause)
        ));
        tokio::time::sleep(policy.backoff(attempt)).await;
        attempt += 1;
    }
}

// ---------------------------------------------------------------------------
// jobs-api helpers
// ---------------------------------------------------------------------------

/// The list body, whether or not the endpoint wrapped it in
/// `{"data": [...]}`.
pub(crate) fn rows(resp: Option<Value>) -> Result<Vec<Value>> {
    let resp = resp.ok_or_else(|| anyhow!("empty response for a list call"))?;
    let list = match resp {
        Value::Object(mut o) if o.contains_key("data") => o.remove("data").unwrap_or(Value::Null),
        other => other,
    };
    match list {
        Value::Array(v) => Ok(v),
        other => bail!("expected a job list, got: {other}"),
    }
}

fn find_step<'a>(job: &'a Value, slug: &str, title: &str) -> Option<&'a Value> {
    let steps = job.get("steps").and_then(Value::as_array)?;
    steps
        .iter()
        .find(|s| s.get("spec_slug").and_then(Value::as_str) == Some(slug))
        .or_else(|| {
            steps
                .iter()
                .find(|s| s.get("title").and_then(Value::as_str) == Some(title))
        })
}

fn step_done(step: Option<&Value>) -> bool {
    step.and_then(|s| s.get("status"))
        .and_then(Value::as_str)
        .is_some_and(|s| s == "completed" || s == "skipped")
}

/// `spec_slug or title` — the label the python conductor logged.
fn step_label(step: &Value) -> String {
    step.get("spec_slug")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .or_else(|| step.get("title").and_then(Value::as_str))
        .unwrap_or("?")
        .to_string()
}

fn id8(id: &str) -> String {
    id.chars().take(8).collect()
}

fn job_id(job: &Value) -> Result<&str> {
    job.get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("job without an id"))
}

/// Python truthiness for the metadata fields the conductor reads —
/// absent, null, "", 0 and empty containers are all "not set".
pub(crate) fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

fn metadata_map(v: &Value) -> Map<String, Value> {
    match v.get("metadata") {
        Some(Value::Object(m)) => m.clone(),
        _ => Map::new(),
    }
}

/// The overlay half of `merge_job_metadata`, pure: jobs-api's PATCH
/// semantics stop at the top level — a PUT replaces `metadata`
/// wholesale — so every update must carry the record's existing keys
/// forward. A `Value::Null` value REMOVES the key: how a boarding car
/// sheds a stale `skip_reason` instead of carrying "" forever.
pub(crate) fn overlay_metadata(container: &Value, kv: Vec<(&str, Value)>) -> Map<String, Value> {
    let mut md = metadata_map(container);
    for (k, v) in kv {
        match v {
            Value::Null => {
                md.remove(k);
            }
            v => {
                md.insert(k.to_string(), v);
            }
        }
    }
    md
}

/// Character budget for the file list in a conflict skip reason. The
/// reason lands on the car Job's `metadata.skip_reason`, which the
/// yard's PacketCard renders as a chip ("LEFT BEHIND — <reason>") —
/// past this budget the list truncates to a count.
const SKIP_REASON_FILE_BUDGET: usize = 96;

/// The skip reason for a car whose branch would not merge onto this
/// window's train: names the conflicted files, truncated to stay
/// chip-sized. At least one file always shows.
/// Replay `branch`'s own commits on top of the consist as it stands,
/// returning a ref that merges cleanly — or `None` when the car has a
/// conflict a rebase cannot resolve either.
///
/// Rebases from the merge-base, so only the car's OWN work is replayed:
/// anything it carries that already reached main (the squash-merge
/// case) is dropped by git as an applied patch rather than re-applied
/// as a conflict.
///
/// Leaves the clone on `train_branch` whatever happens — a caller
/// mid-consist must not be handed a detached HEAD or a half-finished
/// rebase, and the next car in the loop merges into whatever branch it
/// finds itself on.
fn rerail_onto_consist(clone: &str, train_branch: &str, branch: &str) -> Result<Option<String>> {
    let scratch = "boss-train-rerail";
    let car = format!("fork/{branch}");
    let base_out = sh_unchecked(&["git", "-C", clone, "merge-base", train_branch, &car])?;
    if !base_out.status.success() {
        return Ok(None);
    }
    let base = stdout_str(&base_out).trim().to_string();
    if base.is_empty() {
        return Ok(None);
    }
    sh_unchecked(&["git", "-C", clone, "checkout", "-q", "-B", scratch, &car])?;
    let rebase = sh_unchecked(&[
        "git",
        "-C",
        clone,
        "rebase",
        "--onto",
        train_branch,
        &base,
        scratch,
    ])?;
    if !rebase.status.success() {
        sh_unchecked(&["git", "-C", clone, "rebase", "--abort"])?;
        sh_unchecked(&["git", "-C", clone, "checkout", "-q", train_branch])?;
        return Ok(None);
    }
    sh_unchecked(&["git", "-C", clone, "checkout", "-q", train_branch])?;
    Ok(Some(scratch.to_string()))
}

pub(crate) fn skip_reason_conflict(conflicted: &[String]) -> String {
    if conflicted.is_empty() {
        return "conflict: unresolved (merge died before conflict markers)".to_string();
    }
    let mut shown = 0usize;
    let mut len = 0usize;
    for f in conflicted {
        let add = f.len() + if shown == 0 { 0 } else { 2 };
        if shown > 0 && len + add > SKIP_REASON_FILE_BUDGET {
            break;
        }
        shown += 1;
        len += add;
    }
    let files = conflicted[..shown].join(", ");
    match conflicted.len() - shown {
        0 => format!("conflict: {files}"),
        hidden => format!("conflict: {files} +{hidden} more"),
    }
}

/// The skip reason for a car parked at review whose branch was never
/// pushed to the fork.
pub(crate) fn skip_reason_branch_missing(branch: &str) -> String {
    format!("branch {branch} not on fork")
}

/// Is this ship-a-change Job a parked ready car — at review with a
/// branch declared and not already on a train? ONE definition, shared
/// by the boarding collector below and the cadence loop's dock-depth
/// probe (`boss train cadence`, the queue-depth basis): the count that
/// fires a boarding must be the same predicate boarding itself uses.
/// (The fork-branch existence check stays in `candidates` — it needs
/// the clone, and a car whose branch was never pushed still occupies
/// the dock from the author's point of view.)
pub(crate) fn parked_ready(job: &Value) -> bool {
    let md = job.get("metadata").cloned().unwrap_or(Value::Null);
    let branch = md.get("branch").and_then(Value::as_str).unwrap_or_default();
    if branch.is_empty() || truthy(md.get("train")) || branch.starts_with("train/") {
        return false;
    }
    let review = find_step(job, "review", "Open for review");
    let review_status = review
        .and_then(|s| s.get("status"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    review_status == "ready" || review_status == "active"
}

/// The branch-sweep decision at arrival (protocol decision, David):
/// train PRs squash-merge, so git ancestry can never prove a car's
/// content landed — the JOB RECORD is the proof. Given the cars a
/// landed train boarded and the branches still-open cars name, a
/// car's branch is deletable iff:
///   - the car's own bookkeeping completed: closed with the `merged`
///     outcome (an abandoned car closes too, but its branch holds
///     unmerged work — never touch it);
///   - the branch is named and is not `main`;
///   - no still-open car rides the same branch (a follow-up car's
///     claim keeps it alive).
/// Two landed cars naming one branch delete it once. Pure — the
/// forge call and the journal line belong to the caller.
pub(crate) fn deletable_branches(
    boarded_cars: &[Value],
    open_branches: &BTreeSet<String>,
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for car in boarded_cars {
        let Some(cid) = car.get("id").and_then(Value::as_str) else {
            continue;
        };
        let md = car.get("metadata");
        let branch = md
            .and_then(|m| m.get("branch"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let landed = car.get("status").and_then(Value::as_str) == Some("closed")
            && md.and_then(|m| m.get("outcome")).and_then(Value::as_str) == Some("merged");
        if branch.is_empty()
            || branch == "main"
            || !landed
            || open_branches.contains(branch)
            || out.iter().any(|(b, _)| b == branch)
        {
            continue;
        }
        out.push((branch.to_string(), cid.to_string()));
    }
    out
}

/// The branch head recorded when this car boarded — stamped by the
/// assembly onto the car Job in the same update that stamps `train`
/// (see `board`). Absent or empty reads as no stamp at all.
pub(crate) fn boarded_head(car: &Value) -> Option<&str> {
    car.get("metadata")?
        .get("boarded_head")?
        .as_str()
        .filter(|s| !s.is_empty())
}

/// The sweep's second question, and the answers to it.
///
/// `deletable_branches` asks whether the job record proves the car's
/// CONTENT landed. It does not — it cannot — prove the branch still
/// holds only that content. Car 23923b40's known_gap is what the gap
/// costs: `fix/conductor-hardening` boarded at fc55e4d, two more
/// commits were pushed to the branch AFTER boarding, the train landed
/// carrying only the boarded ones, and the sweep deleted the branch
/// on a job record that was entirely correct. The unmerged commits
/// went with it.
///
/// So the sweep now deletes only what it can prove it carried: the
/// head recorded at ASSEMBLY time must still be the branch's head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SweepGuard {
    /// The branch still points at exactly what boarded.
    Delete,
    /// Commits arrived after boarding — the train never carried them,
    /// and they live nowhere else.
    Moved { recorded: String, current: String },
    /// The branch EXISTS and no head is on the record (a car that
    /// boarded before the conductor recorded one). An unknown head is
    /// not evidence: the cost of keeping a stale branch is a stale
    /// branch; the cost of deleting a moved one is lost work.
    NoRecord,
    /// The branch is not on the forge — nothing left to sweep.
    Gone,
}

/// The head-guard decision, pure. Both shas are full 40-char heads —
/// the assembly records what `git rev-parse` merged, the guard reads
/// what the forge names now — so equality is the whole test.
///
/// The forge's answer is read FIRST, and an absent branch settles the
/// question whatever the record says. Ordering the record first
/// conflates "we cannot vouch for this branch" with "there is no such
/// branch", and the second is not a finding: nothing to delete,
/// nothing to rescue, nothing an operator can do. Job 1bd1fb3d is the
/// bill — every car that boarded before this guard existed has
/// neither a recorded head nor a surviving branch, so the record-first
/// order made each one a `NoRecord` line on every reconcile, forever.
///
/// The reorder is free: `branch_head` was already called
/// unconditionally for every deletable branch, so the sweep asks the
/// forge exactly as often as it did before.
pub(crate) fn sweep_guard(recorded: Option<&str>, current: Option<&str>) -> SweepGuard {
    let recorded = recorded.filter(|s| !s.is_empty());
    let current = current.filter(|s| !s.is_empty());
    match (recorded, current) {
        (_, None) => SweepGuard::Gone,
        (None, Some(_)) => SweepGuard::NoRecord,
        (Some(r), Some(c)) if r == c => SweepGuard::Delete,
        (Some(r), Some(c)) => SweepGuard::Moved {
            recorded: r.to_string(),
            current: c.to_string(),
        },
    }
}

/// The journal line a guard verdict earns — `None` when it earns
/// none. Pure, so "what does the operator hear" is a decision with a
/// test rather than a shape buried in the sweep loop.
///
/// The sweep's journal is an operator surface, and a line belongs
/// there only when a human could act on it. `Gone` is not that: the
/// branch is not on the forge, so there is nothing to delete and
/// nothing to rescue. Job 1bd1fb3d is the cost of getting this wrong
/// — every car that boarded before the head guard existed has no
/// recorded head and no surviving branch, and narrating that pair put
/// dozens of lines in every reconcile, forever, about branches swept
/// by hand hours earlier.
///
/// `Delete` is silent here too, but for the opposite reason: the
/// caller does the deleting and is the only one who knows whether it
/// was a dry run, a deletion, or a race lost to something faster.
pub(crate) fn sweep_note(guard: &SweepGuard, branch: &str, car: &str) -> Option<String> {
    match guard {
        SweepGuard::Gone | SweepGuard::Delete => None,
        SweepGuard::NoRecord => Some(format!(
            "branch {branch} has no boarded head on record — not deleting (car {} landed)",
            id8(car)
        )),
        SweepGuard::Moved { recorded, current } => {
            Some(branch_moved_line(branch, recorded, current))
        }
    }
}

/// The line the sweep journals when a branch outgrew its boarding —
/// operator surface, and the only notice that unmerged commits are
/// sitting on a branch the train did not carry.
pub(crate) fn branch_moved_line(branch: &str, recorded: &str, current: &str) -> String {
    format!(
        "branch {branch} moved since boarding ({} -> {}) — not deleting",
        id8(recorded),
        id8(current)
    )
}

/// A train's sweep is settled once every boarded car has reached a
/// terminal status — each branch is then deleted, deliberately kept
/// (main / a still-open car's claim), or the car never landed and
/// its branch outlives the train. A car still open keeps the train
/// on the sweep list for the next reconcile.
pub(crate) fn sweep_settled(boarded_cars: &[Value]) -> bool {
    boarded_cars.iter().all(|car| {
        matches!(
            car.get("status").and_then(Value::as_str),
            Some("closed") | Some("cancelled")
        )
    })
}

/// A step's `completed_at` evidence stamp, raw as stored. The
/// conductor stamps this on every step IT completes; steps closed by
/// other hands (the dispatcher's terminals) may not carry one.
fn step_stamp<'a>(train: &'a Value, slug: &str, title: &str) -> Option<&'a str> {
    find_step(train, slug, title)
        .and_then(|s| s.get("metadata"))
        .and_then(|m| m.get("completed_at"))
        .and_then(Value::as_str)
}

fn parse_stamp(s: Option<&str>) -> Option<DateTime<chrono::FixedOffset>> {
    s.and_then(|t| DateTime::parse_from_rfc3339(t).ok())
}

fn secs_between(
    a: Option<DateTime<chrono::FixedOffset>>,
    b: Option<DateTime<chrono::FixedOffset>>,
) -> Value {
    match (a, b) {
        (Some(a), Some(b)) => json!((b - a).num_seconds()),
        _ => Value::Null,
    }
}

/// The deployed sha out of the deploy step's summary evidence
/// (`main@<sha>; ...`). None when the summary is absent or shaped
/// differently — the report never guesses.
fn deployed_generation(summary: &str) -> Option<&str> {
    summary
        .strip_prefix("main@")
        .and_then(|rest| rest.split([';', ' ']).next())
        .filter(|sha| !sha.is_empty())
}

/// The arrival report — the landing's final structured entry, filed
/// on the `arrived` step when the sweep visits an arrived train.
/// Everything derives from evidence the job record already holds:
/// the boarded cars (consist), the board-time skips the train
/// recorded (left_behind), the deployed generation, and the timings
/// the conductor's own `completed_at` stamps make derivable. Missing
/// evidence reads as null, never a guess — `arrived_at` stays null
/// until whatever completes the outcome step stamps a time, and no
/// CI round count appears because the record does not carry one.
pub(crate) fn arrival_report(train: &Value, boarded_cars: &[Value]) -> Value {
    let consist: Vec<Value> = boarded_cars
        .iter()
        .map(|c| {
            json!({
                "car_id_short": id8(c.get("id").and_then(Value::as_str).unwrap_or("?")),
                "title": c.get("title").and_then(Value::as_str).unwrap_or_default(),
                "branch": c
                    .get("metadata")
                    .and_then(|m| m.get("branch"))
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            })
        })
        .collect();
    let left_behind = train
        .get("metadata")
        .and_then(|m| m.get("left_behind"))
        .cloned()
        .unwrap_or_else(|| json!([]));
    let generation = find_step(train, "deployed", "Deployed to the playground")
        .and_then(|s| s.get("metadata"))
        .and_then(|m| m.get("deployed"))
        .and_then(Value::as_str)
        .and_then(deployed_generation);
    let merged_sha = find_step(train, "merged", "Merged into main")
        .and_then(|s| s.get("metadata"))
        .and_then(|m| m.get("merge_ref"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let boarded = step_stamp(train, "collect", "Collect what is ready to board");
    let merged = step_stamp(train, "merged", "Merged into main");
    let deployed = step_stamp(train, "deployed", "Deployed to the playground");
    let arrived = step_stamp(train, "arrived", "Train arrived");
    let mut report = json!({
        "consist": consist,
        "left_behind": left_behind,
        "generation": generation,
        "timings": {
            "boarded_at": boarded,
            "merged_at": merged,
            "deployed_at": deployed,
            "arrived_at": arrived,
            "board_to_merge_s": secs_between(parse_stamp(boarded), parse_stamp(merged)),
            "merge_to_deploy_s": secs_between(parse_stamp(merged), parse_stamp(deployed)),
            "total_s": secs_between(parse_stamp(boarded), parse_stamp(arrived)),
        },
    });
    // The merged sha is the generation seen from the other end — a
    // short deploy sha prefixing the full merge sha is the SAME
    // commit, and repeating it would imply a divergence that is not
    // there. It appears only when genuinely distinct (or when the
    // deploy evidence is missing and it is the only sha on record).
    if let Some(m) =
        merged_sha.filter(|m| generation.is_none_or(|g| !(g.starts_with(m) || m.starts_with(g))))
    {
        report["merged_sha"] = json!(m);
    }
    report
}

/// The one-line form of the report — filed beside it as `summary`,
/// and the shape of the journal line. Reads the report, not the
/// world: unknowns print as "unknown" / "?", never as guesses.
pub(crate) fn arrival_summary(report: &Value) -> String {
    let n = report
        .get("consist")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let generation = report
        .get("generation")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let total = report
        .get("timings")
        .and_then(|t| t.get("total_s"))
        .and_then(Value::as_i64)
        .map_or_else(|| "?".to_string(), |s| s.to_string());
    format!("{n} cars; generation {generation}; total {total}s")
}

/// Is a deploy actually needed? `current_key` is the generation
/// store's live key — the 8-char short-sha release dirname
/// (infra/generation.sh); `remote_main` is the FULL 40-char sha
/// `git ls-remote` answers. Same generation iff the full sha starts
/// with the short key — exactly that direction (the live incident:
/// this pair failing the comparison re-ran a full no-op deploy every
/// 10-minute reconcile). Missing evidence on either side deploys —
/// the deploy path surfaces its own errors, and a skip must never
/// rest on absence.
pub(crate) fn deploy_needed(current_key: &str, remote_main: &str) -> bool {
    current_key.is_empty() || remote_main.is_empty() || !remote_main.starts_with(current_key)
}

/// The live generation's key — the basename of the store's `current`
/// symlink. The store layout is owned by infra/generation.sh (the
/// one definition); this reads the same BOSS_GEN_ROOT contract.
/// Empty when the box has no generation store yet.
fn current_generation_key() -> String {
    let root = env_or("BOSS_GEN_ROOT", "/usr/local/boss");
    fs::read_link(Path::new(&root).join("current"))
        .ok()
        .and_then(|t| t.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_default()
}

/// The newest `completed_at` stamp across a train's steps — when
/// progress last provably happened. None when no step carries a
/// parseable stamp.
fn newest_completion(train: &Value) -> Option<&str> {
    train
        .get("steps")
        .and_then(Value::as_array)?
        .iter()
        .filter_map(|s| {
            let raw = s
                .get("metadata")
                .and_then(|m| m.get("completed_at"))
                .and_then(Value::as_str)?;
            Some((DateTime::parse_from_rfc3339(raw).ok()?, raw))
        })
        .max_by_key(|(t, _)| *t)
        .map(|(_, raw)| raw)
}

/// The stall sentinel's decision, pure: an open train counts stalled
/// when its newest step completion is at least `threshold_hours` old,
/// and the age in whole hours comes back for the journal line. No
/// completion evidence means no basis — None, never a guess.
pub(crate) fn stall_age_hours(
    train: &Value,
    now: DateTime<Utc>,
    threshold_hours: i64,
) -> Option<i64> {
    let newest = DateTime::parse_from_rfc3339(newest_completion(train)?).ok()?;
    let age = (now.signed_duration_since(newest)).num_hours();
    (age >= threshold_hours).then_some(age)
}

/// Which boarded cars a cancelled train releases back to the dock:
/// the still-open ones. A closed or cancelled car's record is history
/// — merged or abandoned, either way not the cancel path's to touch.
pub(crate) fn releasable_cars(cars: &[Value]) -> Vec<&Value> {
    cars.iter()
        .filter(|c| c.get("status").and_then(Value::as_str) == Some("open"))
        .collect()
}

/// The auto-cancel decision, pure: should reconcile kill this train and
/// release its consist? Some(reason) or None, and the reason is what
/// lands on every released car.
///
/// A red train holds its whole consist hostage — the cars carry a
/// `train` marker so `parked_ready` no longer counts them, and the
/// conductor merges only on green, so nothing recovers on its own.
/// Overnight that is the difference between a pipeline that keeps
/// running and one that stops at the first fault. This is the reversal
/// of the older rule that only the operator may cancel (David,
/// 2026-08-15, choosing auto-cancel with a two-strike hold): raising is
/// still protocol, but an unattended pipeline has nobody to raise to.
///
/// THE VERDICT MUST BE THE LIVE ONE. `reconcile` reads it from the
/// forge each pass; the train's `ci` step keeps whatever verdict it was
/// first stamped with and is NOT re-stamped when CI re-runs. Deciding
/// from the step would cancel a train whose repair had already been
/// pushed and gone green — the exact case this is meant to rescue. A
/// re-running check reads `pending`, which is not `failing`, so a train
/// under repair is left alone.
pub(crate) fn auto_cancel_reason(
    train: &Value,
    live_verdict: &str,
    now: DateTime<Utc>,
    stall_hours: i64,
) -> Option<String> {
    if live_verdict != "failing" {
        return None;
    }
    // A merged train is not a candidate whatever its checks say — the
    // content landed and the remaining steps are bookkeeping.
    if step_done(find_step(train, "merged", "Merged into main")) {
        return None;
    }
    let age = stall_age_hours(train, now, stall_hours)?;
    Some(format!(
        "CI red and no progress for {age}h (threshold {stall_hours}h) — cars released to board a later train"
    ))
}

/// Has the CI verdict MOVED since it was last recorded?
///
/// THE BLIND SPOT THIS CLOSES. The `ci` step is completed exactly once,
/// the first time the rollup settles, and never looked at again. So the
/// verdict on a train that was repaired — pushed to, re-run, and gone
/// red a second time — is recorded nowhere and logged nowhere. On
/// 2026-08-15 train 20260815-0621 sat red for 45 minutes after a repair
/// with the system reporting nothing; it was found by querying the
/// forge by hand. The repair loop is exactly the path with no feedback,
/// which is the worst place to have none.
///
/// WHY THIS DOES NOT RE-STAMP THE STEP, which is the obvious fix and is
/// impossible: `update_step_at` freezes status, completed_on AND
/// METADATA on a terminal row, so the step's `result` cannot be
/// rewritten — and today's other lesson is not to design a path that
/// needs to un-complete a step. The train JOB's metadata is not frozen,
/// so the moving fact lives there, next to the immutable record of what
/// the verdict was when it first settled. Both are true and they are
/// different facts.
///
/// `pending` is never a change worth reporting: a re-run passes through
/// it on the way to an answer, and announcing it would make the signal
/// fire on every repair.
pub(crate) fn verdict_drift(recorded: Option<&str>, live: &str) -> Option<String> {
    let recorded = recorded?;
    if live == "pending" || live == recorded {
        return None;
    }
    Some(format!(
        "CI verdict moved {recorded} -> {live} since it was recorded"
    ))
}

/// CI has been asked and has not answered — the case `verdict_drift`
/// cannot see, because there is no verdict to compare.
///
/// Drift reports a verdict that MOVED. A runner that hangs, a job that
/// never reports, a queue nothing picks up: those produce no verdict at
/// all, so the train sits with its `ci` step incomplete and every
/// reconcile finds `pending` and says nothing. This is the backstop for
/// that, and only that.
///
/// THE THRESHOLD IS MEASURED, NOT GUESSED (David, 2026-08-15, choosing
/// 2x p90). Across 22 trains the pr->ci time had a median of ~33
/// minutes, a p90 of ~56, and a range of 10 to 169. Half again the
/// median would be ~50 minutes and would fire on six of those 22 — a
/// quarter of all trains, which is how an alert becomes furniture. Two
/// hours is roughly twice p90 and clears every train ever observed
/// except the 169-minute outlier, so when it fires it means something.
///
/// Worth recording alongside it, because it argues for LONGER trains:
/// that spread has no relationship to car count. A one-car train took
/// 63 minutes and an eight-car train took 12. The cost is per run, not
/// per car.
pub(crate) fn ci_overdue(
    train: &Value,
    now: DateTime<Utc>,
    threshold_hours: i64,
) -> Option<String> {
    // Only once the PR exists — before that there is nothing for CI to
    // answer about, and a train stuck earlier is the stall sentinel's.
    let asked = parse_stamp(step_stamp(train, "pr", "Open the batched PR"))?;
    if step_done(find_step(train, "ci", "CI verdict")) {
        return None;
    }
    let hours = now.signed_duration_since(asked).num_hours();
    (hours >= threshold_hours).then(|| {
        format!(
            "CI has not answered in {hours}h (threshold {threshold_hours}h) — no verdict, not a red one"
        )
    })
}

/// How many red trains a car may ride before boarding leaves it behind.
/// Two: one red is bad luck — the fault is usually a neighbour's — and
/// a second aboard a different consist is the car itself.
pub(crate) const MAX_RED_TRAINS: i64 = 2;

/// The boarding hold, pure: a car released from that many red trains
/// stops boarding until someone looks at it. Without this the auto
/// cancel above is a loop — the same consist re-boards, goes red, and
/// cancels again all night, burning CI and landing nothing.
pub(crate) fn car_hold_reason(car: &Value, max_reds: i64) -> Option<String> {
    let reds = car
        .get("metadata")
        .and_then(|m| m.get("red_trains"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    (reds >= max_reds)
        .then(|| format!("held after {reds} red trains — needs a look before it boards again"))
}

/// The ONE branch a cancelled train may delete: its own `train/*`
/// assembly branch (the Job's subject id). Car branches hold the
/// cars' unmerged work and are never the cancel path's to touch —
/// this filter is the pin.
pub(crate) fn train_branch_to_delete(train: &Value) -> Option<String> {
    train
        .get("subject")
        .and_then(|s| s.get("id"))
        .and_then(Value::as_str)
        .filter(|b| b.starts_with("train/"))
        .map(str::to_string)
}

/// Resolve the operator's handle — a Job id, an id prefix, or the
/// train's PR url — against the open trains. Exactly one match or an
/// error saying what went wrong; an ambiguous prefix refuses rather
/// than guessing which train to cancel.
pub(crate) fn resolve_train<'a>(trains: &'a [Value], handle: &str) -> Result<&'a Value> {
    let matches: Vec<&Value> = trains
        .iter()
        .filter(|t| {
            let id = t.get("id").and_then(Value::as_str).unwrap_or_default();
            let pr_url = find_step(t, "pr", "Open the batched PR")
                .and_then(|s| s.get("metadata"))
                .and_then(|m| m.get("pr_url"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            id == handle || (!handle.is_empty() && id.starts_with(handle)) || pr_url == handle
        })
        .collect();
    match matches.as_slice() {
        [one] => Ok(one),
        [] => bail!("no open train matches {handle:?}"),
        many => bail!(
            "{handle:?} is ambiguous — matches trains {}",
            many.iter()
                .map(|t| id8(t.get("id").and_then(Value::as_str).unwrap_or("?")))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

// The DRY log lines mirror the python conductor's dict/list reprs —
// the journal is operator surface, and the port keeps its lines.

fn py_dict(fields: &[(&str, Option<String>)]) -> String {
    let inner = fields
        .iter()
        .map(|(k, v)| match v {
            Some(v) => format!("'{k}': '{v}'"),
            None => format!("'{k}': None"),
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("{{{inner}}}")
}

fn py_keys(keys: &[&str]) -> String {
    let inner = keys
        .iter()
        .map(|k| format!("'{k}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{inner}]")
}

fn py_pairs(cands: &[(Value, String)]) -> String {
    let inner = cands
        .iter()
        .map(|(j, b)| {
            let id = j.get("id").and_then(Value::as_str).unwrap_or("?");
            format!("('{}', '{b}')", id8(id))
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{inner}]")
}

// ---------------------------------------------------------------------------
// The forge seam (internal-forge.md Q7a): every talk-to-the-code-host
// call goes through Forge, so internalizing Git/CI is an adapter swap
// — a ForgejoForge sibling selected by BOSS_TRAIN_FORGE — instead of
// a conductor rewrite at cutover. The GitHub adapter shells to `gh`
// exactly as before; behavior is unchanged by this refactor.
// ---------------------------------------------------------------------------

/// The code host as the conductor sees it: five verbs.
#[async_trait]
trait Forge: Send + Sync {
    /// -> {state, mergeCommit, statusCheckRollup} for a PR url.
    async fn pr_info(&self, url: &str) -> Result<Value>;
    /// Open a PR head->main on repo; return its url.
    async fn pr_create(
        &self,
        repo: &str,
        head_branch: &str,
        title: &str,
        body: &str,
    ) -> Result<String>;
    async fn merge(&self, url: &str) -> Result<()>;
    /// Close a PR WITHOUT merging — a cancelled train's PR must not
    /// sit open inviting a merge.
    async fn close_pr(&self, url: &str) -> Result<()>;
    /// Delete `branch` from the repo car branches are pushed to.
    /// Ok(true) = deleted; Ok(false) = already gone (404) — an
    /// expected state, the repo auto-deletes merged `train/*` PR
    /// heads and hand sweeps happen. Anything else is an error.
    async fn delete_branch(&self, branch: &str) -> Result<bool>;
    /// The branch's head sha right now, or Ok(None) when the branch is
    /// not there (404). The sweep's head guard reads this: a landed
    /// car's branch is only deletable while it still points at what
    /// boarded.
    async fn branch_head(&self, branch: &str) -> Result<Option<String>>;
}

/// `owner/name` from a clone url — https or ssh, with or without
/// `.git`: `https://github.com/dauld/boss-fork.git` and
/// `git@github.com:dauld/boss-fork` both give `dauld/boss-fork`.
pub(crate) fn repo_path(url: &str) -> String {
    let u = url.trim_end_matches('/').trim_end_matches(".git");
    let mut segs = u.rsplit(['/', ':']);
    let name = segs.next().unwrap_or_default();
    let owner = segs.next().unwrap_or_default();
    format!("{owner}/{name}")
}

struct GitHubForge {
    head_owner: String,
    /// The fork holding car branches (`owner/name`) — under GitHub
    /// the cars push to the fork, so that is where a landed car's
    /// branch gets deleted from.
    fork_repo: String,
}

#[async_trait]
impl Forge for GitHubForge {
    async fn pr_info(&self, url: &str) -> Result<Value> {
        let r = sh(&[
            "gh",
            "pr",
            "view",
            url,
            "--json",
            "state,mergeCommit,statusCheckRollup",
        ])?;
        serde_json::from_str(&stdout_str(&r)).context("parsing gh pr view output")
    }

    async fn pr_create(
        &self,
        repo: &str,
        head_branch: &str,
        title: &str,
        body: &str,
    ) -> Result<String> {
        let head = format!("{}:{head_branch}", self.head_owner);
        let r = sh(&[
            "gh", "pr", "create", "--repo", repo, "--head", &head, "--base", "main", "--title",
            title, "--body", body,
        ])?;
        let out = stdout_str(&r);
        Ok(out.trim().lines().last().unwrap_or_default().to_string())
    }

    async fn merge(&self, url: &str) -> Result<()> {
        sh(&["gh", "pr", "merge", url, "--squash"])?;
        Ok(())
    }

    async fn close_pr(&self, url: &str) -> Result<()> {
        sh(&["gh", "pr", "close", url])?;
        Ok(())
    }

    async fn delete_branch(&self, branch: &str) -> Result<bool> {
        let path = format!("repos/{}/git/refs/heads/{branch}", self.fork_repo);
        let r = sh_unchecked(&["gh", "api", "--method", "DELETE", &path])?;
        if r.status.success() {
            return Ok(true);
        }
        let stderr = String::from_utf8_lossy(&r.stderr);
        if stderr.contains("HTTP 404") || stderr.contains("Not Found") {
            return Ok(false);
        }
        bail!("gh api DELETE {path}: {}", stderr.trim());
    }

    /// `git/ref/heads/<branch>` — the singular form, which answers
    /// with the ONE ref; the plural `git/refs/...` answers with every
    /// ref sharing the prefix, and `feat/x` would happily return
    /// `feat/x-followup`.
    async fn branch_head(&self, branch: &str) -> Result<Option<String>> {
        let path = format!("repos/{}/git/ref/heads/{branch}", self.fork_repo);
        let r = sh_unchecked(&["gh", "api", &path])?;
        if !r.status.success() {
            let stderr = String::from_utf8_lossy(&r.stderr);
            if stderr.contains("HTTP 404") || stderr.contains("Not Found") {
                return Ok(None);
            }
            bail!("gh api {path}: {}", stderr.trim());
        }
        let v: Value =
            serde_json::from_str(&stdout_str(&r)).context("parsing gh api git/ref output")?;
        Ok(v.get("object")
            .and_then(|o| o.get("sha"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string))
    }
}

/// The same five verbs against the internal forge's API. PRs are
/// same-repo (no fork dance): the train branch pushes to the one
/// repo, the PR head is the bare branch name, and car branches get
/// deleted from that same repo at arrival.
struct ForgejoForge {
    base: String,
    repo: String,
    token: String,
    http: reqwest::Client,
}

impl ForgejoForge {
    fn new() -> Result<Self> {
        let base = env_or("BOSS_TRAIN_FORGE_URL", "http://10.20.0.15:3000")
            .trim_end_matches('/')
            .to_string();
        let repo = env_or("BOSS_TRAIN_FORGE_REPO", "david/boss");
        let token_file = env_or("BOSS_TRAIN_FORGE_TOKEN_FILE", "/etc/boss-train/forge.token");
        let token = fs::read_to_string(&token_file)
            .with_context(|| format!("reading {token_file}"))?
            .trim()
            .to_string();
        Ok(ForgejoForge {
            base,
            repo,
            token,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()?,
        })
    }

    async fn api(
        &self,
        method: Method,
        path: &str,
        payload: Option<Value>,
    ) -> Result<Option<Value>> {
        let mut req = self
            .http
            .request(method.clone(), format!("{}/api/v1{path}", self.base))
            .header("Authorization", format!("token {}", self.token))
            .header("Content-Type", "application/json");
        if let Some(p) = &payload {
            req = req.json(p);
        }
        let resp = req
            .send()
            .await
            .with_context(|| format!("forge {method} {path}"))?;
        let status = resp.status();
        let body = resp.text().await?;
        if !status.is_success() {
            bail!("forge {method} {path}: HTTP {status}: {}", body.trim());
        }
        if body.trim().is_empty() {
            Ok(None)
        } else {
            Ok(Some(serde_json::from_str(&body).with_context(|| {
                format!("parsing forge {method} {path} response")
            })?))
        }
    }

    fn index(url: &str) -> String {
        url.trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_string()
    }
}

#[async_trait]
impl Forge for ForgejoForge {
    /// Shape Forgejo's PR + combined status into the exact dict the
    /// GitHub adapter returns, so reconcile stays forge-blind.
    async fn pr_info(&self, url: &str) -> Result<Value> {
        let idx = Self::index(url);
        let pr = self
            .api(
                Method::GET,
                &format!("/repos/{}/pulls/{idx}", self.repo),
                None,
            )
            .await?
            .ok_or_else(|| anyhow!("empty PR body for {url}"))?;
        let state = if truthy(pr.get("merged")) {
            "MERGED"
        } else if pr.get("state").and_then(Value::as_str) == Some("open") {
            "OPEN"
        } else {
            "CLOSED"
        };
        let head_sha = pr
            .get("head")
            .and_then(|h| h.get("sha"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let mut rollup = Vec::new();
        if !head_sha.is_empty() {
            let combined = self
                .api(
                    Method::GET,
                    &format!("/repos/{}/commits/{head_sha}/status", self.repo),
                    None,
                )
                .await?;
            let statuses = combined
                .as_ref()
                .and_then(|c| c.get("statuses"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for st in &statuses {
                let verdict = st
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_lowercase();
                let conclusion = match verdict.as_str() {
                    "success" => "SUCCESS",
                    "failure" | "error" => "FAILURE",
                    _ => "",
                };
                rollup.push(json!({
                    "conclusion": conclusion,
                    "status": if verdict == "pending" { "PENDING" } else { "COMPLETED" },
                }));
            }
        }
        Ok(json!({
            "state": state,
            "mergeCommit": {
                "oid": pr.get("merge_commit_sha").and_then(Value::as_str).unwrap_or_default()
            },
            "statusCheckRollup": rollup,
        }))
    }

    async fn pr_create(
        &self,
        _repo: &str,
        head_branch: &str,
        title: &str,
        body: &str,
    ) -> Result<String> {
        let pr = self
            .api(
                Method::POST,
                &format!("/repos/{}/pulls", self.repo),
                Some(json!({
                    "head": head_branch, "base": "main",
                    "title": title, "body": body,
                })),
            )
            .await?
            .ok_or_else(|| anyhow!("empty create-PR response from the forge"))?;
        pr.get("html_url")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow!("create-PR response without html_url"))
    }

    async fn merge(&self, url: &str) -> Result<()> {
        let idx = Self::index(url);
        self.api(
            Method::POST,
            &format!("/repos/{}/pulls/{idx}/merge", self.repo),
            Some(json!({"Do": "squash"})),
        )
        .await?;
        Ok(())
    }

    async fn close_pr(&self, url: &str) -> Result<()> {
        let idx = Self::index(url);
        self.api(
            Method::PATCH,
            &format!("/repos/{}/pulls/{idx}", self.repo),
            Some(json!({"state": "closed"})),
        )
        .await?;
        Ok(())
    }

    /// DELETE /repos/{owner}/{repo}/branches/{branch}. Not through
    /// `api()` — a 404 here is an answer (already gone), not an
    /// error, and `api()` bails on every non-2xx.
    async fn delete_branch(&self, branch: &str) -> Result<bool> {
        let resp = self
            .http
            .request(
                Method::DELETE,
                format!("{}/api/v1/repos/{}/branches/{branch}", self.base, self.repo),
            )
            .header("Authorization", format!("token {}", self.token))
            .send()
            .await
            .with_context(|| format!("forge DELETE branches/{branch}"))?;
        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(false);
        }
        let body = resp.text().await?;
        // Forgejo answers a DELETE of an absent branch with 500 and
        // `object does not exist [id: refs/heads/<b>]`, not 404 —
        // observed 2026-08-13 against branches removed out of band,
        // where it failed every reconcile AFTER the merge and deploy
        // had already succeeded, so the run reported rc=1 and re-filed
        // its arrival report each tick. Already-gone is the sweep's
        // success condition whatever status dresses it up.
        if !status.is_success() && body.contains("object does not exist") {
            return Ok(false);
        }
        if !status.is_success() {
            bail!(
                "forge DELETE /repos/{}/branches/{branch}: HTTP {status}: {}",
                self.repo,
                body.trim()
            );
        }
        Ok(true)
    }

    /// GET /repos/{owner}/{repo}/branches/{branch} — `commit.id` is
    /// the head. Not through `api()` for the same reason as the delete
    /// above: a 404 here is an answer (no such branch), not an error.
    async fn branch_head(&self, branch: &str) -> Result<Option<String>> {
        let resp = self
            .http
            .request(
                Method::GET,
                format!("{}/api/v1/repos/{}/branches/{branch}", self.base, self.repo),
            )
            .header("Authorization", format!("token {}", self.token))
            .send()
            .await
            .with_context(|| format!("forge GET branches/{branch}"))?;
        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let body = resp.text().await?;
        if !status.is_success() {
            bail!(
                "forge GET /repos/{}/branches/{branch}: HTTP {status}: {}",
                self.repo,
                body.trim()
            );
        }
        let v: Value = serde_json::from_str(&body)
            .with_context(|| format!("parsing forge branches/{branch} response"))?;
        Ok(v.get("commit")
            .and_then(|c| c.get("id"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string))
    }
}

fn make_forge(cfg: &Config) -> Result<Box<dyn Forge>> {
    let kind = env_or("BOSS_TRAIN_FORGE", "github");
    match kind.as_str() {
        "github" => Ok(Box::new(GitHubForge {
            head_owner: cfg.head_owner.clone(),
            fork_repo: repo_path(&cfg.fork_url),
        })),
        "forgejo" => Ok(Box::new(ForgejoForge::new()?)),
        other => bail!("unknown BOSS_TRAIN_FORGE {other:?} — expected github or forgejo"),
    }
}

/// Collapse the forge's per-check rollup to green/pending/failing.
fn ci_verdict(rollup: Option<&Value>) -> &'static str {
    let Some(items) = rollup.and_then(Value::as_array).filter(|a| !a.is_empty()) else {
        return "pending";
    };
    let states: BTreeSet<String> = items
        .iter()
        .map(|c| {
            c.get("conclusion")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    c.get("status")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                })
                .unwrap_or_default()
                .to_uppercase()
        })
        .collect();
    const FAILING: [&str; 4] = ["FAILURE", "TIMED_OUT", "CANCELLED", "ACTION_REQUIRED"];
    if states.iter().any(|s| FAILING.contains(&s.as_str())) {
        return "failing";
    }
    const SETTLED: [&str; 4] = ["SUCCESS", "NEUTRAL", "SKIPPED", "COMPLETED"];
    if states.iter().any(|s| !SETTLED.contains(&s.as_str())) {
        return "pending";
    }
    "green"
}

// ---------------------------------------------------------------------------
// The conductor
// ---------------------------------------------------------------------------

struct Conductor {
    cfg: Config,
    http: reqwest::Client,
    forge: Box<dyn Forge>,
}

impl Conductor {
    fn new(cfg: Config, forge: Box<dyn Forge>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()?;
        Ok(Conductor { cfg, http, forge })
    }

    /// Every jobs-API call the conductor makes, under the blip guard:
    /// a rolling system of record must not fail a whole verb.
    async fn api(
        &self,
        method: Method,
        path: &str,
        payload: Option<Value>,
    ) -> Result<Option<Value>> {
        retrying(&JOBS_API_RETRY, &method, &|m| log(m), || {
            let method = method.clone();
            let payload = payload.clone();
            async move { self.api_once(method, path, payload).await }
        })
        .await
    }

    /// One attempt. Every way it can fail is classified on the way
    /// out, so the caller above decides retry-or-surface on evidence
    /// rather than on a string match over an error message.
    async fn api_once(
        &self,
        method: Method,
        path: &str,
        payload: Option<Value>,
    ) -> std::result::Result<Option<Value>, ApiFailure> {
        let mut req = self
            .http
            .request(method.clone(), format!("{}{path}", self.cfg.jobs))
            .header("content-type", "application/json")
            .header("x-boss-user", boss_user());
        if let Some(p) = &payload {
            req = req.json(p);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| ApiFailure::transport(e, format!("{method} {path}")))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| ApiFailure::transport(e, format!("reading {method} {path} response")))?;
        if !status.is_success() {
            return Err(ApiFailure {
                kind: Failure::Http(status.as_u16()),
                cause: anyhow!("{method} {path}: HTTP {status}: {}", body.trim()),
            });
        }
        if body.trim().is_empty() {
            return Ok(None);
        }
        serde_json::from_str(&body)
            .map(Some)
            .map_err(|e| ApiFailure {
                kind: Failure::Malformed,
                cause: anyhow::Error::new(e).context(format!("parsing {method} {path} response")),
            })
    }

    async fn get_job(&self, id: &str) -> Result<Value> {
        self.api(Method::GET, &format!("/api/jobs/{id}"), None)
            .await?
            .ok_or_else(|| anyhow!("job {id} came back empty"))
    }

    /// Complete `step` on `job` with evidence fields (None values are
    /// dropped, matching the python kwargs filter).
    async fn complete_step(
        &self,
        job: &Value,
        step: Option<&Value>,
        fields: &[(&str, Option<String>)],
    ) -> Result<()> {
        if step_done(step) {
            return Ok(());
        }
        let jid = job_id(job)?;
        let step = step.ok_or_else(|| anyhow!("step missing on job {}", id8(jid)))?;
        let mut md = metadata_map(step);
        for (k, v) in fields {
            if let Some(v) = v {
                md.insert((*k).to_string(), json!(v));
            }
        }
        if self.cfg.dry {
            log(format!(
                "DRY: would complete {} on {} with {}",
                step_label(step),
                id8(jid),
                py_dict(fields)
            ));
            return Ok(());
        }
        let sid = step
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("step without an id on job {jid}"))?;
        // WHEN is evidence too: steps carry only a completion DATE,
        // so the conductor stamps the instant itself — the arrival
        // report's timings derive from these.
        md.insert(
            "completed_at".to_string(),
            json!(Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
        );
        self.api(
            Method::PUT,
            &format!("/api/jobs/{jid}/steps/{sid}"),
            Some(json!({"status": "completed", "metadata": md})),
        )
        .await?;
        log(format!("completed {} on {}", step_label(step), id8(jid)));
        Ok(())
    }

    /// update_job takes a whole Job; fetch, merge metadata, put back.
    /// The overlay itself is `overlay_metadata` — pure, and pinned by
    /// tests: PUT replaces metadata wholesale, so clobbering here
    /// would silently eat another writer's keys. A `Value::Null`
    /// value removes the key.
    async fn merge_job_metadata(&self, jid: &str, kv: Vec<(&str, Value)>) -> Result<Value> {
        let mut job = self.get_job(jid).await?;
        let keys: Vec<&str> = kv.iter().map(|(k, _)| *k).collect();
        let md = overlay_metadata(&job, kv);
        job["metadata"] = Value::Object(md);
        if self.cfg.dry {
            log(format!(
                "DRY: would set {} on job {}",
                py_keys(&keys),
                id8(jid)
            ));
            return Ok(job);
        }
        self.api(Method::PUT, &format!("/api/jobs/{jid}"), Some(job.clone()))
            .await?;
        Ok(job)
    }

    // -----------------------------------------------------------------------
    // Phase 1 — reconcile open trains against reality
    // -----------------------------------------------------------------------

    /// Carry a merged train out to the playground — only from a clean
    /// main tree; anything else is recorded and retried next run.
    async fn deploy(&self, train: &Value, deployed_step: &Value) -> Result<()> {
        let tree = self.cfg.deploy_tree.clone();
        let tree_path = Path::new(&tree);
        // Deploy only when needed. The skip decision comes before the
        // busy check — a no-op deploy has no business caring about
        // the tree — and reads two facts: the generation store's live
        // key and what `main` is on the remote. Matching pair: record
        // the evidence on the step and journal the skip; the services
        // stay unbounced.
        let pull_remote = env_or("BOSS_TRAIN_DEPLOY_REMOTE", "origin");
        let remote_out = sh_unchecked(&["git", "-C", &tree, "ls-remote", &pull_remote, "main"])?;
        let remote_main = stdout_str(&remote_out)
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_string();
        let current = current_generation_key();
        if !deploy_needed(&current, &remote_main) {
            let short: String = remote_main.chars().take(12).collect();
            log(format!(
                "deploy skipped — generation {current} already serves main@{short}"
            ));
            self.complete_step(
                train,
                Some(deployed_step),
                &[(
                    "deployed",
                    Some(format!(
                        "already live: generation {current} serves main@{short}; no deploy run"
                    )),
                )],
            )
            .await?;
            return Ok(());
        }
        let dirty_out = sh_unchecked(&["git", "-C", &tree, "status", "--porcelain"])?;
        let dirty = !stdout_str(&dirty_out).trim().is_empty();
        let branch_out = sh(&["git", "-C", &tree, "rev-parse", "--abbrev-ref", "HEAD"])?;
        let branch = stdout_str(&branch_out).trim().to_string();
        if dirty || branch != "main" {
            // dirty prints True/False — python's bool repr; the journal
            // line is operator surface and stays byte-identical.
            let reason = format!(
                "deploy tree busy (branch={branch}, dirty={}) — will retry",
                if dirty { "True" } else { "False" }
            );
            log(&reason);
            if !self.cfg.dry {
                let tid = job_id(train)?;
                let sid = deployed_step
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("deployed step without an id on job {tid}"))?;
                let mut md = metadata_map(deployed_step);
                md.insert("deploy_blocked".to_string(), json!(reason));
                self.api(
                    Method::PUT,
                    &format!("/api/jobs/{tid}/steps/{sid}"),
                    Some(json!({"metadata": md})),
                )
                .await?;
            }
            return Ok(());
        }
        if self.cfg.dry {
            log("DRY: would pull main, migrate, build, deploy services + web");
            return Ok(());
        }
        // Under the forge protocol the playground converges on forge
        // main; GitHub is the mirror, never the source (27ab7680).
        sh(&["git", "-C", &tree, "pull", &pull_remote, "main"])?;
        let main_ref_out = sh(&["git", "-C", &tree, "rev-parse", "--short", "HEAD"])?;
        let main_ref = stdout_str(&main_ref_out).trim().to_string();
        let mig = Command::new(format!("{tree}/infra/postgres/migrate.sh"))
            .args(["--", "psql", "-U", "boss", "-h", "127.0.0.1", "-d", "boss"])
            .current_dir(tree_path)
            .env("PGPASSWORD", "boss")
            .output()
            .context("spawning migrate.sh")?;
        if !mig.status.success() {
            bail!(
                "migrate.sh failed:\n{}",
                String::from_utf8_lossy(&mig.stderr).trim()
            );
        }
        sh_in(
            Some(tree_path),
            true,
            &[&format!("{tree}/infra/build-release.sh")],
        )?;
        sh_in(
            Some(tree_path),
            true,
            &[
                "sudo",
                "-n",
                &format!("{tree}/infra/deploy-services.sh"),
                "prod",
            ],
        )?;
        sh_in(
            Some(tree_path),
            true,
            &["sudo", "-n", &format!("{tree}/infra/deploy-web.sh")],
        )?;
        let mig_out = stdout_str(&mig);
        let summary = format!(
            "main@{main_ref}; {}; services: prod; web: deployed",
            mig_out.trim().lines().last().unwrap_or_default()
        );
        self.complete_step(train, Some(deployed_step), &[("deployed", Some(summary))])
            .await?;
        Ok(())
    }

    async fn reconcile(&self, now: DateTime<Utc>) -> Result<()> {
        let trains = rows(
            self.api(
                Method::GET,
                "/api/jobs?kind=pr-train&status=open&limit=50",
                None,
            )
            .await?,
        )?;
        for t0 in trains {
            let tid = job_id(&t0)?.to_string();
            let mut t = self.get_job(&tid).await?;
            // The stall sentinel first — a train stuck BEFORE its PR
            // (assembly died, push hung) would slip past the
            // pr-step early-continues below and stall invisibly.
            self.note_stall(&t, now).await?;
            let pr_step = find_step(&t, "pr", "Open the batched PR");
            if !step_done(pr_step) {
                continue; // this window's board phase, or a stalled assembly
            }
            let pr_url = pr_step
                .and_then(|s| s.get("metadata"))
                .and_then(|m| m.get("pr_url"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if pr_url.is_empty() {
                continue;
            }
            let mut info = self.forge.pr_info(&pr_url).await?;

            let ci_step = find_step(&t, "ci", "CI verdict");
            let verdict = ci_verdict(info.get("statusCheckRollup"));
            if !step_done(ci_step) && verdict != "pending" {
                self.complete_step(&t, ci_step, &[("result", Some(verdict.to_string()))])
                    .await?;
            } else if step_done(ci_step) {
                // The step has already recorded its verdict and cannot
                // record another — terminal rows are frozen. Compare
                // against the last verdict we NOTICED (the job stamp,
                // falling back to the step's original) so this fires on
                // each change rather than on every ten-minute tick.
                let md = t.get("metadata");
                let noticed = md
                    .and_then(|m| m.get("ci_verdict_latest"))
                    .and_then(Value::as_str)
                    .or_else(|| {
                        ci_step
                            .and_then(|s| s.get("metadata"))
                            .and_then(|m| m.get("result"))
                            .and_then(Value::as_str)
                    });
                if let Some(note) = verdict_drift(noticed, verdict) {
                    log(format!("train {}: {note}", id8(&tid)));
                    if !self.cfg.dry {
                        self.merge_job_metadata(
                            &tid,
                            vec![
                                ("ci_verdict_latest", json!(verdict)),
                                ("ci_verdict_changed_at", json!(now.to_rfc3339())),
                            ],
                        )
                        .await?;
                        t = self.get_job(&tid).await?;
                    }
                }
            }

            // Asked and unanswered. Stamped once, like the stall
            // sentinel, so a hung runner produces one line rather than
            // one every ten minutes for as long as it hangs.
            if !truthy(t.get("metadata").and_then(|m| m.get("ci_overdue_since")))
                && let Some(note) = ci_overdue(&t, now, self.cfg.ci_hours)
            {
                log(format!("train {}: {note}", id8(&tid)));
                if !self.cfg.dry {
                    self.merge_job_metadata(
                        &tid,
                        vec![("ci_overdue_since", json!(now.to_rfc3339()))],
                    )
                    .await?;
                    t = self.get_job(&tid).await?;
                }
            }

            // The overnight rule, before the merge check: a train that
            // is red AND has stopped moving releases its consist so the
            // next window can board without it. Decided on the LIVE
            // verdict just read, never on the `ci` step's first stamp.
            if self.cfg.auto_cancel
                && info.get("state").and_then(Value::as_str) == Some("OPEN")
                && let Some(reason) = auto_cancel_reason(&t, verdict, now, self.cfg.stall_hours)
            {
                log(format!("train {} auto-cancelling: {reason}", id8(&tid)));
                if self.cfg.dry {
                    log(format!("DRY: would cancel {} ({reason})", id8(&tid)));
                } else {
                    self.cancel_train(&tid, &reason, true).await?;
                }
                continue;
            }

            if self.cfg.auto_merge
                && verdict == "green"
                && info.get("state").and_then(Value::as_str) == Some("OPEN")
            {
                log(format!(
                    "CI green — merging {pr_url} (train protocol 27ab7680)"
                ));
                if !self.cfg.dry {
                    self.forge.merge(&pr_url).await?;
                    info = self.forge.pr_info(&pr_url).await?;
                }
            }

            let merged_step = find_step(&t, "merged", "Merged into main");
            if info.get("state").and_then(Value::as_str) == Some("MERGED")
                && !step_done(merged_step)
            {
                let merge_ref: String = info
                    .get("mergeCommit")
                    .and_then(|m| m.get("oid"))
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .chars()
                    .take(12)
                    .collect();
                self.complete_step(&t, merged_step, &[("merge_ref", Some(merge_ref.clone()))])
                    .await?;
                let boarded: Vec<String> = t
                    .get("metadata")
                    .and_then(|m| m.get("boarded_jobs"))
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                for cid in boarded {
                    // The car's review closes HERE, not at boarding —
                    // the change was open for review until it landed,
                    // and leaving the step ready while the car rides is
                    // what lets a cancelled train release it (see the
                    // boarding loop). Completed first, because the
                    // ship-a-change spec gates `merged` on
                    // `steps.review.done AND job.metadata.merged`, and
                    // the marker below is what the dispatcher watches to
                    // close the Job.
                    let car = self.get_job(&cid).await?;
                    let review = find_step(&car, "review", "Open for review");
                    if !step_done(review) {
                        self.complete_step(
                            &car,
                            review,
                            &[
                                ("pr_url", Some(pr_url.clone())),
                                ("note", Some(format!("landed on main as {merge_ref}"))),
                            ],
                        )
                        .await?;
                    }
                    // v3 ship-a-change gates `merged` on this marker; the
                    // dispatcher closes the Job once it is set.
                    self.merge_job_metadata(
                        &cid,
                        vec![
                            ("merged", json!("true")),
                            ("merge_ref", json!(merge_ref.as_str())),
                        ],
                    )
                    .await?;
                }
                t = self.get_job(&tid).await?;
            }

            let merged_step = find_step(&t, "merged", "Merged into main");
            let deployed_step = find_step(&t, "deployed", "Deployed to the playground");
            if step_done(merged_step) && !step_done(deployed_step) {
                let deployed_step = deployed_step
                    .ok_or_else(|| anyhow!("deployed step missing on job {}", id8(&tid)))?;
                self.deploy(&t, deployed_step).await?;
            }
        }
        // Housekeeping must not fail a run whose real work succeeded.
        // The sweep runs last, after merges, deploys and evidence are
        // recorded; on 2026-08-13 a single un-deletable branch made
        // every reconcile report rc=1 and re-file its arrival report,
        // which reads as "the conductor is broken" when the trains had
        // in fact landed. Journal the failure, keep the verb green.
        if let Err(e) = self.sweep_landed_branches().await {
            log(format!(
                "branch sweep failed (housekeeping, run stands): {e}"
            ));
        }
        Ok(())
    }

    /// The stall sentinel: stamp `stalled_since` (once) when an open
    /// train's newest step completion ages past the threshold; clear
    /// the stamp when the train advances. Raising is protocol,
    /// cancelling is judgment — nothing here auto-cancels; the
    /// operator's verb for that is `boss train cancel`.
    async fn note_stall(&self, t: &Value, now: DateTime<Utc>) -> Result<()> {
        let tid = job_id(t)?;
        let stamped = truthy(t.get("metadata").and_then(|m| m.get("stalled_since")));
        match stall_age_hours(t, now, self.cfg.stall_hours) {
            Some(age) if !stamped => {
                log(format!(
                    "train {} stalled ({age}h, threshold {}h)",
                    id8(tid),
                    self.cfg.stall_hours
                ));
                // Since WHEN: the newest completion — the moment
                // progress provably stopped, not the moment the
                // sentinel happened to look.
                let since = newest_completion(t).unwrap_or_default().to_string();
                self.merge_job_metadata(tid, vec![("stalled_since", json!(since))])
                    .await?;
            }
            None if stamped => {
                log(format!("train {} advanced — stall stamp cleared", id8(tid)));
                self.merge_job_metadata(tid, vec![("stalled_since", Value::Null)])
                    .await?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Reconcile's arrival sweep: delete landed cars' branches from
    /// the forge (protocol decision, David). The repo auto-deletes
    /// merged `train/*` PR heads, but each CAR branch survives its
    /// squash-merged content landing — and ancestry cannot prove the
    /// landing, so nothing git-side can ever say "safe to sweep".
    /// The job record can: once a train has closed (arrived) and a
    /// boarded car closed with the merged outcome, that branch's
    /// work is on main and the conductor deletes it. A 404 is a fine
    /// answer — something got there first, and the sweep says nothing
    /// about it (see `sweep_note`). A train whose cars have all
    /// reached a terminal is stamped `branches_swept`, so the steady
    /// state costs one list call and no per-car fetches.
    ///
    /// Forge cost, unchanged by the quieting: one list call, plus per
    /// UNSWEPT train one fetch per boarded car and one `branch_head`
    /// per deletable branch. The `branches_swept` stamp is what bounds
    /// it — coverage is never capped, so no landed branch goes
    /// uninspected.
    async fn sweep_landed_branches(&self) -> Result<()> {
        let arrived = rows(
            self.api(
                Method::GET,
                "/api/jobs?kind=pr-train&status=closed&limit=50",
                None,
            )
            .await?,
        )?;
        // Filter on the list rows (they carry metadata): swept trains
        // and cancelled ones (nothing boarded) drop out fetch-free.
        let pending: Vec<&Value> = arrived
            .iter()
            .filter(|t| {
                let md = t.get("metadata");
                !truthy(md.and_then(|m| m.get("branches_swept")))
                    && truthy(md.and_then(|m| m.get("boarded_jobs")))
            })
            .collect();
        if pending.is_empty() {
            return Ok(());
        }
        // Branches still-open cars name, fetched once per pass: a
        // live car's claim beats any landed car's deletion.
        let open_branches = self.open_car_branches().await?;
        for t in pending {
            let tid = job_id(t)?;
            let boarded: Vec<String> = t
                .get("metadata")
                .and_then(|m| m.get("boarded_jobs"))
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let mut cars = Vec::with_capacity(boarded.len());
            for cid in &boarded {
                cars.push(self.get_job(cid).await?);
            }
            self.file_arrival_report(tid, &cars).await?;
            for (branch, car) in deletable_branches(&cars, &open_branches) {
                // The job record proved the CONTENT landed; the head
                // guard proves the branch still holds only that
                // content. Both, or the branch stays (car 23923b40).
                let recorded = cars
                    .iter()
                    .find(|c| c.get("id").and_then(Value::as_str) == Some(car.as_str()))
                    .and_then(boarded_head)
                    .map(str::to_string);
                let current = self.forge.branch_head(&branch).await?;
                let guard = sweep_guard(recorded.as_deref(), current.as_deref());
                // Verdicts that keep a branch narrate themselves, and
                // a branch already off the forge narrates nothing.
                if let Some(note) = sweep_note(&guard, &branch, &car) {
                    log(note);
                }
                if guard == SweepGuard::Delete {
                    if self.cfg.dry {
                        log(format!(
                            "DRY: would delete branch {branch} (car {} landed)",
                            id8(&car)
                        ));
                    } else if self.forge.delete_branch(&branch).await? {
                        log(format!(
                            "deleted branch {branch} (car {} landed)",
                            id8(&car)
                        ));
                    } else {
                        // It existed a moment ago — something else
                        // swept it between the two calls. Rare, and
                        // worth saying so it is not read as our doing.
                        log(format!(
                            "branch {branch} already gone (car {} landed)",
                            id8(&car)
                        ));
                    }
                }
            }
            if sweep_settled(&cars) {
                self.merge_job_metadata(tid, vec![("branches_swept", json!("true"))])
                    .await?;
            }
        }
        Ok(())
    }

    /// File the arrival report — the landing's final structured entry
    /// — on an arrived train's `arrived` step, once. The sweep is the
    /// conductor's visit to every arrived train, so the report is
    /// composed here from the full job record plus the boarded cars
    /// the sweep already fetched. The step PUT merges metadata (the
    /// same rule `overlay_metadata` pins): the outcome step's own
    /// keys survive the filing.
    async fn file_arrival_report(&self, tid: &str, cars: &[Value]) -> Result<()> {
        let train = self.get_job(tid).await?;
        let Some(step) = find_step(&train, "arrived", "Train arrived") else {
            return Ok(());
        };
        let filed = step
            .get("metadata")
            .and_then(|m| m.get("arrival_report"))
            .is_some();
        // Strictly `completed` — never `skipped`: a cancelled train
        // closes with its arrived step SKIPPED, and a landing report
        // on a train that never landed would be fiction.
        let arrived = step.get("status").and_then(Value::as_str) == Some("completed");
        if !arrived || filed {
            return Ok(());
        }
        let report = arrival_report(&train, cars);
        let summary = arrival_summary(&report);
        let n = report
            .get("consist")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        let total = report
            .get("timings")
            .and_then(|t| t.get("total_s"))
            .and_then(Value::as_i64)
            .map_or_else(|| "?".to_string(), |s| s.to_string());
        if self.cfg.dry {
            log(format!(
                "DRY: would file the arrival report on {}",
                id8(tid)
            ));
            return Ok(());
        }
        let sid = step
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("arrived step without an id on job {tid}"))?;
        let md = overlay_metadata(
            step,
            vec![("arrival_report", report), ("summary", json!(summary))],
        );
        self.api(
            Method::PUT,
            &format!("/api/jobs/{tid}/steps/{sid}"),
            Some(json!({"metadata": md})),
        )
        .await?;
        log(format!(
            "arrival report on {} ({n} cars, total {total}s)",
            id8(tid)
        ));
        Ok(())
    }

    /// The branches named by still-open ship-a-change cars — never
    /// deletable, whoever landed on them. Read off the list rows
    /// (the jobs list returns full metadata); an open car with no
    /// branch yet contributes nothing.
    async fn open_car_branches(&self) -> Result<BTreeSet<String>> {
        let listed = rows(
            self.api(
                Method::GET,
                "/api/jobs?kind=ship-a-change&status=open&limit=100",
                None,
            )
            .await?,
        )?;
        Ok(listed
            .iter()
            .filter_map(|j| {
                j.get("metadata")
                    .and_then(|m| m.get("branch"))
                    .and_then(Value::as_str)
                    .filter(|b| !b.is_empty())
                    .map(str::to_string)
            })
            .collect())
    }

    // -----------------------------------------------------------------------
    // Phase 2 — board this window's train
    // -----------------------------------------------------------------------

    fn ensure_clone(&self) -> Result<()> {
        let clone = &self.cfg.clone;
        if !Path::new(clone).join(".git").is_dir() {
            fs::create_dir_all(&self.cfg.home)?;
            sh(&["git", "clone", &self.cfg.upstream_url, clone])?;
            sh(&[
                "git",
                "-C",
                clone,
                "remote",
                "add",
                "fork",
                &self.cfg.fork_url,
            ])?;
            // The merge commits the assembly makes need an author, and the
            // honest one is the machine that made them (a fresh clone has
            // no identity — the first real run failed exactly here).
            sh(&[
                "git",
                "-C",
                clone,
                "config",
                "user.name",
                "BOSS train conductor",
            ])?;
            sh(&[
                "git",
                "-C",
                clone,
                "config",
                "user.email",
                "train-conductor@boss.invalid",
            ])?;
        }
        sh(&["git", "-C", clone, "fetch", "origin", "--prune"])?;
        sh(&["git", "-C", clone, "fetch", "fork", "--prune"])?;
        Ok(())
    }

    /// The parked-ready cars whose branch is actually on the fork,
    /// plus the left-behind record for the ones whose branch is not
    /// — each of those gets its `skip_reason` stamped (the yard's
    /// "LEFT BEHIND" chip) and an entry for the train's own books.
    async fn candidates(&self) -> Result<(Vec<(Value, String)>, Vec<Value>)> {
        let mut out = Vec::new();
        let mut left_behind = Vec::new();
        let listed = rows(
            self.api(
                Method::GET,
                "/api/jobs?kind=ship-a-change&status=open&limit=100",
                None,
            )
            .await?,
        )?;
        for j0 in listed {
            let jid = job_id(&j0)?.to_string();
            let j = self.get_job(&jid).await?;
            if !parked_ready(&j) {
                continue;
            }
            // The two-strike hold. Without it the auto-cancel above is
            // a loop: the same consist re-boards, goes red, cancels,
            // and burns the night landing nothing.
            if let Some(reason) = car_hold_reason(&j, MAX_RED_TRAINS) {
                log(format!("{}: {reason} — leaving behind", id8(&jid)));
                left_behind.push(json!({"car_id_short": id8(&jid), "reason": reason.as_str()}));
                if !self.cfg.dry {
                    self.merge_job_metadata(&jid, vec![("skip_reason", json!(reason))])
                        .await?;
                }
                continue;
            }
            let branch = j
                .get("metadata")
                .and_then(|m| m.get("branch"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let ok = sh_unchecked(&[
                "git",
                "-C",
                &self.cfg.clone,
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("fork/{branch}"),
            ])?;
            // RECOVER RATHER THAN SKIP. A car is parked at review by its
            // author pushing the branch; the natural place to push is
            // the upstream the author cloned, and the fork is an
            // implementation detail of how this conductor assembles a
            // train. On 2026-08-14 that gap silently held NINE cars for
            // a whole session: the dock reported 12 parked while the
            // boardable count was 0, because `parked_ready` asks
            // "branch declared, review ready" and this asks "branch on
            // the fork" — two predicates for one question, and only the
            // first is on any dashboard.
            //
            // So if the branch exists upstream, put it on the fork and
            // board the car. Copying a ref the author already published
            // is not a judgement call; refusing to, and reporting a
            // dock depth that cannot board, is the surprising
            // behaviour. A branch that exists in NEITHER place is still
            // a real skip — that car was never pushed at all.
            let mut ok = ok;
            if !ok.status.success() {
                let upstream = sh_unchecked(&[
                    "git",
                    "-C",
                    &self.cfg.clone,
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("origin/{branch}"),
                ])?;
                if upstream.status.success() && !self.cfg.dry {
                    let pushed = sh_unchecked(&[
                        "git",
                        "-C",
                        &self.cfg.clone,
                        "push",
                        "fork",
                        &format!("origin/{branch}:refs/heads/{branch}"),
                    ])?;
                    if pushed.status.success() {
                        log(format!(
                            "{}: branch {branch} was upstream but not on the fork — pushed it",
                            id8(&jid)
                        ));
                        ok = sh_unchecked(&[
                            "git",
                            "-C",
                            &self.cfg.clone,
                            "rev-parse",
                            "--verify",
                            "--quiet",
                            &format!("fork/{branch}"),
                        ])?;
                    }
                }
            }
            if !ok.status.success() {
                let reason = skip_reason_branch_missing(&branch);
                log(format!("{}: {reason} — leaving behind", id8(&jid)));
                left_behind.push(json!({"car_id_short": id8(&jid), "reason": reason.as_str()}));
                if !self.cfg.dry {
                    // Loud on the Job, not just in the journal: the author
                    // parked this at review believing it would board.
                    self.merge_job_metadata(&jid, vec![("skip_reason", json!(reason))])
                        .await?;
                }
                continue;
            }
            out.push((j, branch));
        }
        Ok((out, left_behind))
    }

    async fn open_train_job(&self, train_branch: &str, window: &str) -> Result<Option<Value>> {
        let payload = json!({
            "kind": "pr-train",
            "subject": {"subject_kind": "custom", "id": train_branch},
            "title": format!("PR train {window}"),
            // The conductor is a machine and says so. `resolve_owner`
            // reads any colon-bearing id as automation and places the
            // Job on an active holder of the kind's `owner_role`
            // (`platform-admin` for pr-train) — so the responsible
            // human is whoever actually holds the role today.
            //
            // This used to name `emp-bootstrap-admin` outright, which
            // survived only because that row happened to be the
            // deployment's admin. Once the bootstrap identity is
            // retired in favour of a named person, a hardcoded owner
            // is a dead id that resolution has to quietly override —
            // right by accident rather than by construction.
            "owner_id": ACTOR,
            "status": "open",
            "priority": "standard",
            "metadata": {"actor": ACTOR},
            "tags": ["train"],
        });
        if self.cfg.dry {
            log(format!("DRY: would open train Job for {train_branch}"));
            return Ok(None);
        }
        let created = self.api(Method::POST, "/api/jobs", Some(payload)).await?;
        let jid = created
            .as_ref()
            .and_then(|c| c.get("id"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let jid = match jid {
            Some(id) => id,
            None => {
                // some create paths return the row wrapped
                let listed = rows(
                    self.api(
                        Method::GET,
                        "/api/jobs?kind=pr-train&status=open&limit=5",
                        None,
                    )
                    .await?,
                )?;
                job_id(
                    listed
                        .first()
                        .ok_or_else(|| anyhow!("no open pr-train Job found after create"))?,
                )?
                .to_string()
            }
        };
        Ok(Some(self.get_job(&jid).await?))
    }

    async fn board(&self, now: DateTime<Utc>) -> Result<()> {
        self.ensure_clone()?;
        let (cands, mut left_behind) = self.candidates().await?;
        let window = format!(
            "{}{}",
            now.format("%Y-%m-%d "),
            if now.hour() < 12 { "AM" } else { "PM" }
        );
        let train_branch = format!("train/{}", now.format("%Y%m%d-%H%M"));
        let Some(train) = self.open_train_job(&train_branch, &window).await? else {
            // dry run
            log(format!("DRY: candidates: {}", py_pairs(&cands)));
            return Ok(());
        };
        let train_id = job_id(&train)?.to_string();
        let collect = find_step(&train, "collect", "Collect what is ready to board");

        if cands.is_empty() {
            self.merge_job_metadata(&train_id, vec![("empty", json!("true"))])
                .await?;
            self.complete_step(
                &train,
                collect,
                &[(
                    "boarded",
                    Some("nothing ready to board this window".to_string()),
                )],
            )
            .await?;
            log("empty window — train cancels via the marker");
            return Ok(());
        }

        let clone = &self.cfg.clone;
        sh(&[
            "git",
            "-C",
            clone,
            "checkout",
            "-B",
            &train_branch,
            "origin/main",
        ])?;
        // (car, branch, boarded head) — the head is WHAT boarded, and
        // the sweep's licence to delete the branch later depends on it
        // (car 23923b40). Read from the fetched `fork/<branch>` ref,
        // which is precisely the commit the merge below carries.
        let mut boarded: Vec<(Value, String, String)> = Vec::new();
        let mut skipped: Vec<(Value, String)> = Vec::new();
        for (j, branch) in cands {
            let head_out = sh(&["git", "-C", clone, "rev-parse", &format!("fork/{branch}")])?;
            let head = stdout_str(&head_out).trim().to_string();
            let r = sh_unchecked(&[
                "git",
                "-C",
                clone,
                "merge",
                "--no-ff",
                "-m",
                &format!("train: merge {branch}"),
                &format!("fork/{branch}"),
            ])?;
            if r.status.success() {
                boarded.push((j, branch, head));
            } else {
                let diff =
                    sh_unchecked(&["git", "-C", clone, "diff", "--name-only", "--diff-filter=U"])?;
                let conflicted: Vec<String> = stdout_str(&diff)
                    .split_whitespace()
                    .map(str::to_string)
                    .collect();
                sh_unchecked(&["git", "-C", clone, "merge", "--abort"])?;

                // Before abandoning it, try re-railing.
                //
                // The commonest conflict here is not a real one. The
                // repo squash-merges, so a car cut before the last
                // train — or stacked on a car that has since landed —
                // carries commits whose CHANGES are already in main but
                // whose SHAS are not ancestors of it. Merging re-applies
                // landed hunks on top of themselves and collides.
                //
                // `git rebase` is the tool that knows the difference: it
                // drops a patch already present upstream. So replay the
                // car's own commits onto the consist as it stands and
                // merge that instead. A car with a GENUINE conflict
                // fails the rebase too and is skipped exactly as before.
                //
                // Measured cost of not doing this: four cars re-railed
                // by hand in one evening (2026-08-15), each one a fresh
                // branch name, a repointed `metadata.branch` and a wait
                // for the next window — and the same by hand on 08-12
                // and 08-14. The conductor already knows everything it
                // needs; it just gave up one step early.
                if let Some(rerailed) = rerail_onto_consist(clone, &train_branch, &branch)? {
                    let retry = sh_unchecked(&[
                        "git",
                        "-C",
                        clone,
                        "merge",
                        "--no-ff",
                        "-m",
                        &format!("train: merge {branch} (re-railed)"),
                        &rerailed,
                    ])?;
                    if retry.status.success() {
                        log(format!(
                            "{branch}: re-railed onto the consist — its base was no longer an \
                             ancestor of main"
                        ));
                        // The ORIGINAL head is still what boarded: the
                        // sweep's licence to delete the branch compares
                        // against the ref the car names, and re-railing
                        // changed the shas we merged, not the car.
                        boarded.push((j, branch, head));
                        continue;
                    }
                    sh_unchecked(&["git", "-C", clone, "merge", "--abort"])?;
                }
                // ONE reason string, journal and Job alike — the chip
                // the yard renders and the line the operator greps
                // must never tell different stories.
                let reason = skip_reason_conflict(&conflicted);
                log(format!("{branch}: {reason} — left for the next train"));
                left_behind.push(json!({
                    "car_id_short": id8(job_id(&j)?),
                    "reason": reason.as_str(),
                }));
                self.merge_job_metadata(job_id(&j)?, vec![("skip_reason", json!(reason))])
                    .await?;
                skipped.push((j, branch));
            }
        }

        let skipped_names = skipped
            .iter()
            .map(|(_, b)| b.as_str())
            .collect::<Vec<_>>()
            .join(", ");

        if boarded.is_empty() {
            self.merge_job_metadata(&train_id, vec![("empty", json!("true"))])
                .await?;
            self.complete_step(
                &train,
                collect,
                &[(
                    "boarded",
                    Some(format!(
                        "all candidates skipped on merge conflicts: {skipped_names}"
                    )),
                )],
            )
            .await?;
            return Ok(());
        }

        sh(&["git", "-C", clone, "push", "fork", &train_branch])?;
        let train_ref_out = sh(&["git", "-C", clone, "rev-parse", "--short", "HEAD"])?;
        let train_ref = stdout_str(&train_ref_out).trim().to_string();

        let mut lines: Vec<String> = boarded
            .iter()
            .map(|(j, b, _)| {
                format!(
                    "- `{b}` — {} (Job `{}`)",
                    j.get("title").and_then(Value::as_str).unwrap_or_default(),
                    id8(j.get("id").and_then(Value::as_str).unwrap_or("?"))
                )
            })
            .collect();
        if !skipped.is_empty() {
            lines.push(String::new());
            lines.push(format!(
                "Left behind on merge conflicts (next train): {skipped_names}"
            ));
        }
        let body = format!(
            "The {window} train: {} change(s) batched by the conductor.\n\n{}\n\n\
             🤖 opened by `boss train` (pr-train Workflow)",
            boarded.len(),
            lines.join("\n")
        );
        let pr_url = self
            .forge
            .pr_create(
                &self.cfg.gh_repo,
                &train_branch,
                &format!("train: {window} ({} changes)", boarded.len()),
                &body,
            )
            .await?;

        let boarded_ids: Vec<String> = boarded
            .iter()
            .map(|(j, _, _)| job_id(j).map(str::to_string))
            .collect::<Result<_>>()?;
        let skipped_branches: Vec<String> = skipped.iter().map(|(_, b)| b.clone()).collect();
        self.merge_job_metadata(
            &train_id,
            vec![
                ("boarded_jobs", json!(boarded_ids)),
                ("skipped_branches", json!(skipped_branches)),
                // The train's own record of who it left behind and
                // why — the arrival report reads THIS, because a
                // car's skip_reason clears the moment a later train
                // boards it.
                ("left_behind", json!(left_behind)),
            ],
        )
        .await?;
        let train = self.get_job(&train_id).await?;
        let boarded_note = boarded
            .iter()
            .map(|(j, b, _)| {
                format!(
                    "{b} ({})",
                    id8(j.get("id").and_then(Value::as_str).unwrap_or("?"))
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        self.complete_step(
            &train,
            find_step(&train, "collect", "Collect what is ready to board"),
            &[("boarded", Some(boarded_note))],
        )
        .await?;
        self.complete_step(
            &train,
            find_step(&train, "assemble", "Assemble the train branch"),
            &[
                ("train_ref", Some(format!("{train_branch}@{train_ref}"))),
                (
                    "skipped",
                    Some(if skipped_names.is_empty() {
                        "none".to_string()
                    } else {
                        skipped_names.clone()
                    }),
                ),
            ],
        )
        .await?;
        self.complete_step(
            &train,
            find_step(&train, "pr", "Open the batched PR"),
            &[("pr_url", Some(pr_url.clone()))],
        )
        .await?;

        for (j, _branch, head) in &boarded {
            // BOARDING DOES NOT COMPLETE `review` — the merge does.
            //
            // It used to complete it here, and that quietly made
            // cancelling a loaded train impossible. A released car has
            // to become `parked_ready` again, which requires its review
            // step to be ready or active; but a completed step is FROZEN
            // at the row (`update_step_at` pins status, completed_on and
            // metadata on terminal rows, deliberately, so a racing
            // read-modify-write cannot demote it). So the cancel path's
            // reopen was a no-op that returned 204, and every "released
            // car back to the dock" line it logged was false — the car
            // had `train` cleared but stayed unboardable forever. The
            // only reason nobody hit it is that every cancel until now
            // carried zero cars.
            //
            // Boarded-ness does not need the step at all: it is
            // `metadata.train`, which is what `parked_ready` already
            // reads, and which a cancel can clear because metadata is
            // not frozen. So the step keeps meaning what it says —
            // this change is open for review until it lands — and
            // release becomes a metadata write with nothing to reverse.
            // (Requires no workflow edit: the spec still gates the
            // `merged` outcome on `steps.review.done`, and the merge
            // block below is what satisfies it.)
            //
            // skip_reason cleared on boarding, in the same update that
            // stamps the train: an earlier window's skip note must not
            // outlive the skip — the key is REMOVED (Null), not left
            // behind as "".
            //
            // `boarded_head` rides here too, and lives on the CAR
            // rather than in a second list on the train: the sweep
            // already fetches every boarded car, so the fact stays in
            // one place (guideline 9a) and costs no extra call. It is
            // rewritten on every boarding, so a car that rides a later
            // train carries that train's head, not the first one's.
            self.merge_job_metadata(
                job_id(j)?,
                vec![
                    ("train", json!(train_id.as_str())),
                    ("boarded_head", json!(head.as_str())),
                    ("skip_reason", Value::Null),
                ],
            )
            .await?;
        }
        log(format!(
            "train {} boarded {}, PR {pr_url}",
            id8(&train_id),
            boarded.len()
        ));
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Cancel — the operator's judgment on a train that will not arrive
    // -----------------------------------------------------------------------

    /// Cancel an open train (David's ask: trains that don't arrive
    /// were cleaned up by hand, and cancellation orphaned the cars).
    /// Car-release comes FIRST, so a crash mid-cancel leaves cars
    /// free rather than orphaned:
    ///   1. release every still-open boarded car back to the dock —
    ///      review step back to `ready` (the dock predicate requires
    ///      it; clearing metadata alone re-boards nothing),
    ///      `metadata.train` removed, `skip_reason` saying why;
    ///   2. close the PR unmerged;
    ///   3. complete the `cancelled` terminal with the reason —
    ///      jobs-api then closes the Job with outcome=cancelled and
    ///      skips the remaining steps;
    ///   4. delete the train's OWN `train/*` branch — never a car's:
    ///      the cars keep their branches (train_branch_to_delete is
    ///      the pin, and it is tested).
    /// The operator's verb. Never counts a red against the cars — an
    /// operator cancels for reasons of their own (a bad consist, a
    /// withdrawn change), and only the automatic red-stall path below
    /// has evidence that the CARS were implicated.
    async fn cancel(&self, handle: &str, reason: &str) -> Result<()> {
        self.cancel_train(handle, reason, false).await
    }

    async fn cancel_train(&self, handle: &str, reason: &str, count_red: bool) -> Result<()> {
        let listed = rows(
            self.api(
                Method::GET,
                "/api/jobs?kind=pr-train&status=open&limit=50",
                None,
            )
            .await?,
        )?;
        let mut trains = Vec::with_capacity(listed.len());
        for t0 in &listed {
            trains.push(self.get_job(job_id(t0)?).await?);
        }
        let train = resolve_train(&trains, handle)?;
        let tid = job_id(train)?;

        let boarded: Vec<String> = train
            .get("metadata")
            .and_then(|m| m.get("boarded_jobs"))
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let mut cars = Vec::with_capacity(boarded.len());
        for cid in &boarded {
            cars.push(self.get_job(cid).await?);
        }
        for car in releasable_cars(&cars) {
            let cid = job_id(car)?;
            // NOTHING TO REOPEN. Releasing a car is a metadata write and
            // only a metadata write, because boarding no longer completes
            // its `review` step — see the boarding loop. This used to PUT
            // the step back to `ready`, which the row silently refused
            // (terminal steps are frozen in `update_step_at`) and which
            // now 409s out loud, taking the whole cancel with it. A car
            // that predates this change still carries a completed review
            // and cannot be released; those were translated into fresh
            // packets by hand on 2026-08-15 rather than reversed.
            //
            // The boarded head goes with the train stamp: this car
            // boarded nothing now, and a stale head is not evidence
            // about whatever it boards next.
            let mut stamps = vec![
                ("train", Value::Null),
                ("boarded_head", Value::Null),
                (
                    "skip_reason",
                    json!(format!("returned to dock: train cancelled ({reason})")),
                ),
            ];
            // A red release counts against the car. Every car aboard is
            // counted, not just the guilty one — which car turned the
            // consist red is exactly what nobody knows yet. One red is
            // survivable (see `car_hold_reason`); it takes a second,
            // aboard a DIFFERENT consist, before boarding holds it.
            if count_red {
                let reds = car
                    .get("metadata")
                    .and_then(|m| m.get("red_trains"))
                    .and_then(Value::as_i64)
                    .unwrap_or(0)
                    + 1;
                stamps.push(("red_trains", json!(reds)));
            }
            self.merge_job_metadata(cid, stamps).await?;
            log(format!("released car {} back to the dock", id8(cid)));
        }

        let pr_url = find_step(train, "pr", "Open the batched PR")
            .and_then(|s| s.get("metadata"))
            .and_then(|m| m.get("pr_url"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !pr_url.is_empty() {
            if self.cfg.dry {
                log(format!("DRY: would close {pr_url} unmerged"));
            } else {
                self.forge.close_pr(pr_url).await?;
                log(format!("closed {pr_url} unmerged"));
            }
        }

        // The cancelled terminal is gated (blocked_by) on collect; a
        // train that died mid-assembly never completed it. Close that
        // gate honestly first — nothing boarded on the record.
        let collect = find_step(train, "collect", "Collect what is ready to board");
        if !step_done(collect) {
            self.complete_step(
                train,
                collect,
                &[(
                    "boarded",
                    Some("nothing — train cancelled before boarding completed".to_string()),
                )],
            )
            .await?;
        }
        self.complete_step(
            train,
            find_step(train, "cancelled", "Cancelled — nothing to board"),
            &[("reason", Some(reason.to_string()))],
        )
        .await?;

        if let Some(branch) = train_branch_to_delete(train) {
            if self.cfg.dry {
                log(format!(
                    "DRY: would delete branch {branch} (train cancelled)"
                ));
            } else if self.forge.delete_branch(&branch).await? {
                log(format!("deleted branch {branch} (train cancelled)"));
            } else {
                log(format!("branch {branch} already gone (train cancelled)"));
            }
        }
        log(format!("train {} cancelled: {reason}", id8(tid)));
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Entry
// ---------------------------------------------------------------------------

pub async fn run(phase: Phase, dry: bool, now: DateTime<Utc>) -> Result<()> {
    let cfg = Config::from_env(dry);
    // The forge adapter is built before anything else — the python
    // conductor constructed FORGE at import, so a misconfigured
    // BOSS_TRAIN_FORGE fails every entry loudly, not just the boarding
    // that needed it.
    let forge = make_forge(&cfg)?;
    fs::create_dir_all(&cfg.home)?;
    let lock = File::create(Path::new(&cfg.home).join("lock"))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            // A held lock means a conductor run is active right now — the
            // locomotive is demonstrably pulling; a standalone pre-flight
            // has nothing further to prove.
            log("another conductor run holds the lock — leaving");
            return Ok(());
        }
        Err(TryLockError::Error(e)) => {
            return Err(e).context("locking the conductor's lock file");
        }
    }
    let problems = preflight(&cfg)?;
    if !problems.is_empty() {
        for p in &problems {
            log(format!("preflight FAIL: {p}"));
        }
        // Exit 3 — distinct from a crash, loud in the unit's status.
        // (The lock releases with the process; destructors are moot.)
        std::process::exit(3);
    }
    log("preflight ok");
    if matches!(phase, Phase::Preflight) {
        return Ok(());
    }
    let conductor = Conductor::new(cfg, forge)?;
    match phase {
        Phase::Preflight => {} // returned above; the arm keeps the match total
        Phase::Reconcile => conductor.reconcile(now).await?,
        Phase::Board => conductor.board(now).await?,
        Phase::Run => {
            conductor.reconcile(now).await?;
            conductor.board(now).await?;
        }
        Phase::Cancel { handle, reason } => conductor.cancel(&handle, &reason).await?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------
    // Re-railing — the conductor's answer to a squash-merged base.
    // ---------------------------------------------------------------

    /// The exact shape that cost four hand re-rails on 2026-08-15: a
    /// car whose work is partly in main already, because the branch it
    /// was cut from was SQUASH-merged and so is not an ancestor of
    /// main. Merging re-applies the landed hunk onto itself; rebasing
    /// recognises it as already applied and drops it.
    #[test]
    fn a_car_whose_base_was_squash_merged_is_re_railed_not_skipped() {
        let dir = std::env::temp_dir().join(format!("boss-rerail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let d = dir.to_str().unwrap();
        let git = |args: &[&str]| {
            let mut a = vec!["git", "-C", d];
            a.extend_from_slice(args);
            let out = sh_unchecked(&a).unwrap();
            assert!(out.status.success(), "git {args:?}: {}", stdout_str(&out));
        };
        let write = |name: &str, body: &str| {
            std::fs::write(dir.join(name), body).unwrap();
        };

        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        write("base.txt", "base\n");
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "base"]);

        // The parent car adds a line; the child car is cut from it and
        // adds another to the SAME file.
        git(&["checkout", "-q", "-b", "parent"]);
        write("shared.txt", "from parent\n");
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "parent work"]);
        git(&["checkout", "-q", "-b", "child"]);
        write("shared.txt", "from parent\nfrom child\n");
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "child work"]);

        // The parent lands as a SQUASH — new sha, same content, and
        // `parent` is now not an ancestor of main.
        git(&["checkout", "-q", "main"]);
        git(&["merge", "-q", "--squash", "parent"]);
        git(&["commit", "-q", "-m", "train: parent (squashed)"]);

        // The conductor's world: fork/<branch> refs and a train branch
        // cut from main.
        git(&["update-ref", "refs/remotes/fork/child", "child"]);
        git(&["checkout", "-q", "-B", "train", "main"]);

        // A plain merge collides on the line the squash already landed.
        let merged = sh_unchecked(&[
            "git",
            "-C",
            d,
            "merge",
            "--no-ff",
            "-m",
            "train: merge child",
            "fork/child",
        ])
        .unwrap();
        assert!(
            !merged.status.success(),
            "the bug only exists because this merge conflicts"
        );
        sh_unchecked(&["git", "-C", d, "merge", "--abort"]).unwrap();

        // Re-railing replays only the child's own commit and lands it.
        let rerailed = rerail_onto_consist(d, "train", "child")
            .unwrap()
            .expect("a squash-merged base is exactly what rebase resolves");
        let retry = sh_unchecked(&[
            "git",
            "-C",
            d,
            "merge",
            "--no-ff",
            "-m",
            "train: merge child (re-railed)",
            &rerailed,
        ])
        .unwrap();
        assert!(retry.status.success(), "re-railed car must merge cleanly");

        let body = std::fs::read_to_string(dir.join("shared.txt")).unwrap();
        assert_eq!(
            body, "from parent\nfrom child\n",
            "the child's work lands on top of the parent's, once"
        );

        // And the clone is left on the train branch, ready for the next
        // car in the loop.
        let head = sh_unchecked(&["git", "-C", d, "rev-parse", "--abbrev-ref", "HEAD"]).unwrap();
        assert_eq!(stdout_str(&head).trim(), "train");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A car that genuinely disagrees with the consist still gets
    /// skipped — re-railing must not paper over a real conflict.
    #[test]
    fn a_real_conflict_still_refuses_to_re_rail() {
        let dir = std::env::temp_dir().join(format!("boss-rerail-real-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let d = dir.to_str().unwrap();
        let git = |args: &[&str]| {
            let mut a = vec!["git", "-C", d];
            a.extend_from_slice(args);
            let out = sh_unchecked(&a).unwrap();
            assert!(out.status.success(), "git {args:?}: {}", stdout_str(&out));
        };

        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        std::fs::write(dir.join("f.txt"), "original\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "base"]);

        git(&["checkout", "-q", "-b", "car"]);
        std::fs::write(dir.join("f.txt"), "the car's answer\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "car"]);

        git(&["checkout", "-q", "main"]);
        std::fs::write(dir.join("f.txt"), "a different answer\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "someone else"]);

        git(&["update-ref", "refs/remotes/fork/car", "car"]);
        git(&["checkout", "-q", "-B", "train", "main"]);

        assert!(
            rerail_onto_consist(d, "train", "car").unwrap().is_none(),
            "two answers to the same line is a conflict a human owns"
        );
        let head = sh_unchecked(&["git", "-C", d, "rev-parse", "--abbrev-ref", "HEAD"]).unwrap();
        assert_eq!(
            stdout_str(&head).trim(),
            "train",
            "a failed re-rail must still leave the clone usable"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
    use super::{
        ApiFailure, Failure, JOBS_API_RETRY, MAX_RED_TRAINS, RetryPolicy, SweepGuard,
        arrival_report, arrival_summary, auto_cancel_reason, boarded_head, branch_moved_line,
        car_hold_reason, ci_overdue, classify_transport, deletable_branches, deploy_needed,
        local_jobs_problem, overlay_metadata, parked_ready, releasable_cars, repo_path,
        resolve_train, retryable, retrying, short_cause, skip_reason_branch_missing,
        skip_reason_conflict, stall_age_hours, sweep_guard, sweep_note, sweep_settled,
        train_branch_to_delete, verdict_drift,
    };
    use anyhow::{Result, anyhow};
    use chrono::{DateTime, Utc};
    use reqwest::Method;
    use serde_json::{Value, json};
    use std::cell::Cell;
    use std::collections::BTreeSet;
    use std::time::Duration;

    /// A car parked at review, branch pushed, not on a train.
    fn ready_car() -> serde_json::Value {
        json!({
            "id": "car-1",
            "metadata": {"branch": "feat/x"},
            "steps": [
                {"spec_slug": "review", "title": "Open for review", "status": "ready"}
            ],
        })
    }

    #[test]
    fn a_car_at_review_with_a_branch_is_parked_ready() {
        assert!(parked_ready(&ready_car()));
        let mut active = ready_car();
        active["steps"][0]["status"] = json!("active");
        assert!(parked_ready(&active));
    }

    #[test]
    fn a_car_without_a_branch_is_not_ready() {
        let mut j = ready_car();
        j["metadata"] = json!({});
        assert!(!parked_ready(&j));
        j["metadata"] = json!({"branch": ""});
        assert!(!parked_ready(&j));
    }

    #[test]
    fn a_car_already_on_a_train_is_not_ready() {
        let mut j = ready_car();
        j["metadata"]["train"] = json!("train-job-id");
        assert!(!parked_ready(&j));
        // A train's own branch is never a car either.
        let mut t = ready_car();
        t["metadata"]["branch"] = json!("train/20260812-0600");
        assert!(!parked_ready(&t));
    }

    #[test]
    fn a_released_car_is_parked_ready_again() {
        // THE INVARIANT THAT MAKES CANCELLING A LOADED TRAIN POSSIBLE.
        // Releasing a car clears `train` and nothing else, so the car
        // must be boardable on that write alone. It is — as long as
        // boarding left the review step ready.
        let mut boarded = ready_car();
        boarded["metadata"]["train"] = json!("train-job-id");
        boarded["metadata"]["boarded_head"] = json!("abc1234");
        assert!(!parked_ready(&boarded));

        let mut released = boarded.clone();
        released["metadata"]["train"] = Value::Null;
        released["metadata"]["boarded_head"] = Value::Null;
        assert!(
            parked_ready(&released),
            "a released car must re-enter the dock on the metadata write alone"
        );

        // And the reason boarding must NOT complete the step: a
        // completed review is frozen at the row, so a released car
        // carrying one could never board again and the cancel would be
        // a lie.
        let mut released_but_reviewed = released.clone();
        released_but_reviewed["steps"][0]["status"] = json!("completed");
        assert!(!parked_ready(&released_but_reviewed));
    }

    #[test]
    fn a_car_not_yet_at_review_is_not_ready() {
        let mut j = ready_car();
        j["steps"][0]["status"] = json!("pending");
        assert!(!parked_ready(&j));
        j["steps"][0]["status"] = json!("completed");
        assert!(!parked_ready(&j));
        // No review step at all.
        j["steps"] = json!([]);
        assert!(!parked_ready(&j));
    }

    // -- the branch-sweep decision at arrival ------------------------------
    //
    // Train PRs squash-merge, so git ancestry can never prove a car's
    // content landed; the JOB RECORD is the proof (protocol decision,
    // David). These pin exactly which branches the conductor may
    // delete once a train has arrived.

    /// A boarded car whose bookkeeping completed: closed with the
    /// `merged` outcome stamped by the terminal close.
    fn landed_car(id: &str, branch: &str) -> serde_json::Value {
        json!({
            "id": id,
            "status": "closed",
            "metadata": {"branch": branch, "outcome": "merged", "merged": "true"},
        })
    }

    fn no_open() -> BTreeSet<String> {
        BTreeSet::new()
    }

    #[test]
    fn a_landed_cars_branch_is_deletable() {
        let cars = vec![landed_car("car-1", "feat/x")];
        assert_eq!(
            deletable_branches(&cars, &no_open()),
            vec![("feat/x".to_string(), "car-1".to_string())]
        );
    }

    #[test]
    fn a_car_still_open_keeps_its_branch() {
        // Bookkeeping incomplete — the dispatcher has not closed the
        // car yet, whatever the train did.
        let mut car = landed_car("car-1", "feat/x");
        car["status"] = json!("open");
        assert!(deletable_branches(&[car], &no_open()).is_empty());
    }

    #[test]
    fn an_abandoned_car_keeps_its_branch() {
        // Abandoned cars close too — but their branch holds unmerged
        // work. Only the `merged` outcome is landing evidence.
        let mut car = landed_car("car-1", "feat/x");
        car["metadata"]["outcome"] = json!("abandoned");
        assert!(deletable_branches(&[car], &no_open()).is_empty());
    }

    #[test]
    fn a_closed_car_without_an_outcome_keeps_its_branch() {
        // Closed by hand, no terminal outcome on the record: not proof.
        let mut car = landed_car("car-1", "feat/x");
        car["metadata"].as_object_mut().unwrap().remove("outcome");
        assert!(deletable_branches(&[car], &no_open()).is_empty());
    }

    #[test]
    fn main_is_never_deletable() {
        let cars = vec![landed_car("car-1", "main")];
        assert!(deletable_branches(&cars, &no_open()).is_empty());
    }

    #[test]
    fn a_branch_a_still_open_car_names_survives() {
        // A follow-up car may ride a landed car's branch; the open
        // car's claim wins.
        let open: BTreeSet<String> = ["feat/x".to_string()].into();
        let cars = vec![landed_car("car-1", "feat/x")];
        assert!(deletable_branches(&cars, &open).is_empty());
    }

    #[test]
    fn a_car_without_a_branch_contributes_nothing() {
        let empty = landed_car("car-1", "");
        assert!(deletable_branches(&[empty], &no_open()).is_empty());
        let mut none = landed_car("car-2", "feat/x");
        none["metadata"] = json!({"outcome": "merged"});
        assert!(deletable_branches(&[none], &no_open()).is_empty());
    }

    #[test]
    fn two_landed_cars_on_one_branch_delete_it_once() {
        let cars = vec![landed_car("car-1", "feat/x"), landed_car("car-2", "feat/x")];
        assert_eq!(
            deletable_branches(&cars, &no_open()),
            vec![("feat/x".to_string(), "car-1".to_string())]
        );
    }

    #[test]
    fn the_sweep_settles_only_when_every_boarded_car_is_terminal() {
        let landed = landed_car("car-1", "feat/x");
        let mut still_open = landed_car("car-2", "feat/y");
        still_open["status"] = json!("open");
        let mut cancelled = landed_car("car-3", "feat/z");
        cancelled["status"] = json!("cancelled");
        assert!(sweep_settled(std::slice::from_ref(&landed)));
        assert!(sweep_settled(&[landed.clone(), cancelled]));
        assert!(!sweep_settled(&[landed, still_open]));
        // Nothing boarded is trivially settled.
        assert!(sweep_settled(&[]));
    }

    // -- the skip reason on the car job ------------------------------------
    //
    // Train #8 conflict-skipped three cars; the journal said "left for
    // the next train" but the car Jobs carried nothing, so the yard's
    // dock showed them unexplained. The PacketCard chip renders
    // `metadata.skip_reason` ("LEFT BEHIND — <reason>"), so the string
    // stays short: a truncated file list, or the missing branch.

    #[test]
    fn a_conflict_skip_reason_names_the_files() {
        let files = vec!["src/a.rs".to_string(), "src/b.rs".to_string()];
        assert_eq!(skip_reason_conflict(&files), "conflict: src/a.rs, src/b.rs");
    }

    #[test]
    fn a_long_conflict_list_truncates_with_a_count() {
        let files: Vec<String> = (0..20)
            .map(|i| format!("crates/core/boss-jobs/src/file_{i:02}.rs"))
            .collect();
        let reason = skip_reason_conflict(&files);
        assert!(
            reason.starts_with("conflict: crates/core/boss-jobs/src/file_00.rs"),
            "leads with the first file: {reason}"
        );
        assert!(reason.ends_with("+18 more"), "counts what it hid: {reason}");
        assert!(
            reason.len() <= 120,
            "stays chip-sized ({} chars): {reason}",
            reason.len()
        );
    }

    #[test]
    fn one_huge_conflict_file_is_still_named() {
        // Truncation drops files, never the whole answer: at least one
        // file always shows.
        let long = format!("crates/{}.rs", "x".repeat(150));
        let reason = skip_reason_conflict(std::slice::from_ref(&long));
        assert_eq!(reason, format!("conflict: {long}"));
    }

    #[test]
    fn a_merge_that_died_before_markers_says_so() {
        assert_eq!(
            skip_reason_conflict(&[]),
            "conflict: unresolved (merge died before conflict markers)"
        );
    }

    /// The dock-depth metric and the boardable count must answer the
    /// same question. On 2026-08-14 they did not: `parked_ready` said
    /// 12 while `candidates` boarded 0, because every branch had been
    /// pushed upstream and none to the fork. The conductor now closes
    /// that gap by copying the ref, so this reason is reserved for a
    /// branch that exists in NEITHER place — a car never pushed at all.
    #[test]
    fn a_branch_missing_everywhere_is_still_a_real_skip() {
        assert_eq!(
            skip_reason_branch_missing("feat/never-pushed"),
            "branch feat/never-pushed not on fork",
            "the skip survives for the genuine case: nothing to copy"
        );
    }

    #[test]
    fn a_missing_branch_skip_reason_names_the_branch() {
        assert_eq!(
            skip_reason_branch_missing("feat/x"),
            "branch feat/x not on fork"
        );
    }

    // -- metadata overlays merge, never clobber ----------------------------
    //
    // jobs-api PUT replaces top-level `metadata` wholesale; every
    // update must carry the existing keys forward, and clearing a key
    // means removing it, not writing "".

    #[test]
    fn a_metadata_overlay_preserves_existing_keys() {
        let job = json!({"metadata": {"branch": "feat/x", "queue": "q-1"}});
        let md = overlay_metadata(&job, vec![("skip_reason", json!("conflict: a.rs"))]);
        assert_eq!(md.get("branch"), Some(&json!("feat/x")));
        assert_eq!(md.get("queue"), Some(&json!("q-1")));
        assert_eq!(md.get("skip_reason"), Some(&json!("conflict: a.rs")));
    }

    #[test]
    fn a_null_overlay_removes_the_key() {
        // Boarding stamps `train` and sheds the stale skip note in one
        // update; the key goes away rather than lingering as "".
        let job = json!({"metadata": {"branch": "feat/x", "skip_reason": "conflict: a.rs"}});
        let md = overlay_metadata(
            &job,
            vec![("train", json!("t-1")), ("skip_reason", Value::Null)],
        );
        assert!(!md.contains_key("skip_reason"));
        assert_eq!(md.get("train"), Some(&json!("t-1")));
        assert_eq!(md.get("branch"), Some(&json!("feat/x")));
    }

    #[test]
    fn an_overlay_on_a_bare_job_starts_fresh() {
        let job = json!({"id": "j-1"});
        let md = overlay_metadata(&job, vec![("skip_reason", json!("x"))]);
        assert_eq!(md.len(), 1);
        // Removing a key that was never there is a quiet no-op.
        let md = overlay_metadata(&job, vec![("skip_reason", Value::Null)]);
        assert!(md.is_empty());
    }

    // -- the drift sentinel (split-brain incident c4b4a6b0) ----------------
    //
    // BOSS_JOBS_URL defaulted to localhost and the conductor silently
    // booked a whole window on the wrong instance. Preflight goes red
    // on a loopback jobs URL unless the box says it means it.

    #[test]
    fn a_loopback_jobs_url_is_a_preflight_problem() {
        for url in [
            "http://127.0.0.1:7900",
            "http://localhost:7900",
            "http://LOCALHOST:7900",
            "http://[::1]:7900",
            "http://127.9.9.9/api",
        ] {
            let p = local_jobs_problem(url, false)
                .unwrap_or_else(|| panic!("{url} must trip the sentinel"));
            assert!(p.contains("BOSS_JOBS_URL"), "names the env var: {p}");
            assert!(
                p.contains("BOSS_TRAIN_ALLOW_LOCAL_JOBS"),
                "names the override: {p}"
            );
            assert!(
                p.contains("system of record"),
                "names the incident class: {p}"
            );
        }
    }

    #[test]
    fn the_allowance_and_remote_jobs_urls_pass_the_sentinel() {
        // The allowance is the deliberate test/demo-box escape hatch.
        assert!(local_jobs_problem("http://127.0.0.1:7900", true).is_none());
        assert!(local_jobs_problem("http://10.20.0.15:7900", false).is_none());
        assert!(local_jobs_problem("https://jobs.boss.internal/api", false).is_none());
    }

    // -- the arrival report ------------------------------------------------
    //
    // The landing's final structured entry: when the sweep visits an
    // arrived train, it composes what the record proves — the consist,
    // who got left behind, the generation, and the timings the
    // conductor's own `completed_at` stamps make derivable — and files
    // it on the `arrived` step. Missing evidence reads as null, never
    // a guess.

    fn arrived_train() -> serde_json::Value {
        json!({
            "id": "train-77",
            "status": "closed",
            "metadata": {
                "boarded_jobs": ["car-1", "car-2"],
                "left_behind": [
                    {"car_id_short": "car-3-id", "reason": "conflict: src/a.rs"}
                ],
            },
            "steps": [
                {"spec_slug": "collect", "title": "Collect what is ready to board",
                 "status": "completed",
                 "metadata": {"completed_at": "2026-08-13T06:00:00Z"}},
                {"spec_slug": "merged", "title": "Merged into main",
                 "status": "completed",
                 "metadata": {"completed_at": "2026-08-13T06:05:00Z",
                              "merge_ref": "abc1234def56"}},
                {"spec_slug": "deployed", "title": "Deployed to the playground",
                 "status": "completed",
                 "metadata": {"completed_at": "2026-08-13T06:12:00Z",
                              "deployed": "main@abc1234; 0 applied; services: prod; web: deployed"}},
                {"spec_slug": "arrived", "title": "Train arrived",
                 "status": "completed",
                 "metadata": {"completed_at": "2026-08-13T06:20:00Z"}},
            ],
        })
    }

    fn boarded_cars() -> Vec<serde_json::Value> {
        vec![
            json!({"id": "car-1-uuid-long", "title": "Fix the thing",
                   "metadata": {"branch": "feat/x"}}),
            json!({"id": "car-2-uuid-long", "title": "Add the widget",
                   "metadata": {"branch": "feat/y"}}),
        ]
    }

    #[test]
    fn the_arrival_report_carries_consist_left_behind_and_timings() {
        let report = arrival_report(&arrived_train(), &boarded_cars());
        assert_eq!(
            report["consist"],
            json!([
                {"car_id_short": "car-1-uu", "title": "Fix the thing", "branch": "feat/x"},
                {"car_id_short": "car-2-uu", "title": "Add the widget", "branch": "feat/y"},
            ])
        );
        assert_eq!(
            report["left_behind"],
            json!([{"car_id_short": "car-3-id", "reason": "conflict: src/a.rs"}])
        );
        assert_eq!(report["generation"], json!("abc1234"));
        // merge_ref abc1234def56 IS the deployed generation (short sha
        // prefix) — not distinct, so no merged_sha key.
        assert!(report.get("merged_sha").is_none(), "same commit: {report}");
        assert_eq!(
            report["timings"]["boarded_at"],
            json!("2026-08-13T06:00:00Z")
        );
        assert_eq!(
            report["timings"]["merged_at"],
            json!("2026-08-13T06:05:00Z")
        );
        assert_eq!(
            report["timings"]["deployed_at"],
            json!("2026-08-13T06:12:00Z")
        );
        assert_eq!(
            report["timings"]["arrived_at"],
            json!("2026-08-13T06:20:00Z")
        );
        assert_eq!(report["timings"]["board_to_merge_s"], json!(300));
        assert_eq!(report["timings"]["merge_to_deploy_s"], json!(420));
        assert_eq!(report["timings"]["total_s"], json!(1200));
    }

    #[test]
    fn a_distinct_merge_sha_is_reported() {
        let mut train = arrived_train();
        train["steps"][2]["metadata"]["deployed"] =
            json!("main@999aaaa; 0 applied; services: prod; web: deployed");
        let report = arrival_report(&train, &boarded_cars());
        assert_eq!(report["generation"], json!("999aaaa"));
        assert_eq!(report["merged_sha"], json!("abc1234def56"));
    }

    #[test]
    fn missing_evidence_reads_as_null_never_a_guess() {
        // A train whose steps carry no completed_at stamps (they
        // predate the stamping, or the dispatcher closed `arrived`)
        // and whose deploy summary is absent.
        let train = json!({
            "id": "train-78",
            "status": "closed",
            "metadata": {"boarded_jobs": ["car-1"]},
            "steps": [
                {"spec_slug": "collect", "title": "Collect what is ready to board",
                 "status": "completed", "metadata": {}},
                {"spec_slug": "merged", "title": "Merged into main",
                 "status": "completed", "metadata": {}},
                {"spec_slug": "arrived", "title": "Train arrived",
                 "status": "completed", "metadata": {}},
            ],
        });
        let report = arrival_report(&train, &boarded_cars());
        assert_eq!(report["left_behind"], json!([]));
        assert_eq!(report["generation"], Value::Null);
        // No deployed sha to compare against — the merge evidence is
        // absent too, so no merged_sha key appears.
        assert!(report.get("merged_sha").is_none());
        assert_eq!(report["timings"]["boarded_at"], Value::Null);
        assert_eq!(report["timings"]["arrived_at"], Value::Null);
        assert_eq!(report["timings"]["board_to_merge_s"], Value::Null);
        assert_eq!(report["timings"]["merge_to_deploy_s"], Value::Null);
        assert_eq!(report["timings"]["total_s"], Value::Null);
    }

    #[test]
    fn the_summary_reads_the_report_not_the_world() {
        let full = arrival_report(&arrived_train(), &boarded_cars());
        assert_eq!(
            arrival_summary(&full),
            "2 cars; generation abc1234; total 1200s"
        );
        let bare = arrival_report(&json!({"id": "t", "metadata": {}, "steps": []}), &[]);
        assert_eq!(
            arrival_summary(&bare),
            "0 cars; generation unknown; total ?s"
        );
    }

    // -- the stall sentinel ------------------------------------------------
    //
    // A train counts stalled when open and its newest step completion
    // is older than the threshold. Raising is protocol, cancelling is
    // judgment — the sentinel only makes the stall visible.

    fn ts(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    fn train_with_stamps(stamps: &[&str]) -> serde_json::Value {
        let steps: Vec<serde_json::Value> = stamps
            .iter()
            .map(|t| json!({"status": "completed", "metadata": {"completed_at": t}}))
            .collect();
        json!({"id": "t-1", "status": "open", "metadata": {}, "steps": steps})
    }

    #[test]
    fn a_train_past_the_threshold_counts_stalled() {
        let t = train_with_stamps(&["2026-08-13T00:00:00Z"]);
        assert_eq!(stall_age_hours(&t, ts("2026-08-13T08:30:00Z"), 6), Some(8));
        // The boundary counts: exactly at the threshold is stalled.
        assert_eq!(stall_age_hours(&t, ts("2026-08-13T06:00:00Z"), 6), Some(6));
    }

    #[test]
    fn a_train_inside_the_threshold_is_not_stalled() {
        let t = train_with_stamps(&["2026-08-13T00:00:00Z"]);
        assert_eq!(stall_age_hours(&t, ts("2026-08-13T05:59:00Z"), 6), None);
    }

    #[test]
    fn the_newest_completion_is_the_stall_basis() {
        // Unordered stamps: the NEWEST one anchors the age (3h ago),
        // not the oldest (30h ago).
        let t = train_with_stamps(&["2026-08-12T00:00:00Z", "2026-08-13T03:00:00Z"]);
        assert_eq!(stall_age_hours(&t, ts("2026-08-13T06:00:00Z"), 6), None);
        assert_eq!(stall_age_hours(&t, ts("2026-08-13T09:00:00Z"), 6), Some(6));
    }

    #[test]
    fn a_train_without_stamps_never_counts_stalled() {
        // No completion evidence, no basis — the sentinel never
        // guesses an age.
        let t = train_with_stamps(&[]);
        assert_eq!(stall_age_hours(&t, ts("2026-08-13T06:00:00Z"), 6), None);
    }

    // -- auto-cancelling a red train ---------------------------------------
    //
    // The overnight rule: a train that is red AND has stopped moving
    // releases its consist rather than holding it until morning.

    fn red_train(stamps: &[&str], merged: bool) -> serde_json::Value {
        let mut steps: Vec<serde_json::Value> = stamps
            .iter()
            .map(|t| json!({"status": "completed", "metadata": {"completed_at": t}}))
            .collect();
        steps.push(json!({
            "spec_slug": "merged",
            "title": "Merged into main",
            "status": if merged { "completed" } else { "ready" },
            "metadata": {}
        }));
        json!({"id": "t-1", "status": "open", "metadata": {}, "steps": steps})
    }

    #[test]
    fn a_red_train_that_stopped_moving_is_auto_cancelled() {
        let t = red_train(&["2026-08-13T00:00:00Z"], false);
        let r = auto_cancel_reason(&t, "failing", ts("2026-08-13T08:00:00Z"), 6);
        assert!(r.is_some(), "red and 8h stalled should cancel");
        assert!(r.unwrap().contains("8h"), "the reason carries the age");
    }

    #[test]
    fn a_red_train_inside_the_threshold_is_left_alone() {
        // Still young enough that a re-run or a repair may yet save it.
        let t = red_train(&["2026-08-13T00:00:00Z"], false);
        assert_eq!(
            auto_cancel_reason(&t, "failing", ts("2026-08-13T05:00:00Z"), 6),
            None
        );
    }

    #[test]
    fn a_stalled_train_under_repair_is_not_cancelled() {
        // THE REGRESSION THIS EXISTS FOR: a repair has been pushed and
        // CI is re-running, so the LIVE verdict is `pending` even
        // though the train's own `ci` step still reads `failing` from
        // the first run. Deciding from the step would cancel the train
        // the repair was about to save.
        let t = red_train(&["2026-08-13T00:00:00Z"], false);
        assert_eq!(
            auto_cancel_reason(&t, "pending", ts("2026-08-13T09:00:00Z"), 6),
            None
        );
    }

    #[test]
    fn a_green_stalled_train_is_never_auto_cancelled() {
        // Green and stalled means waiting on the merge, not broken —
        // cancelling would throw away a consist that is about to land.
        let t = red_train(&["2026-08-13T00:00:00Z"], false);
        assert_eq!(
            auto_cancel_reason(&t, "green", ts("2026-08-13T09:00:00Z"), 6),
            None
        );
    }

    #[test]
    fn a_merged_train_is_never_auto_cancelled() {
        // The content landed; red post-merge checks are not the
        // consist's problem and its cars must not be released.
        let t = red_train(&["2026-08-13T00:00:00Z"], true);
        assert_eq!(
            auto_cancel_reason(&t, "failing", ts("2026-08-13T09:00:00Z"), 6),
            None
        );
    }

    // -- the CI verdict blind spot -----------------------------------------

    #[test]
    fn a_verdict_that_moves_after_recording_is_reported() {
        // The 2026-08-15 case: recorded failing, repaired, red again.
        // Nothing in the system said so for 45 minutes.
        assert!(verdict_drift(Some("failing"), "green").is_some());
        let note = verdict_drift(Some("green"), "failing").expect("green -> failing is a change");
        assert!(note.contains("green"), "the note names where it came from");
        assert!(note.contains("failing"), "and where it went");
    }

    #[test]
    fn an_unchanged_verdict_is_silent() {
        // Reconcile runs every ten minutes; a verdict that has not moved
        // must not produce a line each time or the signal is noise.
        assert_eq!(verdict_drift(Some("failing"), "failing"), None);
        assert_eq!(verdict_drift(Some("green"), "green"), None);
    }

    #[test]
    fn pending_is_not_a_change() {
        // A re-run passes through pending on its way to an answer.
        // Reporting it would fire on every repair, twice.
        assert_eq!(verdict_drift(Some("failing"), "pending"), None);
    }

    #[test]
    fn nothing_recorded_yet_is_not_drift() {
        // Before the step completes, the ordinary path records the
        // first verdict; this is only about the ones after it.
        assert_eq!(verdict_drift(None, "failing"), None);
    }

    #[test]
    fn ci_that_never_answers_is_reported_after_the_threshold() {
        // The case drift cannot see: no verdict at all, so there is
        // nothing to compare against.
        let t = json!({"id":"t-1","status":"open","metadata":{},"steps":[
            {"spec_slug":"pr","title":"Open the batched PR","status":"completed",
             "metadata":{"completed_at":"2026-08-15T06:00:00Z"}},
            {"spec_slug":"ci","title":"CI verdict","status":"ready","metadata":{}}
        ]});
        assert!(ci_overdue(&t, ts("2026-08-15T08:00:00Z"), 2).is_some());
        assert_eq!(ci_overdue(&t, ts("2026-08-15T07:30:00Z"), 2), None);
    }

    #[test]
    fn an_answered_ci_is_never_overdue() {
        // Red counts as answered. A red train is the stall sentinel's
        // problem and auto-cancel's; this signal is only about silence.
        let t = json!({"id":"t-1","status":"open","metadata":{},"steps":[
            {"spec_slug":"pr","title":"Open the batched PR","status":"completed",
             "metadata":{"completed_at":"2026-08-15T06:00:00Z"}},
            {"spec_slug":"ci","title":"CI verdict","status":"completed",
             "metadata":{"result":"failing","completed_at":"2026-08-15T06:20:00Z"}}
        ]});
        assert_eq!(ci_overdue(&t, ts("2026-08-15T20:00:00Z"), 2), None);
    }

    #[test]
    fn a_train_with_no_pr_yet_is_not_overdue() {
        // Nothing has been asked, so nothing is unanswered — a train
        // stuck before its PR belongs to the stall sentinel.
        let t = json!({"id":"t-1","status":"open","metadata":{},"steps":[
            {"spec_slug":"pr","title":"Open the batched PR","status":"ready","metadata":{}},
            {"spec_slug":"ci","title":"CI verdict","status":"pending","metadata":{}}
        ]});
        assert_eq!(ci_overdue(&t, ts("2026-08-16T00:00:00Z"), 2), None);
    }

    // -- the two-strike hold -----------------------------------------------

    #[test]
    fn a_car_that_took_two_trains_red_is_held() {
        let car = json!({"id": "car-1", "metadata": {"red_trains": 2}});
        assert!(car_hold_reason(&car, MAX_RED_TRAINS).is_some());
    }

    #[test]
    fn a_car_with_one_red_still_boards() {
        // One red is usually a neighbour's fault — holding on the first
        // would quarantine innocent cars and stall the queue.
        let car = json!({"id": "car-1", "metadata": {"red_trains": 1}});
        assert_eq!(car_hold_reason(&car, MAX_RED_TRAINS), None);
        let fresh = json!({"id": "car-2", "metadata": {}});
        assert_eq!(car_hold_reason(&fresh, MAX_RED_TRAINS), None);
    }

    // -- cancelling a train ------------------------------------------------

    #[test]
    fn cancel_releases_only_the_still_open_cars() {
        let open = json!({"id": "car-1", "status": "open",
                          "metadata": {"train": "t-1", "branch": "feat/x"}});
        let landed = landed_car("car-2", "feat/y");
        let mut cancelled = landed_car("car-3", "feat/z");
        cancelled["status"] = json!("cancelled");
        let cars = vec![open, landed, cancelled];
        let released: Vec<&str> = releasable_cars(&cars)
            .iter()
            .map(|c| c.get("id").and_then(Value::as_str).unwrap())
            .collect();
        // Closed cars are history — merged or abandoned, not ours to
        // touch. Only the open car returns to the dock.
        assert_eq!(released, vec!["car-1"]);
    }

    #[test]
    fn cancel_deletes_only_the_trains_own_branch_never_a_cars() {
        let train = json!({
            "id": "t-1",
            "subject": {"subject_kind": "custom", "id": "train/20260813-0600"},
        });
        assert_eq!(
            train_branch_to_delete(&train),
            Some("train/20260813-0600".to_string())
        );
        // A subject that is not a train/* branch — whatever went
        // wrong upstream, the cancel path deletes NO car branch.
        let odd = json!({
            "id": "t-2",
            "subject": {"subject_kind": "custom", "id": "feat/x"},
        });
        assert_eq!(train_branch_to_delete(&odd), None);
        assert_eq!(train_branch_to_delete(&json!({"id": "t-3"})), None);
    }

    #[test]
    fn a_cancel_handle_resolves_by_id_prefix_or_pr_url() {
        let a = json!({
            "id": "aaaa1111-2222-3333-4444-555566667777",
            "steps": [{"spec_slug": "pr", "title": "Open the batched PR",
                       "status": "completed",
                       "metadata": {"pr_url": "http://forge/repo/pulls/9"}}],
        });
        let b = json!({"id": "bbbb1111-0000-0000-0000-000000000000", "steps": []});
        let trains = vec![a, b];
        assert_eq!(
            resolve_train(&trains, "aaaa1111-2222-3333-4444-555566667777")
                .unwrap()
                .get("id"),
            trains[0].get("id")
        );
        assert_eq!(
            resolve_train(&trains, "bbbb1111").unwrap().get("id"),
            trains[1].get("id")
        );
        assert_eq!(
            resolve_train(&trains, "http://forge/repo/pulls/9")
                .unwrap()
                .get("id"),
            trains[0].get("id")
        );
        assert!(resolve_train(&trains, "cccc0000").is_err(), "no match");
        // An ambiguous prefix refuses rather than guessing a train.
        let twins = vec![
            json!({"id": "aaaa1111-x", "steps": []}),
            json!({"id": "aaaa1111-y", "steps": []}),
        ];
        assert!(resolve_train(&twins, "aaaa1111").is_err(), "ambiguous");
    }

    // -- the deploy-needed decision ----------------------------------------
    //
    // Live incident: every 10-minute reconcile re-ran a full no-op
    // deploy — generation unchanged, services bounced anyway. The
    // store's `current` key is the 8-char release dirname; ls-remote
    // answers the FULL 40-char sha. full.starts_with(short) is the
    // match — that exact direction, pinned here with the real shapes.

    #[test]
    fn a_generation_already_serving_remote_main_skips_the_deploy() {
        let full = "c0020201aa5f3d9e8b7c6d5e4f3a2b1c0d9e8f7a";
        assert!(
            !deploy_needed("c0020201", full),
            "8-char store key vs 40-char remote sha must read as up to date"
        );
    }

    #[test]
    fn every_other_pair_deploys() {
        let full = "deadbeefaa5f3d9e8b7c6d5e4f3a2b1c0d9e8f7a";
        assert!(deploy_needed("c0020201", full), "different generations");
        // The reversed half-match must never read as up to date.
        assert!(deploy_needed(
            "c0020201aa5f3d9e8b7c6d5e4f3a2b1c0d9e8f7a",
            "c0020201"
        ));
        // Missing evidence on either side deploys — the deploy path
        // surfaces its own errors; a skip must never rest on absence.
        assert!(deploy_needed("", full));
        assert!(deploy_needed("c0020201", ""));
    }

    #[test]
    fn repo_path_reads_https_and_ssh_clone_urls() {
        assert_eq!(
            repo_path("https://github.com/dauld/boss-fork.git"),
            "dauld/boss-fork"
        );
        assert_eq!(
            repo_path("git@github.com:dauld/boss-fork"),
            "dauld/boss-fork"
        );
    }

    // -- the sweep's head guard (car 23923b40's known_gap) -----------------
    //
    // `fix/conductor-hardening` boarded at fc55e4d; two more commits
    // (705230b) were pushed to the branch AFTER boarding; the train
    // landed carrying only the boarded ones; the sweep read the job
    // record ("closed, outcome=merged" — true) and deleted the branch,
    // taking the unmerged commits with it. The job record proves the
    // CONTENT landed, never that the branch still holds only that
    // content. These pin the second question the sweep must now ask.

    const BOARDED: &str = "fc55e4d1a2b3c4d5e6f708192a3b4c5d6e7f8091";
    const MOVED: &str = "705230b9f8e7d6c5b4a39281706f5e4d3c2b1a09";

    #[test]
    fn a_branch_still_at_its_boarded_head_is_deleted() {
        assert_eq!(
            sweep_guard(Some(BOARDED), Some(BOARDED)),
            SweepGuard::Delete
        );
    }

    #[test]
    fn a_branch_that_moved_since_boarding_is_kept() {
        // The incident, exactly: the recorded head is not the branch's
        // head any more, so the delete would take work the train never
        // carried.
        assert_eq!(
            sweep_guard(Some(BOARDED), Some(MOVED)),
            SweepGuard::Moved {
                recorded: BOARDED.to_string(),
                current: MOVED.to_string(),
            }
        );
    }

    #[test]
    fn a_car_with_no_recorded_head_keeps_its_branch() {
        // An unknown head is not evidence. A car that boarded before
        // the conductor recorded heads keeps its branch: the cost of
        // keeping one is a stale branch, the cost of deleting one is
        // lost work. The branch has to EXIST for the question to mean
        // anything — see the Gone test for the other half.
        assert_eq!(sweep_guard(None, Some(BOARDED)), SweepGuard::NoRecord);
        // An empty stamp is no stamp.
        assert_eq!(sweep_guard(Some(""), Some(BOARDED)), SweepGuard::NoRecord);
    }

    #[test]
    fn a_branch_already_off_the_forge_is_nothing_to_sweep() {
        assert_eq!(sweep_guard(Some(BOARDED), None), SweepGuard::Gone);
        assert_eq!(sweep_guard(Some(BOARDED), Some("")), SweepGuard::Gone);
        // The forge's answer is asked FIRST, so an absent branch reads
        // Gone whatever the record says. Job 1bd1fb3d: every pre-guard
        // historical car has no recorded head AND no branch left, and
        // ordering the record first made each one a NoRecord line on
        // every reconcile, forever, about a branch swept by hand hours
        // earlier.
        assert_eq!(sweep_guard(None, None), SweepGuard::Gone);
        assert_eq!(sweep_guard(None, Some("")), SweepGuard::Gone);
        assert_eq!(sweep_guard(Some(""), None), SweepGuard::Gone);
    }

    #[test]
    fn only_a_branch_that_still_exists_is_worth_narrating() {
        // The sweep's journal is an operator surface: a line earns its
        // place by naming something a human can act on. A branch that
        // is not on the forge is not that — nothing to delete, nothing
        // to rescue, no action available.
        assert_eq!(sweep_note(&SweepGuard::Gone, "fix/x", "car-1"), None);
        // Delete narrates at the call site, which knows whether it was
        // a dry run, a deletion, or a race.
        assert_eq!(sweep_note(&SweepGuard::Delete, "fix/x", "car-1"), None);
        // The two keep-and-tell cases: the branch exists and the sweep
        // declined it, which is exactly what an operator must hear.
        let no_record = sweep_note(&SweepGuard::NoRecord, "fix/x", "car-1")
            .expect("a surviving branch with no record is worth a line");
        assert!(no_record.contains("fix/x"), "{no_record}");
        assert!(
            no_record.contains("no boarded head on record"),
            "{no_record}"
        );
        let moved = sweep_note(
            &SweepGuard::Moved {
                recorded: BOARDED.to_string(),
                current: MOVED.to_string(),
            },
            "fix/conductor-hardening",
            "car-1",
        )
        .expect("a branch that outgrew its boarding is worth a line");
        assert_eq!(
            moved,
            branch_moved_line("fix/conductor-hardening", BOARDED, MOVED)
        );
    }

    #[test]
    fn the_boarded_head_is_read_off_the_car_job() {
        let mut car = landed_car("car-1", "feat/x");
        car["metadata"]["boarded_head"] = json!(BOARDED);
        assert_eq!(boarded_head(&car), Some(BOARDED));
        // Absent, empty, or non-string reads as no stamp at all.
        assert_eq!(boarded_head(&landed_car("car-2", "feat/y")), None);
        let mut blank = landed_car("car-3", "feat/z");
        blank["metadata"]["boarded_head"] = json!("");
        assert_eq!(boarded_head(&blank), None);
        assert_eq!(boarded_head(&json!({"id": "car-4"})), None);
    }

    #[test]
    fn the_moved_branch_line_names_both_heads() {
        // Operator surface: the only notice that unmerged commits are
        // sitting on a branch the train did not carry.
        assert_eq!(
            branch_moved_line("fix/conductor-hardening", BOARDED, MOVED),
            "branch fix/conductor-hardening moved since boarding \
             (fc55e4d1 -> 705230b9) — not deleting"
        );
    }

    // -- the jobs-API retry classifier -------------------------------------
    //
    // The cluster is the system of record and it rolls. Twice on
    // 2026-08-13 a reconcile hit `Connection refused` to the jobs API
    // mid-converge and failed the whole verb; the blip lasted seconds.
    // A bounded retry covers the roll — but only for failures that are
    // blips, and only where re-sending is safe.

    #[test]
    fn a_refused_connection_is_a_blip_under_any_method() {
        // Nothing was received, so nothing was done: even a create may
        // go again.
        assert!(retryable(&Method::GET, &Failure::Connect));
        assert!(retryable(&Method::PUT, &Failure::Connect));
        assert!(retryable(&Method::POST, &Failure::Connect));
    }

    #[test]
    fn an_ambiguous_blip_only_retries_an_idempotent_call() {
        // A timeout leaves the write UNKNOWN — re-POSTing an ambiguous
        // create is how one blip becomes two train Jobs.
        assert!(retryable(&Method::GET, &Failure::Ambiguous));
        assert!(retryable(&Method::PUT, &Failure::Ambiguous));
        assert!(!retryable(&Method::POST, &Failure::Ambiguous));
    }

    #[test]
    fn a_5xx_is_a_blip_and_a_4xx_is_an_answer() {
        for status in [500, 502, 503, 504] {
            assert!(
                retryable(&Method::GET, &Failure::Http(status)),
                "{status} is the SoR failing to answer"
            );
            assert!(
                !retryable(&Method::POST, &Failure::Http(status)),
                "{status} leaves a create ambiguous"
            );
        }
        // A 422 is the jobs API telling the conductor no. Retrying an
        // answer just asks the same question three times — including
        // 429, which is an answer about rate, not a transport blip.
        for status in [400, 404, 409, 422, 429] {
            assert!(!retryable(&Method::GET, &Failure::Http(status)), "{status}");
            assert!(!retryable(&Method::PUT, &Failure::Http(status)), "{status}");
        }
        // 2xx/3xx never reach the classifier, and are not blips either.
        assert!(!retryable(&Method::GET, &Failure::Http(200)));
        assert!(!retryable(&Method::GET, &Failure::Http(301)));
    }

    #[test]
    fn an_unusable_answer_is_never_a_blip() {
        // The SoR answered; the body was garbage. Retrying re-reads
        // the same garbage.
        assert!(!retryable(&Method::GET, &Failure::Malformed));
        assert!(!retryable(&Method::POST, &Failure::Malformed));
    }

    #[test]
    fn the_backoff_doubles_from_the_base() {
        assert_eq!(JOBS_API_RETRY.attempts, 3);
        assert_eq!(JOBS_API_RETRY.backoff(1), Duration::from_secs(2));
        assert_eq!(JOBS_API_RETRY.backoff(2), Duration::from_secs(4));
        // The tests' policy makes the same decisions and never waits.
        assert_eq!(RetryPolicy::immediate(3).backoff(1), Duration::ZERO);
    }

    #[test]
    fn a_blip_cause_reads_the_innermost_error() {
        // "GET /api/jobs: error sending request: ... : Connection
        // refused" — the fact is at the bottom; the url is already
        // implied by the line around it.
        let e = anyhow!("Connection refused (os error 61)")
            .context("error sending request for url (http://10.20.0.34:7900/api/jobs)")
            .context("GET /api/jobs?kind=pr-train");
        assert_eq!(short_cause(&e), "Connection refused (os error 61)");
        // A bare error is its own innermost cause.
        assert_eq!(short_cause(&anyhow!("HTTP 503")), "HTTP 503");
        // And it stays journal-sized.
        let long = short_cause(&anyhow!("{}", "x".repeat(500)));
        assert!(long.chars().count() <= 81, "{} chars", long.chars().count());
        assert!(long.ends_with('…'), "says it truncated: {long}");
    }

    #[test]
    fn a_real_refused_connection_classifies_as_a_blip() {
        // The production failure end to end: reqwest's own error for a
        // refused connect must land on a retryable Failure, or the
        // classifier above is pinning a shape the wire never produces.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let err = rt.block_on(async {
            reqwest::Client::builder()
                .timeout(Duration::from_millis(250))
                .build()
                .unwrap()
                // Port 1 refuses; a filtered port times out. Both are
                // blips, and neither is an answer.
                .get("http://127.0.0.1:1/api/jobs")
                .send()
                .await
                .expect_err("nothing serves port 1")
        });
        let kind = classify_transport(&err);
        assert!(
            matches!(kind, Failure::Connect | Failure::Ambiguous),
            "a refused/timed-out connect must be a transport failure, got {kind:?}"
        );
        assert!(retryable(&Method::GET, &kind));
    }

    // -- the retry driver --------------------------------------------------

    fn blip(kind: Failure) -> ApiFailure {
        ApiFailure {
            kind,
            cause: anyhow!("Connection refused (os error 61)"),
        }
    }

    /// A journal that counts its lines instead of printing them.
    fn counting_journal(lines: &Cell<u32>) -> impl Fn(&str) {
        move |_| lines.set(lines.get() + 1)
    }

    #[tokio::test]
    async fn a_blip_retries_to_the_attempt_budget_then_surfaces() {
        let mut calls = 0u32;
        let lines = Cell::new(0u32);
        let out: Result<()> = retrying(
            &RetryPolicy::immediate(3),
            &Method::GET,
            &counting_journal(&lines),
            || {
                calls += 1;
                async { Err(blip(Failure::Connect)) }
            },
        )
        .await;
        assert!(out.is_err(), "the verb still surfaces a real outage");
        assert_eq!(calls, 3, "three attempts, not more");
        assert_eq!(lines.get(), 2, "one line per retry — blips stay countable");
    }

    #[tokio::test]
    async fn a_recovered_blip_costs_nothing_but_a_line() {
        let mut calls = 0u32;
        let lines = Cell::new(0u32);
        let out: Result<u8> = retrying(
            &RetryPolicy::immediate(3),
            &Method::PUT,
            &counting_journal(&lines),
            || {
                calls += 1;
                let attempt = calls;
                async move {
                    if attempt == 1 {
                        Err(blip(Failure::Ambiguous))
                    } else {
                        Ok(7)
                    }
                }
            },
        )
        .await;
        assert_eq!(out.unwrap(), 7);
        assert_eq!(calls, 2, "stops the moment the SoR answers");
        assert_eq!(lines.get(), 1);
    }

    #[tokio::test]
    async fn an_answer_is_surfaced_on_the_first_attempt() {
        let mut calls = 0u32;
        let lines = Cell::new(0u32);
        let out: Result<()> = retrying(
            &RetryPolicy::immediate(3),
            &Method::PUT,
            &counting_journal(&lines),
            || {
                calls += 1;
                async {
                    Err(ApiFailure {
                        kind: Failure::Http(422),
                        cause: anyhow!("PUT /api/jobs/x: HTTP 422: metadata_schema"),
                    })
                }
            },
        )
        .await;
        assert!(
            out.unwrap_err().to_string().contains("422"),
            "the answer reaches the operator unchanged"
        );
        assert_eq!(calls, 1, "a 422 is an answer — asked once");
        assert_eq!(lines.get(), 0, "an answer is not a blip and journals none");
    }
}
