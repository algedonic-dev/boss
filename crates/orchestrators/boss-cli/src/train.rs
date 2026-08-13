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
//!     open, visibly.
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
pub enum Phase {
    Preflight,
    Reconcile,
    Board,
    Run,
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

/// Return the list of problems; empty means the locomotive is fit.
fn preflight(cfg: &Config) -> Result<Vec<String>> {
    let mut problems = Vec::new();
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

/// The code host as the conductor sees it: three verbs.
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
}

struct GitHubForge {
    head_owner: String,
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
}

/// The same three verbs against the internal forge's API. PRs are
/// same-repo (no fork dance): the train branch pushes to the one
/// repo and the PR head is the bare branch name.
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
}

fn make_forge(head_owner: &str) -> Result<Box<dyn Forge>> {
    let kind = env_or("BOSS_TRAIN_FORGE", "github");
    match kind.as_str() {
        "github" => Ok(Box::new(GitHubForge {
            head_owner: head_owner.to_string(),
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

    async fn api(
        &self,
        method: Method,
        path: &str,
        payload: Option<Value>,
    ) -> Result<Option<Value>> {
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
            .with_context(|| format!("{method} {path}"))?;
        let status = resp.status();
        let body = resp.text().await?;
        if !status.is_success() {
            bail!("{method} {path}: HTTP {status}: {}", body.trim());
        }
        if body.trim().is_empty() {
            Ok(None)
        } else {
            Ok(Some(serde_json::from_str(&body).with_context(|| {
                format!("parsing {method} {path} response")
            })?))
        }
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
    async fn merge_job_metadata(&self, jid: &str, kv: Vec<(&str, Value)>) -> Result<Value> {
        let mut job = self.get_job(jid).await?;
        let keys: Vec<&str> = kv.iter().map(|(k, _)| *k).collect();
        let mut md = metadata_map(&job);
        for (k, v) in kv {
            md.insert(k.to_string(), v);
        }
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
        let pull_remote = env_or("BOSS_TRAIN_DEPLOY_REMOTE", "origin");
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

    async fn reconcile(&self) -> Result<()> {
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
        Ok(())
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

    async fn candidates(&self) -> Result<Vec<(Value, String)>> {
        let mut out = Vec::new();
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
            if !ok.status.success() {
                log(format!(
                    "{}: branch {branch} not on fork — leaving behind",
                    id8(&jid)
                ));
                if !self.cfg.dry {
                    // Loud on the Job, not just in the journal: the author
                    // parked this at review believing it would board.
                    self.merge_job_metadata(
                        &jid,
                        vec![(
                            "skip_reason",
                            json!(format!(
                                "branch {branch} not found on the fork — push it, then \
                                 the next train boards it"
                            )),
                        )],
                    )
                    .await?;
                }
                continue;
            }
            out.push((j, branch));
        }
        Ok(out)
    }

    async fn open_train_job(&self, train_branch: &str, window: &str) -> Result<Option<Value>> {
        let payload = json!({
            "kind": "pr-train",
            "subject": {"subject_kind": "custom", "id": train_branch},
            "title": format!("PR train {window}"),
            "owner_id": "emp-bootstrap-admin",
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
        let cands = self.candidates().await?;
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
        let mut boarded: Vec<(Value, String)> = Vec::new();
        let mut skipped: Vec<(Value, String)> = Vec::new();
        for (j, branch) in cands {
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
                boarded.push((j, branch));
            } else {
                let diff =
                    sh_unchecked(&["git", "-C", clone, "diff", "--name-only", "--diff-filter=U"])?;
                let conflicted: Vec<String> = stdout_str(&diff)
                    .split_whitespace()
                    .map(str::to_string)
                    .collect();
                sh_unchecked(&["git", "-C", clone, "merge", "--abort"])?;
                let files = if conflicted.is_empty() {
                    "unresolved (merge died before conflict markers)".to_string()
                } else {
                    conflicted.join(", ")
                };
                self.merge_job_metadata(
                    job_id(&j)?,
                    vec![(
                        "skip_reason",
                        json!(format!(
                            "merge conflict with this window's train in: {files} — rebase \
                             onto main and repark at review"
                        )),
                    )],
                )
                .await?;
                log(format!(
                    "conflict merging {branch} ({files}) — left for the next train"
                ));
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
            .map(|(j, b)| {
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
            .map(|(j, _)| job_id(j).map(str::to_string))
            .collect::<Result<_>>()?;
        let skipped_branches: Vec<String> = skipped.iter().map(|(_, b)| b.clone()).collect();
        self.merge_job_metadata(
            &train_id,
            vec![
                ("boarded_jobs", json!(boarded_ids)),
                ("skipped_branches", json!(skipped_branches)),
            ],
        )
        .await?;
        let train = self.get_job(&train_id).await?;
        let boarded_note = boarded
            .iter()
            .map(|(j, b)| {
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

        for (j, _branch) in &boarded {
            let review = find_step(j, "review", "Open for review");
            self.complete_step(
                j,
                review,
                &[
                    ("pr_url", Some(pr_url.clone())),
                    (
                        "note",
                        Some(format!("boarded train {} ({train_branch})", id8(&train_id))),
                    ),
                ],
            )
            .await?;
            // skip_reason cleared on boarding: an earlier window's skip note
            // must not outlive the skip.
            self.merge_job_metadata(
                job_id(j)?,
                vec![
                    ("train", json!(train_id.as_str())),
                    ("skip_reason", json!("")),
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
    let forge = make_forge(&cfg.head_owner)?;
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
    if !matches!(phase, Phase::Board) {
        conductor.reconcile().await?;
    }
    if !matches!(phase, Phase::Reconcile) {
        conductor.board(now).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::parked_ready;
    use serde_json::json;

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
}
