//! Reindex — scan `docs/design/*.md` on disk, parse each file, and
//! upsert the results into the repository.
//!
//! Uses `git log` for per-file metadata (last_modified, last_author,
//! last_commit_sha). Falls back to filesystem timestamps if git is
//! not available or the file isn't tracked.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use chrono::{DateTime, TimeZone, Utc};
use tracing::warn;

use crate::parser::parse_doc;
use crate::port::{DocsError, DocsRepository};
use crate::types::DesignDoc;

/// Result of a reindex run.
#[derive(Debug, Clone, Default)]
pub struct ReindexStats {
    pub docs_indexed: usize,
    pub docs_deleted: usize,
    pub duration_ms: u64,
    /// Docs skipped because parse validation failed. The most common
    /// reason today is a Shipped/Superseded doc with unresolved
    /// questions — see reindex_one for the rule. Surfaced in the
    /// HTTP response so the author sees why their edit didn't land.
    pub rejected: Vec<RejectedDoc>,
}

#[derive(Debug, Clone)]
pub struct RejectedDoc {
    pub path: String,
    pub reason: String,
}

/// Walk `{repo_root}/docs/design/*.md` and upsert each file into the
/// repository. Any docs in the repository whose files no longer
/// exist on disk are deleted.
pub async fn reindex(
    repo: &dyn DocsRepository,
    repo_root: &Path,
) -> Result<ReindexStats, DocsError> {
    let start = std::time::Instant::now();
    let mut stats = ReindexStats::default();

    let design_dir = repo_root.join("docs/design");
    let Ok(entries) = std::fs::read_dir(&design_dir) else {
        // Directory missing is not an error — just means zero docs.
        return Ok(stats);
    };

    // Collect candidate files.
    let mut disk_paths: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let p = entry.path();
        if p.extension().and_then(|s| s.to_str()) == Some("md")
            && p.file_name()
                .and_then(|s| s.to_str())
                .map(|s| !s.starts_with('.'))
                .unwrap_or(false)
        {
            disk_paths.push(p);
        }
    }
    disk_paths.sort();

    // Reindex each doc.
    let mut indexed_paths: Vec<String> = Vec::new();
    for abs_path in &disk_paths {
        let rel_path = rel_from_repo_root(repo_root, abs_path);
        match reindex_one(repo, repo_root, abs_path, &rel_path).await {
            Ok(()) => {
                stats.docs_indexed += 1;
                indexed_paths.push(rel_path);
            }
            Err(e) => {
                let reason = e.to_string();
                warn!(path = %rel_path, error = %reason, "failed to reindex doc");
                stats.rejected.push(RejectedDoc {
                    path: rel_path.clone(),
                    reason,
                });
                // The doc stays in the index at whatever state it
                // was last valid — we just don't update it. That
                // way the tracker keeps working for every other doc
                // while the author fixes this one.
                indexed_paths.push(rel_path);
            }
        }
    }

    // Prune docs in the DB that no longer exist on disk.
    let existing = repo.all_docs().await?;
    for doc in existing {
        if !indexed_paths.contains(&doc.path) {
            repo.delete_doc(&doc.path).await?;
            stats.docs_deleted += 1;
        }
    }

    stats.duration_ms = start.elapsed().as_millis() as u64;
    Ok(stats)
}

async fn reindex_one(
    repo: &dyn DocsRepository,
    repo_root: &Path,
    abs_path: &Path,
    rel_path: &str,
) -> Result<(), DocsError> {
    let markdown = std::fs::read_to_string(abs_path)
        .map_err(|e| DocsError::Storage(format!("reading {rel_path}: {e}")))?;
    let parsed = parse_doc(rel_path, &markdown);

    // A Shipped or Superseded doc must not carry live questions.
    // The author has to either mark them `(resolved)` or move the
    // doc to `reopened` — we force the choice at reindex time so
    // new questions don't silently disappear from the tracker.
    if parsed.status.is_terminal() && !parsed.unresolved_questions.is_empty() {
        return Err(DocsError::BadRequest(format!(
            "{rel_path}: status is `{status}` but {n} unresolved question{plural} found ({titles}). \
             Mark each with `(resolved)` in the heading, or change the doc status to `reopened`.",
            rel_path = rel_path,
            status = parsed.status.as_str(),
            n = parsed.unresolved_questions.len(),
            plural = if parsed.unresolved_questions.len() == 1 {
                ""
            } else {
                "s"
            },
            titles = parsed.unresolved_questions.join("; "),
        )));
    }

    // Fetch git metadata; fall back to filesystem mtime + "unknown"
    // author if git fails.
    let (last_modified, last_author, last_commit_sha) = git_metadata(repo_root, abs_path)
        .unwrap_or_else(|| {
            let mtime = fs_mtime(abs_path).unwrap_or_else(Utc::now);
            (mtime, "unknown".to_string(), "0000000".to_string())
        });

    let doc = DesignDoc {
        path: rel_path.to_string(),
        title: parsed.title,
        status: parsed.status,
        pending_count: 0, // refreshed by pending-decision endpoints
        word_count: parsed.word_count,
        last_modified,
        last_author,
        last_indexed_at: Utc::now(),
        last_commit_sha,
        content_html: parsed.content_html,
    };

    // Preserve pending_count if there's already a row for this doc
    // (re-index shouldn't clobber in-flight human clicks).
    let preserved_pending = repo
        .doc_by_path(rel_path)
        .await?
        .map(|d| d.pending_count)
        .unwrap_or(0);
    let doc = DesignDoc {
        pending_count: preserved_pending,
        ..doc
    };

    repo.upsert_doc(&doc, &parsed.questions).await?;
    Ok(())
}

fn rel_from_repo_root(repo_root: &Path, abs_path: &Path) -> String {
    abs_path
        .strip_prefix(repo_root)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| abs_path.to_string_lossy().replace('\\', "/"))
}

fn fs_mtime(path: &Path) -> Option<DateTime<Utc>> {
    let mtime = std::fs::metadata(path).ok()?.modified().ok()?;
    let unix = mtime.duration_since(SystemTime::UNIX_EPOCH).ok()?;
    Utc.timestamp_opt(unix.as_secs() as i64, unix.subsec_nanos())
        .single()
}

/// Run `git log -1 --format=%H|%ct|%an -- <path>` in the repo root.
/// Returns `(last_modified, last_author, last_commit_sha)` on success.
fn git_metadata(repo_root: &Path, abs_path: &Path) -> Option<(DateTime<Utc>, String, String)> {
    let output = Command::new("git")
        .args(["log", "-1", "--format=%H|%ct|%an", "--", abs_path.to_str()?])
        .current_dir(repo_root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let line = String::from_utf8(output.stdout).ok()?;
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let parts: Vec<&str> = line.splitn(3, '|').collect();
    if parts.len() != 3 {
        return None;
    }
    let sha = parts[0].to_string();
    let ts: i64 = parts[1].parse().ok()?;
    let author = parts[2].to_string();
    let last_modified = Utc.timestamp_opt(ts, 0).single()?;
    Some((last_modified, author, sha))
}

/// Return the current HEAD commit sha for the repo, used as the
/// `base_commit_sha` on flush job payloads. Returns "0000000" if
/// git isn't available.
pub fn current_head_sha(repo_root: &Path) -> String {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo_root)
        .output();
    if let Ok(o) = output
        && o.status.success()
        && let Ok(s) = String::from_utf8(o.stdout)
    {
        return s.trim().to_string();
    }
    "0000000".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InMemoryDocsRepo;

    #[tokio::test]
    async fn reindex_against_repo_root() {
        // Use the actual repo root so we index the real docs under
        // docs/design/. This test is an integration-adjacent unit test
        // that validates the reindex path end-to-end against real files.
        let repo = InMemoryDocsRepo::new();
        let repo_root = std::path::Path::new("../../..");
        let stats = reindex(&repo, repo_root).await.unwrap();
        assert!(
            stats.docs_indexed >= 3,
            "expected at least 3 docs, got {}",
            stats.docs_indexed
        );
        let docs = repo.all_docs().await.unwrap();
        // human-powered-state-machine.md is a stable framing doc —
        // approved status, no open questions, won't be purged or
        // migrated. Reliable fixture as design docs come and go.
        let target = "docs/design/human-powered-state-machine.md";
        assert!(docs.iter().any(|d| d.path == target));
        let doc = repo.doc_by_path(target).await.unwrap().unwrap();
        assert_eq!(doc.title, "Design: BOSS as a Human-Powered State Machine");
        // Git metadata should have a real commit sha (7+ chars).
        assert!(doc.last_commit_sha.len() >= 7);
        let questions = repo.questions_for_doc(target).await.unwrap();
        assert_eq!(questions.len(), 0, "framing doc has no open questions");
    }

    #[tokio::test]
    async fn reindex_prunes_missing_docs() {
        let repo = InMemoryDocsRepo::new();
        // Pre-seed a doc that doesn't exist on disk.
        let now = Utc::now();
        let ghost = DesignDoc {
            path: "docs/design/nonexistent.md".to_string(),
            title: "Ghost".to_string(),
            status: crate::types::DocStatus::Draft,
            pending_count: 0,
            word_count: 0,
            last_modified: now,
            last_author: "ghost".to_string(),
            last_indexed_at: now,
            last_commit_sha: "0".to_string(),
            content_html: String::new(),
        };
        repo.upsert_doc(&ghost, &[]).await.unwrap();
        let stats = reindex(&repo, std::path::Path::new("../../.."))
            .await
            .unwrap();
        assert!(stats.docs_deleted >= 1);
        assert!(
            repo.doc_by_path("docs/design/nonexistent.md")
                .await
                .unwrap()
                .is_none()
        );
    }
}
