//! Complete subsystem handback on the existing cadence firing.
use super::*;
use std::path::Path;

pub(super) const FILE_ENV: &str = "BOSS_CADENCE_OBSERVATIONS_FILE";

pub(super) struct Captured {
    pub observations: serde_json::Map<String, Value>,
    original: Option<Vec<u8>>,
    deletable: bool,
}

pub(super) fn path(firing: &str) -> std::path::PathBuf {
    let safe: String = firing
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    crate::own_temp::own_temp_path(&format!(
        "boss-cadence-observations-{safe}-{}",
        uuid::Uuid::new_v4()
    ))
}

fn unknown(path: &Path, bytes: Option<&[u8]>, reason: String) -> serde_json::Map<String, Value> {
    use sha2::{Digest, Sha256};
    serde_json::Map::from_iter([(
        "gate_queue".into(),
        json!({"contract_version":1,"state":"unknown","reason":reason,
        "retained_local_file":path,"size_bytes":bytes.map(<[u8]>::len),
        "sha256":bytes.map(|bytes|hex::encode(Sha256::digest(bytes)))}),
    )])
}

pub(super) fn capture(path: &Path, process_rc: i32) -> Captured {
    let read = std::fs::read(path);
    let original = read.as_ref().ok().cloned();
    let parsed = read.map_err(anyhow::Error::from).and_then(|bytes| {
        serde_json::from_slice::<serde_json::Map<String, Value>>(&bytes)
            .map_err(anyhow::Error::from)
    });
    let parsed = parsed.and_then(|object| {
        if process_rc != 0 {
            bail!("child exited {process_rc}; this firing cannot assert completed supervision");
        }
        let queue = object
            .get("gate_queue")
            .context("this firing left no gate_queue observation")?;
        if queue.get("contract_version").and_then(Value::as_u64) != Some(1)
            || !matches!(
                queue.get("state").and_then(Value::as_str),
                Some("completed" | "unknown")
            )
        {
            bail!("this firing's gate_queue observation is malformed or unsupported");
        }
        if queue.get("state").and_then(Value::as_str) == Some("completed")
            && (!queue.get("report").is_some_and(Value::is_object)
                || queue
                    .get("source_build")
                    .and_then(Value::as_str)
                    .is_none_or(str::is_empty)
                || queue
                    .get("observed_at")
                    .and_then(Value::as_str)
                    .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
                    .is_none())
        {
            bail!("completed supervision lacks its current source/time/whole report");
        }
        Ok(object)
    });
    match parsed {
        Ok(observations) => Captured {
            observations,
            original,
            deletable: true,
        },
        Err(error) => Captured {
            observations: unknown(path, original.as_deref(), format!("{error:#}")),
            original,
            deletable: false,
        },
    }
}

/// The child never replaces another firing's original, even after a
/// crash. The caller provides an owned random per-firing path.
pub(crate) fn handback(observations: &Value) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let Some(path) = std::env::var_os(FILE_ENV) else {
        eprintln!(
            "cadence supervision observation {}",
            serde_json::to_string(observations)?
        );
        return Ok(());
    };
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    file.write_all(&serde_json::to_vec(observations)?)?;
    file.sync_all()?;
    Ok(())
}

async fn write_readback(
    http: &crate::machine_http::MachineClient,
    base: &str,
    rule: &str,
    id: &str,
    outcome: &FiringOutcome,
) -> Result<()> {
    record_outcome(http, base, id, outcome).await?;
    let back = last_firing(http, base, rule)
        .await?
        .context("the recorded firing read back absent")?;
    if back.firing_id != id
        || back.rc != Some(outcome.rc)
        || back.board_decision != outcome.board_decision
        || back.observations != outcome.observations
    {
        bail!(
            "the exact firing/outcome/whole observations did not read back equal; original retained"
        );
    }
    Ok(())
}

pub(super) async fn persist(
    http: &crate::machine_http::MachineClient,
    base: &str,
    rule: &str,
    id: &str,
    outcome: &FiringOutcome,
    path: &Path,
    captured: &Captured,
) -> Result<()> {
    if let Err(error) = write_readback(http, base, rule, id, outcome).await {
        // A body limit or failed/ambiguous persistence cannot turn a
        // retained full local original into a truncated completed row.
        let fallback = FiringOutcome {
            observations: Some(unknown(
                path,
                captured.original.as_deref(),
                format!("whole observation persistence/readback failed: {error:#}"),
            )),
            ..outcome.clone()
        };
        let fallback_result = write_readback(http, base, rule, id, &fallback).await;
        bail!(
            "full observation retained at {}: {error:#}; explicit UNKNOWN persistence: {fallback_result:?}",
            path.display()
        );
    }
    if captured.deletable {
        let current = std::fs::read(path)?;
        if captured.original.as_deref() != Some(current.as_slice()) {
            bail!(
                "observation changed since capture; original at {} retained",
                path.display()
            );
        }
        std::fs::remove_file(path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        extract::State,
        routing::{get, post},
    };
    use boss_jobs::cadence::{CadenceRepository, InMemoryCadence, NewFiring};
    use std::sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    };
    #[derive(Clone)]
    struct Door {
        repo: Arc<InMemoryCadence>,
        fault: Arc<AtomicU8>,
    }
    async fn fixture() -> (String, Door, tokio::task::JoinHandle<()>) {
        let door = Door {
            repo: Arc::new(InMemoryCadence::default()),
            fault: Arc::new(AtomicU8::new(0)),
        };
        door.repo
            .claim_firing(&NewFiring {
                firing_id: "one".into(),
                rule_name: "queue".into(),
                verb: "reconcile".into(),
                basis: "wall".into(),
                fired_at: Utc::now(),
                detail: json!({}),
            })
            .await
            .unwrap();
        async fn write(
            State(door): State<Door>,
            Json(outcome): Json<FiringOutcome>,
        ) -> axum::http::StatusCode {
            match door.fault.load(Ordering::SeqCst) {
                1 => return axum::http::StatusCode::FORBIDDEN,
                4 => return axum::http::StatusCode::NO_CONTENT,
                _ => {}
            }
            door.repo.record_outcome("one", &outcome).await.unwrap();
            axum::http::StatusCode::NO_CONTENT
        }
        async fn read(State(door): State<Door>) -> axum::response::Response {
            use axum::response::IntoResponse;
            if door.fault.load(Ordering::SeqCst) == 2 {
                return axum::http::StatusCode::FORBIDDEN.into_response();
            }
            let mut last = door.repo.last_firing("queue").await.unwrap().unwrap();
            if door.fault.load(Ordering::SeqCst) == 3 {
                last.observations = Some(serde_json::Map::new());
            }
            Json(last).into_response()
        }
        let app = Router::new()
            .route("/api/cadence/firings/one/outcome", post(write))
            .route("/api/cadence/rules/queue/last-firing", get(read))
            .with_state(door.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        (
            base,
            door,
            tokio::spawn(async move { axum::serve(listener, app).await.unwrap() }),
        )
    }
    fn source(path: &Path) -> Vec<u8> {
        let bytes=serde_json::to_vec(&json!({"gate_queue":{"contract_version":1,"state":"completed","source_build":"fixture-current-build","observed_at":"2026-10-16T06:00:00Z","report":{"all":[1,2,3],"original":"whole"}}})).unwrap();
        std::fs::write(path, &bytes).unwrap();
        bytes
    }
    #[tokio::test]
    async fn full_observation_is_deleted_only_after_exact_durable_readback() {
        let (base, door, server) = fixture().await;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("owned");
        let original = source(&path);
        let captured = capture(&path, 0);
        let outcome = FiringOutcome {
            rc: 75,
            runtime_secs: 2,
            board_decision: None,
            observations: Some(captured.observations.clone()),
        };
        let http =
            crate::machine_http::MachineClient::unstamped(reqwest::Client::builder()).unwrap();
        persist(&http, &base, "queue", "one", &outcome, &path, &captured)
            .await
            .unwrap();
        assert!(!path.exists());
        let last = door.repo.last_firing("queue").await.unwrap().unwrap();
        assert_eq!(last.rc, Some(75));
        assert_eq!(
            serde_json::to_vec(&last.observations.unwrap()).unwrap(),
            original
        );
        server.abort();
    }
    #[tokio::test]
    async fn persistence_faults_preserve_every_original_byte() {
        for fault in [1, 2, 3, 4] {
            let (base, door, server) = fixture().await;
            door.fault.store(fault, Ordering::SeqCst);
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("owned");
            let original = source(&path);
            let captured = capture(&path, 0);
            let outcome = FiringOutcome {
                rc: 0,
                runtime_secs: 1,
                board_decision: None,
                observations: Some(captured.observations.clone()),
            };
            let http =
                crate::machine_http::MachineClient::unstamped(reqwest::Client::builder()).unwrap();
            assert!(
                persist(&http, &base, "queue", "one", &outcome, &path, &captured)
                    .await
                    .is_err(),
                "fault {fault}"
            );
            assert_eq!(std::fs::read(&path).unwrap(), original, "fault {fault}");
            server.abort();
        }
    }
    #[tokio::test]
    async fn body_limit_keeps_the_full_original_and_records_explicit_unknown() {
        let (base, door, server) = fixture().await;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("owned");
        let mut object: Value = serde_json::from_slice(&source(&path)).unwrap();
        object["gate_queue"]["report"]["large_original"] = json!("x".repeat(3 * 1024 * 1024));
        let original = serde_json::to_vec(&object).unwrap();
        std::fs::write(&path, &original).unwrap();
        let captured = capture(&path, 0);
        let outcome = FiringOutcome {
            rc: 75,
            runtime_secs: 1,
            board_decision: None,
            observations: Some(captured.observations.clone()),
        };
        let http =
            crate::machine_http::MachineClient::unstamped(reqwest::Client::builder()).unwrap();
        assert!(
            persist(&http, &base, "queue", "one", &outcome, &path, &captured)
                .await
                .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), original);
        let last = door.repo.last_firing("queue").await.unwrap().unwrap();
        assert_eq!(last.rc, Some(75));
        let queue = &last.observations.unwrap()["gate_queue"];
        assert_eq!(queue["state"], "unknown");
        assert_eq!(queue["size_bytes"], original.len());
        assert!(queue["reason"].as_str().unwrap().contains("413"));
        server.abort();
    }

    #[tokio::test]
    async fn a_changed_local_original_is_retained_after_readback() {
        let (base, _, server) = fixture().await;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("owned");
        source(&path);
        let captured = capture(&path, 0);
        std::fs::write(&path, b"a later complete original must not be deleted").unwrap();
        let outcome = FiringOutcome {
            rc: 0,
            runtime_secs: 1,
            board_decision: None,
            observations: Some(captured.observations.clone()),
        };
        let http =
            crate::machine_http::MachineClient::unstamped(reqwest::Client::builder()).unwrap();
        assert!(
            persist(&http, &base, "queue", "one", &outcome, &path, &captured)
                .await
                .is_err()
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"a later complete original must not be deleted"
        );
        server.abort();
    }

    #[test]
    fn missing_malformed_or_failed_child_never_reports_completed() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("owned");
        assert_eq!(
            capture(&path, 0).observations["gate_queue"]["state"],
            "unknown"
        );
        std::fs::write(&path, b"{incomplete").unwrap();
        let malformed = capture(&path, 0);
        assert!(!malformed.deletable);
        assert_eq!(malformed.observations["gate_queue"]["state"], "unknown");
        std::fs::write(
            &path,
            serde_json::to_vec(&json!({"gate_queue":{"contract_version":1,"state":"completed"}}))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            capture(&path, 0).observations["gate_queue"]["state"],
            "unknown",
            "a completed word without its current source/time/full report is not evidence"
        );
        source(&path);
        let failed = capture(&path, 9);
        assert!(!failed.deletable);
        assert_eq!(failed.observations["gate_queue"]["state"], "unknown");
    }
}
