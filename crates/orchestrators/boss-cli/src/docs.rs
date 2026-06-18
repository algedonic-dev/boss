//! `boss docs` subcommand — reindex + flush worker.
//!
//! `boss docs reindex` hits the reindex endpoint.
//! `boss docs flush-pending` polls the queued flush jobs, applies
//! each one to its markdown file via `docs_flush::apply_decisions`,
//! commits the result, and marks the job succeeded. Failures mark
//! the job as failed with the error message.

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::docs_flush::{self, DecisionKind, FlushDecision};

fn api_base() -> String {
    std::env::var("BOSS_DOCS_API").unwrap_or_else(|_| boss_ports::url("docs"))
}

fn repo_root() -> PathBuf {
    std::env::var("BOSS_REPO_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/opt/boss"))
}

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
struct ApiFlushJob {
    id: String,
    doc_path: String,
    status: String,
    payload: ApiPayload,
}

#[derive(Deserialize, Debug)]
struct ApiPayload {
    doc_path: String,
    #[allow(dead_code)]
    base_commit_sha: String,
    decisions: Vec<ApiDecision>,
}

#[derive(Deserialize, Debug)]
struct ApiDecision {
    anchor: String,
    kind: String,
    resolution: String,
    rationale: Option<String>,
}

#[derive(Serialize)]
struct StatusUpdate<'a> {
    status: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    commit_sha: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'a str>,
}

pub async fn reindex() -> Result<()> {
    let url = format!("{}/api/design/reindex", api_base());
    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .send()
        .await
        .context("POST /api/design/reindex")?;
    if !resp.status().is_success() {
        anyhow::bail!("reindex failed: HTTP {}", resp.status());
    }
    let body: serde_json::Value = resp.json().await?;
    println!(
        "reindex complete: {} docs indexed, {} deleted ({} ms)",
        body["docs_indexed"].as_u64().unwrap_or(0),
        body["docs_deleted"].as_u64().unwrap_or(0),
        body["duration_ms"].as_u64().unwrap_or(0),
    );
    Ok(())
}

pub async fn flush_pending() -> Result<()> {
    let client = reqwest::Client::new();
    let base = api_base();
    let root = repo_root();

    // 1. Pull queued jobs.
    let url = format!("{base}/api/design/flush-jobs?status=queued");
    let resp = client.get(&url).send().await.context("GET queued jobs")?;
    if !resp.status().is_success() {
        anyhow::bail!("fetching queued jobs: HTTP {}", resp.status());
    }
    let jobs: Vec<ApiFlushJob> = resp.json().await?;

    if jobs.is_empty() {
        println!("no pending flush jobs");
        return Ok(());
    }

    println!(
        "{} pending flush job{}",
        jobs.len(),
        if jobs.len() == 1 { "" } else { "s" }
    );

    let mut any_failed = false;
    for job in jobs {
        match process_job(&client, &base, &root, &job).await {
            Ok(sha) => {
                println!("  ✓ {} → {}", job.id, sha);
            }
            Err(e) => {
                any_failed = true;
                println!("  ✗ {} failed: {e}", job.id);
                // Mark the job as failed on the server so the UI can
                // surface the error + offer a retry.
                let _ = mark_failed(&client, &base, &job.id, &e.to_string()).await;
            }
        }
    }

    if any_failed {
        anyhow::bail!("one or more jobs failed — see output above");
    }
    Ok(())
}

async fn process_job(
    client: &reqwest::Client,
    base: &str,
    root: &Path,
    job: &ApiFlushJob,
) -> Result<String> {
    if job.status != "queued" {
        return Err(anyhow!("job {} is not queued ({})", job.id, job.status));
    }

    // 2. Mark running so concurrent workers skip it.
    mark_running(client, base, &job.id).await?;

    // 3. Translate the API payload into the pure types and apply.
    let decisions: Vec<FlushDecision> = job
        .payload
        .decisions
        .iter()
        .map(|d| {
            let kind = match d.kind.as_str() {
                "override" => DecisionKind::Override,
                _ => DecisionKind::Accept,
            };
            FlushDecision {
                anchor: d.anchor.clone(),
                kind,
                resolution: d.resolution.clone(),
                rationale: d.rationale.clone(),
            }
        })
        .collect();

    let file_path = docs_flush::locate_file(root, &job.payload.doc_path);
    let before = docs_flush::load_markdown(&file_path)?;
    let after = docs_flush::apply_decisions(&before, &decisions, docs_flush::today())
        .with_context(|| format!("applying decisions to {}", job.payload.doc_path))?;

    if before == after {
        return Err(anyhow!("decisions produced no changes to the file"));
    }

    docs_flush::save_markdown(&file_path, &after)?;

    // 4. git add + commit + push.
    let doc_slug = Path::new(&job.payload.doc_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("doc")
        .to_string();
    let commit_msg = format!(
        "docs: resolve {} question{} in {}",
        decisions.len(),
        if decisions.len() == 1 { "" } else { "s" },
        doc_slug,
    );

    run_git(root, &["add", &job.payload.doc_path])?;
    run_git(root, &["commit", "-m", &commit_msg])?;
    // Push is best-effort — if the branch is behind, let the human fix it.
    if let Err(e) = run_git(root, &["push", "origin", "HEAD"]) {
        return Err(anyhow!("commit succeeded but push failed: {e}"));
    }

    let sha = run_git_capture(root, &["rev-parse", "HEAD"])?;
    let sha = sha.trim();

    // 5. Mark succeeded on the server.
    mark_succeeded(client, base, &job.id, sha).await?;

    Ok(sha.to_string())
}

async fn mark_running(client: &reqwest::Client, base: &str, job_id: &str) -> Result<()> {
    let url = format!("{base}/api/design/flush-jobs?id={job_id}");
    let body = StatusUpdate {
        status: "running",
        commit_sha: None,
        error: None,
    };
    let resp = client.put(&url).json(&body).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("PUT running: HTTP {}", resp.status());
    }
    Ok(())
}

async fn mark_succeeded(
    client: &reqwest::Client,
    base: &str,
    job_id: &str,
    sha: &str,
) -> Result<()> {
    let url = format!("{base}/api/design/flush-jobs?id={job_id}");
    let body = StatusUpdate {
        status: "succeeded",
        commit_sha: Some(sha),
        error: None,
    };
    let resp = client.put(&url).json(&body).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("PUT succeeded: HTTP {}", resp.status());
    }
    Ok(())
}

async fn mark_failed(client: &reqwest::Client, base: &str, job_id: &str, err: &str) -> Result<()> {
    let url = format!("{base}/api/design/flush-jobs?id={job_id}");
    let body = StatusUpdate {
        status: "failed",
        commit_sha: None,
        error: Some(err),
    };
    client.put(&url).json(&body).send().await?;
    Ok(())
}

fn run_git(cwd: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("spawning git {}", args.join(" ")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git {} failed: {}", args.join(" "), stderr.trim());
    }
    Ok(())
}

fn run_git_capture(cwd: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("spawning git {}", args.join(" ")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git {} failed: {}", args.join(" "), stderr.trim());
    }
    Ok(String::from_utf8(output.stdout)?)
}
