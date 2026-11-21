//! `BOSS_TRUNK=none` — the public mirror's declaration that it has no
//! per-car trunk — makes every lint that judges "what this change added
//! or altered against the trunk merge-base" skip LOUDLY, and nothing else.
//!
//! MEASURED 2026-11-16 (backlog 2d28df78, design e2710fbc). On the forge
//! the trunk is the main a car merges into, so "this change" is one car.
//! On the public mirror (`.github/workflows/ci.yml`, `infra/gate.sh`
//! full) the only trunk is the previous publish, so "this change" is
//! every forge car since then, judged as one. Publish d2e189f5 (PR 258):
//! factory-vocabulary classed 711 files as added against merge-base
//! 5ef67f02, the 2026-10-22 publish, and found 364 hit lines in 27 files
//! that every forge car and every forge train gate had passed; the same
//! tree against the forge's previous main added 0. A rule that lands
//! partway through a publish window reds every publish that spans it, so
//! public publishing stopped.
//!
//! THE CALL. Not a replay and not a grandfathering rule — both rebuild or
//! add a judgement the forge already made or never made. The mirror SAYS
//! it has no per-car trunk, beside `BOSS_ESTATE=none` and with the same
//! runtime guard (`lib/public-mirror.sh`), and each of the lints that
//! source `lib/trunk-ref.sh` prints that its comparison was not made and
//! why, and passes; the gate's receipt names them under `trunk`. The
//! forge's per-car receipts are the evidence for those judgements; the
//! forge gates judge exactly as before, because the declaration is
//! refused anywhere but a GitHub-hosted runner on github.com.

use boss_testing::repo_root;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// The lints that judge against the trunk merge-base. Held to the tree by
/// `the_lints_that_honour_the_declaration_are_exactly_the_trunk_relative_ones`,
/// which derives both sets from `infra/lint/`, so this list only says
/// which lints the fixture runs.
const TRUNK_LINTS: [&str; 8] = [
    "a-car-stays-under-the-edit-level",
    "a-new-style-has-a-caller",
    "a-rule-edit-bumps-its-version",
    "break-glass-awaiting-re-enrolment-only-shrinks",
    "factory-vocabulary",
    "migrations-append-only",
    "native-proof-evidence-is-adversarially-checked",
    "steptype-bundle-ratchet",
];

/// The two of them that are scanners (`a_lint_that_scanned_nothing_is_red.rs`
/// holds a scanner's clean exit to a positive `scanned <N>` line, and on
/// the mirror that sweep inherits the declaration).
const SCANNERS: [&str; 2] = ["factory-vocabulary", "steptype-bundle-ratchet"];

/// `NAME="value"` read from its one definition in a lint lib, never typed
/// here (CLAUDE.md §9a).
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
    lib_value("trunk-ref.sh", "LINT_NO_TRUNK_MARKER")
}

fn cannot_answer() -> i32 {
    lib_value("git-answer.sh", "LINT_CANNOT_ANSWER")
        .parse()
        .expect("LINT_CANNOT_ANSWER is a number")
}

/// Where a run happens, as the five variables the shared runtime guard
/// reads (`lib/public-mirror.sh`). Every place names all five and `None`
/// removes one, so the environment this test runs in — the dev pod, a
/// gate-runner Job, the mirror itself — never decides a case.
type Place = [(&'static str, Option<&'static str>); 5];

/// The public mirror: a GitHub-hosted runner on github.com.
const GITHUB_RUNNER: Place = [
    ("GITHUB_ACTIONS", Some("true")),
    ("GITHUB_SERVER_URL", Some("https://github.com")),
    ("FORGEJO_ACTIONS", None),
    ("GITEA_ACTIONS", None),
    ("KUBERNETES_SERVICE_HOST", None),
];
/// The cluster gate-runner Job, claiming every GitHub variable as well —
/// the worst case for the guard. This is where every car IS judged.
const THE_GATE_RUNNER_JOB: Place = [
    ("GITHUB_ACTIONS", Some("true")),
    ("GITHUB_SERVER_URL", Some("https://github.com")),
    ("FORGEJO_ACTIONS", None),
    ("GITEA_ACTIONS", None),
    ("KUBERNETES_SERVICE_HOST", Some("10.96.0.1")),
];
/// A forge Actions job: Forgejo sets `GITHUB_ACTIONS=true` beside its own.
const A_FORGE_ACTIONS_JOB: Place = [
    ("GITHUB_ACTIONS", Some("true")),
    ("GITHUB_SERVER_URL", Some("http://forge.test:3000")),
    ("FORGEJO_ACTIONS", Some("true")),
    ("GITEA_ACTIONS", None),
    ("KUBERNETES_SERVICE_HOST", None),
];
/// No Actions runner at all: a session on the dev pod, a workstation.
const NO_ACTIONS_RUNNER: Place = [
    ("GITHUB_ACTIONS", None),
    ("GITHUB_SERVER_URL", None),
    ("FORGEJO_ACTIONS", None),
    ("GITEA_ACTIONS", None),
    ("KUBERNETES_SERVICE_HOST", None),
];

fn git(dir: &Path, args: &[&str]) {
    let out = boss_testing::git_config_isolated(&mut Command::new("git"))
        .args(["-c", "user.email=no-trunk@test", "-c", "user.name=no-trunk"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("spawn git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "fixture git {args:?}: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn plant(tree: &Path, rel: &str, body: &str) {
    let to = tree.join(rel);
    boss_testing::create_dir(to.parent().expect("a parent"));
    boss_testing::write_file(&to, body);
}

/// A repository in the MIRROR'S shape: its only trunk (`origin/main`) is
/// an old publish, and HEAD carries the forge history since then — here,
/// one code file using a word a rule that landed in that window retires
/// (a station's capacity spelt in v1's word, refused in `crates/` since
/// the station tree's cutover — built at run time, so this file, which
/// factory-vocabulary also reads, does not carry it).
struct Mirror {
    dir: PathBuf,
    tree: PathBuf,
}

impl Mirror {
    fn new(tag: &str) -> Mirror {
        let dir = boss_testing::scratch_dir(&format!("no-trunk-{tag}"));
        let tree = dir.join("tree");
        boss_testing::copy_lint_libs(&tree);
        for lint in TRUNK_LINTS {
            let rel = format!("infra/lint/{lint}.sh");
            boss_testing::write_exec(
                &tree.join(&rel),
                &std::fs::read_to_string(repo_root().join(&rel)).expect("read the lint"),
            );
        }
        // What each lint reads before it asks for the trunk.
        plant(
            &tree,
            "apps/web/src/styles.css",
            ".a-panel { color: red; }\n",
        );
        plant(
            &tree,
            "crates/core/boss-jobs/seeds/step_types.toml",
            "[[step_type]]\nkind = \"task\"\n",
        );
        // native-proof-evidence-is-adversarially-checked runs its two
        // self-tests on the tree alone first; empty ones pass here.
        for t in ["native-evidence_test.py", "proof-observation_test.py"] {
            plant(&tree, &format!("infra/forge/{t}"), "\n");
        }
        git(&tree, &["init", "-q", "-b", "main"]);
        git(&tree, &["add", "-A"]);
        git(&tree, &["commit", "-q", "-m", "the previous publish"]);
        git(&tree, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        plant(
            &tree,
            "crates/core/boss-stations/src/capacity.rs",
            &format!(
                "pub const GATE: &str = \"two {}\";\n",
                ["b", "ays"].concat()
            ),
        );
        git(&tree, &["add", "-A"]);
        git(
            &tree,
            &["commit", "-q", "-m", "forge history since the publish"],
        );
        Mirror { dir, tree }
    }

    /// Run `infra/lint/<lint>.sh` at `place`, with `trunk` as BOSS_TRUNK
    /// (`None` REMOVES it — the mirror runs this suite with it declared)
    /// and `extra` set on top. No estate is declared and none is reachable.
    fn run(&self, lint: &str, trunk: Option<&str>, place: Place, extra: &[(&str, &str)]) -> Output {
        let mut cmd = Command::new("bash");
        boss_testing::git_config_isolated(&mut cmd);
        for (k, v) in place {
            match v {
                Some(v) => cmd.env(k, v),
                None => cmd.env_remove(k),
            };
        }
        match trunk {
            Some(v) => cmd.env("BOSS_TRUNK", v),
            None => cmd.env_remove("BOSS_TRUNK"),
        };
        cmd.arg(self.tree.join(format!("infra/lint/{lint}.sh")))
            .current_dir(&self.tree)
            .stdin(Stdio::null())
            .env_remove("BOSS_TRUNK_REF")
            .env_remove("BOSS_ESTATE")
            .env_remove("BOSS_EDIT_LEVEL_ANSWER")
            .env("BOSS_JOBS_URL", "http://[::1]:9")
            .env("BOSS_SOR_WAIT_SECONDS", "0")
            .env("GIT_CEILING_DIRECTORIES", &self.dir);
        for (k, v) in extra {
            cmd.env(k, v);
        }
        cmd.output()
            .unwrap_or_else(|e| panic!("run {lint} in {}: {e}", self.tree.display()))
    }
}

impl Drop for Mirror {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
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

/// THE CONTROL: the fixture is strong enough. Undeclared, on the same
/// runner, factory-vocabulary judges the publish window as one car and
/// reds on the file the forge added — the defect, reproduced.
#[test]
fn undeclared_the_mirror_shape_reds_on_history_the_forge_already_judged() {
    let m = Mirror::new("control");
    let out = m.run("factory-vocabulary", None, GITHUB_RUNNER, &[]);
    let (stdout, stderr) = text(&out);
    assert_eq!(
        out.status.code(),
        Some(1),
        "the mirror shape: a file added since the previous publish uses a word a later \
         rule retires, and undeclared the lint reds on it:\n{stdout}\n{stderr}"
    );
    assert!(
        stderr.contains("crates/core/boss-stations/src/capacity.rs:1:"),
        "{stderr}"
    );
    assert!(!stderr.contains(&marker()), "{stderr}");
}

/// THE DECLARATION. Each trunk-relative lint exits 0 on the mirror shape,
/// prints the marker naming itself and the declaration on stderr, carries
/// the same fact as a `::warning` annotation for the GitHub checks page,
/// and claims no comparison. A scanner still says how much it read.
#[test]
fn declared_every_trunk_relative_lint_skips_and_says_so() {
    let m = Mirror::new("declared");
    let marker = marker();
    for lint in TRUNK_LINTS {
        let out = m.run(lint, Some("none"), GITHUB_RUNNER, &[]);
        let (stdout, stderr) = text(&out);
        assert_eq!(
            out.status.code(),
            Some(0),
            "{lint}: under BOSS_TRUNK=none on the mirror it judges no car and passes:\n\
             {stdout}\n{stderr}"
        );
        assert!(
            stderr.contains(&format!("{lint}: {marker}")) && stderr.contains("BOSS_TRUNK=none"),
            "{lint}: the lint SAYS its trunk comparison was not made, and why — the marker \
             `{marker}` with its own name, and the declaration:\n{stderr}"
        );
        assert!(
            stdout
                .lines()
                .any(|l| l.starts_with(&format!("::warning title={marker}::{lint} "))),
            "{lint}: the GitHub checks page carries the same fact:\n{stdout}"
        );
        assert!(
            !stdout.contains(" ok ") && !stdout.contains("clean"),
            "{lint}: no comparison ran, so no line may read as one that did:\n{stdout}"
        );
        if SCANNERS.contains(&lint) {
            let n = scanned_count(&stdout);
            assert!(
                n.is_some_and(|n| n > 0),
                "{lint}: a scanner says how much it read even when it judged nothing \
                 (got {n:?}):\n{stdout}"
            );
        }
    }
}

/// THE GUARD. The declaration is honoured only where it is true. In a
/// gate-runner Job, a forge Actions job, or off Actions altogether it
/// would pass every car without its trunk judgements, so each is
/// refused, exit 3, naming the variable that decided it. So is a word
/// the tree does not know, and `none` beside a `BOSS_TRUNK_REF` that
/// names a trunk — the run cannot both have one and not.
#[test]
fn the_declaration_is_refused_anywhere_but_the_public_mirror() {
    let m = Mirror::new("guard");
    let n = cannot_answer();
    let marker = marker();
    for lint in TRUNK_LINTS {
        for (case, trunk, place, extra, names) in [
            (
                "the gate-runner Job",
                "none",
                THE_GATE_RUNNER_JOB,
                &[][..],
                "KUBERNETES_SERVICE_HOST",
            ),
            (
                "a forge Actions job",
                "none",
                A_FORGE_ACTIONS_JOB,
                &[][..],
                "FORGEJO_ACTIONS",
            ),
            (
                "no Actions runner",
                "none",
                NO_ACTIONS_RUNNER,
                &[][..],
                "GITHUB_ACTIONS",
            ),
            (
                "a misspelt value",
                "None",
                GITHUB_RUNNER,
                &[][..],
                "BOSS_TRUNK=None",
            ),
            (
                "a trunk named beside it",
                "none",
                GITHUB_RUNNER,
                &[("BOSS_TRUNK_REF", "origin/main")][..],
                "BOSS_TRUNK_REF",
            ),
        ] {
            let out = m.run(lint, Some(trunk), place, extra);
            let (stdout, stderr) = text(&out);
            assert_eq!(
                out.status.code(),
                Some(n),
                "{lint} [{case}]: refused as CANNOT ANSWER (exit {n}), never honoured:\n\
                 {stdout}\n{stderr}"
            );
            assert!(
                stderr.contains("CANNOT ANSWER") && stderr.contains(names),
                "{lint} [{case}]: the refusal names {names}:\n{stderr}"
            );
            assert!(
                !stderr.contains(&marker),
                "{lint} [{case}]: a refused declaration claims to have skipped nothing:\n{stderr}"
            );
        }
    }
}

fn lint_names_where(root: &Path, pred: impl Fn(&str) -> bool) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(root.join("infra/lint"))
        .expect("read infra/lint")
        .map(|e| e.expect("an entry").path())
        .filter(|p| p.extension().is_some_and(|e| e == "sh"))
        .filter(|p| pred(&std::fs::read_to_string(p).unwrap_or_default()))
        .map(|p| {
            p.file_stem()
                .expect("a stem")
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

/// Does this line of a lint compare against a trunk merge-base? A call to
/// the lib's `resolve_merge_base`, or git asked for a `merge-base` or a
/// three-dot range (`A...B` diffs from the merge-base of A and B). A
/// comment, a message naming the merge-base and a two-dot range do not.
fn compares_against_a_merge_base(line: &str) -> bool {
    let l = line.trim_start();
    if l.starts_with('#') {
        return false;
    }
    let three_dot = l.match_indices("...").any(|(i, _)| {
        let before = l[..i].chars().next_back();
        let after = l[i + 3..].chars().next();
        before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '}' || c == '_')
            && after.is_some_and(|c| c.is_ascii_alphanumeric() || c == '$')
    });
    l.contains("resolve_merge_base")
        || (l.contains("git") && (l.contains("merge-base") || three_dot))
}

#[test]
fn the_merge_base_classifier_sees_every_spelling_of_a_trunk_comparison() {
    for line in [
        "changed=\"$(git -C \"$repo\" diff --name-only origin/main...HEAD -- x/)\"",
        "git diff \"${BASE}...$head\"",
        "mb=$(git merge-base \"$BASE\" HEAD)",
        "mb=$(git_answer \"$LINT\" 0,1 merge-base \"$base\" HEAD)",
        "mb=$(resolve_merge_base \"$LINT\" \"$BASE\" HEAD) || exit $?",
    ] {
        assert!(
            compares_against_a_merge_base(line),
            "a trunk comparison: {line}"
        );
    }
    for line in [
        "# git diff origin/main...HEAD, said in a comment",
        "echo \"$LINT: $BUNDLE absent at merge-base — nothing to ratchet\"",
        "git log --oneline main..HEAD",
        "git_answer() { # <lint> <answer-statuses> <git args...>",
        "echo 'loading...done'",
    ] {
        assert!(
            !compares_against_a_merge_base(line),
            "not a trunk comparison: {line}"
        );
    }
}

/// Derived from the directory, three ways that must agree: the lints that
/// compare against a trunk merge-base (read off what they RUN, so a lint
/// carrying its own `git diff origin/main...HEAD` is found — review
/// 30397e74 of car 4f1a96ab named one, native-proof-evidence, that a
/// set derived only from the `source` line missed, backlog 6665aa97),
/// the lints that source `lib/trunk-ref.sh`, and the lints that call
/// `lint_no_trunk`. A new trunk-relative lint that forgets the
/// declaration would red every publish again, and fails here by name.
#[test]
fn the_lints_that_honour_the_declaration_are_exactly_the_trunk_relative_ones() {
    let root = repo_root();
    let comparing = lint_names_where(&root, |t| t.lines().any(compares_against_a_merge_base));
    let sourcing = lint_names_where(&root, |t| {
        t.lines()
            .any(|l| l.trim_start().starts_with(". ") && l.contains("lib/trunk-ref.sh"))
    });
    let calling = lint_names_where(&root, |t| {
        t.lines().any(|l| {
            let l = l.trim_start();
            !l.starts_with('#') && l.contains("lint_no_trunk \"$LINT\"")
        })
    });
    assert_eq!(
        sourcing, comparing,
        "every lint that compares against a trunk merge-base resolves the trunk through \
         lib/trunk-ref.sh, so BOSS_TRUNK_REF and BOSS_TRUNK=none reach it"
    );
    assert_eq!(
        calling, sourcing,
        "every lint that judges against the trunk honours BOSS_TRUNK=none, and no other"
    );
    let mut want: Vec<String> = TRUNK_LINTS.iter().map(|l| l.to_string()).collect();
    want.sort();
    assert_eq!(
        sourcing, want,
        "the fixture above runs every trunk-relative lint"
    );
}

/// ONE GUARD (CLAUDE.md §9a). The runner check lives in
/// `lib/public-mirror.sh`; both declarations call it, and no other lint
/// lib re-spells it.
#[test]
fn both_declarations_share_one_runner_guard() {
    let root = repo_root();
    let lib = root.join("infra/lint/lib");
    for decl in ["no-estate.sh", "trunk-ref.sh"] {
        let t = std::fs::read_to_string(lib.join(decl)).expect("read the lib");
        assert!(
            t.lines()
                .any(|l| l.trim_start().starts_with(". ") && l.contains("public-mirror.sh")),
            "{decl} sources lib/public-mirror.sh"
        );
        assert!(
            t.lines().any(|l| !l.trim_start().starts_with('#')
                && l.contains("refuse_off_the_public_mirror \"$lint\"")),
            "{decl} calls the shared guard"
        );
    }
    let respelt: Vec<String> = std::fs::read_dir(&lib)
        .expect("read the lib dir")
        .map(|e| e.expect("an entry").path())
        .filter(|p| p.file_name().is_some_and(|n| n != "public-mirror.sh"))
        .filter(|p| {
            std::fs::read_to_string(p)
                .unwrap_or_default()
                .lines()
                .any(|l| !l.trim_start().starts_with('#') && l.contains("FORGEJO_ACTIONS"))
        })
        .map(|p| p.display().to_string())
        .collect();
    assert!(
        respelt.is_empty(),
        "the runner guard is spelt once, in lib/public-mirror.sh: {respelt:?}"
    );
}

// ---- only the mirror declares it ----
//
// Inverted, as a_declared_no_estate_skips_only_the_live_half.rs learned
// to be: ANY mention of the variable outside a comment is a leak unless
// the file has a reason to name it, and those may only read or print it.
// The runtime guard is the other half — a scan cannot see a variable set
// outside the tree.

const DECLARER: &str = ".github/workflows/ci.yml";
const DECLARATION: &str = "BOSS_TRUNK: none";
/// The files that define and report the declaration: they may read it and
/// print it, never set it.
const DEFINERS: [&str; 2] = ["infra/lint/lib/trunk-ref.sh", "infra/gate.sh"];
const TESTS_DIR: &str = "crates/core/boss-testing/tests/";

/// Does `line` name `BOSS_TRUNK` as a whole word (not `BOSS_TRUNK_REF`)?
fn mentions(line: &str) -> bool {
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    line.match_indices("BOSS_TRUNK").any(|(i, m)| {
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
    let writes = has(&["export", ":=", "printf -v", ">", "tee "]);
    let chained = has(&["env ", "ENV ", "&&", "||", ";", "`", "|"]);
    let prints = t.starts_with("printf ") || t.starts_with("echo ");
    // Naming the declaration to the shared runtime guard, which can only
    // refuse (lib/public-mirror.sh).
    let guards = t.starts_with("refuse_off_the_public_mirror ");
    let reads =
        (t.contains("$BOSS_TRUNK") || t.contains("${BOSS_TRUNK")) && !t.contains("BOSS_TRUNK=");
    !writes && (((prints || guards) && !chained) || reads)
}

/// Every line of `text` (the file at `rel`) that leaks the declaration.
fn trunk_leaks(rel: &str, text: &str) -> Vec<String> {
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

#[test]
fn the_leak_scan_catches_a_declaration_that_would_reach_a_forge_gate() {
    for (rel, text) in [
        (
            "infra/gate-runner/gate-runner.yaml",
            "        - {name: BOSS_TRUNK, value: \"none\"}",
        ),
        (".forgejo/workflows/ci.yml", "      BOSS_TRUNK: none"),
        ("infra/forge/boss-ci/Dockerfile", "ENV BOSS_TRUNK=none"),
        (
            "infra/gate-runner/run.sh",
            "cd /w && BOSS_TRUNK=none ./infra/gate.sh",
        ),
        ("infra/gate.sh", "export BOSS_TRUNK=none"),
        ("infra/gate.sh", "echo BOSS_TRUNK=none >> \"$GITHUB_ENV\""),
        (DECLARER, "      run: BOSS_TRUNK=none infra/gate.sh"),
        (
            "infra/lint/lib/trunk-ref.sh",
            "refuse_off_the_public_mirror x \"BOSS_TRUNK=none\"; export BOSS_TRUNK=none",
        ),
    ] {
        assert!(
            !trunk_leaks(rel, text).is_empty(),
            "{rel}: this spelling leaks the declaration and must be refused:\n{text}"
        );
    }
    for (rel, text) in [
        (
            DECLARER,
            "  # BOSS_TRUNK, said out loud\n      BOSS_TRUNK: none",
        ),
        (
            "infra/lint/lib/trunk-ref.sh",
            "    case \"${BOSS_TRUNK:-}\" in",
        ),
        (
            "infra/lint/lib/trunk-ref.sh",
            "            refuse_off_the_public_mirror \"$lint\" \"BOSS_TRUNK=none\"",
        ),
        ("infra/gate.sh", "    if [ -n \"${BOSS_TRUNK:-}\" ]; then"),
        ("infra/lint/x.sh", "BASE=${BOSS_TRUNK_REF:-origin/main}"),
    ] {
        assert_eq!(
            trunk_leaks(rel, text),
            Vec::<String>::new(),
            "{rel}: a read, a comment or another variable is not a leak:\n{text}"
        );
    }
}

/// Only the mirror declares it, on the tree.
#[test]
fn only_the_public_mirror_declares_no_trunk() {
    let root = repo_root();
    let out = Command::new("git")
        .args(["grep", "-lw", "BOSS_TRUNK"])
        .current_dir(&root)
        .output()
        .expect("git grep");
    assert!(
        out.status.success(),
        "git grep found no mention of BOSS_TRUNK at all — the declaration is gone:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let leaks: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .flat_map(|rel| {
            let text = std::fs::read_to_string(root.join(rel))
                .unwrap_or_else(|e| panic!("read {rel}: {e}"));
            trunk_leaks(rel, &text)
        })
        .collect();
    assert!(
        leaks.is_empty(),
        "BOSS_TRUNK=none is the public mirror's declaration alone; anywhere else it would \
         pass cars without their trunk judgements. Named where it must not be:\n{}",
        leaks.join("\n")
    );
    let yml = std::fs::read_to_string(root.join(DECLARER)).expect("ci.yml");
    assert!(
        yml.lines().any(|l| l.trim() == DECLARATION),
        "the mirror declares `{DECLARATION}`"
    );
}

// ---- a test that scrubs one declaration scrubs both ----
//
// The mirror runs this suite with BOSS_ESTATE=none AND BOSS_TRUNK=none
// (backlog 6665aa97, from builder run dd24c588 of car 4f1a96ab: a
// simulated mirror run caught 18 failures). A test that removes
// BOSS_ESTATE from a child's environment does so because the child is a
// lint or a gate whose answer must not depend on where the suite runs;
// leaving BOSS_TRUNK in that same environment reds only on the mirror,
// where nobody building the test ran it. Held by file: every file that
// removes the one removes the other.

/// Does this line remove `var` from a child's environment — an
/// `env_remove("<var>")` call, or a bare `"<var>",` element of a list a
/// loop removes?
fn removes(line: &str, var: &str) -> bool {
    let t = line.trim();
    !t.starts_with("//")
        && (t.contains(&format!("env_remove(\"{var}\")")) || t == format!("\"{var}\","))
}

/// The files in `files` (path, text) that remove BOSS_ESTATE and never
/// BOSS_TRUNK.
fn scrubs_estate_not_trunk(files: &[(String, String)]) -> Vec<String> {
    files
        .iter()
        .filter(|(_, text)| {
            text.lines().any(|l| removes(l, "BOSS_ESTATE"))
                && !text.lines().any(|l| removes(l, "BOSS_TRUNK"))
        })
        .map(|(rel, _)| rel.clone())
        .collect()
}

#[test]
fn the_scrub_scan_tells_a_half_scrub_from_a_whole_one() {
    let file = |body: &str| vec![("t.rs".to_string(), body.to_string())];
    for half in [
        "cmd.env_remove(\"BOSS_ESTATE\");",
        "    .env_remove(\"BOSS_ESTATE\")\n    .env_remove(\"BOSS_TRUNK_REF\")",
        "for var in [\n    \"BOSS_ESTATE\",\n] {",
        "cmd.env_remove(\"BOSS_ESTATE\");\n// .env_remove(\"BOSS_TRUNK\") said in a comment",
    ] {
        assert_eq!(
            scrubs_estate_not_trunk(&file(half)),
            vec!["t.rs".to_string()],
            "removes BOSS_ESTATE and leaves BOSS_TRUNK:\n{half}"
        );
    }
    for whole in [
        "cmd.env_remove(\"BOSS_ESTATE\").env_remove(\"BOSS_TRUNK\");",
        "for var in [\n    \"BOSS_ESTATE\",\n    \"BOSS_TRUNK\",\n] {",
        "    None => cmd.env_remove(\"BOSS_ESTATE\"),\n    None => cmd.env_remove(\"BOSS_TRUNK\"),",
        "cmd.env(\"BOSS_ESTATE\", \"none\");",
        "cmd.env_remove(\"BOSS_ESTATE_NODES_URL\");",
        "// .env_remove(\"BOSS_ESTATE\") said in a comment",
    ] {
        assert_eq!(
            scrubs_estate_not_trunk(&file(whole)),
            Vec::<String>::new(),
            "scrubs both, or neither:\n{whole}"
        );
    }
}

/// On the tree: every Rust file that removes BOSS_ESTATE from a child's
/// environment removes BOSS_TRUNK too.
#[test]
fn every_test_that_removes_the_estate_declaration_removes_the_trunk_one() {
    let root = repo_root();
    let out = Command::new("git")
        .args(["grep", "-lw", "BOSS_ESTATE", "--", "*.rs"])
        .current_dir(&root)
        .output()
        .expect("git grep");
    assert!(
        out.status.success(),
        "git grep found no Rust file naming BOSS_ESTATE — this scan has nothing to hold:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let files: Vec<(String, String)> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|rel| {
            let text = std::fs::read_to_string(root.join(rel))
                .unwrap_or_else(|e| panic!("read {rel}: {e}"));
            (rel.to_string(), text)
        })
        .collect();
    assert!(
        files
            .iter()
            .any(|(_, t)| t.lines().any(|l| removes(l, "BOSS_ESTATE"))),
        "no file removes BOSS_ESTATE at all — the scan reads nothing"
    );
    let half = scrubs_estate_not_trunk(&files);
    assert!(
        half.is_empty(),
        "these remove BOSS_ESTATE from a child's environment and leave BOSS_TRUNK, so on the \
         public mirror (BOSS_TRUNK=none) the child judges no trunk and the test reds there \
         alone. Add .env_remove(\"BOSS_TRUNK\") beside it:\n{}",
        half.join("\n")
    );
}

// ---- the receipt ----

const STUB: &str = "a-stub-that-judges-against-the-trunk";

/// A synthetic repository with this tree's gate and one planted lint that
/// honours the declaration as the real eight do, run as the gate proper
/// with every compiled phase stubbed (the shape the no-estate pin uses).
struct Gate {
    dir: PathBuf,
    tree: PathBuf,
}

impl Gate {
    fn new(tag: &str) -> Gate {
        let dir = boss_testing::scratch_dir(&format!("no-trunk-gate-{tag}"));
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
                 . infra/lint/lib/trunk-ref.sh || exit 3\n\
                 LINT={STUB}\n\
                 if lint_no_trunk \"$LINT\" 'what this car added'; then exit 0; fi\n\
                 echo '{STUB}: an added file breaks the rule' >&2\n\
                 exit 1\n"
            ),
        );
        for web in ["apps/web", "apps/simulator", "libs/web-kit"] {
            boss_testing::create_dir(&tree.join(web));
        }
        git(&tree, &["init", "-q", "-b", "main"]);
        git(&tree, &["add", "."]);
        git(&tree, &["commit", "-q", "-m", "the gate under test"]);
        let bin = dir.join("bin");
        boss_testing::create_dir(&bin);
        for tool in ["cargo", "bun"] {
            boss_testing::write_exec(&bin.join(tool), "#!/usr/bin/env bash\nexit 0\n");
        }
        boss_testing::write_exec(
            &dir.join("df"),
            "#!/usr/bin/env bash\n\
             echo 'Filesystem 1024-blocks Used Available Capacity Mounted on'\n\
             for _ in \"$@\"; do echo '/dev/fake 1 1 943718400 1% /'; done\n",
        );
        Gate { dir, tree }
    }

    fn receipt(&self) -> PathBuf {
        self.dir.join("receipt.json")
    }

    fn run(&self, trunk: Option<&str>) -> Output {
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
            .env("GIT_CEILING_DIRECTORIES", "")
            .env_remove("BOSS_TRUNK_REF")
            .env_remove("BOSS_ESTATE");
        match trunk {
            Some(v) => cmd.env("BOSS_TRUNK", v),
            None => cmd.env_remove("BOSS_TRUNK"),
        };
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

/// THE RECEIPT NAMES THEM. Declared, the stub's check is `pass` and the
/// receipt carries `trunk`: the declared word and every lint that did not
/// judge against the trunk, read off what the lints printed. Undeclared,
/// the same stub is a red and the receipt carries no `trunk`.
#[test]
fn the_receipt_names_every_lint_that_did_not_judge_against_the_trunk() {
    let gate = Gate::new("declared");
    let out = gate.run(Some("none"));
    let t = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let receipt = gate.read_receipt(&t);
    assert_eq!(
        receipt["trunk"]["declared"], "none",
        "the receipt records the declaration:\n{receipt:#}"
    );
    assert_eq!(
        receipt["trunk"]["not_judged"],
        serde_json::json!([STUB]),
        "the receipt names every lint that made no trunk comparison:\n{receipt:#}"
    );
    let entry = receipt["checks"]
        .as_array()
        .and_then(|c| c.iter().find(|c| c["name"] == STUB))
        .unwrap_or_else(|| panic!("the stub lint is in the checks list:\n{receipt:#}"));
    assert_eq!(entry["result"], "pass", "{receipt:#}");
    assert!(
        t.lines()
            .any(|l| l.contains("BOSS_TRUNK=none") && l.contains(STUB)),
        "the gate's own output names the declaration and the lint it excused:\n{t}"
    );

    let gate = Gate::new("undeclared");
    let out = gate.run(None);
    let t = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let receipt = gate.read_receipt(&t);
    let entry = receipt["checks"]
        .as_array()
        .and_then(|c| c.iter().find(|c| c["name"] == STUB))
        .unwrap_or_else(|| panic!("the stub lint is in the checks list:\n{receipt:#}"));
    assert_eq!(entry["result"], "fail", "{receipt:#}");
    assert!(
        receipt.get("trunk").is_none(),
        "no declaration, no `trunk` on the receipt:\n{receipt:#}"
    );
}
