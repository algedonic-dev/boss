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

use boss_testing::{repo_root, scratch_dir, without_proxy, write_exec, write_file};
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

/// Run run.sh's tree block against `bare`, with BRANCH moved on the
/// forge AFTER the bootstrap resolved `gate_sha`. (Until a531f508 this
/// cloned main itself and ran only the gated-sha block; the block now
/// makes its own repository, see `materialise`.)
fn checkout(dir: &Path, bare: &Path, gate_sha: Option<&str>) -> Output {
    materialise(dir, bare, gate_sha).0
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

/// Run run.sh's whole tree block as it ships — from no repository at all
/// to the gated checkout — against `bare`, with every connection the run
/// opens to the forge traced. Returns the run, the number of those
/// connections, and the repository it built.
fn materialise(dir: &Path, bare: &Path, gate_sha: Option<&str>) -> (Output, usize, PathBuf) {
    let repo = dir.join("repo");
    let trace = dir.join("trace2.json");
    let _ = fs::remove_dir_all(&repo);
    let _ = fs::remove_file(&trace);
    let script = format!(
        "set -euo pipefail\nfail_lost() {{ echo \"LOST: $1\"; exit 1; }}\nFORGE_URL=file://{bare}\nGATE_BRANCH={BRANCH}\nGATE_SHA={sha}\n{block}\necho \"HEAD_SHA=$HEAD_SHA\"\n",
        bare = bare.display(),
        sha = gate_sha.unwrap_or(""),
        block = block(
            "# --- the gate's tree (begin) ---",
            "# --- the gate's tree (end) ---"
        )
        .replace("/gate-target/repo", repo.to_str().unwrap()),
    );
    let out = Command::new("bash")
        .args(["-c", &script])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", dir)
        .env("GIT_TRACE2_EVENT", &trace)
        .output()
        .expect("bash");
    // Over file:// every connection to the forge is one upload-pack the
    // client starts — the same count as the gate's requests over http.
    let trips = fs::read_to_string(&trace)
        .unwrap_or_default()
        .lines()
        .filter(|l| l.contains("\"event\":\"child_start\"") && l.contains("git-upload-pack"))
        .count();
    (out, trips, repo)
}

/// ONE TRIP TO THE FORGE FOR THE TREE (backlog a531f508). run.sh used to
/// `git clone --depth 50` main and then fetch the branch again — two
/// connections and two transfers on every gate, ~122 a day, after the
/// bootstrap had already resolved the sha. One fetch carries main (the
/// trunk the scoped gate diffs against) and the gated commit together.
#[test]
fn the_gate_fetches_its_tree_from_the_forge_once() {
    let dir = scratch_dir("runner-one-forge-fetch");
    let (bare, work) = forge(&dir, BRANCH_RUNNER);
    let tip = git(&work, &["rev-parse", "HEAD"]);
    let main = git(&work, &["rev-parse", "main"]);

    let mut seen = Vec::new();
    for (label, sha) in [("pinned", Some(tip.as_str())), ("unpinned", None)] {
        let (out, trips, repo) = materialise(&dir, &bare, sha);
        let said = String::from_utf8_lossy(&out.stdout).to_string();
        let err = String::from_utf8_lossy(&out.stderr).to_string();
        assert!(
            out.status.success() && said.contains(&format!("HEAD_SHA={tip}")),
            "{label}: the tree block must end on the gated commit {tip}:\n{said}{err}"
        );
        // The trunk is still there for `gate.sh --auto`, which derives a
        // car's scope from its merge base with it.
        let base = git(&repo, &["merge-base", "origin/main", "HEAD"]);
        seen.push((label, trips, base));
    }
    let _ = fs::remove_dir_all(&dir);

    for (label, trips, base) in seen {
        assert_eq!(
            trips, 1,
            "{label}: the gate opened {trips} connections to the forge for its tree; one \
             fetch carries main and the gated commit (a531f508)"
        );
        assert_eq!(base, main, "{label}: the merge base with the trunk is main");
    }
}

/// EVERY TRIP TO THE FORGE IS ON THE RECEIPT (backlog 9f3a6107). The
/// seed decision asks the forge whether main moved during the gate — a
/// deliberate second fetch of main — and it used to make it AFTER the
/// receipt was written, inside a `$(...)` that could count nothing, so
/// `clone.fetches` said 1 on a run that had made 2. Run the tree block
/// and then the main re-read as they ship, against the stand-in forge:
/// the connections traced must equal `fetches` plus `seed_fetches`, on
/// a green run with a seed (which re-reads) and a red one (which does
/// not), and the seed decision itself fetches nothing.
#[test]
fn the_receipt_counts_every_forge_fetch_the_run_made() {
    let dir = scratch_dir("runner-counts-the-seed-fetch");
    let (bare, work) = forge(&dir, BRANCH_RUNNER);
    let tip = git(&work, &["rev-parse", "HEAD"]);
    let seed = dir.join("seed");
    fs::create_dir_all(&seed).unwrap();
    let repo = dir.join("repo");
    let trace = dir.join("trace2.json");

    let mut seen = Vec::new();
    for verdict in ["green", "failed"] {
        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_file(&trace);
        let script = format!(
            "set -euo pipefail\nfail_lost() {{ echo \"LOST: $1\"; exit 1; }}\nFORGE_URL=file://{bare}\nGATE_BRANCH={BRANCH}\nGATE_SHA={tip}\nSEED={seed}\nVERDICT={verdict}\n{tree}\n{reread}\necho \"CLONE_JSON=$CLONE_JSON\"\necho \"SEED_MAIN=$SEED_MAIN\"\n",
            bare = bare.display(),
            seed = seed.display(),
            tree = block(
                "# --- the gate's tree (begin) ---",
                "# --- the gate's tree (end) ---"
            )
            .replace("/gate-target/repo", repo.to_str().unwrap()),
            reread = block(
                "# --- main, re-read for the seed (begin) ---",
                "# --- main, re-read for the seed (end) ---"
            ),
        );
        let out = Command::new("bash")
            .args(["-c", &script])
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", &dir)
            .env("GIT_TRACE2_EVENT", &trace)
            .output()
            .expect("bash");
        let said = String::from_utf8_lossy(&out.stdout).to_string();
        let err = String::from_utf8_lossy(&out.stderr).to_string();
        let trips = fs::read_to_string(&trace)
            .unwrap_or_default()
            .lines()
            .filter(|l| l.contains("\"event\":\"child_start\"") && l.contains("git-upload-pack"))
            .count();
        assert!(out.status.success(), "{verdict}: {said}{err}");
        let json = said
            .lines()
            .find_map(|l| l.strip_prefix("CLONE_JSON="))
            .unwrap_or_else(|| panic!("{verdict}: no CLONE_JSON:\n{said}{err}"));
        let clone: serde_json::Value =
            serde_json::from_str(json).unwrap_or_else(|e| panic!("{verdict}: {e}: {json}"));
        seen.push((verdict, trips, clone, said));
    }
    let _ = fs::remove_dir_all(&dir);

    for (verdict, trips, clone, said) in &seen {
        let counted =
            clone["fetches"].as_u64().unwrap_or(99) + clone["seed_fetches"].as_u64().unwrap_or(99);
        assert_eq!(
            *trips as u64, counted,
            "{verdict}: the run opened {trips} connection(s) to the forge and the receipt \
             counts {clone} — every fetch the run makes is on the receipt (9f3a6107):\n{said}"
        );
    }
    assert_eq!(
        seen[0].2["seed_fetches"], 1,
        "a green run with a seed re-reads main: {}",
        seen[0].2
    );
    assert!(seen[0].3.contains("SEED_MAIN=fetched"), "{}", seen[0].3);
    assert_eq!(
        seen[1].2["seed_fetches"], 0,
        "a red run does not re-read main: {}",
        seen[1].2
    );

    let decision = block("seed_decision() {", "SEED_DECISION=$(seed_decision)");
    assert!(
        !decision.contains("git fetch"),
        "the seed decision runs inside $(...), where a fetch it makes cannot be counted; \
         it reads SEED_MAIN instead:\n{decision}"
    );
}

/// The re-read's bound as it ships, and the bound these pins put in its
/// place, so a stalled forge costs a test two seconds rather than sixty.
const REREAD_BOUND: &str = "timeout -k 5 60 git fetch";
const REREAD_BOUND_IN_TEST: &str = "timeout -k 1 2 git fetch";

/// The "main, re-read for the seed" block as it ships, with its bound
/// shortened (a no-op if the block has none, which the stall pin catches).
fn reread_block() -> String {
    block(
        "# --- main, re-read for the seed (begin) ---",
        "# --- main, re-read for the seed (end) ---",
    )
    .replace(REREAD_BOUND, REREAD_BOUND_IN_TEST)
}

/// A FAILED RE-READ IS STILL A TRIP (review 1527ebca, N1). The forge is
/// gone by the time the seed asks whether main moved: the re-read fails,
/// the seed is not refreshed, and the receipt still counts the trip the
/// run made, because a fetch that failed went to the forge all the same.
#[test]
fn a_failed_reread_of_main_is_still_counted() {
    let dir = scratch_dir("runner-counts-a-failed-seed-fetch");
    let (bare, work) = forge(&dir, BRANCH_RUNNER);
    let tip = git(&work, &["rev-parse", "HEAD"]);
    let seed = dir.join("seed");
    fs::create_dir_all(&seed).unwrap();
    let repo = dir.join("repo");
    let script = format!(
        "set -euo pipefail\nfail_lost() {{ echo \"LOST: $1\"; exit 1; }}\nFORGE_URL=file://{bare}\nGATE_BRANCH={BRANCH}\nGATE_SHA={tip}\nSEED={seed}\nVERDICT=green\n{tree}\nrm -rf {bare}\n{reread}\necho \"CLONE_JSON=$CLONE_JSON\"\necho \"SEED_MAIN=$SEED_MAIN\"\n",
        bare = bare.display(),
        seed = seed.display(),
        tree = block(
            "# --- the gate's tree (begin) ---",
            "# --- the gate's tree (end) ---"
        )
        .replace("/gate-target/repo", repo.to_str().unwrap()),
        reread = reread_block(),
    );
    let out = Command::new("bash")
        .args(["-c", &script])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", &dir)
        .output()
        .expect("bash");
    let said = String::from_utf8_lossy(&out.stdout).to_string();
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    let _ = fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{said}{err}");
    assert!(
        said.contains("SEED_MAIN=fetch-failed"),
        "the forge was gone, so the re-read failed:\n{said}{err}"
    );
    let json = said
        .lines()
        .find_map(|l| l.strip_prefix("CLONE_JSON="))
        .unwrap_or_else(|| panic!("no CLONE_JSON:\n{said}{err}"));
    let clone: serde_json::Value =
        serde_json::from_str(json).unwrap_or_else(|e| panic!("{e}: {json}"));
    assert_eq!(
        clone["seed_fetches"], 1,
        "a re-read that failed still went to the forge, so it is counted (9f3a6107): {clone}"
    );
}

/// A STALLED FORGE CANNOT HOLD THE VERDICT (review 1527ebca, B2). The
/// re-read runs BEFORE `leave_verdict`, so a fetch with no limit against
/// a forge that accepts and never answers would keep a green gate's
/// receipt line and termination message unwritten for as long as the
/// stall lasts, and a container that ended meanwhile would be settled
/// `lost`. The fetch is bounded; a fetch the bound stopped is a failed
/// one (no refresh) and still a counted trip. The stand-in is a listener
/// on 127.0.0.1 that accepts every connection and sends nothing.
#[test]
fn a_stalled_forge_cannot_hold_the_verdict() {
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    let dir = scratch_dir("runner-seed-fetch-against-a-stalled-forge");
    let seed = dir.join("seed");
    let repo = dir.join("repo");
    fs::create_dir_all(&seed).unwrap();
    fs::create_dir_all(&repo).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
    listener.set_nonblocking(true).expect("nonblocking");
    let port = listener.local_addr().expect("local addr").port();
    let accepted = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let holder = {
        let (accepted, stop) = (Arc::clone(&accepted), Arc::clone(&stop));
        std::thread::spawn(move || {
            let mut held = Vec::new();
            while !stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((conn, _)) => {
                        accepted.fetch_add(1, Ordering::SeqCst);
                        held.push(conn);
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(20)),
                }
            }
        })
    };

    git(&repo, &["init", "-q"]);
    git(
        &repo,
        &[
            "remote",
            "add",
            "-t",
            "main",
            "origin",
            &format!("http://127.0.0.1:{port}/david/boss.git"),
        ],
    );
    let script = format!(
        "set -euo pipefail\ncd {repo}\nSEED={seed}\nVERDICT=green\nCLONE_JSON='{{\"s\":4,\"fetches\":1}}'\n{reread}\necho \"CLONE_JSON=$CLONE_JSON\"\necho \"SEED_MAIN=$SEED_MAIN\"\n",
        repo = repo.display(),
        seed = seed.display(),
        reread = reread_block(),
    );
    // The outer 20 s is this test's own guard, so an unbounded re-read
    // fails the pin rather than hanging it. No proxy: git's http fetch
    // must reach the loopback stand-in, not a proxy that fails it fast.
    let started = Instant::now();
    let mut cmd = Command::new("timeout");
    let out = without_proxy(&mut cmd)
        .args(["20", "bash", "-c", &script])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", &dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("timeout runs bash");
    let took = started.elapsed();
    stop.store(true, Ordering::SeqCst);
    let _ = holder.join();
    let said = String::from_utf8_lossy(&out.stdout).to_string();
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    let _ = fs::remove_dir_all(&dir);

    assert!(
        out.status.success(),
        "the re-read against a forge that never answers had not returned after {took:?} \
         (exit {:?}); it runs before leave_verdict, so the verdict waited on it:\n{said}{err}",
        out.status.code()
    );
    assert!(
        took < Duration::from_secs(15),
        "the re-read returned, but only after {took:?}"
    );
    assert!(
        accepted.load(Ordering::SeqCst) >= 1,
        "the re-read never reached the stalled stand-in, so the stall was not exercised:\n{said}{err}"
    );
    assert!(
        said.contains("SEED_MAIN=fetch-failed"),
        "a fetch the bound stopped is a failed one:\n{said}{err}"
    );
    let json = said
        .lines()
        .find_map(|l| l.strip_prefix("CLONE_JSON="))
        .unwrap_or_else(|| panic!("no CLONE_JSON:\n{said}{err}"));
    let clone: serde_json::Value =
        serde_json::from_str(json).unwrap_or_else(|e| panic!("{e}: {json}"));
    assert_eq!(
        clone["seed_fetches"], 1,
        "a fetch the bound stopped still went to the forge: {clone}"
    );
    assert!(
        read(RUN_SH).contains(REREAD_BOUND),
        "run.sh's re-read of main carries the bound `{REREAD_BOUND}` this pin shortens"
    );
}

/// The single fetch is shallow from BOTH ends, so a branch whose merge
/// base lies further than its depth — a long branch, or one cut from an
/// old main — must still find it: without one, `gate.sh --auto` derives
/// no scope and refuses the car as having changed nothing. The clone this
/// replaced found the long branch's base and lost the old-base one.
#[test]
fn a_merge_base_beyond_the_fetch_depth_is_still_found() {
    for (label, own, behind) in [("long branch", 60, 0), ("old base", 2, 60)] {
        let dir = scratch_dir(&format!("runner-deep-base-{}", own + behind));
        let (bare, work) = forge(&dir, BRANCH_RUNNER);
        let base = git(&work, &["rev-parse", "main"]);
        for i in 0..own {
            write_file(&work.join("own"), &format!("{i}\n"));
            git(&work, &["add", "-A"]);
            git(&work, &["commit", "-q", "-m", "own"]);
        }
        let tip = git(&work, &["rev-parse", "HEAD"]);
        git(&work, &["checkout", "-q", "main"]);
        for i in 0..behind {
            write_file(&work.join("trunk"), &format!("{i}\n"));
            git(&work, &["add", "-A"]);
            git(&work, &["commit", "-q", "-m", "trunk"]);
        }
        let b = bare.to_str().unwrap();
        git(&work, &["push", "-q", "-f", b, "main", BRANCH]);

        let (out, _, repo) = materialise(&dir, &bare, Some(&tip));
        let said = String::from_utf8_lossy(&out.stdout).to_string();
        let err = String::from_utf8_lossy(&out.stderr).to_string();
        let found = Command::new("git")
            .current_dir(&repo)
            .args(["merge-base", "origin/main", "HEAD"])
            .output()
            .expect("git");
        let _ = fs::remove_dir_all(&dir);

        assert!(
            out.status.success() && said.contains(&format!("HEAD_SHA={tip}")),
            "{label}: {said}{err}"
        );
        assert_eq!(
            String::from_utf8_lossy(&found.stdout).trim(),
            base,
            "{label}: no merge base with the trunk within the fetched depth — the scoped gate \
             would refuse this car as changing nothing:\n{said}{err}"
        );
    }
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

/// What the tree cost is READ from the record too (a531f508): the
/// seconds and the forge fetches the tree block measured ride the
/// receipt, on a readable receipt and an unreadable one.
#[test]
fn the_receipt_carries_what_the_tree_cost() {
    let dir = scratch_dir("runner-clone-on-the-receipt");
    let prov = r#"CLONE_JSON='{"s":3,"fetches":1}'"#;
    let readable = summarize(&dir, r#"{"verdict":"green","head":"c0ffee"}"#, prov);
    let unreadable = summarize(&dir, "not json", prov);
    let _ = fs::remove_dir_all(&dir);

    for got in [&readable, &unreadable] {
        assert_eq!(got["clone"]["s"], 3, "{got}");
        assert_eq!(got["clone"]["fetches"], 1, "{got}");
    }
}
