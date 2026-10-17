//! Exercise the actual candidate executable, without contacting the estate.
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[test]
fn a_candidate_prove_recheck_without_a_credential_sends_no_request() {
    for token in [None, Some(""), Some("invalid\nheader")] {
        let (output, requests) = candidate(token);
        assert!(!output.status.success());
        assert!(
            requests.is_empty(),
            "candidate emitted an unstamped live-shaped request: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("machine credential"));
    }
}

#[test]
fn the_same_candidate_command_with_a_synthetic_credential_reaches_the_fixture_once() {
    let (output, requests) = candidate(Some("synthetic-fixture-credential"));
    // The stub intentionally supplies no car record; this checks transport,
    // not whether a nonexistent car can be proven.
    assert!(!output.status.success());
    assert_eq!(requests.len(), 1);
    let head = requests[0].to_lowercase();
    assert_eq!(head.matches("x-boss-machine-token:").count(), 1);
    assert!(head.contains("x-boss-machine-token: synthetic-fixture-credential"));
}

fn candidate(token: Option<&str>) -> (std::process::Output, Vec<String>) {
    let root = boss_testing::scratch_dir("candidate-machine-refusal");
    let token_dir = root.join("fixture-token");
    if let Some(token) = token {
        std::fs::create_dir_all(&token_dir).unwrap();
        std::fs::write(token_dir.join("current"), token).unwrap();
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
                        .set_read_timeout(Some(std::time::Duration::from_secs(1)))
                        .unwrap();
                    let mut bytes = [0; 4096];
                    let length = stream.read(&mut bytes).unwrap();
                    seen.lock()
                        .unwrap()
                        .push(String::from_utf8_lossy(&bytes[..length]).into_owned());
                    let _ = stream.write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                    );
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(error) => panic!("fixture listener: {error}"),
            }
        }
    });
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_boss"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &root)
        .env("BOSS_ACTOR", "fixture-builder")
        .env("BOSS_JOBS_URL", format!("http://{address}"))
        .env("BOSS_MACHINE_TOKEN_DIR", token_dir)
        .env("BOSS_MACHINE_TOKEN_HOSTS", "127.0.0.1")
        .env("BOSS_SOR_ENV", root.join("absent-sor.env"))
        .args(["prove", "98361745-4a33-46a0-9c4d-0e396d4a8ea6", "--recheck"])
        .output()
        .unwrap();
    stopped.store(true, Ordering::SeqCst);
    server.join().unwrap();
    let requests = requests.lock().unwrap().clone();
    std::fs::remove_dir_all(root).unwrap();
    (output, requests)
}
