//! A sender killed mid-request must not leave the machine token's header
//! file where the next process, or another account, can find it (backlog
//! df38075a, F1 of review a92c0b94).
//!
//! THE DEFECT, MEASURED 2026-10-06: the dev pod's /tmp held four
//! `boss-secret-header.*` directories dated 10-03 to 10-06, three with a
//! `TOKEN_HDR` file — the pod door's variable — left by killed `boss-api`
//! calls. `infra/lib/secret-header.sh` removes its directory from an EXIT
//! trap, and a SIGKILL runs no trap in any shell, a SIGTERM none under
//! dash. Five of the six stamped host units declared no
//! `RuntimeDirectory=`, so their header lived in the host's shared /tmp,
//! and `post_observation`'s curl had no time bound: in a oneshot, whose
//! start timeout systemd leaves OFF by default, a system of record that
//! accepts and never answers held the tick — and its header — for good.
//!
//! WHAT IS HELD HERE, each by running it:
//!   * the pod door hands curl an OPEN DESCRIPTOR of a file it has already
//!     removed, so a kill at any point of the request finds no file;
//!   * the reader records who owns a header directory and, at its next
//!     start, removes the ones this account's dead runs left — and only
//!     those: not a live sender's, not another pid namespace's, not a
//!     young directory it cannot place, never through a link;
//!   * the six units declare a private RuntimeDirectory, no two alike;
//!   * a post to a record that never answers ends by its own time bound.
//!
//! tree-wide pin — it walks every unit file under infra/, which no
//! changed-file map attributes to this crate: a unit added anywhere that
//! names a RuntimeDirectory another unit holds must turn it red.

use boss_testing::{create_dir, repo_root, scratch_dir, write_exec, write_file};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const LIB: &str = "infra/lib/secret-header.sh";
const DOOR: &str = "infra/dev/boss-api";
const OBSERVE_LIB: &str = "infra/estate/observe-lib.sh";

/// Not a real token: a string no path or argv holds by accident.
const SECRET: &str = "not-a-token-41c7e2";

/// The six units the host-stamping car made senders of the token (the
/// item's own list), with the account each runs as — read from the file,
/// asserted below, because WHO owns /run/<name> is the point.
const UNITS: [(&str, &str); 6] = [
    ("infra/forge/cluster-deploy-runner.service", "david"),
    // root since backlog a604a35b: /run/cluster-watchdog is root's alone.
    ("infra/forge/cluster-watchdog.service", "root"),
    ("infra/forge/estate-observe-host.service", "root"),
    ("infra/forge/estate-observe-units.service", "root"),
    ("infra/estate/boss-estate-observe-host.service", "root"),
    ("infra/estate/boss-estate-observe-units.service", "root"),
];

struct Fixture {
    root: PathBuf,
    bin: PathBuf,
    tmp: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Fixture {
        let root = scratch_dir(&format!("killed-sender-{tag}"));
        let bin = root.join("bin");
        let tmp = root.join("tmp");
        create_dir(&bin);
        create_dir(&tmp);
        // curl: for a `-H @file`, record the path and the text exactly as
        // curl would read them; say it started; with STUB_HANG, wait to be
        // released (30 s at most, so a failed test leaves no process); then
        // say whether the header could STILL be read, and answer as the
        // real one does under `-w '\n%{http_code}'`.
        write_exec(
            &bin.join("curl"),
            "#!/bin/sh\n\
             hdr=\n\
             prev=\n\
             for a in \"$@\"; do\n\
                 if [ \"$prev\" = -H ]; then case \"$a\" in @*) hdr=${a#@} ;; esac; fi\n\
                 prev=$a\n\
             done\n\
             if [ -n \"$hdr\" ]; then\n\
                 printf '%s\\n' \"$hdr\" > \"$STUB_DIR/path.txt\"\n\
                 cat \"$hdr\" > \"$STUB_DIR/header.txt\" 2>/dev/null\n\
             fi\n\
             : > \"$STUB_DIR/started\"\n\
             if [ -n \"${STUB_HANG:-}\" ]; then\n\
                 n=0\n\
                 while [ ! -e \"$STUB_DIR/release\" ] && [ \"$n\" -lt 300 ]; do sleep 0.1; n=$((n + 1)); done\n\
                 if [ -n \"$hdr\" ] && cat \"$hdr\" > \"$STUB_DIR/header-after.txt\" 2>/dev/null; then : ; fi\n\
             fi\n\
             printf '\\n200'\n",
        );
        Fixture { root, bin, tmp }
    }

    fn command(&self, program: &Path) -> Command {
        let mut cmd = Command::new(program);
        cmd.current_dir(&self.root)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("STUB_DIR", &self.root)
            .env("TMPDIR", &self.tmp)
            .env("SECRET", SECRET)
            .env("HOME", &self.root)
            .env_remove("RUNTIME_DIRECTORY")
            .env_remove("STUB_HANG")
            .env_remove("BOSS_MACHINE_TOKEN_HOSTS")
            .env_remove("BOSS_MACHINE_TOKEN")
            .env_remove("BOSS_ACTOR_FILE")
            .env_remove("BOSS_SOR_PORTS")
            .env_remove("BOSS_SOR_SERVICE")
            .env_remove("BOSS_SOR_READ_PORTS")
            .env_remove("BOSS_SOR_READ_URL")
            .env_remove("BOSS_SOR_WAIT_SECONDS")
            .env("BOSS_MACHINE_TOKEN_DIR", self.root.join("no-token-dir"))
            .env("BOSS_SOR_ENV", self.root.join("no-sor.env"))
            .stdin(Stdio::null());
        cmd
    }

    /// A shell driver with the lib sourced first.
    fn lib_driver(&self, shell: &str, name: &str, body: &str) -> Command {
        let driver = self.root.join(format!("{name}.sh"));
        write_file(
            &driver,
            &format!(". \"{}\"\n{body}", repo_root().join(LIB).display()),
        );
        let mut cmd = self.command(Path::new(shell));
        cmd.arg(&driver);
        cmd
    }

    /// The pod door, with a mounted token it will stamp `sor.test` with.
    fn door(&self) -> Command {
        let mount = self.root.join("machine-token");
        if !mount.exists() {
            create_dir(&mount);
            write_file(&mount.join("current"), &format!("{SECRET}\n"));
        }
        let mut cmd = self.command(&repo_root().join(DOOR));
        cmd.args(["GET", "/api/jobs"])
            .env("BOSS_JOBS_URL", "http://sor.test:7900")
            .env("BOSS_MACHINE_TOKEN_DIR", &mount)
            .env("BOSS_MACHINE_TOKEN_HOSTS", "sor.test")
            .env("BOSS_ACTOR", "emp-reader");
        cmd
    }

    /// Start `cmd` with its output in files (a pipe would be held open by
    /// the orphaned stub after its parent is killed) and wait until the
    /// stub curl says the request is in flight.
    fn spawn_until_in_flight(&self, mut cmd: Command, tag: &str) -> Child {
        let _ = std::fs::remove_file(self.root.join("started"));
        let out = std::fs::File::create(self.root.join(format!("{tag}.out"))).expect("out file");
        let err = std::fs::File::create(self.root.join(format!("{tag}.err"))).expect("err file");
        let child = cmd
            .env("STUB_HANG", "1")
            .stdout(out)
            .stderr(err)
            .spawn()
            .expect("spawn the sender");
        self.wait_for("started", tag);
        child
    }

    fn wait_for(&self, name: &str, tag: &str) {
        let until = Instant::now() + Duration::from_secs(20);
        while !self.root.join(name).exists() {
            assert!(
                Instant::now() < until,
                "{tag}: `{name}` never appeared; stderr: {}",
                self.read(&format!("{tag}.err"))
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Let a hanging stub curl finish, and wait until it has.
    fn release(&self) {
        write_file(&self.root.join("release"), "");
    }

    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.root.join(name)).unwrap_or_default()
    }

    /// The header directories under `dir`.
    fn headers_under(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .map(|rd| {
                rd.filter_map(Result::ok)
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    }

    /// Every file under `dir`, at any depth, whose text holds the token.
    fn files_holding_the_token(dir: &Path) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let Ok(rd) = std::fs::read_dir(dir) else {
            return found;
        };
        for e in rd.filter_map(Result::ok) {
            let p = e.path();
            let Ok(meta) = std::fs::symlink_metadata(&p) else {
                continue;
            };
            if meta.is_dir() {
                found.extend(Self::files_holding_the_token(&p));
            } else if std::fs::read_to_string(&p)
                .map(|t| t.contains(SECRET))
                .unwrap_or(false)
            {
                found.push(p);
            }
        }
        found
    }
}

/// Signal `child` with bash's BUILTIN kill. The gate's image ships no
/// `kill` program — it has neither procps nor psmisc — and this file's
/// first gate (gate-run 204a7f86) died on `run kill: NotFound` in the two
/// tests that signal. The spelling is the tree's own, from
/// the_chore_runs_through_a_sor_that_answers_badly.rs, which met the same.
fn kill(child: &mut Child, signal: &str) {
    let status = Command::new("bash")
        .args(["-c", "kill \"-$1\" -- \"$2\"", "kill", signal])
        .arg(child.id().to_string())
        .status()
        .expect("run bash's builtin kill");
    assert!(status.success(), "kill -{signal} {}", child.id());
    let _ = child.wait();
}

fn my_pid_namespace() -> String {
    std::fs::read_link("/proc/self/ns/pid")
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// A pid no process holds: one that has just been reaped.
fn a_dead_pid() -> u32 {
    let mut c = Command::new("sh")
        .args(["-c", "exit 0"])
        .spawn()
        .expect("spawn sh");
    let pid = c.id();
    c.wait().expect("reap");
    pid
}

/// Plant a header directory as a killed run would have left it. `owner`
/// is the text of its `.owner` record, or None for a directory made by a
/// reader older than the record.
fn plant(tmp: &Path, name: &str, owner: Option<&str>) -> PathBuf {
    let dir = tmp.join(format!("boss-secret-header.{name}"));
    create_dir(&dir);
    write_file(
        &dir.join("TOKEN_HDR"),
        &format!("x-boss-machine-token: {SECRET}\n"),
    );
    if let Some(owner) = owner {
        write_file(&dir.join(".owner"), &format!("{owner}\n"));
    }
    dir
}

fn age_by_two_days(dir: &Path) {
    let ok = Command::new("touch")
        .args(["-d", "2 days ago"])
        .arg(dir)
        .status()
        .expect("run touch")
        .success();
    assert!(ok, "touch -d on {}", dir.display());
}

const SEND: &str = "secret_header H \"x-boss-machine-token: $SECRET\"\n\
                    curl -sS -H \"$H\" http://sor.test/api/jobs >/dev/null\n";

/// The reader cannot stop a SIGKILL from leaving its directory — nothing
/// in a shell can — so the NEXT run of the same account removes it, and
/// says that it did by path and count, never by content.
#[test]
fn a_killed_run_leaves_a_directory_and_the_accounts_next_run_removes_it() {
    for (shell, signal) in [("bash", "KILL"), ("dash", "KILL"), ("dash", "TERM")] {
        let f = Fixture::new(&format!("sweep-{shell}-{signal}"));
        let mut first = f.spawn_until_in_flight(f.lib_driver(shell, "first", SEND), "first");
        kill(&mut first, signal);
        f.release();
        let left = Fixture::headers_under(&f.tmp);
        assert_eq!(
            left.len(),
            1,
            "{shell} -{signal}: the control — a killed run DOES leave its directory, \
             or this test proves nothing: {left:?}"
        );
        assert_eq!(
            Fixture::files_holding_the_token(&f.tmp).len(),
            1,
            "{shell} -{signal}: and the header in it"
        );

        let out = f
            .lib_driver(shell, "second", SEND)
            .output()
            .expect("run the second sender");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{shell} -{signal}: {err}");
        assert_eq!(
            Fixture::headers_under(&f.tmp),
            Vec::<String>::new(),
            "{shell} -{signal}: the next run removes what the killed one left, and its own: {err}"
        );
        assert!(
            err.contains("removed 1 header directory") && err.contains(&left[0]),
            "{shell} -{signal}: and names what it removed: {err}"
        );
        assert!(
            !err.contains(SECRET),
            "{shell} -{signal}: never the value: {err}"
        );
        assert_eq!(
            f.read("header.txt"),
            format!("x-boss-machine-token: {SECRET}\n"),
            "{shell} -{signal}: the second run's own header still reached curl"
        );
    }
}

/// A sweep that took a LIVE sender's header would send its next request
/// unstamped — a would-refuse fact made by the cleanup. A second run of
/// the same account, started while the first is mid-request, leaves the
/// first one's directory alone.
#[test]
fn a_live_senders_header_is_never_swept() {
    for shell in ["bash", "dash"] {
        let f = Fixture::new(&format!("live-{shell}"));
        let mut first = f.spawn_until_in_flight(f.lib_driver(shell, "first", SEND), "first");
        let mine = Fixture::headers_under(&f.tmp);
        assert_eq!(mine.len(), 1, "{shell}: the first sender's directory");

        // The second sender uses its own stub directory, so it cannot
        // release or overwrite the first one's record.
        let g = Fixture::new(&format!("live-{shell}-second"));
        let out = g
            .lib_driver(shell, "second", SEND)
            .env("TMPDIR", &f.tmp)
            .output()
            .expect("run the second sender");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{shell}: {err}");
        assert!(
            !err.contains("removed"),
            "{shell}: nothing was there to remove: {err}"
        );
        assert_eq!(
            Fixture::headers_under(&f.tmp),
            mine,
            "{shell}: the live sender's directory stands, the second's own is gone"
        );

        f.release();
        assert!(first.wait().expect("first sender").success());
        assert_eq!(
            f.read("header-after.txt"),
            format!("x-boss-machine-token: {SECRET}\n"),
            "{shell}: the first sender's curl could still read its header after the second ran"
        );
        assert_eq!(Fixture::headers_under(&f.tmp), Vec::<String>::new());
    }
}

/// Which directories the sweep may judge. One table, both shells.
#[test]
fn the_sweep_removes_only_what_it_can_show_is_dead() {
    let ns = my_pid_namespace();
    assert!(
        ns.starts_with("pid:["),
        "this suite runs on Linux with /proc: {ns}"
    );
    let live = std::process::id();
    for shell in ["bash", "dash"] {
        let f = Fixture::new(&format!("table-{shell}"));
        // (name, .owner, aged two days, swept?)
        let dead = a_dead_pid();
        let rows: Vec<(&str, Option<String>, bool, bool)> = vec![
            // Its owner is gone from this pid namespace.
            ("dead", Some(format!("{dead} 1 {ns}")), false, true),
            // The pid is alive but was born at another time: a recycled
            // pid, not the owner.
            ("recycled", Some(format!("{live} 1 {ns}")), false, true),
            // Another pid namespace shares this directory (a second
            // container on one volume): its pids mean nothing here, so
            // only age can judge it.
            ("elsewhere", Some(format!("{dead} 1 pid:[1]")), false, false),
            (
                "elsewhere-old",
                Some(format!("{dead} 1 pid:[1]")),
                true,
                true,
            ),
            // Made by a reader older than the record, or killed before
            // the record was written: young stays, a day old goes.
            ("unowned", None, false, false),
            ("unowned-old", None, true, true),
            // A record that is not three plain fields is no record.
            ("garbled", Some("$(id) x y z".to_string()), false, false),
            // ...even when its first three fields would name a dead owner.
            (
                "four-fields",
                Some(format!("{dead} 1 {ns} more")),
                false,
                false,
            ),
        ];
        for (name, owner, aged, _) in &rows {
            let dir = plant(&f.tmp, name, owner.as_deref());
            if *aged {
                age_by_two_days(&dir);
            }
        }
        // A LINK under the name is never followed: what it points at is
        // someone else's directory.
        let victim = f.root.join("victim");
        create_dir(&victim);
        write_file(&victim.join("keep"), "kept\n");
        // Old enough that only the link check keeps it.
        age_by_two_days(&victim);
        let link = f.tmp.join("boss-secret-header.link");
        std::os::unix::fs::symlink(&victim, &link).expect("plant the link");
        // The link's OWN age too (`touch -h`), which is what find reads.
        let aged = Command::new("touch")
            .args(["-h", "-d", "2 days ago"])
            .arg(&link)
            .status()
            .expect("run touch -h")
            .success();
        assert!(aged, "touch -h on the link");
        // And a name that is not the reader's is not the reader's.
        create_dir(&f.tmp.join("boss-secret-headers-of-someone-else"));

        let out = f
            .lib_driver(shell, "sweeper", SEND)
            .output()
            .expect("run the sweeper");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{shell}: {err}");
        for (name, _, _, swept) in &rows {
            let there = f.tmp.join(format!("boss-secret-header.{name}")).exists();
            assert_eq!(
                there, !*swept,
                "{shell}: `{name}` — swept is {swept}, still there is {there}: {err}"
            );
        }
        assert_eq!(
            std::fs::read_to_string(victim.join("keep")).unwrap_or_default(),
            "kept\n",
            "{shell}: nothing was removed through the link"
        );
        assert!(
            std::fs::symlink_metadata(&link).is_ok(),
            "{shell}: and the link itself was not judged at all: {err}"
        );
        assert!(
            f.tmp.join("boss-secret-headers-of-someone-else").exists(),
            "{shell}: a directory under another name is untouched"
        );
        assert!(
            err.contains("removed 4 header directories"),
            "{shell}: the count is said: {err}"
        );
        assert!(!err.contains(SECRET), "{shell}: never the value: {err}");
    }
}

/// THE MEASURED CASE. `boss-api` killed while curl waits on the system of
/// record: nothing under the temp directory holds the token, at the
/// moment of the request or after the kill, and the header still reached
/// curl. The door has no unit and so no RuntimeDirectory; it hands curl a
/// descriptor of a file it removed before the request left.
#[test]
fn the_pod_door_killed_mid_request_leaves_no_header_on_disk() {
    let f = Fixture::new("door-kill");
    let mut door = f.spawn_until_in_flight(f.door(), "door");
    assert_eq!(
        f.read("header.txt"),
        format!("x-boss-machine-token: {SECRET}\n"),
        "the control — the token header reached curl: {}",
        f.read("door.err")
    );
    assert_eq!(
        Fixture::files_holding_the_token(&f.tmp),
        Vec::<PathBuf>::new(),
        "while the request is in flight no file under the temp directory holds the token"
    );
    assert_eq!(
        Fixture::headers_under(&f.tmp),
        Vec::<String>::new(),
        "nor is there a header directory"
    );
    kill(&mut door, "KILL");
    assert_eq!(
        Fixture::headers_under(&f.tmp),
        Vec::<String>::new(),
        "and a kill leaves nothing behind"
    );
    f.release();
    f.wait_for("header-after.txt", "door");
    assert_eq!(
        f.read("header-after.txt"),
        format!("x-boss-machine-token: {SECRET}\n"),
        "a second read of the header — the door's retry after a refused connect is a new \
         curl — finds the whole line again"
    );
    let path = f.read("path.txt");
    assert!(
        !Path::new(path.trim()).starts_with(&f.tmp),
        "curl was handed no path under the temp directory: {path}"
    );
}

/// The stub reads the descriptor with `cat`; this is the REAL curl
/// against a real socket, because "curl can read a header from
/// /proc/self/fd/N" is the claim the door now rests on. The request that
/// arrives carries the header line, and nothing is left under the temp
/// directory.
#[test]
fn the_real_curl_sends_the_header_it_reads_from_the_descriptor() {
    use std::io::{Read, Write};
    let f = Fixture::new("door-real-curl");
    std::fs::remove_file(f.bin.join("curl")).expect("drop the stub: this test runs the real curl");
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let server = std::thread::spawn(move || {
        let (mut s, _) = listener.accept().expect("accept");
        s.set_read_timeout(Some(Duration::from_secs(10)))
            .expect("read timeout");
        let mut req = Vec::new();
        let mut buf = [0u8; 1024];
        while !req.windows(4).any(|w| w == b"\r\n\r\n") {
            match s.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => req.extend_from_slice(&buf[..n]),
            }
        }
        let _ = s.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}");
        String::from_utf8_lossy(&req).into_owned()
    });
    let out = f
        .door()
        .env("BOSS_JOBS_URL", format!("http://127.0.0.1:{port}"))
        .env("BOSS_SOR_WAIT_SECONDS", "0")
        .output()
        .expect("run the door");
    let err = String::from_utf8_lossy(&out.stderr);
    let request = server.join().expect("the listener thread");
    assert!(out.status.success(), "{err}");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "{}", "{err}");
    assert!(
        request
            .lines()
            .any(|l| l.eq_ignore_ascii_case(&format!("x-boss-machine-token: {SECRET}"))),
        "the request the real curl sent carries the header: {} header line(s), stderr {err}",
        request.lines().count()
    );
    assert_eq!(Fixture::headers_under(&f.tmp), Vec::<String>::new());
}

/// The door answers as before, and removes what an earlier killed call of
/// this account left (the four directories of 2026-10-06 were this).
#[test]
fn the_pod_door_removes_a_killed_calls_directory_and_still_answers() {
    let f = Fixture::new("door-sweep");
    let ns = my_pid_namespace();
    let stale = plant(&f.tmp, "stale", Some(&format!("{} 1 {ns}", a_dead_pid())));
    let out = f.door().output().expect("run the door");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{err}");
    assert!(!stale.exists(), "the stale directory is gone: {err}");
    assert!(
        err.lines().last() == Some("HTTP:200"),
        "the HTTP line is still the last thing on stderr: {err}"
    );
    assert_eq!(
        f.read("header.txt"),
        format!("x-boss-machine-token: {SECRET}\n")
    );
    assert_eq!(Fixture::headers_under(&f.tmp), Vec::<String>::new());
    assert!(!err.contains(SECRET), "never the value: {err}");
}

/// Each stamped unit keeps the header in a directory of its own under
/// /run: 0700, owned by the account the unit runs as, removed by systemd
/// when the unit stops whatever ended it. A oneshot on a timer loses it at
/// every exit, which is the point.
#[test]
fn every_stamped_host_unit_declares_a_private_runtime_directory() {
    for (unit, user) in UNITS {
        let text = std::fs::read_to_string(repo_root().join(unit))
            .unwrap_or_else(|e| panic!("read {unit}: {e}"));
        let lines: Vec<&str> = text.lines().map(str::trim).collect();
        let stem = Path::new(unit)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        assert!(
            lines.contains(&format!("RuntimeDirectory={stem}").as_str()),
            "{unit}: must declare RuntimeDirectory={stem} — without it the token's header \
             file lives in the host's shared /tmp and outlives a killed run (backlog df38075a)"
        );
        assert!(
            lines.contains(&"RuntimeDirectoryMode=0700"),
            "{unit}: the directory is the account's alone (RuntimeDirectoryMode=0700)"
        );
        assert!(
            lines.contains(&"Type=oneshot"),
            "{unit}: a oneshot — its RuntimeDirectory goes at every exit"
        );
        assert!(
            !lines
                .iter()
                .any(|l| l.starts_with("RuntimeDirectoryPreserve=")),
            "{unit}: nothing may keep the directory past the run"
        );
        // No User= is root for a system unit, which is what the two
        // boss-gcp units are.
        let declared = lines
            .iter()
            .find_map(|l| l.strip_prefix("User="))
            .unwrap_or("root");
        assert_eq!(
            declared, user,
            "{unit}: the account that owns /run/{stem} changed — re-read who may read the header"
        );
    }
}

/// THE HALF THE FIRST CAR LEFT (backlog df38075a, F1, second car). A unit
/// that gains a RuntimeDirectory stops making its header under /tmp — and
/// stopped SWEEPING /tmp too, because the reader swept only the directory
/// it was about to write in. What the unit's killed runs had already left
/// there then had no remover at all: the only senders still looking in
/// /tmp were hand runs. So a run under a RuntimeDirectory judges the temp
/// directory as well, by the same rules, and makes its own header in the
/// RuntimeDirectory as before.
#[test]
fn a_run_under_a_runtime_directory_removes_what_a_killed_run_left_in_the_temp_directory() {
    let ns = my_pid_namespace();
    for shell in ["bash", "dash"] {
        let f = Fixture::new(&format!("sweep-tmp-under-run-{shell}"));
        // The unit's last run before it had a RuntimeDirectory, killed.
        let mut first = f.spawn_until_in_flight(f.lib_driver(shell, "first", SEND), "first");
        kill(&mut first, "KILL");
        f.release();
        let left = Fixture::headers_under(&f.tmp);
        assert_eq!(
            left.len(),
            1,
            "{shell}: the control — the killed run left its directory under the temp \
             directory: {left:?}"
        );
        // One it cannot place and that is young: not this run's to judge.
        plant(&f.tmp, "young", None);
        // And one a dead run left in the RuntimeDirectory itself (a pod's
        // memory volume outlives the process that wrote in it).
        let run = f.root.join("run");
        create_dir(&run);
        let in_run = plant(&run, "dead", Some(&format!("{} 1 {ns}", a_dead_pid())));

        let out = f
            .lib_driver(shell, "second", SEND)
            .env("RUNTIME_DIRECTORY", &run)
            .output()
            .expect("run the sender under a RuntimeDirectory");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{shell}: {err}");
        assert_eq!(
            Fixture::headers_under(&f.tmp),
            vec!["boss-secret-header.young".to_string()],
            "{shell}: the killed run's directory under the temp directory is gone, and the \
             young one it could not place stands: {err}"
        );
        assert!(
            err.contains(&left[0]),
            "{shell}: and it is named, by path: {err}"
        );
        assert!(
            !in_run.exists() && Fixture::headers_under(&run).is_empty(),
            "{shell}: the RuntimeDirectory is swept as before, and this run's own header \
             is gone at exit: {err}"
        );
        assert!(!err.contains(SECRET), "{shell}: never the value: {err}");
        assert_eq!(
            f.read("header.txt"),
            format!("x-boss-machine-token: {SECRET}\n"),
            "{shell}: this run's header still reached curl"
        );
        assert!(
            Path::new(f.read("path.txt").trim()).starts_with(&run),
            "{shell}: from a file in the RuntimeDirectory, not the temp directory: {}",
            f.read("path.txt")
        );
    }
}

/// A run under a RuntimeDirectory judges the temp directory by the SAME
/// rules: a live sender there — a hand run mid-request while the unit
/// ticks — keeps its header.
#[test]
fn a_run_under_a_runtime_directory_never_sweeps_a_live_sender_in_the_temp_directory() {
    for shell in ["bash", "dash"] {
        let f = Fixture::new(&format!("live-tmp-under-run-{shell}"));
        let mut first = f.spawn_until_in_flight(f.lib_driver(shell, "first", SEND), "first");
        let mine = Fixture::headers_under(&f.tmp);
        assert_eq!(mine.len(), 1, "{shell}: the live sender's directory");

        let g = Fixture::new(&format!("live-tmp-under-run-{shell}-second"));
        let run = g.root.join("run");
        create_dir(&run);
        let out = g
            .lib_driver(shell, "second", SEND)
            .env("TMPDIR", &f.tmp)
            .env("RUNTIME_DIRECTORY", &run)
            .output()
            .expect("run the sender under a RuntimeDirectory");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{shell}: {err}");
        assert!(!err.contains("removed"), "{shell}: nothing was dead: {err}");
        assert_eq!(
            Fixture::headers_under(&f.tmp),
            mine,
            "{shell}: the live sender's directory stands"
        );
        f.release();
        assert!(first.wait().expect("first sender").success());
        assert_eq!(
            f.read("header-after.txt"),
            format!("x-boss-machine-token: {SECRET}\n"),
            "{shell}: and its curl could still read the header after the second ran"
        );
    }
}

/// Every file under `dir`, at any depth, whose name ends `suffix`.
fn files_ending(dir: &Path, suffix: &str, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.filter_map(Result::ok) {
        let p = e.path();
        if p.is_dir() {
            files_ending(&p, suffix, out);
        } else if p.to_string_lossy().ends_with(suffix) {
            out.push(p);
        }
    }
}

/// The lines of a shell file or a unit that are not comments.
fn live_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .map(str::to_string)
        .collect()
}

/// The `*.sh` names a line holds, without their directories.
fn shell_names(line: &str) -> Vec<String> {
    line.split(|c: char| c.is_whitespace() || "\"'`;()".contains(c))
        .filter(|w| w.ends_with(".sh"))
        .filter_map(|w| w.rsplit('/').next())
        .filter(|n| n.len() > 3)
        .map(str::to_string)
        .collect()
}

/// Does this shell file make a header through the reader — on a live line
/// of its own, or of a tree file it sources (`.` or `source`), at any
/// depth? A name several tree files share counts if any of them does.
fn reaches_the_reader(
    script: &Path,
    by_name: &std::collections::BTreeMap<String, Vec<PathBuf>>,
    seen: &mut std::collections::BTreeSet<PathBuf>,
) -> bool {
    if !seen.insert(script.to_path_buf()) {
        return false;
    }
    let lines = live_lines(script);
    if lines.iter().any(|l| l.contains("secret-header.sh")) {
        return true;
    }
    lines
        .iter()
        .filter(|l| {
            l.starts_with(". ")
                || l.starts_with("source ")
                || [" . ", " source "].iter().any(|s| l.contains(s))
        })
        .flat_map(|l| shell_names(l))
        .filter_map(|n| by_name.get(&n))
        .flatten()
        .any(|p| reaches_the_reader(p, by_name, seen))
}

/// THE CLASS, NOT THE LIST (backlog df38075a, F1, second car). The first
/// car gave a RuntimeDirectory to the six units the item named and left
/// the rest to the sweep: thirteen more unit files start the wrap pair
/// (`boss-maintenance-wrap.sh` before, `boss-step.sh` after), and each of
/// those presents the machine token through the same reader, so each
/// made its header under the host's shared /tmp. This walks every unit
/// under infra/ and reads what its Exec lines start; a unit that starts a
/// tree script reaching the reader must give the header a directory that
/// dies with the unit. A unit added tomorrow is held without being named
/// here.
#[test]
fn every_unit_that_starts_a_sender_of_the_token_declares_a_private_runtime_directory() {
    let root = repo_root();
    let mut shells = Vec::new();
    files_ending(&root.join("infra"), ".sh", &mut shells);
    let mut by_name: std::collections::BTreeMap<String, Vec<PathBuf>> = Default::default();
    for p in &shells {
        // A lint is run by the gate, never started by a unit, and its
        // fixtures name the reader on purpose.
        if p.starts_with(root.join("infra/lint")) {
            continue;
        }
        if let Some(n) = p.file_name().and_then(|n| n.to_str()) {
            by_name.entry(n.to_string()).or_default().push(p.clone());
        }
    }
    let mut units = Vec::new();
    files_ending(&root.join("infra"), ".service", &mut units);
    units.sort();

    let mut senders: Vec<String> = Vec::new();
    for unit in &units {
        let rel = unit
            .strip_prefix(&root)
            .expect("a unit under the repo")
            .to_string_lossy()
            .into_owned();
        let lines = live_lines(unit);
        let started: Vec<String> = lines
            .iter()
            .filter(|l| l.starts_with("Exec") && l.contains('='))
            .flat_map(|l| shell_names(l))
            .collect();
        let sends =
            started.iter().any(|n| {
                by_name.get(n).into_iter().flatten().any(|p| {
                    reaches_the_reader(p, &by_name, &mut std::collections::BTreeSet::new())
                })
            });
        if !sends {
            continue;
        }
        senders.push(rel.clone());
        let dirs: Vec<&str> = lines
            .iter()
            .filter_map(|l| l.strip_prefix("RuntimeDirectory="))
            .flat_map(str::split_whitespace)
            .collect();
        assert_eq!(
            dirs.len(),
            1,
            "{rel}: starts a sender of the machine token ({started:?}) and must declare ONE \
             RuntimeDirectory= — without it the token's header file is made in the host's \
             shared /tmp and outlives a killed run (backlog df38075a); found {dirs:?}"
        );
        assert!(
            !dirs[0].contains('/') && !dirs[0].starts_with('.'),
            "{rel}: RuntimeDirectory={} must be one plain name under /run",
            dirs[0]
        );
        assert!(
            lines.iter().any(|l| l == "RuntimeDirectoryMode=0700"),
            "{rel}: the directory is the unit's account's alone (RuntimeDirectoryMode=0700)"
        );
        assert!(
            !lines
                .iter()
                .any(|l| l.starts_with("RuntimeDirectoryPreserve=")),
            "{rel}: nothing may keep the directory past the run"
        );
    }
    // The derivation still sees: the six the item named, the wrap pair's
    // units, and the runner that sources the reader itself. A walk that
    // went blind would pass every assertion above by finding no one.
    for (unit, _) in UNITS {
        assert!(
            senders.iter().any(|s| s == unit),
            "the walk no longer finds {unit} as a sender: {senders:?}"
        );
    }
    for unit in [
        "infra/boss-files-gc.service",
        "infra/forge/forge-backup.service",
        "infra/ml/boss-ml-inference-batch.service",
        "infra/ops/boss-ops-runner.service",
    ] {
        assert!(
            senders.iter().any(|s| s == unit),
            "the walk no longer finds {unit} as a sender: {senders:?}"
        );
    }
    assert!(
        senders.len() >= 24,
        "the walk found {} sender unit(s), fewer than the tree held on 2026-10-08: {senders:?}",
        senders.len()
    );
}

/// systemd removes a unit's RuntimeDirectory when THAT unit stops. Two
/// units naming one directory would each delete the other's header
/// mid-request.
#[test]
fn no_two_units_share_a_runtime_directory() {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.filter_map(Result::ok) {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().and_then(|x| x.to_str()) == Some("service") {
                out.push(p);
            }
        }
    }
    let mut units = Vec::new();
    walk(&repo_root().join("infra"), &mut units);
    let mut seen: std::collections::BTreeMap<String, PathBuf> = Default::default();
    for unit in units {
        let text = std::fs::read_to_string(&unit).unwrap_or_default();
        for name in text
            .lines()
            .filter_map(|l| l.trim().strip_prefix("RuntimeDirectory="))
            .flat_map(str::split_whitespace)
        {
            if let Some(other) = seen.insert(name.to_string(), unit.clone()) {
                panic!(
                    "RuntimeDirectory={name} is declared by both {} and {}",
                    other.display(),
                    unit.display()
                );
            }
        }
    }
    assert!(
        seen.len() >= UNITS.len(),
        "the walk found the units: {seen:?}"
    );
}

/// A system of record that accepts the connection and never answers. The
/// real curl, a real socket: `post_observation` ends by its own bound
/// and says which, instead of holding the tick and its header for as
/// long as the peer likes.
#[test]
fn a_post_to_a_record_that_never_answers_ends_by_its_own_bound() {
    let real_curl = ["/usr/bin/curl", "/bin/curl", "/usr/local/bin/curl"]
        .iter()
        .find(|p| Path::new(p).exists())
        .expect("this suite needs the real curl, as the_chore_runs_through_a_sor_that_answers_badly does");
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    // Accept and hold: the peer is up, and silent.
    std::thread::spawn(move || {
        let mut held = Vec::new();
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let until = Instant::now() + Duration::from_secs(25);
        while Instant::now() < until {
            if let Ok((s, _)) = listener.accept() {
                held.push(s);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    });
    for shell in ["bash", "dash"] {
        let f = Fixture::new(&format!("bound-{shell}"));
        let driver = f.root.join("observe.sh");
        write_file(
            &driver,
            &format!(
                ". \"{}\"\npost_observation '{{\"observed_at\":\"2026-10-07T00:00:00Z\"}}'\necho \"rc=$?\"\n",
                repo_root().join(OBSERVE_LIB).display()
            ),
        );
        let started = Instant::now();
        let out = Command::new(shell)
            .arg(&driver)
            .current_dir(&f.root)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    Path::new(real_curl)
                        .parent()
                        .expect("curl's directory")
                        .display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("JOBS_API", format!("http://127.0.0.1:{port}"))
            .env("OBSERVE_POST_MAX_TIME", "2")
            .env("TMPDIR", &f.tmp)
            .env("SPOOL_DIR", f.root.join("spool"))
            .env("BOSS_MACHINE_TOKEN_DIR", f.root.join("no-token-dir"))
            .env_remove("RUNTIME_DIRECTORY")
            .stdin(Stdio::null())
            .output()
            .expect("run the observer driver");
        let took = started.elapsed();
        let said = String::from_utf8_lossy(&out.stdout);
        assert!(
            took < Duration::from_secs(12),
            "{shell}: the post must end by its bound (2 s here), took {took:?}: {said}"
        );
        assert!(
            said.contains("jobs api: curl failed (exit 28,") && said.contains("rc=1"),
            "{shell}: and say it timed out (curl exit 28), returning 1 so the reading is spooled: {said}"
        );
    }
}

/// The bound a unit actually runs with: a whole number of seconds, small
/// against the fifteen-minute cadence, and a value that is not a number
/// falls back to it rather than reaching curl.
#[test]
fn the_posts_default_bound_is_a_number_and_a_bad_override_does_not_reach_curl() {
    let rows = [
        (None, "60"),
        (Some("soon"), "60"),
        (Some(""), "60"),
        // curl reads 0 as NO bound.
        (Some("0"), "60"),
        (Some("7"), "7"),
    ];
    for (i, (given, want)) in rows.into_iter().enumerate() {
        let f = Fixture::new(&format!("bound-argv-{i}"));
        write_exec(
            &f.bin.join("curl"),
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$STUB_DIR/argv.txt\"\nprintf '\\n202'\n",
        );
        let driver = f.root.join("observe.sh");
        write_file(
            &driver,
            &format!(
                ". \"{}\"\npost_observation '{{}}'\n",
                repo_root().join(OBSERVE_LIB).display()
            ),
        );
        let mut cmd = f.command(Path::new("dash"));
        cmd.arg(&driver)
            .env("JOBS_API", "http://sor.test:7900")
            .env_remove("OBSERVE_POST_MAX_TIME");
        if let Some(v) = given {
            cmd.env("OBSERVE_POST_MAX_TIME", v);
        }
        let out = cmd.output().expect("run the observer driver");
        assert!(
            out.status.success(),
            "{given:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let argv: Vec<String> = f.read("argv.txt").lines().map(str::to_string).collect();
        let bound = argv
            .windows(2)
            .find(|w| w[0] == "--max-time")
            .map(|w| w[1].clone());
        assert_eq!(
            bound.as_deref(),
            Some(want),
            "{given:?}: curl's --max-time: {argv:?}"
        );
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "--connect-timeout" && w[1].parse::<u32>().is_ok()),
            "{given:?}: and a connect bound: {argv:?}"
        );
    }
}
