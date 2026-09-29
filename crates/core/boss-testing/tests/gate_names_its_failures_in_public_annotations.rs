//! On GitHub Actions `infra/gate.sh` names every failed check in a PUBLIC
//! annotation — and nowhere else does it print one.
//!
//! THE GAP (backlog 29c36336, measured 2026-09-27). PR #245's mirror
//! Gate (check-run 108632200196) exited 1, and the estate recorded only
//! `failing: Gate (infra/gate.sh, full): failure`. gate.sh names the
//! failed check twice — `GATE FAIL: <name>` on stderr and the receipt
//! that ci.yml cats into the job log — but GitHub serves a job's log only
//! to a repository admin (`GET /repos/…/actions/jobs/<id>/logs` answers
//! 403 with no credential), so no BOSS reader could name the check and a
//! human reproduced the lint roster by hand. A check-run's ANNOTATIONS
//! are public through the API, and the Gate's seven carried only the
//! exit code, three LIVE HALF NOT RUN warnings and two deliberate browser
//! fixtures. So:
//!
//!   * under `GITHUB_ACTIONS=true` each failed check prints one
//!     `::error title=GATE FAIL: <name>::<its first useful line> (exit N,
//!     after Ts)` — title escaped as GitHub's workflow-command grammar
//!     requires (`:` is `%3A` in a property), message escaped for `%`;
//!   * at most FIVE by name, because GitHub keeps ten error annotations
//!     per step and the web suite's own fixtures spend two of them; the
//!     rest are named in ONE `GATE FAIL (N more)` summary, a name per
//!     line;
//!   * a refusal prints `::error title=GATE REFUSED::<why>`;
//!   * without `GITHUB_ACTIONS=true` the gate prints no annotation at all.
//!
//! `infra/forge/read-publish-checks.sh` reads these back into the
//! publish packet's `code_scanning.failing`; its half is pinned in
//! `read_publish_checks_sh.rs`. The grammar between the two scripts is
//! ONE fact written in two files (CLAUDE.md §9a), so the last case here
//! is the round trip: what this gate prints, stored the way GitHub
//! stores it, read back by that verb, names every failed check.
//!
//! Driven through the REAL gate in a synthetic tree (the LevelTree shape
//! in gate_sh.rs): this tree's gate.sh and lint libs, a roster of lints
//! written for the purpose, `cargo` and `bun` stubbed, and a `df` that
//! reports plenty — so `--quick` runs fmt and the roster and nothing
//! compiles.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The lints this tree's roster runs, in roster order, and what each
/// prints. Seven fail, one passes.
const LINTS: &[(&str, &str)] = &[
    (
        "fail-1-error-line",
        "echo 'checking 3 files'\necho 'the tree carries a stray file' >&2\necho 'error: stray.rs is not in the manifest' >&2\nexit 1\n",
    ),
    (
        "fail-2-plain-stdout",
        "echo ''\necho '  a plain first line  '\necho 'a second line'\nexit 1\n",
    ),
    ("fail-3-silent", "exit 1\n"),
    (
        "fail-4-escapes",
        "echo 'ERROR: 100% of a, b: c' >&2\nexit 1\n",
    ),
    ("fail-5", "echo 'test boss::x ... FAILED'\nexit 1\n"),
    ("fail-6", "echo 'six' >&2\nexit 1\n"),
    ("fail-7", "echo 'seven' >&2\nexit 1\n"),
    (
        "pass-1",
        "echo 'error: printed by a lint that passed'\nexit 0\n",
    ),
];

struct Tree {
    dir: PathBuf,
    tree: PathBuf,
}

impl Tree {
    fn new(tag: &str) -> Tree {
        let dir = boss_testing::scratch_dir(&format!("gate-annotations-{tag}"));
        let tree = dir.join("tree");
        boss_testing::copy_lint_libs(&tree);
        boss_testing::copy_gate_sh(&tree);
        boss_testing::write_file(
            &tree.join("infra/lint/workspace-declares-what-it-runs.sh"),
            "#!/usr/bin/env bash\nexit 0\n",
        );
        for (name, body) in LINTS {
            boss_testing::write_file(
                &tree.join(format!("infra/lint/{name}.sh")),
                &format!("#!/usr/bin/env bash\n# A fixture lint.\n{body}"),
            );
        }
        let t = Tree { dir, tree };
        t.vcs(&["init", "-q", "-b", "main"]);
        t.vcs(&["add", "."]);
        t.vcs(&[
            "-c",
            "user.email=annotations@test",
            "-c",
            "user.name=annotations",
            "commit",
            "-q",
            "-m",
            "the tree",
        ]);
        let bin = t.dir.join("bin");
        boss_testing::create_dir(&bin);
        for tool in ["cargo", "bun"] {
            boss_testing::write_exec(&bin.join(tool), "#!/usr/bin/env bash\nexit 0\n");
        }
        boss_testing::write_exec(
            &t.dir.join("df"),
            "#!/usr/bin/env bash\n\
             echo 'Filesystem 1024-blocks Used Available Capacity Mounted on'\n\
             echo '/dev/fake 1 1 943718400 1% /'\n",
        );
        t
    }

    fn vcs(&self, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(&self.tree)
            .output()
            .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// `gate.sh --quick` in this tree, with GITHUB_ACTIONS as `actions`
    /// says — removed, not merely unset to empty, when `None`: the
    /// mirror runs this very test under GITHUB_ACTIONS=true.
    fn quick(&self, actions: Option<&str>, df: &Path) -> Output {
        let path = std::env::var("PATH").unwrap_or_default();
        let mut cmd = Command::new("bash");
        cmd.arg(self.tree.join("infra/gate.sh"))
            .arg("--quick")
            .current_dir(&self.tree)
            .env("PATH", format!("{}:{path}", self.dir.join("bin").display()))
            .env("BOSS_GATE_DF_CMD", df)
            .env("BOSS_GATE_MIN_FREE_GB", "12")
            .env("BOSS_GATE_TRUNK", "main")
            .env("BOSS_TRUNK_REF", "main")
            .env(
                "BOSS_GATE_RECEIPT",
                self.dir.join("receipt.json").to_str().expect("utf8"),
            )
            .env("GIT_CEILING_DIRECTORIES", "")
            .env_remove("BOSS_ESTATE");
        match actions {
            Some(v) => cmd.env("GITHUB_ACTIONS", v),
            None => cmd.env_remove("GITHUB_ACTIONS"),
        };
        cmd.output().expect("run gate.sh --quick")
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn both(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// Every workflow-command annotation the run printed, on either stream.
fn annotations(out: &Output) -> Vec<String> {
    both(out)
        .lines()
        .filter(|l| l.starts_with("::error") || l.starts_with("::warning"))
        .map(str::to_string)
        .collect()
}

#[test]
fn on_github_actions_each_failed_check_is_one_public_annotation_capped_with_a_summary() {
    let t = Tree::new("on");
    let out = t.quick(Some("true"), &t.dir.join("df"));
    let text = both(&out);
    assert_eq!(out.status.code(), Some(1), "seven lints fail:\n{text}");

    let got = annotations(&out);
    // The duration is the lint's own measured seconds, so it is the one
    // field matched rather than spelled.
    let want = [
        r"^::error title=GATE FAIL%3A fail-1-error-line::error: stray\.rs is not in the manifest \(exit 1, after \d+s\)$",
        r"^::error title=GATE FAIL%3A fail-2-plain-stdout::a plain first line \(exit 1, after \d+s\)$",
        r"^::error title=GATE FAIL%3A fail-3-silent::fail-3-silent failed and printed nothing \(exit 1, after \d+s\)$",
        r"^::error title=GATE FAIL%3A fail-4-escapes::ERROR: 100%25 of a, b: c \(exit 1, after \d+s\)$",
        r"^::error title=GATE FAIL%3A fail-5::test boss::x \.\.\. FAILED \(exit 1, after \d+s\)$",
        r"^::error title=GATE FAIL \(2 more\)::2 more check\(s\) failed past the 5 this gate annotates by name; their words are in the job log:%0Afail-6%0Afail-7$",
    ];
    assert_eq!(
        got.len(),
        want.len(),
        "exactly one annotation per failed check up to the cap, then one summary; got:\n{}\n\nfull run:\n{text}",
        got.join("\n")
    );
    for (line, re) in got.iter().zip(want) {
        assert!(
            regex::Regex::new(re).expect("a pattern").is_match(line),
            "annotation {line:?} does not read {re}\nall of them:\n{}",
            got.join("\n")
        );
    }
    // Nothing is reduced: the check's own words still reach the log,
    // once, as they did before the capture existed.
    assert_eq!(
        text.matches("error: stray.rs is not in the manifest")
            .count(),
        2,
        "the lint's line in its own group, and quoted once in its annotation:\n{text}"
    );
    assert!(
        text.contains("GATE FAIL: fail-7 (exit 1, after "),
        "the stderr GATE FAIL line is unchanged for every check:\n{text}"
    );
}

#[test]
fn without_github_actions_the_gate_prints_no_annotation() {
    let t = Tree::new("off");
    for actions in [None, Some(""), Some("false")] {
        let out = t.quick(actions, &t.dir.join("df"));
        let text = both(&out);
        assert_eq!(out.status.code(), Some(1), "seven lints fail:\n{text}");
        assert!(
            annotations(&out).is_empty(),
            "GITHUB_ACTIONS={actions:?} printed an annotation:\n{text}"
        );
        assert!(
            text.contains("GATE FAIL: fail-1-error-line (exit 1, after "),
            "the failures are still named on stderr:\n{text}"
        );
    }
}

/// A refusal is not a failed check, and reads as neither a GATE FAIL
/// nor silence: the disk floor trips before the first lint (calls 1 and
/// 2 of `df` see plenty — startup and `fmt` — and call 3 does not), and
/// the one annotation says WHY the gate declined.
#[test]
fn on_github_actions_a_refusal_is_one_gate_refused_annotation() {
    let t = Tree::new("refused");
    let counter = t.dir.join("df-calls");
    let df = t.dir.join("df-trips");
    boss_testing::write_exec(
        &df,
        &format!(
            "#!/usr/bin/env bash\n\
             n=$(cat {c} 2>/dev/null || echo 0)\n\
             echo $((n+1)) > {c}\n\
             echo 'Filesystem 1024-blocks Used Available Capacity Mounted on'\n\
             if [ \"$n\" -lt 2 ]; then echo '/dev/fake 1 1 943718400 1% /'; \
             else echo '/dev/fake 1 1 1048576 99% /'; fi\n",
            c = counter.display()
        ),
    );
    let out = t.quick(Some("true"), &df);
    let text = both(&out);
    assert_eq!(out.status.code(), Some(2), "the floor refuses:\n{text}");
    let got = annotations(&out);
    assert_eq!(got.len(), 1, "one annotation, the refusal:\n{text}");
    assert!(
        got[0].starts_with("::error title=GATE REFUSED::1GB free, need 12GB"),
        "the refusal names its reason: {}",
        got[0]
    );
}

// ---- the round trip: gate.sh prints, GitHub stores, the verb reads ----

/// A workflow-command property or message as the Actions runner stores
/// it: `%0D`, `%0A`, `%3A` and `%2C` decoded, `%25` last so an escaped
/// percent is never decoded twice.
fn unescape(s: &str) -> String {
    s.replace("%0D", "\r")
        .replace("%0A", "\n")
        .replace("%3A", ":")
        .replace("%2C", ",")
        .replace("%25", "%")
}

/// One `::error title=…::…` line as the check-runs annotations API
/// answers for it: the measured shape of #245's own log annotations
/// (`.github` path, the log line as `start_line`).
fn stored(line: &str, at: u64) -> serde_json::Value {
    let rest = line.strip_prefix("::error ").expect("an ::error line");
    let (props, data) = rest.split_once("::").expect("properties::data");
    let title = props
        .split(',')
        .find_map(|p| p.strip_prefix("title="))
        .expect("a title property");
    serde_json::json!({
        "path": ".github", "start_line": at, "end_line": at,
        "start_column": null, "end_column": null,
        "annotation_level": "failure",
        "title": unescape(title), "message": unescape(data), "raw_details": ""
    })
}

/// `infra/forge/read-publish-checks.sh` run offline against PR #245's
/// measured check-runs, with its Gate's annotations replaced by
/// `gate_annotations`. Returns the `code_scanning` reading it PATCHed.
fn read_back(t: &Tree, gate_annotations: &[serde_json::Value]) -> serde_json::Value {
    let fixtures = boss_testing::repo_root().join("crates/core/boss-testing/tests/fixtures/github");
    let root = t.dir.join("verb");
    boss_testing::create_dir(&root.join("stubs"));
    let jobs = root.join("jobs.json");
    boss_testing::write_file(
        &jobs,
        &serde_json::json!({"data": [{
        "id": "00000000-0000-0000-0000-0000000000aa", "title": "publish", "status": "open",
        "metadata": {},
        "steps": [
            {"id": "00000000-0000-0000-0000-0000000000bb", "spec_slug": "open-pr",
             "status": "completed", "completed_at": "2026-09-26T00:00:00Z",
             "metadata": {"pr_url": "https://github.com/algedonic-dev/boss/pull/245",
                          "snapshot_commit": "28554177812cc9645ab0eff2158574c25286f34a"}},
            {"id": "00000000-0000-0000-0000-0000000000cc", "spec_slug": "read-checks",
             "status": "ready", "metadata": {}}
        ]}]})
        .to_string(),
    );
    let none = root.join("none.json");
    boss_testing::write_file(&none, "[]");
    let gate = root.join("gate.json");
    boss_testing::write_file(
        &gate,
        &serde_json::Value::Array(gate_annotations.to_vec()).to_string(),
    );
    let routes = root.join("routes");
    boss_testing::write_file(
        &routes,
        &format!(
            "api/jobs?kind=publish-to-github\t{}\n\
             /commits/28554177812cc9645ab0eff2158574c25286f34a/check-runs\t{}\n\
             /check-runs/108632377406/annotations\t{}\n\
             /check-runs/108632200196/annotations\t{}\n",
            jobs.display(),
            fixtures.join("check-runs-pr245.json").display(),
            none.display(),
            gate.display()
        ),
    );
    let writes = root.join("writes");
    boss_testing::write_exec(
        &root.join("stubs/curl"),
        &format!(
            r#"#!/bin/sh
url=""; data=""; method=GET; prev=""
for a in "$@"; do
    case "$prev" in --data-binary) data="$a" ;; -X) method="$a" ;; esac
    case "$a" in http://*|https://*) url="$a" ;; esac
    prev="$a"
done
if [ "$method" != GET ]; then
    case "$data" in @*) cat "${{data#@}}" >> '{writes}'; echo >> '{writes}' ;; esac
    exit 0
fi
while IFS='	' read -r needle file; do
    case "$url" in *"$needle"*) cat "$file"; exit 0 ;; esac
done < '{routes}'
echo "curl: (22) The requested URL returned error: 404 for $url" >&2
exit 22
"#,
            writes = writes.display(),
            routes = routes.display(),
        ),
    );
    let outer = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into());
    let out = Command::new("bash")
        .arg(boss_testing::repo_root().join("infra/forge/read-publish-checks.sh"))
        .env_clear()
        .env("PATH", format!("{}:{outer}", root.join("stubs").display()))
        .env("BOSS_JOBS_URL", "http://jobs.invalid")
        .env("BOSS_GITHUB_API", "https://api.github.invalid")
        .env("BOSS_MIRROR_SLUG", "fixture-upstream/mirror")
        .output()
        .expect("read-publish-checks runs");
    let text = both(&out);
    assert_eq!(out.status.code(), Some(0), "{text}");
    let body = std::fs::read_to_string(&writes).unwrap_or_default();
    body.lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find_map(|v| v.get("code_scanning").cloned())
        .unwrap_or_else(|| panic!("no code_scanning reading was written:\n{body}\n{text}"))
}

#[test]
fn what_the_gate_prints_on_actions_the_publish_reading_names_check_by_check() {
    let t = Tree::new("round-trip");
    let out = t.quick(Some("true"), &t.dir.join("df"));
    let printed: Vec<serde_json::Value> = annotations(&out)
        .iter()
        .enumerate()
        .map(|(i, l)| stored(l, 100 + i as u64))
        .collect();
    assert_eq!(printed.len(), 6, "{}", both(&out));

    let reading = read_back(&t, &printed);
    let gate = &reading["failing"][0];
    assert_eq!(gate["name"], "Gate (infra/gate.sh, full)", "{reading}");
    let names: Vec<&str> = gate["failed_checks"]
        .as_array()
        .unwrap_or_else(|| panic!("the Gate entry carries failed_checks: {gate}"))
        .iter()
        .map(|c| c["name"].as_str().expect("a name"))
        .collect();
    let failed: Vec<&str> = LINTS
        .iter()
        .map(|(n, _)| *n)
        .filter(|n| n.starts_with("fail-"))
        .collect();
    assert_eq!(
        names, failed,
        "every check the gate failed, by name and in its order — the capped two from the summary"
    );
    let message = gate["failed_checks"][3]["message"].as_str().unwrap_or("");
    assert!(
        message.starts_with("ERROR: 100% of a, b: c (exit 1, after "),
        "the message arrives as the check printed it: {message:?}"
    );
}
