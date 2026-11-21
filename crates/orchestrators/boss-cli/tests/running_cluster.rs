//! The real CLI reads the converge record, never a workstation release link.
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::json;

const SHA: &str = "436f4485a3024c199c119e7b74c9a0e56e4ca6a3";

fn rolled_record() -> serde_json::Value {
    json!({
        "data": [{
            "id": "87620e3a-a6cb-412b-9834-a04734006202",
            "kind": "maintenance-cluster-converge", "status": "open",
            "opened_at": "2026-11-14T13:55:36Z",
            "steps": [{ "spec_slug": "run", "status": "active",
                "metadata": { "rolled": SHA, "rolled_at": "2026-11-14T13:55:40Z" } }]
        }], "total": 8212, "limit": 1, "offset": 0, "full": true
    })
}

#[test]
fn a_roll_record_is_read_before_the_rest_of_the_converge_finishes() {
    let (output, _) = candidate(rolled_record());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("ALIGNED"), "{stdout}");
    assert!(stdout.contains("2026-11-14T13:55:40Z"), "{stdout}");
}

#[test]
fn an_unjudged_or_malformed_latest_converge_is_not_alignment() {
    let original = rolled_record();
    let mut variants = Vec::new();
    for (pointer, value) in [
        ("/data/0/steps/0/metadata", json!({})),
        ("/data/0/steps/0/metadata/rolled", json!("latest")),
        ("/data/0/steps/0/metadata/rolled_at", json!(null)),
        ("/data/0/kind", json!("maintenance-forge-converge")),
        ("/full", json!(false)),
        ("/total", json!(null)),
    ] {
        let mut record = original.clone();
        *record.pointer_mut(pointer).unwrap() = value;
        variants.push(record);
    }
    let mut multiple = original.clone();
    multiple["data"]
        .as_array_mut()
        .unwrap()
        .push(original["data"][0].clone());
    variants.push(multiple);
    for record in variants {
        let (output, _) = candidate(record.clone());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("UNKNOWN"), "{record}: {stdout}");
        assert!(!stdout.contains("ALIGNED"), "{record}: {stdout}");
        assert!(
            stdout.contains("converge"),
            "the unread layer is named: {stdout}"
        );
    }
}

#[test]
fn running_reads_the_cluster_record_without_a_local_release_link() {
    let record = json!({
        "data": [{
            "id": "87620e3a-a6cb-412b-9834-a04734006202",
            "kind": "maintenance-cluster-converge", "status": "closed",
            "opened_at": "2026-11-14T13:55:36Z",
            "steps": [{ "spec_slug": "run", "status": "completed",
                "metadata": { "result": "ok", "unchanged": "436f448" } }]
        }], "total": 8212, "limit": 1, "offset": 0, "full": true
    });
    let (output, requests) = candidate(record);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("ALIGNED"), "{stdout}");
    assert!(
        stdout.contains("87620e3a-a6cb-412b-9834-a04734006202"),
        "{stdout}"
    );
    assert_eq!(requests.len(), 2, "health and the latest converge record");
    for request in requests {
        assert!(
            request
                .to_lowercase()
                .contains("x-boss-machine-token: synthetic-running-fixture")
        );
    }
}

#[test]
fn an_unread_merge_names_the_clone_and_remote() {
    let (output, _) = candidate_with_git(rolled_record(), true);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("UNKNOWN"), "{stdout}");
    assert!(stdout.contains("merged unread: clone "), "{stdout}");
    assert!(stdout.contains("remote \"origin\""), "{stdout}");
    assert!(stdout.contains("fixture-merge-refusal"), "{stdout}");
}

fn candidate(record: serde_json::Value) -> (std::process::Output, Vec<String>) {
    candidate_with_git(record, false)
}

fn candidate_with_git(
    record: serde_json::Value,
    refuse_git: bool,
) -> (std::process::Output, Vec<String>) {
    let root = boss_testing::scratch_dir("running-cluster-record");
    let bin = root.join("bin");
    let tokens = root.join("fixture-token");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(&tokens).unwrap();
    std::fs::write(tokens.join("current"), "synthetic-running-fixture").unwrap();
    // No remote or credential is consulted by the merged-layer control.
    for (name, source) in [
        (
            "git",
            if refuse_git {
                "#!/bin/sh\necho fixture-merge-refusal >&2\nexit 2\n".to_owned()
            } else {
                format!("#!/bin/sh\nprintf '%s\\trefs/heads/main\\n' '{SHA}'\n")
            },
        ),
        ("readlink", "#!/bin/sh\nexit 1\n".into()),
    ] {
        let path = bin.join(name);
        boss_testing::write_exec(&path, &source);
    }
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let stopped = Arc::new(AtomicBool::new(false));
    let seen = requests.clone();
    let done = stopped.clone();
    let server = std::thread::spawn(move || {
        while !done.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream
                        .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                        .unwrap();
                    let mut head = Vec::new();
                    let mut bytes = [0; 4096];
                    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                        let n = stream.read(&mut bytes).unwrap();
                        if n == 0 {
                            break;
                        }
                        head.extend_from_slice(&bytes[..n]);
                    }
                    let request = String::from_utf8_lossy(&head).into_owned();
                    let body = if request.starts_with("GET /api/jobs/health ") {
                        json!({ "capabilities": { "commit": SHA } }).to_string()
                    } else if request.starts_with("GET /api/jobs?") {
                        assert!(request.contains("kind=maintenance-cluster-converge"));
                        assert!(request.contains("full=true"));
                        assert!(request.contains("limit=1"));
                        record.to_string()
                    } else {
                        panic!("unexpected request: {request}");
                    };
                    seen.lock().unwrap().push(request);
                    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(e) => panic!("fixture listener: {e}"),
            }
        }
    });
    let path = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))
    .unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_boss"))
        .env_clear()
        .env("PATH", path)
        .env("HOME", &root)
        .env("BOSS_ACTOR", "fixture-builder")
        .env("BOSS_JOBS_URL", format!("http://{address}"))
        .env("BOSS_MACHINE_TOKEN_DIR", tokens)
        .env("BOSS_MACHINE_TOKEN_HOSTS", "127.0.0.1")
        .env("BOSS_SOR_ENV", root.join("absent-sor.env"))
        .args(["running", "--clone", root.to_str().unwrap()])
        .output()
        .unwrap();
    stopped.store(true, Ordering::SeqCst);
    server.join().unwrap();
    let requests = requests.lock().unwrap().clone();
    std::fs::remove_dir_all(root).unwrap();
    (output, requests)
}
