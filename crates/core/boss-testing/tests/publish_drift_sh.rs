//! `infra/gcp/publish-drift.sh` is RUN, not read — against a planted
//! checkout, a planted `publish-workflow.sh` stub that answers each of
//! the sub-verb's exit codes by name, and a stub `curl` standing in for
//! the system of record's pr-train read — so every verdict below is one
//! the script actually reached.
//!
//! WHY THE VERB EXISTS (backlog a2f97942, ratified by retro 27fad542).
//! `publish-workflow` is one kind per request by design (bounded), and
//! on 2026-09-18 13:4x that bound became the operator's loop: after the
//! maintenance-audience car landed, 23 `publish-workflow` ops-requests
//! were hand-scripted one per kind, preceded by a hand `until --check
//! ok` wait for boss-gcp's checkout to carry the train. The gap is one
//! level up: after a train lands, every kind the tree moved ahead of
//! should be published as ONE act, and the wait should be a verdict.
//!
//! WHAT IT PINS. The comparison is never re-derived here: the drift set
//! is `publish-workflow.sh <kind> --check` per kind, classified by that
//! verb's documented exit codes — 0 tree-ahead, 5 equal, 6 the live row
//! carries what the tree never said, 4 does not lint, 8 no live row —
//! so a refusal-6 kind is LISTED field by field and never published,
//! in either mode. `--check` lists and writes nothing; `--for-real`
//! publishes each tree-ahead kind in bundle order through
//! `publish-workflow.sh` itself and exits non-zero when any publish was
//! not confirmed. The first line names the checkout sha the verb read;
//! a checkout behind the newest converged train is `not yet` (75),
//! never a publish from stale files. The phrases the table reads
//! versions from are pinned to the sub-verb's own text.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use std::path::{Path, PathBuf};
use std::process::Command;

fn has(tool: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {tool} >/dev/null 2>&1")])
        .status()
        .is_ok_and(|s| s.success())
}

const SCRIPT: &str = "infra/gcp/publish-drift.sh";
const SUB_VERB: &str = "infra/gcp/publish-workflow.sh";
const VERB_FILE: &str = "infra/ops/verbs/publish-drift.json";
const HOLDS_REL: &str = "infra/platform/workflow-holds";
const HOLDS_READER: &str = "infra/gcp/workflow-holds.py";

/// A pr-train listing as `GET /api/jobs?kind=pr-train` answers it: the
/// newest closed train's `merged` step carries `merge_ref`.
fn trains(merge_ref: &str) -> String {
    format!(
        r#"{{"data":[
  {{"id":"t-old","status":"closed","steps":[{{"spec_slug":"merged","status":"completed","completed_at":"2026-09-18T10:55:00Z","metadata":{{"merge_ref":"0000000000000000000000000000000000000000"}}}}]}},
  {{"id":"t-open","status":"open","steps":[{{"spec_slug":"merged","status":"pending","metadata":{{}}}}]}},
  {{"id":"t-new","status":"closed","steps":[{{"spec_slug":"merged","status":"completed","completed_at":"2026-09-18T13:58:00Z","metadata":{{"merge_ref":"{merge_ref}"}}}}]}}
],"total":3}}"#
    )
}

struct Case {
    root: PathBuf,
    bin: PathBuf,
    repo: PathBuf,
    stub: PathBuf,
    verdicts: PathBuf,
    trains: PathBuf,
    pw_log: PathBuf,
    head: String,
}

impl Case {
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("publish-drift-{name}"));
        let bin = root.join("bin");
        let repo = root.join("repo");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(repo.join("infra/platform/workflows")).unwrap();
        let git = |args: &[&str]| {
            let st = Command::new("git")
                .args(args)
                .current_dir(&repo)
                .env("GIT_AUTHOR_NAME", "fixture")
                .env("GIT_AUTHOR_EMAIL", "f@example.invalid")
                .env("GIT_COMMITTER_NAME", "fixture")
                .env("GIT_COMMITTER_EMAIL", "f@example.invalid")
                .output()
                .expect("git runs");
            assert!(
                st.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&st.stderr)
            );
        };
        git(&["init", "-q", "-b", "main"]);
        // The history the freshness read is judged against; the bundle
        // itself is planted per case by `verdicts`, so each case's
        // table is exactly the kinds it names.
        write_file(&repo.join("README"), "the fixture checkout\n");
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "the checkout"]);
        let out = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&repo)
            .output()
            .expect("git rev-parse runs");
        let head = String::from_utf8_lossy(&out.stdout).trim().to_string();

        // The planted sub-verb: answers `<kind> --check` and `<kind>`
        // from a verdict table (`kind rc version` per line), in the
        // sub-verb's own words for the lines the table reads, and logs
        // every call so the test can say what was published.
        let stub = root.join("publish-workflow.sh");
        write_exec(
            &stub,
            r#"#!/usr/bin/env bash
kind="$1"; mode="${2:-}"
echo "$kind ${mode:-PUBLISH}" >> "$STUB_PW_LOG"
row=$(grep -E "^$kind " "$STUB_VERDICTS" | head -n1)
rc=$(cut -d' ' -f2 <<<"$row"); ver=$(cut -d' ' -f3 <<<"$row")
[ -n "$rc" ] || { echo "publish-workflow: REFUSED — the tree has no infra/platform/workflows/$kind.toml"; exit 3; }
file="infra/platform/workflows/$kind.toml"
if [ "$mode" = "--check" ]; then
  case "$rc" in
    0) echo "publish-workflow: live $kind v$ver differs from $file:"
       echo "  description  tree=A run that died closes this packet failed.  live=<absent>"
       echo "publish-workflow: the tree moved ahead: live $kind v$ver is the row at 8cc7a03e:infra/platform/workflows.toml"
       echo "publish-workflow: --check ok: would publish $file over live v$ver at $BOSS_JOBS_URL (nothing written; packet ${OPS_REQUEST_ID:-none})"; exit 0 ;;
    5) echo "publish-workflow: REFUSED — nothing to publish — the live $kind v$ver already says what $file says (label, description, category, subject_kinds and every step compared equal)."; exit 5 ;;
    6) echo "publish-workflow: live $kind v$ver differs from $file:"
       echo "  steps[1].fields  tree=[[\"result\",\"string\",true]]  live=[[\"result\",\"string\",true],[\"proof\",\"string\",true]]"
       echo "  steps[1].metadata_defaults  tree=<absent>  live={\"procedure\":\"Observe the sweep working in production.\"}"
       echo "publish-workflow: REFUSED — live $kind v$ver carries what the tree never said: no revision of infra/platform in 500 commit(s) walked renders to it"; exit 6 ;;
    4) echo "publish-workflow: boss workflow publish --dry-run refused the tree's row; its output, whole:"
       echo "  [\"step run: ready_when references no step\"]"
       echo "publish-workflow: REFUSED — the tree's $file does not lint clean"; exit 4 ;;
    8) echo "publish-workflow: REFUSED — '$kind' has no live active row at $BOSS_JOBS_URL — the seed admits a new kind"; exit 8 ;;
    9) echo "publish-workflow: HELD: '$kind' is HELD out of every unattended publish (declared in infra/platform/workflow-holds/$kind.toml)"
       echo "publish-workflow: REFUSED — --check HELD: would NOT publish $file over live v$ver at $BOSS_JOBS_URL — the tree is ahead of live and the kind is held"; exit 9 ;;
    75) echo "publish-workflow: could not read $BOSS_JOBS_URL/api/workflows/$kind — HTTP 000"
        echo "publish-workflow: cannot answer: nothing compared, nothing published"; exit 75 ;;
    *) echo "stub: unexpected rc $rc"; exit 99 ;;
  esac
fi
if [ -n "$mode" ]; then echo "stub: unexpected mode $mode"; exit 2; fi
case " ${STUB_PUBLISH_FAIL:-} " in
  *" $kind "*) echo "publish-workflow: NOT CONFIRMED — the active row is still v$ver after a publish over v$ver"; exit 7 ;;
esac
case " ${STUB_PUBLISH_HELD:-} " in
  *" $kind "*) echo "publish-workflow: REFUSED — '$kind' is HELD out of every unattended publish (declared in infra/platform/workflow-holds/$kind.toml): a hold that landed late"; exit 9 ;;
esac
# What a case does to the checkout WHILE a publish runs (a hold landing,
# the checkout moving): the verb must look again before the next kind.
[ -z "${STUB_PUBLISH_HOOK:-}" ] || bash -c "$STUB_PUBLISH_HOOK"
next=$((ver + 1))
echo "publish-workflow: publishing $file as ${BOSS_ACTOR:-unset} for packet ${OPS_REQUEST_ID:-none}"
echo "publish-workflow: $kind v$ver -> v$next live at $BOSS_JOBS_URL — confirmed by reading the active row back and comparing it to $file (equal); packet ${OPS_REQUEST_ID:-none}"
exit 0
"#,
        );
        // Stub curl: the pr-train read, from a file; STUB_SOR_DOWN
        // answers nothing.
        write_exec(
            &bin.join("curl"),
            r#"#!/bin/sh
[ -n "${STUB_SOR_DOWN:-}" ] && { echo "curl: (7) Failed to connect" >&2; exit 7; }
for a in "$@"; do case "$a" in @*) cp "${a#@}" "$STUB_PUT"; exit 0;; esac; done
cat "$STUB_TRAINS"
"#,
        );
        let trains_f = root.join("trains.json");
        write_file(&trains_f, &trains(&head));
        Self {
            root: root.clone(),
            bin,
            repo,
            stub,
            verdicts: root.join("verdicts.txt"),
            trains: trains_f,
            pw_log: root.join("pw.log"),
            head,
        }
    }

    /// The bundle: one file per kind named, in whatever order the case
    /// lists them (the verb reads the directory in file-name order,
    /// not this order), and `kind rc version` per line for what the
    /// planted sub-verb answers each.
    fn verdicts(&self, rows: &[(&str, u32, u32)]) {
        let bundle = self.repo.join("infra/platform/workflows");
        let _ = std::fs::remove_dir_all(&bundle);
        std::fs::create_dir_all(&bundle).unwrap();
        for (k, _, _) in rows {
            write_file(
                &bundle.join(format!("{k}.toml")),
                &format!("[[workflow]]\nkind = \"{k}\"\nlabel = \"{k}\"\n"),
            );
        }
        let body: String = rows
            .iter()
            .map(|(k, rc, v)| format!("{k} {rc} {v}\n"))
            .collect();
        write_file(&self.verdicts, &body);
    }

    /// Replace one kind's row file with a real row (the planted bundle
    /// is a stub per kind) — what the default hold is derived from.
    fn row_file(&self, kind: &str, body: &str) {
        write_file(
            &self
                .repo
                .join(format!("infra/platform/workflows/{kind}.toml")),
            body,
        );
    }

    /// Declare something under infra/platform/workflow-holds/.
    fn hold_file(&self, name: &str, body: &str) {
        let dir = self.repo.join(HOLDS_REL);
        std::fs::create_dir_all(&dir).unwrap();
        write_file(&dir.join(name), body);
    }

    fn clear_holds(&self) {
        let _ = std::fs::remove_dir_all(self.repo.join(HOLDS_REL));
    }

    fn clear_calls(&self) {
        let _ = std::fs::remove_file(&self.pw_log);
    }

    fn run(&self, args: &[&str]) -> (i32, String) {
        self.run_env(args, &[])
    }

    fn run_env(&self, args: &[&str], extra: &[(&str, String)]) -> (i32, String) {
        let mut cmd = Command::new("bash");
        cmd.arg(repo_root().join(SCRIPT))
            .args(args)
            // The ops runner hands a verb root's environment with no
            // HOME; the script must not need one.
            .env_clear()
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("BOSS_JOBS_URL", "http://sor.invalid")
            .env("OPS_REQUEST_ID", "aaaaaaaa-0000-4000-8000-000000000000")
            .env("BOSS_PUBLISH_WORKFLOW_REPO", &self.repo)
            .env("BOSS_PUBLISH_WORKFLOW_SH", &self.stub)
            .env("STUB_VERDICTS", &self.verdicts)
            .env("STUB_TRAINS", &self.trains)
            .env("STUB_PW_LOG", &self.pw_log);
        for (k, v) in extra {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("publish-drift.sh runs");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.code().unwrap_or(-1), text)
    }

    /// Every sub-verb call, in order: `<kind> --check` or `<kind> PUBLISH`.
    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(&self.pw_log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn publishes(&self) -> Vec<String> {
        self.calls()
            .into_iter()
            .filter_map(|l| l.strip_suffix(" PUBLISH").map(str::to_string))
            .collect()
    }
}

fn contains_all(text: &str, needles: &[&str], what: &str) {
    for n in needles {
        assert!(text.contains(n), "{what}: expected `{n}` in:\n{text}");
    }
}

/// The one line the table row for `kind` is on.
fn row<'a>(text: &'a str, kind: &str) -> &'a str {
    text.lines()
        .find(|l| l.starts_with(&format!("{kind} ")) || l.starts_with(&format!("{kind}\t")))
        .unwrap_or_else(|| panic!("no table row for {kind} in:\n{text}"))
}

fn ready() -> bool {
    let tomllib = Command::new("python3")
        .args(["-c", "import tomllib"])
        .output()
        .is_ok_and(|o| o.status.success());
    for (ok, why) in [
        (has("git"), "git"),
        (has("jq"), "jq"),
        (tomllib, "python3 with tomllib (the hold reader)"),
    ] {
        if !ok {
            eprintln!("skipping: publish-drift.sh needs {why}, and this box has none");
            return false;
        }
    }
    true
}

// ---------------------------------------------------------------------------
// --check: the drift set, classified, nothing written.
// ---------------------------------------------------------------------------

/// Every kind in the bundle is asked once with `--check`, classified by
/// the sub-verb's exit code, and listed in one table; a refusal-6 kind
/// is named field by field; the verdict line counts; nothing is
/// published; the first line names the checkout sha.
#[test]
fn check_lists_every_kind_by_the_sub_verbs_verdict_and_publishes_nothing() {
    if !ready() {
        return;
    }
    let c = Case::new("check");
    c.verdicts(&[
        ("maintenance-alpha", 0, 3),
        ("maintenance-beta", 5, 2),
        ("maintenance-gamma", 6, 31),
        ("maintenance-delta", 4, 1),
        ("maintenance-epsilon", 8, 0),
    ]);
    let (rc, out) = c.run(&["--check"]);
    assert_eq!(rc, 0, "{out}");
    let first = out.lines().next().unwrap_or_default();
    assert!(
        first.starts_with("publish-drift: checkout ") && first.contains(&c.head[..12]),
        "the first line names the checkout sha: `{first}`"
    );
    contains_all(
        row(&out, "maintenance-alpha"),
        &["v3", "would publish"],
        "the tree-ahead row",
    );
    contains_all(
        row(&out, "maintenance-beta"),
        &["v2", "equal"],
        "the equal row",
    );
    contains_all(
        row(&out, "maintenance-gamma"),
        &["v31", "REFUSED", "never said"],
        "the refusal-6 row",
    );
    // The drift is named field by field, copied from the sub-verb —
    // the operator's decision needs the fields, not the count.
    contains_all(
        &out,
        &["steps[1].fields", "\"proof\"", "steps[1].metadata_defaults"],
        "the refusal-6 fields",
    );
    contains_all(
        row(&out, "maintenance-delta"),
        &["REFUSED", "lint"],
        "the lint-failed row",
    );
    contains_all(
        row(&out, "maintenance-epsilon"),
        &["no live row"],
        "the pending row",
    );
    contains_all(
        &out,
        &["publish-drift: would publish 1, skipped 1 equal, refused 2"],
        "the verdict line",
    );
    assert!(
        c.publishes().is_empty(),
        "--check published: {:?}",
        c.calls()
    );
    let checks: Vec<_> = c
        .calls()
        .into_iter()
        .filter(|l| l.ends_with(" --check"))
        .collect();
    assert_eq!(checks.len(), 5, "one --check per kind: {checks:?}");
}

/// No argument is `--check`: the mode a rule-filed packet carries by
/// the verb file's default, and the safe one.
#[test]
fn no_mode_is_check() {
    if !ready() {
        return;
    }
    let c = Case::new("default-mode");
    c.verdicts(&[("maintenance-alpha", 0, 3)]);
    let (rc, out) = c.run(&[]);
    assert_eq!(rc, 0, "{out}");
    contains_all(&out, &["would publish 1"], "the default mode's verdict");
    assert!(
        c.publishes().is_empty(),
        "no mode published: {:?}",
        c.calls()
    );
}

// ---------------------------------------------------------------------------
// --for-real: each tree-ahead kind, in bundle order, through the sub-verb.
// ---------------------------------------------------------------------------

/// Only the tree-ahead kinds are published, in bundle (file-name)
/// order, each through `publish-workflow.sh <kind>`; the equal and the
/// refused are listed and left; the table shows each movement.
#[test]
fn for_real_publishes_only_tree_ahead_kinds_in_bundle_order() {
    if !ready() {
        return;
    }
    let c = Case::new("for-real");
    // Listed out of order on purpose: bundle order is file-name order.
    c.verdicts(&[
        ("maintenance-epsilon", 0, 1),
        ("maintenance-gamma", 6, 31),
        ("maintenance-beta", 0, 7),
        ("maintenance-delta", 5, 2),
        ("maintenance-alpha", 0, 3),
    ]);
    let (rc, out) = c.run(&["--for-real"]);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(
        c.publishes(),
        vec![
            "maintenance-alpha".to_string(),
            "maintenance-beta".to_string(),
            "maintenance-epsilon".to_string()
        ],
        "the tree-ahead kinds, in bundle order, and no other: {:?}",
        c.calls()
    );
    contains_all(
        row(&out, "maintenance-alpha"),
        &["v3", "v4", "published"],
        "a published row shows its movement",
    );
    contains_all(
        row(&out, "maintenance-gamma"),
        &["v31", "REFUSED", "never said"],
        "the refusal-6 row under --for-real",
    );
    contains_all(
        &out,
        &["publish-drift: published 3, skipped 1 equal, refused 1"],
        "the verdict line",
    );
    // Every publish is signed as the runner's automation, the
    // identity rule every `boss` verb applies.
    contains_all(&out, &["as automation:ops-runner"], "the actor");
}

/// The load-bearing negative: a live row the tree never said is never
/// published by this verb, even when it is the only drift there is —
/// no `--force-tree` exists here; that stays the Drift tab's approve,
/// one kind at a time, with the field named.
#[test]
fn a_refusal_6_kind_is_never_published_even_alone() {
    if !ready() {
        return;
    }
    let c = Case::new("refusal-6-alone");
    c.verdicts(&[("maintenance-gamma", 6, 31)]);
    let (rc, out) = c.run(&["--for-real"]);
    assert_eq!(
        rc, 0,
        "a refusal is the operator's decision, not the verb's failure: {out}"
    );
    assert!(
        c.publishes().is_empty(),
        "published over a refusal-6: {:?}",
        c.calls()
    );
    contains_all(
        &out,
        &[
            "steps[1].fields",
            "publish-drift: published 0, skipped 0 equal, refused 1",
            "force-tree",
        ],
        "the refusal listed, and the way through named",
    );
    assert!(
        !out.contains("--for-real --force-tree"),
        "the verb must not offer itself a force: {out}"
    );
}

/// A publish the sub-verb did not confirm is reported on its row and
/// the verb exits non-zero — while the other kinds still land, because
/// each kind's publish is independent and a stopped loop would leave
/// the operator back in the loop.
#[test]
fn an_unconfirmed_publish_is_reported_and_exits_nonzero() {
    if !ready() {
        return;
    }
    let c = Case::new("unconfirmed");
    c.verdicts(&[
        ("maintenance-alpha", 0, 3),
        ("maintenance-beta", 0, 7),
        ("maintenance-gamma", 0, 1),
    ]);
    let (rc, out) = c.run_env(
        &["--for-real"],
        &[("STUB_PUBLISH_FAIL", "maintenance-beta".into())],
    );
    assert_eq!(rc, 1, "{out}");
    contains_all(
        row(&out, "maintenance-beta"),
        &["NOT CONFIRMED"],
        "the unconfirmed row",
    );
    contains_all(
        row(&out, "maintenance-gamma"),
        &["published"],
        "the kind after the failure still lands",
    );
    contains_all(
        &out,
        &["publish-drift: published 2, skipped 0 equal, refused 0, held 0, not confirmed 1"],
        "the verdict counts the failure by name",
    );
}

// ---------------------------------------------------------------------------
// Freshness: a stale checkout is `not yet`, never a publish.
// ---------------------------------------------------------------------------

/// The newest converged train's merge sha is read off the system of
/// record; when the checkout does not carry it the answer is `not yet`
/// with both shas named, exit 75, and the sub-verb is never asked —
/// the operator's `until --check ok` loop, as a verdict.
#[test]
fn a_checkout_behind_the_newest_train_is_not_yet_75_and_asks_nothing() {
    if !ready() {
        return;
    }
    let c = Case::new("stale");
    c.verdicts(&[("maintenance-alpha", 0, 3)]);
    let ahead = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
    write_file(&c.trains, &trains(ahead));
    for mode in ["--check", "--for-real"] {
        let (rc, out) = c.run(&[mode]);
        assert_eq!(rc, 75, "{mode}: {out}");
        contains_all(
            &out,
            &[
                &format!(
                    "not yet: checkout at {}, main at {}",
                    &c.head[..8],
                    &ahead[..8]
                ),
                "t-new",
            ],
            "the not-yet line names both shas and the train",
        );
        assert!(
            c.calls().is_empty(),
            "{mode} asked the sub-verb: {:?}",
            c.calls()
        );
    }
}

/// A system of record that cannot be read, or one that lists no
/// converged train (a denied scope answers an empty page, not an
/// error), is 75 — never a publish from an unjudged checkout.
#[test]
fn an_unreadable_or_empty_train_listing_cannot_answer() {
    if !ready() {
        return;
    }
    let c = Case::new("sor-down");
    c.verdicts(&[("maintenance-alpha", 0, 3)]);
    let (rc, out) = c.run_env(&["--for-real"], &[("STUB_SOR_DOWN", "1".into())]);
    assert_eq!(rc, 75, "{out}");
    contains_all(&out, &["cannot answer"], "the unreadable read");
    assert!(c.calls().is_empty(), "{:?}", c.calls());

    write_file(&c.trains, r#"{"data":[],"total":0}"#);
    let (rc, out) = c.run(&["--for-real"]);
    assert_eq!(rc, 75, "{out}");
    contains_all(
        &out,
        &["no ", "pr-train"],
        "the empty listing is named, not read as a fact",
    );
    assert!(c.calls().is_empty(), "{:?}", c.calls());
}

/// A sub-verb answer that is not a verdict — the registry could not be
/// read for one kind — stops the run at 75: half a drift set judged
/// from half a registry is the confident wrong answer.
#[test]
fn a_sub_verb_that_cannot_answer_stops_the_run() {
    if !ready() {
        return;
    }
    let c = Case::new("sub-75");
    c.verdicts(&[
        ("maintenance-alpha", 0, 3),
        ("maintenance-beta", 75, 0),
        ("maintenance-gamma", 0, 1),
    ]);
    let (rc, out) = c.run(&["--for-real"]);
    assert_eq!(rc, 75, "{out}");
    contains_all(
        &out,
        &["maintenance-beta", "cannot answer"],
        "the kind that could not be read",
    );
    assert!(
        c.publishes().is_empty(),
        "published past an unreadable kind: {:?}",
        c.calls()
    );
}

/// No system of record is a configuration fault, a mode outside the
/// two is usage, and a sub-verb that is not there is configuration —
/// each refused before anything is read.
#[test]
fn no_sor_a_foreign_mode_or_a_missing_sub_verb_is_refused_first() {
    if !ready() {
        return;
    }
    let c = Case::new("config");
    c.verdicts(&[("maintenance-alpha", 0, 3)]);
    let (rc, out) = c.run(&["--force-tree"]);
    assert_eq!(rc, 2, "{out}");
    contains_all(&out, &["--check", "--for-real"], "usage names the modes");
    let (rc, out) = c.run_env(&["--check"], &[("BOSS_JOBS_URL", String::new())]);
    assert_eq!(rc, 78, "{out}");
    contains_all(&out, &["BOSS_JOBS_URL"], "the missing address is named");
    let (rc, out) = c.run_env(
        &["--check"],
        &[(
            "BOSS_PUBLISH_WORKFLOW_SH",
            c.root.join("absent.sh").display().to_string(),
        )],
    );
    assert_eq!(rc, 78, "{out}");
    contains_all(&out, &["absent.sh"], "the missing sub-verb is named");
    assert!(c.calls().is_empty(), "{:?}", c.calls());
}

// ---------------------------------------------------------------------------
// HOLDS: a kind the tree holds out of the unattended publish (backlog
// 083d240e). A workflow row that turns on a REFUSAL goes live at a
// deliberate registry publish (design b08725c2; design 09618594 question
// `signer`), and until this door the tree published it by itself on the
// next clean check — and published it AGAIN after a rollback, because
// the tree was ahead of live once more.
// ---------------------------------------------------------------------------

const HOLD: &str = "drift_publish = \"held\"\nwhy = '''This row turns on a refusal on the approve path; it goes live at a deliberate publish with its positive control.'''\nlifts = 'backlog 6c9183de: removed by the car that follows the deliberate publish'\n";

/// A row that declares a field `writer` — a refusal row by construction.
fn writer_row(kind: &str) -> String {
    format!(
        "[[workflow]]\nkind = \"{kind}\"\nlabel = \"{kind}\"\ncategory = \"platform\"\nsubject_kinds = [\"custom\"]\n\n[[workflow.step]]\ntitle = \"approve\"\nkind = \"sign-off\"\nready_when = \"true\"\nfields = [{{ name = \"decision\", field_type = \"string\", writer = \"signer\" }}]\n"
    )
}

/// A row whose step declares an `executor` — the other refusal shape.
fn executor_row(kind: &str) -> String {
    format!(
        "[[workflow]]\nkind = \"{kind}\"\nlabel = \"{kind}\"\ncategory = \"platform\"\nsubject_kinds = [\"custom\"]\n\n[[workflow.step]]\ntitle = \"execute\"\nkind = \"task\"\nready_when = \"true\"\nexecutor = \"runner:ops\"\n"
    )
}

fn verdict_line(out: &str) -> &str {
    out.lines()
        .find(|l| {
            l.starts_with("publish-drift: would publish ")
                || l.starts_with("publish-drift: published ")
        })
        .unwrap_or_else(|| panic!("no verdict line in:\n{out}"))
}

/// The check NAMES a held kind, with its why and what lifts it, and
/// counts it as `held` — never in `would publish` (it will not be) and
/// never in `refused` (one refusal holds the whole for-real, and a hold
/// must not stop the other kinds).
#[test]
fn check_names_a_held_kind_and_counts_it_in_neither_would_publish_nor_refused() {
    if !ready() {
        return;
    }
    let c = Case::new("hold-check");
    c.verdicts(&[
        ("maintenance-alpha", 0, 3),
        ("maintenance-beta", 0, 9),
        ("maintenance-gamma", 5, 2),
    ]);
    c.hold_file("maintenance-beta.toml", HOLD);
    let (rc, out) = c.run(&["--check"]);
    assert_eq!(rc, 0, "{out}");
    assert!(
        verdict_line(&out)
            .starts_with("publish-drift: would publish 1, skipped 1 equal, refused 0, held 1"),
        "{out}"
    );
    contains_all(
        row(&out, "maintenance-beta"),
        &["v9", "HELD", "turns on a refusal", "6c9183de"],
        "the held row carries its why and what lifts it",
    );
    assert!(
        !row(&out, "maintenance-beta").contains("\twould publish"),
        "a held kind is not listed as a publish: {out}"
    );
    // One greppable line per held kind, outside the table.
    contains_all(
        &out,
        &[
            "publish-drift: held maintenance-beta (declared in infra/platform/workflow-holds/maintenance-beta.toml)",
        ],
        "the held line",
    );
    assert!(c.publishes().is_empty(), "{:?}", c.calls());

    // No hold declared: the count is still stated, as zero.
    c.clear_holds();
    let (rc, out) = c.run(&["--check"]);
    assert_eq!(rc, 0, "{out}");
    assert!(
        verdict_line(&out)
            .starts_with("publish-drift: would publish 2, skipped 1 equal, refused 0, held 0"),
        "{out}"
    );
}

/// The publish itself: the others land, the held kind does not — and it
/// still does not on the NEXT run, which is the rollback case (the old
/// row republished by hand leaves the tree ahead of live again).
#[test]
fn for_real_publishes_the_others_and_never_a_held_kind_run_after_run() {
    if !ready() {
        return;
    }
    let c = Case::new("hold-for-real");
    c.verdicts(&[
        ("maintenance-alpha", 0, 3),
        ("maintenance-beta", 0, 9),
        ("maintenance-gamma", 0, 1),
    ]);
    c.hold_file("maintenance-beta.toml", HOLD);
    for pass in ["first", "after a rollback"] {
        c.clear_calls();
        let (rc, out) = c.run(&["--for-real"]);
        assert_eq!(rc, 0, "{pass}: {out}");
        assert_eq!(
            c.publishes(),
            vec![
                "maintenance-alpha".to_string(),
                "maintenance-gamma".to_string()
            ],
            "{pass}: the held kind is not published and the others are: {:?}",
            c.calls()
        );
        assert!(
            verdict_line(&out)
                .starts_with("publish-drift: published 2, skipped 0 equal, refused 0, held 1"),
            "{pass}: {out}"
        );
        contains_all(
            &out,
            &["publish-drift: held maintenance-beta (declared in "],
            pass,
        );
    }
}

/// A rollback made with a row the tree never said (the sub-verb's 6) on
/// a held kind is still `held`, not `refused`: the kind is out of this
/// verb's hands in every state, and the others publish.
#[test]
fn a_held_kind_is_held_in_every_state_the_sub_verb_reports() {
    if !ready() {
        return;
    }
    let c = Case::new("hold-states");
    c.verdicts(&[
        ("maintenance-alpha", 0, 3),
        ("maintenance-beta", 6, 9),
        ("maintenance-delta", 5, 4),
        ("maintenance-gamma", 4, 1),
    ]);
    for k in ["maintenance-beta", "maintenance-delta", "maintenance-gamma"] {
        c.hold_file(&format!("{k}.toml"), HOLD);
    }
    let (rc, out) = c.run(&["--for-real"]);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(c.publishes(), vec!["maintenance-alpha".to_string()]);
    assert!(
        verdict_line(&out)
            .starts_with("publish-drift: published 1, skipped 0 equal, refused 0, held 3"),
        "{out}"
    );
    contains_all(
        row(&out, "maintenance-beta"),
        &["HELD", "never said"],
        "the state rides the held row",
    );
    contains_all(
        row(&out, "maintenance-delta"),
        &["HELD", "equal"],
        "an equal held kind is still named",
    );
}

/// TWO LAYERS, ONE READER (review R1 of the signer car, run 7cee49b9).
/// The sub-verb holds too — it answers 9 where a held kind would have
/// been `--check ok`, and refuses 9 to publish one — because the
/// `publish-workflow` verb is a second machine road. This verb reads
/// that 9 as held (with the version, from the sub-verb's own phrase),
/// reports the kind ONCE, and still publishes the others. A 9 at the
/// publish itself (a hold that landed after this run's last look) is a
/// held row, not a failed publish. And a 9 the reader, asked again,
/// does not bear out is a tree moving under the run: not an answer.
#[test]
fn the_sub_verbs_own_hold_is_read_as_held_once_and_never_as_a_failure() {
    if !ready() {
        return;
    }
    let c = Case::new("hold-layers");
    c.verdicts(&[
        ("maintenance-alpha", 0, 3),
        ("maintenance-beta", 9, 7),
        ("maintenance-gamma", 0, 1),
    ]);
    c.hold_file("maintenance-beta.toml", HOLD);
    let (rc, out) = c.run(&["--for-real"]);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(
        c.publishes(),
        vec![
            "maintenance-alpha".to_string(),
            "maintenance-gamma".to_string()
        ],
        "{out}"
    );
    contains_all(
        row(&out, "maintenance-beta"),
        &["v7", "HELD", "ahead of live v7"],
        "the sub-verb's 9",
    );
    assert_eq!(
        out.matches("publish-drift: held maintenance-beta ").count(),
        1,
        "one held line per kind, whichever layer said it: {out}"
    );
    assert!(
        verdict_line(&out)
            .starts_with("publish-drift: published 2, skipped 0 equal, refused 0, held 1"),
        "{out}"
    );

    // A hold that lands between this verb's look and the sub-verb's
    // publish: refused there, held here, exit 0, the other kind lands.
    let c = Case::new("hold-at-publish");
    c.verdicts(&[("maintenance-alpha", 0, 3), ("maintenance-beta", 0, 9)]);
    let (rc, out) = c.run_env(
        &["--for-real"],
        &[("STUB_PUBLISH_HELD", "maintenance-beta".into())],
    );
    assert_eq!(rc, 0, "{out}");
    contains_all(
        row(&out, "maintenance-beta"),
        &["HELD", "refused by the sub-verb"],
        "held at the publish",
    );
    contains_all(
        row(&out, "maintenance-alpha"),
        &["published"],
        "the other kind",
    );
    let v = verdict_line(&out);
    assert!(
        v.starts_with("publish-drift: published 1, skipped 0 equal, refused 0, held 1")
            && !v.contains("not confirmed"),
        "{out}"
    );

    // The sub-verb says held and the reader, asked again, does not.
    let c = Case::new("hold-disagree");
    c.verdicts(&[("maintenance-alpha", 0, 3), ("maintenance-beta", 9, 7)]);
    let (rc, out) = c.run(&["--for-real"]);
    assert_eq!(rc, 75, "{out}");
    contains_all(
        &out,
        &["cannot answer", "maintenance-beta"],
        "the layers disagree",
    );
    assert!(c.publishes().is_empty(), "{:?}", c.calls());
}

/// A registry that cannot be read for a HELD kind still stops the run:
/// the hold changes what is published, never what counts as an answer.
#[test]
fn an_unreadable_held_kind_still_stops_the_run() {
    if !ready() {
        return;
    }
    let c = Case::new("hold-75");
    c.verdicts(&[("maintenance-alpha", 0, 3), ("maintenance-beta", 75, 0)]);
    c.hold_file("maintenance-beta.toml", HOLD);
    let (rc, out) = c.run(&["--for-real"]);
    assert_eq!(rc, 75, "{out}");
    assert!(c.publishes().is_empty(), "{:?}", c.calls());
}

/// FAIL CLOSED. A hold that cannot be read is not an absent hold: the
/// run refuses in BOTH modes, names the file, asks the sub-verb nothing,
/// publishes nothing, and prints no verdict line a rule could read as
/// clean.
#[test]
fn a_hold_that_cannot_be_read_refuses_both_modes_and_publishes_nothing() {
    if !ready() {
        return;
    }
    let cases: &[(&str, &str, &str, &str)] = &[
        (
            "unparsable",
            "maintenance-beta.toml",
            "drift_publish = \"held\nwhy = ",
            "maintenance-beta.toml",
        ),
        (
            "unknown-kind",
            "maintenance-betta.toml",
            HOLD,
            "no infra/platform/workflows/maintenance-betta.toml",
        ),
        (
            "no-why",
            "maintenance-beta.toml",
            "drift_publish = \"held\"\nlifts = 'backlog 6c9183de'\n",
            "`why`",
        ),
        (
            "empty-why",
            "maintenance-beta.toml",
            "drift_publish = \"held\"\nwhy = '  '\nlifts = 'backlog 6c9183de'\n",
            "`why`",
        ),
        (
            "no-lifts",
            "maintenance-beta.toml",
            "drift_publish = \"held\"\nwhy = 'This row turns on a refusal and goes live deliberately.'\n",
            "`lifts`",
        ),
        (
            "no-state",
            "maintenance-beta.toml",
            "why = 'This row turns on a refusal and goes live deliberately.'\nlifts = 'backlog 6c9183de'\n",
            "`drift_publish`",
        ),
        (
            "foreign-state",
            "maintenance-beta.toml",
            "drift_publish = \"hold\"\nwhy = 'This row turns on a refusal and goes live deliberately.'\nlifts = 'backlog 6c9183de'\n",
            "`drift_publish`",
        ),
        (
            "unknown-key",
            "maintenance-beta.toml",
            "drift_publish = \"held\"\nwhy = 'This row turns on a refusal and goes live deliberately.'\nlifts = 'backlog 6c9183de'\nuntil = 'tomorrow'\n",
            "`until`",
        ),
        (
            "stray-file",
            "maintenance-beta.tml",
            HOLD,
            "maintenance-beta.tml",
        ),
        (
            "release-of-nothing",
            "maintenance-beta.toml",
            "drift_publish = \"released\"\nwhy = 'This row was published by hand and its control passed.'\n",
            "releases nothing",
        ),
    ];
    for (name, file, body, needle) in cases {
        let c = Case::new(&format!("hold-bad-{name}"));
        c.verdicts(&[("maintenance-alpha", 0, 3), ("maintenance-beta", 0, 9)]);
        c.hold_file(file, body);
        for mode in ["--check", "--for-real"] {
            let (rc, out) = c.run(&[mode]);
            assert_eq!(rc, 78, "{name} {mode}: {out}");
            contains_all(
                &out,
                &[needle, "REFUSED", "nothing published"],
                &format!("{name} {mode}"),
            );
            assert!(
                !out.contains("would publish ") && !out.contains("publish-drift: published "),
                "{name} {mode}: a refused run prints no verdict a rule could read: {out}"
            );
            assert!(
                c.calls().is_empty(),
                "{name} {mode} asked or published: {:?}",
                c.calls()
            );
        }
    }
}

/// THE DEFAULT (backlog 083d240e, part 5). A row that declares a field
/// `writer` or a step `executor` is a refusal row by construction, so it
/// is held with NO file — forgetting the file cannot publish a refusal
/// unattended. The file is the override in both directions: `held` adds
/// the why and the lift, `released` hands the row back to this verb.
#[test]
fn a_row_declaring_a_writer_or_an_executor_is_held_by_default_and_a_release_file_frees_it() {
    if !ready() {
        return;
    }
    let c = Case::new("hold-default");
    c.verdicts(&[
        ("maintenance-alpha", 0, 3),
        ("maintenance-beta", 0, 9),
        ("maintenance-gamma", 0, 1),
    ]);
    c.row_file("maintenance-beta", &writer_row("maintenance-beta"));
    c.row_file("maintenance-gamma", &executor_row("maintenance-gamma"));
    let (rc, out) = c.run(&["--for-real"]);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(c.publishes(), vec!["maintenance-alpha".to_string()]);
    assert!(
        verdict_line(&out)
            .starts_with("publish-drift: published 1, skipped 0 equal, refused 0, held 2"),
        "{out}"
    );
    contains_all(
        &out,
        &[
            "publish-drift: held maintenance-beta (by default: step approve field decision declares writer signer)",
            "publish-drift: held maintenance-gamma (by default: step execute declares executor runner:ops)",
        ],
        "the default names what it read",
    );

    // Released on the record: the row is this verb's again, and named.
    c.clear_calls();
    c.hold_file(
        "maintenance-beta.toml",
        "drift_publish = \"released\"\nwhy = 'Published by hand on 2026-10-07 and the positive control passed; later edits may ride the drift publish.'\n",
    );
    let (rc, out) = c.run(&["--for-real"]);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(
        c.publishes(),
        vec![
            "maintenance-alpha".to_string(),
            "maintenance-beta".to_string()
        ]
    );
    contains_all(
        &out,
        &[
            "publish-drift: released maintenance-beta (declared in infra/platform/workflow-holds/maintenance-beta.toml)",
        ],
        "a release is named every time too",
    );
    assert!(
        verdict_line(&out)
            .starts_with("publish-drift: published 2, skipped 0 equal, refused 0, held 1"),
        "{out}"
    );
}

/// The check that filed a `--for-real` may predate the hold, and a
/// checkout can move while the publishes run. The verb looks again
/// before EVERY publish: a hold that appeared is honoured, and a
/// checkout that moved stops the rest (they were judged against another
/// tree) with a non-zero exit.
#[test]
fn for_real_looks_again_before_every_publish() {
    if !ready() {
        return;
    }
    let c = Case::new("hold-late");
    c.verdicts(&[("maintenance-alpha", 0, 3), ("maintenance-beta", 0, 9)]);
    let holds = c.repo.join(HOLDS_REL);
    let late = c.root.join("late-hold.toml");
    write_file(&late, HOLD);
    let hook = format!(
        "mkdir -p '{}' && cp '{}' '{}/maintenance-beta.toml'",
        holds.display(),
        late.display(),
        holds.display()
    );
    let (rc, out) = c.run_env(&["--for-real"], &[("STUB_PUBLISH_HOOK", hook)]);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(
        c.publishes(),
        vec!["maintenance-alpha".to_string()],
        "a hold that landed mid-run is honoured: {out}"
    );
    contains_all(
        row(&out, "maintenance-beta"),
        &["HELD"],
        "the late hold's row",
    );
    assert!(
        verdict_line(&out)
            .starts_with("publish-drift: published 1, skipped 0 equal, refused 0, held 1"),
        "{out}"
    );

    let c = Case::new("hold-moved");
    c.verdicts(&[("maintenance-alpha", 0, 3), ("maintenance-beta", 0, 9)]);
    let hook = format!(
        "git -C '{}' -c user.name=fixture -c user.email=f@example.invalid commit -q --allow-empty -m 'the checkout moved'",
        c.repo.display()
    );
    let (rc, out) = c.run_env(&["--for-real"], &[("STUB_PUBLISH_HOOK", hook)]);
    assert_eq!(rc, 1, "{out}");
    assert_eq!(c.publishes(), vec!["maintenance-alpha".to_string()]);
    contains_all(
        row(&out, "maintenance-beta"),
        &["NOT PUBLISHED", "moved"],
        "the kind judged against another tree",
    );
}

/// The shipped tree's own holds are readable: a hold that would make
/// the live verb refuse is refused HERE, on the car that wrote it.
#[test]
fn the_shipped_holds_are_readable() {
    if !has("python3") {
        eprintln!("skipping: the hold reader needs python3");
        return;
    }
    let out = Command::new("python3")
        .arg(repo_root().join(HOLDS_READER))
        .arg(repo_root())
        .output()
        .expect("the reader runs");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success(),
        "{HOLDS_REL} does not read clean in this tree — publish-drift would refuse every run:\n{text}"
    );
    assert!(
        repo_root().join(HOLDS_REL).join("README.md").is_file(),
        "{HOLDS_REL}/README.md is what keeps the directory, and says the shape"
    );
}

// ---------------------------------------------------------------------------
// One definition: the sub-verb's exit codes and phrases are what this
// verb reads. Pinned rather than collapsed — the phrases live in the
// sub-verb's say/refuse calls and the classification in its EXIT block.
// ---------------------------------------------------------------------------

/// The phrases `publish-drift.sh` reads a version or a verdict out of
/// are the sub-verb's own words: each appears in BOTH files, so a
/// rewording of one names the other here instead of reading `?`.
#[test]
fn the_phrases_the_table_reads_are_the_sub_verbs_own() {
    let drift = std::fs::read_to_string(repo_root().join(SCRIPT)).expect(SCRIPT);
    let sub = std::fs::read_to_string(repo_root().join(SUB_VERB)).expect(SUB_VERB);
    for phrase in [
        "--check ok: would publish",
        "already says what",
        "carries what the tree never said",
        "-> v",
        "--check HELD: would NOT publish",
    ] {
        assert!(sub.contains(phrase), "{SUB_VERB} no longer says `{phrase}`");
        assert!(drift.contains(phrase), "{SCRIPT} does not read `{phrase}`");
    }
    // The exit codes are the sub-verb's documented contract, read by
    // number in the classifier: each number the EXIT block documents
    // for a refusal appears as a case there.
    for code in ["5)", "6)", "4)", "8)", "7)", "9)"] {
        assert!(
            drift.contains(code),
            "{SCRIPT} has no case for sub-verb exit {code}"
        );
    }
}

// ---------------------------------------------------------------------------
// THROUGH THE RUNNER, with the real allowlist, as boss-gcp.
// ---------------------------------------------------------------------------

fn shipped_verbs(root: &Path) -> PathBuf {
    let dst = root.join("verbs");
    std::fs::create_dir_all(&dst).unwrap();
    for e in std::fs::read_dir(repo_root().join("infra/ops/verbs")).expect("infra/ops/verbs/") {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "json") {
            std::fs::copy(&p, dst.join(p.file_name().unwrap())).unwrap();
        }
    }
    dst
}

/// One open ops-request for boss-gcp carrying the verb and args, run
/// through `ops-runner.sh` against the stub `curl`: the jobs read
/// answers the packet, the pr-train read answers the fixture, what the runner
/// recorded on the step is kept.
fn run_runner(c: &Case, verbs: &Path, args: &str) -> (String, Option<serde_json::Value>) {
    write_exec(
        &c.bin.join("curl"),
        &[
            "#!/bin/sh\n",
            boss_testing::ops_runner_stub::RECORD_STEP_METADATA,
            r#"url=""
for a in "$@"; do case "$a" in http*) url="$a" ;; esac; done
case "$url" in
  *kind=pr-train*) cat "$STUB_TRAINS"; exit 0 ;;
esac
cat "$STUB_JOBS"
"#,
        ]
        .concat(),
    );
    write_file(
        &c.root.join("jobs.json"),
        &format!(
            r#"{{"data":[{{"id":"aaaaaaaa-0000-4000-8000-000000000000","status":"open","metadata":{{"host":"boss-gcp","verb":"publish-drift","args":{args}}},"steps":[{{"id":"s-execute","spec_slug":"execute","status":"ready","metadata":{{"authority_role":"platform-admin"}}}}]}}]}}"#
        ),
    );
    let step_md = c.root.join("step-metadata.json");
    let _ = std::fs::remove_file(&step_md);
    let _ = std::fs::remove_file(&c.pw_log);
    let out = Command::new("sh")
        .arg(repo_root().join("infra/ops/ops-runner.sh"))
        .env_clear()
        .env(
            "PATH",
            format!(
                "{}:{}",
                c.bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("HOST_ID", "boss-gcp")
        .env("BOSS_JOBS_URL", "http://sor.invalid")
        .env("OPS_VERBS_DIR", verbs)
        .env("STUB_JOBS", c.root.join("jobs.json"))
        .env("STUB_STEP_METADATA", &step_md)
        .env("BOSS_PUBLISH_WORKFLOW_REPO", &c.repo)
        .env("BOSS_PUBLISH_WORKFLOW_SH", &c.stub)
        .env("STUB_VERDICTS", &c.verdicts)
        .env("STUB_TRAINS", &c.trains)
        .env("STUB_PW_LOG", &c.pw_log)
        .output()
        .expect("ops-runner.sh runs");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let meta = boss_testing::ops_runner_stub::step_metadata_written(&step_md);
    (text, meta)
}

/// A packet with NO args — the shape a `jobs.spawn` rule files, since a
/// rule cannot carry a list — runs `--check` by the verb file's default
/// and is ANSWERED with the table; `--for-real` publishes; a word
/// outside the two literals is REFUSED by the runner with the reason
/// on the step, and the script never runs.
#[test]
fn through_the_runner_no_args_is_check_for_real_publishes_and_a_foreign_mode_is_refused() {
    if !ready() {
        return;
    }
    let c = Case::new("runner");
    c.verdicts(&[("maintenance-alpha", 0, 3), ("maintenance-gamma", 6, 31)]);
    let verbs = shipped_verbs(&c.root);

    let (text, meta) = run_runner(&c, &verbs, "[]");
    let meta = meta.unwrap_or_else(|| panic!("no step completed: {text}"));
    assert_eq!(meta["disposition"], "answered", "{meta}");
    assert_eq!(meta["exit_code"], "0", "{meta}");
    let out = meta["output"].as_str().unwrap_or_default();
    contains_all(
        out,
        &[
            "publish-drift: checkout ",
            "would publish 1, skipped 0 equal, refused 1",
        ],
        "the no-args answer is a --check",
    );
    assert!(
        c.publishes().is_empty(),
        "no args published: {:?}",
        c.calls()
    );

    let (text, meta) = run_runner(&c, &verbs, r#"["--for-real"]"#);
    let meta = meta.unwrap_or_else(|| panic!("no step completed: {text}"));
    assert_eq!(meta["disposition"], "answered", "{meta}");
    assert_eq!(meta["exit_code"], "0", "{meta}");
    let out = meta["output"].as_str().unwrap_or_default();
    contains_all(
        out,
        &["published 1, skipped 0 equal, refused 1"],
        "the --for-real answer",
    );
    assert_eq!(c.publishes(), vec!["maintenance-alpha".to_string()]);

    let (text, meta) = run_runner(&c, &verbs, r#"["--force-tree"]"#);
    let meta = meta.unwrap_or_else(|| panic!("no step completed: {text}"));
    assert_eq!(meta["disposition"], "refused", "{meta}");
    let reason = meta["reason"].as_str().unwrap_or_default();
    assert!(
        reason.contains("is not one of --check, --for-real"),
        "the refusal names the literal list: {reason}"
    );
    assert!(
        c.calls().is_empty(),
        "a refused mode reached the script: {:?}",
        c.calls()
    );
}

/// The verb file itself: MUTATING, authorized by name, boss-gcp only,
/// the script this test runs with one literal-list param whose default
/// is `--check` (so a rule-filed packet, which carries no args, can
/// only ever check), and a timeout sized for a whole bundle of
/// sub-verb runs rather than one.
#[test]
fn the_verb_file_has_the_reviewed_shape() {
    let v: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(repo_root().join(VERB_FILE)).expect(VERB_FILE),
    )
    .expect("the verb file is JSON");
    let about = v["about"].as_str().unwrap_or_default();
    contains_all(
        about,
        &[
            "MUTATING",
            "David",
            "--check",
            "--for-real",
            "publish-workflow.sh",
            "27fad542",
        ],
        "the verb's about",
    );
    assert!(
        about.to_lowercase().contains("never said"),
        "the about names the refusal this verb never overrides"
    );
    assert!(
        about.contains("no --force-tree"),
        "the about says this verb has no --force-tree"
    );
    assert_eq!(v["hosts"], serde_json::json!(["boss-gcp"]));
    assert_eq!(v["argv"], serde_json::json!([SCRIPT, "{1}"]));
    let params = v["params"].as_array().expect("params");
    assert_eq!(params.len(), 1, "one param, the mode: {params:?}");
    assert_eq!(params[0]["name"], "mode");
    assert_eq!(
        params[0]["one_of"],
        serde_json::json!(["--check", "--for-real"])
    );
    assert_eq!(
        params[0]["default"], "--check",
        "a packet with no args must check, never publish"
    );
    let sub: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(repo_root().join("infra/ops/verbs/publish-workflow.json"))
            .expect("infra/ops/verbs/publish-workflow.json"),
    )
    .expect("the sub-verb file is JSON");
    let one = sub["timeout"]
        .as_u64()
        .expect("publish-workflow declares a timeout");
    let whole = v["timeout"]
        .as_u64()
        .expect("publish-drift declares a timeout");
    assert!(
        whole >= 4 * one,
        "publish-drift's timeout ({whole}s) must cover many sub-verb runs (one is {one}s)"
    );
}
