//! The root-run doors read no curlrc (backlog 2b1bf9b4, found by review
//! 4ceac0c8 of car 4c4e23bf, 2026-10-21).
//!
//! `/work/home` is HOME for root on the dev pod, and it is writable by
//! group 1500 — the gate's gid, which every builder runs as. A sticky
//! bit stops a rename, not a new file, so a group member can create
//! `/work/home/.curlrc`, and curl reads that file with no ownership
//! check. `infra/dev/boss-api` runs curl as root with the machine token
//! aboard, and `infra/lib/secret-header.sh` judges only the environment's
//! proxies: a `proxy =` line planted in the home routed the door's next
//! call, token included, to an address of the planter's choosing. The
//! reviewer demonstrated it on a fixture home: a request for 127.0.0.1:9
//! went to 127.0.0.1:1.
//!
//! curl's `-q` (`--disable`) skips every curlrc, and ONLY as curl's first
//! argument — anywhere else it is a no-op. So:
//!
//!   * the door is run here with the REAL curl, a fixture HOME holding a
//!     planted `.curlrc` that names a trap listener as its proxy, and a
//!     fixture token: the request must reach the record's listener, with
//!     the token, and the trap must see nothing. A control leg runs a
//!     bare curl under the same HOME first and must reach the trap, so
//!     the fixture is proven strong enough to catch a door that reads it;
//!   * every curl a shell file under `infra/dev` runs, and the one the
//!     roll wait runs for every shell reader of the record
//!     (`infra/lib/curl-through-a-roll.sh`), has `-q` as its first
//!     argument, so a curl added later cannot quietly read the home's.

use boss_testing::{create_dir, repo_root, scratch_dir, without_proxy, write_file};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

/// A loopback HTTP listener that answers every request `200` with its
/// own name as the body and keeps each request's head, until finished.
struct Listener {
    port: u16,
    stop: Arc<AtomicBool>,
    seen: JoinHandle<Vec<String>>,
}

impl Listener {
    fn start(name: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
        listener
            .set_nonblocking(true)
            .expect("a non-blocking listener");
        let port = listener.local_addr().expect("its address").port();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let seen = std::thread::spawn(move || {
            let mut seen = Vec::new();
            while !stopping.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut conn, _)) => {
                        let _ = conn.set_nonblocking(false);
                        let _ = conn.set_read_timeout(Some(Duration::from_secs(5)));
                        let mut head = Vec::new();
                        let mut chunk = [0u8; 4096];
                        while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                            match conn.read(&mut chunk) {
                                Ok(0) | Err(_) => break,
                                Ok(n) => head.extend_from_slice(&chunk[..n]),
                            }
                        }
                        seen.push(String::from_utf8_lossy(&head).into_owned());
                        let _ = write!(
                            conn,
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                             Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                            name.len(),
                            name
                        );
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    Err(_) => break,
                }
            }
            seen
        });
        Self { port, stop, seen }
    }

    /// Stop listening; every request head it saw.
    fn finish(self) -> Vec<String> {
        self.stop.store(true, Ordering::SeqCst);
        self.seen.join().expect("the listener thread")
    }
}

/// A fixture HOME whose `.curlrc` sends everything through `trap_port`,
/// as a planted one would. Every other place curl looks for a curlrc is
/// unset by `curl_env`, so this HOME's is the one it would read.
fn planted_home(root: &Path, trap_port: u16) -> PathBuf {
    let home = root.join("home");
    create_dir(&home);
    write_file(
        &home.join(".curlrc"),
        &format!("proxy = \"http://127.0.0.1:{trap_port}\"\nheader = \"X-Planted: yes\"\n"),
    );
    home
}

/// Strip what reaches curl from the suite's own shell: its proxies and
/// every other curlrc location, so only the fixture HOME's can steer it.
fn curl_env<'a>(cmd: &'a mut Command, home: &Path) -> &'a mut Command {
    without_proxy(cmd)
        .env("HOME", home)
        .env_remove("CURL_HOME")
        .env_remove("XDG_CONFIG_HOME")
}

#[test]
fn a_curlrc_planted_in_the_home_never_steers_the_door() {
    let root = scratch_dir("doors-read-no-curlrc");
    let record = Listener::start("record");
    let trap = Listener::start("trap");
    let home = planted_home(&root, trap.port);
    let url = format!("http://127.0.0.1:{}", record.port);

    // CONTROL: this curl, under this HOME, honours the planted file — a
    // bare request goes to the trap. Without it, a door that reads the
    // file would pass on a fixture that steers nothing.
    let control = curl_env(&mut Command::new("curl"), &home)
        .args(["-sS", &format!("{url}/control")])
        .output()
        .expect("run curl");
    assert_eq!(
        String::from_utf8_lossy(&control.stdout),
        "trap",
        "the control: a bare curl under the fixture HOME must be steered to the trap, or this \
         fixture proves nothing\n{control:?}"
    );

    // THE DOOR, with a fixture token, run as it runs on the pod.
    let token_dir = root.join("machine-token");
    create_dir(&token_dir);
    write_file(
        &token_dir.join("current"),
        "fixture-token-not-a-credential\n",
    );
    let mut door = Command::new(repo_root().join("infra/dev/boss-api"));
    curl_env(&mut door, &home)
        .current_dir(&root)
        .args(["GET", "/api/jobs/fixture"])
        .env("BOSS_JOBS_URL", &url)
        .env("BOSS_ACTOR", "agent-fixture")
        .env("BOSS_MACHINE_TOKEN_DIR", &token_dir)
        .env("BOSS_SOR_WAIT_SECONDS", "0")
        .env("BOSS_DOOR_FRESHNESS", "off")
        .env_remove("BOSS_ACTOR_FILE")
        .env_remove("BOSS_MACHINE_TOKEN_HOSTS")
        .env_remove("BOSS_SOR_PORTS")
        .env_remove("BOSS_SOR_SERVICE")
        .env_remove("BOSS_SOR_READ_PORTS")
        .env_remove("BOSS_SOR_READ_URL");
    let out = door.output().expect("run boss-api");
    let said = format!(
        "status {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let at_record = record.finish();
    let at_trap = trap.finish();
    let _ = std::fs::remove_dir_all(&root);

    assert!(
        at_trap.iter().all(|h| !h.contains("/api/jobs/fixture")),
        "the door's request went to the proxy a planted .curlrc named — with the machine token \
         aboard (backlog 2b1bf9b4); -q must be curl's FIRST argument\ntrap saw:\n{at_trap:#?}\n{said}"
    );
    assert_eq!(out.status.code(), Some(0), "{said}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "record",
        "the door's answer is the record's\n{said}"
    );
    let head = at_record
        .iter()
        .find(|h| h.starts_with("GET /api/jobs/fixture "))
        .unwrap_or_else(|| panic!("the record saw the door's request\n{at_record:#?}\n{said}"));
    let lower = head.to_ascii_lowercase();
    assert!(
        lower.contains("x-boss-machine-token:"),
        "the door still stamps the record it was pointed at\n{said}"
    );
    assert!(
        !lower.contains("x-planted"),
        "no header the planted file names reaches the record\n{head}"
    );
}

/// Every shell file under `infra/dev`, and the roll wait every shell
/// reader of the record runs curl through.
fn root_run_curl_sources() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                walk(&path, out);
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let first = text.lines().next().unwrap_or_default();
            let shell = path.extension().is_some_and(|e| e == "sh")
                || (first.starts_with("#!") && (first.contains("bash") || first.ends_with("/sh")));
            if shell {
                out.push(path);
            }
        }
    }
    let root = repo_root();
    let mut out = vec![root.join("infra/lib/curl-through-a-roll.sh")];
    walk(&root.join("infra/dev"), &mut out);
    out
}

/// Each curl run on `line` whose first argument is not `-q`. Quoted
/// spans are dropped first: there curl is prose (`echo "… curl …"`),
/// not a command.
fn curls_without_q_first(line: &str) -> Vec<String> {
    let code = line.trim_start();
    if code.starts_with('#') {
        return Vec::new();
    }
    let unquoted = regex::Regex::new(r#"'[^']*'|"(?:[^"\\]|\\.)*""#)
        .expect("the quote pattern")
        .replace_all(code, "''");
    regex::Regex::new(r"(?:^|[\s;&|(!{`])curl(?:\s+(\S+))?(?:\s|$)")
        .expect("the curl pattern")
        .captures_iter(&unquoted)
        .filter(|c| c.get(1).map(|m| m.as_str()) != Some("-q"))
        .map(|c| c[0].trim().to_string())
        .collect()
}

#[test]
fn every_root_run_curl_under_infra_dev_passes_q_first() {
    let mut offenders = Vec::new();
    let mut curls = 0;
    for path in root_run_curl_sources() {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        for (n, line) in text.lines().enumerate() {
            let code = line.trim_start();
            if !code.starts_with('#') && code.contains("curl -q") {
                curls += 1;
            }
            if !curls_without_q_first(line).is_empty() {
                offenders.push(format!(
                    "{}:{}: {}",
                    path.strip_prefix(repo_root()).unwrap_or(&path).display(),
                    n + 1,
                    line.trim()
                ));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "a root-run curl must pass -q as its FIRST argument, or it reads a .curlrc a group \
         member may plant in /work/home (backlog 2b1bf9b4):\n{}",
        offenders.join("\n")
    );
    assert!(
        curls >= 2,
        "the scan found the roll wait's curl and the installer's: it reads the files it means to"
    );
}

#[test]
fn the_scan_names_a_curl_without_q_first_and_passes_prose() {
    assert_eq!(curls_without_q_first("rc=0; curl \"$@\" >&3").len(), 1);
    assert_eq!(curls_without_q_first("  x=$(curl -sS \"$u\")").len(), 1);
    assert_eq!(
        curls_without_q_first("a | curl -fsSL https://x | bash").len(),
        1
    );
    assert!(curls_without_q_first("rc=0; curl -q \"$@\" >&3").is_empty());
    assert!(curls_without_q_first("claude) curl -q -fsSL https://x | bash ;;").is_empty());
    assert!(curls_without_q_first("# curl runs here").is_empty());
    assert!(curls_without_q_first("echo 'lacks the curl HTTP trailer' >&2").is_empty());
    assert!(curls_without_q_first("curl_through_a_roll 0 who svc what").is_empty());
    assert!(curls_without_q_first("ROLL_LIB=\"$HERE/../lib/curl-through-a-roll.sh\"").is_empty());
}
