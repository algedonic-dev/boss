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
        // A psql that stands in for the sidecar's: `SHOW max_connections`
        // answers FAKE_MAX_CONNECTIONS, and with none set it fails the
        // way an unreachable server does. It records the URL it was given.
        let tools = dir.join("tools");
        create_dir(&tools);
        write_exec(
            &tools.join("psql"),
            &format!(
                "#!/usr/bin/env bash\n\
                 printf '%s\\n' \"$*\" >> '{marks}/psql-argv'\n\
                 [ -n \"${{FAKE_MAX_CONNECTIONS:-}}\" ] || {{ echo 'psql: connection refused' >&2; exit 2; }}\n\
                 echo \"$FAKE_MAX_CONNECTIONS\"\n",
                marks = marks.display(),
            ),
        );
        let script = dir.join(SCRIPT);
        create_dir(script.parent().unwrap());
        write_file(
            &script,
            &fs::read_to_string(repo_root().join(SCRIPT)).unwrap(),
        );
        let weights = "infra/gate-runner/test-binary-weights.json";
        if let Ok(bytes) = fs::read_to_string(repo_root().join(weights)) {
            create_dir(dir.join(weights).parent().unwrap());
            write_file(&dir.join(weights), &bytes);
        }
        let helper = "infra/lib/jq.sh";
        create_dir(dir.join(helper).parent().unwrap());
        write_file(
            &dir.join(helper),
            &fs::read_to_string(repo_root().join(helper)).unwrap(),
        );
        Fixture {
            state: dir.join("state"),
            dir,
            marks,
            cargo,
        }
    }

    /// Set the width cap in THIS fixture's copy of the script, so the
    /// derivation is tested against a cap the test chooses rather than
    /// the production constant (which gate_runner_manifests.rs pins, with
    /// the measurement that set it).
    fn with_max_width(self, cap: u64) -> Fixture {
        let script = self.dir.join(SCRIPT);
        let text = fs::read_to_string(&script).unwrap();
        let lines: Vec<String> = text
            .lines()
            .map(|l| {
                if l.starts_with("max_width=") {
                    format!("max_width={cap}")
                } else {
                    l.to_string()
                }
            })
            .collect();
        assert!(
            lines.iter().any(|l| *l == format!("max_width={cap}")),
            "{SCRIPT} declares `max_width=<n>` on a line of its own"
        );
        write_file(&script, &(lines.join("\n") + "\n"));
        self
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
        cmd.arg(self.dir.join(SCRIPT))
            .arg(mode)
            .arg(&self.state)
            .arg(&self.cargo)
            .args(["test", "-p", "alpha", "--all-features", "--no-fail-fast"])
            .current_dir(&self.dir)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.dir.join("tools").display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env_remove("CARGO_BUILD_JOBS")
            .env_remove("BOSS_TEST_BINARIES_WIDE")
            .env_remove("BOSS_TEST_POSTGRES_ADMIN_URL")
            .env_remove("FAKE_MAX_CONNECTIONS")
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

fn group_sources(f: &Fixture, names: &[&str]) {
    let path = f.dir.join("listing.json");
    let listing = fs::read_to_string(&path).unwrap();
    let rows: Vec<String> = listing
        .lines()
        .map(|line| {
            let mut row: Value = serde_json::from_str(line).unwrap();
            if names.contains(&row["target"]["name"].as_str().unwrap_or("")) {
                let source = PathBuf::from(row["manifest_path"].as_str().unwrap())
                    .parent()
                    .unwrap()
                    .join("tests/groups/area.rs");
                row["target"]["src_path"] = json!(source);
            }
            row.to_string()
        })
        .collect();
    write_file(&path, &(rows.join("\n") + "\n"));
}

#[test]
fn source_defined_groups_use_the_declared_default_without_changing_cargo_kind() {
    let f = Fixture::new(
        "wide-source-group",
        &[
            bin("ordinary", "alpha", 0),
            bin("unnamed", "alpha", 101),
            bin("measured", "alpha", 0),
        ],
        false,
        0,
    );
    group_sources(&f, &["unnamed"]);
    write_file(
        &f.dir.join("infra/gate-runner/test-binary-weights.json"),
        r#"{"version":1,"kind_defaults":{"bin":1,"lib":1,"test":1,"group":300},"targets":{"alpha:measured":{"seconds":600,"basis":"fixture measurement"}}}"#,
    );
    let out = f.build_and_run(&[("BOSS_TEST_BINARIES_WIDE", "1")]);
    assert_eq!(out.status.code(), Some(101), "{}", text(&out));
    let seen = f.seen();
    let names: Vec<&str> = seen.iter().map(|row| row[0].as_str()).collect();
    assert_eq!(names, ["measured", "unnamed", "ordinary"]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("-p alpha --test unnamed"));
}

#[test]
fn receipt_keeps_every_group_time_and_the_actual_last_finisher() {
    let f = Fixture::new(
        "wide-all-group-times",
        &[
            Bin {
                first: "for job in 2 3 4 5 6 7; do for i in $(seq 1 1000); do [ ! -f '{state}/logs/'\"$job\"'.status' ] || break; sleep 0.01; done; test -f '{state}/logs/'\"$job\"'.status' || exit 97; done",
                ..bin("a_last", "alpha", 0)
            },
            bin("b", "alpha", 0),
            bin("c", "alpha", 0),
            bin("d", "alpha", 0),
            bin("e", "alpha", 0),
            bin("f", "alpha", 0),
            bin("g", "alpha", 0),
        ],
        false,
        0,
    );
    group_sources(&f, &["a_last", "b", "c", "d", "e", "f", "g"]);
    // The first causally waits for every peer's recorded
    // completion, independent of wall-clock speed or reporting order.
    let built = f.run("build", &[]);
    assert!(built.status.success(), "{}", text(&built));
    let out = f.run("run", &[("BOSS_TEST_BINARIES_WIDE", "8")]);
    assert!(out.status.success(), "{}", text(&out));
    let summary = f.summary();
    assert_eq!(summary["slowest"].as_array().unwrap().len(), 5);
    assert_eq!(summary["tail"]["target"], "alpha:a_last");
    assert!(
        summary["tail"]["started_at_s"].as_f64().unwrap() < 60.0,
        "start is relative to this test phase, not an epoch"
    );
    assert!(summary["tail"]["seconds"].as_f64().unwrap() >= 0.0);
    let groups = summary["groups"]
        .as_array()
        .expect("all group timings retained");
    assert_eq!(groups.len(), 7);
    let names: BTreeSet<&str> = groups
        .iter()
        .map(|g| {
            assert!(g["seconds"].as_f64().unwrap() >= 0.0);
            g["target"].as_str().unwrap()
        })
        .collect();
    assert_eq!(
        names,
        [
            "alpha:a_last",
            "alpha:b",
            "alpha:c",
            "alpha:d",
            "alpha:e",
            "alpha:f",
            "alpha:g"
        ]
        .into_iter()
        .collect()
    );
    let gate = fs::read_to_string(repo_root().join("infra/gate.sh")).unwrap();
    let function = gate
        .split("test_binaries_json() {")
        .nth(1)
        .unwrap()
        .split("\n}\n")
        .next()
        .unwrap();
    let out = Command::new("bash").arg("-c").arg(format!(
        "ran_name() {{ printf '%s' \"$1\"; }}; RAN=(test); TEST_WIDE_STATE='{}'; test_binaries_json() {{{function}\n}}; test_binaries_json", f.state.display())).output().unwrap();
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stdout).unwrap(),
        summary,
        "the actual receipt consumer preserves every group and tail field"
    );
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

/// Without an explicit width it is HALF of CARGO_BUILD_JOBS (which the
/// gate-runner sets from the cgroup quota; each binary runs
/// RUST_TEST_THREADS=2, so half the quota in binaries is the quota in
/// test threads), at most `max_width`, and ONE — cargo's own serial
/// order — where CARGO_BUILD_JOBS is unset (backlog 62435ba1; it was a
/// quarter, at most four). The cap here is the FIXTURE's, 8, so every
/// bound can be seen to bind; the production cap is 4 and is pinned,
/// with the gate-drive measurement that kept it there, by
/// gate_runner_manifests.rs.
///
/// THE SIDECAR'S CONNECTION BUDGET BOUNDS IT TOO. Every DB-backed binary
/// holds up to `clients_per_binary` clients against one Postgres (two
/// tests' pools of 5 plus the reaper's 8 drops; the worst three measured
/// 15-16 each, gate_runner_manifests.rs), so the width is also at most
/// `max_connections / clients_per_binary`, with `max_connections` read
/// from the server itself (`SHOW max_connections` over
/// BOSS_TEST_POSTGRES_ADMIN_URL) — the one place the number is true. A
/// gate launched from a manifest that has not converged yet runs the
/// width ITS server admits, not the width the branch hoped for
/// (e3100bc8), and the receipt's `connection_budget` says which. With no
/// URL, no psql, or a server that does not answer, the budget is ASSUMED
/// to be Postgres's default of 100 — the narrower width, never the wider
/// — and `source` says `assumed`. The summary's `width_bound` names the
/// bound that set the width, so a receipt says WHY it ran that wide.
#[test]
fn the_width_is_half_the_build_jobs_within_the_connection_budget() {
    const URL: &str = "postgres://fixture:5432/postgres";
    for (jobs, max_connections, want, bound) in [
        (None, Some("200"), 1, "serial"),
        (Some("2"), Some("200"), 1, "build_jobs"),
        (Some("8"), Some("200"), 4, "build_jobs"),
        (Some("20"), Some("200"), 8, "max_width"),
        (Some("32"), Some("200"), 8, "max_width"),
        (Some("20"), Some("400"), 8, "max_width"),
        // 100 / 18 = 5: the budget, not the cap, bounds a 20-core gate
        // whose sidecar runs Postgres's default under a cap of 8.
        (Some("20"), Some("100"), 5, "connections"),
        (Some("20"), Some("36"), 2, "connections"),
        (Some("20"), Some("17"), 1, "connections"),
    ] {
        let f = Fixture::new(
            "cargo-test-wide-default",
            &[bin("only", "alpha", 0)],
            false,
            0,
        )
        .with_max_width(8);
        let mut env: Vec<(&str, &str)> = vec![("BOSS_TEST_POSTGRES_ADMIN_URL", URL)];
        env.extend(jobs.map(|j| ("CARGO_BUILD_JOBS", j)));
        env.extend(max_connections.map(|m| ("FAKE_MAX_CONNECTIONS", m)));
        let out = f.build_and_run(&env);
        assert!(out.status.success(), "{}", text(&out));
        let s = f.summary();
        assert_eq!(
            s["width"], want,
            "CARGO_BUILD_JOBS={jobs:?} max_connections={max_connections:?}: {s}"
        );
        assert_eq!(
            s["width_bound"], bound,
            "CARGO_BUILD_JOBS={jobs:?} max_connections={max_connections:?}: {s}"
        );
        assert_eq!(
            s["connection_budget"],
            json!({"max_connections": max_connections.unwrap().parse::<u64>().unwrap(),
                   "source": "server", "clients_per_binary": 18}),
            "{s}"
        );
        let asked = fs::read_to_string(f.marks.join("psql-argv")).unwrap_or_default();
        assert!(
            asked.contains(URL) && asked.contains("max_connections"),
            "the width was sized without asking the server at the admin URL: {asked:?}"
        );
    }

    // No answer from the server, no URL at all: the budget is assumed to
    // be the default 100, and the run says so rather than widening.
    for (label, env) in [
        (
            "the server does not answer",
            vec![
                ("CARGO_BUILD_JOBS", "20"),
                ("BOSS_TEST_POSTGRES_ADMIN_URL", URL),
            ],
        ),
        (
            "no admin URL is set",
            vec![("CARGO_BUILD_JOBS", "20"), ("FAKE_MAX_CONNECTIONS", "200")],
        ),
        (
            "the server answers nonsense",
            vec![
                ("CARGO_BUILD_JOBS", "20"),
                ("BOSS_TEST_POSTGRES_ADMIN_URL", URL),
                ("FAKE_MAX_CONNECTIONS", "lots"),
            ],
        ),
    ] {
        let f = Fixture::new(
            "cargo-test-wide-assumed",
            &[bin("only", "alpha", 0)],
            false,
            0,
        )
        .with_max_width(8);
        let out = f.build_and_run(&env);
        assert!(out.status.success(), "{label}: {}", text(&out));
        let s = f.summary();
        assert_eq!(s["width"], 5, "{label}: {s}");
        assert_eq!(s["width_bound"], "connections", "{label}: {s}");
        assert_eq!(
            s["connection_budget"],
            json!({"max_connections": 100, "source": "assumed", "clients_per_binary": 18}),
            "{label}: {s}"
        );
        assert!(
            text(&out).contains("assumed"),
            "{label}: the run must say the budget was assumed:\n{}",
            text(&out)
        );
    }

    let f = Fixture::new(
        "cargo-test-wide-override",
        &[bin("only", "alpha", 0)],
        false,
        0,
    );
    let out = f.build_and_run(&[
        ("CARGO_BUILD_JOBS", "20"),
        ("BOSS_TEST_POSTGRES_ADMIN_URL", URL),
        ("FAKE_MAX_CONNECTIONS", "100"),
        ("BOSS_TEST_BINARIES_WIDE", "7"),
    ]);
    assert!(out.status.success(), "{}", text(&out));
    let s = f.summary();
    assert_eq!(
        s["width"], 7,
        "an explicit width wins, over the budget too: it is the operator's say"
    );
    assert_eq!(s["width_bound"], "explicit", "{s}");

    // The production script, its cap untouched: a 20-core gate on a
    // sidecar that admits 100 runs exactly the cap, and says so.
    let f = Fixture::new(
        "cargo-test-wide-production-cap",
        &[bin("only", "alpha", 0)],
        false,
        0,
    );
    let out = f.build_and_run(&[
        ("CARGO_BUILD_JOBS", "20"),
        ("BOSS_TEST_POSTGRES_ADMIN_URL", URL),
        ("FAKE_MAX_CONNECTIONS", "100"),
    ]);
    assert!(out.status.success(), "{}", text(&out));
    let s = f.summary();
    assert_eq!(
        (s["width"].clone(), s["width_bound"].clone()),
        (json!(4), json!("max_width")),
        "the gate runs the production cap of 4 (gate_runner_manifests.rs pins why): {s}"
    );
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

#[test]
fn longest_declared_binary_runs_first_without_an_elapsed_time_oracle() {
    let f = Fixture::new(
        "wide-longest-first",
        &[
            bin("a_short", "alpha", 0),
            bin("z_long", "alpha", 0),
            bin("m_middle", "alpha", 0),
        ],
        false,
        0,
    );
    let path = f.dir.join("infra/gate-runner/test-binary-weights.json");
    create_dir(path.parent().unwrap());
    write_file(&path, &json!({"version":1,"kind_defaults":{"bin":1,"lib":1,"test":1,"group":300},"targets":{"alpha:a_short":{"seconds":1,"basis":"fixture-declared"},"alpha:z_long":{"seconds":30,"basis":"fixture-declared"},"alpha:m_middle":{"seconds":10,"basis":"fixture-declared"}}}).to_string());
    let out = f.build_and_run(&[("BOSS_TEST_BINARIES_WIDE", "1")]);
    assert!(out.status.success(), "{}", text(&out));
    let seen: Vec<_> = f.seen().iter().map(|r| r[0].clone()).collect();
    assert_eq!(
        seen,
        ["z_long", "m_middle", "a_short"],
        "numeric weights order every binary exactly once"
    );
}

#[test]
fn an_unreadable_weight_contract_refuses_before_running_a_binary() {
    for (i, document) in ["{}", "", " \n "].into_iter().enumerate() {
        let f = Fixture::new(
            &format!("wide-invalid-weights-{i}"),
            &[bin("a_short", "alpha", 0)],
            false,
            0,
        );
        let path = f.dir.join("infra/gate-runner/test-binary-weights.json");
        create_dir(path.parent().unwrap());
        write_file(&path, document);
        let built = f.run("build", &[]);
        assert!(built.status.success(), "{}", text(&built));
        let out = f.run("run", &[]);
        assert_eq!(
            out.status.code(),
            Some(2),
            "input {document:?}: {}",
            text(&out)
        );
        assert!(
            f.seen().is_empty(),
            "invalid scheduling data never becomes an empty or partial pass"
        );
    }
}
