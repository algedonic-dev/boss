//! `infra/cargo-test-wide.sh` — the gate's `cargo test`, split into a
//! `test build` check and a `test` check that runs several binaries at
//! a time (backlog 2d234222).
//!
//! WHY. Measured 2026-10-09 by run c4a8df09 over every gate-run: the
//! `test` check took a median 2041 s at the fixed scope boss-testing on
//! 2.25 of the gate container's 20 cores, IO wait 9%, because cargo runs
//! test binaries one at a time and boss-testing had tripled. Running
//! them wide is only a gain if it changes NOTHING ELSE about what a test
//! sees, so most of these pins hold the sameness: each binary starts in
//! its package root with cargo's runtime environment, every binary runs
//! when one fails, a failure is named in cargo's own words (which
//! infra/gate-runner/run.sh parses), and a run that ran nothing is not a
//! pass. The rest hold the width to its bound and the gate to its use.
//!
//! The fixtures stand in for cargo and for the test binaries, so the
//! script is exercised on a listing written for the purpose rather than
//! on a real build. What a test reads of cargo's environment, against
//! what the runner exports, is pinned tree-wide in
//! the_wide_runner_sets_what_tests_read_of_cargo.rs.

use boss_testing::{create_dir, repo_root, scratch_dir, write_exec, write_file};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

const SCRIPT: &str = "infra/cargo-test-wide.sh";

/// What a bounded run left: its exit code (`None` = still running at
/// the limit, and killed; `Some(None)` = ended by a signal) and output.
struct Bounded {
    code: Option<Option<i32>>,
    stdout: String,
    stderr: String,
}

impl Bounded {
    fn text(&self) -> String {
        // The stderr of a spinning run is tens of thousands of lines;
        // its head says what it was repeating.
        let head: Vec<&str> = self.stderr.lines().take(20).collect();
        format!(
            "exit {:?}\n--- stdout\n{}\n--- stderr ({} line(s), first 20)\n{}",
            self.code,
            self.stdout,
            self.stderr.lines().count(),
            head.join("\n")
        )
    }
}

/// One fake test binary: its target name, its package id (in whichever
/// spelling cargo used), the package's directory name, and its exit.
struct Bin {
    name: &'static str,
    kind: &'static str,
    package_id_tail: &'static str,
    pkg_dir: &'static str,
    exit: i32,
    /// Shell run first, before the binary records anything; `{state}` is
    /// the runner's state dir. How a fixture dies before its exit.
    first: &'static str,
}

const fn bin(name: &'static str, pkg_dir: &'static str, exit: i32) -> Bin {
    Bin {
        name,
        kind: "test",
        package_id_tail: "",
        pkg_dir,
        exit,
        first: "",
    }
}

struct Fixture {
    dir: PathBuf,
    state: PathBuf,
    marks: PathBuf,
    cargo: PathBuf,
}

impl Fixture {
    /// A fake cargo whose `--no-run` prints `listing` and whose `--doc`
    /// exits `doc_exit`, beside a binary for each of `bins` that records
    /// where and how it was started and how many binaries were running
    /// at once, then sleeps long enough for neighbours to overlap.
    fn new(label: &str, bins: &[Bin], doc: bool, doc_exit: i32) -> Fixture {
        let dir = scratch_dir(label);
        let marks = dir.join("marks");
        create_dir(&marks);
        let mut listing = String::new();
        // Noise cargo really prints and the runner must skip: a dependency
        // built in the normal profile, and the closing message.
        listing.push_str(
            &json!({"reason": "compiler-artifact", "package_id": "registry+https://x#serde@1.0.0",
                    "manifest_path": "/nowhere/serde/Cargo.toml",
                    "target": {"kind": ["lib"], "name": "serde", "src_path": "/nowhere/lib.rs", "doctest": true},
                    "profile": {"test": false}, "executable": null})
            .to_string(),
        );
        listing.push('\n');
        for b in bins {
            let pkg = dir.join("pkgs").join(b.pkg_dir);
            create_dir(&pkg);
            let exe = dir.join("bin").join(b.name);
            create_dir(exe.parent().expect("bin dir"));
            write_exec(
                &exe,
                &format!(
                    "#!/usr/bin/env bash\n\
                     {first}\n\
                     m='{marks}'\n\
                     printf '%s\\t%s\\t%s\\t%s\\t%s\\n' '{name}' \"$PWD\" \"${{CARGO_MANIFEST_DIR:-}}\" \"${{CARGO_PKG_NAME:-}}\" \"${{CARGO_CRATE_NAME:-}}\" >> \"$m/seen\"\n\
                     mkdir \"$m/running.{name}\"\n\
                     ls -d \"$m\"/running.* | wc -l >> \"$m/concurrency\"\n\
                     sleep 0.6\n\
                     rmdir \"$m/running.{name}\"\n\
                     echo 'running 1 test'\n\
                     if [ {exit} -eq 0 ]; then echo 'test result: ok. 1 passed; 0 failed'; else\n\
                     printf 'failures:\\n    {name}_case\\n\\ntest result: FAILED. 0 passed; 1 failed\\n'; fi\n\
                     exit {exit}\n",
                    first = b
                        .first
                        .replace("{state}", &dir.join("state").display().to_string()),
                    marks = marks.display(),
                    name = b.name,
                    exit = b.exit,
                ),
            );
            let id = if b.package_id_tail.is_empty() {
                format!("path+file://{}#0.1.0", pkg.display())
            } else {
                b.package_id_tail
                    .replace("{dir}", &pkg.display().to_string())
            };
            listing.push_str(
                &json!({"reason": "compiler-artifact", "package_id": id,
                        "manifest_path": pkg.join("Cargo.toml"),
                        "target": {"kind": [b.kind], "name": b.name,
                                   "src_path": pkg.join("tests").join(format!("{}.rs", b.name)),
                                   "doctest": false},
                        "profile": {"test": true}, "executable": exe})
                .to_string(),
            );
            listing.push('\n');
        }
        if doc {
            let pkg = dir.join("pkgs/alpha");
            create_dir(&pkg);
            listing.push_str(
                &json!({"reason": "compiler-artifact", "package_id": format!("path+file://{}#0.1.0", pkg.display()),
                        "manifest_path": pkg.join("Cargo.toml"),
                        "target": {"kind": ["lib"], "name": "alpha", "src_path": pkg.join("src/lib.rs"), "doctest": true},
                        "profile": {"test": true}, "executable": null})
                .to_string(),
            );
            listing.push('\n');
        }
        listing.push_str("{\"reason\":\"build-finished\",\"success\":true}\n");
        write_file(&dir.join("listing.json"), &listing);
        let cargo = dir.join("cargo");
        write_exec(
            &cargo,
            &format!(
                "#!/usr/bin/env bash\n\
                 printf '%s\\n' \"$*\" >> '{marks}/cargo-argv'\n\
                 case \" $* \" in\n\
                 *' --no-run '*) cat '{listing}'; exit 0 ;;\n\
                 *' --doc '*) echo 'doctests ran'; exit {doc_exit} ;;\n\
                 esac\n\
                 echo 'the fake cargo was asked for something the runner should not ask' >&2\n\
                 exit 99\n",
                marks = marks.display(),
                listing = dir.join("listing.json").display(),
            ),
        );
        Fixture {
            state: dir.join("state"),
            dir,
            marks,
            cargo,
        }
    }

    fn run(&self, mode: &str, env: &[(&str, &str)]) -> Output {
        self.command(mode, env)
            .output()
            .expect("run cargo-test-wide.sh")
    }

    /// `run`, but killed after `limit`: for the cases whose defect was a
    /// run that never ends, so a regression reds this test rather than
    /// hanging the suite. Output goes to files, because a spinning run
    /// writes faster than an unread pipe drains. `None` is "still
    /// running at the limit".
    fn run_bounded(&self, mode: &str, env: &[(&str, &str)], limit: Duration) -> Bounded {
        let out = self.dir.join(format!("{mode}.stdout"));
        let err = self.dir.join(format!("{mode}.stderr"));
        let mut child = self
            .command(mode, env)
            .stdout(fs::File::create(&out).expect("stdout file"))
            .stderr(fs::File::create(&err).expect("stderr file"))
            .spawn()
            .expect("start cargo-test-wide.sh");
        let began = Instant::now();
        let code = loop {
            if let Some(s) = child.try_wait().expect("poll the run") {
                break Some(s.code());
            }
            if began.elapsed() > limit {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        Bounded {
            code,
            stdout: fs::read_to_string(&out).unwrap_or_default(),
            stderr: fs::read_to_string(&err).unwrap_or_default(),
        }
    }

    fn command(&self, mode: &str, env: &[(&str, &str)]) -> Command {
        let mut cmd = Command::new("bash");
        cmd.arg(repo_root().join(SCRIPT))
            .arg(mode)
            .arg(&self.state)
            .arg(&self.cargo)
            .args(["test", "-p", "alpha", "--all-features", "--no-fail-fast"])
            .current_dir(&self.dir)
            .env_remove("CARGO_BUILD_JOBS")
            .env_remove("BOSS_TEST_BINARIES_WIDE")
            .env_remove("CARGO_MANIFEST_DIR")
            .env_remove("CARGO_PKG_NAME");
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd
    }

    /// `build`, which must succeed, then `run`.
    fn build_and_run(&self, env: &[(&str, &str)]) -> Output {
        let built = self.run("build", env);
        assert!(
            built.status.success(),
            "build half failed: {}",
            text(&built)
        );
        self.run("run", env)
    }

    fn seen(&self) -> Vec<Vec<String>> {
        fs::read_to_string(self.marks.join("seen"))
            .unwrap_or_default()
            .lines()
            .map(|l| l.split('\t').map(str::to_string).collect())
            .collect()
    }

    fn max_concurrency(&self) -> usize {
        fs::read_to_string(self.marks.join("concurrency"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.trim().parse().ok())
            .max()
            .unwrap_or(0)
    }

    fn summary(&self) -> Value {
        let raw = fs::read_to_string(self.state.join("summary.json"))
            .unwrap_or_else(|e| panic!("the run wrote its summary: {e}"));
        serde_json::from_str(&raw).unwrap_or_else(|e| panic!("summary.json is JSON: {e}\n{raw}"))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn text(out: &Output) -> String {
    format!(
        "exit {:?}\n--- stdout\n{}\n--- stderr\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn canon(p: &Path) -> String {
    p.canonicalize()
        .unwrap_or_else(|_| p.to_path_buf())
        .display()
        .to_string()
}

/// A binary started by the runner sees what it saw under `cargo test`:
/// its package root as cwd and cargo's runtime environment — read off
/// all three spellings of a package id cargo has used.
/// `boss_testing::test_db` reads CARGO_MANIFEST_DIR at RUN time to find
/// the schema directory of the checkout under test, so this is not
/// cosmetic.
#[test]
fn each_binary_starts_in_its_package_root_with_cargos_environment() {
    let bins = [
        bin("one", "alpha", 0),
        Bin {
            package_id_tail: "path+file://{dir}#beta-pkg@2.3.4",
            ..bin("two", "beta-dir", 0)
        },
        Bin {
            package_id_tail: "gamma 0.9.0 (path+file://{dir})",
            ..bin("three", "gamma", 0)
        },
        Bin {
            kind: "bin",
            ..bin("four-cli", "alpha", 0)
        },
    ];
    let f = Fixture::new("cargo-test-wide-env", &bins, false, 0);
    let out = f.build_and_run(&[("BOSS_TEST_BINARIES_WIDE", "2")]);
    assert!(out.status.success(), "{}", text(&out));
    let seen = f.seen();
    assert_eq!(
        seen.len(),
        4,
        "every binary ran exactly once: {seen:?}\n{}",
        text(&out)
    );
    for (name, dir, pkg, krate) in [
        ("one", "alpha", "alpha", "one"),
        ("two", "beta-dir", "beta-pkg", "two"),
        ("three", "gamma", "gamma", "three"),
        ("four-cli", "alpha", "alpha", "four_cli"),
    ] {
        let row = seen
            .iter()
            .find(|r| r[0] == name)
            .unwrap_or_else(|| panic!("{name} ran: {seen:?}"));
        let want = canon(&f.dir.join("pkgs").join(dir));
        assert_eq!(
            canon(Path::new(&row[1])),
            want,
            "{name}'s cwd is its package root"
        );
        assert_eq!(
            canon(Path::new(&row[2])),
            want,
            "{name}'s CARGO_MANIFEST_DIR"
        );
        assert_eq!(
            row[3], pkg,
            "{name}'s CARGO_PKG_NAME, read off its package id"
        );
        assert_eq!(row[4], krate, "{name}'s CARGO_CRATE_NAME");
    }
    // The fake cargo was asked to build once and never to run a binary.
    let argv = fs::read_to_string(f.marks.join("cargo-argv")).expect("cargo argv");
    assert_eq!(
        argv.lines().collect::<Vec<_>>(),
        [
            "test -p alpha --all-features --no-fail-fast --no-run --message-format json-render-diagnostics"
        ],
        "the build half is the gate's own cargo test argv plus --no-run"
    );
}

/// THE WIDTH IS A BOUND AND IT IS USED: with six binaries and a width of
/// three, never more than three run at once, and more than one does.
#[test]
fn binaries_run_concurrently_up_to_the_width_and_never_wider() {
    let names = ["b1", "b2", "b3", "b4", "b5", "b6"];
    let bins: Vec<Bin> = names.iter().map(|n| bin(n, "alpha", 0)).collect();
    let f = Fixture::new("cargo-test-wide-width", &bins, false, 0);
    let out = f.build_and_run(&[("BOSS_TEST_BINARIES_WIDE", "3")]);
    assert!(out.status.success(), "{}", text(&out));
    let most = f.max_concurrency();
    assert!(
        most <= 3,
        "{most} binaries ran at once against a width of 3\n{}",
        text(&out)
    );
    assert!(
        most >= 2,
        "the binaries never overlapped (at most {most} at once) — the run is serial\n{}",
        text(&out)
    );
    let s = f.summary();
    assert_eq!(s["width"], 3, "{s}");
    assert_eq!(s["binaries"], 6, "{s}");
    // Six 0.6 s binaries: their own times sum past the wall time, which
    // is the gain the receipt is there to show.
    let wall = s["wall_s"].as_u64().expect("wall_s");
    let own = s["own_time_sum_s"].as_u64().expect("own_time_sum_s");
    assert!(own >= 3, "six 0.6 s binaries' own times sum to {own}s: {s}");
    assert!(
        wall < own,
        "wall {wall}s is not under the serial sum {own}s: {s}"
    );
}

/// Without an explicit width it is a quarter of CARGO_BUILD_JOBS (which
/// the gate-runner sets from the cgroup quota), at most four, and ONE —
/// cargo's own serial order — where CARGO_BUILD_JOBS is unset.
#[test]
fn the_width_follows_the_build_jobs_and_is_serial_without_them() {
    for (jobs, want) in [
        (None, 1),
        (Some("2"), 1),
        (Some("8"), 2),
        (Some("20"), 4),
        (Some("32"), 4),
    ] {
        let f = Fixture::new(
            "cargo-test-wide-default",
            &[bin("only", "alpha", 0)],
            false,
            0,
        );
        let env: Vec<(&str, &str)> = jobs
            .map(|j| vec![("CARGO_BUILD_JOBS", j)])
            .unwrap_or_default();
        let out = f.build_and_run(&env);
        assert!(out.status.success(), "{}", text(&out));
        assert_eq!(f.summary()["width"], want, "CARGO_BUILD_JOBS={jobs:?}");
    }
    let f = Fixture::new(
        "cargo-test-wide-override",
        &[bin("only", "alpha", 0)],
        false,
        0,
    );
    let out = f.build_and_run(&[("CARGO_BUILD_JOBS", "20"), ("BOSS_TEST_BINARIES_WIDE", "7")]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(f.summary()["width"], 7, "an explicit width wins");
}

/// `--no-fail-fast`, kept (backlog 3bef4198): a failing binary fails the
/// run with cargo's exit code, every other binary still runs, and the
/// failure is printed as cargo prints it — the libtest block whole, then
/// `error: test failed, to rerun pass …` — which is what
/// infra/gate-runner/run.sh's failure-detail parser reads.
#[test]
fn a_failing_binary_fails_the_run_and_every_other_binary_still_runs() {
    let bins = [
        bin("first", "alpha", 0),
        bin("broken", "alpha", 1),
        bin("last", "alpha", 0),
    ];
    let f = Fixture::new("cargo-test-wide-fail", &bins, false, 0);
    let out = f.build_and_run(&[("BOSS_TEST_BINARIES_WIDE", "1")]);
    assert_eq!(out.status.code(), Some(101), "{}", text(&out));
    let ran: BTreeSet<String> = f.seen().into_iter().map(|r| r[0].clone()).collect();
    assert_eq!(
        ran,
        ["broken", "first", "last"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let block = stdout
        .split("error: test failed, to rerun pass `-p alpha --test broken`")
        .next()
        .filter(|before| before.len() < stdout.len())
        .unwrap_or_else(|| panic!("cargo's rerun line names the binary\n{}", text(&out)));
    assert!(
        block
            .trim_end()
            .ends_with("test result: FAILED. 0 passed; 1 failed"),
        "the rerun line follows the binary's own libtest block\n{}",
        text(&out)
    );
    assert_eq!(f.summary()["failed"], json!(["-p alpha --test broken"]));
}

/// A STATUS THAT CANNOT BE WRITTEN ENDS THE RUN RED, NEVER NOT AT ALL
/// (review 465e723f F1, backlog c0259ecc). Each job records its exit in
/// a status file the loop reads; when that write fails — the logs
/// directory gone, the disk refusing it — the loop's 125 fallback wrote
/// into the same directory, failed the same way, and the loop spun:
/// 42,937 error lines in 20 s and no end, so a gate burned CPU to its
/// deadline and its verdict was lost rather than a named red. The
/// fixture removes the logs directory from inside a binary, as the
/// reviewer did; the run must end, nonzero, naming the binary and why.
#[test]
fn a_status_that_cannot_be_written_fails_the_run_instead_of_spinning() {
    let bins = [Bin {
        first: "rm -rf '{state}/logs'",
        ..bin("vanish", "alpha", 0)
    }];
    let f = Fixture::new("cargo-test-wide-nologs", &bins, false, 0);
    let built = f.run("build", &[]);
    assert!(built.status.success(), "{}", text(&built));
    let out = f.run_bounded(
        "run",
        &[("BOSS_TEST_BINARIES_WIDE", "1")],
        Duration::from_secs(60),
    );
    assert!(
        out.code.is_some(),
        "the run was still going after 60 s — the status loop spins\n{}",
        out.text()
    );
    assert_eq!(out.code, Some(Some(101)), "{}", out.text());
    assert!(
        out.stdout.contains(
            "error: test failed, to rerun pass `-p alpha --test vanish` (no exit status: \
             the runner's log directory"
        ) && out.stdout.contains("is gone"),
        "the failure names the binary and the cause\n{}",
        out.text()
    );
    assert!(
        out.stderr.lines().count() < 20,
        "a bounded account on stderr, not a flood\n{}",
        out.text()
    );
    assert_eq!(f.summary()["failed"], json!(["-p alpha --test vanish"]));
}

/// A BINARY THAT DIES ON A SIGNAL SAYS SO (review 465e723f F2, backlog
/// c0259ecc). cargo follows its rerun line with `process didn't exit
/// successfully: … (signal: 9, SIGKILL: kill)`; the runner printed the
/// rerun line alone, so an OOM-killed or crashed binary read as a test
/// failure with no failing test. The exit status, and the signal above
/// 128, ride ON the rerun line — the one line infra/gate-runner/run.sh's
/// `failure_blocks` keeps beside a binary's libtest block.
#[test]
fn a_binary_killed_by_a_signal_is_named_with_its_exit_status_and_signal() {
    let bins = [
        Bin {
            first: "kill -KILL $$",
            ..bin("killed", "alpha", 0)
        },
        Bin {
            first: "kill -SEGV $$",
            ..bin("crashed", "alpha", 0)
        },
        bin("broken", "alpha", 1),
    ];
    let f = Fixture::new("cargo-test-wide-signal", &bins, false, 0);
    let out = f.build_and_run(&[("BOSS_TEST_BINARIES_WIDE", "2")]);
    assert_eq!(out.status.code(), Some(101), "{}", text(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    for line in [
        "error: test failed, to rerun pass `-p alpha --test killed` (exit status: 137, signal: 9, SIGKILL)",
        "error: test failed, to rerun pass `-p alpha --test crashed` (exit status: 139, signal: 11, SIGSEGV)",
        "error: test failed, to rerun pass `-p alpha --test broken` (exit status: 1)",
    ] {
        assert!(
            stdout.lines().any(|l| l == line),
            "missing: {line}\n{}",
            text(&out)
        );
    }
}

/// The doctests run through cargo, with the SAME argv the build half
/// took, so they select exactly what the gate selected — and a doctest
/// failure fails the run like any binary's.
#[test]
fn the_doctests_run_through_cargo_with_the_gates_own_selection() {
    let f = Fixture::new("cargo-test-wide-doc", &[bin("one", "alpha", 0)], true, 0);
    let out = f.build_and_run(&[("BOSS_TEST_BINARIES_WIDE", "2")]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("doctests ran"),
        "{}",
        text(&out)
    );
    let argv = fs::read_to_string(f.marks.join("cargo-argv")).expect("cargo argv");
    assert!(
        argv.lines()
            .any(|l| l == "test -p alpha --all-features --no-fail-fast --doc"),
        "doctests ran with the gate's argv plus --doc: {argv}"
    );
    assert_eq!(f.summary()["doctests"], true);

    let failing = Fixture::new(
        "cargo-test-wide-doc-red",
        &[bin("one", "alpha", 0)],
        true,
        101,
    );
    let out = failing.build_and_run(&[("BOSS_TEST_BINARIES_WIDE", "2")]);
    assert_eq!(out.status.code(), Some(101), "{}", text(&out));
    assert_eq!(failing.summary()["failed"], json!(["doctests"]));
}

/// NO EVIDENCE IS NOT A PASS. A run with no listing (its build failed or
/// never ran) and a listing that names nothing to run are refused, never
/// an empty green.
#[test]
fn a_run_with_nothing_to_run_is_refused_not_passed() {
    let f = Fixture::new(
        "cargo-test-wide-nolist",
        &[bin("one", "alpha", 0)],
        false,
        0,
    );
    let out = f.run("run", &[]);
    assert_eq!(out.status.code(), Some(2), "no listing: {}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no test ran"),
        "{}",
        text(&out)
    );

    let empty = Fixture::new("cargo-test-wide-empty", &[], false, 0);
    let out = empty.build_and_run(&[]);
    assert_eq!(out.status.code(), Some(2), "empty listing: {}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("ran nothing is not a pass"),
        "{}",
        text(&out)
    );
}

/// The gate runs its tests through the runner in BOTH cargo modes, as
/// two checks, and its receipt carries the run's own account.
#[test]
fn the_gate_tests_in_two_halves_and_its_receipt_carries_the_run() {
    let gate = fs::read_to_string(repo_root().join("infra/gate.sh")).expect("read gate.sh");
    let code: Vec<&str> = gate
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect();
    for (full, scoped) in [
        ("check \"test build\"", "build"),
        ("check \"test\" ", "run  "),
    ] {
        let hits: Vec<&&str> = code
            .iter()
            .filter(|l| l.trim_start().starts_with(full))
            .collect();
        assert_eq!(
            hits.len(),
            2,
            "{full} appears once per cargo mode (full and scoped): {hits:#?}"
        );
        for h in hits {
            assert!(
                h.contains(&format!("infra/cargo-test-wide.sh {scoped}"))
                    && h.contains("cargo test "),
                "{full} runs through infra/cargo-test-wide.sh {scoped}: {h}"
            );
        }
    }
    assert!(
        code.iter()
            .any(|l| l.trim() == "\"test_binaries\": $(test_binaries_json),"),
        "the receipt carries test_binaries"
    );
}
