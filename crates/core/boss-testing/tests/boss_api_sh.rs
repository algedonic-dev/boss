//! `infra/dev/boss-api` — the jobs-API door CLAUDE.md §Doors names:
//! `boss-api METHOD /api/path [body.json]`, pinned to the system of
//! record, signed as the session's actor, invoked bare so it stays
//! inside a permission allowlist. Body to stdout, `HTTP:<code>` to
//! stderr, non-2xx exits 1 so a driver fails loudly.
//!
//! Until 2026-09-14 this script was pod-local text under
//! /work/tools/bin (backlog 0d8d7a90): a fresh pod, a second builder
//! host, or a builder pod would not have had it, and the base URL and
//! actor-header logic were unversioned. The pod file is the spec —
//! these tests pin its behaviour against a stub `curl` on PATH so
//! nothing here reaches a network:
//!
//!   * a GET prints the body and the `HTTP:` line, and asks curl for
//!     `$BOSS_JOBS_URL$path` with the method;
//!   * a write method sends `--data-binary @<body file>`, and a non-2xx
//!     answer exits 1 with the code still on stderr;
//!   * the `X-Boss-User` header carries `BOSS_ACTOR`, else the one line
//!     in `$HOME/.config/boss/actor`; with neither, a READ goes out
//!     marked `operator:unidentified` and a WRITE is refused before
//!     curl runs, naming both fixes — the CLI's rule
//!     (crates/orchestrators/boss-cli/src/identity.rs, backlog
//!     5083d6f5). Until 2026-09-14 the script signed an unnamed write
//!     as the pod copy's fixed id instead (backlog 416d503c);
//!   * the default base URL is the one line in `infra/dev/sor-url`,
//!     read from beside the script, so the system-of-record address is
//!     spelled once in infra/dev (CLAUDE.md §9a);
//!   * the machine token rides as `X-Boss-Machine-Token` only when the
//!     `current` slot of its mounted directory holds one — the directory
//!     boss-core reads, not the FILE this door read until backlog
//!     1876bbdb INFO-6 (the stub records the header NAME, never a value),
//!     and in a 0600 header file handed over as `-H @file`, never in
//!     curl's argv (backlog 5f3ad356, `infra/lib/secret-header.sh`);
//!   * a bad method or a missing path is refused with exit 2 before
//!     curl runs;
//!   * the PORT follows the path (backlog de0989d2, 2026-09-17): the
//!     system of record is several services on one LAN IP, and until
//!     this the door sent every path to the jobs port, so
//!     `POST /api/people/accounts` — the first real step of the first
//!     real sponsorship loop — could not be done through any door. The
//!     rules are the ONE route function both doors source
//!     (`infra/forge/probe-bin/sor-routes.sh`); the `name=port` table is
//!     `BOSS_SOR_PORTS`, else `infra/forge/sor-ports.env` read from
//!     beside the script; absent both, nothing is routed. A table that
//!     lacks the routed service is refused before curl;
//!   * `BOSS_SOR_SERVICE` names the service when no path can route to
//!     it (backlog bf1f5ad2, 2026-09-19) — the gateway fronts every
//!     path, so it is reached by name, the way the forge's
//!     `boss-gateway-read` reaches it. Without this a builder
//!     rehearsing a gateway probe hand-set the host and the port table
//!     and was not using the door at all;
//!   * a rollout is waited out (backlog 034002b3, 2026-09-23): a curl
//!     that never got its request out (exit 6 or 7) is re-sent for up
//!     to `BOSS_SOR_WAIT_SECONDS` (120) with a visible line per wait,
//!     then fails naming the elapsed time; every other exit and every
//!     HTTP status is surfaced on the first attempt.

use boss_testing::{create_dir, repo_root, scratch_dir, write_exec, write_file};
use std::path::{Path, PathBuf};
use std::process::Command;

const SCRIPT: &str = "infra/dev/boss-api";

/// The fixed actor id the pod copy used to sign an unnamed call with.
/// A refusal names it, so a reader can tell "nobody is named" apart
/// from a policy denial — the same reason the CLI's refusal names the
/// conductor.
const FORMER_DEFAULT_ACTOR: &str = "claude@algedonic.dev";

/// The id an unnamed READ carries — `identity::UNIDENTIFIED` in the
/// CLI. Not an `automation:` slug: an unidentified operator is not a
/// process, and the server's automation branch must not read it as one.
const UNIDENTIFIED: &str = "operator:unidentified";

/// The one-line file beside the script that spells the system of record.
const SOR_URL_FILE: &str = "infra/dev/sor-url";

struct Fixture {
    root: PathBuf,
    bin: PathBuf,
    /// A HOME the test owns, so the actor-file leg never reads the
    /// real `~/.config/boss/actor`.
    home: PathBuf,
    /// Written by the `curl` stub: one argv element per line, with the
    /// machine-token header's VALUE replaced by `<present>`.
    argv: PathBuf,
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("boss-api-{name}"));
        let bin = root.join("bin");
        create_dir(&bin);
        let home = root.join("home");
        create_dir(&home);
        let argv = root.join("curl-argv.txt");
        // curl: record what the script asked for, then answer the way
        // the real one does under `-w '\n%{http_code}'` — the body,
        // a newline, the code. STUB_BODY / STUB_CODE choose the answer.
        // A header whose name is the machine token is recorded by name
        // only: the test asserts the header is PRESENT, never its value.
        // A `-H @file` is read the way curl reads it — the file's line is
        // what the header list records — and the argv exactly as handed
        // over goes to `<argv>.raw`, which is where a token in the
        // command line would show (backlog 5f3ad356).
        write_exec(
            &bin.join("curl"),
            "#!/usr/bin/env bash\n\
             : > \"$STUB_ARGV\"\n\
             : > \"$STUB_ARGV.raw\"\n\
             prev=\n\
             for a in \"$@\"; do\n\
                 printf '%s\\n' \"$a\" >> \"$STUB_ARGV.raw\"\n\
                 h=$a\n\
                 if [ \"$prev\" = -H ]; then case \"$a\" in @*) h=$(cat \"${a#@}\") ;; esac; fi\n\
                 case \"$h\" in\n\
                     [Xx]-[Bb]oss-[Mm]achine-[Tt]oken:*) echo 'X-Boss-Machine-Token: <present>' ;;\n\
                     *) printf '%s\\n' \"$h\" ;;\n\
                 esac >> \"$STUB_ARGV\"\n\
                 prev=$a\n\
             done\n\
             printf '%s\\n%s' \"${STUB_BODY:-}\" \"${STUB_CODE:-200}\"\n",
        );
        Self {
            root,
            bin,
            home,
            argv,
        }
    }

    /// The tree's script, with `BOSS_JOBS_URL` pinned to a test address.
    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> Run {
        let mut cmd = self.command(&repo_root().join(SCRIPT));
        cmd.env("BOSS_JOBS_URL", "http://sor.test:7900");
        Self::finish(cmd, args, env)
    }

    /// A `Command` for `script` with the fixture's isolation — the stub
    /// PATH, an owned HOME, no token file, no actor from the caller's
    /// shell — and NO `BOSS_JOBS_URL`, so a test can watch the default.
    fn command(&self, script: &Path) -> Command {
        let mut cmd = Command::new(script);
        cmd.current_dir(&self.root)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("HOME", &self.home)
            .env("STUB_ARGV", &self.argv)
            .env_remove("BOSS_JOBS_URL")
            // A token directory that does not exist, so neither the
            // pod's doors' copy (infra/dev/machine-token-dir) nor
            // /etc/boss/machine-token is ever read by a test.
            .env(
                "BOSS_MACHINE_TOKEN_DIR",
                self.root.join("no-such-token-dir"),
            )
            .env_remove("BOSS_ACTOR")
            .env_remove("BOSS_ACTOR_FILE")
            // Nor the machine token's host list (backlog 2ee29275).
            .env_remove("BOSS_MACHINE_TOKEN_HOSTS")
            // The port table comes from the tree's file unless a test
            // sets it — never from the caller's shell. Nor does the
            // named-service override leak in from the shell that ran
            // the suite.
            .env_remove("BOSS_SOR_PORTS")
            .env_remove("BOSS_SOR_SERVICE")
            // Nor the read-only rows or their host (backlog 9a440539).
            .env_remove("BOSS_SOR_READ_PORTS")
            .env_remove("BOSS_SOR_READ_URL")
            // Nor the roll wait's window (backlog 034002b3).
            .env_remove("BOSS_SOR_WAIT_SECONDS")
            .env_remove("STUB_BODY")
            .env_remove("STUB_CODE");
        cmd
    }

    fn finish(mut cmd: Command, args: &[&str], env: &[(&str, &str)]) -> Run {
        cmd.args(args);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("run boss-api");
        Run {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    /// Forget the previous run's argv, so "curl must not run" after a
    /// run that DID reach curl asserts on this run, not the last one.
    fn clear_argv(&self) {
        let _ = std::fs::remove_file(&self.argv);
    }

    fn curl_argv(&self) -> Vec<String> {
        std::fs::read_to_string(&self.argv)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// The value of a `-H` header the stub recorded, by name.
    fn header(&self, name: &str) -> Option<String> {
        let prefix = format!("{name}: ");
        self.curl_argv()
            .into_iter()
            .find_map(|a| a.strip_prefix(&prefix).map(str::to_string))
    }
}

#[test]
fn the_script_is_in_the_tree_and_executable() {
    use std::os::unix::fs::PermissionsExt;
    let path = repo_root().join(SCRIPT);
    let meta = std::fs::metadata(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert!(
        meta.permissions().mode() & 0o111 != 0,
        "{SCRIPT} must be executable: the pod's /work/tools/bin/boss-api is a symlink to it"
    );
    let text = std::fs::read_to_string(&path).expect("read boss-api");
    assert!(
        !text.contains("svc.cluster.local:7900") && !text.contains("10.20.0.34:7900"),
        "the SoR address is read from {SOR_URL_FILE}, not spelled in the script (CLAUDE.md 9a)"
    );
    assert!(
        !text.contains("cat /etc/boss/machine-token"),
        "the token is read through infra/lib's machine_token_header, not a literal read"
    );
    assert!(
        !text.contains("BOSS_MACHINE_TOKEN_FILE"),
        "the token is the `current` slot of BOSS_MACHINE_TOKEN_DIR, the directory boss-core \
         reads — never a FILE (backlog 1876bbdb, INFO-6)"
    );
}

/// The read every session makes: body to stdout, code to stderr,
/// exit 0, and curl asked for the pinned base URL plus the path.
#[test]
fn a_get_prints_the_body_and_the_http_line() {
    let f = Fixture::new("get");
    let r = f.run(
        &["GET", "/api/jobs?kind=pr-train"],
        &[
            ("STUB_BODY", r#"{"total":3}"#),
            ("BOSS_ACTOR", "emp-reader"),
        ],
    );
    assert_eq!(r.code, 0, "a 2xx exits 0: {}", r.stderr);
    // The newline is the `-w '\n%{http_code}'` separator, which the pod
    // copy leaves on the body; `boss-api GET … > file` has always
    // written it, so it stays.
    assert_eq!(
        r.stdout, "{\"total\":3}\n",
        "the body (and the separator newline) goes to stdout, nothing else"
    );
    assert_eq!(
        r.stderr, "HTTP:200\n",
        "the code goes to stderr as one HTTP: line"
    );

    let argv = f.curl_argv();
    let method = argv
        .iter()
        .position(|a| a == "-X")
        .map(|i| argv[i + 1].as_str());
    assert_eq!(method, Some("GET"), "curl -X carries the method: {argv:?}");
    assert_eq!(
        argv.last().map(String::as_str),
        Some("http://sor.test:7900/api/jobs?kind=pr-train"),
        "the URL is BOSS_JOBS_URL plus the path: {argv:?}"
    );
    assert!(
        !argv.iter().any(|a| a == "--data-binary"),
        "a GET sends no body: {argv:?}"
    );
    assert_eq!(
        f.header("Content-Type").as_deref(),
        Some("application/json"),
        "{argv:?}"
    );
    assert_eq!(
        f.header("X-Boss-Machine-Token"),
        None,
        "no token file, no token header: {argv:?}"
    );
}

/// A write: the body file rides as `--data-binary @file`, and a
/// non-2xx answer keeps the body and the code but exits 1 — the
/// property drivers rely on to fail loudly.
#[test]
fn a_write_sends_the_body_file_and_a_non_2xx_exits_1() {
    let f = Fixture::new("write");
    let body = f.root.join("body.json");
    write_file(&body, r#"{"note":"hello"}"#);
    let body_arg = format!("@{}", body.display());

    let r = f.run(
        &[
            "PATCH",
            "/api/jobs/abc/metadata",
            body.to_str().expect("utf8"),
        ],
        &[
            ("STUB_BODY", r#"{"ok":true}"#),
            ("BOSS_ACTOR", "emp-writer"),
        ],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(r.stdout, "{\"ok\":true}\n");
    let argv = f.curl_argv();
    let data = argv
        .iter()
        .position(|a| a == "--data-binary")
        .map(|i| argv[i + 1].as_str());
    assert_eq!(data, Some(body_arg.as_str()), "{argv:?}");
    assert!(argv.iter().any(|a| a == "PATCH"), "{argv:?}");

    let r = f.run(
        &["PUT", "/api/jobs/abc", body.to_str().expect("utf8")],
        &[
            ("STUB_BODY", r#"{"error":"conflict"}"#),
            ("STUB_CODE", "409"),
            ("BOSS_ACTOR", "emp-writer"),
        ],
    );
    assert_eq!(r.code, 1, "a non-2xx exits 1: {}", r.stderr);
    assert_eq!(
        r.stdout, "{\"error\":\"conflict\"}\n",
        "the error body still reaches stdout"
    );
    assert_eq!(r.stderr, "HTTP:409\n");
}

/// Who the call signs as, in the order the `boss` CLI uses
/// (crates/orchestrators/boss-cli/src/identity.rs): `BOSS_ACTOR`,
/// else the one line in `$HOME/.config/boss/actor`, else — for a READ,
/// which attributes nothing — the unidentified marker, said once on
/// stderr. Never the pod copy's fixed id: that signed a fresh session's
/// history as the operator's agent (backlog 416d503c).
#[test]
fn the_actor_header_comes_from_env_then_the_actor_file_and_an_unnamed_read_is_marked() {
    let f = Fixture::new("actor");

    let r = f.run(&["GET", "/api/jobs"], &[]);
    assert_eq!(r.code, 0, "an unnamed read still goes out: {}", r.stderr);
    let user = f.header("X-Boss-User").expect("X-Boss-User header");
    assert!(
        user.contains(&format!(r#""id":"{UNIDENTIFIED}""#)),
        "unnamed, a read is marked unidentified: {user}"
    );
    assert!(
        !user.contains(FORMER_DEFAULT_ACTOR),
        "the pod copy's fixed id must never ride unasked: {user}"
    );
    // Backlog d843abf2 (measured 2026-09-18): nobody-in-particular
    // reads under the platform's own READ role, not the operator's —
    // the same rule as the CLI's identity.rs. The role's one home is
    // boss_core::roles; a shell script cannot import it, so this pins
    // the copy (CLAUDE.md §9a).
    let reader_role = boss_core::roles::AUDIT_READONLY_ROLE;
    assert!(
        user.contains(&format!(r#""role":"{reader_role}""#))
            && user.contains(r#""access_tier":"auditor""#),
        "an unnamed read carries {reader_role} at the auditor tier: {user}"
    );
    assert!(
        !user.contains("platform-admin"),
        "an unnamed read must not carry the operator's role: {user}"
    );
    assert!(
        r.stderr.contains(UNIDENTIFIED) && r.stderr.contains("BOSS_ACTOR"),
        "the read says on stderr that nobody is named and how to fix it: {}",
        r.stderr
    );
    assert!(
        r.stderr.ends_with("HTTP:200\n"),
        "the HTTP line is still the last thing on stderr: {}",
        r.stderr
    );

    let cfg = f.home.join(".config/boss");
    create_dir(&cfg);
    write_file(&cfg.join("actor"), "emp-from-file\n");
    let r = f.run(&["GET", "/api/jobs"], &[]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    let user = f.header("X-Boss-User").expect("X-Boss-User header");
    assert!(
        user.contains(r#""id":"emp-from-file""#),
        "the actor file's one line names the actor, trailing newline dropped: {user}"
    );
    assert!(
        user.contains(r#""role":"platform-admin""#) && user.contains(r#""access_tier":"operator""#),
        "a NAMED caller keeps the operator's role and tier: {user}"
    );
    assert_eq!(r.stderr, "HTTP:200\n", "a named read says nothing extra");

    let r = f.run(&["GET", "/api/jobs"], &[("BOSS_ACTOR", "emp-from-env")]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    let user = f.header("X-Boss-User").expect("X-Boss-User header");
    assert!(
        user.contains(r#""id":"emp-from-env""#),
        "BOSS_ACTOR wins over the file: {user}"
    );

    // A blank env falls THROUGH to the file rather than shadowing it —
    // `export BOSS_ACTOR=` is a misconfiguration, not the empty actor.
    let r = f.run(&["GET", "/api/jobs"], &[("BOSS_ACTOR", "  ")]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    let user = f.header("X-Boss-User").expect("X-Boss-User header");
    assert!(
        user.contains(r#""id":"emp-from-file""#),
        "blank is not an answer; the file still names the actor: {user}"
    );
}

/// The claim of backlog 416d503c: an unnamed WRITE is refused before
/// curl runs — exit 2, the CLI's sentence naming both fixes and the
/// identity it refused to use — for every method that records an actor.
#[test]
fn an_unnamed_write_is_refused_before_curl_runs_and_names_both_fixes() {
    let f = Fixture::new("unnamed-write");
    let body = f.root.join("body.json");
    write_file(&body, r#"{"status":"completed"}"#);

    let r = f.run(
        &[
            "PUT",
            "/api/jobs/abc/steps/s1",
            body.to_str().expect("utf8"),
        ],
        &[],
    );
    assert_eq!(
        r.code, 2,
        "an unnamed write is a refusal, not a request: {}",
        r.stderr
    );
    assert!(r.stdout.is_empty(), "nothing on stdout: {}", r.stdout);
    assert!(f.curl_argv().is_empty(), "curl must not run");
    assert!(
        r.stderr
            .contains("refusing to sign PUT /api/jobs/abc/steps/s1"),
        "{}",
        r.stderr
    );
    assert!(
        r.stderr
            .contains("nothing names the actor running this command"),
        "the CLI's sentence, mirrored: {}",
        r.stderr
    );
    assert!(
        r.stderr.contains("export BOSS_ACTOR=")
            && r.stderr
                .contains(&f.home.join(".config/boss/actor").display().to_string()),
        "both fixes are named, with the file's location on THIS machine: {}",
        r.stderr
    );
    assert!(
        r.stderr.contains(FORMER_DEFAULT_ACTOR),
        "it names the identity it REFUSED to use, so this reads apart from a policy denial: {}",
        r.stderr
    );
    assert!(
        !r.stderr.contains("HTTP:"),
        "no HTTP line: no request was made: {}",
        r.stderr
    );

    for method in ["POST", "PATCH", "DELETE"] {
        let r = f.run(&[method, "/api/jobs/abc"], &[]);
        assert_eq!(r.code, 2, "{method} records an actor: {}", r.stderr);
        assert!(f.curl_argv().is_empty(), "{method}: curl must not run");
    }

    // A whitespace-only BOSS_ACTOR is not a name either.
    let r = f.run(&["POST", "/api/jobs"], &[("BOSS_ACTOR", " ")]);
    assert_eq!(r.code, 2, "blank is not an answer: {}", r.stderr);
    assert!(f.curl_argv().is_empty(), "curl must not run");

    // Named, the same write goes out.
    let r = f.run(
        &[
            "PUT",
            "/api/jobs/abc/steps/s1",
            body.to_str().expect("utf8"),
        ],
        &[("BOSS_ACTOR", "emp-writer")],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(
        f.header("X-Boss-User")
            .is_some_and(|u| u.contains(r#""id":"emp-writer""#)),
        "{:?}",
        f.curl_argv()
    );
}

/// The system-of-record address is spelled ONCE in infra/dev: the one
/// line in `infra/dev/sor-url`, which the script reads from beside
/// itself when `BOSS_JOBS_URL` is unset. Until 2026-09-14 this script
/// and the boss shim each carried their own spelling of the same
/// deployment (backlog 416d503c, CLAUDE.md 9a).
#[test]
fn the_default_url_is_the_one_line_in_sor_url() {
    let sor_url_path = repo_root().join(SOR_URL_FILE);
    let text = std::fs::read_to_string(&sor_url_path)
        .unwrap_or_else(|e| panic!("{}: {e}", sor_url_path.display()));
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 1, "{SOR_URL_FILE} is one line: {text:?}");
    let url = lines[0].trim();
    assert!(
        url.starts_with("http://") || url.starts_with("https://"),
        "{SOR_URL_FILE} holds a URL: {url:?}"
    );
    assert!(
        !url.ends_with('/'),
        "no trailing slash — the script appends /api/...: {url:?}"
    );

    let f = Fixture::new("sor-url");
    let r = Fixture::finish(
        f.command(&repo_root().join(SCRIPT)),
        &["GET", "/api/yard/status"],
        &[("BOSS_ACTOR", "emp-reader")],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    let argv = f.curl_argv();
    assert_eq!(
        argv.last().map(String::as_str),
        Some(format!("{url}/api/yard/status").as_str()),
        "with BOSS_JOBS_URL unset the file's line is the base: {argv:?}"
    );

    // BOSS_JOBS_URL still wins — the conductor's unit and every test
    // set it explicitly.
    let r = f.run(
        &["GET", "/api/yard/status"],
        &[("BOSS_ACTOR", "emp-reader")],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(
        f.curl_argv().last().map(String::as_str),
        Some("http://sor.test:7900/api/yard/status")
    );

    // A copy of the script with no sor-url beside it names no system
    // of record: refuse (exit 2) and say which file is missing, rather
    // than asking curl for a relative path. A wrong target answers
    // instead of erroring (CLAUDE.md §Doors); a missing one must not.
    let orphan = f.root.join("orphan");
    create_dir(&orphan);
    let script_text = std::fs::read_to_string(repo_root().join(SCRIPT)).expect("read boss-api");
    write_exec(&orphan.join("boss-api"), &script_text);
    f.clear_argv();
    let r = Fixture::finish(
        f.command(&orphan.join("boss-api")),
        &["GET", "/api/yard/status"],
        &[("BOSS_ACTOR", "emp-reader")],
    );
    assert_eq!(r.code, 2, "no file, no default, no request: {}", r.stderr);
    assert!(f.curl_argv().is_empty(), "curl must not run");
    assert!(
        r.stderr.contains("sor-url") && r.stderr.contains("BOSS_JOBS_URL"),
        "the refusal names the file and the override: {}",
        r.stderr
    );
}

/// The machine token rides only when the `current` slot of its directory
/// holds one, and the directory is `BOSS_MACHINE_TOKEN_DIR` — the name
/// boss-core reads (backlog 1876bbdb, INFO-6). The stub records the
/// header's presence, not its value.
#[test]
fn the_machine_token_header_rides_only_when_its_file_exists() {
    let f = Fixture::new("token");
    let token_file = f.root.join("machine-token");
    create_dir(&token_file);
    write_file(&token_file.join("current"), "stub-token-for-the-test\n");

    let r = f.run(
        &["GET", "/api/jobs"],
        &[
            ("BOSS_MACHINE_TOKEN_DIR", token_file.to_str().expect("utf8")),
            ("BOSS_ACTOR", "emp-reader"),
            // The fixture's record is `sor.test`: listed, as the
            // estate's own record host is (backlog 2ee29275).
            ("BOSS_MACHINE_TOKEN_HOSTS", "sor.test"),
        ],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(
        f.header("X-Boss-Machine-Token").as_deref(),
        Some("<present>"),
        "the token header must ride when the file exists: {:?}",
        f.curl_argv()
    );
    // ...and it rides in a FILE: curl's command line is world-readable in
    // ps and /proc/<pid>/cmdline while it runs, and until 2026-09-28 this
    // door put `X-Boss-Machine-Token: $TOK` there (backlog 5f3ad356).
    let raw = std::fs::read_to_string(format!("{}.raw", f.argv.display())).unwrap_or_default();
    assert!(
        !raw.contains("stub-token-for-the-test"),
        "the token must never be in curl's argv:\n{raw}"
    );
    assert!(
        raw.lines()
            .collect::<Vec<_>>()
            .windows(2)
            .any(|w| w[0] == "-H" && w[1].starts_with('@')),
        "the token header is handed over as -H @<file>:\n{raw}"
    );
}

/// THE OLD FILE LAYOUT sends nothing, and says so (backlog 1876bbdb,
/// INFO-6). This door read `/etc/boss/machine-token` as a FILE while
/// boss-core reads it as a DIRECTORY holding `current`; the day car 4
/// mounted the directory, every door write would have gone out
/// unstamped in silence. A file where the directory belongs is now named
/// on stderr, and the call still goes out — a gate in `report` admits it.
#[test]
fn a_token_file_where_the_directory_belongs_is_said_and_not_sent() {
    let f = Fixture::new("token-old-layout");
    let token_file = f.root.join("machine-token");
    write_file(&token_file, "stub-token-for-the-test\n");
    let r = f.run(
        &["GET", "/api/jobs"],
        &[
            ("BOSS_MACHINE_TOKEN_DIR", token_file.to_str().expect("utf8")),
            ("BOSS_ACTOR", "emp-reader"),
            ("BOSS_MACHINE_TOKEN_HOSTS", "sor.test"),
        ],
    );
    assert_eq!(r.code, 0, "the call still goes out: {}", r.stderr);
    assert!(f.header("X-Boss-Machine-Token").is_none(), "{}", r.stderr);
    assert!(
        r.stderr.contains("not a directory"),
        "the wrong layout is named: {}",
        r.stderr
    );
    assert!(!r.stderr.contains("stub-token-for-the-test"));
}

/// THE DOORS' COPY LIVES OFF THE DEFAULT PATH (backlog 1876bbdb, INFO-5).
/// The broker copies the live token into boss-dev, and the dev pod is
/// where builders run handler tests: mounted at boss-core's default
/// `/etc/boss/machine-token`, every stamping client a test built —
/// `common::api_client()` among them, which the name-scan pin cannot see
/// — would stamp the live token onto a loopback mock, and a failing test
/// that prints its request would put it in a transcript. So the pod
/// mounts it at the one directory `infra/dev/machine-token-dir` names,
/// and only the doors read that file: with `BOSS_MACHINE_TOKEN_DIR`
/// unset, this door takes its line, never the core default.
#[test]
fn unnamed_the_door_reads_the_directory_its_machine_token_dir_names() {
    const DIR_FILE: &str = "infra/dev/machine-token-dir";
    let named = std::fs::read_to_string(repo_root().join(DIR_FILE)).expect("machine-token-dir");
    let named = named.trim();
    assert!(named.starts_with('/'), "{DIR_FILE}: an absolute path");
    assert_ne!(
        named,
        boss_core::machine_token::DEFAULT_TOKEN_DIR,
        "{DIR_FILE} must not be boss-core's default, which every test process reads"
    );

    let f = Fixture::new("token-door-default");
    let dev = f.root.join("tree/infra/dev");
    create_dir(&dev);
    let script_text = std::fs::read_to_string(repo_root().join(SCRIPT)).expect("read boss-api");
    write_exec(&dev.join("boss-api"), &script_text);
    write_file(&dev.join("sor-url"), "http://lone.test:7900\n");
    lay_roll_lib_beside(&dev);
    write_file(
        &f.root.join("tree/infra/lib/secret-header.sh"),
        &std::fs::read_to_string(repo_root().join("infra/lib/secret-header.sh")).expect("lib"),
    );
    let mount = f.root.join("doors-mount");
    create_dir(&mount);
    write_file(&mount.join("current"), "stub-token-for-the-test\n");
    write_file(
        &dev.join("machine-token-dir"),
        &format!("{}\n", mount.display()),
    );
    let mut cmd = f.command(&dev.join("boss-api"));
    cmd.env_remove("BOSS_MACHINE_TOKEN_DIR");
    let r = Fixture::finish(cmd, &["GET", "/api/jobs"], &[("BOSS_ACTOR", "agent-x")]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(
        f.header("X-Boss-Machine-Token").as_deref(),
        Some("<present>"),
        "the door read the directory beside it names: {}",
        r.stderr
    );
}

/// The door stamps the machine token only on the estate's own hosts —
/// the rule boss-core's clients take (backlog 2ee29275, F1): loopback,
/// and the hosts and `.namespace` suffixes `BOSS_MACHINE_TOKEN_HOSTS`
/// lists. A base the operator pointed elsewhere
/// (`BOSS_JOBS_URL=https://boss.algedonic.dev`, the edge that 302s to
/// Access; another instance's namespace) goes out unstamped with one
/// line on stderr naming the scheme and host, never the path or query.
/// The rule lives twice — here in shell, in `machine_token::Hosts` in
/// Rust — so every case is held to what boss-core answers for the same
/// host (CLAUDE.md §9a), including the `?`/`#`-before-`@` shapes the
/// review of 54d9a23a (MEDIUM-2) showed the shell reading as loopback
/// while curl and Rust both read the host before them.
#[test]
fn the_machine_token_rides_only_to_an_estate_host_the_way_boss_core_decides() {
    const PATH: &str = "/api/jobs?state=q-secret";
    let f = Fixture::new("token-hosts");
    let token_file = f.root.join("machine-token");
    create_dir(&token_file);
    write_file(&token_file.join("current"), "stub-token-for-the-test\n");
    let token = token_file.to_str().expect("utf8");
    let list = "record.example.net, 192.0.2.34";
    let prod = ".boss.svc.cluster.local";
    for (base, listed) in [
        ("http://127.0.0.1:7900", ""),
        ("http://localhost:7900", ""),
        ("http://[::1]:7900", ""),
        ("http://boss-jobs-internal.boss.svc.cluster.local:7900", ""),
        (
            "http://boss-jobs-internal.boss.svc.cluster.local:7900",
            prod,
        ),
        ("http://BOSS-JOBS.boss.svc.cluster.local.:7900", prod),
        // Another instance's namespace: prod's token never reaches it.
        (
            "http://boss-jobs-internal.boss-playground.svc.cluster.local:7900",
            prod,
        ),
        (
            "http://boss-gateway.boss-playground.svc.cluster.local",
            prod,
        ),
        ("http://boss.svc.cluster.local", prod),
        ("http://192.0.2.34:7900", list),
        ("http://Record.Example.Net", list),
        ("http://192.0.2.34:7900", ""),
        ("https://boss.algedonic.dev", list),
        ("http://127.foo.example:7900", ""),
        ("http://svc.cluster.local.example.com", prod),
        ("http://operator@198.51.100.7:7900", list),
        // MEDIUM-2: a `?` or `#` before an `@` ends the authority.
        ("http://evil.com#@127.0.0.1", ""),
        ("http://evil.com?@127.0.0.1:7900", ""),
        (
            "http://evil.com?x=@boss-jobs-internal.boss.svc.cluster.local",
            prod,
        ),
        // Delta review of ddfa1032 (N1): a dotted quad is loopback only
        // when every octet is 0-255. Url::parse refuses 127.0.0.999, and
        // curl hands it to DNS and the search list.
        ("http://127.0.0.999:7900", ""),
        ("http://127.0.0.256:7900", ""),
        ("http://127.1000.0.1:7900", ""),
        ("http://127.255.255.255:7900", ""),
        ("http://127.0.0.1.evil.example:7900", ""),
    ] {
        f.clear_argv();
        let r = f.run(
            &["GET", PATH],
            &[
                ("BOSS_JOBS_URL", base),
                ("BOSS_MACHINE_TOKEN_DIR", token),
                ("BOSS_MACHINE_TOKEN_HOSTS", listed),
                ("BOSS_ACTOR", "emp-reader"),
                // A port table would move the jobs path nowhere, but a
                // copy with none keeps the case to the host alone.
                ("BOSS_SOR_PORTS", "jobs=7900"),
            ],
        );
        assert_eq!(r.code, 0, "{base}: {}", r.stderr);
        // Held to the URL curl is SENT, base and path together — the
        // door once judged the base alone (delta review of ddfa1032, B1).
        let rust =
            boss_core::machine_token::Hosts::parse(listed).allows_url(&format!("{base}{PATH}"));
        let door = f.header("X-Boss-Machine-Token").is_some();
        assert_eq!(
            door, rust,
            "{base} (list {listed:?}): the door stamped={door}, boss-core allows={rust}"
        );
        if door {
            assert!(
                !r.stderr.contains("withheld"),
                "{base}: a stamped call says nothing: {}",
                r.stderr
            );
        } else {
            let scheme = base.split("://").next().unwrap();
            assert!(
                r.stderr.contains("machine token withheld")
                    && r.stderr.contains(&format!("{scheme}://")),
                "{base}: an unstamped call says so, naming the scheme: {}",
                r.stderr
            );
            assert!(
                !r.stderr.contains("q-secret") && !r.stderr.contains("/api/jobs"),
                "{base}: never the path or the query: {}",
                r.stderr
            );
            assert!(
                !r.stderr.contains("operator@"),
                "{base}: never the userinfo: {}",
                r.stderr
            );
        }
    }
}

/// The door judged the host of the BASE, and curl is sent BASE+PATH, so
/// a path that does not start with `/` continued the authority:
/// `boss-api GET '@evil.invalid/api/jobs'` against a loopback base sent
/// the token on a URL whose base became the userinfo of evil.invalid,
/// which curl and Url::parse both send to evil.invalid (delta review of
/// ddfa1032, B1). Every real path starts with `/`, so anything else is
/// usage — refused before curl and before the token is looked at.
#[test]
fn a_path_that_does_not_start_with_a_slash_is_refused_before_curl_and_the_token() {
    let f = Fixture::new("token-path");
    let token_file = f.root.join("machine-token");
    create_dir(&token_file);
    write_file(&token_file.join("current"), "stub-token-for-the-test\n");
    let token = token_file.to_str().expect("utf8");
    let sor = std::fs::read_to_string(repo_root().join(SOR_URL_FILE)).expect("sor-url");
    let sor = sor.trim();
    for (base, listed) in [
        ("http://127.0.0.1:7900", ""),
        (sor, ".boss.svc.cluster.local"),
    ] {
        for path in ["@evil.invalid/x", ":1@evil.invalid/x", "api/jobs"] {
            f.clear_argv();
            let r = f.run(
                &["GET", path],
                &[
                    ("BOSS_JOBS_URL", base),
                    ("BOSS_MACHINE_TOKEN_DIR", token),
                    ("BOSS_MACHINE_TOKEN_HOSTS", listed),
                    ("BOSS_ACTOR", "emp-reader"),
                    ("BOSS_SOR_PORTS", "jobs=7900"),
                ],
            );
            assert_eq!(r.code, 2, "{base} + {path}: usage, {}", r.stderr);
            assert!(r.stderr.contains("usage:"), "{base} + {path}: {}", r.stderr);
            assert!(
                f.curl_argv().is_empty(),
                "{base} + {path}: curl must not run"
            );
            assert!(
                !r.stderr.contains("withheld"),
                "{base} + {path}: refused before the token decision: {}",
                r.stderr
            );
        }
        // What the refusal prevents: boss-core, reading the URL curl
        // would have been sent, withholds the token from both.
        let hosts = boss_core::machine_token::Hosts::parse(listed);
        for path in ["@evil.invalid/x", ":1@evil.invalid/x"] {
            assert!(
                !hosts.allows_url(&format!("{base}{path}")),
                "{base}{path} is not an estate host"
            );
        }
    }
}

/// Where the door and boss-core still answer differently, named one by
/// one with why each is harmless (delta review of ddfa1032, N2), so
/// "held equal case by case" stays true of everything not listed here.
/// Each row asserts BOTH answers as they stand: a row where the two
/// have come to agree fails, and belongs in the equality test instead.
#[test]
fn the_door_and_boss_core_disagree_only_where_it_is_named_and_harmless() {
    const PATH: &str = "/api/jobs";
    let f = Fixture::new("token-exceptions");
    let token_file = f.root.join("machine-token");
    create_dir(&token_file);
    write_file(&token_file.join("current"), "stub-token-for-the-test\n");
    let token = token_file.to_str().expect("utf8");
    // (base, door stamps, boss-core stamps, why it is harmless)
    for (base, door_stamps, core_stamps, why) in [
        (
            "http://evil.com\\@127.0.0.1:7900",
            true,
            false,
            "WHATWG reads the backslash as a path separator (host evil.com); curl connects to 127.0.0.1, so the token goes to loopback",
        ),
        (
            "http://[0:0:0:0:0:0:0:1]:7900",
            false,
            true,
            "the door knows ::1 only as spelled; withholding costs a stamp, never a leak",
        ),
        (
            "http://localhost..:7900",
            false,
            true,
            "the door drops one trailing dot, boss-core every one; withholding fails closed",
        ),
        (
            "http://%31%32%37.0.0.1:7900",
            false,
            true,
            "Url::parse percent-decodes the host to 127.0.0.1; the door does not decode, and fails closed",
        ),
        (
            "http://127.1:7900",
            false,
            true,
            "Url::parse expands the short IPv4 form to 127.0.0.1; the door takes only a full dotted quad, and fails closed",
        ),
    ] {
        f.clear_argv();
        let r = f.run(
            &["GET", PATH],
            &[
                ("BOSS_JOBS_URL", base),
                ("BOSS_MACHINE_TOKEN_DIR", token),
                ("BOSS_MACHINE_TOKEN_HOSTS", ""),
                ("BOSS_ACTOR", "emp-reader"),
                ("BOSS_SOR_PORTS", "jobs=7900"),
            ],
        );
        assert_eq!(r.code, 0, "{base}: {}", r.stderr);
        let door = f.header("X-Boss-Machine-Token").is_some();
        let core = boss_core::machine_token::Hosts::parse("").allows_url(&format!("{base}{PATH}"));
        assert_eq!(door, door_stamps, "{base}: the door stamped={door} ({why})");
        assert_eq!(core, core_stamps, "{base}: boss-core allows={core} ({why})");
        assert_ne!(
            door, core,
            "{base}: the two now agree — move it to the equality test"
        );
    }
}

/// On the dev pod nothing sets `BOSS_MACHINE_TOKEN_HOSTS`, and the pod's
/// record is `infra/dev/sor-url` — a Service name, which is no longer
/// allowed by default (review of 54d9a23a, MEDIUM-1). So an UNSET list
/// is the host of that file's line: the door's own record is stamped,
/// and another namespace is not. Set (even empty), the variable wins.
#[test]
fn an_unset_list_is_the_host_of_the_doors_own_sor_url() {
    let f = Fixture::new("token-hosts-default");
    let token_file = f.root.join("machine-token");
    create_dir(&token_file);
    write_file(&token_file.join("current"), "stub-token-for-the-test\n");
    let token = token_file.to_str().expect("utf8");
    let sor = std::fs::read_to_string(repo_root().join(SOR_URL_FILE)).expect("sor-url");
    let sor = sor.trim();
    let playground = sor.replace(".boss.svc.", ".boss-playground.svc.");
    assert_ne!(
        playground, sor,
        "the pod's record is a Service in namespace boss: {sor}"
    );
    for (base, want) in [(sor, true), (playground.as_str(), false)] {
        f.clear_argv();
        let r = f.run(
            &["GET", "/api/jobs"],
            &[
                ("BOSS_JOBS_URL", base),
                ("BOSS_MACHINE_TOKEN_DIR", token),
                ("BOSS_ACTOR", "emp-reader"),
                ("BOSS_SOR_PORTS", "jobs=7900"),
            ],
        );
        assert_eq!(r.code, 0, "{base}: {}", r.stderr);
        assert_eq!(
            f.header("X-Boss-Machine-Token").is_some(),
            want,
            "{base}: stamped should be {want}: {}",
            r.stderr
        );
    }
    // Set and empty is a list: loopback only, the record withheld.
    f.clear_argv();
    let r = f.run(
        &["GET", "/api/jobs"],
        &[
            ("BOSS_JOBS_URL", sor),
            ("BOSS_MACHINE_TOKEN_DIR", token),
            ("BOSS_MACHINE_TOKEN_HOSTS", ""),
            ("BOSS_ACTOR", "emp-reader"),
            ("BOSS_SOR_PORTS", "jobs=7900"),
        ],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(f.header("X-Boss-Machine-Token").is_none(), "{}", r.stderr);
}

/// Refusals happen before curl: a method outside the five, or a
/// missing path, is usage (exit 2), and nothing is sent.
#[test]
fn a_bad_method_or_a_missing_path_is_refused_before_curl_runs() {
    let f = Fixture::new("usage");

    let r = f.run(&["FETCH", "/api/jobs"], &[]);
    assert_eq!(r.code, 2, "{}", r.stderr);
    assert!(r.stderr.contains("usage:"), "{}", r.stderr);
    assert!(f.curl_argv().is_empty(), "curl must not run");

    let r = f.run(&["GET"], &[]);
    assert_eq!(r.code, 2, "{}", r.stderr);
    assert!(r.stderr.contains("usage:"), "{}", r.stderr);
    assert!(f.curl_argv().is_empty(), "curl must not run");
}

// =====================================================================
// THE PORT FOLLOWS THE PATH (backlog de0989d2, measured 2026-09-17
// 14:50Z). The reconcile step of the first real sponsorship needs an
// account created through POST /api/people/accounts, which
// boss-accounts serves on 7550; this door sent it to the jobs port,
// where it is a 404 about a surface that exists. The rules are the one
// route function the forge's probe reader also sources; the port table
// is BOSS_SOR_PORTS, else infra/forge/sor-ports.env beside the tree.
// =====================================================================

/// The measured case: a write to an accounts path leaves on the
/// accounts port, on the same host, with the body and the actor intact.
#[test]
fn a_post_to_an_accounts_path_routes_to_the_accounts_port() {
    let f = Fixture::new("route-accounts");
    let body = f.root.join("account.json");
    write_file(&body, r#"{"name":"sponsor"}"#);
    let r = f.run(
        &["POST", "/api/people/accounts", body.to_str().expect("utf8")],
        &[
            ("STUB_BODY", r#"{"id":"acct-1"}"#),
            ("BOSS_ACTOR", "agent-x"),
        ],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    let argv = f.curl_argv();
    assert_eq!(
        argv.last().map(String::as_str),
        Some(
            format!(
                "http://sor.test:{}/api/people/accounts",
                boss_ports::prod("accounts")
            )
            .as_str()
        ),
        "the host is kept and the port is boss-accounts': {argv:?}"
    );
    assert!(
        argv.iter().any(|a| a == "--data-binary"),
        "the body still rides: {argv:?}"
    );
    assert!(
        f.header("X-Boss-User")
            .is_some_and(|u| u.contains(r#""id":"agent-x""#)),
        "the routed write is still signed: {argv:?}"
    );

    // A read of the same surface, with a query string, goes the same way.
    let r = f.run(
        &["GET", "/api/people/accounts?limit=1"],
        &[("BOSS_ACTOR", "agent-x")],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(
        f.curl_argv().last().map(String::as_str),
        Some(
            format!(
                "http://sor.test:{}/api/people/accounts?limit=1",
                boss_ports::prod("accounts")
            )
            .as_str()
        )
    );
}

/// The ledger and the dispatcher joined the door on 2026-09-17
/// (backlog 77fd7b5a + 4145d2c1): a journal entry posts to boss-ledger
/// on its port, and the rule registry reads from the dispatcher on
/// its own — neither is the jobs port any longer.
#[test]
fn a_ledger_path_and_a_dispatcher_path_route_to_their_own_ports() {
    let f = Fixture::new("route-ledger-dispatcher");
    for (path, service) in [
        ("/api/ledger/journal-entries", "ledger"),
        ("/api/dispatcher/rules", "dispatcher"),
    ] {
        let r = f.run(&["GET", path], &[("BOSS_ACTOR", "agent-x")]);
        assert_eq!(r.code, 0, "{path}: {}", r.stderr);
        assert_eq!(
            f.curl_argv().last().map(String::as_str),
            Some(format!("http://sor.test:{}{path}", boss_ports::prod(service)).as_str()),
            "{path} leaves on {service}'s port, host kept"
        );
    }
}

/// The jobs API stays where it was, and so does anything the table
/// does not name — including a prefix that merely resembles a routed
/// one. Routing adds ports; it never moves the base.
#[test]
fn the_jobs_api_and_an_unknown_prefix_stay_on_the_base() {
    let f = Fixture::new("route-default");
    for path in [
        "/api/jobs?kind=pr-train",
        "/api/yard/status",
        "/api/peoples/x",
    ] {
        let r = f.run(&["GET", path], &[("BOSS_ACTOR", "agent-x")]);
        assert_eq!(r.code, 0, "{path}: {}", r.stderr);
        assert_eq!(
            f.curl_argv().last().map(String::as_str),
            Some(format!("http://sor.test:7900{path}").as_str()),
            "{path} is the jobs API, on the base as given"
        );
    }
}

/// A table that exists but lacks the routed service is a defect in the
/// table, not a reason to send the request to the jobs port: refuse
/// (exit 2), name the service and the table, and never reach curl.
/// `BOSS_SOR_PORTS` wins over the file when set, which is also how a
/// test hands the door a table of its choosing.
#[test]
fn a_table_that_lacks_the_service_is_refused_before_curl_and_the_env_table_wins() {
    let f = Fixture::new("route-missing");
    let body = f.root.join("account.json");
    write_file(&body, r#"{"name":"sponsor"}"#);
    let r = f.run(
        &["POST", "/api/people/accounts", body.to_str().expect("utf8")],
        &[
            ("BOSS_ACTOR", "agent-x"),
            ("BOSS_SOR_PORTS", "jobs=7900 people=7500"),
        ],
    );
    assert_eq!(
        r.code, 2,
        "a table without the service is a refusal: {}",
        r.stderr
    );
    assert!(f.curl_argv().is_empty(), "curl must not run");
    assert!(r.stdout.is_empty(), "nothing on stdout: {}", r.stdout);
    assert!(
        r.stderr.contains("accounts") && r.stderr.contains("jobs=7900 people=7500"),
        "the refusal names the missing service and the table it read: {}",
        r.stderr
    );
    assert!(
        !r.stderr.contains("HTTP:"),
        "no request was made: {}",
        r.stderr
    );

    // The env table, when set, is the table — the file beside the tree
    // is not consulted.
    let r = f.run(
        &["GET", "/api/people/accounts"],
        &[
            ("BOSS_ACTOR", "agent-x"),
            ("BOSS_SOR_PORTS", "jobs=7900 accounts=9999"),
        ],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(
        f.curl_argv().last().map(String::as_str),
        Some("http://sor.test:9999/api/people/accounts")
    );
}

// =====================================================================
// THE ONE SERVICE NO PATH ROUTES TO (backlog bf1f5ad2, 2026-09-19).
// The gateway fronts every path there is, so no prefix can route to it
// — the machine door carries it as a row a reader reaches BY NAME
// (the_machine_door_carries_every_read_surface.rs, NOT_PATH_ROUTED).
// The forge has such a reader, `boss-gateway-read`; the pod had none,
// so a builder rehearsing a gateway probe set the host AND the port
// table by hand and the door was not the door. `BOSS_SOR_SERVICE` is
// that name on this door: the path decides the service unless a name
// is given, and then the name decides it.
// =====================================================================

/// Named, the read leaves on the gateway's port with the host kept —
/// and the SAME path with no name is the jobs API, which is what makes
/// the name (not the path) the thing that routed it.
#[test]
fn a_named_service_sends_the_read_to_that_services_port() {
    let f = Fixture::new("route-named-service");

    let r = f.run(
        &["GET", "/health"],
        &[("BOSS_ACTOR", "agent-x"), ("BOSS_SOR_SERVICE", "gateway")],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(
        f.curl_argv().last().map(String::as_str),
        Some(format!("http://sor.test:{}/health", boss_ports::prod("gateway")).as_str()),
        "the named service decides the port; the host is still the system of record's"
    );

    let r = f.run(&["GET", "/health"], &[("BOSS_ACTOR", "agent-x")]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(
        f.curl_argv().last().map(String::as_str),
        Some("http://sor.test:7900/health"),
        "unnamed, /health is not a routed prefix and stays on the base"
    );

    // Blank is not a name: it falls through to the path, exactly as a
    // blank BOSS_ACTOR falls through to the file.
    let r = f.run(
        &["GET", "/api/people/accounts"],
        &[("BOSS_ACTOR", "agent-x"), ("BOSS_SOR_SERVICE", "  ")],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(
        f.curl_argv().last().map(String::as_str),
        Some(
            format!(
                "http://sor.test:{}/api/people/accounts",
                boss_ports::prod("accounts")
            )
            .as_str()
        ),
        "a blank name is not an answer; the path still routes"
    );
}

/// A name the table cannot place, or a name with no table at all, is
/// refused before curl — never sent to the jobs port. Reading the jobs
/// port under a gateway question answers 200 with a narrowed world
/// (the defect boss-gateway-read refuses for the same reason), and a
/// wrong target answers instead of erroring.
#[test]
fn a_named_service_the_table_cannot_place_is_refused_before_curl() {
    let f = Fixture::new("route-named-missing");

    let r = f.run(
        &["GET", "/health"],
        &[
            ("BOSS_ACTOR", "agent-x"),
            ("BOSS_SOR_SERVICE", "gateway"),
            ("BOSS_SOR_PORTS", "jobs=7900 people=7500"),
        ],
    );
    assert_eq!(
        r.code, 2,
        "a table without the service refuses: {}",
        r.stderr
    );
    assert!(f.curl_argv().is_empty(), "curl must not run");
    assert!(
        r.stderr.contains("gateway") && r.stderr.contains("jobs=7900 people=7500"),
        "the refusal names the service and the table it read: {}",
        r.stderr
    );
    assert!(!r.stderr.contains("HTTP:"), "no request: {}", r.stderr);

    // No table at all is the same refusal, not the silent fall-through
    // an unnamed path gets: a name that cannot be placed must never
    // leave on the base.
    let lone = f.root.join("lone-named");
    create_dir(&lone);
    let script_text = std::fs::read_to_string(repo_root().join(SCRIPT)).expect("read boss-api");
    write_exec(&lone.join("boss-api"), &script_text);
    write_file(&lone.join("sor-url"), "http://lone.test:7900\n");
    f.clear_argv();
    let r = Fixture::finish(
        f.command(&lone.join("boss-api")),
        &["GET", "/health"],
        &[("BOSS_ACTOR", "agent-x"), ("BOSS_SOR_SERVICE", "gateway")],
    );
    assert_eq!(r.code, 2, "no table, no placing the name: {}", r.stderr);
    assert!(f.curl_argv().is_empty(), "curl must not run");
    assert!(
        r.stderr.contains("gateway") && r.stderr.contains("BOSS_SOR_PORTS"),
        "the refusal names the service and where a table comes from: {}",
        r.stderr
    );
}

// =====================================================================
// THE READ-ONLY ROWS (backlog 9a440539, 2026-10-01). Measured twice that
// day: `BOSS_SOR_SERVICE=assets` was refused (no assets row), and
// `GET /api/customers` answered 404 on the jobs port — so a read-only
// production count before a customer-data migration could not be made
// through any door, and the car carrying the migration sat HELD. Every
// boss-ports service the LAN door does not carry is now a row of
// infra/dev/sor-read-ports.env, read on the in-cluster read Service
// named by infra/dev/sor-read-url — GET only.
// =====================================================================

/// The host the tree's read rows are read on: the one line of
/// infra/dev/sor-read-url.
fn tree_read_url() -> String {
    std::fs::read_to_string(repo_root().join("infra/dev/sor-read-url"))
        .expect("infra/dev/sor-read-url is readable")
        .trim()
        .to_string()
}

/// The two measured reads now leave: the customers path by its prefix,
/// the assets service by its name — each on the read Service's host at
/// boss-ports' port, with the query string and the actor intact. The
/// LAN rows are untouched: an accounts path still leaves on the base.
#[test]
fn a_read_of_a_module_service_leaves_on_the_read_door_at_its_port() {
    let f = Fixture::new("route-read-only");
    let read = tree_read_url();

    let r = f.run(
        &["GET", "/api/customers?limit=1"],
        &[("BOSS_ACTOR", "agent-x")],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(
        f.curl_argv().last().map(String::as_str),
        Some(
            format!(
                "{read}:{}/api/customers?limit=1",
                boss_ports::prod("customers")
            )
            .as_str()
        ),
        "the customers path is read on the read door at the customers port"
    );
    assert!(
        f.header("X-Boss-User")
            .is_some_and(|u| u.contains(r#""id":"agent-x""#)),
        "the read is still signed"
    );

    let r = f.run(
        &["GET", "/api/assets/health"],
        &[("BOSS_ACTOR", "agent-x"), ("BOSS_SOR_SERVICE", "assets")],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(
        f.curl_argv().last().map(String::as_str),
        Some(format!("{read}:{}/api/assets/health", boss_ports::prod("assets")).as_str()),
        "BOSS_SOR_SERVICE=assets is placed, not refused"
    );

    let r = f.run(
        &["GET", "/api/people/accounts"],
        &[("BOSS_ACTOR", "agent-x")],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(
        f.curl_argv().last().map(String::as_str),
        Some(
            format!(
                "http://sor.test:{}/api/people/accounts",
                boss_ports::prod("accounts")
            )
            .as_str()
        ),
        "a LAN row stays on the system of record's host"
    );

    // The overrides are the tables, as BOSS_SOR_PORTS is for the LAN rows.
    let r = f.run(
        &["GET", "/api/customers"],
        &[
            ("BOSS_ACTOR", "agent-x"),
            ("BOSS_SOR_READ_PORTS", "customers=9855"),
            ("BOSS_SOR_READ_URL", "http://read.test"),
        ],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(
        f.curl_argv().last().map(String::as_str),
        Some("http://read.test:9855/api/customers")
    );
}

/// THE READ DOOR IS STAMPED (review 1d893c98 B1). Every service behind
/// boss-read-internal mounts the machine gate, and in report mode an
/// unstamped read is tallied as a miss against the window enforcement
/// must earn. So a read-row GET on the host ./sor-read-url names carries
/// the token, while one aimed elsewhere by BOSS_SOR_READ_URL does not —
/// the rule a BOSS_JOBS_URL override already lives under.
#[test]
fn a_read_row_is_stamped_on_the_read_doors_host_and_not_on_an_override() {
    let f = Fixture::new("route-read-only-token");
    let token_dir = f.root.join("machine-token");
    create_dir(&token_dir);
    write_file(&token_dir.join("current"), "stub-token-for-the-test\n");
    let token = token_dir.to_str().expect("utf8");
    let read = tree_read_url();

    for (override_url, want) in [(None, true), (Some("http://read.test"), false)] {
        f.clear_argv();
        let mut env = vec![("BOSS_ACTOR", "agent-x"), ("BOSS_MACHINE_TOKEN_DIR", token)];
        if let Some(u) = override_url {
            env.push(("BOSS_SOR_READ_URL", u));
        }
        let r = f.run(&["GET", "/api/customers?limit=1"], &env);
        assert_eq!(r.code, 0, "{override_url:?}: {}", r.stderr);
        let host = override_url.unwrap_or(&read);
        assert_eq!(
            f.curl_argv().last().map(String::as_str),
            Some(
                format!(
                    "{host}:{}/api/customers?limit=1",
                    boss_ports::prod("customers")
                )
                .as_str()
            )
        );
        assert_eq!(
            f.header("X-Boss-Machine-Token").is_some(),
            want,
            "{host}: stamped should be {want}: {}",
            r.stderr
        );
    }
}

/// READS ONLY. The operator's bound on this change: a read-only row
/// does not widen what a WRITE may reach. Every write method to one is
/// refused with exit 2 before curl, saying why and where a write row is
/// decided — by path and by name alike.
#[test]
fn a_write_to_a_read_only_row_is_refused_before_curl() {
    let f = Fixture::new("route-read-only-write");
    let body = f.root.join("customer.json");
    write_file(&body, r#"{"email":"x@example.test"}"#);
    let body = body.to_str().expect("utf8");
    for method in ["POST", "PUT", "PATCH", "DELETE"] {
        for (path, named) in [("/api/customers/c-1", ""), ("/api/assets/a-1", "assets")] {
            f.clear_argv();
            let r = f.run(
                &[method, path, body],
                &[("BOSS_ACTOR", "agent-x"), ("BOSS_SOR_SERVICE", named)],
            );
            assert_eq!(r.code, 2, "{method} {path}: {}", r.stderr);
            assert!(
                f.curl_argv().is_empty(),
                "{method} {path}: curl must not run"
            );
            assert!(
                r.stderr.contains("READS only") && r.stderr.contains("sor-ports.env"),
                "{method} {path}: the refusal says reads only and where a write row is decided: {}",
                r.stderr
            );
            assert!(!r.stderr.contains("HTTP:"), "no request: {}", r.stderr);
        }
    }
}

/// NO TABLE, NO ROUTING. A copy of the script with sor-url beside it
/// but no forge/sor-ports.env two directories up, and no
/// BOSS_SOR_PORTS, is the door as it was: every path on the base.
#[test]
fn without_a_table_every_path_goes_to_the_base_as_before() {
    let f = Fixture::new("route-no-table");
    let lone = f.root.join("lone");
    create_dir(&lone);
    let script_text = std::fs::read_to_string(repo_root().join(SCRIPT)).expect("read boss-api");
    write_exec(&lone.join("boss-api"), &script_text);
    write_file(&lone.join("sor-url"), "http://lone.test:7900\n");
    lay_roll_lib_beside(&lone);
    let r = Fixture::finish(
        f.command(&lone.join("boss-api")),
        &["GET", "/api/people/accounts"],
        &[("BOSS_ACTOR", "agent-x")],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(
        f.curl_argv().last().map(String::as_str),
        Some("http://lone.test:7900/api/people/accounts"),
        "absent both tables, the path is not routed"
    );
}

/// The roll wait's one definition (backlog 834ddb7c), laid where a copy
/// of the door at `dir/boss-api` looks for it: `dir/../lib/`, the tree's
/// `infra/dev` → `infra/lib` shape.
fn lay_roll_lib_beside(dir: &Path) {
    const ROLL_LIB: &str = "infra/lib/curl-through-a-roll.sh";
    let lib = dir.parent().expect("a parent").join("lib");
    create_dir(&lib);
    write_file(
        &lib.join("curl-through-a-roll.sh"),
        &std::fs::read_to_string(repo_root().join(ROLL_LIB)).expect("read the roll lib"),
    );
}

/// NO LIB, NO REQUEST. The wait lives in infra/lib since backlog
/// 834ddb7c, so a copy of the door without it cannot wait out a roll —
/// and a door that sent anyway would make a rollout minute a failed
/// write again, silently. It refuses before curl, naming the file, the
/// way a copy without its route table does.
#[test]
fn a_copy_without_the_roll_lib_is_refused_before_curl() {
    let f = Fixture::new("no-roll-lib");
    let lone = f.root.join("lone");
    create_dir(&lone);
    let script_text = std::fs::read_to_string(repo_root().join(SCRIPT)).expect("read boss-api");
    write_exec(&lone.join("boss-api"), &script_text);
    write_file(&lone.join("sor-url"), "http://lone.test:7900\n");
    let r = Fixture::finish(
        f.command(&lone.join("boss-api")),
        &["GET", "/api/jobs"],
        &[("BOSS_ACTOR", "agent-x")],
    );
    assert_eq!(r.code, 2, "no lib, no request: {}", r.stderr);
    assert!(f.curl_argv().is_empty(), "curl must not run");
    assert!(
        r.stderr.contains("curl-through-a-roll.sh"),
        "the refusal names the file it could not read: {}",
        r.stderr
    );
}

/// THROUGH A SYMLINK the door finds the lib beside its REAL file, never
/// beside the link — the pod reaches it as /work/tools/bin/boss-api, a
/// symlink into the checkout, and /work/tools/lib does not exist.
#[test]
fn through_a_symlink_the_door_finds_the_lib_beside_its_real_file() {
    let f = Fixture::new("roll-lib-symlink");
    let link = f.root.join("tools-bin");
    create_dir(&link);
    std::os::unix::fs::symlink(repo_root().join(SCRIPT), link.join("boss-api"))
        .expect("link the door");
    let r = Fixture::finish(
        f.command(&link.join("boss-api")),
        &["GET", "/api/jobs"],
        &[
            ("BOSS_ACTOR", "agent-x"),
            ("BOSS_JOBS_URL", "http://sor.test:7900"),
        ],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(
        f.curl_argv().last().map(String::as_str),
        Some("http://sor.test:7900/api/jobs")
    );
}

// -- a rollout is waited out ----------------------------------------------
//
// Backlog 034002b3, twice on 2026-09-23: the stack rolls with strategy
// Recreate, so every converging train takes the jobs API dark for about
// a minute, and every write through this door in that minute exited 7
// (`No route to host`) at once and had to be relaunched by hand. curl
// exits 6 (could not resolve host) and 7 (could not connect: refused,
// no route, network unreachable) only when the request never left —
// those, and only those, are waited out. Every other exit may be a
// request that reached the server (28 is one code for a connect timeout
// AND a transfer that timed out after the body went; 52 and 56 are a
// reply lost after sending), so it is surfaced as before, and an HTTP
// status is an answer.

/// Replace the fixture's curl with one that refuses its first
/// `STUB_REFUSALS` calls the way real curl does under `-sS -w
/// '\n%{http_code}'` (its message on stderr, `000` on stdout, exit
/// `STUB_RC`, default 7), then answers as the plain stub does. With
/// `STUB_OUTLAST_A_SECOND` set, each refusal first takes 1.1 s of real
/// time, through the system's own `sleep` (`command -p`, so not the
/// recording one below). Every
/// call is counted in `curl-calls.txt`. And a `sleep` that records the
/// wait it was asked for in `sleeps.txt` and returns at once, so a test
/// reads the backoff without spending it.
fn install_rolling_stubs(f: &Fixture) -> (PathBuf, PathBuf) {
    let calls = f.root.join("curl-calls.txt");
    let sleeps = f.root.join("sleeps.txt");
    write_exec(
        &f.bin.join("curl"),
        &format!(
            "#!/usr/bin/env bash\n\
             echo call >> '{calls}'\n\
             n=$(wc -l < '{calls}')\n\
             if [ \"$n\" -le \"${{STUB_REFUSALS:-0}}\" ]; then\n\
                 if [ -n \"${{STUB_OUTLAST_A_SECOND:-}}\" ]; then\n\
                     command -p sleep 1.1\n\
                 fi\n\
                 echo 'curl: (7) Failed to connect to sor.test port 7900: No route to host' >&2\n\
                 printf '\\n000'\n\
                 exit \"${{STUB_RC:-7}}\"\n\
             fi\n\
             : > \"$STUB_ARGV\"\n\
             for a in \"$@\"; do printf '%s\\n' \"$a\" >> \"$STUB_ARGV\"; done\n\
             printf '%s\\n%s' \"${{STUB_BODY:-}}\" \"${{STUB_CODE:-200}}\"\n",
            calls = calls.display()
        ),
    );
    write_exec(
        &f.bin.join("sleep"),
        &format!(
            "#!/usr/bin/env bash\necho \"$1\" >> '{}'\n",
            sleeps.display()
        ),
    );
    (calls, sleeps)
}

fn count_lines(path: &Path) -> usize {
    std::fs::read_to_string(path)
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

#[test]
fn a_refused_connect_is_waited_out_and_the_write_goes_out_once_the_api_is_back() {
    let f = Fixture::new("roll-waited");
    let (calls, sleeps) = install_rolling_stubs(&f);
    let body = f.root.join("body.json");
    write_file(&body, r#"{"kind":"backlog-item"}"#);
    let r = f.run(
        &["POST", "/api/jobs", body.to_str().expect("utf8")],
        &[
            ("STUB_REFUSALS", "2"),
            ("STUB_BODY", r#"{"id":"j1"}"#),
            ("BOSS_ACTOR", "agent-x"),
        ],
    );
    assert_eq!(
        r.code, 0,
        "the write lands once the API is back: {}",
        r.stderr
    );
    assert_eq!(
        r.stdout, "{\"id\":\"j1\"}\n",
        "only the answer reaches stdout"
    );
    assert_eq!(count_lines(&calls), 3, "two refusals, then the write");
    let said = r
        .stderr
        .lines()
        .filter(|l| {
            l.contains("the jobs API is not answering (a rollout?) — retrying POST /api/jobs")
        })
        .count();
    assert_eq!(said, 2, "one visible line per wait:\n{}", r.stderr);
    assert_eq!(
        std::fs::read_to_string(&sleeps).unwrap_or_default(),
        "2\n4\n",
        "the waits back off"
    );
    assert!(r.stderr.ends_with("HTTP:200\n"), "{}", r.stderr);
}

#[test]
fn a_dns_failure_is_waited_out_too() {
    // exit 6: the name did not resolve, so nothing was sent.
    let f = Fixture::new("roll-dns");
    let (calls, _) = install_rolling_stubs(&f);
    let r = f.run(
        &["GET", "/api/jobs"],
        &[
            ("STUB_REFUSALS", "1"),
            ("STUB_RC", "6"),
            ("BOSS_ACTOR", "agent-x"),
        ],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(count_lines(&calls), 2);
}

#[test]
fn only_a_request_that_never_left_is_retried() {
    for rc in ["28", "52", "56"] {
        let f = Fixture::new(&format!("roll-not-{rc}"));
        let (calls, sleeps) = install_rolling_stubs(&f);
        let body = f.root.join("body.json");
        write_file(&body, "{}");
        let r = f.run(
            &["POST", "/api/jobs", body.to_str().expect("utf8")],
            &[
                ("STUB_REFUSALS", "1"),
                ("STUB_RC", rc),
                ("BOSS_ACTOR", "agent-x"),
            ],
        );
        assert_eq!(
            r.code.to_string(),
            rc,
            "curl {rc} may have reached the server — surfaced with curl's own code: {}",
            r.stderr
        );
        assert_eq!(count_lines(&calls), 1, "curl {rc} is asked once");
        assert_eq!(count_lines(&sleeps), 0, "curl {rc} is not waited out");
        assert!(!r.stderr.contains("retrying"), "{}", r.stderr);
    }
    // And an HTTP status is an answer, even a 503.
    let f = Fixture::new("roll-not-503");
    let (calls, _) = install_rolling_stubs(&f);
    let r = f.run(
        &["GET", "/api/jobs"],
        &[("STUB_CODE", "503"), ("BOSS_ACTOR", "agent-x")],
    );
    assert_eq!(r.code, 1, "{}", r.stderr);
    assert_eq!(count_lines(&calls), 1, "a 503 is asked once");
}

#[test]
fn a_roll_that_outlasts_the_window_fails_naming_the_elapsed_time() {
    // BOSS_SOR_WAIT_SECONDS is the window; 0 means the first refusal is
    // already past it, so the failure is read without spending one.
    let f = Fixture::new("roll-outlasted");
    let (calls, sleeps) = install_rolling_stubs(&f);
    let r = f.run(
        &["GET", "/api/jobs"],
        &[
            ("STUB_REFUSALS", "99"),
            ("STUB_OUTLAST_A_SECOND", "1"),
            ("BOSS_SOR_WAIT_SECONDS", "0"),
            ("BOSS_ACTOR", "agent-x"),
        ],
    );
    assert_eq!(r.code, 7, "curl's own code, as before: {}", r.stderr);
    assert_eq!(count_lines(&calls), 1);
    assert_eq!(count_lines(&sleeps), 0);
    // The elapsed time is bash's whole-second $SECONDS, so it reads the
    // wall clock's boundaries, not the call's length: a sub-second
    // refusal names 0s or 1s (the gate flaked on an exact "0s",
    // 2026-09-23). So the refusal takes MORE than a second of real
    // time, and any whole-second reading of an interval over one second
    // is at least 1 — the reading is the real elapsed time, never a
    // constant, and no boundary decides it.
    //
    // It used to spin until `printf '%(%s)T' -1` changed second, and
    // that raced too (backlog b53dca8c, gate 2510ac65, 2026-09-24,
    // "for 0s" again): printf reads the kernel's COARSE time(), which
    // lags the gettimeofday() $SECONDS reads by up to a tick after
    // every boundary. A $SECONDS read in those few milliseconds and a
    // stub whose first printf still saw the old second ended the spin
    // inside the SAME $SECONDS second. Forced at the boundary with the
    // stub as a function, the old spin read "for 0s" 10 times in 10;
    // a spin that watches a different clock from the one it is meant
    // to advance proves nothing, so the stub now waits rather than
    // watches.
    const NAMED: &str = "boss-api: GET /api/jobs: the jobs API refused every connection for ";
    let elapsed = r.stderr.split_once(NAMED).and_then(|(_, rest)| {
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        rest[digits.len()..]
            .starts_with("s (1 attempts")
            .then(|| digits.parse::<u64>().ok())
            .flatten()
    });
    assert!(
        elapsed.is_some_and(|s| s >= 1)
            && r.stderr.contains("waited out for up to 0s")
            && r.stderr.contains("nothing was sent"),
        "the failure names the call, the elapsed time, and that relaunching is safe:\n{}",
        r.stderr
    );
    assert!(
        r.stdout.is_empty(),
        "no body on a refused connect: {:?}",
        r.stdout
    );

    // A window that is not a whole number of seconds is refused before
    // curl runs, rather than read as zero or as forever.
    let r = f.run(
        &["GET", "/api/jobs"],
        &[("BOSS_SOR_WAIT_SECONDS", "2m"), ("BOSS_ACTOR", "agent-x")],
    );
    assert_eq!(r.code, 2, "{}", r.stderr);
    assert!(r.stderr.contains("BOSS_SOR_WAIT_SECONDS"), "{}", r.stderr);
    assert_eq!(count_lines(&calls), 1, "curl did not run");
}

#[test]
fn the_default_window_is_two_minutes() {
    let text = std::fs::read_to_string(repo_root().join(SCRIPT)).expect("read boss-api");
    assert!(
        text.contains("WAIT_WINDOW=${BOSS_SOR_WAIT_SECONDS:-120}"),
        "the door waits out a roll for 120s, the same window as the CLI's ROLL_WAIT"
    );
}
