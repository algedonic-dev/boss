//! A gate runs the `run.sh` of the sha it gates, and says which one ran.
//!
//! THE DEFECT (backlog e3100bc8, measured on car 646c0ade). The gate pod
//! runs `bash /repo-script/run.sh`, and `/repo-script` is ConfigMap
//! `gate-runner-script`, which the forge converge rewrites from MAIN after
//! every train. So a car that changes run.sh was gated by the OLD script:
//! gate 19b7253f's receipt carried no `seed_order`, the key its own run.sh
//! adds, and so did every gate between the merge and the converge. A
//! run.sh car was proven only by pins that lift functions out of the
//! script, never by a gate that ran it.
//!
//! THE FIX (design 1d7a1669, shape C, car 1 of 2). The ConfigMap carries
//! a fixed `bootstrap.sh` beside main's `run.sh`. The bootstrap resolves
//! the branch ONCE, fetches that sha's run.sh and execs it with
//! `GATE_SHA`; when it cannot, it execs main's copy and says so. run.sh
//! checks out `GATE_SHA` rather than the head it finds, and writes
//! `runner_source` / `runner_sha256` onto the receipt — which script ran
//! is read from the record, never inferred from an absent key. Car 2
//! points the manifest's command at the bootstrap.
//!
//! These pins run the bootstrap and the lifted run.sh blocks as they
//! ship, against a local bare repository standing in for the forge.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const RUN_SH: &str = "infra/gate-runner/run.sh";
const BOOTSTRAP: &str = "infra/gate-runner/bootstrap.sh";
const BRANCH: &str = "fix/a-runner-change";

fn read(rel: &str) -> String {
    fs::read_to_string(repo_root().join(rel)).unwrap_or_else(|e| panic!("reading {rel}: {e}"))
}

/// The text between two markers of run.sh, exactly as it ships.
fn block(begin: &str, end: &str) -> String {
    let src = read(RUN_SH);
    let a = src
        .find(begin)
        .unwrap_or_else(|| panic!("{RUN_SH} has no `{begin}` marker"));
    let b = src
        .find(end)
        .unwrap_or_else(|| panic!("{RUN_SH} has no `{end}` marker"));
    assert!(a < b, "`{begin}` must precede `{end}`");
    src[a..b].to_string()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", dir)
        .args([
            "-c",
            "user.email=gate@test",
            "-c",
            "user.name=gate",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A bare repository standing in for the forge, with `main` and BRANCH,
/// whose run.sh is `runner_body`. Returns (forge dir, work dir).
fn forge(dir: &Path, runner_body: &str) -> (PathBuf, PathBuf) {
    let bare = dir.join("forge.git");
    let work = dir.join("work");
    fs::create_dir_all(&bare).unwrap();
    fs::create_dir_all(work.join("infra/gate-runner")).unwrap();
    git(&bare, &["init", "-q", "--bare"]);
    // The forge (Forgejo) serves filtered and by-id fetches; a local
    // bare repository has to be told to.
    git(&bare, &["config", "uploadpack.allowFilter", "true"]);
    git(&bare, &["config", "uploadpack.allowAnySHA1InWant", "true"]);
    git(&work, &["init", "-q"]);
    write_file(&work.join("README"), "main\n");
    git(&work, &["add", "-A"]);
    git(&work, &["commit", "-q", "-m", "main"]);
    git(
        &work,
        &["push", "-q", bare.to_str().unwrap(), "HEAD:refs/heads/main"],
    );
    git(&work, &["checkout", "-q", "-b", BRANCH]);
    write_file(&work.join("infra/gate-runner/run.sh"), runner_body);
    git(&work, &["add", "-A"]);
    git(&work, &["commit", "-q", "-m", "the branch's runner"]);
    git(
        &work,
        &[
            "push",
            "-q",
            bare.to_str().unwrap(),
            &format!("HEAD:refs/heads/{BRANCH}"),
        ],
    );
    (bare, work)
}

fn sha256(path: &Path) -> String {
    let out = Command::new("sha256sum")
        .arg(path)
        .output()
        .expect("sha256sum");
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string()
}

const BRANCH_RUNNER: &str = "echo \"RAN=branch SHA=${GATE_SHA:-none} SOURCE=${GATE_RUNNER_SOURCE:-none} SHA256=${GATE_RUNNER_SHA256:-none}\"\n";
const MAIN_RUNNER: &str = "echo \"RAN=fallback SHA=${GATE_SHA:-none} SOURCE=${GATE_RUNNER_SOURCE:-none} REASON=${GATE_RUNNER_FALLBACK_REASON:-none}\"\n";

/// Run the bootstrap the way the pod would: from the ConfigMap's
/// directory, beside main's run.sh.
fn bootstrap(dir: &Path, forge_url: &str, branch: &str) -> Output {
    let cm = dir.join("configmap");
    fs::create_dir_all(&cm).unwrap();
    write_exec(&cm.join("bootstrap.sh"), &read(BOOTSTRAP));
    write_exec(&cm.join("run.sh"), MAIN_RUNNER);
    Command::new("bash")
        .arg(cm.join("bootstrap.sh"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", dir)
        .env("FORGE_URL", forge_url)
        .env("GATE_BRANCH", branch)
        .env("GATE_BOOT_DIR", dir.join("boot"))
        .env_remove("GATE_SHA")
        .env_remove("GATE_RUNNER_SOURCE")
        .env_remove("GATE_RUNNER_SHA256")
        .env_remove("GATE_RUNNER_FALLBACK_REASON")
        .output()
        .expect("bash runs the bootstrap")
}

/// THE ONE THIS CAR EXISTS FOR: the script that runs is the branch's, at
/// the sha the bootstrap resolved, and it is told so.
#[test]
fn the_bootstrap_execs_the_branchs_run_sh_at_the_gated_sha() {
    let dir = scratch_dir("bootstrap-branch-runner");
    let (bare, work) = forge(&dir, BRANCH_RUNNER);
    let tip = git(&work, &["rev-parse", "HEAD"]);
    let want_sha256 = sha256(&work.join("infra/gate-runner/run.sh"));

    let out = bootstrap(&dir, &format!("file://{}", bare.display()), BRANCH);
    let said = String::from_utf8_lossy(&out.stdout).to_string();
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    let _ = fs::remove_dir_all(&dir);

    assert!(out.status.success(), "{said}{err}");
    assert!(
        said.contains("RAN=branch"),
        "the bootstrap ran main's copy, not the branch's run.sh — the defect this \
         car exists for:\n{said}{err}"
    );
    assert!(
        said.contains(&format!("SHA={tip}")),
        "GATE_SHA must be the branch tip the bootstrap resolved ({tip}):\n{said}"
    );
    assert!(said.contains("SOURCE=branch"), "{said}");
    assert!(
        said.contains(&format!("SHA256={want_sha256}")),
        "GATE_RUNNER_SHA256 must be the sha256 of the bytes it execs:\n{said}"
    );
}

/// A fallback is allowed and is NEVER silent: main's copy runs, told it
/// is the fallback and why, and the reason is on stderr too.
#[test]
fn an_unreachable_branch_falls_back_to_mains_copy_and_says_so() {
    let dir = scratch_dir("bootstrap-fallback-runner");
    let (bare, _work) = forge(&dir, BRANCH_RUNNER);

    let out = bootstrap(
        &dir,
        &format!("file://{}", bare.display()),
        "fix/no-such-branch",
    );
    let said = String::from_utf8_lossy(&out.stdout).to_string();
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    let _ = fs::remove_dir_all(&dir);

    assert!(out.status.success(), "{said}{err}");
    assert!(said.contains("RAN=fallback"), "{said}{err}");
    assert!(
        said.contains("SOURCE=main-fallback"),
        "the fallback must be named on the run it hands over to:\n{said}"
    );
    assert!(
        said.contains("SHA=none"),
        "a fallback resolved no sha and must not pretend to gate one:\n{said}"
    );
    assert!(
        !said.contains("REASON=none") && err.contains("fix/no-such-branch"),
        "the reason must reach both the runner and the log, naming the branch:\n{said}{err}"
    );
}

/// Run run.sh's gated-sha block in a clone of `bare`, with BRANCH moved
/// on the forge AFTER the bootstrap resolved `gate_sha`.
fn checkout(dir: &Path, bare: &Path, gate_sha: Option<&str>) -> Output {
    let repo = dir.join("repo");
    let _ = fs::remove_dir_all(&repo);
    git(
        dir,
        &[
            "clone",
            "-q",
            "--depth",
            "50",
            &format!("file://{}", bare.display()),
            repo.to_str().unwrap(),
        ],
    );
    let script = format!(
        "set -euo pipefail\nfail_lost() {{ echo \"LOST: $1\"; exit 1; }}\nGATE_BRANCH={BRANCH}\nGATE_SHA={sha}\ncd {repo}\n{block}\necho \"HEAD_SHA=$HEAD_SHA\"\n",
        sha = gate_sha.unwrap_or(""),
        repo = repo.display(),
        block = block("# --- gated sha (begin) ---", "# --- gated sha (end) ---"),
    );
    Command::new("bash")
        .args(["-c", &script])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", dir)
        .output()
        .expect("bash")
}

/// The script and the tree it gates are ONE sha: a push after the
/// bootstrap resolved the branch does not move what is gated.
#[test]
fn run_sh_gates_the_resolved_sha_not_the_moving_head() {
    let dir = scratch_dir("runner-gates-gate-sha");
    let (bare, work) = forge(&dir, BRANCH_RUNNER);
    let resolved = git(&work, &["rev-parse", "HEAD"]);
    write_file(&work.join("later"), "pushed after the bootstrap\n");
    git(&work, &["add", "-A"]);
    git(&work, &["commit", "-q", "-m", "a later push"]);
    let moved = git(&work, &["rev-parse", "HEAD"]);
    git(
        &work,
        &[
            "push",
            "-q",
            bare.to_str().unwrap(),
            &format!("HEAD:refs/heads/{BRANCH}"),
        ],
    );

    let pinned = checkout(&dir, &bare, Some(&resolved));
    let unpinned = checkout(&dir, &bare, None);
    let missing = checkout(&dir, &bare, Some(&"0".repeat(40)));
    let _ = fs::remove_dir_all(&dir);

    let pinned_out = String::from_utf8_lossy(&pinned.stdout);
    assert!(
        pinned.status.success() && pinned_out.contains(&format!("HEAD_SHA={resolved}")),
        "with GATE_SHA set the gate must check out that sha, not the head the branch \
         moved to ({moved}):\n{pinned_out}{}",
        String::from_utf8_lossy(&pinned.stderr)
    );
    let unpinned_out = String::from_utf8_lossy(&unpinned.stdout);
    assert!(
        unpinned.status.success() && unpinned_out.contains(&format!("HEAD_SHA={moved}")),
        "with no GATE_SHA (a runner started without the bootstrap) the gate keeps \
         gating the branch head:\n{unpinned_out}{}",
        String::from_utf8_lossy(&unpinned.stderr)
    );
    let missing_out = String::from_utf8_lossy(&missing.stdout);
    assert!(
        !missing.status.success() && missing_out.contains("LOST:"),
        "a gated sha the forge no longer has must end the run `lost` with the reason, \
         never gate some other tree:\n{missing_out}"
    );
}

/// run.sh knows which script it is and where it came from, and keeps
/// what the bootstrap handed it out of every child's environment.
#[test]
fn run_sh_records_its_own_provenance_and_hands_it_to_no_child() {
    let dir = scratch_dir("runner-provenance");
    let script = dir.join("runner.sh");
    let probe = "bash -c 'echo \"child=${GATE_SHA:-unset}/${GATE_RUNNER_SOURCE:-unset}\"'\n";
    let body = format!(
        "set -euo pipefail\n{}\necho \"source=$RUNNER_SOURCE sha256=$RUNNER_SHA256 sha=$GATE_SHA reason=$RUNNER_FALLBACK_REASON\"\n{probe}",
        block(
            "# --- runner provenance (begin) ---",
            "# --- runner provenance (end) ---"
        )
    );
    write_file(&script, &body);
    let want = sha256(&script);

    let direct = Command::new("bash")
        .arg(&script)
        .env_remove("GATE_SHA")
        .env_remove("GATE_RUNNER_SOURCE")
        .env_remove("GATE_RUNNER_FALLBACK_REASON")
        .output()
        .expect("bash");
    let booted = Command::new("bash")
        .arg(&script)
        .env("GATE_SHA", "c0ffee")
        .env("GATE_RUNNER_SOURCE", "branch")
        .env_remove("GATE_RUNNER_FALLBACK_REASON")
        .output()
        .expect("bash");
    let _ = fs::remove_dir_all(&dir);

    let direct_out = String::from_utf8_lossy(&direct.stdout);
    assert!(
        direct.status.success()
            && direct_out.contains(&format!("source=configmap sha256={want} sha= reason=")),
        "run straight from the ConfigMap (no bootstrap) the runner says so and hashes \
         itself:\n{direct_out}{}",
        String::from_utf8_lossy(&direct.stderr)
    );
    let booted_out = String::from_utf8_lossy(&booted.stdout);
    assert!(
        booted.status.success()
            && booted_out.contains(&format!("source=branch sha256={want} sha=c0ffee")),
        "{booted_out}{}",
        String::from_utf8_lossy(&booted.stderr)
    );
    assert!(
        booted_out.contains("child=unset/unset"),
        "a child of run.sh inherited what the bootstrap handed it — the branch's \
         gate.sh and every test it runs would see it:\n{booted_out}"
    );
}

/// Run the receipt-summary block with the runner's provenance set.
fn summarize(dir: &Path, receipt_body: &str, provenance: &str) -> serde_json::Value {
    let receipt = dir.join("receipt.json");
    write_file(&receipt, receipt_body);
    let script = format!(
        "set -euo pipefail\nRECEIPT={r}\nHEAD_SHA=deadbeef\n{provenance}\n{block}\nprintf '%s' \"$SUMMARY\"\n",
        r = receipt.display(),
        block = block(
            "# --- receipt summary (begin) ---",
            "# --- receipt summary (end) ---"
        ),
    );
    let out = Command::new("bash")
        .args(["-c", &script])
        .output()
        .expect("bash");
    assert!(
        out.status.success(),
        "summary block failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("not JSON ({e}): {text}"))
}

/// Which script ran is READ from the record: the receipt carries the
/// source and the hash, on a readable receipt and an unreadable one.
#[test]
fn the_receipt_names_the_runner_that_produced_it() {
    let dir = scratch_dir("runner-on-the-receipt");
    let prov =
        "RUNNER_SOURCE=main-fallback\nRUNNER_SHA256=abc123\nRUNNER_FALLBACK_REASON='no branch'";
    let readable = summarize(&dir, r#"{"verdict":"green","head":"c0ffee"}"#, prov);
    let unreadable = summarize(&dir, "not json", prov);
    let _ = fs::remove_dir_all(&dir);

    for got in [&readable, &unreadable] {
        assert_eq!(got["runner_source"], "main-fallback", "{got}");
        assert_eq!(got["runner_sha256"], "abc123", "{got}");
        assert_eq!(got["runner_fallback_reason"], "no branch", "{got}");
    }
    assert_eq!(readable["verdict"], "green", "{readable}");
}
