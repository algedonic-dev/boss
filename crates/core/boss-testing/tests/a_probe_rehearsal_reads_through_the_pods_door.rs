//! `infra/dev/probe-rehearsal/boss-sor-read` — the dev pod's copy of a
//! recorded probe's reader, for REHEARSING a probe before it is recorded.
//! It takes exactly what the forge's reader takes and refuses exactly
//! what it refuses, and it reads through the pod's door, `boss-api`,
//! which presents the container's machine token. It never sends a
//! request of its own.
//!
//! MEASURED (backlog 37742794 R3, part of urgent 2710c8fc; builder run
//! f76adc85, 2026-10-09T18:35Z against origin/main 8ba37163075f). The
//! machine gate's 24 h window held six would-refuse facts signed
//! `automation:run-car-probe-reader` from the dev pod (10.244.3.189),
//! each presenting no token. Three are a builder rehearsing a probe by
//! hand: run 0c777a6b's measure.sh exported `BOSS_SOR_USER` as the
//! probe reader and ran the forge's `infra/forge/probe-bin/boss-sor-read`
//! from its worktree (16:49:44Z GET /api/jobs, 16:49:46Z GET
//! /api/jobs/{id}, 16:58:38Z GET /api/delivery/policy/{name} from its
//! probe.sh). The forge's reader sends no machine token by design — it
//! runs car-supplied text, and on the forge its door is a reader
//! credential behind a socket (design b35c22b4) — and its own refusal
//! told a hand runner to "export the read-scoped actor yourself". Each
//! such read keeps the gate's clean window dirty now, and is a 401 the
//! day it enforces, which a rehearsal then reads as the CAR's failure.
//! (Two more are `boss prove`'s hand door on the pod, 05:53:31Z, which
//! is a decision on the item's ledger and not this file's.)
//!
//! So the pod gets the reader a builder should have reached for: the
//! same contract, read through the door every builder already uses.
//! FIDELITY is the point of a rehearsal, so the refusals are not
//! re-derived here — they are run against the forge's own reader, side
//! by side, and must agree (CLAUDE.md §9a): the argv, the URL, the list
//! read asked for whole (backlog 9b473d4a), and a path whose service the
//! forge's port table does not carry. That last one matters most:
//! `boss-api` reaches the read-only services the forge cannot (assets,
//! customers, …, backlog 9a440539), so a rehearsal that simply handed
//! the path over would pass on the pod a probe the forge refuses.
//!
//! REWORK (review 9edcfa36, 2026-10-09, B1 measured): on a non-2xx
//! answer the first cut exec'd the door, which prints the error body on
//! stdout and exits 1, where the forge's `curl -fsS` prints nothing and
//! exits 22. An absence probe (`jq '.items | length'`, and `null |
//! length` is 0) then PASSED the rehearsal and read not-yet on the
//! forge. Both readers now face one real loopback answer, through the
//! real curl and the tree's real `boss-api`, and must print the same.

use boss_testing::{repo_root, without_proxy};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;

const REHEARSAL: &str = "infra/dev/probe-rehearsal/boss-sor-read";
const FORGE: &str = "infra/forge/probe-bin/boss-sor-read";
const DOOR: &str = "infra/dev/boss-api";
const RULES: &str = "infra/platform/documents/builder-rules.md";

/// A read-scoped actor, as the forge's door hands it to its reader.
const ACTOR: &str = "{\"id\":\"automation:run-car-probe-reader\",\"role\":\"audit-readonly\"}";

/// The variables the stub door reports, as `NAME=<value or unset>`
/// lines after its argv: the ones a caller's shell could carry into the
/// door and move a rehearsal's read somewhere the forge's never goes.
const DOOR_ENV: [&str; 5] = [
    "BOSS_SOR_SERVICE",
    "BOSS_JOBS_URL",
    "BOSS_SOR_READ_PORTS",
    "BOSS_SOR_READ_URL",
    "BOSS_SOR_PORTS",
];

struct Ran {
    rc: i32,
    stdout: String,
    stderr: String,
    /// What the stub `boss-api` was handed, one argument per line, then
    /// one `NAME=<value or unset>` line per `DOOR_ENV` entry. Empty when
    /// it was never run.
    door: String,
    /// What the stub `curl` was handed, one argument per line. Empty
    /// when it was never run.
    curl: String,
}

/// Run `script` with a stub `boss-api` and a stub `curl` first on PATH,
/// each recording what it was handed and printing a body.
fn run(case: &str, script: &str, args: &[&str], env: &[(&str, &str)]) -> Ran {
    let dir = boss_testing::scratch_dir(&format!("probe-rehearsal-{case}"));
    let bin = dir.join("bin");
    boss_testing::create_dir(&bin);
    let door = dir.join("door-argv");
    let curl = dir.join("curl-argv");
    let report_env: String = DOOR_ENV
        .iter()
        .map(|v| format!(" printf '{v}=%s\\n' \"${{{v}-unset}}\";"))
        .collect();
    boss_testing::write_exec(
        &bin.join("boss-api"),
        &format!(
            "#!/usr/bin/env bash\n{{ for a in \"$@\"; do printf '%s\\n' \"$a\"; done;{report_env} }} > '{}'\n\
             printf 'DOOR_BODY\\n'\nprintf 'HTTP:200\\n' >&2\n",
            door.display()
        ),
    );
    boss_testing::write_exec(
        &bin.join("curl"),
        &format!(
            "#!/usr/bin/env bash\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done > '{}'\n\
             printf 'CURL_BODY\\n'\n",
            curl.display()
        ),
    );
    let mut cmd = Command::new(repo_root().join(script));
    without_proxy(&mut cmd);
    cmd.args(args)
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env_remove("BOSS_SOR_USER")
        .env_remove("BOSS_SOR_DOOR")
        .current_dir(&dir);
    for v in DOOR_ENV {
        cmd.env_remove(v);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("the reader runs");
    let read = |p: &Path| std::fs::read_to_string(p).unwrap_or_default();
    Ran {
        rc: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
        door: read(&door),
        curl: read(&curl),
    }
}

/// The forge's reader, as its door would run it on the LAN path.
fn forge(case: &str, args: &[&str], table: Option<&str>) -> Ran {
    let mut env = vec![
        ("BOSS_JOBS_URL", "http://sor.invalid:7900"),
        ("BOSS_SOR_USER", ACTOR),
    ];
    if let Some(t) = table {
        env.push(("BOSS_SOR_PORTS", t));
    }
    run(&format!("forge-{case}"), FORGE, args, &env)
}

/// The rehearsal reader, in a builder's shell.
fn rehearsal(case: &str, args: &[&str], table: Option<&str>) -> Ran {
    let mut env = vec![];
    if let Some(t) = table {
        env.push(("BOSS_SOR_PORTS", t));
    }
    run(&format!("pod-{case}"), REHEARSAL, args, &env)
}

/// The door's argv lines (without the trailing env lines).
fn door_args(ran: &Ran) -> Vec<String> {
    ran.door
        .lines()
        .filter(|l| !DOOR_ENV.iter().any(|v| l.starts_with(&format!("{v}="))))
        .map(str::to_string)
        .collect()
}

/// What the door saw `name` set to, or `unset`.
fn door_env(ran: &Ran, name: &str) -> String {
    let prefix = format!("{name}=");
    ran.door
        .lines()
        .find_map(|l| l.strip_prefix(&prefix).map(str::to_string))
        .unwrap_or_else(|| panic!("the stub door never reported {name}: {}", ran.door))
}

/// The forge's port table, read out of the tree the way the forge's
/// door hands it to its reader: the file's entries, one space apart.
fn tree_table() -> String {
    std::fs::read_to_string(repo_root().join("infra/forge/sor-ports.env"))
        .expect("the forge's port table")
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The path the forge's reader asked curl for: its last argument, less
/// the base.
fn forge_path(ran: &Ran) -> String {
    let url = ran.curl.lines().last().unwrap_or_default();
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.find('/').map_or("", |i| &rest[i..]).to_string()
}

#[test]
fn the_rehearsal_reader_ships_executable_beside_the_pods_doors() {
    let p: PathBuf = repo_root().join(REHEARSAL);
    assert!(p.is_file(), "{} does not exist", p.display());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&p)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o111,
            0o111,
            "{} is not executable (mode {mode:o}) — a rehearsal would answer `permission \
             denied`, which reads as the probe failing",
            p.display()
        );
    }
}

/// THE READ GOES THROUGH THE DOOR. One `boss-api GET <path>`, nothing
/// else: no curl of its own, so no request leaves this script without
/// the door's token (the door signs as the session's own actor). The
/// body is the door's, on stdout, where a probe reads it.
#[test]
fn the_rehearsal_reader_reads_through_the_pods_door_and_sends_nothing_itself() {
    let ran = rehearsal("through-the-door", &["/api/yard/status"], None);
    assert_eq!(ran.rc, 0, "stderr: {}", ran.stderr);
    assert!(ran.stdout.contains("DOOR_BODY"), "stdout: {}", ran.stdout);
    assert_eq!(door_args(&ran), ["GET", "/api/yard/status"], "{}", ran.door);
    assert!(
        ran.curl.is_empty(),
        "the rehearsal reader sent a request of its own, without the door's token:\n{}",
        ran.curl
    );
}

/// A PROVE DOOR'S ENVIRONMENT IS REFUSED (review 9edcfa36 N2).
/// `BOSS_SOR_USER` and `BOSS_SOR_DOOR` are what a `boss prove` door
/// hands the reader it runs. A hand `boss prove` run outside a checkout
/// inherits the caller's PATH, so a shell that had put this directory
/// on PATH would have recorded a PROOF read through the session's own
/// token. Seeing either, this reader stops before the door is asked
/// anything, and names the variable — which is also what a builder
/// who exported the reader's identity by hand, as the forge's old
/// refusal told them to, needs to hear.
#[test]
fn the_rehearsal_reader_refuses_a_prove_doors_environment() {
    for (var, value) in [
        ("BOSS_SOR_USER", ACTOR),
        ("BOSS_SOR_DOOR", "/run/boss-probe/door.sock"),
    ] {
        let ran = run(
            &format!("prove-env-{var}"),
            REHEARSAL,
            &["/api/yard/status"],
            &[(var, value)],
        );
        assert_eq!(ran.rc, 2, "{var}: stderr: {}", ran.stderr);
        assert!(ran.door.is_empty(), "{var}: reached the door: {}", ran.door);
        assert!(ran.curl.is_empty(), "{var}: reached curl: {}", ran.curl);
        assert!(
            ran.stderr.contains(var),
            "{var}: the refusal must name the variable: {}",
            ran.stderr
        );
    }
}

/// THE DOOR IS HANDED THE FORGE'S WORLD, NOT THE CALLER'S (review
/// 9edcfa36 N5). The door honours a base URL, a read-only port table
/// and its host from the environment; the forge's reader takes its base
/// from its door and has no read-only rows at all. So none of them
/// reaches the door from a builder's shell, and the port table the door
/// routes by is the one this reader judged the path with — the tree's.
#[test]
fn the_rehearsal_reader_hands_the_door_no_override_from_the_callers_shell() {
    let ran = run(
        "no-overrides",
        REHEARSAL,
        &["/api/people?limit=1"],
        &[
            ("BOSS_JOBS_URL", "http://elsewhere.invalid:7900"),
            ("BOSS_SOR_READ_PORTS", "assets=7700"),
            ("BOSS_SOR_READ_URL", "http://elsewhere.invalid"),
            ("BOSS_SOR_SERVICE", "gateway"),
        ],
    );
    assert_eq!(ran.rc, 0, "stderr: {}", ran.stderr);
    for v in [
        "BOSS_JOBS_URL",
        "BOSS_SOR_READ_PORTS",
        "BOSS_SOR_READ_URL",
        "BOSS_SOR_SERVICE",
    ] {
        assert_eq!(
            door_env(&ran, v),
            "unset",
            "{v} reached the door from the caller's shell: {}",
            ran.door
        );
    }
    assert_eq!(
        door_env(&ran, "BOSS_SOR_PORTS"),
        tree_table(),
        "the door must route by the table the path was judged with"
    );
}

/// The forge's port table is the TREE's, never a caller's (review
/// 9edcfa36 N5): the forge's door always hands its reader the file's
/// table, so a stale `BOSS_SOR_PORTS` wider than the file would let a
/// rehearsal pass a path the forge refuses. One that says what the file
/// says is accepted; one that says anything else is refused, naming the
/// variable, before the door.
#[test]
fn a_port_table_in_the_callers_shell_that_is_not_the_trees_is_refused() {
    let tree = tree_table();
    let same = rehearsal("table-same", &["/api/people?limit=1"], Some(&tree));
    assert_eq!(same.rc, 0, "the tree's own table: {}", same.stderr);
    assert_eq!(door_args(&same), ["GET", "/api/people?limit=1"]);

    let wider = format!("{tree} assets=7700");
    for (case, table) in [
        ("table-wider", wider.as_str()),
        ("table-short", "jobs=7900"),
    ] {
        let r = rehearsal(case, &["/api/jobs?kind=x"], Some(table));
        assert_eq!(r.rc, 2, "{case}: stderr: {}", r.stderr);
        assert!(r.door.is_empty(), "{case}: reached the door: {}", r.door);
        assert!(
            r.stderr.contains("BOSS_SOR_PORTS"),
            "{case}: the refusal must name the variable: {}",
            r.stderr
        );
    }
}

/// A loopback system of record: every connection is answered once and
/// closed. A path containing `/present` gets 200 and a list; anything
/// else gets 403 with a JSON error body — the shape that made B1 a
/// false pass, since `jq '.items | length'` reads that body's null as 0.
fn serve_loopback() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().expect("clone"));
            let mut request = String::new();
            let _ = reader.read_line(&mut request);
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) if line == "\r\n" || line == "\n" => break,
                    Ok(_) => {}
                }
            }
            let (status, body) = if request.contains("/present") {
                ("200 OK", "{\"items\":[{\"id\":\"a\"}],\"total\":1}")
            } else {
                (
                    "403 Forbidden",
                    "{\"error\":\"forbidden: no active rule\",\"path\":\"/api/jobs\"}",
                )
            };
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    port
}

/// BOTH READERS FACE ONE REAL ANSWER (review 9edcfa36 B1, measured).
/// The real curl, the forge's reader as its door runs it, and the
/// rehearsal reader through the tree's REAL `boss-api` — with a `curl`
/// first on PATH that only re-aims each URL at a loopback server, so
/// nothing leaves this host and the door's own base (its `sor-url`) is
/// what it would be on the pod. No machine token is in reach. On a 403
/// carrying a JSON body the forge's `curl -fsS` prints NOTHING and
/// exits 22; the rehearsal must print nothing too, and exit non-zero,
/// or an absence probe passes on the pod and reads not-yet on the
/// forge. On a 200 both print the body.
#[test]
fn on_a_refused_read_both_readers_print_nothing_and_on_an_answer_the_same_body() {
    let real_curl = ["/usr/bin/curl", "/bin/curl", "/usr/local/bin/curl"]
        .iter()
        .find(|p| Path::new(p).exists())
        .expect("this test needs the real curl, as a_killed_sender_leaves_no_header_behind does");
    let port = serve_loopback();
    let dir = boss_testing::scratch_dir("probe-rehearsal-real-door");
    let bin = dir.join("bin");
    boss_testing::create_dir(&bin);
    let home = dir.join("home");
    boss_testing::create_dir(&home);
    // The real curl, its URL re-aimed at the loopback server. Every
    // other argument is handed over untouched, so `-f`, `-w` and the
    // door's own handling of the answer are the real ones.
    boss_testing::write_exec(
        &bin.join("curl"),
        &format!(
            "#!/usr/bin/env bash\nargs=()\nfor a in \"$@\"; do\n  case \"$a\" in\n    \
             http://*|https://*) rest=${{a#*://}}; case \"$rest\" in */*) a=\"http://127.0.0.1:{port}/${{rest#*/}}\" ;; \
             *) a=\"http://127.0.0.1:{port}/\" ;; esac ;;\n  esac\n  args+=(\"$a\")\ndone\nexec {real_curl} \"${{args[@]}}\"\n"
        ),
    );
    std::os::unix::fs::symlink(repo_root().join(DOOR), bin.join("boss-api"))
        .expect("link the door");
    let read = |script: &str, path: &str, env: &[(&str, &str)]| -> Ran {
        let mut cmd = Command::new(repo_root().join(script));
        without_proxy(&mut cmd);
        cmd.arg(path)
            .current_dir(&dir)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("HOME", &home)
            // No token in reach: neither the pod's doors' directory
            // nor /etc/boss/machine-token is ever read by this test.
            .env("BOSS_MACHINE_TOKEN_DIR", dir.join("no-such-token-dir"))
            .env("BOSS_ACTOR", "test-rehearsal")
            .env_remove("BOSS_ACTOR_FILE")
            .env_remove("BOSS_MACHINE_TOKEN_HOSTS")
            .env("BOSS_DOOR_FRESHNESS", "off")
            .env("BOSS_SOR_WAIT_SECONDS", "0")
            .env_remove("BOSS_SOR_USER")
            .env_remove("BOSS_SOR_DOOR");
        for v in DOOR_ENV {
            cmd.env_remove(v);
        }
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("the reader runs");
        Ran {
            rc: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).to_string(),
            door: String::new(),
            curl: String::new(),
        }
    };
    let base = format!("http://127.0.0.1:{port}");
    let forge_env = [("BOSS_JOBS_URL", base.as_str()), ("BOSS_SOR_USER", ACTOR)];

    let f = read(FORGE, "/api/jobs?kind=x&status=open", &forge_env);
    assert_eq!(f.rc, 22, "the forge's reader on a 403: {}", f.stderr);
    assert_eq!(f.stdout, "", "the forge's reader printed a refused body");
    let r = read(REHEARSAL, "/api/jobs?kind=x&status=open", &[]);
    assert_ne!(r.rc, 0, "the rehearsal passed a refused read: {}", r.stderr);
    assert_eq!(
        r.stdout, "",
        "the rehearsal printed the door's error body, which an absence probe reads as \
         an empty list; the forge's reader prints nothing (stderr: {})",
        r.stderr
    );
    assert_eq!(r.rc, f.rc, "a non-2xx exits as curl -f does");

    let f = read(FORGE, "/api/jobs/present", &forge_env);
    assert_eq!(f.rc, 0, "the forge's reader on a 200: {}", f.stderr);
    let r = read(REHEARSAL, "/api/jobs/present", &[]);
    assert_eq!(r.rc, 0, "the rehearsal on a 200: {}", r.stderr);
    assert_eq!(
        r.stdout.trim_end(),
        f.stdout.trim_end(),
        "the two readers print different bodies for one answer"
    );
}

/// The pod's door routes a path to a service by name when
/// `BOSS_SOR_SERVICE` is set; a probe never names one, and the forge's
/// reader ignores it. A builder's shell that carried it must not move a
/// rehearsal's read to another port.
#[test]
fn the_rehearsal_reader_never_hands_the_door_a_service_name() {
    let env = [("BOSS_SOR_SERVICE", "gateway")];
    let ran = run("no-service-name", REHEARSAL, &["/api/yard/status"], &env);
    assert_eq!(ran.rc, 0, "stderr: {}", ran.stderr);
    assert_eq!(
        door_env(&ran, "BOSS_SOR_SERVICE"),
        "unset",
        "the door was handed a service name the probe never chose: {}",
        ran.door
    );
}

/// WHAT THE FORGE REFUSES, THE REHEARSAL REFUSES — run against the
/// forge's reader, not against a list kept here. Each is refused with
/// exit 2 before anything is sent, and says why.
#[test]
fn the_rehearsal_reader_refuses_what_the_forge_reader_refuses() {
    for (case, args) in [
        ("no-args", vec![]),
        ("two-args", vec!["/api/jobs", "/api/yard/status"]),
        ("method", vec!["PUT", "/api/jobs"]),
        ("url", vec!["http://127.0.0.1:7900/api/jobs"]),
        ("relative", vec!["api/jobs"]),
    ] {
        let f = forge(case, &args, None);
        assert_eq!(f.rc, 2, "{case}: the forge's reader no longer refuses this");
        let r = rehearsal(case, &args, None);
        assert_eq!(
            r.rc, 2,
            "{case}: the rehearsal accepted what the forge refuses"
        );
        assert!(r.door.is_empty(), "{case}: reached the door: {}", r.door);
        assert!(r.curl.is_empty(), "{case}: reached curl: {}", r.curl);
        assert!(!r.stderr.is_empty(), "{case}: refused without saying why");
    }
}

/// THE LIST IS ASKED FOR WHOLE, EXACTLY AS THE FORGE ASKS (backlog
/// 9b473d4a). A probe recorded against `full=true` judges an absent key
/// on a slim read, so a rehearsal that read slim would rehearse a
/// different claim. Held equal to the forge's reader path by path.
#[test]
fn a_list_read_is_asked_for_exactly_as_the_forge_reader_asks_for_it() {
    for (case, path) in [
        (
            "list-query",
            "/api/jobs?kind=ops-request&status=closed&limit=50",
        ),
        ("list-bare", "/api/jobs"),
        ("list-says-slim", "/api/jobs?kind=x&full=false"),
        ("list-says-full-first", "/api/jobs?full=true&kind=x"),
        ("one-packet", "/api/jobs/abc"),
        ("summary", "/api/jobs/summary?kind=x"),
        ("lookalike", "/api/jobsx?kind=x"),
    ] {
        let f = forge(case, &[path], None);
        assert_eq!(f.rc, 0, "{case}: {}", f.stderr);
        let r = rehearsal(case, &[path], None);
        assert_eq!(r.rc, 0, "{case}: {}", r.stderr);
        assert_eq!(
            door_args(&r),
            ["GET".to_string(), forge_path(&f)],
            "{case}: the rehearsal asks for a different read than the forge does"
        );
    }
}

/// A PATH THE FORGE CANNOT ROUTE IS REFUSED HERE TOO. The forge's reader
/// refuses a path whose service its port table does not carry; the
/// pod's door would answer it, through the read-only rows. The
/// rehearsal judges with the forge's own table read out of the tree,
/// which is the table the recorded probe will be routed by, and names
/// the service it refused.
#[test]
fn a_path_the_forge_cannot_route_is_refused_by_the_rehearsal_too() {
    let short = "jobs=7900 people=7500";
    let f = forge("short-table", &["/api/events/tail"], Some(short));
    assert_eq!(
        f.rc, 2,
        "the forge's reader no longer refuses an unrouted path"
    );

    // The tree's own table: what the forge's door hands its reader.
    let table = tree_table();
    let r = rehearsal("tree-customers-named", &["/api/customers"], None);
    assert_eq!(r.rc, 2, "stderr: {}", r.stderr);
    assert!(
        r.stderr.contains("customers"),
        "the refusal must name the service: {}",
        r.stderr
    );
    for (case, path, forge_reads) in [
        ("tree-assets", "/api/assets?limit=1", false),
        ("tree-customers", "/api/customers", false),
        ("tree-people", "/api/people?limit=1", true),
        ("tree-events", "/api/events/tail?limit=1", true),
    ] {
        let f = forge(case, &[path], Some(&table));
        assert_eq!(f.rc == 0, forge_reads, "{case}: forge: {}", f.stderr);
        let r = rehearsal(case, &[path], None);
        assert_eq!(
            r.rc == 0,
            forge_reads,
            "{case}: the rehearsal and the forge disagree about {path}: {}",
            r.stderr
        );
        if forge_reads {
            assert_eq!(door_args(&r), ["GET", path], "{case}");
        } else {
            assert_eq!(r.rc, 2, "{case}: {}", r.stderr);
            assert!(r.door.is_empty(), "{case}: reached the door: {}", r.door);
        }
    }
}

/// THE RULE AND THE REFUSAL SEND A BUILDER HERE. The forge's reader is
/// the one a builder finds in the tree, and its refusal for a missing
/// identity told a hand runner to export the reader's actor — which is
/// exactly the tokenless read measured above. It now names this reader
/// instead, and rule 7's rehearsal sentence says to put its directory
/// first on PATH.
#[test]
fn the_forge_readers_refusal_and_the_builder_rules_name_the_rehearsal_reader() {
    let ran = run(
        "forge-unidentified",
        FORGE,
        &["/api/yard/status"],
        &[("BOSS_JOBS_URL", "http://sor.invalid:7900")],
    );
    assert_eq!(ran.rc, 2, "stderr: {}", ran.stderr);
    assert!(ran.curl.is_empty(), "curl was reached: {}", ran.curl);
    assert!(
        ran.stderr.contains("BOSS_SOR_USER"),
        "the refusal still names what is missing: {}",
        ran.stderr
    );
    assert!(
        ran.stderr.contains("infra/dev/probe-rehearsal"),
        "the refusal must send a hand runner on the pod to the rehearsal reader: {}",
        ran.stderr
    );
    assert!(
        !ran.stderr.contains("export the read-scoped actor yourself"),
        "the refusal still tells a hand runner to make the tokenless read: {}",
        ran.stderr
    );

    let rules = std::fs::read_to_string(repo_root().join(RULES)).expect("the builder rules");
    let rule7 = rules
        .split("\n7. ")
        .nth(1)
        .and_then(|r| r.split("\n8. ").next())
        .expect("rule 7");
    assert!(
        rule7.contains("infra/dev/probe-rehearsal"),
        "rule 7's rehearsal must name the pod's rehearsal reader"
    );
    assert!(
        rule7.contains("BOSS_SOR_USER"),
        "rule 7 must say not to export the reader's identity by hand"
    );
    // The two rehearsal legs stay one sentence (review 9edcfa36 N4): the
    // origin/main leg follows the branch leg directly, and the reader's
    // instruction comes after both, so the leg that catches a probe
    // passing on main is not read as part of a prohibition.
    let main_leg = rule7
        .find("ff56cc00) — AND against origin/main")
        .expect("rule 7's origin/main leg follows its branch leg directly");
    let reader = rule7
        .find("infra/dev/probe-rehearsal")
        .expect("rule 7 names the rehearsal reader");
    assert!(
        main_leg < reader,
        "rule 7's rehearsal-reader text sits inside the two rehearsal legs"
    );
}
