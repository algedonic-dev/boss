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
            .env_remove("RUNTIME_DIRECTORY");
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
