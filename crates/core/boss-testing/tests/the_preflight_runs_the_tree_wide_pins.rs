//! `infra/gate.sh --lint` runs the declared tree-wide pins (backlog
//! 2f0c722a, 2026-10-16).
//!
//! WHAT WAS MEASURED. Protocol-retro adc3812b read every failed W41 car
//! gate one by one: 19 of 61 (31%) failed on a STANDING source-scan pin
//! in boss-testing or boss-policy-client that the builder's change broke
//! — `an_executable_is_written_by_write_exec` 5, the machine-token
//! sender pins 6, `every_door_is_a_declared_control` 4,
//! `a_jq_guard_asks_whether_there_is_a_document` 4, and others. Each
//! answers in seconds and needs no database, and each cost a full car
//! gate (median 27.7-57.3 min by concurrency) plus a re-gate, because
//! `--lint` ran fmt, the build-free lints and clippy and nothing else.
//!
//! THE SET IS THE ONE THE GATE ALREADY OWES. Those pins are tree-wide:
//! each declares itself with a `//! tree-wide pin` header (or a
//! `// tree-wide pin` line above one test under `src/`), and
//! `tree_wide_pins` in infra/gate.sh derives every scoped gate's set
//! from those declarations. `--lint` runs the same set through the same
//! `run_tree_wide_pins`, so the pre-flight cannot disagree with the
//! check it predicts and no list of pins lives in gate.sh (CLAUDE.md
//! §9a). A synthetic tree drives the real script here with a `cargo`
//! that records what it was asked.
//!
//! DATABASE-FREE, AND HELD SO. `--lint` runs where there is no test
//! database — on the dev pod 127.0.0.1 may be a port-forward to
//! production — so a declared pin that opens a test database would turn
//! the pre-flight into a database client. The last test here refuses
//! one, naming it: a tests/ pin with every module its binary compiles
//! in, and every test a src/ pin's filter runs, found by the markers
//! read out of infra/gate.sh (backlog 1c03eeb0).
//!
//! tree-wide pin — the last test reads every crate's tests/ and src/,
//! which no changed-file map attributes to this crate, so every scoped
//! gate runs it whatever its scope (`tree_wide_pins` in infra/gate.sh).

use boss_testing::{TREE_WIDE_FILE_PATTERN, TREE_WIDE_TEST_PATTERN, repo_root, tree_wide_pattern};
use proc_macro2::{Delimiter, TokenStream, TokenTree};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::str::FromStr;

/// The one lint the roster refuses to run without.
const LINT: &str = "infra/lint/workspace-declares-what-it-runs.sh";

/// The declared pin the synthetic tree carries, and one undeclared test
/// beside it that `--lint` must leave alone.
const PIN: &str = "crates/core/boss-testing/tests/a_declared_pin.rs";
const PLAIN: &str = "crates/core/boss-testing/tests/a_plain_test.rs";

/// A synthetic repository with this tree's gate.sh, one lint, one
/// declared tree-wide pin, and a `cargo` that appends each invocation to
/// `cargo.log` — exiting 1 for `cargo test` when `fail_tests` is set.
struct Tree {
    dir: PathBuf,
    tree: PathBuf,
}

impl Tree {
    fn new(tag: &str, fail_tests: bool) -> Tree {
        let fail = if fail_tests {
            "[ \"$1\" = test ] && exit 1\n"
        } else {
            ""
        };
        Tree::with(tag, &[PIN], fail)
    }

    /// The tree with `pins` declared, and a `cargo` that records its
    /// argv and then runs `cargo_body`.
    fn with(tag: &str, pins: &[&str], cargo_body: &str) -> Tree {
        let dir = boss_testing::scratch_dir(&format!("preflight-tree-wide-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        let tree = dir.join("tree");
        boss_testing::copy_lint_libs(&tree);
        boss_testing::copy_gate_sh(&tree);
        boss_testing::write_file(&tree.join(LINT), "#!/usr/bin/env bash\nexit 0\n");
        boss_testing::create_dir(&tree.join("crates/core/boss-testing/tests"));
        for pin in pins {
            boss_testing::write_file(
                &tree.join(pin),
                "//! tree-wide pin — it scans a tree\n#[test]\nfn t() {}\n",
            );
        }
        boss_testing::write_file(&tree.join(PLAIN), "#[test]\nfn t() {}\n");
        boss_testing::write_file(&tree.join(".gitignore"), "scratch/\n");
        run_git(&tree, &["init", "-q", "-b", "main"]);
        run_git(&tree, &["add", "."]);
        run_git(
            &tree,
            &[
                "-c",
                "user.email=preflight-pins@test",
                "-c",
                "user.name=preflight-pins",
                "commit",
                "-q",
                "-m",
                "the gate under test",
            ],
        );
        let bin = dir.join("bin");
        boss_testing::create_dir(&bin);
        boss_testing::write_exec(
            &bin.join("cargo"),
            &format!(
                "#!/usr/bin/env bash\necho \"$*\" >> '{}'\n{cargo_body}exit 0\n",
                dir.join("cargo.log").display()
            ),
        );
        boss_testing::write_exec(
            &dir.join("df"),
            "#!/usr/bin/env bash\n\
             echo 'Filesystem 1024-blocks Used Available Capacity Mounted on'\n\
             for _ in \"$@\"; do echo '/dev/fake 1 1 943718400 1% /'; done\n",
        );
        Tree { dir, tree }
    }

    fn run(&self, mode: &str) -> Output {
        let path = std::env::var("PATH").unwrap_or_default();
        Command::new("bash")
            .arg(self.tree.join("infra/gate.sh"))
            .arg(mode)
            .current_dir(&self.tree)
            .env("PATH", format!("{}:{path}", self.dir.join("bin").display()))
            .env("BOSS_GATE_DF_CMD", self.dir.join("df"))
            .env("BOSS_GATE_MIN_FREE_GB", "12")
            .env("BOSS_GATE_RECEIPT", self.dir.join("receipt.json"))
            .env("BOSS_GATE_TRUNK", "main")
            .env("BOSS_TRUNK_REF", "main")
            // The width the pins run at is cargo's (backlog 19347752),
            // fixed here rather than inherited from whoever runs this.
            .env("CARGO_BUILD_JOBS", "2")
            .env("GIT_CEILING_DIRECTORIES", "")
            .output()
            .unwrap_or_else(|e| panic!("run gate.sh {mode} in {}: {e}", self.tree.display()))
    }

    fn cargo_log(&self) -> String {
        std::fs::read_to_string(self.dir.join("cargo.log")).unwrap_or_default()
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn run_git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?} in {}: {e}", dir.display()));
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// THE DEFECT. `--lint` runs the declared pin, by the gate's own
/// invocation, and only the declared one.
#[test]
fn lint_runs_every_declared_tree_wide_pin() {
    let tree = Tree::new("runs", false);
    let out = tree.run("--lint");
    let log = tree.cargo_log();
    let t = text(&out);
    assert_eq!(out.status.code(), Some(0), "a passing pin is clean:\n{t}");
    assert!(
        log.lines().any(|l| l.starts_with("test -p boss-testing")
            && l.contains("--test a_declared_pin")
            && l.contains("--all-features")),
        "--lint must run the declared tree-wide pin the way the gate does \
         (cargo test -p <crate> --test <file> --all-features); cargo was asked:\n{log}\n{t}"
    );
    assert!(
        !log.contains("a_plain_test"),
        "--lint runs the DECLARED set, not every test in the crate; cargo was asked:\n{log}"
    );
}

/// THE CONTROL, AND THE VERDICT. A failing pin fails `--lint`, named.
#[test]
fn a_failing_tree_wide_pin_fails_the_preflight() {
    let tree = Tree::new("fails", true);
    let out = tree.run("--lint");
    let t = text(&out);
    assert_eq!(
        out.status.code(),
        Some(1),
        "a red pin must red the pre-flight, or a gate learns it instead:\n{t}"
    );
    assert!(
        t.contains("tree-wide pins"),
        "the failed check must be named:\n{t}"
    );
}

/// `--quick` stays build-free: it compiles nothing, so it runs no pin.
#[test]
fn quick_runs_no_pin() {
    let tree = Tree::new("quick", false);
    let out = tree.run("--quick");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(
        !tree.cargo_log().contains("a_declared_pin"),
        "--quick compiles nothing; cargo was asked:\n{}",
        tree.cargo_log()
    );
}

/// Two more declared pins, for the width tests below.
const PASSING: &str = "crates/core/boss-testing/tests/passing_pin.rs";
const FAILING: &str = "crates/core/boss-testing/tests/failing_pin.rs";

/// A `cargo` whose test RUN waits up to 10 s for a second pin's run to
/// start, and fails as `ran alone` if none does — so a runner that runs
/// the pins one after another reds here instead of passing slowly. The
/// build (`--no-run`) answers at once. `then` runs after the wait, with
/// the pin's name in `$pin`.
fn concurrent_cargo(then: &str) -> String {
    format!(
        "case \" $* \" in *' --no-run '*) exit 0 ;; esac\n\
         [ \"$1\" = test ] || exit 0\n\
         argv=\"$*\"; pin=${{argv##*--test }}; pin=${{pin%% *}}\n\
         started=\"${{0%/bin/cargo}}/started\"\n\
         mkdir -p \"$started\" && : > \"$started/$pin\"\n\
         for _ in $(seq 100); do set -- \"$started\"/*; [ $# -ge 2 ] && break; sleep 0.1; done\n\
         set -- \"$started\"/*\n\
         [ $# -ge 2 ] || {{ echo \"$pin ran alone: no other pin's run started within 10 s\"; exit 1; }}\n\
         {then}"
    )
}

/// THE SPEED (backlog 19347752). The pins are built once, then RUN
/// several at a time, as wide as cargo builds: measured 2026-10-18 the
/// serial run took 276 s warm on the dev pod, all but ~2 s of it the
/// binaries one after another.
#[test]
fn the_tree_wide_pins_run_at_the_same_time() {
    let tree = Tree::with("parallel", &[PIN, PASSING], &concurrent_cargo(""));
    let out = tree.run("--lint");
    let log = tree.cargo_log();
    let t = text(&out);
    assert_eq!(
        out.status.code(),
        Some(0),
        "at CARGO_BUILD_JOBS=2 the two pins must run at once — each one's run waits for \
         the other to start; cargo was asked:\n{log}\n{t}"
    );
    let builds: Vec<_> = log.lines().filter(|l| l.contains("--no-run")).collect();
    assert!(
        builds.len() == 1
            && builds[0].contains("--test a_declared_pin")
            && builds[0].contains("--test passing_pin"),
        "the crate's pins are BUILT once, together, before any runs; cargo was asked:\n{log}"
    );
}

/// THE VERDICT, KEPT. A pin that fails while another runs beside it
/// still fails the check, and the runner names it — not only cargo's
/// output somewhere above.
#[test]
fn a_failing_pin_run_in_parallel_still_fails_the_check_by_name() {
    let tree = Tree::with(
        "parallel-fails",
        &[PASSING, FAILING],
        &concurrent_cargo(
            "[ \"$pin\" = failing_pin ] && { echo 'test t ... FAILED'; exit 101; }\n",
        ),
    );
    let out = tree.run("--lint");
    let t = text(&out);
    assert_eq!(
        out.status.code(),
        Some(1),
        "a red pin must red the pre-flight when the pins run in parallel:\n{t}"
    );
    let named: Vec<_> = t
        .lines()
        .filter(|l| l.starts_with("gate: tree-wide pin") && l.contains("failed"))
        .collect();
    assert!(
        named.iter().any(|l| l.contains("failing_pin")),
        "the runner must name the pin that failed:\n{t}"
    );
    assert!(
        !named.iter().any(|l| l.contains("passing_pin")),
        "the pin that passed beside it is not named as failed:\n{t}"
    );
    assert!(
        t.contains("GATE FAIL: tree-wide pins"),
        "the failed check must be named:\n{t}"
    );
}

// ---------------------------------------------------------------------
// DATABASE-FREE: every pin `--lint` runs, as the gate finds and runs it
// ---------------------------------------------------------------------
//
// Review 306f1831 of car 10c9c7ab measured three ways the first form of
// this guard read less than `--lint` runs (backlog 1c03eeb0):
//
//   - it read only the top-level file carrying the header. A declared
//     tests/ pin is a whole test BINARY, and `mod x;` compiles x.rs or
//     x/mod.rs into it, so the reviewer's database opener in
//     tests/server_gate_scan/mod.rs — compiled into
//     every_service_mounts_the_machine_gate — left it green;
//   - it never read the src/ form of the marker, so the four unit
//     tests `tree_wide_pins` runs out of crate binaries were unguarded;
//   - it spelled the marker a second time, so a gate that changed its
//     pattern would have left this guard reading another set.
//
// So it now reads both markers out of infra/gate.sh, follows every
// module declaration from a tests/ pin, and reads every test a src/
// pin's filter runs. It reads TOKENS, not lines: a comment or a string
// that mentions the database type opens nothing, and a fixture can
// plant one without the guard reading the fixture's own source.
//
// WHAT IT DOES NOT FOLLOW: a call. A pin that calls a helper in a crate
// library which opens the database is not seen; the opener has to be
// named in the pin's own code. That is the boundary this guard holds.

/// The type a test opens its database through, spelled in two halves so
/// this file plants it in fixtures without naming it in its own code —
/// the workspace lint counts database-backed test files by the name.
const DB: &str = concat!("Test", "Db");

/// What one scan read and what it refused.
#[derive(Default)]
struct Scan {
    /// Declared tests/ pins: whole test binaries.
    file_pins: usize,
    /// Declared src/ pins: single tests run as a filter.
    src_pins: usize,
    offenders: Vec<String>,
}

/// A marker as infra/gate.sh spells it — the value of `<name>='<ERE>'`
/// — compiled for the one-line-at-a-time match grep gives it.
fn gate_marker(name: &str) -> regex::Regex {
    let value = tree_wide_pattern(name);
    regex::Regex::new(&value)
        .unwrap_or_else(|e| panic!("{name} in infra/gate.sh is not a regex: {value}: {e}"))
}

/// Every declared pin under `<root>/crates`, found as `tree_wide_pins`
/// finds it, and every one that names the database type in code it runs.
fn scan(root: &Path) -> Scan {
    let file_marker = gate_marker(TREE_WIDE_FILE_PATTERN);
    let test_marker = gate_marker(TREE_WIDE_TEST_PATTERN);
    let rel = |p: &Path| p.strip_prefix(root).unwrap_or(p).display().to_string();
    let mut scan = Scan::default();
    for tier in read_dirs(&root.join("crates")) {
        for krate in read_dirs(&tier) {
            // `crates/*/*/tests/*.rs`: the top level only, as the gate globs.
            for pin in rs_files(&krate.join("tests"), false)
                .into_iter()
                .filter(|f| read(f).lines().any(|l| file_marker.is_match(l)))
            {
                scan.file_pins += 1;
                match module_tree(&pin) {
                    Ok(files) => scan.offenders.extend(
                        files
                            .iter()
                            .filter(|m| names_the_database(&tokens(m)))
                            .map(|m| {
                                if m == &pin {
                                    rel(&pin)
                                } else {
                                    format!("{}, through {}", rel(&pin), rel(m))
                                }
                            }),
                    ),
                    Err(why) => scan.offenders.push(format!("{}: {why}", rel(&pin))),
                }
            }
            // `crates/*/*/src/**.rs`: one test under each marker. The
            // gate runs it as `cargo test … -- <name>`, a SUBSTRING
            // filter, so every function in the crate whose name holds
            // the pin's runs too when it is a test — read them all.
            let src = rs_files(&krate.join("src"), true);
            let names: Vec<String> = src
                .iter()
                .flat_map(|f| pinned_tests(&read(f), &test_marker))
                .collect();
            scan.src_pins += names.len();
            if names.is_empty() {
                continue;
            }
            for f in &src {
                for (func, body) in functions(tokens(f)) {
                    if names.iter().any(|n| func.contains(n.as_str())) && names_the_database(&body)
                    {
                        scan.offenders.push(format!("{}: fn {func}", rel(f)));
                    }
                }
            }
        }
    }
    scan
}

/// The test under each src/ marker, picked as `tree_wide_pins`' awk
/// picks it: the first `fn <name>` on a non-comment line below it.
fn pinned_tests(body: &str, marker: &regex::Regex) -> Vec<String> {
    let mut want = false;
    let mut out = Vec::new();
    for line in body.lines() {
        if marker.is_match(line) {
            want = true;
        } else if want
            && !line.trim_start().starts_with("//")
            && let Some(name) = line
                .split("fn ")
                .nth(1)
                .map(|rest| rest.split(|c: char| !c.is_ascii_alphanumeric() && c != '_'))
                .and_then(|mut words| words.next())
                .filter(|n| !n.is_empty())
        {
            out.push(name.to_string());
            want = false;
        }
    }
    out
}

/// `pin` and every file a `mod x;` in it, or in a file it reaches,
/// compiles into its test binary — x.rs or x/mod.rs beside the module
/// that declares it, or the `#[path]` it names.
fn module_tree(pin: &Path) -> Result<Vec<PathBuf>, String> {
    let mut seen: Vec<PathBuf> = Vec::new();
    // A crate root, a mod.rs and a `#[path]` file own their directory;
    // any other x.rs declares its children under x/.
    let mut todo = vec![(pin.to_path_buf(), true)];
    while let Some((file, owns_dir)) = todo.pop() {
        if seen.contains(&file) {
            continue;
        }
        let dir = file.parent().map(Path::to_path_buf).unwrap_or_default();
        let children = if owns_dir {
            dir.clone()
        } else {
            dir.join(file.file_stem().unwrap_or_default())
        };
        declared_modules(tokens(&file), &children, &dir, &mut todo)
            .map_err(|why| format!("{why} (declared in {})", file.display()))?;
        seen.push(file);
    }
    Ok(seen)
}

/// Push each out-of-line `mod` in `stream` onto `todo`, descending into
/// inline `mod x { … }` blocks with their directory.
fn declared_modules(
    stream: TokenStream,
    children: &Path,
    attr_base: &Path,
    todo: &mut Vec<(PathBuf, bool)>,
) -> Result<(), String> {
    let tree: Vec<TokenTree> = stream.into_iter().collect();
    let mut path_attr: Option<String> = None;
    for (i, tok) in tree.iter().enumerate() {
        match tok {
            TokenTree::Punct(p) if p.as_char() == '#' => {
                if let Some(p) = tree.get(i + 1).and_then(path_attribute) {
                    path_attr = Some(p);
                }
            }
            TokenTree::Ident(id) if id == "mod" => {
                let Some(TokenTree::Ident(name)) = tree.get(i + 1) else {
                    continue;
                };
                let name = name.to_string();
                let name = name.trim_start_matches("r#");
                match tree.get(i + 2) {
                    Some(TokenTree::Punct(p)) if p.as_char() == ';' => {
                        let candidates = match path_attr.take() {
                            Some(p) => vec![(attr_base.join(p), true)],
                            None => vec![
                                (children.join(format!("{name}.rs")), false),
                                (children.join(name).join("mod.rs"), true),
                            ],
                        };
                        let found = candidates.iter().find(|(p, _)| p.is_file()).ok_or_else(
                            || {
                                format!(
                                    "`mod {name};` resolves to none of {:?} — a module this guard \
                                 cannot read is a module it did not check",
                                    candidates.iter().map(|(p, _)| p).collect::<Vec<_>>()
                                )
                            },
                        )?;
                        todo.push(found.clone());
                    }
                    Some(TokenTree::Group(g)) if g.delimiter() == Delimiter::Brace => {
                        path_attr = None;
                        let nested = children.join(name);
                        declared_modules(g.stream(), &nested, &nested, todo)?;
                    }
                    _ => {}
                }
            }
            TokenTree::Punct(p) if p.as_char() == ';' => path_attr = None,
            TokenTree::Group(g) if g.delimiter() == Delimiter::Brace => path_attr = None,
            _ => {}
        }
    }
    Ok(())
}

/// The literal of a `[path = "…"]` attribute body, if `tok` is one.
fn path_attribute(tok: &TokenTree) -> Option<String> {
    let TokenTree::Group(g) = tok else {
        return None;
    };
    let inner: Vec<TokenTree> = g.stream().into_iter().collect();
    match inner.as_slice() {
        [
            TokenTree::Ident(k),
            TokenTree::Punct(eq),
            TokenTree::Literal(lit),
        ] if g.delimiter() == Delimiter::Bracket && k == "path" && eq.as_char() == '=' => {
            Some(lit.to_string().trim_matches('"').to_string())
        }
        _ => None,
    }
}

/// Every `fn <name> … { body }` in `stream`, at any depth.
fn functions(stream: TokenStream) -> Vec<(String, TokenStream)> {
    let tree: Vec<TokenTree> = stream.into_iter().collect();
    let mut out = Vec::new();
    for (i, tok) in tree.iter().enumerate() {
        match tok {
            TokenTree::Ident(id) if id == "fn" => {
                let Some(TokenTree::Ident(name)) = tree.get(i + 1) else {
                    continue;
                };
                let body = tree[i + 2..]
                    .iter()
                    .take_while(|t| !matches!(t, TokenTree::Punct(p) if p.as_char() == ';'))
                    .find_map(|t| match t {
                        TokenTree::Group(g) if g.delimiter() == Delimiter::Brace => {
                            Some(g.stream())
                        }
                        _ => None,
                    });
                if let Some(body) = body {
                    out.push((name.to_string(), body));
                }
            }
            TokenTree::Group(g) => out.extend(functions(g.stream())),
            _ => {}
        }
    }
    out
}

/// Whether the database type is named anywhere in `stream`'s code. A
/// comment is not a token and a string is a literal, so neither counts.
fn names_the_database(stream: &TokenStream) -> bool {
    stream.clone().into_iter().any(|t| match t {
        TokenTree::Ident(id) => id == DB,
        TokenTree::Group(g) => names_the_database(&g.stream()),
        _ => false,
    })
}

/// `file` lexed as Rust. A file the lexer refuses would not compile, so
/// that is a panic naming it rather than a file read as clean.
fn tokens(file: &Path) -> TokenStream {
    TokenStream::from_str(&read(file))
        .unwrap_or_else(|e| panic!("lex {} as Rust: {e}", file.display()))
}

fn read(file: &Path) -> String {
    std::fs::read_to_string(file).unwrap_or_else(|e| panic!("read {}: {e}", file.display()))
}

/// The `.rs` files in `dir` (none when it is absent), in name order —
/// with every subdirectory's too when `deep`.
fn rs_files(dir: &Path, deep: bool) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries.map(|e| e.expect("dir entry").path()).collect();
    paths.sort();
    paths
        .into_iter()
        .flat_map(|p| {
            if p.is_dir() {
                if deep { rs_files(&p, true) } else { Vec::new() }
            } else if p.extension().is_some_and(|x| x == "rs") {
                vec![p]
            } else {
                Vec::new()
            }
        })
        .collect()
}

fn read_dirs(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .map(|e| e.expect("dir entry").path())
        .filter(|p| p.is_dir())
        .collect()
}

/// A synthetic tree under a scratch dir, `(path, body)` per file.
fn fixture(tag: &str, files: &[(&str, String)]) -> PathBuf {
    let root = boss_testing::scratch_dir(&format!("no-test-db-{tag}"));
    for (rel, body) in files {
        let path = root.join(rel);
        boss_testing::create_dir(path.parent().expect("a fixture file has a parent"));
        boss_testing::write_file(&path, body);
    }
    root
}

/// THE REVIEWER'S CASE. The opener sits two modules below the pin, in
/// the x/mod.rs shape every_service_mounts_the_machine_gate uses, and a
/// plain test beside the pin opens one too and is left alone.
#[test]
fn a_database_opened_in_a_pin_module_is_named() {
    let root = fixture(
        "module",
        &[
            (
                "crates/core/fixture/tests/a_pin.rs",
                "//! tree-wide pin — it scans a tree\nmod scan;\n#[test]\nfn t() { scan::walk(); }\n"
                    .to_string(),
            ),
            (
                "crates/core/fixture/tests/scan/mod.rs",
                "mod helper;\npub fn walk() { helper::open(); }\n".to_string(),
            ),
            (
                "crates/core/fixture/tests/scan/helper.rs",
                format!("pub fn open() {{\n    let _db = {DB}::new();\n}}\n"),
            ),
            (
                "crates/core/fixture/tests/a_plain_test.rs",
                format!("#[test]\nfn t() {{\n    let _db = {DB}::new();\n}}\n"),
            ),
        ],
    );
    let scan = scan(&root);
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(scan.file_pins, 1, "one declared tests/ pin");
    assert!(
        scan.offenders.len() == 1
            && scan.offenders[0].contains("tests/a_pin.rs")
            && scan.offenders[0].contains("tests/scan/helper.rs"),
        "a module the pin's binary compiles in is the pin's own code: {:?}",
        scan.offenders
    );
}

/// A src/ pin that opens the database is named; an unpinned test in the
/// same module that opens one is not, because the filter never runs it.
#[test]
fn a_database_opened_by_a_src_pin_is_named() {
    let root = fixture(
        "src",
        &[(
            "crates/core/fixture/src/lib.rs",
            format!(
                "#[cfg(test)]\nmod tests {{\n    // tree-wide pin — it reads a tree\n    #[test]\n    fn clean_pin() {{}}\n\n    #[test]\n    fn unpinned_db_test() {{\n        let _db = {DB}::new();\n    }}\n\n    // tree-wide pin — it reads a tree\n    #[test]\n    fn db_pin() {{\n        let _db = {DB}::new();\n    }}\n}}\n"
            ),
        )],
    );
    let scan = scan(&root);
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(scan.src_pins, 2, "two declared src/ pins");
    assert!(
        scan.offenders.len() == 1 && scan.offenders[0].ends_with("fn db_pin"),
        "the pinned test that opens the database is named, the unpinned one is not: {:?}",
        scan.offenders
    );
}

/// No declared pin opens a test database, so `--lint` stays a source
/// scan wherever it runs.
#[test]
fn no_tree_wide_pin_opens_a_test_database() {
    let scan = scan(&repo_root());
    assert!(
        scan.file_pins > 0 && scan.src_pins > 0,
        "read {} tests/ and {} src/ pins — a scan that found none of either kind scanned \
         nothing there, and that is not a pass",
        scan.file_pins,
        scan.src_pins
    );
    assert!(
        scan.offenders.is_empty(),
        "a declared tree-wide pin opens a test database, and `infra/gate.sh --lint` runs every \
         declared pin where there is no test database (backlog 2f0c722a). Move the \
         database half to a file of its own: {:?}",
        scan.offenders
    );
}
