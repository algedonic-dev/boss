//! `BOSS_ESTATE=none` — a run's explicit, recorded declaration that no
//! estate exists where it runs — skips the LIVE half of the three lints
//! that read the estate, and nothing else.
//!
//! MEASURED 2026-09-27 (backlog 3e63662c, run 37e76794). The public
//! mirror's Gate (`.github/workflows/ci.yml`, `infra/gate.sh` full)
//! ended REFUSED (exit 2) on every publish PR since ci.yml returned on
//! 2026-09-22 (#243, #244, #245). Three roster lints read the estate —
//! `a-car-stays-under-the-edit-level`, `the-live-rules-are-the-authored-
//! rules`, `the-live-protocols-are-the-authored-protocols` — their
//! defaults are `*.svc.cluster.local`, and on a GitHub runner each
//! exits 3 (CANNOT ANSWER), which `check_lint` turns into a refusal
//! receipt that outranks even a real red. So a publish PR's Gate said
//! nothing about the tree, on every run, for a reason that was true of
//! the runner and not of the branch.
//!
//! THE OPERATOR'S CALL. Not a subset mode — `the_mirror_runs_the_one_gate.rs`
//! refuses those, and a mode that drops checks by name is the second
//! definition that file exists to prevent. Instead the mirror SAYS it
//! has no estate, and the three lints honour the saying for their live
//! half only: every tree check still runs, the lint prints that the
//! live comparison was not made and why, and the gate's receipt names
//! the declaration and each lint that did not read the estate. Unset,
//! nothing changes — an unreachable estate is still a refusal — and a
//! value the tree does not know is refused too, because a misspelt
//! declaration must read as neither answer.
//!
//! Every lint run here is handed a `curl` that records its arguments
//! and answers as a refused connect does, so "the estate was not read"
//! is an observation (no call named the estate's address), not an
//! inference from the exit code.

use boss_testing::repo_root;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// The three lints that read the estate, with the variable each one's
/// header names for its address. `the_live_lints_are_exactly_the_ones_
/// that_read_the_estate` holds this list to the tree.
const LIVE_LINTS: [(&str, &str); 3] = [
    (
        "the-live-protocols-are-the-authored-protocols",
        "BOSS_JOBS_URL",
    ),
    (
        "the-live-rules-are-the-authored-rules",
        "BOSS_DISPATCHER_URL",
    ),
    ("a-car-stays-under-the-edit-level", "BOSS_JOBS_URL"),
];

/// An address that names the estate for the recording `curl` — any
/// call carrying it is a read of the estate.
const ESTATE: &str = "http://estate.test:7900";

/// `NAME="value"` read from its one definition in a lint lib, never
/// typed here (CLAUDE.md §9a).
fn lib_value(lib: &str, name: &str) -> String {
    let path = repo_root().join("infra/lint/lib").join(lib);
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let prefix = format!("{name}=");
    text.lines()
        .find_map(|l| l.strip_prefix(&prefix))
        .unwrap_or_else(|| panic!("{lib} defines {name}=<value>"))
        .trim()
        .trim_matches('"')
        .to_string()
}

fn marker() -> String {
    lib_value("no-estate.sh", "LINT_NO_ESTATE_MARKER")
}

fn cannot_answer() -> i32 {
    lib_value("git-answer.sh", "LINT_CANNOT_ANSWER")
        .parse()
        .expect("LINT_CANNOT_ANSWER is a number")
}

/// A `bin/` holding the recording `curl`, and the file it records to.
struct Curl {
    dir: PathBuf,
    calls: PathBuf,
}

impl Curl {
    fn new(tag: &str) -> Curl {
        let dir = boss_testing::scratch_dir(&format!("no-estate-{tag}"));
        let bin = dir.join("bin");
        boss_testing::create_dir(&bin);
        let calls = dir.join("curl-calls");
        boss_testing::write_exec(
            &bin.join("curl"),
            &format!(
                "#!/usr/bin/env bash\nprintf '%s\\n' \"$*\" >> '{}'\nprintf 000\nexit 7\n",
                calls.display()
            ),
        );
        Curl { dir, calls }
    }

    fn path(&self) -> String {
        format!(
            "{}:{}",
            self.dir.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        )
    }

    /// Every recorded call that named the estate.
    fn estate_reads(&self) -> Vec<String> {
        std::fs::read_to_string(&self.calls)
            .unwrap_or_default()
            .lines()
            .filter(|l| l.contains("estate.test"))
            .map(str::to_string)
            .collect()
    }
}

impl Drop for Curl {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Where a run happens, as the five variables the declaration's runtime
/// guard reads (lib/no-estate.sh). Every place names all five, and
/// `None` removes one, so the environment this test itself runs in — a
/// session on the dev pod, a gate-runner Job, a GitHub runner on the
/// mirror — never decides a case.
type Place = [(&'static str, Option<&'static str>); 5];

/// The public mirror: a GitHub-hosted runner on github.com. The one
/// place the declaration may be honoured.
const GITHUB_RUNNER: Place = [
    ("GITHUB_ACTIONS", Some("true")),
    ("GITHUB_SERVER_URL", Some("https://github.com")),
    ("FORGEJO_ACTIONS", None),
    ("GITEA_ACTIONS", None),
    ("KUBERNETES_SERVICE_HOST", None),
];
/// The cluster gate-runner Job: its container carries
/// `KUBERNETES_SERVICE_HOST` (measured by the review of cc7273c0), and
/// it is exactly where the estate must be read. Planted here with every
/// GitHub variable claimed as well, the worst case for the guard.
const THE_GATE_RUNNER_JOB: Place = [
    ("GITHUB_ACTIONS", Some("true")),
    ("GITHUB_SERVER_URL", Some("https://github.com")),
    ("FORGEJO_ACTIONS", None),
    ("GITEA_ACTIONS", None),
    ("KUBERNETES_SERVICE_HOST", Some("10.96.0.1")),
];
/// A forge Actions job: `.forgejo/workflows/ci.yml` runs `runs-on:
/// docker` on the forge host, off the cluster, and Forgejo sets
/// `GITHUB_ACTIONS=true` beside `FORGEJO_ACTIONS` (the review of
/// cc7273c0 reproduced all three lints honouring the declaration
/// there). The server URL is the forge's, spelt as a placeholder host.
const A_FORGE_ACTIONS_JOB: Place = [
    ("GITHUB_ACTIONS", Some("true")),
    ("GITHUB_SERVER_URL", Some("http://forge.test:3000")),
    ("FORGEJO_ACTIONS", Some("true")),
    ("GITEA_ACTIONS", None),
    ("KUBERNETES_SERVICE_HOST", None),
];
/// A Gitea runner that even claims github.com as its server — the
/// runner-kind variable decides before the URL does.
const A_GITEA_RUNNER_CLAIMING_GITHUB: Place = [
    ("GITHUB_ACTIONS", Some("true")),
    ("GITHUB_SERVER_URL", Some("https://github.com")),
    ("FORGEJO_ACTIONS", None),
    ("GITEA_ACTIONS", Some("true")),
    ("KUBERNETES_SERVICE_HOST", None),
];
/// GitHub's Actions protocol on another server (Enterprise, a
/// self-hosted clone): not the public mirror.
const ACTIONS_ON_ANOTHER_SERVER: Place = [
    ("GITHUB_ACTIONS", Some("true")),
    ("GITHUB_SERVER_URL", Some("https://git.example.com")),
    ("FORGEJO_ACTIONS", None),
    ("GITEA_ACTIONS", None),
    ("KUBERNETES_SERVICE_HOST", None),
];
/// No Actions runner at all: a session on the dev pod (sshd and tmux
/// start it without `KUBERNETES_SERVICE_HOST`, measured by the review
/// of cc7273c0, so it is caught here and not by the pod check), a
/// workstation, the forge host outside a job.
const NO_ACTIONS_RUNNER: Place = [
    ("GITHUB_ACTIONS", None),
    ("GITHUB_SERVER_URL", None),
    ("FORGEJO_ACTIONS", None),
    ("GITEA_ACTIONS", None),
    ("KUBERNETES_SERVICE_HOST", None),
];

/// Run `infra/lint/<lint>.sh` from `root` on a GitHub runner; see
/// [`run_in`].
fn run(root: &Path, curl: &Curl, lint: &str, url_var: &str, estate: Option<&str>) -> Output {
    run_in(root, curl, lint, url_var, estate, GITHUB_RUNNER)
}

/// Run `infra/lint/<lint>.sh` from `root` with the estate at [`ESTATE`],
/// no roll wait, `place` as given, and `BOSS_ESTATE` as given — `None`
/// REMOVES it, so a declaration in the environment this test runs in
/// (the mirror sets one) cannot decide a case that is about its absence.
fn run_in(
    root: &Path,
    curl: &Curl,
    lint: &str,
    url_var: &str,
    estate: Option<&str>,
    place: Place,
) -> Output {
    let mut cmd = Command::new("bash");
    for (k, v) in place {
        match v {
            Some(v) => cmd.env(k, v),
            None => cmd.env_remove(k),
        };
    }
    cmd.arg(root.join(format!("infra/lint/{lint}.sh")))
        .current_dir(root)
        .stdin(Stdio::null())
        .env("PATH", curl.path())
        .env(url_var, ESTATE)
        .env("BOSS_SOR_WAIT_SECONDS", "0")
        .env("GIT_CEILING_DIRECTORIES", root.parent().unwrap_or(root));
    match estate {
        Some(v) => cmd.env("BOSS_ESTATE", v),
        None => cmd.env_remove("BOSS_ESTATE"),
    };
    cmd.output()
        .unwrap_or_else(|e| panic!("run {lint} in {}: {e}", root.display()))
}

fn text(out: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The count a lint printed on its `<lint>: scanned <N>` line.
fn scanned_count(out: &str) -> Option<u64> {
    out.lines().find_map(|l| {
        let (_, rest) = l.split_once(": scanned ")?;
        rest.split_whitespace().next()?.parse().ok()
    })
}

/// THE DECLARATION, on this tree. Each live lint exits 0 without asking
/// the estate anything, prints the marker with the declaration's own
/// words, and claims no comparison: no `OK —` line. The two scanners
/// still print a positive `scanned` count — the tree half ran and says
/// how much it read — because the lint-of-lints sweep
/// (`a_lint_that_scanned_nothing_is_red.rs`) inherits this environment
/// on the mirror and holds a clean exit to exactly that.
#[test]
fn the_declaration_skips_the_live_half_of_each_live_lint_and_says_so() {
    let root = repo_root();
    let marker = marker();
    for (lint, var) in LIVE_LINTS {
        let curl = Curl::new(&format!("declared-{lint}"));
        let out = run(&root, &curl, lint, var, Some("none"));
        let (stdout, stderr) = text(&out);
        assert_eq!(
            out.status.code(),
            Some(0),
            "{lint}: under BOSS_ESTATE=none the live half is not run and the tree half is \
             clean, so the lint passes:\n{stdout}\n{stderr}"
        );
        assert!(
            stderr.contains(&marker) && stderr.contains("BOSS_ESTATE=none"),
            "{lint}: the lint SAYS the live comparison was not made, and why — the marker \
             `{marker}` and the declaration by name:\n{stderr}"
        );
        assert_eq!(
            curl.estate_reads(),
            Vec::<String>::new(),
            "{lint}: a declared run asks the estate nothing"
        );
        assert!(
            !stdout.contains("OK —"),
            "{lint}: no comparison ran, so no line may read as one that did:\n{stdout}"
        );
        assert!(
            stdout
                .lines()
                .any(|l| l.starts_with(&format!("::warning title={marker}::{lint} "))),
            "{lint}: the GitHub checks page carries the same fact, as a ::warning:: \
             annotation on stdout:\n{stdout}"
        );
        if lint != "a-car-stays-under-the-edit-level" {
            let n = scanned_count(&format!("{stdout}\n{stderr}"));
            assert!(
                n.is_some_and(|n| n > 0),
                "{lint}: the tree half ran, and a scanner says how much it read \
                 (got {n:?}):\n{stdout}\n{stderr}"
            );
        }
    }
}

/// A synthetic tree holding the lint libs and one lint, for the cases a
/// real tree cannot plant.
fn synthetic_tree(tag: &str, lint: &str) -> PathBuf {
    let dir = boss_testing::scratch_dir(&format!("no-estate-tree-{tag}"));
    let tree = dir.join("tree");
    boss_testing::copy_lint_libs(&tree);
    let rel = format!("infra/lint/{lint}.sh");
    boss_testing::write_exec(
        &tree.join(&rel),
        &std::fs::read_to_string(repo_root().join(&rel)).expect("read the lint"),
    );
    tree
}

fn plant(tree: &Path, rel: &str, body: &str) {
    let to = tree.join(rel);
    boss_testing::create_dir(to.parent().expect("a parent"));
    boss_testing::write_file(&to, body);
}

/// THE DECLARATION EXCUSES ONLY THE LIVE HALF. A tree the static half
/// refuses is still refused under `BOSS_ESTATE=none`, with the static
/// half's own words: an empty rule directory, a protocol written as a
/// Rust literal, a checkout git cannot read. Were the declaration an
/// early `exit 0`, all three would pass here.
#[test]
fn the_declaration_does_not_excuse_the_tree_half() {
    let n = cannot_answer();

    // The rules lint: an authored directory with no rule in it.
    let tree = synthetic_tree("rules", "the-live-rules-are-the-authored-rules");
    boss_testing::create_dir(&tree.join("infra/dispatcher/rules"));
    let curl = Curl::new("tree-rules");
    let out = run(
        &tree,
        &curl,
        "the-live-rules-are-the-authored-rules",
        "BOSS_DISPATCHER_URL",
        Some("none"),
    );
    let (stdout, stderr) = text(&out);
    assert_eq!(
        out.status.code(),
        Some(1),
        "an empty rule directory is the tree's red, declared estate or not:\n{stdout}\n{stderr}"
    );
    assert!(
        stderr.contains("no *.toml in infra/dispatcher/rules"),
        "{stderr}"
    );

    // The protocols lint: a protocol authored as a Rust literal.
    let tree = synthetic_tree("protocols", "the-live-protocols-are-the-authored-protocols");
    plant(
        &tree,
        "infra/platform/workflows/alpha.toml",
        "[[workflow]]\nkind = \"alpha\"\nlabel = \"Alpha\"\n",
    );
    plant(
        &tree,
        "examples/fixture/seeds/workflows.toml",
        "[[workflow]]\nkind = \"beta\"\nlabel = \"Beta\"\n",
    );
    plant(
        &tree,
        "crates/core/boss-jobs/src/registry.rs",
        "pub fn platform_workflows() -> Vec<Spec> {\n    vec![maintenance_spec(\"gamma\")]\n}\n",
    );
    let curl = Curl::new("tree-protocols");
    let out = run(
        &tree,
        &curl,
        "the-live-protocols-are-the-authored-protocols",
        "BOSS_JOBS_URL",
        Some("none"),
    );
    let (stdout, stderr) = text(&out);
    assert_eq!(
        out.status.code(),
        Some(1),
        "a Rust-literal protocol is the tree's red, declared estate or not:\n{stdout}\n{stderr}"
    );
    assert!(stderr.contains("as Rust literals"), "{stderr}");
    assert_eq!(curl.estate_reads(), Vec::<String>::new());

    // The edit-level lint: its tree half is the diff, and git cannot
    // read a directory that is not a repository.
    let tree = synthetic_tree("edit-level", "a-car-stays-under-the-edit-level");
    plant(
        &tree,
        "infra/platform/tiers.toml",
        &std::fs::read_to_string(repo_root().join("infra/platform/tiers.toml"))
            .expect("read the tier map"),
    );
    let curl = Curl::new("tree-edit-level");
    let out = run(
        &tree,
        &curl,
        "a-car-stays-under-the-edit-level",
        "BOSS_JOBS_URL",
        Some("none"),
    );
    let (stdout, stderr) = text(&out);
    assert_eq!(
        out.status.code(),
        Some(n),
        "a checkout git cannot read is still CANNOT ANSWER under the declaration — the \
         paths are the tree half, and they were never read:\n{stdout}\n{stderr}"
    );
    assert!(stderr.contains("git"), "{stderr}");
    assert_eq!(curl.estate_reads(), Vec::<String>::new());
}

/// FAIL CLOSED EVERYWHERE ELSE. Without the declaration — unset, or
/// set empty — an unreachable estate is still exit 3 with the marker
/// and no scanned line, which the gate turns into a refusal; and a
/// value the tree does not know (`None`, `off`) is refused as well,
/// naming the variable and the value, never read as either answer.
#[test]
fn without_the_declaration_an_unreachable_estate_still_refuses() {
    let root = repo_root();
    let n = cannot_answer();
    for (lint, var) in LIVE_LINTS {
        for (case, estate) in [
            ("unset", None),
            ("empty", Some("")),
            ("capitalised", Some("None")),
            ("another word", Some("off")),
        ] {
            let curl = Curl::new(&format!("undeclared-{lint}"));
            let out = run(&root, &curl, lint, var, estate);
            let (stdout, stderr) = text(&out);
            assert_eq!(
                out.status.code(),
                Some(n),
                "{lint} [{case}]: no declaration this tree knows, so an unreachable estate \
                 is CANNOT ANSWER (exit {n}):\n{stdout}\n{stderr}"
            );
            assert!(
                stderr.contains("CANNOT ANSWER"),
                "{lint} [{case}]:\n{stderr}"
            );
            assert_eq!(
                scanned_count(&format!("{stdout}\n{stderr}")),
                None,
                "{lint} [{case}]: a refusal prints no scanned line:\n{stdout}\n{stderr}"
            );
            assert!(
                !stderr.contains(&marker()),
                "{lint} [{case}]: only the declaration prints the no-estate marker:\n{stderr}"
            );
            if let Some(v) = estate.filter(|v| !v.is_empty()) {
                assert!(
                    stderr.contains(&format!("BOSS_ESTATE={v}")),
                    "{lint} [{case}]: an unknown declaration is refused BY NAME, with its \
                     value, so the misspelling is the first thing a reader sees:\n{stderr}"
                );
            }
        }
    }
}

/// THE RUNTIME GUARD (the adversarial review of c9439bd1). A text pin
/// can only refuse the spellings it knows; a declaration that leaks
/// into a cluster gate by one it does not know — a flow-map env entry,
/// a Dockerfile `ENV`, an inline prefix — would pass every car without
/// its live comparisons. So the lib itself refuses `none` anywhere but
/// a GitHub-hosted runner on github.com. The re-review of cc7273c0
/// showed `GITHUB_ACTIONS=true` alone is not that — Forgejo sets it for
/// every forge Actions job — so each place below is refused, and each
/// refusal is exit 3 naming the variable that decided it, prints no
/// marker and no scanned line, and asks the estate nothing.
#[test]
fn a_declaration_anywhere_but_a_github_hosted_runner_is_refused() {
    let root = repo_root();
    let n = cannot_answer();
    for (lint, var) in LIVE_LINTS {
        for (case, place, names) in [
            (
                "the gate-runner Job",
                THE_GATE_RUNNER_JOB,
                "KUBERNETES_SERVICE_HOST",
            ),
            (
                "a forge Actions job",
                A_FORGE_ACTIONS_JOB,
                "FORGEJO_ACTIONS",
            ),
            (
                "a Gitea runner claiming github.com",
                A_GITEA_RUNNER_CLAIMING_GITHUB,
                "GITEA_ACTIONS",
            ),
            (
                "Actions on another server",
                ACTIONS_ON_ANOTHER_SERVER,
                "GITHUB_SERVER_URL",
            ),
            ("no Actions runner", NO_ACTIONS_RUNNER, "GITHUB_ACTIONS"),
        ] {
            let curl = Curl::new(&format!("guard-{lint}"));
            let out = run_in(&root, &curl, lint, var, Some("none"), place);
            let (stdout, stderr) = text(&out);
            assert_eq!(
                out.status.code(),
                Some(n),
                "{lint} [{case}]: BOSS_ESTATE=none here is a leak, refused as CANNOT ANSWER \
                 (exit {n}), never honoured:\n{stdout}\n{stderr}"
            );
            assert!(
                stderr.contains("CANNOT ANSWER") && stderr.contains(names),
                "{lint} [{case}]: the refusal names {names}, the fact that decided it:\n{stderr}"
            );
            assert!(
                !stderr.contains(&marker()),
                "{lint} [{case}]: a refused declaration does not claim to have skipped \
                 anything:\n{stderr}"
            );
            assert_eq!(
                scanned_count(&format!("{stdout}\n{stderr}")),
                None,
                "{lint} [{case}]: a refusal prints no scanned line:\n{stdout}\n{stderr}"
            );
            assert_eq!(curl.estate_reads(), Vec::<String>::new());
        }
    }
}

/// The list above is the tree's: every lint that reads the estate
/// (sources `lib/sor-read.sh`) honours the declaration (sources
/// `lib/no-estate.sh`), and no other lint does. A fourth live lint
/// added without honouring it would refuse every mirror run again; one
/// honouring it without reading the estate would be skipping something
/// that is not a live half.
#[test]
fn the_live_lints_are_exactly_the_ones_that_read_the_estate() {
    let root = repo_root();
    let sourcing = |lib: &str| -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(root.join("infra/lint"))
            .expect("read infra/lint")
            .map(|e| e.expect("an entry").path())
            .filter(|p| p.extension().is_some_and(|e| e == "sh"))
            .filter(|p| {
                std::fs::read_to_string(p)
                    .unwrap_or_default()
                    .lines()
                    .any(|l| l.trim_start().starts_with(". ") && l.contains(lib))
            })
            .map(|p| {
                p.file_stem()
                    .expect("a stem")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    };
    let mut want: Vec<String> = LIVE_LINTS.iter().map(|(l, _)| l.to_string()).collect();
    want.sort();
    assert_eq!(
        sourcing("lib/sor-read.sh"),
        want,
        "the lints that read the estate"
    );
    assert_eq!(
        sourcing("lib/no-estate.sh"),
        want,
        "the lints that honour BOSS_ESTATE=none are exactly the ones that read the estate"
    );
}

// ---- only the mirror declares it ----
//
// The first version of this pin recognised SETTER SHAPES — a YAML key,
// an `export`, a systemd `Environment=` — and the adversarial review of
// c9439bd1 showed it missing five that would leak the declaration into
// a cluster gate: a flow-map env entry (`- {name: BOSS_ESTATE, value:
// "none"}`, the style of infra/gate-runner/gate-runner.yaml), a
// Dockerfile `ENV`, a Rust `.env("BOSS_ESTATE", …)`, `env BOSS_ESTATE=…`
// and an inline `… && BOSS_ESTATE=none ./infra/gate.sh`. Enumerating
// spellings loses to the next spelling, so the scan is inverted: ANY
// mention of the variable outside a comment is a leak unless the file
// is one of the few with a reason to name it, and even those may only
// read it or print it. The runtime guard in lib/no-estate.sh is the
// other half — this pin cannot see a variable set outside the tree.

/// The one file that declares, and the one line it declares with.
const DECLARER: &str = ".github/workflows/ci.yml";
const DECLARATION: &str = "BOSS_ESTATE: none";

/// The files that define and honour the declaration. They may READ it
/// (`$BOSS_ESTATE`, `${BOSS_ESTATE:-}`) and PRINT it (a line that is an
/// `echo` or a `printf` and nothing else), and never set it.
const DEFINERS: [&str; 5] = [
    "infra/lint/lib/no-estate.sh",
    "infra/lint/the-live-protocols-are-the-authored-protocols.sh",
    "infra/lint/the-live-rules-are-the-authored-rules.sh",
    "infra/lint/a-car-stays-under-the-edit-level.sh",
    "infra/gate.sh",
];

/// Tests here set the variable on the child processes they spawn to
/// exercise the lints, which is where a setting belongs in a test.
const TESTS_DIR: &str = "crates/core/boss-testing/tests/";

/// Does `line` name the variable as a whole word (not
/// `BOSS_ESTATE_NODES_URL`)?
fn mentions(line: &str) -> bool {
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    line.match_indices("BOSS_ESTATE").any(|(i, m)| {
        !line[..i].chars().next_back().is_some_and(word)
            && !line[i + m.len()..].chars().next().is_some_and(word)
    })
}

fn is_comment(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with('#') || t.starts_with("//")
}

/// A definer's line that only reads the variable or only prints it.
fn reads_or_prints(line: &str) -> bool {
    let t = line.trim();
    let has = |needles: &[&str]| needles.iter().any(|s| t.contains(s));
    // Anything that ASSIGNS or WRITES, whatever the line starts with:
    // `printf -v` sets a variable, and a redirect or `tee` into
    // `$GITHUB_ENV` declares one for every later step of a job (the
    // re-review of cc7273c0).
    let writes = has(&["export", ":=", "printf -v", ">", "tee "]);
    // A print is one plain echo/printf: no second command beside it.
    let chained = has(&["env ", "ENV ", "&&", "||", ";", "`", "|"]);
    let prints = t.starts_with("printf ") || t.starts_with("echo ");
    let reads =
        (t.contains("$BOSS_ESTATE") || t.contains("${BOSS_ESTATE")) && !t.contains("BOSS_ESTATE=");
    // A read inside a test (`[ -n "${BOSS_ESTATE:-}" ]; then`) carries a
    // `;` legitimately, and a read piped through `tr` a `|`; a print
    // needs neither.
    !writes && ((prints && !chained) || reads)
}

/// Every line of `text` (the file at `rel`) that leaks the declaration,
/// as `rel:line: text`. Empty is clean.
fn estate_leaks(rel: &str, text: &str) -> Vec<String> {
    if rel.starts_with(TESTS_DIR) || rel.ends_with(".md") {
        return Vec::new();
    }
    let named: Vec<(usize, &str)> = text
        .lines()
        .enumerate()
        .filter(|(_, l)| !is_comment(l) && mentions(l))
        .map(|(i, l)| (i + 1, l))
        .collect();
    let at = |(n, l): &(usize, &str)| format!("{rel}:{n}: {}", l.trim());
    if rel == DECLARER {
        let mut leaks: Vec<String> = named
            .iter()
            .filter(|(_, l)| l.trim() != DECLARATION)
            .map(at)
            .collect();
        if named.len() > 1 {
            leaks.push(format!(
                "{rel}: declares {} times; once is the rule",
                named.len()
            ));
        }
        return leaks;
    }
    if DEFINERS.contains(&rel) {
        return named
            .iter()
            .filter(|(_, l)| !reads_or_prints(l))
            .map(at)
            .collect();
    }
    named.iter().map(at).collect()
}

/// The scan, against every spelling the review named and the forms the
/// tree legitimately carries.
#[test]
fn the_leak_scan_catches_every_spelling_the_review_named() {
    for (rel, text) in [
        (
            "infra/gate-runner/gate-runner.yaml",
            "        - {name: BOSS_ESTATE, value: \"none\"}",
        ),
        (
            "infra/gate-runner/gate-runner.yaml",
            "        - name: BOSS_ESTATE\n          value: none",
        ),
        ("infra/forge/boss-ci/Dockerfile", "ENV BOSS_ESTATE=none"),
        (
            "crates/orchestrators/boss-cli/src/gate.rs",
            "    cmd.env(\"BOSS_ESTATE\",\"none\");",
        ),
        (
            "infra/gate-runner/run.sh",
            "env BOSS_ESTATE=none bash infra/gate.sh",
        ),
        (
            "infra/gate-runner/run.sh",
            "cd /w && BOSS_ESTATE=none ./infra/gate.sh",
        ),
        ("infra/boss-gate.service", "Environment=BOSS_ESTATE=none"),
        (".forgejo/workflows/ci.yml", "      BOSS_ESTATE: none"),
        (DECLARER, "      run: BOSS_ESTATE=none infra/gate.sh"),
        (DECLARER, "      BOSS_ESTATE: none\n      BOSS_ESTATE: none"),
        ("infra/gate.sh", "cd /w && BOSS_ESTATE=none ./infra/gate.sh"),
        ("infra/gate.sh", "export BOSS_ESTATE=none"),
        ("infra/gate.sh", ": \"${BOSS_ESTATE:=none}\""),
        ("infra/gate.sh", "echo hi; export BOSS_ESTATE=none"),
        ("infra/gate.sh", "BOSS_ESTATE=none"),
        // The re-review of cc7273c0: a line that starts with a print
        // can still set or declare. `printf -v` assigns, and a redirect
        // or a pipe into `$GITHUB_ENV` declares the variable for every
        // later step of the job.
        ("infra/gate.sh", "printf -v BOSS_ESTATE '%s' none"),
        ("infra/gate.sh", "echo BOSS_ESTATE=none >> \"$GITHUB_ENV\""),
        (
            "infra/gate.sh",
            "printf 'BOSS_ESTATE=none\\n' > \"$GITHUB_ENV\"",
        ),
        (
            "infra/gate.sh",
            "echo BOSS_ESTATE=none | tee -a \"$GITHUB_ENV\"",
        ),
        (
            "infra/gate.sh",
            "echo \"BOSS_ESTATE=$BOSS_ESTATE\" >> \"$GITHUB_ENV\"",
        ),
        (
            "infra/lint/lib/no-estate.sh",
            "    declared=\"${BOSS_ESTATE}\" && printf '%s' x > \"$OUT\"",
        ),
    ] {
        assert!(
            !estate_leaks(rel, text).is_empty(),
            "{rel}: this spelling leaks the declaration and must be refused:\n{text}"
        );
    }
    for (rel, text) in [
        (
            DECLARER,
            "  # BOSS_ESTATE, said out loud\n      BOSS_ESTATE: none",
        ),
        (
            "infra/lint/lib/no-estate.sh",
            "    case \"${BOSS_ESTATE:-}\" in",
        ),
        ("infra/gate.sh", "    if [ -n \"${BOSS_ESTATE:-}\" ]; then"),
        (
            "infra/gate.sh",
            "        printf 'gate: NO ESTATE — BOSS_ESTATE=%s — the live half' \\",
        ),
        (
            "infra/forge/boss-ci/Dockerfile",
            "# BOSS_ESTATE is never set here",
        ),
        (
            "crates/core/boss-testing/tests/x.rs",
            ".env(\"BOSS_ESTATE\", \"none\")",
        ),
        ("infra/node-roles.sh", "BOSS_ESTATE_NODES_URL=http://x"),
    ] {
        assert_eq!(
            estate_leaks(rel, text),
            Vec::<String>::new(),
            "{rel}: a read, a print, a comment or another variable is not a leak:\n{text}"
        );
    }
}

/// Only the mirror declares it, on the tree. Every tracked file that
/// names the variable is read with [`estate_leaks`], and the declarer
/// declares `none`, once.
#[test]
fn only_the_public_mirror_declares_no_estate() {
    let root = repo_root();
    let out = Command::new("git")
        .args(["grep", "-lw", "BOSS_ESTATE"])
        .current_dir(&root)
        .output()
        .expect("git grep");
    assert!(
        out.status.success(),
        "git grep found no mention of BOSS_ESTATE at all — the declaration is gone:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let leaks: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .flat_map(|rel| {
            let text = std::fs::read_to_string(root.join(rel))
                .unwrap_or_else(|e| panic!("read {rel}: {e}"));
            estate_leaks(rel, &text)
        })
        .collect();
    assert!(
        leaks.is_empty(),
        "BOSS_ESTATE=none is the public mirror's declaration alone; anywhere else it \
         would excuse a live check on a machine that has an estate to read. Named \
         outside a comment where it must not be:\n{}",
        leaks.join("\n")
    );
    let yml = std::fs::read_to_string(root.join(DECLARER)).expect("ci.yml");
    assert!(
        yml.lines().any(|l| l.trim() == DECLARATION),
        "the mirror declares the one word the lints know, `none`; any other value is \
         refused and would red every publish PR again"
    );
}

// ---- the receipt ----

/// A synthetic repository with this tree's gate and a planted lint that
/// honours the declaration exactly as the three real ones do, run as
/// the gate proper with every compiled phase stubbed — the shape
/// `a_lint_that_cannot_answer_refuses_the_gate.rs` uses.
struct Gate {
    dir: PathBuf,
    tree: PathBuf,
}

const STUB: &str = "a-stub-that-reads-the-estate";

impl Gate {
    fn new(tag: &str) -> Gate {
        let dir = boss_testing::scratch_dir(&format!("no-estate-gate-{tag}"));
        let tree = dir.join("tree");
        boss_testing::copy_lint_libs(&tree);
        boss_testing::copy_gate_sh(&tree);
        boss_testing::write_file(
            &tree.join("infra/lint/workspace-declares-what-it-runs.sh"),
            "#!/usr/bin/env bash\nexit 0\n",
        );
        boss_testing::write_exec(
            &tree.join(format!("infra/lint/{STUB}.sh")),
            &format!(
                "#!/usr/bin/env bash\n\
                 . infra/lint/lib/git-answer.sh || exit 3\n\
                 . infra/lint/lib/no-estate.sh || exit 3\n\
                 if lint_no_estate {STUB} 'the stub registry'; then\n\
                 \x20   echo '{STUB}: tree half clean'\n\
                 \x20   exit 0\n\
                 fi\n\
                 echo '{STUB}: '\"$LINT_CANNOT_ANSWER_MARKER\"' — the stub registry answered HTTP 000' >&2\n\
                 exit \"$LINT_CANNOT_ANSWER\"\n"
            ),
        );
        for web in ["apps/web", "apps/simulator", "libs/web-kit"] {
            boss_testing::create_dir(&tree.join(web));
        }
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["add", "."][..],
            &[
                "-c",
                "user.email=no-estate@test",
                "-c",
                "user.name=no-estate",
                "commit",
                "-q",
                "-m",
                "the gate under test",
            ][..],
        ] {
            let out = Command::new("git")
                .args(args)
                .current_dir(&tree)
                .output()
                .expect("git");
            assert!(out.status.success(), "git {args:?}: {out:?}");
        }
        let bin = dir.join("bin");
        boss_testing::create_dir(&bin);
        for tool in ["cargo", "bun"] {
            boss_testing::write_exec(&bin.join(tool), "#!/usr/bin/env bash\nexit 0\n");
        }
        boss_testing::write_exec(
            &dir.join("df"),
            "#!/usr/bin/env bash\n\
             echo 'Filesystem 1024-blocks Used Available Capacity Mounted on'\n\
             echo '/dev/fake 1 1 943718400 1% /'\n",
        );
        Gate { dir, tree }
    }

    fn receipt(&self) -> PathBuf {
        self.dir.join("receipt.json")
    }

    fn run(&self, estate: Option<&str>) -> Output {
        let path = std::env::var("PATH").unwrap_or_default();
        let mut cmd = Command::new("bash");
        cmd.arg(self.tree.join("infra/gate.sh"))
            .current_dir(&self.tree)
            .stdin(Stdio::null())
            .env("PATH", format!("{}:{path}", self.dir.join("bin").display()))
            .env("BOSS_GATE_DF_CMD", self.dir.join("df"))
            .env("BOSS_GATE_MIN_FREE_GB", "12")
            .env("BOSS_GATE_RECEIPT", self.receipt())
            .env("BOSS_GATE_TRUNK", "HEAD")
            .env("GIT_CEILING_DIRECTORIES", "");
        match estate {
            Some(v) => cmd.env("BOSS_ESTATE", v),
            None => cmd.env_remove("BOSS_ESTATE"),
        };
        // The one place the declaration is honoured: a GitHub runner.
        for (k, v) in GITHUB_RUNNER {
            match v {
                Some(v) => cmd.env(k, v),
                None => cmd.env_remove(k),
            };
        }
        cmd.output().expect("run gate.sh")
    }

    fn read_receipt(&self, t: &str) -> serde_json::Value {
        let raw = std::fs::read_to_string(self.receipt())
            .unwrap_or_else(|e| panic!("the gate wrote a receipt: {e}\n{t}"));
        serde_json::from_str(&raw).unwrap_or_else(|e| panic!("the receipt is JSON: {e}\n{raw}"))
    }
}

impl Drop for Gate {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// THE RECEIPT NAMES IT. Under the declaration the gate is not refused
/// over the live lint, and its receipt carries `estate`: the declared
/// word, and every lint whose live half did not run — derived from what
/// the lints printed, not from a list in the gate. The lint's own check
/// entry is `pass`, and the gate's output says the same in one line.
/// The control, undeclared: the same lint refuses the gate (exit 2) and
/// the receipt carries no `estate` at all.
#[test]
fn the_receipt_names_the_declaration_and_every_lint_that_did_not_read_the_estate() {
    let gate = Gate::new("declared");
    let out = gate.run(Some("none"));
    let t = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_ne!(
        out.status.code(),
        Some(2),
        "a declared run is not refused over a live half it was told not to run:\n{t}"
    );
    let receipt = gate.read_receipt(&t);
    assert_eq!(
        receipt["estate"]["declared"], "none",
        "the receipt records the declaration:\n{receipt:#}"
    );
    assert_eq!(
        receipt["estate"]["live_not_run"],
        serde_json::json!([STUB]),
        "the receipt names every lint whose live half did not run:\n{receipt:#}"
    );
    let entry = receipt["checks"]
        .as_array()
        .and_then(|c| c.iter().find(|c| c["name"] == STUB))
        .unwrap_or_else(|| panic!("the stub lint is in the checks list:\n{receipt:#}"));
    assert_eq!(entry["result"], "pass", "{receipt:#}");
    assert!(
        t.lines()
            .any(|l| l.contains("BOSS_ESTATE=none") && l.contains(STUB)),
        "the gate's own output names the declaration and the lint it excused:\n{t}"
    );

    let gate = Gate::new("undeclared");
    let out = gate.run(None);
    let t = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        out.status.code(),
        Some(2),
        "undeclared, a live lint that cannot answer still refuses the gate:\n{t}"
    );
    let receipt = gate.read_receipt(&t);
    assert_eq!(receipt["verdict"], "refused", "{receipt:#}");
    assert!(
        receipt.get("estate").is_none(),
        "no declaration, no `estate` on the receipt:\n{receipt:#}"
    );
}
