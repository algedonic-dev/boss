//! `infra/lib/secret-header.sh` — a secret HTTP header reaches curl in a
//! 0600 file, never in curl's argv. RUN, not read: each test sources the
//! lib into a real shell — bash AND dash, because `infra/ops/ops-runner.sh`
//! is `#!/bin/sh` — and hands the result to a stub `curl` on PATH that
//! records its argv and reads the header file the way curl's `-H @file`
//! does.
//!
//! THE DEFECT (backlog 5f3ad356, 2026-09-27; the sweep on cfa678e9,
//! 2026-09-28): `curl -H "x-boss-machine-token: $BOSS_MACHINE_TOKEN"` put
//! the token in curl's command line — world-readable in `ps` and
//! /proc/<pid>/cmdline while curl runs — at 28 sites in 13 files, plus
//! four more secrets under names a `*TOKEN*` grep missed. The lib is the
//! one way round it every site now takes; this file pins what the lib
//! promises (its header says each one and why), and
//! `a_secret_header_rides_in_a_file.rs` pins the lint that keeps the argv
//! shape out.

use boss_testing::{create_dir, repo_root, scratch_dir, write_exec, write_file};
use std::path::{Path, PathBuf};
use std::process::Command;

const LIB: &str = "infra/lib/secret-header.sh";

/// Not a real token: a string no argv, file name or path would hold by
/// accident, so "absent from argv" means what it says.
const SECRET: &str = "s3cret-value-7f1c9e";

struct Fixture {
    root: PathBuf,
    bin: PathBuf,
    /// ${TMPDIR} for the driver: the fallback directory lives here, so a
    /// test can see what was left behind.
    tmp: PathBuf,
}

struct Run {
    code: i32,
    stderr: String,
}

impl Fixture {
    fn new(tag: &str) -> Fixture {
        let root = scratch_dir(&format!("secret-header-{tag}"));
        let bin = root.join("bin");
        let tmp = root.join("tmp");
        create_dir(&bin);
        create_dir(&tmp);
        // curl: every argument on its own line to argv.txt; for each
        // `-H @file`, the file's text to headers.txt and the modes of
        // the file and its directory to modes.txt — read while the
        // caller is still running, as curl reads it.
        write_exec(
            &bin.join("curl"),
            "#!/bin/sh\n\
             printf '%s\\n' \"$@\" > \"$STUB_DIR/argv.txt\"\n\
             prev=\n\
             for a in \"$@\"; do\n\
                 if [ \"$prev\" = -H ]; then\n\
                     case \"$a\" in\n\
                         @*) f=${a#@}\n\
                             cat \"$f\" >> \"$STUB_DIR/headers.txt\"\n\
                             stat -c '%a' \"$f\" \"$(dirname \"$f\")\" >> \"$STUB_DIR/modes.txt\"\n\
                             printf '%s\\n' \"$f\" >> \"$STUB_DIR/paths.txt\" ;;\n\
                     esac\n\
                 fi\n\
                 prev=$a\n\
             done\n",
        );
        Fixture { root, bin, tmp }
    }

    /// Run `body` under `shell` with the lib sourced first.
    fn run(&self, shell: &str, body: &str, env: &[(&str, &str)]) -> Run {
        let driver = self.root.join(format!("driver-{shell}.sh"));
        write_file(
            &driver,
            &format!(". \"{}\"\n{body}", repo_root().join(LIB).display()),
        );
        let mut cmd = Command::new(shell);
        cmd.arg(&driver)
            .current_dir(&self.root)
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
            .env_remove("RUNTIME_DIRECTORY")
            // The machine token's inputs come from the test, never from
            // the shell that ran the suite: a directory that does not
            // exist, no host list, no rendered sor.env.
            .env("BOSS_MACHINE_TOKEN_DIR", self.root.join("no-token-dir"))
            .env_remove("BOSS_MACHINE_TOKEN_HOSTS")
            .env("BOSS_SOR_ENV", self.root.join("no-sor.env"))
            .env_remove("BOSS_MACHINE_TOKEN");
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd
            .output()
            .unwrap_or_else(|e| panic!("run {shell} {}: {e}", driver.display()));
        Run {
            code: out.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.root.join(name)).unwrap_or_default()
    }

    /// Entries under `dir`, for "nothing was left behind".
    fn entries(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .map(|rd| {
                rd.filter_map(Result::ok)
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default()
    }
}

const SHELLS: [&str; 2] = ["bash", "dash"];

/// The whole promise in one run: curl is handed `-H @<file>`, the token
/// is nowhere in its argv, the file holds exactly the header line, the
/// file is 0600 in a 0700 directory, and the directory is gone when the
/// script ends.
#[test]
fn a_secret_line_reaches_curl_as_a_file_never_as_an_argument() {
    for shell in SHELLS {
        let f = Fixture::new(&format!("argv-{shell}"));
        let r = f.run(
            shell,
            "secret_header MT_HDR ${SECRET:+\"x-boss-machine-token: $SECRET\"}\n\
             curl -fsS ${MT_HDR:+-H \"$MT_HDR\"} http://sor.test/api/jobs\n",
            &[],
        );
        assert_eq!(r.code, 0, "{shell}: {}", r.stderr);
        let argv = f.read("argv.txt");
        assert!(
            !argv.contains(SECRET),
            "{shell}: the token must not be in curl's argv:\n{argv}"
        );
        assert!(
            argv.lines().any(|a| a.starts_with('@')),
            "{shell}: curl must be handed -H @<file>:\n{argv}"
        );
        assert_eq!(
            f.read("headers.txt"),
            format!("x-boss-machine-token: {SECRET}\n"),
            "{shell}: the file holds exactly the header line"
        );
        assert_eq!(
            f.read("modes.txt"),
            "600\n700\n",
            "{shell}: the file is 0600 and its directory 0700"
        );
        let path = f.read("paths.txt");
        let path = Path::new(path.trim());
        assert!(
            path.starts_with(&f.tmp),
            "{shell}: with no RUNTIME_DIRECTORY the file lives under TMPDIR: {}",
            path.display()
        );
        assert!(
            Fixture::entries(&f.tmp).is_empty(),
            "{shell}: the directory must be removed at exit; left: {:?}",
            Fixture::entries(&f.tmp)
        );
    }
}

/// Most callers already `trap 'rm -rf "$workdir"' EXIT`. The lib's
/// cleanup is chained IN FRONT of it, not over it: the caller's trap
/// still runs, still sees the status the script exited with, and the
/// script still exits with it.
///
/// UNDER `set -e` TOO (the adversarial review of car 676f4cdd, HOLD F1):
/// the chained trap restored the status with `(exit $rc)`, and under
/// errexit a non-zero `(exit rc)` STOPPED the trap there, so the
/// caller's own trap never ran — read-publish-checks, publish-github-pr
/// and credential-deposit leaked their workdirs on every refusal, and
/// the maintenance wrap's `on_exit` never folded its claimed ledger rows
/// back. Three endings, in both shells: a plain `exit 3`, `exit 3` under
/// `set -e`, and a failing command under `set -e`.
#[test]
fn the_callers_exit_trap_still_runs_and_sees_the_exit_status() {
    let endings = [
        ("plain", "", "exit 3", 3),
        ("errexit-exit", "set -e\n", "exit 3", 3),
        ("errexit-false", "set -e\n", "false", 1),
    ];
    for shell in SHELLS {
        for (tag, prelude, ending, want) in endings {
            let f = Fixture::new(&format!("trap-{shell}-{tag}"));
            let w = f.root.join("caller-workdir");
            create_dir(&w);
            let r = f.run(
                shell,
                &format!(
                    "{prelude}trap 'echo \"caller trap saw $?\" > \"$STUB_DIR/trap.txt\"; rm -rf \"{w}\"' EXIT\n\
                     secret_header H \"Authorization: Bearer $SECRET\"\n\
                     curl -H \"$H\" http://x/\n\
                     {ending}\n\
                     echo unreachable > \"$STUB_DIR/after.txt\"\n",
                    w = w.display()
                ),
                &[],
            );
            assert_eq!(
                r.code, want,
                "{shell} {tag}: the script's own status survives: {}",
                r.stderr
            );
            assert_eq!(
                f.read("trap.txt"),
                format!("caller trap saw {want}\n"),
                "{shell} {tag}: the caller's trap runs and sees the exit status"
            );
            assert!(!w.exists(), "{shell} {tag}: the caller's own cleanup ran");
            assert!(
                Fixture::entries(&f.tmp).is_empty(),
                "{shell} {tag}: and so did the lib's: {:?}",
                Fixture::entries(&f.tmp)
            );
        }
    }
}

/// Under a unit with `RuntimeDirectory=` the file lives inside
/// `$RUNTIME_DIRECTORY` (tmpfs, removed by systemd when the unit stops),
/// never under TMPDIR. systemd may hand a colon-separated list; the
/// first entry is the one.
#[test]
fn under_a_runtime_directory_the_file_lives_there() {
    for shell in SHELLS {
        let f = Fixture::new(&format!("runtime-{shell}"));
        let run = f.root.join("run");
        create_dir(&run);
        let list = format!("{}:{}", run.display(), f.root.join("second").display());
        let r = f.run(
            shell,
            "secret_header H \"x-boss-machine-token: $SECRET\"\ncurl -H \"$H\" http://x/\n",
            &[("RUNTIME_DIRECTORY", &list)],
        );
        assert_eq!(r.code, 0, "{shell}: {}", r.stderr);
        let path = f.read("paths.txt");
        assert!(
            Path::new(path.trim()).starts_with(&run),
            "{shell}: the file must live in RUNTIME_DIRECTORY: {path}"
        );
        assert!(
            Fixture::entries(&f.tmp).is_empty(),
            "{shell}: nothing under TMPDIR"
        );
        assert!(
            Fixture::entries(&run).is_empty(),
            "{shell}: removed at exit: {:?}",
            Fixture::entries(&run)
        );
    }
}

/// A host whose system of record needs no token sets none: no line, no
/// header, no directory — what `${BOSS_MACHINE_TOKEN:+-H …}` did.
#[test]
fn no_line_sends_no_header_and_writes_nothing() {
    for shell in SHELLS {
        let f = Fixture::new(&format!("empty-{shell}"));
        let r = f.run(
            shell,
            "set -u\n\
             secret_header MT_HDR ${NO_TOKEN_HERE:+\"x-boss-machine-token: $NO_TOKEN_HERE\"}\n\
             curl ${MT_HDR:+-H \"$MT_HDR\"} http://x/\n",
            &[],
        );
        assert_eq!(r.code, 0, "{shell}: {}", r.stderr);
        assert!(
            !f.read("argv.txt").lines().any(|a| a == "-H"),
            "{shell}: no header: {}",
            f.read("argv.txt")
        );
        assert!(
            Fixture::entries(&f.tmp).is_empty(),
            "{shell}: no directory was made"
        );
    }
}

/// curl reads a header file line by line, so a second line would be a
/// second header: refused (2), and nothing is written.
#[test]
fn a_line_break_in_the_header_is_refused() {
    for shell in SHELLS {
        let f = Fixture::new(&format!("newline-{shell}"));
        let r = f.run(
            shell,
            "secret_header H \"$(printf 'x-boss-machine-token: a\\nx-boss-user: b')\"\n\
             echo \"rc=$?\" > \"$STUB_DIR/rc.txt\"\n",
            &[],
        );
        assert_eq!(r.code, 0, "{shell}: {}", r.stderr);
        assert_eq!(f.read("rc.txt"), "rc=2\n", "{shell}: {}", r.stderr);
        assert!(r.stderr.contains("line break"), "{shell}: {}", r.stderr);
        assert!(Fixture::entries(&f.tmp).is_empty(), "{shell}");
    }
}

/// bash lists the PARENT's traps inside a subshell although none of them
/// will run there, so chaining a first call made in a subshell would hand
/// the subshell's exit the parent's `rm -rf "$workdir"`. Refused instead,
/// and the parent's trap runs once, at the parent's exit.
#[test]
fn a_first_call_in_a_bash_subshell_is_refused_and_steals_no_trap() {
    let f = Fixture::new("subshell");
    let r = f.run(
        "bash",
        "trap 'echo parent-trap >> \"$STUB_DIR/order.txt\"' EXIT\n\
         ( secret_header H \"x-boss-machine-token: $SECRET\"; echo \"sub rc=$?\" >> \"$STUB_DIR/order.txt\" )\n\
         echo parent-continues >> \"$STUB_DIR/order.txt\"\n",
        &[],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(
        f.read("order.txt"),
        "sub rc=1\nparent-continues\nparent-trap\n",
        "{}",
        r.stderr
    );
    assert!(r.stderr.contains("subshell"), "{}", r.stderr);
}

/// The chore pair runs from the boss image too (`/usr/local/bin`, and the
/// playground crawl's `/tools`, which copies `lib/` whole), and each
/// sources the lib from `lib/` beside itself and refuses without it. The
/// gate never builds the image, so a missing COPY is caught here or on
/// the cluster (the launcher's same failure bricked production,
/// 2026-09-05).
#[test]
fn the_image_carries_the_lib_beside_the_chore_pair() {
    const DOCKERFILE: &str = "infra/oss-quickstart/Dockerfile";
    for script in ["infra/boss-step.sh", "infra/boss-maintenance-wrap.sh"] {
        let text = std::fs::read_to_string(repo_root().join(script)).expect("read the script");
        assert!(
            text.contains("SECRET_LIB=\"$(dirname \"$0\")/lib/secret-header.sh\""),
            "{script} finds the lib beside itself, as it finds boss-api-curl.sh"
        );
    }
    let dockerfile = std::fs::read_to_string(repo_root().join(DOCKERFILE)).expect("read");
    let bin_dir = dockerfile
        .lines()
        .filter(|l| l.starts_with("COPY "))
        .find(|l| l.split_whitespace().any(|w| w == "infra/boss-step.sh"))
        .and_then(|l| l.split_whitespace().last())
        .map(|d| d.trim_end_matches('/').to_string())
        .expect("the Dockerfile COPYs boss-step.sh");
    let want = format!("{bin_dir}/lib/secret-header.sh");
    assert!(
        dockerfile.lines().any(|l| {
            let w: Vec<&str> = l.split_whitespace().collect();
            w.first() == Some(&"COPY") && w.contains(&LIB) && w.last() == Some(&want.as_str())
        }),
        "{DOCKERFILE} must COPY {LIB} to {want}, beside the chore pair"
    );
}

/// A later call in a subshell only rewrites its file in the directory the
/// first call made — the shape of a curl inside `$(…)`.
#[test]
fn a_later_call_in_a_subshell_reuses_the_directory() {
    for shell in SHELLS {
        let f = Fixture::new(&format!("reuse-{shell}"));
        let r = f.run(
            shell,
            "secret_header H \"x-boss-machine-token: first\"\n\
             out=$(secret_header H \"x-boss-machine-token: $SECRET\" && curl -H \"$H\" http://x/ && echo ok)\n\
             [ \"$out\" = ok ] || exit 9\n\
             curl -H \"$H\" http://x/\n",
            &[],
        );
        assert_eq!(r.code, 0, "{shell}: {}", r.stderr);
        let paths = f.read("paths.txt");
        let lines: Vec<&str> = paths.lines().collect();
        assert_eq!(lines.len(), 2, "{shell}: {paths}");
        assert_eq!(lines[0], lines[1], "{shell}: one file per VAR: {paths}");
        assert_eq!(
            f.read("headers.txt"),
            format!("x-boss-machine-token: {SECRET}\nx-boss-machine-token: {SECRET}\n"),
            "{shell}: the subshell's rewrite is what the parent's next curl reads"
        );
        assert!(Fixture::entries(&f.tmp).is_empty(), "{shell}");
    }
}

// ---------------------------------------------------------------------
// machine_token_header — the shell senders' ONE reader of the estate
// machine token (design 6805c764 car 4; backlog 1876bbdb INFO-6, INFO-7;
// the car-4 mount checklist on backlog 2710c8fc). Until car 4 every
// script stamped `$BOSS_MACHINE_TOKEN` — an env var the mount replaces —
// onto whatever `$BASE` it was handed, with no host check, while
// boss-core read the `current` slot of a mounted DIRECTORY and stamped
// only estate hosts.
// ---------------------------------------------------------------------

/// A mounted-Secret-shaped directory under the fixture, holding `current`.
fn token_dir(f: &Fixture, current: &str) -> PathBuf {
    let dir = f.root.join("machine-token");
    create_dir(&dir);
    write_file(&dir.join("current"), current);
    dir
}

/// The `current` slot of `BOSS_MACHINE_TOKEN_DIR`, trimmed, rides to a
/// loopback URL in a header FILE, never in argv, and nothing is said.
#[test]
fn the_machine_token_is_read_from_the_current_slot_of_its_mount() {
    for shell in SHELLS {
        let f = Fixture::new(&format!("mt-current-{shell}"));
        let dir = token_dir(&f, &format!("  {SECRET}\n"));
        let r = f.run(
            shell,
            "set -eu\n\
             machine_token_header MT_HDR http://127.0.0.1:7900\n\
             curl ${MT_HDR:+-H \"$MT_HDR\"} http://127.0.0.1:7900/api/jobs\n",
            &[("BOSS_MACHINE_TOKEN_DIR", dir.to_str().unwrap())],
        );
        assert_eq!(r.code, 0, "{shell}: {}", r.stderr);
        assert_eq!(
            f.read("headers.txt"),
            format!("x-boss-machine-token: {SECRET}\n"),
            "{shell}: the header carries the trimmed `current` slot"
        );
        assert!(!f.read("argv.txt").contains(SECRET), "{shell}: never argv");
        assert_eq!(r.stderr, "", "{shell}: a stamped call says nothing");
        assert!(Fixture::entries(&f.tmp).is_empty(), "{shell}: cleaned up");
    }
}

/// The env var is not a source any longer (design choice 2: two sources
/// for one fact drift). Exported with no mount, it sends nothing.
#[test]
fn the_env_var_is_not_a_source() {
    for shell in SHELLS {
        let f = Fixture::new(&format!("mt-env-{shell}"));
        let r = f.run(
            shell,
            "set -eu\n\
             machine_token_header MT_HDR http://127.0.0.1:7900\n\
             curl ${MT_HDR:+-H \"$MT_HDR\"} http://127.0.0.1:7900/\n",
            &[("BOSS_MACHINE_TOKEN", SECRET)],
        );
        assert_eq!(r.code, 0, "{shell}: {}", r.stderr);
        assert!(
            !f.read("argv.txt").lines().any(|a| a == "-H"),
            "{shell}: no header from the env var: {}",
            f.read("argv.txt")
        );
        assert!(Fixture::entries(&f.tmp).is_empty(), "{shell}");
    }
}

/// No token is the state every host is in until the broker's first mint:
/// no directory, a directory without `current` (an optional Secret that
/// is absent mounts EMPTY), a blank `current` — no header, nothing
/// written, nothing said, exit 0.
#[test]
fn no_token_sends_no_header_and_says_nothing() {
    for shell in SHELLS {
        let f = Fixture::new(&format!("mt-none-{shell}"));
        let empty = f.root.join("empty-mount");
        create_dir(&empty);
        let blank = f.root.join("blank-mount");
        create_dir(&blank);
        write_file(&blank.join("current"), "  \n");
        for dir in [f.root.join("absent"), empty, blank] {
            let r = f.run(
                shell,
                "set -eu\n\
                 machine_token_header MT_HDR http://127.0.0.1:7900\n\
                 curl ${MT_HDR:+-H \"$MT_HDR\"} http://127.0.0.1:7900/\n",
                &[("BOSS_MACHINE_TOKEN_DIR", dir.to_str().unwrap())],
            );
            assert_eq!(r.code, 0, "{shell} {}: {}", dir.display(), r.stderr);
            assert_eq!(r.stderr, "", "{shell} {}: silent", dir.display());
            assert!(
                !f.read("argv.txt").lines().any(|a| a == "-H"),
                "{shell} {}: no header",
                dir.display()
            );
            assert!(Fixture::entries(&f.tmp).is_empty(), "{shell}");
        }
    }
}

/// What boss-core's reader refuses, this one refuses the same way — sent
/// WITHOUT the token, said on stderr, never an exit that would stop the
/// caller's work (a chore's packet, a runner's answer): the old FILE
/// layout at the directory's path (what infra/dev/boss-api read until
/// INFO-6), a `current` larger than 4096 bytes (MAX_SLOT_BYTES), and a
/// `current` holding a line break, which no header can carry.
#[test]
fn a_slot_core_refuses_is_sent_without_and_said() {
    for shell in SHELLS {
        let f = Fixture::new(&format!("mt-bad-{shell}"));
        let file = f.root.join("old-layout-token");
        write_file(&file, &format!("{SECRET}\n"));
        let big = f.root.join("big");
        create_dir(&big);
        write_file(&big.join("current"), &"a".repeat(4097));
        let two = f.root.join("two-lines");
        create_dir(&two);
        write_file(
            &two.join("current"),
            &format!("{SECRET}\nx-boss-user: evil\n"),
        );
        for (dir, says) in [
            (&file, "not a directory"),
            (&big, "larger than 4096 bytes"),
            (&two, "line break"),
        ] {
            let r = f.run(
                shell,
                "set -eu\n\
                 machine_token_header MT_HDR http://127.0.0.1:7900\n\
                 curl ${MT_HDR:+-H \"$MT_HDR\"} http://127.0.0.1:7900/\n",
                &[("BOSS_MACHINE_TOKEN_DIR", dir.to_str().unwrap())],
            );
            assert_eq!(r.code, 0, "{shell} {says}: {}", r.stderr);
            assert!(
                r.stderr.contains(says),
                "{shell}: says {says}: {}",
                r.stderr
            );
            assert!(!r.stderr.contains(SECRET), "{shell}: never the value");
            assert!(
                !f.read("argv.txt").lines().any(|a| a == "-H"),
                "{shell} {says}: no header"
            );
        }
    }
}

/// The same host rule boss-core's clients take (backlog 2ee29275, F1;
/// 1876bbdb INFO-7): loopback, and the hosts and `.namespace` suffixes
/// the list names — held to `machine_token::Hosts` on this table's rows
/// (CLAUDE.md §9a), the rows infra/dev/boss-api's own equality test holds
/// the door to. Off the table the two can differ; the lib's header names
/// how, as review ef2da426 F4 measured it against curl. A withheld call says so once, naming the scheme and host and
/// never the path, the query or the userinfo.
#[test]
fn the_token_rides_only_to_an_estate_host_the_way_boss_core_decides() {
    const PATH: &str = "/api/jobs?state=q-secret";
    let list = "record.example.net, 192.0.2.34";
    let prod = ".boss.svc.cluster.local";
    let cases = [
        ("http://127.0.0.1:7900", ""),
        ("http://localhost:7900", ""),
        ("http://[::1]:7900", ""),
        ("http://boss-jobs-internal.boss.svc.cluster.local:7900", ""),
        (
            "http://boss-jobs-internal.boss.svc.cluster.local:7900",
            prod,
        ),
        ("http://BOSS-JOBS.boss.svc.cluster.local.:7900", prod),
        (
            "http://boss-jobs-internal.boss-playground.svc.cluster.local:7900",
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
        ("http://evil.com#@127.0.0.1", ""),
        ("http://evil.com?@127.0.0.1:7900", ""),
        (
            "http://evil.com?x=@boss-jobs-internal.boss.svc.cluster.local",
            prod,
        ),
        ("http://127.0.0.999:7900", ""),
        ("http://127.0.0.256:7900", ""),
        ("http://127.1000.0.1:7900", ""),
        ("http://127.255.255.255:7900", ""),
        ("http://127.0.0.1.evil.example:7900", ""),
    ];
    for shell in SHELLS {
        let f = Fixture::new(&format!("mt-hosts-{shell}"));
        let dir = token_dir(&f, SECRET);
        for (base, listed) in cases {
            let _ = std::fs::remove_file(f.root.join("headers.txt"));
            let url = format!("{base}{PATH}");
            let r = f.run(
                shell,
                "set -eu\n\
                 machine_token_header MT_HDR \"$URL\"\n\
                 curl ${MT_HDR:+-H \"$MT_HDR\"} \"$URL\"\n",
                &[
                    ("BOSS_MACHINE_TOKEN_DIR", dir.to_str().unwrap()),
                    ("BOSS_MACHINE_TOKEN_HOSTS", listed),
                    ("URL", &url),
                ],
            );
            assert_eq!(r.code, 0, "{shell} {base}: {}", r.stderr);
            let core = boss_core::machine_token::Hosts::parse(listed).allows_url(&url);
            let lib = f.read("headers.txt").contains(SECRET);
            assert_eq!(
                lib, core,
                "{shell} {base} (list {listed:?}): the lib stamped={lib}, boss-core allows={core}"
            );
            if lib {
                assert_eq!(r.stderr, "", "{shell} {base}: stamped says nothing");
            } else {
                let scheme = base.split("://").next().unwrap();
                assert!(
                    r.stderr.contains("machine token withheld")
                        && r.stderr.contains(&format!("{scheme}://")),
                    "{shell} {base}: withheld says so, naming the scheme: {}",
                    r.stderr
                );
                for never in ["q-secret", "/api/jobs", "operator@", SECRET] {
                    assert!(
                        !r.stderr.contains(never),
                        "{shell} {base}: never {never}: {}",
                        r.stderr
                    );
                }
            }
        }
    }
}

/// A URL THAT DOES NOT BEGIN WITH `http://` OR `https://` IS WITHHELD,
/// WHATEVER IT HOLDS LATER (backlog 50708d76, F1 of review 927f8602).
/// `machine_token_host` stripped `${1#*://}` — up to the first `://`
/// ANYWHERE — so a URL with no scheme that held `://` further on was
/// judged by the host after it, while curl guesses http and connects to
/// the FIRST host. The first six rows are the reviewer's, each measured
/// against curl 7.88.1 through a loopback stub with the list naming
/// `sor.invalid`: every one read ADMIT and curl delivered the header to
/// `evil.invalid`. Reaching them takes a base URL with no scheme, which
/// no managed host renders, so the exposure is a hand-set or mistyped
/// `BOSS_JOBS_URL` — the hand run this rule exists for.
///
/// The rule now reads an authority only after a leading `http://` or
/// `https://`, and anything else has NO host: withheld, and said without
/// one byte of the URL (its first word may be a path, a query or a
/// userinfo). boss-core withholds every one of these rows too —
/// `Url::parse` refuses a relative URL and finds no host behind a scheme
/// that is really `host:port` — and the first nine are held equal to it.
/// The last three are a NAMED difference, the safe way round: core would
/// admit a loopback or listed host behind any scheme, and its clients can
/// only speak http(s); the lib withholds, because curl speaks twenty.
#[test]
fn a_url_with_no_leading_http_scheme_is_withheld_whatever_it_holds_later() {
    const PATH: &str = "/api/jobs?state=q-secret";
    let list = "sor.invalid";
    // (base, held equal to boss-core)
    let cases = [
        ("evil.invalid/x://sor.invalid", true),
        ("evil.invalid/x://127.0.0.1", true),
        ("evil.invalid:80/x://sor.invalid:7900", true),
        ("evil.invalid?x=://sor.invalid", true),
        ("evil.invalid#://sor.invalid", true),
        ("user@evil.invalid/://localhost", true),
        ("sor.invalid:7900", true),
        ("127.0.0.1:7900", true),
        ("localhost", true),
        ("ftp://127.0.0.1", false),
        ("ws://sor.invalid:7900", false),
        ("evil.invalid://sor.invalid", false),
    ];
    for shell in SHELLS {
        let f = Fixture::new(&format!("mt-no-scheme-{shell}"));
        let dir = token_dir(&f, SECRET);
        // The control, on the same fixture: the rule still stamps.
        for base in ["http://sor.invalid:7900", "HTTPS://Sor.Invalid"] {
            let _ = std::fs::remove_file(f.root.join("headers.txt"));
            let url = format!("{base}{PATH}");
            let r = f.run(
                shell,
                "set -eu\n\
                 machine_token_header MT_HDR \"$URL\"\n\
                 curl ${MT_HDR:+-H \"$MT_HDR\"} \"$URL\"\n",
                &[
                    ("BOSS_MACHINE_TOKEN_DIR", dir.to_str().unwrap()),
                    ("BOSS_MACHINE_TOKEN_HOSTS", list),
                    ("URL", &url),
                ],
            );
            assert_eq!(r.code, 0, "{shell} {base}: {}", r.stderr);
            assert!(
                f.read("headers.txt").contains(SECRET),
                "{shell} {base}: a listed host behind a leading scheme is stamped"
            );
        }
        for (base, as_core) in cases {
            let _ = std::fs::remove_file(f.root.join("headers.txt"));
            let _ = std::fs::remove_file(f.root.join("judged.txt"));
            let url = format!("{base}{PATH}");
            let r = f.run(
                shell,
                "set -eu\n\
                 machine_token_header MT_HDR \"$URL\"\n\
                 machine_gate_value_header GV_HDR \"$URL\" fake-held-value 2>/dev/null\n\
                 if machine_token_admits \"$URL\"; then a=admit; else a=withhold; fi\n\
                 printf '%s [%s] [%s]\\n' \"$a\" \"$GV_HDR\" \"$(machine_token_host \"$URL\")\" > \"$STUB_DIR/judged.txt\"\n\
                 curl ${MT_HDR:+-H \"$MT_HDR\"} \"$URL\"\n",
                &[
                    ("BOSS_MACHINE_TOKEN_DIR", dir.to_str().unwrap()),
                    ("BOSS_MACHINE_TOKEN_HOSTS", list),
                    ("URL", &url),
                ],
            );
            assert_eq!(r.code, 0, "{shell} {base}: {}", r.stderr);
            assert!(
                !f.read("headers.txt").contains(SECRET),
                "{shell} {base}: stamped by a host curl does not connect to"
            );
            assert_eq!(
                f.read("judged.txt"),
                "withhold [] []\n",
                "{shell} {base}: the judgement alone, the value door and the host read agree"
            );
            if as_core {
                assert!(
                    !boss_core::machine_token::Hosts::parse(list).allows_url(&url),
                    "{base}: boss-core admits a URL the lib withholds — the row is no longer equal"
                );
            }
            assert!(
                r.stderr.contains("machine token withheld")
                    && r.stderr.contains("does not begin with http:// or https://"),
                "{shell} {base}: withheld says why: {}",
                r.stderr
            );
            for never in [
                "evil",
                "sor.invalid",
                "user@",
                "q-secret",
                "/api/jobs",
                "/x",
                SECRET,
            ] {
                assert!(
                    !r.stderr.contains(never),
                    "{shell} {base}: never {never}: {}",
                    r.stderr
                );
            }
        }
    }
}

/// UNSET, the list is the rendered sor.env's `BOSS_MACHINE_TOKEN_HOSTS=`
/// line — what boss-core's `Hosts::from_env` falls back to, for a script
/// run by hand that inherits no unit's `EnvironmentFile=` — unless the
/// caller hands a default of its own (the pod's door hands the host of
/// its sor-url). SET, even empty, the variable wins over both.
#[test]
fn an_unset_list_is_the_rendered_sor_env_or_the_callers_default() {
    for shell in SHELLS {
        let f = Fixture::new(&format!("mt-default-{shell}"));
        let dir = token_dir(&f, SECRET);
        let env_file = f.root.join("sor.env");
        write_file(
            &env_file,
            "BOSS_JOBS_URL=http://record.test:7900\nBOSS_MACHINE_TOKEN_HOSTS=record.test\n",
        );
        let stamped = |body: &str, env: &[(&str, &str)]| {
            let _ = std::fs::remove_file(f.root.join("headers.txt"));
            let mut all = vec![("BOSS_MACHINE_TOKEN_DIR", dir.to_str().unwrap())];
            all.extend_from_slice(env);
            let r = f.run(shell, body, &all);
            assert_eq!(r.code, 0, "{shell}: {}", r.stderr);
            f.read("headers.txt").contains(SECRET)
        };
        let plain = "set -eu\nmachine_token_header H \"$URL\"\ncurl ${H:+-H \"$H\"} \"$URL\"\n";
        let with_default =
            "set -eu\nmachine_token_header H \"$URL\" door.test\ncurl ${H:+-H \"$H\"} \"$URL\"\n";
        let sor = env_file.to_str().unwrap();
        assert!(stamped(
            plain,
            &[("BOSS_SOR_ENV", sor), ("URL", "http://record.test:7900/x")]
        ));
        assert!(!stamped(
            plain,
            &[("BOSS_SOR_ENV", sor), ("URL", "http://other.test/x")]
        ));
        // No rendered file: loopback only.
        assert!(!stamped(plain, &[("URL", "http://record.test:7900/x")]));
        assert!(stamped(plain, &[("URL", "http://127.0.0.1:7900/x")]));
        // The caller's default outranks the file; the variable outranks both.
        assert!(stamped(
            with_default,
            &[("BOSS_SOR_ENV", sor), ("URL", "http://door.test/x")]
        ));
        assert!(!stamped(
            with_default,
            &[("BOSS_SOR_ENV", sor), ("URL", "http://record.test/x")]
        ));
        assert!(!stamped(
            with_default,
            &[
                ("BOSS_MACHINE_TOKEN_HOSTS", ""),
                ("URL", "http://door.test/x")
            ]
        ));
    }
}

/// THE LIST IS SPLIT ON SPACES WHATEVER THE CALLER'S IFS (review
/// ef2da426 F8). The lib splits its host list by word splitting; a
/// caller that set IFS to a comma or a newline for its own loop would
/// have had a multi-entry list read as one word and every listed host
/// withheld — safe today, a refused caller the day the gate enforces.
/// The caller's IFS is kept afterwards, set or unset.
#[test]
fn the_host_list_splits_the_same_under_any_caller_ifs() {
    for shell in SHELLS {
        let f = Fixture::new(&format!("mt-ifs-{shell}"));
        let dir = token_dir(&f, SECRET);
        for (tag, set_ifs) in [
            ("newline", "IFS='\n'"),
            ("comma", "IFS=','"),
            ("empty", "IFS=''"),
            ("unset", "unset IFS"),
        ] {
            let _ = std::fs::remove_file(f.root.join("headers.txt"));
            let r = f.run(
                shell,
                &format!(
                    "{set_ifs}\n\
                     before=\"${{IFS-unset}}\"\n\
                     machine_token_header H http://second.test:7900\n\
                     [ \"${{IFS-unset}}\" = \"$before\" ] || {{ echo 'IFS changed' >&2; exit 9; }}\n\
                     [ -z \"$H\" ] || cat \"${{H#@}}\" > \"$STUB_DIR/headers.txt\"\n"
                ),
                &[
                    ("BOSS_MACHINE_TOKEN_DIR", dir.to_str().unwrap()),
                    ("BOSS_MACHINE_TOKEN_HOSTS", "first.test, second.test"),
                ],
            );
            assert_eq!(r.code, 0, "{shell} {tag}: {}", r.stderr);
            assert!(
                f.read("headers.txt").contains(SECRET),
                "{shell} {tag}: the second listed host is stamped: {}",
                r.stderr
            );
        }
    }
}

/// `machine_token_host URL` is the host the decision above is made on —
/// public, because the pod's door derives its default list from its own
/// sor-url with it rather than keeping a second copy of the parse.
#[test]
fn the_host_a_decision_reads_is_one_function() {
    for shell in SHELLS {
        let f = Fixture::new(&format!("mt-host-{shell}"));
        let r = f.run(
            shell,
            "for u in 'http://Boss-Jobs.boss.svc.cluster.local.:7900/api' \
                      'http://op@[::1]:7900' 'http://evil.com#@127.0.0.1'; do\n\
                 machine_token_host \"$u\"; echo\n\
             done > \"$STUB_DIR/hosts.txt\"\n",
            &[],
        );
        assert_eq!(r.code, 0, "{shell}: {}", r.stderr);
        assert_eq!(
            f.read("hosts.txt"),
            "boss-jobs.boss.svc.cluster.local\n::1\nevil.com\n",
            "{shell}"
        );
    }
}
