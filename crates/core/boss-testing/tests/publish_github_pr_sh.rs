//! `infra/forge/publish-github-pr.sh --check` is RUN, not read — against
//! fixture paths, a fixture compose file and (where this box is root) a
//! second uid, so every verdict below is one the script actually reached.
//!
//! THE DEFECT (backlog ed84b5d9, measured 2026-09-11). Two faults in one
//! refusal:
//!
//! 1. The verb's `BOSS_FORGE_REPO_PATH` default —
//!    `/opt/forgejo/data/git/repositories/david/boss.git` — appeared
//!    EXACTLY ONCE in the tree, as its own default, with the only other
//!    references being test overrides that substitute a tmpdir. It was
//!    authored from a plausible Forgejo layout and had never been run
//!    against the real host: ops-request 04975694 ran `--check` on the
//!    forge and the repo path was the one and only failure.
//! 2. `check_inputs` tested the path with one `git rev-parse` and
//!    reported `forge repository not found at <path>` — the SAME sentence
//!    whether the path was absent, was not a directory, was unreadable by
//!    the caller, or was readable but refused by git. CLAUDE.md §Doors
//!    ("a wrong target answers instead of erroring") in its most literal
//!    form, and CLAUDE.md §Diagnosis: a verdict must name what failed.
//!
//! 3. And the fix for (2) left the door still broken, because the check
//!    it added read the repository IN ITS OWN PROCESS (`git -c
//!    safe.directory=<repo> -C <repo> rev-parse`) while the publish
//!    FETCHES from it. `-c` cannot exempt a fetch SOURCE: a local fetch
//!    runs `git upload-pack` inside the source repository and git clears
//!    the command-line config crossing into it — its own trace reads
//!    `unset GIT_CONFIG_PARAMETERS … git-upload-pack '<src>'`. So on
//!    2026-09-11 `--check` returned ok at 13:41 and the publish FAILED at
//!    13:43 on the same host with "detected dubious ownership"
//!    (ops-request c258d3b7), and the mirror sat 271 commits behind under
//!    a green check. A `--check` that passes where the operation fails is
//!    worse than no `--check`, so the exemption now travels through a
//!    protected channel (a config file named by `GIT_CONFIG_GLOBAL`,
//!    which the child inherits) and `--check` PERFORMS the fetch.
//!
//! So this file pins FIVE distinct refusals and the derivation that
//! replaces the authored default: the path now comes from the compose
//! file that DECLARES the host directory mounted at the container's
//! `/data` (§9a — one definition, not a second copy in a shell default).
//!
//! Nothing here touches the network, the forge, or a real token: the run
//! path's token is a fixed non-secret string a stub render places, and it
//! is asserted never to be printed.

use boss_testing::{feed_stdin, repo_root};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn script() -> PathBuf {
    repo_root().join("infra/forge/publish-github-pr.sh")
}

fn scratch(case: &str) -> PathBuf {
    // Per-uid and per-process, and it REFUSES by name if a leftover
    // cannot be cleared — see `boss_testing::scratch`.
    let dir = boss_testing::scratch_dir(&format!("publish-github-pr-{case}"));
    // Traversable by the second uid the ownership cases drop to.
    // mode-bits-ok: a directory, not an executable this process runs
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755))
        .unwrap_or_else(|e| panic!("chmod 0755 {}: {e}", dir.display()));
    dir
}

/// A bare repository WITH a `refs/heads/main`, because that is the ref
/// the verb fetches. A fixture without one is not a stand-in for the
/// forge — and an empty bare repository is exactly the fixture that let
/// the `-c`-only check look right: `rev-parse --git-dir` answered and no
/// fetch was ever attempted.
fn git_init_bare(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let st = Command::new("git")
        .args(["init", "-q", "--bare", path.to_str().unwrap()])
        .status()
        .expect("git runs");
    assert!(st.success(), "git init --bare {}", path.display());
    let tree = git_in(path, &["hash-object", "-t", "tree", "-w", "--stdin"]);
    let commit = git_in(path, &["commit-tree", &tree, "-m", "seed"]);
    git_in(path, &["update-ref", "refs/heads/main", &commit]);
}

/// One git command in `dir`, with an identity so `commit-tree` works, and
/// its trimmed stdout. Panics with git's own words — never a bare status.
fn git_in(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(std::process::Stdio::null())
        .env("GIT_AUTHOR_NAME", "fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
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

/// THE NEGATIVE CONTROL, measured in the test rather than assumed: one
/// raw fetch from `src` as `uid`, exempting the source the way the script
/// USED to (`-c safe.directory=<src>`) or not at all. A test that asserts
/// the verb now succeeds proves nothing unless the same fetch, from the
/// same fixture, as the same uid, still FAILS without the protected
/// channel — so this is what tells those two apart.
fn control_fetch(root: &Path, src: &Path, uid: u32, exempt_via_c: bool) -> (bool, String) {
    let pen = root.join("control");
    std::fs::create_dir_all(&pen).unwrap();
    // mode-bits-ok: a directory the second uid writes into
    std::fs::set_permissions(&pen, std::fs::Permissions::from_mode(0o777)).unwrap();
    let dst = pen.join(if exempt_via_c {
        "via-c.git"
    } else {
        "bare.git"
    });
    let _ = std::fs::remove_dir_all(&dst);
    let exempt = if exempt_via_c {
        "-c \"safe.directory=$SRC\""
    } else {
        ""
    };
    let body = format!(
        "git init -q --bare \"$DST\" || exit 9\n\
         exec git -C \"$DST\" {exempt} fetch \"$SRC\" \
         \"+refs/heads/main:refs/control/main\"\n"
    );
    let out = Command::new("setpriv")
        .args([
            "--reuid".to_string(),
            uid.to_string(),
            "--regid".to_string(),
            uid.to_string(),
            "--clear-groups".to_string(),
            "sh".to_string(),
            "-c".to_string(),
            body,
        ])
        .env_clear()
        .env(
            "PATH",
            std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into()),
        )
        // Nothing but the exemption under test may exempt anything.
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("SRC", src)
        .env("DST", &dst)
        .output()
        .expect("setpriv runs");
    let mut text = String::from_utf8_lossy(&out.stdout).to_string();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

/// `--check` only asks that `gh`/`curl` EXIST, and it USES `jq` to read
/// the publisher off the verb file (backlog d2b7c947). This box may lack
/// them (the gate image has them), so stand in a stub for whatever is
/// missing — the same idiom `infra/lint/the-controls-are-bounded-verbs.sh`
/// uses. `git` is never stubbed: every verdict here is a real git's.
fn stub_bin(root: &Path) -> PathBuf {
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    // mode-bits-ok: a directory on PATH, not an executable
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    for tool in ["gh", "jq", "curl"] {
        if Command::new("sh")
            .args(["-c", &format!("command -v {tool} >/dev/null 2>&1")])
            .status()
            .is_ok_and(|s| s.success())
        {
            continue;
        }
        boss_testing::write_exec(&bin.join(tool), "#!/bin/sh\nexit 0\n");
    }
    bin
}

/// A state dir any uid can write, so the only thing a case varies is the
/// forge repository. No token: `--check` holds none, and a run renders
/// its own (backlog d2b7c947; the run path's fixture, `Run`).
fn base_env(root: &Path) -> Vec<(String, String)> {
    let state = root.join("state");
    std::fs::create_dir_all(&state).unwrap();
    // mode-bits-ok: a directory any uid writes into
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o777)).unwrap();
    // The forge's address file: on the host /etc/boss/sor.env, rendered
    // from infra/estate/estate.toml; here the same render into the
    // scratch root, so the verb derives its forge clone URL as it does
    // under the ops runner (backlog 5222163e).
    let sor_env = root.join("sor.env");
    let rendered = Command::new("bash")
        .arg(repo_root().join("infra/estate/render-sor-env.sh"))
        .arg("--to")
        .arg(&sor_env)
        .output()
        .expect("render sor.env");
    assert!(
        rendered.status.success(),
        "{}",
        String::from_utf8_lossy(&rendered.stderr)
    );
    vec![
        ("BOSS_PUBLISH_STATE_DIR".into(), state.display().to_string()),
        ("BOSS_SOR_ENV".into(), sor_env.display().to_string()),
    ]
}

/// `--check`, with a cleared environment (the ops-runner has no HOME).
/// `as_uid` runs the script under a second account, which is the only way
/// to exercise "unreadable by this user" and git's ownership refusal on a
/// box where every fixture belongs to the caller.
fn check(root: &Path, extra: &[(&str, String)], as_uid: Option<u32>) -> (bool, String) {
    let path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into());
    let mut cmd = match as_uid {
        None => {
            let mut c = Command::new("bash");
            c.arg(script());
            c
        }
        Some(uid) => {
            let mut c = Command::new("setpriv");
            c.args([
                "--reuid".to_string(),
                uid.to_string(),
                "--regid".to_string(),
                uid.to_string(),
                "--clear-groups".to_string(),
                "bash".to_string(),
                script().display().to_string(),
            ]);
            c
        }
    };
    let path = format!("{}:{path}", stub_bin(root).display());
    cmd.arg("--check").env_clear().env("PATH", path);
    for (k, v) in base_env(root) {
        cmd.env(k, v);
    }
    for (k, v) in extra {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("the verb runs");
    let mut text = String::from_utf8_lossy(&out.stdout).to_string();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

fn forge_path(p: &Path) -> (&'static str, String) {
    ("BOSS_FORGE_REPO_PATH", p.display().to_string())
}

/// The ownership cases need a second uid, which needs root and setpriv.
/// They SKIP loudly rather than pass vacuously (the gate image is root).
fn second_uid() -> Option<u32> {
    let is_root = Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim() == "0")
        .unwrap_or(false);
    let have_setpriv = Command::new("sh")
        .args(["-c", "command -v setpriv >/dev/null 2>&1"])
        .status()
        .is_ok_and(|s| s.success());
    if is_root && have_setpriv {
        Some(65534)
    } else {
        None
    }
}

// ---------------------------------------------------------------------
// Defect 2 — four refusals, four sentences.
// ---------------------------------------------------------------------

/// ABSENT. Nothing exists at the path, and the refusal names the deepest
/// path that DOES exist — which is what turns "the layout is wrong" into
/// "the layout is right up to here".
#[test]
fn an_absent_forge_repository_names_the_deepest_path_that_does_exist() {
    let root = scratch("absent");
    let here = root.join("forgejo");
    std::fs::create_dir_all(&here).unwrap();
    let missing = here.join("data/git/repositories/david/boss.git");
    let (ok, out) = check(&root, &[forge_path(&missing)], None);
    assert!(!ok, "--check passed with no forge repository: {out}");
    assert!(
        out.contains("nothing exists there"),
        "the absent case does not say nothing exists there: {out}"
    );
    assert!(
        out.contains(&here.display().to_string()),
        "the absent case does not name the deepest existing path {}: {out}",
        here.display()
    );
    assert!(
        !out.contains("not readable by this user"),
        "the absent case reads as a permission finding: {out}"
    );
}

/// NOT A DIRECTORY. A file where a bare repository was expected is its
/// own sentence — the commonest shape of a half-right path.
#[test]
fn a_forge_repository_path_that_is_a_regular_file_says_it_is_not_a_directory() {
    let root = scratch("regular-file");
    let file = root.join("boss.git");
    std::fs::write(&file, "not a repository\n").unwrap();
    let (ok, out) = check(&root, &[forge_path(&file)], None);
    assert!(!ok, "--check passed on a regular file: {out}");
    assert!(
        out.contains("is not a directory"),
        "the regular-file case does not say it is not a directory: {out}"
    );
    assert!(
        !out.contains("nothing exists there"),
        "the regular-file case reads as absent: {out}"
    );
}

/// UNREADABLE, AND THE USER IS NAMED. The verb runs as root through the
/// ops-runner, so "unreadable by root" and "unreadable by david" are
/// different findings and the message must say which.
#[test]
fn an_unreadable_forge_repository_names_the_user_that_cannot_read_it() {
    let Some(uid) = second_uid() else {
        eprintln!("publish_github_pr_sh: SKIPPED — needs root + setpriv for a second uid");
        return;
    };
    let root = scratch("unreadable");
    let locked = root.join("boss.git");
    std::fs::create_dir_all(&locked).unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let (ok, out) = check(&root, &[forge_path(&locked)], Some(uid));
    assert!(!ok, "--check passed on an unreadable directory: {out}");
    assert!(
        out.contains("not readable by this user"),
        "the unreadable case does not say it is a permission finding: {out}"
    );
    assert!(
        out.contains("nobody") || out.contains(&uid.to_string()),
        "the unreadable case does not NAME the user that cannot read it: {out}"
    );
    assert!(
        !out.contains("nothing exists there"),
        "the unreadable case reads as absent — the exact confusion ed84b5d9 filed: {out}"
    );
}

/// REFUSED BY GIT. A readable directory that is not a repository reports
/// GIT'S OWN WORDS, so no reader has to re-derive what git said.
#[test]
fn a_readable_directory_that_is_no_repository_reports_gits_own_words() {
    let root = scratch("not-a-repo");
    let dir = root.join("boss.git");
    std::fs::create_dir_all(&dir).unwrap();
    let (ok, out) = check(&root, &[forge_path(&dir)], None);
    assert!(
        !ok,
        "--check passed on a directory that is no repository: {out}"
    );
    assert!(
        out.contains("git refused it as a repository"),
        "the non-repository case does not say git refused it: {out}"
    );
    assert!(
        out.to_lowercase().contains("not a git repository"),
        "the non-repository case does not carry git's own words: {out}"
    );
}

/// A repository owned by ANOTHER account is read anyway. The ops-runner
/// executes verbs as root and this repository belongs to the Forgejo
/// container's account, so since git 2.35.2 every command refuses it as
/// "dubious ownership" — measured on this host class on ops-request
/// c9877f75 (2026-09-10). `--check` and the run read through one helper,
/// so this is the same read the fetch performs.
#[test]
fn a_repository_owned_by_another_account_is_read_anyway() {
    let Some(uid) = second_uid() else {
        eprintln!("publish_github_pr_sh: SKIPPED — needs root + setpriv for a second uid");
        return;
    };
    let root = scratch("foreign-owner");
    let repo = root.join("boss.git");
    git_init_bare(&repo);
    let (_ok, out) = check(&root, &[forge_path(&repo)], Some(uid));
    // `forge repo :` is the banner; `forge repositor…` only ever appears in
    // a refusal about the repository itself. (The token fixture stays
    // root-owned 0600, so the token line may still complain — a different
    // finding, and the point is that the REPOSITORY read no longer does.)
    assert!(
        !out.contains("forge repositor"),
        "a repository owned by another account was refused — the ownership check was not dropped: {out}"
    );
    assert!(
        !out.to_lowercase().contains("dubious ownership"),
        "git's ownership refusal reached the output: {out}"
    );
}

// ---------------------------------------------------------------------
// Defect 1 — the path is derived from the thing that declares it.
// ---------------------------------------------------------------------

/// The compose file declares which HOST directory is mounted at the
/// container's `/data`; the verb reads it instead of keeping a second
/// copy. A relative mount resolves against the compose file's directory.
#[test]
fn the_forge_repository_path_is_derived_from_the_compose_data_mount() {
    let root = scratch("derive-compose");
    let here = root.join("forgejo");
    std::fs::create_dir_all(&here).unwrap();
    let compose = here.join("docker-compose.yml");
    std::fs::write(
        &compose,
        "services:\n  server:\n    image: codeberg.org/forgejo/forgejo:16.0.2\n    volumes:\n      - ./data:/data\n      - /etc/timezone:/etc/timezone:ro\n",
    )
    .unwrap();
    let expected = here.join("data/git/repositories/david/boss.git");
    git_init_bare(&expected);
    let (ok, out) = check(
        &root,
        &[("BOSS_FORGE_COMPOSE", compose.display().to_string())],
        None,
    );
    assert!(ok, "--check failed on a derived path that exists: {out}");
    assert!(
        out.contains(&expected.display().to_string()),
        "--check does not report the derived path {}: {out}",
        expected.display()
    );
    assert!(
        out.contains(&compose.display().to_string()),
        "--check does not say the path was derived from {}: {out}",
        compose.display()
    );
}

/// And inside `/data`, the repository root is FORGEJO'S OWN —
/// `[repository] ROOT` from app.ini — translated from the container path
/// to the host one. `git/repositories` is only the image's default.
#[test]
fn the_derivation_takes_forgejos_own_repository_root_from_app_ini() {
    let root = scratch("derive-app-ini");
    let here = root.join("forgejo");
    let data = here.join("data");
    std::fs::create_dir_all(data.join("gitea/conf")).unwrap();
    let compose = here.join("docker-compose.yml");
    std::fs::write(
        &compose,
        "services:\n  server:\n    volumes:\n      - ./data:/data\n",
    )
    .unwrap();
    std::fs::write(
        data.join("gitea/conf/app.ini"),
        "[server]\nROOT_URL = http://10.20.0.15:3000/\n\n[repository]\nROOT = /data/elsewhere/repos\n",
    )
    .unwrap();
    let expected = data.join("elsewhere/repos/david/boss.git");
    git_init_bare(&expected);
    let (ok, out) = check(
        &root,
        &[("BOSS_FORGE_COMPOSE", compose.display().to_string())],
        None,
    );
    assert!(ok, "--check failed on the app.ini repository root: {out}");
    assert!(
        out.contains(&expected.display().to_string()),
        "--check does not report the app.ini repository root {}: {out}",
        expected.display()
    );
}

/// With no readable compose file the verb still answers — but it SAYS the
/// path is a fallback guess rather than presenting it as fact. The
/// unexercised default is what ed84b5d9 is about, so it may never again
/// be printed without that label.
#[test]
fn an_unreadable_compose_file_makes_the_check_label_the_path_a_fallback() {
    let root = scratch("no-compose");
    let (ok, out) = check(
        &root,
        &[(
            "BOSS_FORGE_COMPOSE",
            root.join("absent-compose.yml").display().to_string(),
        )],
        None,
    );
    assert!(!ok, "--check passed with no forge repository at all: {out}");
    assert!(
        out.contains("fallback"),
        "--check does not label the underived path a fallback: {out}"
    );
    assert!(
        out.contains("/opt/forgejo/data/git/repositories/david/boss.git"),
        "--check does not report the fallback path it actually used: {out}"
    );
}

/// The override still wins over every derivation — the bounded-verbs lint
/// and every case above depend on it.
#[test]
fn the_env_override_wins_over_the_derivation_and_says_so() {
    let root = scratch("override");
    let repo = root.join("boss.git");
    git_init_bare(&repo);
    let here = root.join("forgejo");
    std::fs::create_dir_all(&here).unwrap();
    let compose = here.join("docker-compose.yml");
    std::fs::write(
        &compose,
        "services:\n  server:\n    volumes:\n      - ./data:/data\n",
    )
    .unwrap();
    let (ok, out) = check(
        &root,
        &[
            forge_path(&repo),
            ("BOSS_FORGE_COMPOSE", compose.display().to_string()),
        ],
        None,
    );
    assert!(
        ok,
        "--check failed on an overridden, existing repository: {out}"
    );
    assert!(
        out.contains("BOSS_FORGE_REPO_PATH"),
        "--check does not say the path came from the override: {out}"
    );
    assert!(
        out.contains("--check ok"),
        "--check did not report ok: {out}"
    );
    assert!(
        !out.contains("not-a-real-token"),
        "--check printed the token fixture: {out}"
    );
}

// ---------------------------------------------------------------------
// Defect 3 — `--check` exercises the operation, not a cheaper cousin.
// ---------------------------------------------------------------------

/// `--check` FETCHES, and says what it read. The sha it reports is the
/// fixture's own `main`, so the line is evidence the fetch happened
/// rather than a claim that it would.
#[test]
fn the_check_performs_the_forge_fetch_and_names_what_it_read() {
    let root = scratch("fetch-performed");
    let repo = root.join("boss.git");
    git_init_bare(&repo);
    let head = git_in(&repo, &["rev-parse", "refs/heads/main"]);
    let (ok, out) = check(&root, &[forge_path(&repo)], None);
    assert!(ok, "--check failed on a fetchable repository: {out}");
    assert!(
        out.contains("forge fetch:"),
        "--check does not report the fetch it performed: {out}"
    );
    assert!(
        out.contains(&head),
        "--check does not name the sha it fetched ({head}): {out}"
    );
}

/// THE REGRESSION THAT LET THIS SHIP. A repository git reads perfectly
/// well and the fetch cannot: an empty bare repository, where
/// `rev-parse --git-dir` answers and `refs/heads/main` does not exist.
/// The old `--check` said ok; the publish would have failed. So the pin
/// is not "the exemption works" — it is "`--check` fails wherever the
/// fetch fails", whatever the reason.
#[test]
fn a_forge_repository_whose_fetch_cannot_succeed_fails_the_check() {
    let root = scratch("fetch-unfetchable");
    let repo = root.join("boss.git");
    // Deliberately NOT git_init_bare: no main, nothing to fetch.
    std::fs::create_dir_all(repo.parent().unwrap()).unwrap();
    let st = Command::new("git")
        .args(["init", "-q", "--bare", repo.to_str().unwrap()])
        .status()
        .expect("git runs");
    assert!(st.success());
    let (ok, out) = check(&root, &[forge_path(&repo)], None);
    assert!(
        !ok,
        "--check passed on a repository the publish cannot fetch from — \
         the 2026-09-11 defect exactly: {out}"
    );
    assert!(
        out.contains("the FETCH the publish performs fails"),
        "the refusal does not say the FETCH is what failed: {out}"
    );
    assert!(
        out.to_lowercase().contains("couldn't find remote ref")
            || out.to_lowercase().contains("could not find remote ref"),
        "the refusal does not carry git's own words about the fetch: {out}"
    );
    // And it is its OWN sentence — not one of the four path findings.
    assert!(
        !out.contains("nothing exists there") && !out.contains("is not a directory"),
        "the fetch finding reads as a path finding: {out}"
    );
}

/// A repository owned by ANOTHER account is FETCHED, not merely read —
/// with the negative control measured alongside it. The control performs
/// the same fetch, from the same fixture, as the same uid, exempting the
/// source the way the script used to; it must FAIL with git's ownership
/// refusal, or this test is vacuous. Then `--check` must succeed.
#[test]
fn a_foreign_owned_forge_repository_is_fetched_not_only_read() {
    let Some(uid) = second_uid() else {
        eprintln!("publish_github_pr_sh: SKIPPED — needs root + setpriv for a second uid");
        return;
    };
    let root = scratch("fetch-foreign-owner");
    let repo = root.join("boss.git");
    git_init_bare(&repo); // root-owned; the check below runs as `uid`

    // CONTROL A — no exemption at all. Establishes that this fixture
    // really is foreign-owned from `uid`'s point of view.
    let (ok, out) = control_fetch(&root, &repo, uid, false);
    assert!(
        !ok,
        "an unexempted fetch from a foreign-owned source SUCCEEDED — the fixture is not foreign-owned, so nothing below is a test: {out}"
    );
    assert!(
        out.to_lowercase().contains("dubious ownership"),
        "the unexempted control failed for some other reason than ownership: {out}"
    );

    // CONTROL B — the script's former shape. `-c safe.directory=<src>`
    // does not reach upload-pack, so this fails identically. This is the
    // bug, reproduced, in the test that guards against it.
    let (ok, out) = control_fetch(&root, &repo, uid, true);
    assert!(
        !ok,
        "`-c safe.directory=<source>` exempted a fetch SOURCE on this git \
         ({}) — the premise of the fix does not hold here and the fix must \
         be re-argued: {out}",
        git_version()
    );
    assert!(
        out.to_lowercase().contains("dubious ownership"),
        "the `-c`-only control failed for some other reason than ownership: {out}"
    );

    // AND THE VERB, through the protected channel, on the same fixture.
    let (ok, out) = check(&root, &[forge_path(&repo)], Some(uid));
    assert!(
        !out.to_lowercase().contains("dubious ownership"),
        "git's ownership refusal reached the verb's output: {out}"
    );
    assert!(
        !out.contains("the FETCH the publish performs fails"),
        "the verb could not fetch from a foreign-owned repository: {out}"
    );
    assert!(
        out.contains("forge fetch:"),
        "the verb did not report a fetch at all: {out}"
    );
    // --check holds no token since backlog d2b7c947 (the App's is minted
    // per request and rendered only by a run), so the fetch is the whole
    // question here, and it must have been answered ok.
    assert!(
        out.contains("forge fetch: ok"),
        "the fetch from a foreign-owned repository must read ok (--check {}): {out}",
        if ok { "passed" } else { "refused" }
    );
}

/// `git --version`, for the one assertion whose premise is version-bound.
fn git_version() -> String {
    Command::new("git")
        .arg("--version")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "git version unknown".into())
}

// ---------------------------------------------------------------------
// THE RUN PATH, offline. Every outside party is a fixture — the jobs API
// a `curl` stub that prints one packet, GitHub's REST API a `gh` stub,
// the forge and the mirror local bare repositories, the credential
// broker's render a stub that fills the token slot — and the slugs are
// fixture names.
//
// FROM THE ORGANISATION, AS THE APP (backlog d2b7c947, David 2026-09-30:
// "Go with option 2, retire the fork"). Until then the run pushed its
// snapshot to a public fork under a personal account and opened the PR
// from there with a personal token, and the cases here pinned the
// fork's relationship to the mirror (defect 4 of 2026-09-11: a namesake
// outside the fork network passed a name check). There is no fork now:
// the branch is pushed to the mirror repository itself and the PR opened
// from it, with the App installation token the broker minted for the
// request the run answers — so the cases pin THAT: the push lands in
// the mirror, the head is a bare branch of it, the token is this
// request's own and live, and it is revoked when the GitHub work ends.
// ---------------------------------------------------------------------

/// One git command in `dir` with `input` on stdin — `hash-object` and
/// `mktree` are the two fixtures below need, and both read stdin.
fn git_stdin(dir: &Path, args: &[&str], input: &str) -> String {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .env("GIT_AUTHOR_NAME", "fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .spawn()
        .expect("git runs");
    // git's exit status, asserted below, is the verdict (backlog fec29a02).
    feed_stdin(&mut child, input.as_bytes());
    let out = child.wait_with_output().expect("git finishes");
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A bare repository whose `main` holds one file with `content`. The
/// forge and the mirror stand-ins must differ in their TREE, not only
/// their history: the verb refuses "nothing to publish" when the two
/// trees match, so two empty-tree fixtures would never reach a push.
fn git_init_bare_holding(path: &Path, content: &str) {
    git_init_bare(path);
    let blob = git_stdin(
        path,
        &["hash-object", "-t", "blob", "-w", "--stdin"],
        content,
    );
    let tree = git_stdin(path, &["mktree"], &format!("100644 blob {blob}\tfile\n"));
    let commit = git_in(path, &["commit-tree", &tree, "-m", "fixture"]);
    git_in(path, &["update-ref", "refs/heads/main", &commit]);
}

/// Real `jq`, not the `exit 0` stand-in `stub_bin` installs for a missing
/// tool: the run path reads the packet and GitHub's answers with it, so
/// a stub would make every case below pass vacuously.
fn have_real_jq() -> bool {
    Command::new("sh")
        .args(["-c", "command -v jq >/dev/null 2>&1"])
        .status()
        .is_ok_and(|s| s.success())
}

const MIRROR_SLUG: &str = "fixture-upstream/mirror";
const PUBLISH_DATE: &str = "2026-01-02";
/// The ops-request the fixture run answers — what the ops runner hands
/// every verb as OPS_REQUEST_ID, and what the broker keys the token on.
const REQUEST: &str = "0000000a-0000-4000-8000-00000000000f";
/// The token the stub render places. Never a real one, and never
/// allowed to reach the run's output.
const FIXTURE_TOKEN: &str = "ghs_fixture-installation-token-9f3a";

/// The mirror's owner, as gh names a head's `headRepositoryOwner`.
fn mirror_owner() -> &'static str {
    MIRROR_SLUG.split('/').next().unwrap()
}

/// This uid, as `stat -c %u` reads it — the owner the fixture's slot must
/// have (root, uid 0, on the forge host; whoever runs the test here).
fn my_uid() -> String {
    let out = Command::new("id").arg("-u").output().expect("id runs");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// One offline run of the verb. Build it, shape what the fixtures answer,
/// then `go()`.
struct Run {
    root: PathBuf,
    forge: PathBuf,
    mirror: PathBuf,
    gh_api: PathBuf,
    stubs: PathBuf,
}

impl Run {
    fn new(case: &str) -> Run {
        let root = scratch(case);
        let forge = root.join("forge.git");
        let mirror = root.join("mirror.git");
        // Distinct trees, or the verb stops at "nothing to publish".
        git_init_bare_holding(&forge, "forge main\n");
        git_init_bare_holding(&mirror, "an older mirror main\n");
        let gh_api = root.join("gh-api");
        std::fs::create_dir_all(&gh_api).unwrap();

        // One open publish-to-github packet whose open-pr step is ready —
        // the shape the verb selects with jq — APPROVED the way registry v6
        // demands: a presence stamp over an approve step naming forge
        // main's head as the measured, scanned tree (backlog 02b65d81).
        let jobs = root.join("jobs.json");
        let source = git_in(&forge, &["rev-parse", "refs/heads/main"]);
        boss_testing::write_file(
            &jobs,
            &serde_json::json!({ "data": [Packet::signed(&source).json()] }).to_string(),
        );

        let stubs = root.join("stubs");
        std::fs::create_dir_all(&stubs).unwrap();
        // `gh`: GitHub's REST answers from fixture files, the PR calls the
        // verb makes, and the two installation-token calls its revoke
        // makes (`api -i`, whose first line is the status GitHub answered).
        boss_testing::write_exec(
            &stubs.join("gh"),
            &format!(
                r#"#!/bin/sh
echo "$*" >> '{log}'
[ -z "$GH_TOKEN" ] || printf '%s\n' "$GH_TOKEN" >> '{tokens}'
if [ "$1" = "api" ]; then
    case "$*" in
        *"--method DELETE installation/token"*)
            if [ -f '{api}/_revoke_refused' ]; then printf 'HTTP/2.0 500 Internal Server Error\n\n{{}}\n'; exit 1; fi
            touch '{api}/_revoked'
            printf 'HTTP/2.0 204 No Content\n\n'
            exit 0 ;;
        *installation/repositories*)
            if [ -f '{api}/_revoked' ]; then
                printf 'HTTP/2.0 401 Unauthorized\n\n{{"message":"Bad credentials"}}\n'
                echo 'gh: Bad credentials (HTTP 401)' >&2
                exit 1
            fi
            printf 'HTTP/2.0 200 OK\n\n{{"total_count":1}}\n'
            exit 0 ;;
    esac
    f='{api}/'"$(echo "${{2#repos/}}" | tr / _)".json
    # GitHub marks a PR merged when its head reaches the base (backlog
    # 602fe95f): with `_ff_marks_merged`, a pulls answer whose head is
    # the mirror's main NOW reads closed and merged, as GitHub's does
    # after a fast-forward push of main.
    if [ -f '{api}/_ff_marks_merged' ] && [ -f "$f" ]; then
        h=$(jq -r '.head.sha // ""' "$f")
        m=$(git --git-dir='{mirror}' rev-parse refs/heads/main 2>/dev/null)
        if [ -n "$h" ] && [ "$h" = "$m" ]; then
            jq '.state = "closed" | .merged = true | .merged_at = "2026-01-02T01:00:00Z" | .closed_at = "2026-01-02T01:00:00Z"' "$f"
            exit 0
        fi
    fi
    if [ -f "$f" ]; then cat "$f"; exit 0; fi
    echo 'gh: Not Found (HTTP 404)' >&2
    exit 1
fi
if [ "$1" = "pr" ] && [ "$2" = "create" ]; then
    if [ -f '{api}/_pr_create_refuses' ]; then cat '{api}/_pr_create_refuses' >&2; exit 1; fi
    # The PR it opens is then on the open listing, as on GitHub — unless
    # a case plants `_pr_create_unlisted` to stand in for a listing that
    # does not show it. A bare head is a branch of the base repository
    # itself; `<owner>:<branch>` a branch of another in the network.
    head=; prev=
    for a in "$@"; do [ "$prev" = "--head" ] && head="$a"; prev="$a"; done
    case "$head" in
        *:*) o="${{head%%:*}}"; b="${{head#*:}}"; r=elsewhere ;;
        *) o='{owner}'; b="$head"; r='{name}' ;;
    esac
    # GitHub's answer for the PR it opens names its opener: the App's bot
    # account, since the PR was opened with the App's token (backlog
    # 16a9c5ae) — unless a case planted its own answer first.
    pulls1='{api}/'"$(echo '{mirror_slug}/pulls/1' | tr / _)".json
    [ -f "$pulls1" ] || printf '%s\n' '{{"number":1,"state":"open","merged":false,"user":{{"login":"boss-publisher[bot]","id":9001,"type":"Bot"}}}}' > "$pulls1"
    if [ ! -f '{api}/_pr_create_unlisted' ]; then
        [ -f '{api}/_open_prs.json' ] || echo '[]' > '{api}/_open_prs.json'
        jq --arg o "$o" --arg b "$b" --arg r "$r" \
            '. + [{{number: 1, url: "https://github.invalid/{mirror_slug}/pull/1", headRefName: $b,
                   headRepositoryOwner: {{login: $o}}, headRepository: {{id: "R_1", name: $r}}}}]' \
            '{api}/_open_prs.json' > '{api}/_open_prs.next' && mv '{api}/_open_prs.next' '{api}/_open_prs.json'
    fi
    echo 'https://github.invalid/{mirror_slug}/pull/1'
    exit 0
fi
# `pr list --head <owner>:<branch>` answers NOTHING, as real gh does:
# its manual says of --head "(\"<owner>:<branch>\" syntax not
# supported)", so the spelling the verb used until backlog 1f0aa60d
# never matched a PR (PR #245, 2026-09-27). Without `--head` it is the
# open-PR listing, which gh answers `[]` when empty.
if [ "$1" = "pr" ] && [ "$2" = "list" ]; then
    case " $* " in *" --head "*) exit 0 ;; esac
    if [ -f '{api}/_open_prs.json' ]; then cat '{api}/_open_prs.json'; else echo '[]'; fi
    exit 0
fi
# `pr close <n>` brings GitHub's answer for pulls/<n> into being — a
# closed PR, unless `on_close` planted a different answer — and takes it
# off the open listing, as GitHub does, unless that answer still reads
# open.
if [ "$1" = "pr" ] && [ "$2" = "close" ]; then
    f='{api}/'"$(echo "{mirror_slug}/pulls/$3" | tr / _)".json
    if [ -f '{api}/_on_close.json' ]; then cp '{api}/_on_close.json' "$f"
    else printf '{{"number":%s,"state":"closed","merged":false,"closed_at":"2026-01-02T00:00:00Z"}}\n' "$3" > "$f"; fi
    if [ -f '{api}/_open_prs.json' ] && [ "$(jq -r '.state // ""' "$f")" = closed ]; then
        jq --argjson n "$3" 'map(select(.number != $n))' '{api}/_open_prs.json' > '{api}/_open_prs.next' \
            && mv '{api}/_open_prs.next' '{api}/_open_prs.json'
    fi
    exit 0
fi
exit 0
"#,
                log = root.join("gh.log").display(),
                tokens = root.join("gh-tokens.log").display(),
                api = gh_api.display(),
                mirror = mirror.display(),
                owner = mirror_owner(),
                name = MIRROR_SLUG.split('/').nth(1).unwrap(),
                mirror_slug = MIRROR_SLUG,
            ),
        );
        // `curl`: the packet on a GET, silence on the step PUT.
        boss_testing::write_exec(
            &stubs.join("curl"),
            &format!(
                r#"#!/bin/sh
echo "$*" >> '{log}'
for a in "$@"; do
    if [ "$a" = "PUT" ]; then exit 0; fi
done
cat '{jobs}'
"#,
                log = root.join("curl.log").display(),
                jobs = jobs.display(),
            ),
        );
        // The broker's render (infra/forge/credential-render.sh), standing
        // in for the k8s Secret it reads: `--request <id>` fills the slot
        // with a token, its expiry an hour out and the request it was
        // rendered for — unless a case planted `_render_*` to stand in for
        // no mint, another request's token or one about to expire. With
        // `--expire` it records that the act marked the Secret's copy
        // expired, and removes the slot, as the real one does.
        boss_testing::write_exec(
            &stubs.join("render"),
            &format!(
                r#"#!/usr/bin/env bash
echo "$*" >> '{log}'
rule= dest= req= expire=
while [ $# -gt 0 ]; do
    case "$1" in
        --rule) rule="$2" ;; --dest) dest="$2" ;; --request) req="$2" ;; --expire) expire="$2" ;;
    esac
    shift 2
done
[ -r "$rule" ] || {{ echo "render stub: no rule at $rule" >&2; exit 78; }}
if [ -n "$expire" ]; then
    printf 'EXPIRED %s %s\n' "$req" "$(cat "$expire")" >> '{log}'
    rm -f "$dest" "$dest.expires-at" "$dest.request"
    exit 0
fi
[ -f '{root}/_render_absent' ] && {{ rm -f "$dest" "$dest.expires-at" "$dest.request"; echo "credential-render: empty"; exit 0; }}
mkdir -p "$(dirname "$dest")"
( umask 077 && printf '%s' '{token}' > "$dest" )
chmod 600 "$dest"
if [ -f '{root}/_render_expiring' ]; then date -u -d '+5 minutes' +%Y-%m-%dT%H:%M:%SZ > "$dest.expires-at"
else date -u -d '+1 hour' +%Y-%m-%dT%H:%M:%SZ > "$dest.expires-at"; fi
if [ -f '{root}/_render_other_request' ]; then printf '%s' 'ffffffff-0000-4000-8000-000000000000' > "$dest.request"
else printf '%s' "$req" > "$dest.request"; fi
echo "credential-render: rendered"
"#,
                log = root.join("render.log").display(),
                root = root.display(),
                token = FIXTURE_TOKEN,
            ),
        );

        Run {
            root,
            forge,
            mirror,
            gh_api,
            stubs,
        }
    }

    /// What GitHub's REST API says about `path` (`repos/` dropped).
    fn gh_repo(&self, path: &str, body: &str) {
        boss_testing::write_file(
            &self.gh_api.join(format!("{}.json", path.replace('/', "_"))),
            body,
        );
    }

    fn go(&self) -> (bool, String) {
        self.go_as("")
    }

    /// The run with the forge push handed to `push_as` through
    /// `runuser` — a stub on the fixture's PATH in the test that uses it.
    fn go_as(&self, push_as: &str) -> (bool, String) {
        self.go_with(&[("BOSS_FORGE_PUSH_AS", push_as.to_string())])
    }

    /// The run with extra environment laid over the fixture's — the
    /// value `UNSET` removes the variable (so the verb takes its
    /// default); an empty string is set empty, as the verb reads it.
    fn go_with(&self, extra: &[(&str, String)]) -> (bool, String) {
        self.go_argv("", extra)
    }

    /// The token slot the fixture's render fills.
    fn slot(&self) -> PathBuf {
        self.root.join("slot/algedonic-dev-publish.token")
    }

    /// `go_with`, with the verb's one argument — empty for a publish
    /// run, `--measure` for the drift refresh (design cb38d806).
    fn go_argv(&self, argv1: &str, extra: &[(&str, String)]) -> (bool, String) {
        let (code, text) = self.go_argv_code(argv1, extra);
        (code == 0, text)
    }

    /// `go_argv`, answering the exit code itself: --merge's code says
    /// whether main moved (backlog 16a9c5ae), which a bool cannot.
    fn go_argv_code(&self, argv1: &str, extra: &[(&str, String)]) -> (i32, String) {
        let outer = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into());
        // Ours first: `stub_bin` stands in for tools this box LACKS, and
        // gh/curl must be ours even where the box has them.
        let path = format!(
            "{}:{}:{outer}",
            self.stubs.display(),
            stub_bin(&self.root).display()
        );
        let mut cmd = Command::new("bash");
        cmd.arg(script()).env_clear().env("PATH", path);
        if !argv1.is_empty() {
            cmd.arg(argv1);
        }
        for (k, v) in base_env(&self.root) {
            cmd.env(k, v);
        }
        // A CLEAN secrets scan by default: the run path scans the approved
        // commit itself (02b65d81 review, H1) and the fixture's one-file
        // trees carry no lint. Its own file, so a case's `planted_scan`
        // (scan.sh), passed as an extra, is never overwritten by it.
        let clean_scan = self.root.join("default-clean-scan.sh");
        boss_testing::write_exec(
            &clean_scan,
            "#!/bin/sh\necho 'no-secrets: scanned (fixture default)'\nexit 0\n",
        );
        // The runner's RuntimeDirectory: a private directory, as systemd
        // makes it (0700).
        let runtime = self.root.join("run");
        std::fs::create_dir_all(&runtime).unwrap();
        // mode-bits-ok: a directory, the runner's private tmpfs stand-in
        std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
        cmd.env("BOSS_SECRETS_SCAN", clean_scan.display().to_string());
        cmd.env("BOSS_JOBS_URL", "http://jobs.invalid")
            .env("BOSS_FORGE_REPO_PATH", self.forge.display().to_string())
            .env("BOSS_MIRROR_SLUG", MIRROR_SLUG)
            .env("BOSS_MIRROR_URL", self.mirror.display().to_string())
            .env("BOSS_PUBLISH_DATE", PUBLISH_DATE)
            .env("OPS_REQUEST_ID", REQUEST)
            .env("RUNTIME_DIRECTORY", runtime.display().to_string())
            .env(
                "BOSS_PUBLISH_RENDER",
                self.stubs.join("render").display().to_string(),
            )
            .env("BOSS_PUBLISH_TOKEN_FILE", self.slot().display().to_string())
            .env("BOSS_PUBLISH_TOKEN_OWNER_UID", my_uid())
            .env("BOSS_PUBLISH_RENDER_SLEEP", "0")
            // The forge push, as the test's own uid into the fixture by
            // path: in production it is `runuser -l david` over Forgejo's
            // HTTP, which no test box can stand in for.
            .env("BOSS_FORGE_PUSH_URL", self.forge.display().to_string())
            .env("BOSS_FORGE_PUSH_AS", "");
        for (k, v) in extra {
            if v == "UNSET" {
                cmd.env_remove(k);
            } else {
                cmd.env(k, v);
            }
        }
        let out = cmd.output().expect("the verb runs");
        let mut text = String::from_utf8_lossy(&out.stdout).to_string();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        (out.status.code().unwrap_or(-1), text)
    }

    /// Did the snapshot reach the repository standing in for the mirror?
    /// This is the assertion that matters: a refusal that still pushed is
    /// not a refusal.
    fn pushed(&self) -> bool {
        self.mirror_branch().is_some()
    }

    /// The run's own branch on the mirror and the commit it holds. Every
    /// publish names its branch `publish/<date>-<snapshot>` (backlog
    /// 1f0aa60d), so the fixture finds it by its dated prefix rather
    /// than spelling a name it cannot know before the run.
    fn mirror_branch(&self) -> Option<(String, String)> {
        published_branch(&self.mirror)
    }

    /// Did the snapshot ALSO reach the forge under the same branch name?
    /// The forge holds every snapshot the verb publishes (ce5339d6: PR
    /// #238 closed 2 min after opening when a push mirror pruned a head
    /// the forge lacked).
    fn forge_has_branch(&self) -> Option<String> {
        published_branch(&self.forge).map(|(_, sha)| sha)
    }

    fn gh_log(&self) -> String {
        std::fs::read_to_string(self.root.join("gh.log")).unwrap_or_default()
    }

    fn curl_log(&self) -> String {
        std::fs::read_to_string(self.root.join("curl.log")).unwrap_or_default()
    }

    fn render_log(&self) -> String {
        std::fs::read_to_string(self.root.join("render.log")).unwrap_or_default()
    }

    /// Every token gh was handed through GH_TOKEN, one per call.
    fn gh_tokens(&self) -> Vec<String> {
        std::fs::read_to_string(self.root.join("gh-tokens.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// Plant a `_render_*` marker the stub render reads.
    fn render_plants(&self, marker: &str) {
        boss_testing::write_file(&self.root.join(marker), "");
    }
}

/// The one `publish/<PUBLISH_DATE>-<snapshot>` branch in `repo`, with
/// the commit it holds — `None` when there is none. More than one is a
/// finding in itself: a run publishes exactly one branch.
fn published_branch(repo: &Path) -> Option<(String, String)> {
    let listed = git_in(
        repo,
        &[
            "for-each-ref",
            "--format=%(refname:short) %(objectname)",
            &format!("refs/heads/publish/{PUBLISH_DATE}-*"),
        ],
    );
    let rows: Vec<(String, String)> = listed
        .lines()
        .filter_map(|l| l.split_once(' '))
        .map(|(n, s)| (n.to_string(), s.to_string()))
        .collect();
    assert!(
        rows.len() <= 1,
        "one run publishes one branch, found {rows:?} in {}",
        repo.display()
    );
    rows.into_iter().next()
}

/// THE POSITIVE CASE (backlog d2b7c947). The snapshot lands on a
/// publish/* branch of the MIRROR ITSELF, the PR is opened from that
/// bare branch — head and base one repository — as the token the render
/// placed for this request, and open-pr is completed. No fork is read,
/// made or pushed to.
#[test]
fn a_publish_opens_its_pr_from_a_branch_of_the_mirror_itself() {
    if !have_real_jq() {
        eprintln!("publish_github_pr_sh: SKIPPED — the run path needs a real jq");
        return;
    }
    let run = Run::new("mirror-branch");
    let (ok, out) = run.go();
    assert!(ok, "the verb refused a signed publish: {out}");
    let (name, _) = run
        .mirror_branch()
        .unwrap_or_else(|| panic!("nothing was pushed to the mirror: {out}"));
    let gh = run.gh_log();
    assert!(
        gh.lines().any(|l| l.starts_with("pr create")
            && l.contains(&format!("--repo {MIRROR_SLUG}"))
            && l.contains(&format!("--head {name} "))),
        "the PR must be opened from the bare branch {name} of {MIRROR_SLUG}: {gh}"
    );
    assert!(
        !gh.contains("repo fork") && !gh.contains("repo view"),
        "the verb asked GitHub about a fork: {gh}"
    );
    assert!(
        out.contains("https://github.invalid"),
        "no PR was opened: {out}"
    );
    assert!(
        run.curl_log().contains("PUT"),
        "open-pr was never completed on the packet: {}",
        run.curl_log()
    );
    // Every GitHub call carried the token the render placed for THIS
    // request, and nothing printed it.
    let tokens = run.gh_tokens();
    assert!(
        !tokens.is_empty() && tokens.iter().all(|t| t == FIXTURE_TOKEN),
        "gh must be handed the request's rendered token and nothing else: {tokens:?}"
    );
    assert!(
        !out.contains(FIXTURE_TOKEN),
        "the token reached the run's output: {out}"
    );
    assert!(
        run.render_log()
            .lines()
            .any(|l| l.contains(&format!("--request {REQUEST}"))
                && l.contains("broker-mints-the-algedonic-dev-publish-token-when-a-publish-request-is-filed.toml")),
        "the slot is rendered from the publish mint rule, for the request the run answers: {}",
        run.render_log()
    );
    // With no older publish PR open, the supersession sweep asks and
    // closes nothing. The sweep's listing is the one WITHOUT `--head`
    // (that one is the reuse question).
    assert!(
        gh.lines()
            .any(|l| l.starts_with("pr list") && !l.contains("--head")),
        "the verb never listed the mirror's open PRs: {gh}"
    );
    assert!(!gh.contains("pr close"), "nothing here is older: {gh}");
    let done = std::fs::read_to_string(run.root.join("curl.log")).unwrap_or_default();
    assert!(
        done.contains("/steps/"),
        "open-pr's keys went through the step merge door: {done}"
    );
}

/// THE PUBLISHER (backlog d2b7c947, David 2026-09-30: "We should also use
/// david@algedonic.dev as the publisher instead of dauld"). The snapshot
/// is authored AND committed as the `publisher` the verb file declares —
/// read from that one file, never retyped — and the PR body names it. A
/// personal handle or a GitHub no-reply address on the public mirror's
/// history is what this replaced.
#[test]
fn the_snapshot_is_authored_and_committed_as_the_declared_publisher() {
    if !have_real_jq() {
        eprintln!("publish_github_pr_sh: SKIPPED — the run path needs a real jq");
        return;
    }
    let verb: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(repo_root().join("infra/ops/verbs/publish-github-pr.json"))
            .expect("the verb file"),
    )
    .expect("the verb file is JSON");
    let name = verb["publisher"]["name"]
        .as_str()
        .expect("the verb file declares publisher.name");
    let email = verb["publisher"]["email"]
        .as_str()
        .expect("the verb file declares publisher.email");
    assert!(
        email.ends_with("@algedonic.dev") && !email.contains("noreply"),
        "the publisher is a company address, never a personal or no-reply one: {email}"
    );

    let run = Run::new("publisher");
    run.echoing_curl();
    let (ok, out) = run.go();
    assert!(ok, "{out}");
    let (_, sha) = run.mirror_branch().expect("the run published");
    let who = git_in(
        &run.mirror,
        &["log", "-1", "--format=%an <%ae>|%cn <%ce>", &sha],
    );
    let want = format!("{name} <{email}>");
    assert_eq!(
        who,
        format!("{want}|{want}"),
        "author and committer must both be the declared publisher"
    );
    let gh = run.gh_log();
    let create = gh
        .lines()
        .find(|l| l.starts_with("pr create"))
        .unwrap_or_else(|| panic!("no PR was opened: {gh}"));
    assert!(
        create.contains(&format!("Published by {want}")),
        "the PR body names the publisher: {create}"
    );
    let puts = run.puts();
    let done = puts
        .iter()
        .find(|p| p["status"] == "completed")
        .unwrap_or_else(|| panic!("open-pr was never completed: {puts:?}"));
    assert_eq!(done["metadata"]["publisher"], want.as_str(), "{done}");
}

/// A verb file with no company publisher is refused by `--check` and by a
/// run, before anything is fetched: the identity on the public history is
/// the declaration's, or there is none.
#[test]
fn a_verb_file_without_a_company_publisher_is_refused() {
    let root = scratch("no-publisher");
    let repo = root.join("boss.git");
    git_init_bare(&repo);
    // The verb reads its file beside itself, so the case runs a COPY of
    // the script inside a scratch tree whose verb file it edits.
    let tree = root.join("tree");
    for rel in [
        "infra/forge/publish-github-pr.sh",
        "infra/forge/forge-repo-path.sh",
        "infra/forge/publish-pr-state.sh",
        "infra/forge/credential-render.sh",
        "infra/lib/jq.sh",
        "infra/lib/step-shape.sh",
        "infra/lib/secret-header.sh",
        "infra/lib/sor.sh",
        "infra/lib/sor-reader.sh",
        "infra/dispatcher/rules/broker-mints-the-algedonic-dev-publish-token-when-a-publish-request-is-filed.toml",
    ] {
        let to = tree.join(rel);
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::copy(repo_root().join(rel), &to).unwrap_or_else(|e| panic!("copy {rel}: {e}"));
    }
    let verb = tree.join("infra/ops/verbs/publish-github-pr.json");
    std::fs::create_dir_all(verb.parent().unwrap()).unwrap();
    for (case, publisher) in [
        ("absent", serde_json::Value::Null),
        (
            "a personal no-reply address",
            serde_json::json!({"name": "someone", "email": "someone@users.noreply.github.com"}),
        ),
    ] {
        let mut v: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(repo_root().join("infra/ops/verbs/publish-github-pr.json"))
                .unwrap(),
        )
        .unwrap();
        v["publisher"] = publisher;
        boss_testing::write_file(&verb, &v.to_string());
        let path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into());
        let mut cmd = Command::new("bash");
        cmd.arg(tree.join("infra/forge/publish-github-pr.sh"))
            .arg("--check")
            .env_clear()
            .env("PATH", format!("{}:{path}", stub_bin(&root).display()))
            .env("BOSS_FORGE_REPO_PATH", repo.display().to_string());
        for (k, v) in base_env(&root) {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("the verb runs");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !out.status.success() && text.contains("publisher"),
            "{case}: --check must refuse a verb file with no company publisher: {text}"
        );
    }
}

/// THE TOKEN IS THIS REQUEST'S, LIVE, AND ROOT'S — or nothing leaves.
/// Each case changes one thing about the slot the render leaves (or the
/// request the run can name), and the run must refuse naming it, with
/// nothing on the forge, nothing on the mirror, no PR and no completion.
fn token_refused(case: &str, plant: &str, extra: &[(&str, String)], names: &str) {
    if !have_real_jq() {
        eprintln!("publish_github_pr_sh: SKIPPED — the run path needs a real jq");
        return;
    }
    let run = Run::new(case);
    if !plant.is_empty() {
        run.render_plants(plant);
    }
    let (ok, out) = run.go_with(extra);
    assert!(!ok, "{case}: the verb published without its token: {out}");
    assert!(
        out.contains("REFUSED") && out.contains(names),
        "{case}: the refusal must say `{names}`: {out}"
    );
    run.assert_nothing_published(&out);
    assert!(
        !out.contains(FIXTURE_TOKEN),
        "{case}: the token reached the output: {out}"
    );
}

#[test]
fn a_run_that_cannot_name_its_request_publishes_nothing() {
    token_refused(
        "token-no-request",
        "",
        &[("OPS_REQUEST_ID", "UNSET".into())],
        "OPS_REQUEST_ID",
    );
}

#[test]
fn a_request_the_broker_minted_nothing_for_publishes_nothing() {
    token_refused(
        "token-absent",
        "_render_absent",
        &[],
        "no installation token for request",
    );
}

#[test]
fn another_requests_token_publishes_nothing() {
    token_refused(
        "token-other-request",
        "_render_other_request",
        &[],
        "another request's token is never used",
    );
}

#[test]
fn a_token_about_to_expire_publishes_nothing() {
    token_refused(
        "token-expiring",
        "_render_expiring",
        &[],
        "too close to outlive this verb",
    );
}

#[test]
fn a_slot_owned_by_another_uid_publishes_nothing() {
    token_refused(
        "token-foreign-owner",
        "",
        &[("BOSS_PUBLISH_TOKEN_OWNER_UID", "4242".into())],
        "must be owned by uid 4242",
    );
}

#[test]
fn a_run_with_no_private_runtime_directory_publishes_nothing() {
    token_refused(
        "token-no-runtime-dir",
        "",
        &[("RUNTIME_DIRECTORY", "UNSET".into())],
        "RUNTIME_DIRECTORY",
    );
}

/// THE TOKEN ENDS WITH THE GITHUB WORK (design 76c46869's posture for a
/// per-act token). After the PR is open, the older ones closed and the
/// branches pruned, the run marks the Secret's copy expired through the
/// render, removes the slot, asks GitHub to end the token and proves it
/// with a read that answers 401 — BEFORE open-pr completes, so the
/// completion carries how it went. And a revoke that cannot be proven
/// does not fail the publish, but it is said, on the record.
#[test]
fn the_token_is_revoked_before_open_pr_completes_and_the_record_says_so() {
    if !have_real_jq() {
        eprintln!("publish_github_pr_sh: SKIPPED — the run path needs a real jq");
        return;
    }
    let run = Run::new("token-revoked");
    run.echoing_curl();
    let (ok, out) = run.go();
    assert!(ok, "{out}");
    let render = run.render_log();
    assert!(
        render.contains(&format!("EXPIRED {REQUEST} {FIXTURE_TOKEN}")),
        "the Secret's copy of the token the run used must be marked expired: {render}"
    );
    assert!(!run.slot().exists(), "the slot must be removed");
    let gh = run.gh_log();
    let delete = gh
        .find("--method DELETE installation/token")
        .unwrap_or_else(|| panic!("the token was never presented for revocation: {gh}"));
    let proof = gh
        .rfind("installation/repositories")
        .unwrap_or_else(|| panic!("the revoke was never read back: {gh}"));
    assert!(
        delete < proof,
        "the read proving the revoke follows it: {gh}"
    );
    let puts = run.puts();
    let done = puts
        .iter()
        .find(|p| p["status"] == "completed")
        .unwrap_or_else(|| panic!("open-pr was never completed: {puts:?}"));
    let revoked = done["metadata"]["token_revoked"]
        .as_str()
        .unwrap_or_default();
    assert!(
        revoked.starts_with("revoked") && revoked.contains("401"),
        "open-pr records the proven revoke: {done}"
    );

    // A revoke GitHub does not take: the publish stands, the record says so.
    let run = Run::new("token-revoke-refused");
    run.echoing_curl();
    boss_testing::write_file(&run.gh_api.join("_revoke_refused"), "");
    let (ok, out) = run.go();
    assert!(
        ok,
        "an unproven revoke must not fail an open, recorded PR: {out}"
    );
    assert!(out.contains("WARNING"), "{out}");
    let puts = run.puts();
    let done = puts
        .iter()
        .find(|p| p["status"] == "completed")
        .unwrap_or_else(|| panic!("open-pr was never completed: {puts:?}"));
    let revoked = done["metadata"]["token_revoked"]
        .as_str()
        .unwrap_or_default();
    assert!(
        revoked.contains("NOT proven revoked"),
        "open-pr records what stands: {done}"
    );
}

/// WHO OPENED THE PR, recorded (backlog 16a9c5ae, review 01561b13 N4):
/// open-pr carries the opener GitHub names when asked with the run's own
/// token — the App's bot account, the one identity --merge holds the PR
/// to. A GitHub that names no Bot records none, and the publish stands:
/// the merge fails closed on the missing record, never the publish.
#[test]
fn open_pr_records_the_app_that_opened_the_pr_as_github_names_it() {
    if !have_real_jq() {
        eprintln!("publish_github_pr_sh: SKIPPED — the run path needs a real jq");
        return;
    }
    let run = Run::new("opened-by");
    run.echoing_curl();
    let (ok, out) = run.go();
    assert!(ok, "{out}");
    let puts = run.puts();
    let done = puts
        .iter()
        .find(|p| p["status"] == "completed")
        .unwrap_or_else(|| panic!("open-pr was never completed: {puts:?}"));
    assert_eq!(done["metadata"]["opened_by"], APP_BOT, "{done}");
    assert_eq!(done["metadata"]["opened_by_id"], APP_BOT_ID, "{done}");
    assert!(
        run.gh_log()
            .contains(&format!("api repos/{MIRROR_SLUG}/pulls/1")),
        "the opener was not read from GitHub: {}",
        run.gh_log()
    );

    // A person's answer: nothing recorded, the publish still completes.
    let run = Run::new("opened-by-a-person");
    run.echoing_curl();
    run.gh_repo(
        &format!("{MIRROR_SLUG}/pulls/1"),
        r#"{"number":1,"state":"open","user":{"login":"someone","id":7,"type":"User"}}"#,
    );
    let (ok, out) = run.go();
    assert!(ok, "an unreadable opener must not fail the publish: {out}");
    assert!(
        out.contains("WARNING") && out.contains("will refuse"),
        "{out}"
    );
    let puts = run.puts();
    let done = puts
        .iter()
        .find(|p| p["status"] == "completed")
        .unwrap_or_else(|| panic!("open-pr was never completed: {puts:?}"));
    assert!(done["metadata"]["opened_by"].is_null(), "{done}");
}

/// A run that ends EARLY, after its token was placed, still revokes it on
/// the way out (the EXIT trap): a PR create GitHub refuses is a FAILED
/// run, and the token it held is presented for revocation all the same.
#[test]
fn a_run_that_fails_after_its_token_was_placed_still_revokes_it() {
    if !have_real_jq() {
        eprintln!("publish_github_pr_sh: SKIPPED — the run path needs a real jq");
        return;
    }
    let run = Run::new("token-revoked-on-failure");
    run.echoing_curl();
    boss_testing::write_file(
        &run.gh_api.join("_pr_create_refuses"),
        "GraphQL: something GitHub refused (createPullRequest)\n",
    );
    let (ok, out) = run.go();
    assert!(!ok, "{out}");
    assert!(
        run.render_log().contains(&format!("EXPIRED {REQUEST}")),
        "the early end must mark the token expired: {}",
        run.render_log()
    );
    assert!(
        run.gh_log().contains("--method DELETE installation/token"),
        "the early end must present the token for revocation: {}",
        run.gh_log()
    );
    assert!(!run.slot().exists(), "the slot must be removed");
}

/// THE BRANCH LIVES ON THE FORGE TOO (ce5339d6). PR #238 opened at
/// 22:46:59Z on 2026-09-11 and was closed at 22:49:17Z with its head
/// deleted: a push mirror of the forge pruned what the forge lacked.
/// `publish/2026-09-08` survived exactly because it also existed on the
/// forge. So a run pushes the snapshot to the forge under the same name,
/// BEFORE GitHub: if the forge push fails, nothing has been opened.
#[test]
fn the_snapshot_is_pushed_to_the_forge_before_the_mirror() {
    if !have_real_jq() {
        eprintln!("publish_github_pr_sh: SKIPPED — the run path needs a real jq");
        return;
    }
    let run = Run::new("forge-carries-the-branch");
    let (ok, out) = run.go();
    assert!(ok, "{out}");
    let on_forge = run
        .forge_has_branch()
        .expect("publish/<date>-<snapshot> must exist on the forge after a run");
    let (_, on_mirror) = run.mirror_branch().expect("and on the mirror");
    assert_eq!(
        on_forge, on_mirror,
        "the forge and the mirror must hold the SAME snapshot commit"
    );
    assert!(
        out.contains("pushed publish/") && out.contains("to the forge"),
        "the run must say it pushed to the forge: {out}"
    );
    let forge_line = out.find("to the forge").expect("forge push line");
    let mirror_line = out
        .find(&format!("pushed {}:publish/", mirror_owner()))
        .expect("mirror push line");
    assert!(
        forge_line < mirror_line,
        "the forge push comes BEFORE the mirror push, so a failed forge push opens nothing"
    );
}

/// The production forge push runs as ANOTHER user (`runuser -l david
/// -c "git -C <clone> push …"`) over a clone root owns, and git ≥ 2.35.2
/// refuses that as "dubious ownership" unless the pushing user's own
/// config exempts it. The script's exemption is root's GIT_CONFIG_GLOBAL
/// file in a 0700 workdir, which `runuser -l` neither carries nor could
/// read — measured 2026-09-18 23:05Z on ops-request c98a782f, the first
/// approved publish: `fatal: detected dubious ownership in repository at
/// '/var/lib/boss-publish/boss.git'`, five hours unread. The command the
/// other user runs must carry the exemption ITSELF (`-c safe.directory=
/// <clone>`); a stub runuser records what it was handed and runs it as
/// this uid, so the production shape is pinned without a second account.
#[test]
fn the_forge_push_as_another_user_carries_its_own_safe_directory() {
    if !have_real_jq() {
        eprintln!("publish_github_pr_sh: SKIPPED — the run path needs a real jq");
        return;
    }
    let run = Run::new("forge-push-as-user");
    let log = run.root.join("runuser.log");
    boss_testing::write_exec(
        &run.stubs.join("runuser"),
        &format!(
            "#!/usr/bin/env bash\n# stub: runuser -l <user> -c <cmd> — record the command, run it here\nprintf '%s\\n' \"$4\" >> '{}'\nexec bash -c \"$4\"\n",
            log.display()
        ),
    );
    let (ok, out) = run.go_as("someone");
    assert!(ok, "{out}");
    let handed = std::fs::read_to_string(&log).expect("the stub runuser recorded the push command");
    let clone = run.root.join("state/boss.git");
    assert!(
        handed.contains(&format!("-c 'safe.directory={}'", clone.display())),
        "the command handed to the other user must carry the exemption for the clone it pushes from:\n{handed}"
    );
    assert!(
        handed.contains("push"),
        "the recorded command is the forge push:\n{handed}"
    );
    assert!(
        out.contains("to the forge as someone"),
        "the run says whom it pushed as: {out}"
    );
}

/// The forge push goes where the converge fetches from: the checkout's
/// `forgejo` remote, as it stands. Since design 1c90d183 (backlog
/// c4cbc6b5) forge-converge's deposit strips that remote's userinfo once
/// the owner's credential helper authenticates, so on a converted host
/// the URL carries nothing and the owner's helper answers (the helper
/// path is measured against a real 401-ing forge in
/// credential_deposit_sh.rs). THIS case is a host the deposit has not
/// converted, where the remote still carries the token: it is pushed to
/// as it stands and never reaches a message. Measured
/// 2026-09-19 04:55Z on ops-request 3d9d5f58, the second approved
/// publish: with the URL built from sor.env the push as david died on
/// `could not read Username for 'http://10.20.0.15:3000'` — no helper,
/// no userinfo. With no BOSS_FORGE_PUSH_URL the verb reads the checkout's
/// remote (as its owner) and pushes there; the userinfo never reaches a
/// message (the FAILED line and the say line are redacted).
#[test]
fn the_forge_push_url_is_the_checkouts_own_credentialed_remote_and_is_redacted() {
    if !have_real_jq() {
        eprintln!("publish_github_pr_sh: SKIPPED — the run path needs a real jq");
        return;
    }
    let run = Run::new("forge-push-url-from-checkout");
    // A checkout whose forgejo remote carries a credential in its URL
    // — the shape forge-converge.sh fetches through. The push target is
    // the fixture forge, reached by a file URL; the userinfo is the
    // thing under test, so it rides a URL git will accept without using
    // it (a file:// URL ignores userinfo).
    let checkout = run.root.join("checkout");
    git_in(&run.root, &["init", "-q", "checkout"]);
    let secret_url = format!("file://david:s3cr3t-token@{}", run.forge.display());
    git_in(&checkout, &["remote", "add", "forgejo", &secret_url]);
    let log = run.root.join("runuser.log");
    boss_testing::write_exec(
        &run.stubs.join("runuser"),
        &format!(
            "#!/usr/bin/env bash\nprintf '%s\\n' \"$4\" >> '{}'\nexec bash -c \"$4\"\n",
            log.display()
        ),
    );
    let (ok, out) = run.go_with(&[
        ("BOSS_FORGE_PUSH_URL", "UNSET".into()),
        ("BOSS_FORGE_CHECKOUT", checkout.display().to_string()),
        ("BOSS_FORGE_PUSH_AS", "someone".into()),
    ]);
    assert!(ok, "{out}");
    let handed = std::fs::read_to_string(&log).expect("the stub runuser recorded the push");
    assert!(
        handed.contains(&secret_url),
        "the push goes to the checkout's own remote URL, credential and all:\n{handed}"
    );
    assert!(
        !out.contains("s3cr3t-token"),
        "the credential must never reach a message:\n{out}"
    );
    assert!(
        out.contains("<redacted>@"),
        "the forge URL in the say line is redacted, not omitted: {out}"
    );
}

/// `--check` holds no token and names no fork: it says where the branch
/// goes (the mirror), which rule mints the token and where the run will
/// render it, and who the publisher is — read off a real run with the
/// defaults, so the defaults are what is pinned.
#[test]
fn the_check_names_the_mirror_the_mint_rule_and_the_publisher_and_no_fork() {
    let root = scratch("check-names");
    let repo = root.join("boss.git");
    git_init_bare(&repo);
    let (ok, out) = check(&root, &[forge_path(&repo)], None);
    assert!(ok, "--check refused a complete input set: {out}");
    assert!(
        out.contains(
            "broker-mints-the-algedonic-dev-publish-token-when-a-publish-request-is-filed.toml"
        ) && out.contains("/etc/boss-publish/github-app/algedonic-dev-publish.token"),
        "--check names the mint rule and the slot the run renders into: {out}"
    );
    assert!(
        out.contains("David Auld <david@algedonic.dev>"),
        "--check names the declared publisher: {out}"
    );
    assert!(
        !out.to_lowercase().contains("fork") && !out.contains("github.token"),
        "--check still names a fork or the personal token file: {out}"
    );
}

// ---------------------------------------------------------------------
// `--measure` — the drift re-measurement onto a HELD publish packet
// (design cb38d806, backlog e1b6ddf7).
//
// A publish packet held at its approve sign-off used to cost the
// cadence every day behind it: `publish-to-github-daily` fires only on
// `NOT open_publish_exists("github-mirror")`, so the PII hold of
// 2026-09-17 -> 09-18 skipped a week and #239 arrived as a 396-commit
// / 1319-file snapshot no reader — and no CodeQL run — can read as a
// change. `--measure` is the other half of the answer: the same two
// fetches a publish makes, no token, no push, no PR, and the numbers
// land on the open packet with the instant they were taken, so the
// packet David signs carries today's measurement.
// ---------------------------------------------------------------------

impl Run {
    /// Replace the fixture's curl with one that SAVES a PATCH body,
    /// answers the PATCH the way the real door does — **204, no body at
    /// all** — and serves the merged packet on the GET that follows.
    ///
    /// IT USED TO ECHO THE PATCH BACK under `data.metadata`, described
    /// in its own words as "the way the jobs API answers a metadata
    /// merge". The jobs API does no such thing:
    /// `patch_job_metadata` ends `StatusCode::NO_CONTENT.into_response()`
    /// on every path, and a live PATCH against the system of record
    /// answers 204 with an empty body. The fixture invented a response
    /// the door has never sent, and the verb's read-back — which was
    /// reading that response — passed here for its whole life while
    /// reporting FAILED on every real run (backlog b88a13d5).
    ///
    /// That is the inversion worth naming: the comment cited "an API's
    /// answer is not an API's effect" while doing the one thing that
    /// rule forbids, reading the write call's own reply as the effect.
    /// A fixture more generous than the door is not a test.
    fn echoing_curl(&self) {
        self.curl_answering_readback(true)
    }

    /// `echoing_curl`, with a dial on whether the GET that follows the
    /// PATCH carries the merge. `false` stands in for a write that
    /// landed nowhere — the case the read-back exists to catch, and
    /// which nothing could exercise while the stub echoed.
    fn curl_answering_readback(&self, merged: bool) {
        boss_testing::write_exec(
            &self.stubs.join("curl"),
            &format!(
                r#"#!/bin/sh
echo "$*" >> '{log}'
payload=
for a in "$@"; do
    case "$a" in
        @*) payload="${{a#@}}" ;;
    esac
done
for a in "$@"; do
    # The step merge door (backlog e39a9d2a, Stage 2): logged beside the
    # step PUTs, in order, so `puts()` can read a completion as the pair
    # it is — and kept out of the packet's own metadata PATCHes.
    if [ "$a" = "PATCH" ]; then
        case "$*" in
            */steps/*/metadata*)
                {{ printf 'MERGE '; tr -d '\n' < "$payload"; echo; }} >> '{puts}'
                exit 0 ;;
        esac
        cp "$payload" '{patch}'
        {{ tr -d '\n' < "$payload"; echo; }} >> '{patches}'
        # 204 No Content: the real door returns NO body. Anything the
        # verb wants to know about its write, it must go and read.
        if [ '{merged}' = true ]; then
            jq -s '(.[0].data[0]) as $j | .[1] as $p
                   | $j | .metadata = (($j.metadata // {{}}) * $p)'                '{jobs}' "$payload" > '{after}'
        fi
        exit 0
    fi
    if [ "$a" = "PUT" ]; then
        {{ printf 'PUT '; tr -d '\n' < "$payload"; echo; }} >> '{puts}'
        exit 0
    fi
done
# `/api/jobs/<id>` is one packet; `/api/jobs?...` is a listing. The
# read-back asks the first question and must not be handed the second.
# `.../pulls/<n>` is GitHub's pulls API: the fixture's answer, or the
# 404 curl -f turns into exit 22.
for a in "$@"; do
    case "$a" in
        # The rules on a branch and a commit's check-runs (backlog
        # 602fe95f): the fixture's answer, or the 404 curl -f exits 22 on.
        */rules/branches/*)
            if [ -f '{pulls}/_rules.json' ]; then cat '{pulls}/_rules.json'; exit 0; fi
            echo 'curl: (22) The requested URL returned error: 404' >&2
            exit 22 ;;
        */check-runs*)
            if [ -f '{pulls}/_check_runs.json' ]; then cat '{pulls}/_check_runs.json'; exit 0; fi
            echo 'curl: (22) The requested URL returned error: 404' >&2
            exit 22 ;;
        */pulls/*)
            if [ -f '{pulls}/'"${{a##*/}}"'.json' ]; then cat '{pulls}/'"${{a##*/}}"'.json'; exit 0; fi
            echo 'curl: (22) The requested URL returned error: 404' >&2
            exit 22 ;;
        *api/jobs/*\?*) ;;
        # The ops-request this run answers (backlog 16a9c5ae): --merge
        # reads which publish it was filed for off it.
        *api/jobs/{request})
            if [ -f '{request_json}' ]; then cat '{request_json}'; exit 0; fi
            echo 'curl: (22) The requested URL returned error: 404' >&2
            exit 22 ;;
        *api/jobs/*)
            if [ -f '{after}' ]; then cat '{after}'; else jq '.data[0]' '{jobs}'; fi
            exit 0 ;;
    esac
done
cat '{jobs}'
"#,
                log = self.root.join("curl.log").display(),
                patch = self.root.join("patch.json").display(),
                patches = self.root.join("patches.jsonl").display(),
                puts = self.root.join("puts.jsonl").display(),
                pulls = self.gh_api.display(),
                jobs = self.root.join("jobs.json").display(),
                after = self.root.join("jobs-after.json").display(),
                request = REQUEST,
                request_json = self.root.join("request.json").display(),
                merged = merged,
            ),
        );
    }

    /// A secrets scan the measure run calls instead of the published
    /// tree's own `infra/lint/no-secrets.sh` — the fixture's tree is
    /// one file and holds no lint. `verdict` is what it answers.
    fn planted_scan(&self, verdict: i32) -> String {
        let scan = self.root.join("scan.sh");
        boss_testing::write_exec(
            &scan,
            &format!("#!/bin/sh\necho 'no-secrets: scanned'\nexit {verdict}\n"),
        );
        scan.display().to_string()
    }

    fn measure(&self, extra: &[(&str, String)]) -> (bool, String) {
        self.echoing_curl();
        self.go_argv("--measure", extra)
    }

    /// What GitHub's pulls API answers for pull request `n`.
    fn github_pull(&self, n: u32, body: &str) {
        boss_testing::write_file(&self.gh_api.join(format!("{n}.json")), body);
    }

    /// Every PATCH body the run sent, in order.
    fn patches(&self) -> Vec<serde_json::Value> {
        std::fs::read_to_string(self.root.join("patches.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).expect("a PATCH body is JSON"))
            .collect()
    }

    /// The PATCH body the run sent, as JSON.
    fn patched(&self) -> serde_json::Value {
        let body = std::fs::read_to_string(self.root.join("patch.json"))
            .expect("the run wrote no PATCH body");
        serde_json::from_str(&body).expect("the PATCH body is JSON")
    }
}

#[test]
fn a_measure_run_records_todays_drift_on_the_open_packet() {
    if !have_real_jq() {
        eprintln!("publish_github_pr_sh: SKIPPED — the measure path needs a real jq");
        return;
    }
    let run = Run::new("measure-refresh");
    let scan = run.planted_scan(0);
    let (ok, out) = run.measure(&[("BOSS_SECRETS_SCAN", scan)]);
    assert!(ok, "--measure refused a complete input set: {out}");
    // The numbers, from the fixture's own two trees: one commit ahead,
    // one file differing, no file that never existed on the mirror.
    let body = run.patched();
    let d = &body["drift_refresh"];
    assert_eq!(d["commits_ahead"], "1", "commits_ahead: {body}");
    assert_eq!(d["files_changed"], "1", "files_changed: {body}");
    assert_eq!(d["newly_public"], "0", "newly_public: {body}");
    assert_eq!(d["secrets_scan"], "clean", "secrets_scan: {body}");
    assert_eq!(d["has_drift"], "true", "has_drift: {body}");
    // The refresh time is the point: a held packet whose measurement
    // carries no instant is last week's number wearing today's date.
    assert!(
        body["drift_refreshed_at"]
            .as_str()
            .is_some_and(|s| s.len() >= 20),
        "no drift_refreshed_at on the annotation: {body}"
    );
    // A measurement, never a publication: no push, no PR.
    assert!(!run.pushed(), "--measure pushed to the mirror: {out}");
    assert!(
        !run.gh_log().contains("pr create"),
        "--measure opened a pull request: {}",
        run.gh_log()
    );
    assert!(
        run.curl_log().contains("PATCH"),
        "the measurement never reached the packet: {}",
        run.curl_log()
    );
}

/// THE PULL REQUESTS' STATE, OBSERVED (backlog a5d4322c). The publish
/// region called #239 open for 86 hours on 2026-09-22 while GitHub said
/// it had merged three days earlier: nothing in the pipeline ever asked
/// GitHub. `--measure` runs every day, so it asks — GitHub's public
/// pulls API, no credential — for every publish PR not already read
/// closed, and writes the answer onto the PR's own packet as
/// `pr_state`. Every packet here is CLOSED, as a publish packet is long
/// before its PR merges, so this also pins that the question is asked
/// on a day with no open packet to re-measure.
#[test]
fn a_measure_run_records_what_github_says_about_each_publish_pull_request() {
    if !have_real_jq() {
        eprintln!("publish_github_pr_sh: SKIPPED — the measure path needs a real jq");
        return;
    }
    let run = Run::new("measure-pr-state");
    let pr = |n: u32| format!("https://github.com/{MIRROR_SLUG}/pull/{n}");
    boss_testing::write_file(
        &run.root.join("jobs.json"),
        &format!(
            r#"{{"total":4,"data":[
  {{"id":"00000000-0000-0000-0000-0000000000c1","status":"closed","metadata":{{}},
    "steps":[{{"spec_slug":"open-pr","status":"completed","metadata":{{"pr_url":"{p239}"}}}}]}},
  {{"id":"00000000-0000-0000-0000-0000000000c2","status":"closed","metadata":{{}},
    "steps":[{{"spec_slug":"open-pr","status":"completed","metadata":{{"pr_url":"{p240}"}}}}]}},
  {{"id":"00000000-0000-0000-0000-0000000000c3","status":"closed",
    "metadata":{{"pr_state":{{"pr_url":"{p238}","state":"closed","merged":true}}}},
    "steps":[{{"spec_slug":"open-pr","status":"completed","metadata":{{"pr_url":"{p238}"}}}}]}},
  {{"id":"00000000-0000-0000-0000-0000000000c4","status":"closed","metadata":{{}},
    "steps":[{{"spec_slug":"open-pr","status":"skipped","metadata":{{}}}}]}}]}}"#,
            p238 = pr(238),
            p239 = pr(239),
            p240 = pr(240),
        ),
    );
    // #239 as GitHub answered it from the pod on 2026-09-22.
    run.github_pull(
        239,
        r#"{"number":239,"state":"closed","merged":true,
            "merged_at":"2026-09-19T14:16:04Z","closed_at":"2026-09-19T14:16:04Z"}"#,
    );
    // #240: no fixture, so GitHub answers 404.
    let scan = run.planted_scan(0);
    let (ok, out) = run.measure(&[("BOSS_SECRETS_SCAN", scan)]);
    assert!(
        ok,
        "a PR GitHub would not answer for is not a failed measurement: {out}"
    );

    let states: Vec<serde_json::Value> = run
        .patches()
        .into_iter()
        .filter(|p| p.get("pr_state").is_some())
        .collect();
    assert_eq!(states.len(), 1, "only #239 was answered: {states:?}\n{out}");
    let st = &states[0]["pr_state"];
    assert_eq!(st["pr_url"], pr(239).as_str(), "{st}");
    assert_eq!(st["state"], "closed", "{st}");
    assert_eq!(st["merged"], true, "{st}");
    assert_eq!(st["merged_at"], "2026-09-19T14:16:04Z", "{st}");
    assert!(
        st["read_at"].as_str().is_some_and(|s| s.len() >= 20),
        "no read_at: {st}"
    );
    let log = run.curl_log();
    assert!(
        log.contains("api/jobs/00000000-0000-0000-0000-0000000000c1/metadata"),
        "the answer must land on #239's own packet: {log}"
    );
    assert!(
        log.contains(&format!("repos/{MIRROR_SLUG}/pulls/239")),
        "GitHub was never asked about #239: {log}"
    );
    assert!(
        !log.contains("pulls/238"),
        "a PR already read closed was asked again: {log}"
    );
    // The unanswered one is SAID, not skipped in silence.
    assert!(
        out.contains(&pr(240)) && out.contains("never read"),
        "an unanswered PR must be named: {out}"
    );
}

#[test]
fn a_measure_run_with_no_open_packet_writes_nothing() {
    if !have_real_jq() {
        eprintln!("publish_github_pr_sh: SKIPPED — the measure path needs a real jq");
        return;
    }
    let run = Run::new("measure-no-packet");
    boss_testing::write_file(&run.root.join("jobs.json"), r#"{"data":[]}"#);
    let scan = run.planted_scan(0);
    let (ok, out) = run.measure(&[("BOSS_SECRETS_SCAN", scan)]);
    assert!(ok, "an empty board is not a failure: {out}");
    assert!(
        !run.curl_log().contains("PATCH"),
        "a run with no open packet wrote anyway: {}",
        run.curl_log()
    );
}

/// A scan that FINDS something is a finding ON the packet, not a
/// refusal: the reviewer must see what today's tree would publish. A
/// scan that cannot RUN is the opposite — no evidence is not a pass —
/// and the run answers `not yet` (75) having written nothing.
#[test]
fn a_secrets_finding_rides_the_annotation_and_an_unrunnable_scan_writes_nothing() {
    if !have_real_jq() {
        eprintln!("publish_github_pr_sh: SKIPPED — the measure path needs a real jq");
        return;
    }
    let found = Run::new("measure-secrets");
    let scan = found.planted_scan(1);
    let (ok, out) = found.measure(&[("BOSS_SECRETS_SCAN", scan)]);
    assert!(ok, "a secrets finding is recorded, not refused: {out}");
    assert_eq!(
        found.patched()["drift_refresh"]["secrets_scan"],
        "FAILED",
        "the finding did not reach the packet: {out}"
    );

    let blind = Run::new("measure-noscan");
    let absent = blind.root.join("absent.sh").display().to_string();
    let (ok, out) = blind.measure(&[("BOSS_SECRETS_SCAN", absent)]);
    assert!(!ok, "a measurement with no scan behind it passed: {out}");
    assert!(
        out.contains("not yet"),
        "an unrunnable scan must answer `not yet`: {out}"
    );
    assert!(
        !blind.curl_log().contains("PATCH"),
        "it wrote a drift with no scan behind it: {}",
        blind.curl_log()
    );
}

/// THE READ-BACK MUST SAY NO WHEN THE WRITE DID NOT LAND.
///
/// THE DEFECT (backlog b88a13d5). `--measure` PATCHes the drift onto
/// the held packet and then verifies it, and the verification read the
/// PATCH's own response body. `PATCH /api/jobs/{id}/metadata` answers
/// **204 with no body**, so there was never anything there to read.
/// What that produced depended entirely on the host's `jq`:
///
///   - on `jq-1.6` — this pod's — `jq -e` over an EMPTY document exits
///     **0**, so the check PASSED and verified nothing for its whole
///     life;
///   - on the forge's, it exits non-zero, so a daily rule reported
///     `FAILED` on runs that had done their work correctly
///     (ops-request 9340fd6e, 2026-09-21 17:48Z: the verb said the
///     measurement was not on the packet, and the packet carried it).
///
/// Both halves are the same defect and the second is the dangerous one.
/// A permanently-red daily check is CLAUDE.md's "a check nobody reads";
/// a silently-vacuous one is worse, because nothing ever tells you.
///
/// So this test does not assert the happy path — `a_measure_run_records_
/// todays_drift_on_the_open_packet` already does, and did while the
/// check was vacuous. It asserts the NEGATIVE: a packet that comes back
/// WITHOUT the stamp must fail the run. Nothing could exercise that
/// while the fixture echoed the PATCH back at the verb.
#[test]
fn a_measurement_that_did_not_land_on_the_packet_fails_the_run() {
    if !have_real_jq() {
        eprintln!("publish_github_pr_sh: SKIPPED — the measure path needs a real jq");
        return;
    }
    let run = Run::new("measure-readback-denies");
    let scan = run.planted_scan(0);
    // The PATCH is accepted and changes nothing — the read-back that
    // follows serves the packet as it was.
    run.curl_answering_readback(false);
    let (ok, out) = run.go_argv("--measure", &[("BOSS_SECRETS_SCAN", scan)]);

    assert!(
        !ok,
        "the run must FAIL when the packet does not carry the measurement back. \
         It passed, which means the read-back is not reading the packet: {out}"
    );
    assert!(
        out.contains("not on the packet") || out.contains("does not carry"),
        "the refusal must say the measurement is not on the packet: {out}"
    );
    // AND IT MUST HAVE TRIED. A run that failed before ever writing
    // would satisfy the assertions above while proving nothing.
    let log = run.curl_log();
    assert!(
        log.contains("PATCH"),
        "the run never attempted the write, so this proves nothing about the read-back: {log}"
    );
}

/// THE CONTROL FOR THE TEST ABOVE, and the one that pins the jq
/// dependence out of existence: the SAME fixture with the merge landing
/// must still pass. Without it, a verb that had simply started refusing
/// every measurement would satisfy the negative case.
#[test]
fn the_same_run_passes_once_the_packet_carries_it() {
    if !have_real_jq() {
        eprintln!("publish_github_pr_sh: SKIPPED — the measure path needs a real jq");
        return;
    }
    let run = Run::new("measure-readback-confirms");
    let scan = run.planted_scan(0);
    run.curl_answering_readback(true);
    let (ok, out) = run.go_argv("--measure", &[("BOSS_SECRETS_SCAN", scan)]);
    assert!(ok, "a measurement that DID land must pass: {out}");
    // THE READ-BACK IS A GET OF THE PACKET, not the PATCH's own reply.
    // Read by LINE, and only lines AFTER the write: the PATCH's own
    // line carries the same `/api/jobs/<id>` text, so a substring
    // search over the whole log would be satisfied by the very call
    // whose answer this fix stopped trusting.
    let log = run.curl_log();
    let lines: Vec<&str> = log.lines().collect();
    let patch_at = lines
        .iter()
        .position(|l| l.contains("PATCH"))
        .expect("the run wrote");
    assert!(
        lines[patch_at + 1..]
            .iter()
            .any(|l| l.contains("/api/jobs/") && !l.contains("PATCH")),
        "no read of the packet follows the PATCH — the verb is still trusting the \
         write call's own answer, which is 204 with no body: {log}"
    );
}

// ---------------------------------------------------------------------
// ONE MIRROR PULL REQUEST AT A TIME (backlog d4bfe548, David 2026-09-24).
//
// Measured 04:05Z that day: #242 (publish 2026-09-23, packet d2967a9c,
// closed `pr-opened`) was still OPEN on GitHub, and #243 (packet
// e0558b28) carried everything in #242 plus newer commits; #239-#242
// had all been open at once. The mirror's head had not moved since
// before #239, so each dated snapshot contains every one before it, and
// nothing marked the older ones superseded. So when a run opens (or
// reuses) today's PR, it closes each OLDER open `publish/<date>` PR
// from the mirror's own branches with a comment naming the new one, reads GitHub
// back to prove the close took, and records it on the older packet.
// ---------------------------------------------------------------------

impl Run {
    /// What `gh pr list --json …` answers for the mirror's open PRs.
    fn open_prs(&self, body: &str) {
        boss_testing::write_file(&self.gh_api.join("_open_prs.json"), body);
    }

    /// What GitHub answers for a PR after `gh pr close` — by default a
    /// closed PR; plant anything else to stand in for a close that did
    /// not take.
    fn on_close(&self, body: &str) {
        boss_testing::write_file(&self.gh_api.join("_on_close.json"), body);
    }

    /// The PR numbers the run asked gh to close, in order.
    fn closed_prs(&self) -> Vec<String> {
        self.gh_log()
            .lines()
            .filter_map(|l| l.strip_prefix("pr close "))
            .filter_map(|rest| rest.split_whitespace().next())
            .map(str::to_string)
            .collect()
    }

    /// Every step write the run made, in order, as `{status, metadata}`:
    /// each step PUT's status with the keys the merge-door PATCHes before
    /// it sent (backlog e39a9d2a, Stage 2). A step PUT that carries a
    /// metadata body FAILS here — the step PUT's end state refuses one,
    /// and the step's stored keys are the server's to keep.
    fn puts(&self) -> Vec<serde_json::Value> {
        let text = std::fs::read_to_string(self.root.join("puts.jsonl")).unwrap_or_default();
        let mut merged = serde_json::Map::new();
        let mut out = Vec::new();
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            if let Some(body) = line.strip_prefix("MERGE ") {
                let keys: serde_json::Value =
                    serde_json::from_str(body).expect("a merge body is JSON");
                merged.extend(keys.as_object().expect("a merge body is an object").clone());
            } else {
                let body = line.strip_prefix("PUT ").expect("a PUT or a MERGE line");
                let put: serde_json::Value =
                    serde_json::from_str(body).expect("a PUT body is JSON");
                assert!(
                    put.get("metadata").is_none(),
                    "a step PUT carried a metadata body — the keys go through \
                     PATCH .../steps/{{id}}/metadata: {put}"
                );
                out.push(serde_json::json!({
                    "status": put["status"],
                    "metadata": std::mem::take(&mut merged),
                }));
            }
        }
        out
    }
}

const OLDER_PACKET: &str = "00000000-0000-0000-0000-0000000000c5";

/// A CLOSED publish packet whose open-pr recorded `pr_url` on the
/// mirror's `branch`, published from forge commit `source` — the shape every
/// packet since 254177e2 carries (d2967a9c on 2026-09-24 among them).
fn recorded_publish(id: &str, pr_url: &str, branch: &str, source: &str) -> serde_json::Value {
    let owner = MIRROR_SLUG.split('/').next().unwrap();
    serde_json::json!({
        "id": id, "title": "publish to github", "status": "closed", "metadata": {},
        "steps": [{"id": format!("{id}-open-pr"), "spec_slug": "open-pr", "status": "completed",
                   "metadata": {"pr_url": pr_url, "head": format!("{owner}:{branch}"),
                                "source_sha": source}}],
    })
}

/// Serve `packet` as the one open publish packet, beside `others`.
fn jobs_with(run: &Run, packet: &Packet, others: Vec<serde_json::Value>) {
    let data: Vec<serde_json::Value> = std::iter::once(packet.json()).chain(others).collect();
    boss_testing::write_file(
        &run.root.join("jobs.json"),
        &serde_json::json!({ "data": data }).to_string(),
    );
}

/// The open packet the run publishes for, plus the older packet whose
/// open-pr recorded pull/5 on `branch`, from the same forge main the run
/// publishes — so the run's snapshot contains everything in it.
fn jobs_with_an_older_publish(run: &Run, branch: &str) {
    let main = run.forge_main();
    jobs_with(
        run,
        &Packet::signed(&main),
        vec![recorded_publish(
            OLDER_PACKET,
            &format!("https://github.com/{MIRROR_SLUG}/pull/5"),
            branch,
            &main,
        )],
    );
}

// ---------------------------------------------------------------------
// THE APPROVAL THE VERB ACTS ON (backlog 02b65d81, with 6ab3a61c and
// 0677a618; David 2026-09-27: "Approved the publish, but did not get a
// passkey check").
//
// Measured on publish 8d7a3507: approve completed with `sign_offs []`,
// and this verb — which read nothing but open-pr's readiness — pushed a
// 660-commit snapshot of whatever forge main was AT THAT MOMENT to a
// public repository. Two faults, both the verb's to close now that the
// row demands a passkey: it trusted readiness as an approval (a flag is
// not a signature), and it published forge main's head rather than the
// tree the measurement scanned, the review read and the passkey signed.
//
// So the run reads the approval off the system of record's own record —
// the approve step's stamps, bound to its current shape, over the two
// shas the measurement named — and publishes EXACTLY that tree.
// ---------------------------------------------------------------------

const OPEN_PACKET: &str = "00000000-0000-0000-0000-0000000000aa";
/// The title the row gives approve (`title_template`), which is part of
/// what the passkey signs.
const APPROVE_TITLE: &str = "Approve publishing to the public mirror";

/// How the fixture's approve step was stamped.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stamp {
    /// A passkey over the step's current shape — the only approval.
    Presence,
    /// A session-assured stamp: a click, no passkey.
    Session,
    /// No stamp at all: the 8d7a3507 shape.
    Absent,
    /// A passkey over a DIFFERENT shape than the step now holds.
    OtherShape,
    /// A passkey over this shape, voided by the server since.
    Voided,
}

/// The open publish packet as the verb reads it: measure, approve and
/// open-pr, each field independently settable so a case changes one.
#[derive(Clone, Debug)]
struct Packet {
    /// The tree the approve step names (and the passkey signs).
    source: String,
    /// The tree the approve step says the scan read.
    scanned: String,
    /// What the MEASURE step recorded for both.
    measured_source: String,
    measured_scanned: String,
    secrets: &'static str,
    decision: &'static str,
    stamp: Stamp,
    approve_status: &'static str,
    /// Whether approve carries `source_sha` / `scanned_sha` at all.
    names_a_tree: bool,
    /// Who the stamp names (`authority_id`).
    stamped_by: &'static str,
    /// An extra LIVE presence stamp on the same shape, by someone else,
    /// before the counted one.
    bystander: Option<&'static str>,
    /// The approve step exactly as publish 8d7a3507 carried it (registry
    /// v5): no `assurance_required`, `sign_offs_required: []`,
    /// `sign_offs: []`, metadata `{decision: approved}` and the tree.
    legacy_v5: bool,
}

impl Packet {
    /// Everything agreeing on `sha`, signed with a passkey.
    fn signed(sha: &str) -> Packet {
        Packet {
            source: sha.to_string(),
            scanned: sha.to_string(),
            measured_source: sha.to_string(),
            measured_scanned: sha.to_string(),
            secrets: "clean",
            decision: "approved",
            stamp: Stamp::Presence,
            approve_status: "completed",
            names_a_tree: true,
            stamped_by: "emp-david",
            bystander: None,
            legacy_v5: false,
        }
    }

    fn approve_metadata(&self) -> serde_json::Value {
        if self.legacy_v5 {
            return serde_json::json!({
                "decision": "approved", "source_sha": self.source, "scanned_sha": self.scanned,
            });
        }
        let mut md = serde_json::json!({
            "authority_role": "platform-admin",
            "human_only": true,
            "decision": self.decision,
        });
        if self.names_a_tree {
            md["source_sha"] = serde_json::json!(self.source);
            md["scanned_sha"] = serde_json::json!(self.scanned);
        }
        md
    }

    fn json(&self) -> serde_json::Value {
        let md = self.approve_metadata();
        let shape = boss_core::job::step_shape_hash(APPROVE_TITLE, &md);
        let by = self.stamped_by;
        let stamp = |assurance: &str, shape: &str| {
            serde_json::json!({
                "authority_id": by, "role": "platform-admin",
                "stamped_at": "2026-01-02T00:00:00.000000Z", "shape_hash": shape,
                "assurance": assurance, "presence_nonce": "fixture-nonce",
            })
        };
        let sign_offs = match self.stamp {
            Stamp::Presence => vec![stamp("presence", &shape)],
            Stamp::Session => vec![stamp("session", &shape)],
            Stamp::Absent => vec![],
            Stamp::OtherShape => vec![stamp("presence", &"0".repeat(64))],
            Stamp::Voided => {
                let mut s = stamp("presence", &shape);
                s["voided_at"] = serde_json::json!("2026-01-02T00:01:00Z");
                vec![s]
            }
        };
        let sign_offs = match self.bystander {
            Some(who) => {
                let mut other = stamp("presence", &shape);
                other["authority_id"] = serde_json::json!(who);
                std::iter::once(other).chain(sign_offs).collect()
            }
            None => sign_offs,
        };
        let approve = if self.legacy_v5 {
            serde_json::json!({
                "id": "00000000-0000-0000-0000-0000000000b2", "spec_slug": "approve",
                "title": APPROVE_TITLE, "status": "completed",
                "sign_offs_required": [], "sign_offs": [], "metadata": md})
        } else {
            serde_json::json!({
                "id": "00000000-0000-0000-0000-0000000000b2", "spec_slug": "approve",
                "title": APPROVE_TITLE, "status": self.approve_status,
                "assurance_required": "presence",
                "sign_offs_required": ["platform-admin"],
                "sign_offs": sign_offs, "metadata": md})
        };
        serde_json::json!({
            "id": OPEN_PACKET, "title": "publish to github", "status": "open", "metadata": {},
            "steps": [
                {"id": "00000000-0000-0000-0000-0000000000b1", "spec_slug": "measure",
                 "title": "Measure drift and scan for secrets", "status": "completed",
                 "metadata": {"source_sha": self.measured_source,
                              "scanned_sha": self.measured_scanned,
                              "secrets_scan": self.secrets, "has_drift": "true"}},
                approve,
                {"id": "00000000-0000-0000-0000-0000000000bb", "spec_slug": "open-pr",
                 "status": "ready", "metadata": {"ops_verb": "publish-github-pr"}},
            ],
        })
    }
}

impl Run {
    /// The forge fixture's `main`, as git says.
    fn forge_main(&self) -> String {
        git_in(&self.forge, &["rev-parse", "refs/heads/main"])
    }

    /// Serve `packet` as the one open publish packet.
    fn packet(&self, packet: &Packet) {
        boss_testing::write_file(
            &self.root.join("jobs.json"),
            &serde_json::json!({ "data": [packet.json()] }).to_string(),
        );
    }

    /// Move forge main on by one commit whose tree differs — a train
    /// landing between the measurement and the publish.
    fn forge_moves_on(&self) -> String {
        let blob = git_stdin(
            &self.forge,
            &["hash-object", "-t", "blob", "-w", "--stdin"],
            "forge main, one train later\n",
        );
        let tree = git_stdin(
            &self.forge,
            &["mktree"],
            &format!("100644 blob {blob}\tfile\n"),
        );
        let parent = self.forge_main();
        let commit = git_in(
            &self.forge,
            &["commit-tree", &tree, "-p", &parent, "-m", "a later train"],
        );
        git_in(&self.forge, &["update-ref", "refs/heads/main", &commit]);
        commit
    }

    /// The tree of the snapshot the run pushed to the mirror, if any —
    /// found through `mirror_branch`, because the branch is named by the
    /// snapshot it holds (publish/<date>-<snapshot>, backlog 1f0aa60d).
    fn pushed_tree(&self) -> Option<String> {
        self.mirror_branch()
            .map(|(_, sha)| git_in(&self.mirror, &["rev-parse", &format!("{sha}^{{tree}}")]))
    }

    /// A refusal must stop BEFORE anything leaves: nothing on the mirror,
    /// nothing on the forge, no PR, no completion.
    fn assert_nothing_published(&self, out: &str) {
        assert!(!self.pushed(), "a refused approval still pushed: {out}");
        assert!(
            self.forge_has_branch().is_none(),
            "a refused approval still pushed the snapshot to the forge: {out}"
        );
        assert!(
            !self.gh_log().contains("pr create"),
            "a refused approval still opened a PR: {}",
            self.gh_log()
        );
        assert!(
            !self.curl_log().contains("PUT"),
            "a refused approval still completed open-pr: {}",
            self.curl_log()
        );
    }
}

/// One refusal case: `change` reshapes the signed packet, and the run
/// must refuse naming `names`, having published nothing.
fn refused_when(case: &str, change: impl FnOnce(&mut Packet, &Run), names: &str) {
    if !have_real_jq() {
        eprintln!("publish_github_pr_sh: SKIPPED — the run path needs a real jq");
        return;
    }
    let run = Run::new(case);
    let mut packet = Packet::signed(&run.forge_main());
    change(&mut packet, &run);
    run.packet(&packet);
    let (ok, out) = run.go();
    assert!(!ok, "{case}: the verb published over {packet:?}: {out}");
    assert!(
        out.contains("REFUSED") && out.contains(names),
        "{case}: the refusal must say `{names}`: {out}"
    );
    run.assert_nothing_published(&out);
}

/// THE BUG: an approval with no stamp at all — 8d7a3507's `sign_offs []`.
#[test]
fn an_approval_with_no_stamp_publishes_nothing() {
    refused_when(
        "approval-unstamped",
        |p, _| p.stamp = Stamp::Absent,
        "no platform-admin sign-off",
    );
}

/// A click is not a passkey: a session-assured stamp is refused.
#[test]
fn an_approval_stamped_without_a_passkey_publishes_nothing() {
    refused_when(
        "approval-session",
        |p, _| p.stamp = Stamp::Session,
        "not presence",
    );
}

/// A passkey over some OTHER shape signed something else.
#[test]
fn a_passkey_over_another_shape_publishes_nothing() {
    refused_when(
        "approval-other-shape",
        |p, _| p.stamp = Stamp::OtherShape,
        "not what was signed",
    );
}

/// A stamp the server voided is dead, even on the shape it signed.
#[test]
fn a_voided_passkey_publishes_nothing() {
    refused_when("approval-voided", |p, _| p.stamp = Stamp::Voided, "voided");
}

/// A rejection runs the same ceremony; only `approved` approves.
#[test]
fn a_signed_rejection_publishes_nothing() {
    refused_when(
        "approval-rejected",
        |p, _| p.decision = "rejected",
        "not \"approved\"",
    );
}

/// Readiness is not an approval: an approve step still open refuses.
#[test]
fn an_approve_step_not_completed_publishes_nothing() {
    refused_when(
        "approval-open",
        |p, _| p.approve_status = "ready",
        "not completed",
    );
}

/// The passkey must have signed a NAMED tree.
#[test]
fn an_approval_that_names_no_tree_publishes_nothing() {
    refused_when(
        "approval-no-tree",
        |p, _| p.names_a_tree = false,
        "names no tree",
    );
}

/// THE SCAN MUST COVER THE PUBLISHED SHA (0677a618): a scanned tree that
/// is not the source tree vouches for nothing being published.
#[test]
fn a_scan_that_does_not_cover_the_published_sha_publishes_nothing() {
    refused_when(
        "scan-elsewhere",
        |p, _| {
            p.scanned = "1".repeat(40);
            p.measured_scanned = "1".repeat(40);
        },
        "scan does not cover",
    );
}

/// The approval must be of what was MEASURED: an approve step naming a
/// different tree from the measure step's is two different decisions.
#[test]
fn an_approval_of_a_tree_the_measurement_did_not_name_publishes_nothing() {
    refused_when(
        "approval-unmeasured-tree",
        |p, _| {
            p.measured_source = "2".repeat(40);
            p.measured_scanned = "2".repeat(40);
        },
        "measure step recorded",
    );
}

/// A tree whose secrets scan FAILED never leaves by machine, signed or
/// not: a public push cannot be taken back.
#[test]
fn a_tree_whose_secrets_scan_failed_publishes_nothing() {
    refused_when("scan-failed", |p, _| p.secrets = "FAILED", "secrets scan");
}

/// THE SOURCE MOVED: the signed sha is not on forge main's history (a
/// rewritten main, or a sha from somewhere else) — refused, by name.
#[test]
fn a_signed_tree_that_is_not_on_forge_main_publishes_nothing() {
    refused_when(
        "source-not-on-forge",
        |p, _| {
            let elsewhere = "3".repeat(40);
            p.source = elsewhere.clone();
            p.scanned = elsewhere.clone();
            p.measured_source = elsewhere.clone();
            p.measured_scanned = elsewhere;
        },
        "not on forge main",
    );
}

/// WHO MAY APPROVE IS A NAMED LIST, NEVER A ROLE (adversarial review of
/// 02b65d81, M1). A passkey stamp from ANOTHER platform-admin satisfies
/// the row's `sign_offs_required` — a role — but the claim the publish
/// rests on is David's passkey: the verb's own `approvers` list decides.
#[test]
fn a_passkey_stamp_by_someone_not_named_publishes_nothing() {
    refused_when(
        "approval-wrong-actor",
        |p, _| p.stamped_by = "emp-another-admin",
        "not among the approvers",
    );
}

/// And a live stamp by someone unnamed is a finding even beside a named
/// approver's own: every live stamp on a public publish's approval is a
/// named approver's.
#[test]
fn a_live_stamp_by_someone_not_named_beside_davids_publishes_nothing() {
    refused_when(
        "approval-bystander",
        |p, _| p.bystander = Some("emp-another-admin"),
        "not among the approvers",
    );
}

/// THE MEASURED SHAPE, pinned as refused (review M2): publish 8d7a3507's
/// approve step exactly as the record held it — no
/// `assurance_required`, `sign_offs_required: []`, `sign_offs: []`,
/// `decision: approved` — even carrying a named tree the measurement
/// agrees with. That is the approval that published 660 commits.
#[test]
fn the_8d7a3507_approval_publishes_nothing() {
    refused_when(
        "approval-8d7a3507",
        |p, _| p.legacy_v5 = true,
        "not presence",
    );
}

/// THE SCAN IS RUN HERE, NOT BELIEVED (review H1). The measure step says
/// `clean` — an agent's transcription nothing signs — over a tree whose
/// secrets scan FAILS when the publish runs it over the approved commit.
/// Refused, naming the scan's own report, nothing pushed.
#[test]
fn a_clean_measure_step_over_a_tree_the_scan_fails_publishes_nothing() {
    if !have_real_jq() {
        eprintln!("publish_github_pr_sh: SKIPPED — the run path needs a real jq");
        return;
    }
    let run = Run::new("scan-disagrees-with-measure");
    run.packet(&Packet::signed(&run.forge_main()));
    let failing = run.planted_scan(1);
    let (ok, out) = run.go_with(&[("BOSS_SECRETS_SCAN", failing)]);
    assert!(!ok, "a publish whose own scan failed went ahead: {out}");
    assert!(
        out.contains("REFUSED") && out.contains("FAILED when run here"),
        "the refusal must say the scan run here failed: {out}"
    );
    assert!(
        out.contains("no-secrets: scanned"),
        "the refusal must carry the scan's own report: {out}"
    );
    run.assert_nothing_published(&out);
}

/// A scan that cannot run is not a pass: no evidence refuses too.
#[test]
fn a_publish_whose_scan_cannot_run_publishes_nothing() {
    if !have_real_jq() {
        eprintln!("publish_github_pr_sh: SKIPPED — the run path needs a real jq");
        return;
    }
    let run = Run::new("scan-unrunnable");
    run.packet(&Packet::signed(&run.forge_main()));
    let absent = run.root.join("absent.sh").display().to_string();
    let (ok, out) = run.go_with(&[("BOSS_SECRETS_SCAN", absent)]);
    assert!(!ok, "a publish with no scan behind it went ahead: {out}");
    assert!(
        out.contains("REFUSED") && out.contains("could not run"),
        "the refusal must say the scan could not run: {out}"
    );
    run.assert_nothing_published(&out);
}

/// THE POSITIVE HALF, and the one that makes "bound to what was
/// measured" a property of the push rather than of a check: forge main
/// moves on after the measurement (a train lands between measure and the
/// passkey), and the snapshot carries the MEASURED tree — the one the
/// scan read, the review read and the passkey signed — not the newer
/// head nobody looked at.
#[test]
fn the_snapshot_carries_the_signed_tree_even_after_forge_main_moves() {
    if !have_real_jq() {
        eprintln!("publish_github_pr_sh: SKIPPED — the run path needs a real jq");
        return;
    }
    let run = Run::new("signed-tree-published");
    run.echoing_curl();
    let measured = run.forge_main();
    run.packet(&Packet::signed(&measured));
    let later = run.forge_moves_on();
    let (ok, out) = run.go();
    assert!(
        ok,
        "a passkey-signed approval of a tree on forge main must publish: {out}"
    );

    let signed_tree = git_in(&run.forge, &["rev-parse", &format!("{measured}^{{tree}}")]);
    let later_tree = git_in(&run.forge, &["rev-parse", &format!("{later}^{{tree}}")]);
    assert_ne!(
        signed_tree, later_tree,
        "fixture: the two trees must differ"
    );
    assert_eq!(
        run.pushed_tree().as_deref(),
        Some(signed_tree.as_str()),
        "the snapshot must carry the SIGNED tree ({measured}), not forge main's newer head \
         ({later}): {out}"
    );
    let puts = run.puts();
    let done = puts
        .iter()
        .find(|p| p["status"] == "completed")
        .unwrap_or_else(|| panic!("open-pr was never completed: {puts:?}"));
    assert_eq!(
        done["metadata"]["source_sha"], measured,
        "open-pr records the tree it published: {done}"
    );
    assert!(
        out.contains(&measured),
        "the run names the signed tree it published: {out}"
    );
}

/// One row of `gh pr list --json number,url,headRefName,headRepositoryOwner,
/// headRepository`, from a repository named like the mirror.
fn pr_row(n: u32, url: &str, head: &str, owner: &str) -> String {
    pr_row_from(n, url, head, owner, MIRROR_SLUG.split('/').nth(1).unwrap())
}

/// `pr_row`, from a repository called `repo` — gh answers `headRepository`
/// as `{id, name}`, the owner being a separate field.
fn pr_row_from(n: u32, url: &str, head: &str, owner: &str, repo: &str) -> String {
    format!(
        r#"{{"number":{n},"url":"{url}","headRefName":"{head}","headRepositoryOwner":{{"login":"{owner}"}},"headRepository":{{"id":"R_{n}","name":"{repo}"}}}}"#
    )
}

#[test]
fn a_new_publish_pr_closes_each_older_publish_pr_as_superseded() {
    if !have_real_jq() {
        eprintln!("publish_github_pr_sh: SKIPPED — the run path needs a real jq");
        return;
    }
    let run = Run::new("supersede-older");
    run.echoing_curl();
    // The older packet published forge main as it stood; a train lands;
    // this run publishes the newer main, which contains it.
    let older_source = run.forge_main();
    let newer_source = run.forge_moves_on();
    let owner = MIRROR_SLUG.split('/').next().unwrap();
    let url = |n: u32| format!("https://github.com/{MIRROR_SLUG}/pull/{n}");
    let new_pr = format!("https://github.invalid/{MIRROR_SLUG}/pull/1");
    jobs_with(
        &run,
        &Packet::signed(&newer_source),
        vec![
            recorded_publish(OLDER_PACKET, &url(5), "publish/2026-01-01", &older_source),
            // A packet recorded #12's url and branch, but #12 comes from
            // ANOTHER repository of the same owner: the record is of the
            // mirror's branch, and this is not it (1a2bcf11, finding 3).
            recorded_publish(
                "00000000-0000-0000-0000-0000000000c7",
                &url(12),
                "publish/2025-12-29",
                &older_source,
            ),
        ],
    );
    run.open_prs(&format!(
        "[{}]",
        [
            // An older publish from the mirror's own branches, recorded by its packet: superseded.
            pr_row(5, &url(5), "publish/2026-01-01", owner),
            // Ours by owner and name, but NO packet recorded it — a PR a
            // person opened by hand on a publish-shaped branch is theirs,
            // not the sweep's (1a2bcf11, finding 3). Kept, and said.
            pr_row(4, &url(4), "publish/2025-12-31", owner),
            // Somebody else's branch that happens to be named publish/:
            // not ours to close.
            pr_row(6, &url(6), "publish/2025-12-30", "someone-else"),
            // Ours, but not a publish.
            pr_row(9, &url(9), "fix-a-typo", owner),
            // Same owner, another repository, recorded above: kept.
            pr_row_from(12, &url(12), "publish/2025-12-29", owner, "another-repo"),
            // Today's own PR, and a NEWER one: never closed by today's.
            pr_row(1, &new_pr, &format!("publish/{PUBLISH_DATE}"), owner),
            pr_row(10, &url(10), "publish/2026-01-03", owner),
        ]
        .join(",")
    ));

    let (ok, out) = run.go();
    assert!(ok, "{out}");

    let mut closed = run.closed_prs();
    closed.sort();
    assert_eq!(
        closed,
        vec!["5".to_string()],
        "exactly the publish PR a packet recorded on the mirror's branch, from a tree this \
         run's contains, is closed: {}",
        run.gh_log()
    );
    assert!(
        out.contains(&url(12)) && out.contains("another-repo"),
        "a PR from another repository of the same owner must be named as kept: {out}"
    );
    // The comment names the PR that supersedes it, and the new PR exists
    // BEFORE anything older is closed.
    let gh = run.gh_log();
    let created = gh.find("pr create").expect("today's PR was opened");
    for line in gh.lines().filter(|l| l.starts_with("pr close ")) {
        assert!(
            line.contains(&new_pr),
            "the close comment must name the superseding PR: {line}"
        );
        assert!(
            gh.find(line).is_some_and(|at| at > created),
            "an older PR was closed before today's was opened: {gh}"
        );
    }

    // Recorded on the older packet: the supersession, and GitHub's own
    // answer about the PR's state, read back after the close.
    let log = run.curl_log();
    assert!(
        log.contains(&format!("api/jobs/{OLDER_PACKET}/metadata")),
        "nothing was written onto the older packet: {log}"
    );
    let patches = run.patches();
    let sup = patches
        .iter()
        .find_map(|p| p.get("pr_superseded"))
        .unwrap_or_else(|| panic!("no pr_superseded annotation: {patches:?}"));
    assert_eq!(sup["pr_url"], url(5).as_str(), "{sup}");
    assert_eq!(sup["by_pr_url"], new_pr.as_str(), "{sup}");
    let effect = patches
        .iter()
        .find(|p| p.get("pr_state").is_some())
        .unwrap_or_else(|| panic!("no pr_state read back: {patches:?}"));
    let st = &effect["pr_state"];
    assert_eq!(st["pr_url"], url(5).as_str(), "{st}");
    assert_eq!(st["state"], "closed", "{st}");
    // THE KEYS THE TERMINAL READS, beside the effect (backlog 78f2fbda):
    // the `superseded` outcome is ready on `job.metadata.superseded_by`
    // and `job.metadata.supersession_translation`, so a publisher that
    // wrote only `pr_superseded` left publish 8d7a3507 open 95 minutes.
    assert_eq!(
        effect["superseded_by"], OPEN_PACKET,
        "the older packet must name the packet that superseded it: {effect}"
    );
    let words = effect["supersession_translation"]
        .as_str()
        .unwrap_or_else(|| panic!("no supersession_translation: {effect}"));
    assert!(
        words.contains(&url(5)) && words.contains(&new_pr),
        "the translation names both pull requests: {words}"
    );
    // #4 was recorded by no packet: KEPT, and said, not skipped in silence.
    assert!(
        out.contains(&url(4)) && out.contains("no publish packet recorded"),
        "an open PR no packet recorded must be named as kept: {out}"
    );

    // Today's packet carries what it superseded.
    let puts = run.puts();
    let done = puts
        .iter()
        .find(|p| p["status"] == "completed")
        .unwrap_or_else(|| panic!("open-pr was never completed: {puts:?}"));
    let superseded = done["metadata"]["superseded_prs"]
        .as_array()
        .unwrap_or_else(|| panic!("no superseded_prs on open-pr: {done}"));
    let urls: Vec<&str> = superseded.iter().filter_map(|v| v.as_str()).collect();
    assert_eq!(urls, vec![url(5).as_str()], "{done}");
}

/// THE OVERLAP (1a2bcf11, finding 2). Two publish packets can be open at
/// once — a superseding packet is filed while the older one waits — and
/// nothing orders their runs. A run for the OLDER tree that comes second
/// must not close the PR carrying the newer one: a PR is superseded only
/// when its recorded source is contained in this run's source, which is
/// the claim the close comment makes.
#[test]
fn a_run_for_an_older_tree_never_closes_the_pr_of_a_newer_one() {
    if !have_real_jq() {
        eprintln!("publish_github_pr_sh: SKIPPED — the run path needs a real jq");
        return;
    }
    let run = Run::new("supersede-never-newer");
    run.echoing_curl();
    let owner = MIRROR_SLUG.split('/').next().unwrap();
    // This packet signed forge main as it stood; a train landed and a
    // second packet published the newer main as PR #7.
    let older_source = run.forge_main();
    let newer_source = run.forge_moves_on();
    let newer = format!("https://github.com/{MIRROR_SLUG}/pull/7");
    jobs_with(
        &run,
        &Packet::signed(&older_source),
        vec![recorded_publish(
            OLDER_PACKET,
            &newer,
            &format!("publish/{PUBLISH_DATE}-0123456789ab"),
            &newer_source,
        )],
    );
    run.open_prs(&format!(
        "[{}]",
        pr_row(
            7,
            &newer,
            &format!("publish/{PUBLISH_DATE}-0123456789ab"),
            owner
        )
    ));

    let (ok, out) = run.go();
    assert!(ok, "{out}");
    assert!(
        run.closed_prs().is_empty(),
        "the older tree's run closed the newer tree's PR: {}",
        run.gh_log()
    );
    assert!(
        out.contains(&newer) && out.contains(&newer_source[..12]),
        "the kept PR must be named with the source that is not in this run's: {out}"
    );
    assert!(
        !run.curl_log()
            .contains(&format!("api/jobs/{OLDER_PACKET}/metadata")),
        "nothing may be written onto the newer packet: {}",
        run.curl_log()
    );
}

/// ONE RUN AT A TIME (1a2bcf11, finding 2). The one-open-packet guard is
/// the daily rule's, and a superseding packet is filed beside the one it
/// supersedes, so two runs can overlap; they share one private clone and
/// both read the open-PR listing before either closes anything. A run
/// that cannot take the state dir's lock within its wait refuses, naming
/// the lock, before it reads or pushes anything.
#[test]
fn a_second_run_waits_for_the_first_and_refuses_when_it_does_not_finish() {
    if !have_real_jq() {
        eprintln!("publish_github_pr_sh: SKIPPED — the run path needs a real jq");
        return;
    }
    let run = Run::new("one-run-at-a-time");
    run.echoing_curl();
    let state = run.root.join("state");
    std::fs::create_dir_all(&state).unwrap();
    let lock = state.join("publish.lock");
    // The first run, standing in: flock(1) holding the same file. `-o`
    // keeps the lock out of the sleep it runs, so killing flock releases
    // it rather than leaving an orphaned sleep holding it.
    let mut holder = Command::new("flock")
        .arg("-o")
        .arg(&lock)
        .args(["sleep", "30"])
        .spawn()
        .expect("flock runs");
    // Wait until it holds the lock, not merely until it started.
    let held = (0..100).any(|_| {
        std::thread::sleep(std::time::Duration::from_millis(50));
        !Command::new("flock")
            .args(["-n"])
            .arg(&lock)
            .arg("true")
            .status()
            .is_ok_and(|s| s.success())
    });
    assert!(held, "the stand-in first run never took {}", lock.display());

    let (ok, out) = run.go_with(&[("BOSS_PUBLISH_LOCK_WAIT", "1".to_string())]);
    let _ = holder.kill();
    let _ = holder.wait();
    assert!(
        !ok,
        "a second run went ahead while the first held the lock: {out}"
    );
    assert!(
        out.contains("REFUSED") && out.contains(&lock.display().to_string()),
        "the refusal must name the lock it could not take: {out}"
    );
    run.assert_nothing_published(&out);
    assert!(
        !run.curl_log().contains("api/jobs"),
        "the second run read the system of record before it held the lock: {}",
        run.curl_log()
    );

    // Released, the same run goes ahead.
    let (ok, out) = run.go_with(&[("BOSS_PUBLISH_LOCK_WAIT", "1".to_string())]);
    assert!(ok, "the run after the lock was released: {out}");
    assert!(run.pushed(), "{out}");
}

/// A close is a claim until GitHub is read back saying so: a PR that
/// still reads open after `gh pr close` fails the run, and open-pr is
/// NOT completed, so a re-run (which reuses today's PR) tries again.
#[test]
fn a_close_github_does_not_confirm_fails_the_run_before_open_pr_completes() {
    if !have_real_jq() {
        eprintln!("publish_github_pr_sh: SKIPPED — the run path needs a real jq");
        return;
    }
    let run = Run::new("supersede-unconfirmed");
    run.echoing_curl();
    jobs_with_an_older_publish(&run, "publish/2026-01-01");
    let owner = MIRROR_SLUG.split('/').next().unwrap();
    let old = format!("https://github.com/{MIRROR_SLUG}/pull/5");
    run.open_prs(&format!(
        "[{}]",
        pr_row(5, &old, "publish/2026-01-01", owner)
    ));
    run.on_close(r#"{"number":5,"state":"open","merged":false}"#);

    let (ok, out) = run.go();
    assert!(!ok, "a close GitHub still reads open must fail: {out}");
    assert!(
        out.contains(&old) && out.contains("still reads open"),
        "the failure must name the PR and what GitHub said: {out}"
    );
    assert!(
        run.puts().is_empty(),
        "open-pr was completed over an unconfirmed close: {:?}",
        run.puts()
    );
}

// ---------------------------------------------------------------------
// A SECOND PUBLISH ON THE SAME DAY (backlog 1f0aa60d, measured
// 2026-09-27). Publish 8d7a3507 superseded 1fcbefde, whose PR #245 stood
// on dauld:publish/2026-09-27. Both ran that day, the branch was named
// by the date alone, and both pushes were `--force`: ops-request
// 9084d6cd moved #245's head from 1fcbefde's snapshot d459c67a to its
// own 28554177, then `gh pr create` refused ("a pull request for branch
// … already exists") and the verb exited 1. The reuse lookup that should
// have found #245 asked `gh pr list --head dauld:publish/2026-09-27`,
// a spelling gh's manual says it does not support, so it never matched
// anything. 1fcbefde's record still said #245 was d459c67a; the PR said
// otherwise; and read-publish-checks then refused 8d7a3507 because
// open-pr, completed by hand, carried no snapshot_commit.
//
// So: every publish gets its own branch, `publish/<date>-<snapshot>`,
// pushed without force; the older PR is closed by the supersession
// sweep like any other; and an open PR is found by reading the listing
// gh does answer — owner and branch compared, not asked of --head.
// ---------------------------------------------------------------------

/// Plants `publish/<PUBLISH_DATE>` — the date-only name every publish
/// used until 1f0aa60d — at `repo`'s main, and returns that commit.
fn a_date_only_branch(repo: &Path) -> String {
    let main = git_in(repo, &["rev-parse", "refs/heads/main"]);
    git_in(
        repo,
        &[
            "update-ref",
            &format!("refs/heads/publish/{PUBLISH_DATE}"),
            &main,
        ],
    );
    main
}

/// THE INCIDENT. An older publish's PR is open on the date-only branch
/// of the SAME day. The run must open its own branch and PR, leave the
/// open PR's branch exactly where it was on the mirror AND the forge, close
/// the older PR as superseded, and complete open-pr with the url, the
/// snapshot and the head it actually published.
#[test]
fn a_second_publish_on_the_same_day_opens_its_own_branch_and_moves_no_open_pr() {
    if !have_real_jq() {
        eprintln!("publish_github_pr_sh: SKIPPED — the run path needs a real jq");
        return;
    }
    let run = Run::new("second-publish-same-day");
    run.echoing_curl();
    jobs_with_an_older_publish(&run, &format!("publish/{PUBLISH_DATE}"));
    let owner = MIRROR_SLUG.split('/').next().unwrap();
    let older = format!("https://github.com/{MIRROR_SLUG}/pull/5");
    let mirror_before = a_date_only_branch(&run.mirror);
    let forge_before = a_date_only_branch(&run.forge);
    run.open_prs(&format!(
        "[{}]",
        pr_row(5, &older, &format!("publish/{PUBLISH_DATE}"), owner)
    ));

    let (ok, out) = run.go();
    assert!(ok, "{out}");

    // The open PR's branch did not move — on either side.
    let date_only = format!("refs/heads/publish/{PUBLISH_DATE}");
    assert_eq!(
        git_in(&run.mirror, &["rev-parse", &date_only]),
        mirror_before,
        "the open PR's branch on the mirror was moved: {out}"
    );
    assert_eq!(
        git_in(&run.forge, &["rev-parse", &date_only]),
        forge_before,
        "the open PR's branch on the forge was moved: {out}"
    );

    // Its own branch, named by the date and the snapshot it holds.
    let (name, sha) = run
        .mirror_branch()
        .unwrap_or_else(|| panic!("no publish/<date>-<snapshot> branch on the mirror: {out}"));
    let suffix = name
        .strip_prefix(&format!("publish/{PUBLISH_DATE}-"))
        .unwrap_or_else(|| panic!("{name} is not publish/<date>-<snapshot>"));
    assert!(
        !suffix.is_empty() && sha.starts_with(suffix),
        "the branch {name} must name the snapshot it holds ({sha})"
    );
    let gh = run.gh_log();
    assert!(
        gh.lines()
            .any(|l| l.starts_with("pr create") && l.contains(&format!("--head {name} "))),
        "the PR must be opened on the run's own branch: {gh}"
    );

    // The older PR is superseded through the sweep, not overwritten.
    assert_eq!(run.closed_prs(), vec!["5".to_string()], "{gh}");

    let puts = run.puts();
    let done = puts
        .iter()
        .find(|p| p["status"] == "completed")
        .unwrap_or_else(|| panic!("open-pr was never completed: {puts:?}"));
    let md = &done["metadata"];
    assert_eq!(
        md["pr_url"],
        format!("https://github.invalid/{MIRROR_SLUG}/pull/1").as_str(),
        "{done}"
    );
    assert_eq!(md["snapshot_commit"], sha.as_str(), "{done}");
    assert_eq!(md["head"], format!("{owner}:{name}").as_str(), "{done}");
}

/// THE REUSE LOOKUP, and the re-run it exists for. The snapshot is a
/// pure function of what it publishes, so a second run of the same
/// packet builds the same commit, pushes nothing new, and must FIND the
/// PR it opened the first time — in the listing, because `--head
/// <owner>:<branch>` answers nothing (the stub answers it the way gh
/// does). Found, it opens no second PR and completes open-pr with that
/// PR's url and the snapshot it carries.
#[test]
fn a_re_run_finds_its_open_pr_in_the_listing_and_opens_no_second_one() {
    if !have_real_jq() {
        eprintln!("publish_github_pr_sh: SKIPPED — the run path needs a real jq");
        return;
    }
    let run = Run::new("re-run-reuses");
    run.echoing_curl();
    let owner = MIRROR_SLUG.split('/').next().unwrap();

    let (ok, out) = run.go();
    assert!(ok, "the first run: {out}");
    let (name, sha) = run.mirror_branch().expect("the first run published");

    // GitHub now holds that run's PR; the log starts over.
    let mine = format!("https://github.com/{MIRROR_SLUG}/pull/3");
    run.open_prs(&format!("[{}]", pr_row(3, &mine, &name, owner)));
    std::fs::remove_file(run.root.join("gh.log")).unwrap();
    std::fs::remove_file(run.root.join("puts.jsonl")).unwrap();

    let (ok, out) = run.go();
    assert!(ok, "the re-run: {out}");
    assert_eq!(
        run.mirror_branch(),
        Some((name.clone(), sha.clone())),
        "the re-run must build the SAME snapshot on the SAME branch: {out}"
    );
    let gh = run.gh_log();
    assert!(
        !gh.contains("pr create"),
        "the open PR was not found, so a second was asked for: {gh}"
    );
    assert!(
        out.contains(&format!("reusing {mine} at {sha}")),
        "the run must say which PR it reused and what it carries: {out}"
    );
    assert!(
        run.closed_prs().is_empty(),
        "its own PR is never superseded: {gh}"
    );
    let puts = run.puts();
    let done = puts
        .iter()
        .find(|p| p["status"] == "completed")
        .unwrap_or_else(|| panic!("open-pr was never completed: {puts:?}"));
    assert_eq!(done["metadata"]["pr_url"], mine.as_str(), "{done}");
    assert_eq!(done["metadata"]["snapshot_commit"], sha.as_str(), "{done}");
}

/// A PR that cannot be opened is a FAILED run that names what it left
/// behind — the branch and the snapshot it pushed, which no PR carries —
/// and completes nothing, so the answer rule troubles open-pr and files
/// the alert (f47861a5) rather than a step reading done.
#[test]
fn a_refused_pr_create_names_what_it_pushed_and_completes_nothing() {
    if !have_real_jq() {
        eprintln!("publish_github_pr_sh: SKIPPED — the run path needs a real jq");
        return;
    }
    let run = Run::new("pr-create-refused");
    run.echoing_curl();
    boss_testing::write_file(
        &run.gh_api.join("_pr_create_refuses"),
        "GraphQL: something GitHub refused (createPullRequest)\n",
    );

    let (ok, out) = run.go();
    assert!(!ok, "a PR that was not opened must fail the run: {out}");
    let (name, sha) = run.mirror_branch().expect("the branch was pushed");
    let failed = out
        .lines()
        .find(|l| l.contains("FAILED"))
        .unwrap_or_else(|| panic!("no FAILED line: {out}"));
    assert!(
        failed.contains(&name)
            && failed.contains(&sha)
            && failed.contains("something GitHub refused"),
        "the FAILED line must name the branch, the snapshot and gh's words: {failed}"
    );
    assert!(
        run.puts().is_empty(),
        "open-pr completed with no PR: {:?}",
        run.puts()
    );
}

/// THE RE-RUN AFTER A TRAIN (02b65d81 meets 1f0aa60d). The snapshot
/// publishes the APPROVED commit, so nothing that moves when forge main
/// moves may enter it — not forge main's commit time, not its sha in the
/// message. Otherwise a re-run of the same approved packet after any
/// train lands builds a NEW snapshot of the same tree, pushes a second
/// `publish/<date>-<snapshot>` branch and opens a second PR for one
/// approval. So: first run, a train lands, re-run — the same branch and
/// the same commit, the PR reused, nothing created.
#[test]
fn a_re_run_after_forge_main_moves_rebuilds_the_same_snapshot() {
    if !have_real_jq() {
        eprintln!("publish_github_pr_sh: SKIPPED — the run path needs a real jq");
        return;
    }
    let run = Run::new("re-run-after-a-train");
    run.echoing_curl();
    let owner = MIRROR_SLUG.split('/').next().unwrap();

    let (ok, out) = run.go();
    assert!(ok, "the first run: {out}");
    let (name, sha) = run.mirror_branch().expect("the first run published");

    // A train lands on forge main one second or more later; GitHub holds
    // the first run's PR; the logs start over.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let later = run.forge_moves_on();
    let mine = format!("https://github.com/{MIRROR_SLUG}/pull/3");
    run.open_prs(&format!("[{}]", pr_row(3, &mine, &name, owner)));
    std::fs::remove_file(run.root.join("gh.log")).unwrap();
    std::fs::remove_file(run.root.join("puts.jsonl")).unwrap();

    let (ok, out) = run.go();
    assert!(ok, "the re-run after forge main moved to {later}: {out}");
    assert_eq!(
        run.mirror_branch(),
        Some((name.clone(), sha.clone())),
        "the re-run must rebuild the SAME snapshot on the SAME branch although forge main \
         moved to {later}: {out}"
    );
    assert!(
        !run.gh_log().contains("pr create"),
        "one approval, one PR: a second was opened after a train: {}",
        run.gh_log()
    );
}

// ---------------------------------------------------------------------
// THE BRANCHES A PUBLISH LEAVES BEHIND (backlog 1a2bcf11, finding 1;
// measured 2026-09-27). Since 1f0aa60d every publish pushes a branch of
// its own to the forge and the mirror, and nothing removed one: that
// evening both held seven publish/* heads whose PRs (#239-#245) GitHub
// read closed, #239-#241 merged. `boss orient` promised "stays until
// GitHub reports the PR merged or closed", and nothing kept the promise.
//
// So each run, after its own PR is open and the older ones closed, takes
// every publish branch a publish packet recorded, asks GitHub about that
// PR, and deletes the branch — forge first (the order the off-site push's
// old publish/* carry needed; backlog a2b58aab ended the carry), then the
// mirror — only when the PR reads closed, no open
// PR stands on the branch, and the branch still holds the head GitHub
// names for the PR. Each delete is leased to that head and read back.
// ---------------------------------------------------------------------

/// A parentless commit in `repo` holding `content` — a publish snapshot
/// stand-in that is on nobody's main.
fn a_commit(repo: &Path, content: &str) -> String {
    let blob = git_stdin(
        repo,
        &["hash-object", "-t", "blob", "-w", "--stdin"],
        content,
    );
    let tree = git_stdin(repo, &["mktree"], &format!("100644 blob {blob}\tfile\n"));
    git_in(repo, &["commit-tree", &tree, "-m", content])
}

/// `branch` at `sha` in `repo`, which must already hold the commit.
fn plant(repo: &Path, branch: &str, sha: &str) {
    git_in(repo, &["update-ref", &format!("refs/heads/{branch}"), sha]);
}

/// `branch`'s head in `repo`, or `None`.
fn head_of(repo: &Path, branch: &str) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "rev-parse",
            "--verify",
            "-q",
            &format!("refs/heads/{branch}"),
        ])
        .output()
        .expect("git runs");
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// What GitHub's pulls API answers for PR `n` on the mirror's `branch`.
fn a_pull(n: u32, state: &str, merged: bool, branch: &str, sha: &str) -> String {
    format!(
        r#"{{"number":{n},"state":"{state}","merged":{merged},
            "head":{{"ref":"{branch}","sha":"{sha}","repo":{{"full_name":"{MIRROR_SLUG}"}}}}}}"#
    )
}

#[test]
fn a_publish_prunes_the_branches_of_its_closed_prs_on_the_forge_and_the_mirror() {
    if !have_real_jq() {
        eprintln!("publish_github_pr_sh: SKIPPED — the run path needs a real jq");
        return;
    }
    let run = Run::new("prune-closed");
    run.echoing_curl();
    let owner = MIRROR_SLUG.split('/').next().unwrap();
    let url = |n: u32| format!("https://github.com/{MIRROR_SLUG}/pull/{n}");
    let pull = |n: u32| format!("{MIRROR_SLUG}/pulls/{n}");
    let main = run.forge_main();
    // Two snapshots on nobody's main, on the forge and the mirror alike.
    let snap = a_commit(&run.forge, "an old snapshot\n");
    let other = a_commit(&run.forge, "a different snapshot\n");
    for sha in [&snap, &other] {
        git_in(
            &run.forge,
            &[
                "push",
                "-q",
                &run.mirror.display().to_string(),
                &format!("{sha}:refs/heads/seed-{}", &sha[..8]),
            ],
        );
    }
    let merged = "publish/2025-12-30";
    let open = "publish/2025-12-29";
    let moved = "publish/2025-12-31";
    let by_hand = "publish/2025-12-28";
    let mirror_moved = "publish/2025-12-27";
    for b in [merged, open, moved, by_hand, mirror_moved] {
        plant(&run.forge, b, &snap);
        plant(&run.mirror, b, &snap);
    }
    // The forge's copy moved on since its PR closed: the head GitHub
    // names is no longer what the forge holds.
    plant(&run.forge, moved, &other);
    // The MIRROR's copy moved on instead: the forge's copy goes, the mirror's
    // stays and is named.
    plant(&run.mirror, mirror_moved, &other);
    // The mirror's main, which a record naming `<owner>:main` must never
    // reach, however closed the PR it names.
    plant(&run.mirror, "main", &snap);

    jobs_with(
        &run,
        &Packet::signed(&main),
        vec![
            recorded_publish(
                "00000000-0000-0000-0000-0000000000d3",
                &url(3),
                merged,
                &snap,
            ),
            // #2 is open, and its tree is not in this run's: never closed,
            // so its branch is never pruned.
            recorded_publish("00000000-0000-0000-0000-0000000000d2", &url(2), open, &snap),
            recorded_publish(
                "00000000-0000-0000-0000-0000000000d4",
                &url(4),
                moved,
                &snap,
            ),
            recorded_publish(
                "00000000-0000-0000-0000-0000000000d7",
                &url(7),
                mirror_moved,
                &snap,
            ),
            recorded_publish(
                "00000000-0000-0000-0000-0000000000d8",
                &url(8),
                "main",
                &snap,
            ),
        ],
    );
    run.gh_repo(&pull(3), &a_pull(3, "closed", true, merged, &snap));
    run.gh_repo(&pull(2), &a_pull(2, "open", false, open, &snap));
    run.gh_repo(&pull(4), &a_pull(4, "closed", false, moved, &snap));
    run.gh_repo(&pull(7), &a_pull(7, "closed", false, mirror_moved, &snap));
    run.gh_repo(&pull(8), &a_pull(8, "closed", true, "main", &snap));
    run.open_prs(&format!("[{}]", pr_row(2, &url(2), open, owner)));

    let (ok, out) = run.go();
    assert!(ok, "{out}");

    // Merged: gone from both, and read back gone.
    assert_eq!(
        head_of(&run.forge, merged),
        None,
        "the merged PR's branch is still on the forge: {out}"
    );
    assert_eq!(
        head_of(&run.mirror, merged),
        None,
        "the merged PR's branch is still on the mirror: {out}"
    );
    // Open: untouched on both.
    assert_eq!(
        head_of(&run.forge, open).as_deref(),
        Some(snap.as_str()),
        "{out}"
    );
    assert_eq!(
        head_of(&run.mirror, open).as_deref(),
        Some(snap.as_str()),
        "{out}"
    );
    // Moved on the forge: kept there, and so kept on the mirror — deleting
    // only the mirror's copy would leave the forge to push it back.
    assert_eq!(
        head_of(&run.forge, moved).as_deref(),
        Some(other.as_str()),
        "{out}"
    );
    assert_eq!(
        head_of(&run.mirror, moved).as_deref(),
        Some(snap.as_str()),
        "{out}"
    );
    // No packet recorded it: never a candidate.
    assert_eq!(
        head_of(&run.forge, by_hand).as_deref(),
        Some(snap.as_str()),
        "{out}"
    );
    assert_eq!(
        head_of(&run.mirror, by_hand).as_deref(),
        Some(snap.as_str()),
        "{out}"
    );
    // Moved on the mirror: gone from the forge (it held the PR's head), kept
    // on the mirror, which holds something else.
    assert_eq!(head_of(&run.forge, mirror_moved), None, "{out}");
    assert_eq!(
        head_of(&run.mirror, mirror_moved).as_deref(),
        Some(other.as_str()),
        "{out}"
    );
    // Main, on both sides, and the run's own branch, untouched — even with
    // a record naming `<owner>:main` over a PR GitHub reads merged.
    assert_eq!(
        head_of(&run.forge, "main").as_deref(),
        Some(main.as_str()),
        "{out}"
    );
    assert_eq!(
        head_of(&run.mirror, "main").as_deref(),
        Some(snap.as_str()),
        "{out}"
    );
    assert!(run.pushed() && run.forge_has_branch().is_some(), "{out}");

    // Said, and on the record: what went, and what stayed and why.
    let puts = run.puts();
    let done = puts
        .iter()
        .find(|p| p["status"] == "completed")
        .unwrap_or_else(|| panic!("open-pr was never completed: {puts:?}"));
    assert_eq!(
        done["metadata"]["pruned_branches"],
        serde_json::json!([merged]),
        "{done}"
    );
    let kept: Vec<String> = done["metadata"]["prune_kept"]
        .as_array()
        .unwrap_or_else(|| panic!("no prune_kept on open-pr: {done}"))
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    let names = |b: &str| kept.iter().any(|k| k.starts_with(&format!("{b}: ")));
    assert!(
        names(open) && names(moved) && !names(by_hand) && !names("main"),
        "prune_kept names the open and the moved branch, and neither a branch no packet \
         recorded nor main: {kept:?}"
    );
    assert!(
        kept.iter().any(|k| k.starts_with(&format!(
            "{mirror_moved}: deleted from the forge, but the mirror holds"
        ))),
        "the mirror-moved half is named, saying the forge's copy went: {kept:?}"
    );
    assert!(
        out.contains(&format!("pruned {merged}")),
        "the run says what it pruned: {out}"
    );
}

/// A GUARD NEEDS A KNOWN POSITIVE (adversarial review of 8ec86b42). The
/// open-PR guard counts PRs from the mirror on each branch; if gh's listing
/// changed shape so `from_mirror` matched nothing, every count would read 0
/// and the guard would be blind. This run's OWN PR is open by then — it
/// was just opened or reused — so a fresh listing that does not show it
/// cannot be trusted to show any other, and nothing is pruned.
#[test]
fn a_listing_that_does_not_show_the_runs_own_pr_prunes_nothing() {
    if !have_real_jq() {
        eprintln!("publish_github_pr_sh: SKIPPED — the run path needs a real jq");
        return;
    }
    let run = Run::new("prune-blind-listing");
    run.echoing_curl();
    let url3 = format!("https://github.com/{MIRROR_SLUG}/pull/3");
    let main = run.forge_main();
    let snap = a_commit(&run.forge, "an old snapshot\n");
    git_in(
        &run.forge,
        &[
            "push",
            "-q",
            &run.mirror.display().to_string(),
            &format!("{snap}:refs/heads/seed"),
        ],
    );
    let merged = "publish/2025-12-30";
    plant(&run.forge, merged, &snap);
    plant(&run.mirror, merged, &snap);
    jobs_with(
        &run,
        &Packet::signed(&main),
        vec![recorded_publish(
            "00000000-0000-0000-0000-0000000000d3",
            &url3,
            merged,
            &snap,
        )],
    );
    run.gh_repo(
        &format!("{MIRROR_SLUG}/pulls/3"),
        &a_pull(3, "closed", true, merged, &snap),
    );
    boss_testing::write_file(&run.gh_api.join("_pr_create_unlisted"), "");

    let (ok, out) = run.go();
    assert!(
        ok,
        "a blind listing keeps branches; it does not fail the publish: {out}"
    );
    assert_eq!(
        head_of(&run.forge, merged).as_deref(),
        Some(snap.as_str()),
        "{out}"
    );
    assert_eq!(
        head_of(&run.mirror, merged).as_deref(),
        Some(snap.as_str()),
        "{out}"
    );
    let puts = run.puts();
    let done = puts
        .iter()
        .find(|p| p["status"] == "completed")
        .unwrap_or_else(|| panic!("open-pr was never completed: {puts:?}"));
    assert_eq!(
        done["metadata"]["pruned_branches"],
        serde_json::json!([]),
        "{done}"
    );
    let kept = done["metadata"]["prune_kept"].to_string();
    assert!(
        kept.contains("does not show this run's own open PR"),
        "prune_kept says why nothing was pruned: {kept}"
    );
}

/// A MEASURE YIELDS TO A PUBLISH (adversarial review of 8ec86b42). The
/// daily refresh re-fires on its own cadence; a passkey-approved publish
/// that refuses has to be re-filed by a person. So a --measure that finds
/// the lock held waits briefly and answers not yet (75) — with its
/// DEFAULT wait, well inside the publish's — and reads nothing.
#[test]
fn a_measure_finding_the_lock_held_answers_not_yet_quickly() {
    if !have_real_jq() {
        eprintln!("publish_github_pr_sh: SKIPPED — the measure path needs a real jq");
        return;
    }
    let run = Run::new("measure-yields");
    let state = run.root.join("state");
    std::fs::create_dir_all(&state).unwrap();
    let lock = state.join("publish.lock");
    let mut holder = Command::new("flock")
        .arg("-o")
        .arg(&lock)
        .args(["sleep", "120"])
        .spawn()
        .expect("flock runs");
    let held = (0..100).any(|_| {
        std::thread::sleep(std::time::Duration::from_millis(50));
        !Command::new("flock")
            .args(["-n"])
            .arg(&lock)
            .arg("true")
            .status()
            .is_ok_and(|s| s.success())
    });
    assert!(held, "the stand-in publish never took {}", lock.display());

    let started = std::time::Instant::now();
    let (ok, out) = run.measure(&[]);
    let took = started.elapsed();
    let _ = holder.kill();
    let _ = holder.wait();
    assert!(
        !ok,
        "a measure went ahead while a publish held the lock: {out}"
    );
    assert!(
        out.contains("not yet") && out.contains(&lock.display().to_string()),
        "the measure answers not yet, naming the lock: {out}"
    );
    assert!(
        took < std::time::Duration::from_secs(90),
        "the measure's default wait must be short — it re-fires on its own, the publish \
         does not: took {took:?}"
    );
    assert!(
        !run.curl_log().contains("api/jobs"),
        "the measure read the system of record before it held the lock: {}",
        run.curl_log()
    );
}

// ---------------------------------------------------------------------
// --merge: THE APP MERGES WHAT THE PASSKEY APPROVED (backlog 602fe95f,
// David 2026-09-30: option (ii), "NOBODY pushes or merges main by hand").
//
// Measured anonymously that day: main carried a classic rule requiring
// the checks `rust` and `web` of non-admins, which no workflow produces
// since ci.yml became one Gate job, so every merge of the public main was
// an admin bypass through GitHub's UI — and a UI squash stamped a personal
// address on it (#248). So the merge is the machine's: the same approval,
// read the same way, over the same tree; every check main's ruleset
// requires green on the PR head; and a FAST-FORWARD of main to the
// approved snapshot, so the commit on main is the one the passkey
// approved, authored and committed as the declared publisher.
// ---------------------------------------------------------------------

const GATE_CHECK: &str = "Gate (infra/gate.sh, full)";
const MERGE_STEP_ID: &str = "00000000-0000-0000-0000-0000000000bd";
/// GitHub Actions' App id, which runs the mirror's Gate (measured on
/// #248's head, review 01561b13 N1).
const ACTIONS_APP: u64 = 15368;
/// The App's bot account as GitHub names the PR's opener, and its id —
/// what open-pr records and --merge holds the PR to (backlog 16a9c5ae).
/// The fixture gh's `pr create` answers with the same two.
const APP_BOT: &str = "boss-publisher[bot]";
const APP_BOT_ID: &str = "9001";

fn merge_pr_url() -> String {
    format!("https://github.com/{MIRROR_SLUG}/pull/1")
}

/// A publish run, then the packet as it stands when its `merge` step goes
/// ready: open-pr completed with what the publish recorded, the reading
/// green, the PR open on GitHub, main's ruleset requiring the Gate, and
/// the Gate green on the snapshot.
struct Merge {
    run: Run,
    snapshot: String,
    branch: String,
    main_before: String,
}

impl Merge {
    fn new(case: &str) -> Merge {
        let run = Run::new(case);
        let (ok, out) = run.go();
        assert!(ok, "the publish that precedes the merge failed: {out}");
        let (branch, snapshot) = run
            .mirror_branch()
            .unwrap_or_else(|| panic!("the publish pushed no branch: {out}"));
        let main_before = git_in(&run.mirror, &["rev-parse", "refs/heads/main"]);
        // What the merge run did, on its own: the publish's logs go.
        for f in ["gh.log", "gh-tokens.log", "render.log", "curl.log"] {
            let _ = std::fs::remove_file(run.root.join(f));
        }
        let m = Merge {
            run,
            snapshot,
            branch,
            main_before,
        };
        m.serve(&Packet::signed(&m.run.forge_main()), |_| {});
        m.request(Some(OPEN_PACKET));
        m.put_pull(&m.pull());
        m.rules(&[GATE_CHECK], Some(ACTIONS_APP));
        m.check_runs(&[(GATE_CHECK, "completed", "success")]);
        boss_testing::write_file(&m.run.gh_api.join("_ff_marks_merged"), "");
        m.run.echoing_curl();
        m
    }

    /// The merge-publish-pr request this run answers, filed for
    /// `for_publish` (the filing rule sets it to the packet whose merge
    /// went ready), or naming none.
    fn request(&self, for_publish: Option<&str>) {
        let md = match for_publish {
            Some(id) => {
                serde_json::json!({"verb": "merge-publish-pr", "host": "forge", "for_publish": id})
            }
            None => serde_json::json!({"verb": "merge-publish-pr", "host": "forge"}),
        };
        boss_testing::write_file(
            &self.run.root.join("request.json"),
            &serde_json::json!({"id": REQUEST, "kind": "ops-request", "status": "open", "metadata": md})
                .to_string(),
        );
    }

    fn go_code(&self, extra: &[(&str, String)]) -> (i32, String) {
        let mut env = vec![("BOSS_PUBLISH_READBACK_SLEEP", "0".to_string())];
        env.extend(extra.iter().cloned());
        self.run.go_argv_code("--merge", &env)
    }

    /// Serve `packet` at its merge step, `change` applied to open-pr's
    /// recorded metadata.
    fn serve(&self, packet: &Packet, change: impl FnOnce(&mut serde_json::Value)) {
        let mut j = packet.json();
        let steps = j["steps"].as_array_mut().unwrap();
        let open_pr = steps
            .iter_mut()
            .find(|s| s["spec_slug"] == "open-pr")
            .expect("the packet has an open-pr step");
        open_pr["status"] = serde_json::json!("completed");
        let mut md = serde_json::json!({
            "ops_verb": "publish-github-pr", "pr_url": merge_pr_url(),
            "snapshot_commit": self.snapshot,
            "head": format!("{}:{}", mirror_owner(), self.branch),
            "source_sha": packet.source,
            "opened_by": APP_BOT, "opened_by_id": APP_BOT_ID,
        });
        change(&mut md);
        open_pr["metadata"] = md;
        steps.push(serde_json::json!({
            "id": "00000000-0000-0000-0000-0000000000bc", "spec_slug": "read-checks",
            "status": "completed", "metadata": {"conclusion": "success", "alerts": "0", "rules": "0"}}));
        steps.push(serde_json::json!({
            "id": MERGE_STEP_ID, "spec_slug": "merge", "status": "ready",
            "metadata": {"ops_verb": "merge-publish-pr"}}));
        boss_testing::write_file(
            &self.run.root.join("jobs.json"),
            &serde_json::json!({ "data": [j] }).to_string(),
        );
    }

    /// The PR as GitHub answers it before the merge.
    fn pull(&self) -> serde_json::Value {
        serde_json::json!({
            "number": 1, "state": "open", "merged": false,
            "user": {"login": APP_BOT, "id": 9001, "type": "Bot"},
            "base": {"ref": "main", "repo": {"full_name": MIRROR_SLUG}},
            "head": {"ref": self.branch, "sha": self.snapshot, "repo": {"full_name": MIRROR_SLUG}},
        })
    }

    /// The PR's answer to the anonymous read (curl) and the token's (gh).
    fn put_pull(&self, v: &serde_json::Value) {
        self.run.github_pull(1, &v.to_string());
        self.run
            .gh_repo(&format!("{MIRROR_SLUG}/pulls/1"), &v.to_string());
    }

    /// The rules GitHub says apply to main: an update restriction and the
    /// required checks, each pinned to `app` when given.
    fn rules(&self, contexts: &[&str], app: Option<u64>) {
        let required: Vec<serde_json::Value> = contexts
            .iter()
            .map(|c| match app {
                Some(id) => serde_json::json!({"context": c, "integration_id": id}),
                None => serde_json::json!({"context": c}),
            })
            .collect();
        let mut rules = vec![serde_json::json!({"type": "update", "ruleset_id": 7})];
        if !required.is_empty() {
            rules.push(
                serde_json::json!({"type": "required_status_checks", "ruleset_id": 7,
                "parameters": {"required_status_checks": required,
                               "strict_required_status_checks_policy": false}}),
            );
        }
        boss_testing::write_file(
            &self.run.gh_api.join("_rules.json"),
            &serde_json::Value::Array(rules).to_string(),
        );
    }

    /// The check-runs on the snapshot, each from GitHub Actions (15368).
    fn check_runs(&self, runs: &[(&str, &str, &str)]) {
        let list: Vec<serde_json::Value> = runs
            .iter()
            .enumerate()
            .map(|(i, (name, status, conclusion))| {
                serde_json::json!({
                    "id": i + 1, "name": name, "status": status,
                    "conclusion": if *status == "completed" { serde_json::json!(conclusion) } else { serde_json::Value::Null },
                    "started_at": format!("2026-01-02T00:1{i}:00Z"),
                    "html_url": format!("https://github.com/{MIRROR_SLUG}/runs/{}", i + 1),
                    "app": {"id": 15368},
                })
            })
            .collect();
        boss_testing::write_file(
            &self.run.gh_api.join("_check_runs.json"),
            &serde_json::json!({"total_count": list.len(), "check_runs": list}).to_string(),
        );
    }

    fn go(&self, extra: &[(&str, String)]) -> (bool, String) {
        let mut env = vec![("BOSS_PUBLISH_READBACK_SLEEP", "0".to_string())];
        env.extend(extra.iter().cloned());
        self.run.go_argv("--merge", &env)
    }

    fn main(&self) -> String {
        git_in(&self.run.mirror, &["rev-parse", "refs/heads/main"])
    }

    /// A refusal must stop before main moves, before the token is even
    /// rendered, and before the step is written.
    fn assert_nothing_merged(&self, out: &str) {
        assert_eq!(
            self.main(),
            self.main_before,
            "main moved on a refusal: {out}"
        );
        assert!(
            self.run.puts().is_empty(),
            "a refused merge still wrote its step: {:?}",
            self.run.puts()
        );
        assert!(
            !self.run.render_log().contains("--request"),
            "a refused merge rendered its token: {}",
            self.run.render_log()
        );
    }
}

/// THE POSITIVE CASE: main fast-forwards to the approved snapshot — the
/// commit itself, authored and committed as the publisher, one parent,
/// the old main — GitHub reads the PR merged, the token is revoked, and
/// `merge` completes with `merged_sha` and the answer line the verb file
/// declares as its effect. A second run pushes nothing and proves again.
#[test]
fn a_merge_fast_forwards_main_to_the_approved_snapshot_and_proves_it() {
    if !have_real_jq() {
        eprintln!("publish_github_pr_sh: SKIPPED — the merge path needs a real jq");
        return;
    }
    let m = Merge::new("merge-ff");
    let (ok, out) = m.go(&[]);
    assert!(ok, "the merge refused a green, signed publish: {out}");
    assert_eq!(
        m.main(),
        m.snapshot,
        "main is not the approved snapshot: {out}"
    );
    assert_eq!(
        git_in(&m.run.mirror, &["rev-parse", "refs/heads/main^"]),
        m.main_before,
        "a fast-forward: the snapshot's one parent is the main it replaced"
    );
    assert_eq!(
        git_in(
            &m.run.mirror,
            &[
                "log",
                "-1",
                "--format=%an <%ae> / %cn <%ce>",
                "refs/heads/main"
            ]
        ),
        "David Auld <david@algedonic.dev> / David Auld <david@algedonic.dev>",
        "the commit on the public main names the declared publisher"
    );

    // The answer line is the verb file's effect, and the answer rule's.
    let verb: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(repo_root().join("infra/ops/verbs/merge-publish-pr.json"))
            .unwrap(),
    )
    .unwrap();
    let effect = regex::Regex::new(verb["effect"].as_str().unwrap()).unwrap();
    let line = format!(
        "publish-github-pr: merged {} — main reads {}",
        merge_pr_url(),
        m.snapshot
    );
    assert!(
        out.lines().any(|l| l == line),
        "no answer line `{line}`: {out}"
    );
    assert!(
        effect.is_match(&line),
        "the effect does not match the answer line"
    );

    // The record: the step completed with the commit main reads, and the
    // packet's pr_state reading the merge.
    let puts = m.run.puts();
    assert_eq!(puts.len(), 1, "{puts:?}");
    assert_eq!(puts[0]["status"], "completed");
    assert_eq!(puts[0]["metadata"]["merged_sha"], m.snapshot);
    assert_eq!(puts[0]["metadata"]["method"], "fast-forward");
    assert_eq!(puts[0]["metadata"]["pushed"], true);
    assert_eq!(puts[0]["metadata"]["main_was"], m.main_before);
    assert_eq!(
        puts[0]["metadata"]["required_checks_green"],
        serde_json::json!([GATE_CHECK])
    );
    assert!(
        m.run.curl_log().contains(&format!("steps/{MERGE_STEP_ID}")),
        "the merge step is the one written: {}",
        m.run.curl_log()
    );
    // Every write to the system of record is BOUNDED (review af3f2996
    // B1): they run after main moved, and one that hangs until the
    // runner's timeout is recorded as 124 — an exit the verb never chose.
    let writes: Vec<String> = m
        .run
        .curl_log()
        .lines()
        .filter(|l| l.contains("-X PATCH") || l.contains("-X PUT"))
        .map(str::to_string)
        .collect();
    assert!(!writes.is_empty(), "{}", m.run.curl_log());
    for w in &writes {
        assert!(
            w.contains("--max-time"),
            "an unbounded SoR write after the push: {w}"
        );
    }
    let state = m
        .run
        .patches()
        .into_iter()
        .find_map(|p| p.get("pr_state").cloned())
        .expect("pr_state is written onto the packet");
    assert_eq!(state["merged"], true);
    assert_eq!(state["pr_url"], merge_pr_url());

    // The token: this request's, used, revoked.
    assert!(
        m.run.render_log().contains(&format!("--request {REQUEST}")),
        "{}",
        m.run.render_log()
    );
    assert!(
        m.run.render_log().contains(&format!("EXPIRED {REQUEST}")),
        "the merge did not mark its token expired: {}",
        m.run.render_log()
    );
    assert!(!out.contains(FIXTURE_TOKEN), "the token reached the output");

    // Idempotent: main already reads the snapshot, nothing is pushed, and
    // the proof is made again.
    let _ = std::fs::remove_file(m.run.root.join("puts.jsonl"));
    let (ok, out) = m.go(&[]);
    assert!(ok, "a re-run over a merged main refused: {out}");
    assert!(out.contains("already reads"), "{out}");
    assert_eq!(m.main(), m.snapshot);
    assert_eq!(m.run.puts()[0]["metadata"]["pushed"], false);
}

/// One refusal: `change` reshapes what GitHub or the record says, and the
/// merge refuses naming `names`, having merged nothing.
fn merge_refused_when(case: &str, change: impl FnOnce(&Merge), names: &str) {
    // Exit 2, never 3: main was read NOT at the snapshot and nothing was
    // pushed, so the protocol's `merge-refused` terminal may close the
    // packet (backlog 16a9c5ae).
    merge_refused_with(case, 2, change, names)
}

/// `merge_refused_when` with the exit it must give: 4 for a refusal made
/// before the mirror's main was read (review af3f2996 B2), which the
/// terminal leaves open because nothing says main did not move.
fn merge_refused_with(case: &str, want: i32, change: impl FnOnce(&Merge), names: &str) {
    if !have_real_jq() {
        eprintln!("publish_github_pr_sh: SKIPPED — the merge path needs a real jq");
        return;
    }
    let m = Merge::new(case);
    change(&m);
    let (code, out) = m.go_code(&[]);
    assert_eq!(code, want, "{case}: this refusal exits {want}: {out}");
    assert!(
        out.contains("REFUSED") && out.contains(names),
        "{case}: the refusal must say `{names}`: {out}"
    );
    m.assert_nothing_merged(&out);
}

#[test]
fn a_red_gate_merges_nothing() {
    merge_refused_when(
        "merge-red",
        |m| m.check_runs(&[(GATE_CHECK, "completed", "failure")]),
        "Gate (infra/gate.sh, full) concluded failure",
    );
}

#[test]
fn a_gate_still_running_merges_nothing() {
    merge_refused_when(
        "merge-running",
        |m| m.check_runs(&[(GATE_CHECK, "in_progress", "")]),
        "Gate (infra/gate.sh, full) is in_progress",
    );
}

#[test]
fn a_required_check_that_never_ran_merges_nothing() {
    // The stale-classic shape: a requirement nothing produces.
    merge_refused_when(
        "merge-absent-check",
        |m| m.rules(&[GATE_CHECK, "rust"], Some(ACTIONS_APP)),
        "no check run named \"rust\"",
    );
}

/// Review 01561b13 N1: a required check the ruleset does not pin to an App
/// is satisfied by a run of that NAME from any App that can write checks —
/// CodeQL's could post a green "Gate" beside a real red one. So an
/// unpinned requirement defines no green, and nothing merges over it.
#[test]
fn a_required_check_pinned_to_no_app_merges_nothing() {
    merge_refused_when(
        "merge-unpinned",
        |m| m.rules(&[GATE_CHECK], None),
        "from ANY App",
    );
}

/// Review 01561b13 N1: a queued re-run carries no started_at, so sorted by
/// it an OLDER success came last and won. Any run of a required check
/// still going is not green, whatever an older one concluded.
#[test]
fn a_rerun_still_queued_beside_an_older_success_merges_nothing() {
    merge_refused_when(
        "merge-rerun-queued",
        |m| {
            let runs = serde_json::json!({"total_count": 2, "check_runs": [
                {"id": 1, "name": GATE_CHECK, "status": "completed", "conclusion": "success",
                 "started_at": "2026-01-02T00:10:00Z", "app": {"id": ACTIONS_APP},
                 "html_url": format!("https://github.com/{MIRROR_SLUG}/runs/1")},
                {"id": 2, "name": GATE_CHECK, "status": "queued", "conclusion": null,
                 "started_at": null, "app": {"id": ACTIONS_APP},
                 "html_url": format!("https://github.com/{MIRROR_SLUG}/runs/2")},
            ]});
            boss_testing::write_file(&m.run.gh_api.join("_check_runs.json"), &runs.to_string());
        },
        "Gate (infra/gate.sh, full) is queued",
    );
}

/// Review 01561b13 N4: "opened by a Bot" admitted ANY Bot. The PR must be
/// opened by the App — the login and id open-pr recorded from GitHub's
/// own answer when the App opened it.
#[test]
fn a_pr_another_bot_opened_merges_nothing() {
    merge_refused_when(
        "merge-other-bot",
        |m| {
            let mut p = m.pull();
            p["user"] =
                serde_json::json!({"login": "dependabot[bot]", "id": 49699333, "type": "Bot"});
            m.put_pull(&p);
        },
        "any other Bot is not this App",
    );
}

/// A PR whose open-pr recorded no opener (a publish opened before the
/// record existed, or one GitHub did not answer for) is held to nothing,
/// so it is refused: a missing record fails closed.
#[test]
fn an_open_pr_that_recorded_no_opener_merges_nothing() {
    merge_refused_when(
        "merge-no-opener",
        |m| {
            let forge_main = m.run.forge_main();
            m.serve(&Packet::signed(&forge_main), |md| {
                let o = md.as_object_mut().unwrap();
                o.remove("opened_by");
                o.remove("opened_by_id");
            });
        },
        "open-pr recorded no App as the PR's opener",
    );
}

/// Review 01561b13 N3: the run acted on the FIRST open packet whose merge
/// was ready. Two can stand open at once (a superseding packet is filed
/// beside the one it supersedes), and the answer lands on the request's
/// `for_publish` — so the run acts on that packet, and a request naming
/// none, or a packet not ready, is refused.
#[test]
fn a_merge_acts_on_the_packet_its_request_was_filed_for() {
    if !have_real_jq() {
        eprintln!("publish_github_pr_sh: SKIPPED — the merge path needs a real jq");
        return;
    }
    const DECOY: &str = "00000000-0000-0000-0000-0000000000dd";
    // A second open packet, FIRST in the listing, its merge ready and its
    // passkey voided: acting on it refuses, acting on ours merges.
    let with_decoy = |m: &Merge| {
        let path = m.run.root.join("jobs.json");
        let mut jobs: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let mut decoy = jobs["data"][0].clone();
        decoy["id"] = serde_json::json!(DECOY);
        for s in decoy["steps"].as_array_mut().unwrap() {
            if s["spec_slug"] == "approve" {
                s["sign_offs"][0]["voided_at"] = serde_json::json!("2026-01-02T00:01:00Z");
            }
        }
        jobs["data"].as_array_mut().unwrap().insert(0, decoy);
        boss_testing::write_file(&path, &jobs.to_string());
    };

    let m = Merge::new("merge-for-publish");
    with_decoy(&m);
    let (code, out) = m.go_code(&[]);
    assert_eq!(code, 0, "the request's own packet did not merge: {out}");
    assert_eq!(m.main(), m.snapshot);
    assert!(
        m.run.curl_log().contains(&format!("api/jobs/{REQUEST}")),
        "the run never read its request: {}",
        m.run.curl_log()
    );

    let m = Merge::new("merge-for-decoy");
    with_decoy(&m);
    m.request(Some(DECOY));
    let (code, out) = m.go_code(&[]);
    assert_eq!(code, 2, "{out}");
    assert!(
        out.contains("voided"),
        "the run acted on another packet: {out}"
    );
    m.assert_nothing_merged(&out);

    // Refused before any packet's main was read: exit 4, never 1 or 2 —
    // nothing here says main did not move (review af3f2996 B2).
    merge_refused_with(
        "merge-no-for-publish",
        4,
        |m| m.request(None),
        "names no publish packet",
    );
    merge_refused_with(
        "merge-for-a-closed-packet",
        4,
        |m| m.request(Some("00000000-0000-0000-0000-0000000000ee")),
        "no open publish-to-github packet of that id has its merge step ready",
    );
}

#[test]
fn a_check_pinned_to_another_app_merges_nothing() {
    merge_refused_when(
        "merge-other-app",
        |m| m.rules(&[GATE_CHECK], Some(999)),
        "from App 999",
    );
}

#[test]
fn a_main_with_no_required_check_merges_nothing() {
    // Before the ruleset is applied there is no definition of green.
    merge_refused_when(
        "merge-no-ruleset",
        |m| m.rules(&[], None),
        "requires no status check",
    );
}

#[test]
fn a_pr_head_other_than_the_snapshot_merges_nothing() {
    merge_refused_when(
        "merge-head-moved",
        |m| {
            let mut p = m.pull();
            p["head"]["sha"] = serde_json::json!("f".repeat(40));
            m.put_pull(&p);
        },
        "not the approved snapshot",
    );
}

#[test]
fn a_pr_a_person_opened_merges_nothing() {
    merge_refused_when(
        "merge-by-person",
        |m| {
            let mut p = m.pull();
            p["user"] = serde_json::json!({"login": "someone", "type": "User"});
            m.put_pull(&p);
        },
        "never merged by machine",
    );
}

#[test]
fn a_pr_closed_unmerged_merges_nothing() {
    merge_refused_when(
        "merge-closed",
        |m| {
            let mut p = m.pull();
            p["state"] = serde_json::json!("closed");
            m.put_pull(&p);
        },
        "it reads closed, not merged",
    );
}

#[test]
fn an_open_pr_record_of_another_tree_merges_nothing() {
    merge_refused_when(
        "merge-other-source",
        |m| {
            let forge_main = m.run.forge_main();
            m.serve(&Packet::signed(&forge_main), |md| {
                md["source_sha"] = serde_json::json!("e".repeat(40));
            });
        },
        "the PR is not of the approved tree",
    );
}

#[test]
fn a_voided_passkey_merges_nothing() {
    merge_refused_when(
        "merge-voided",
        |m| {
            let mut p = Packet::signed(&m.run.forge_main());
            p.stamp = Stamp::Voided;
            m.serve(&p, |_| {});
        },
        "voided",
    );
}

#[test]
fn a_main_that_moved_since_the_snapshot_merges_nothing() {
    if !have_real_jq() {
        eprintln!("publish_github_pr_sh: SKIPPED — the merge path needs a real jq");
        return;
    }
    let m = Merge::new("merge-main-moved");
    // Someone else's commit lands on main after the snapshot was built.
    let tree = git_in(&m.run.mirror, &["rev-parse", "refs/heads/main^{tree}"]);
    let moved = git_in(
        &m.run.mirror,
        &[
            "commit-tree",
            &tree,
            "-p",
            &m.main_before,
            "-m",
            "a hand merge",
        ],
    );
    git_in(&m.run.mirror, &["update-ref", "refs/heads/main", &moved]);
    let (code, out) = m.go_code(&[]);
    assert_eq!(code, 2, "{out}");
    assert!(
        out.contains("REFUSED") && out.contains("was built on"),
        "{out}"
    );
    // What happens next, said as it is (backlog 16a9c5ae): the packet
    // closes at merge-refused, so the next daily publish can run and
    // supersede the PR — not "the next publish supersedes" over a packet
    // that kept the next publish from being filed.
    assert!(
        out.contains("closes packet 00000000 at merge-refused")
            && out.contains("next daily publish"),
        "{out}"
    );
    assert_eq!(m.main(), moved, "main moved again: {out}");
    assert!(m.run.puts().is_empty());
}

/// Past the push, silence is still refused: a PR GitHub never reads
/// merged fails the run, naming that main IS the approved commit, and the
/// step is left open; a push GitHub refuses fails naming why.
#[test]
fn a_merge_github_does_not_record_or_refuses_fails_loudly() {
    if !have_real_jq() {
        eprintln!("publish_github_pr_sh: SKIPPED — the merge path needs a real jq");
        return;
    }
    let m = Merge::new("merge-not-marked");
    let _ = std::fs::remove_file(m.run.gh_api.join("_ff_marks_merged"));
    let (code, out) = m.go_code(&[("BOSS_PUBLISH_MERGE_READS", "2".to_string())]);
    // Exit 3: main MOVED, so the packet must not close `merge-refused`
    // (backlog 16a9c5ae), and the line names the re-file that proves it.
    assert_eq!(code, 3, "past the push a failure exits 3: {out}");
    assert!(
        out.contains("FAILED")
            && out.contains("main IS the approved commit")
            && out.contains(&format!("for_publish={OPEN_PACKET}")),
        "{out}"
    );
    assert_eq!(m.main(), m.snapshot);
    assert!(
        m.run.puts().is_empty(),
        "the step was completed over an unproven merge"
    );
    assert!(
        m.run.render_log().contains(&format!("EXPIRED {REQUEST}")),
        "a failed merge still revokes its token: {}",
        m.run.render_log()
    );

    let m = Merge::new("merge-push-refused");
    let hook = m.run.mirror.join("hooks/pre-receive");
    boss_testing::write_exec(
        &hook,
        "#!/bin/sh\necho 'GH013: Repository rule violations found for refs/heads/main.' >&2\nexit 1\n",
    );
    let (code, out) = m.go_code(&[]);
    // Exit 1: the push was refused and main did not move — the packet may
    // close `merge-refused` and the next daily publish runs.
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains("FAILED") && out.contains("main is NOT merged") && out.contains("GH013"),
        "{out}"
    );
    assert_eq!(m.main(), m.main_before);
    assert!(m.run.puts().is_empty());
}

/// Review af3f2996 B1: an exit the verb did not choose, after the push,
/// must still read "main moved". A `set -e` death past the push (here a
/// read-back pause that cannot run) used to leave with the dying command's
/// status — 1, which `merge-refused` reads as "nothing merged". The EXIT
/// trap maps any exit it did not choose to the state's own: 3.
#[test]
fn a_death_the_verb_did_not_choose_after_the_push_still_exits_3() {
    if !have_real_jq() {
        eprintln!("publish_github_pr_sh: SKIPPED — the merge path needs a real jq");
        return;
    }
    let m = Merge::new("merge-dies-after-push");
    let _ = std::fs::remove_file(m.run.gh_api.join("_ff_marks_merged"));
    let (code, out) = m.go_code(&[("BOSS_PUBLISH_READBACK_SLEEP", "not-a-duration".to_string())]);
    assert_eq!(m.main(), m.snapshot, "the push itself went through: {out}");
    assert_eq!(
        code, 3,
        "main moved, so every exit past the push is 3: {out}"
    );
    assert!(m.run.puts().is_empty(), "{out}");
}

/// Review af3f2996 B2: exit 3 is decided by READING main, first. The exit-3
/// line prescribes a re-run; that re-run used to fail with 1 or 2 before
/// it reached the point that said main moved — at the approval re-read,
/// or at the fetch of a publish branch GitHub deleted on merge — and
/// `merge-refused` then closed a merged packet "Not merged". Now main is
/// read against open-pr's snapshot as soon as the packet is known: a
/// re-run over a moved main that fails anywhere exits 3, and one that can
/// prove the merge does so without the deleted branch.
#[test]
fn a_rerun_over_a_moved_main_never_exits_1_or_2() {
    if !have_real_jq() {
        eprintln!("publish_github_pr_sh: SKIPPED — the merge path needs a real jq");
        return;
    }
    let m = Merge::new("merge-rerun-moved");
    let (code, out) = m.go_code(&[]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(m.main(), m.snapshot);
    // GitHub deletes the head branch on merge.
    git_in(
        &m.run.mirror,
        &["update-ref", "-d", &format!("refs/heads/{}", m.branch)],
    );

    // A re-run that CAN prove it: no branch needed.
    let _ = std::fs::remove_file(m.run.root.join("puts.jsonl"));
    let (code, out) = m.go_code(&[]);
    assert_eq!(
        code, 0,
        "a re-run over a merged main proves it without the branch: {out}"
    );
    assert_eq!(m.run.puts()[0]["metadata"]["merged_sha"], m.snapshot);

    // A re-run that fails before it could reach the old main-moved point:
    // the approval no longer reads. Exit 3 — the packet stays open.
    let mut p = Packet::signed(&m.run.forge_main());
    p.stamp = Stamp::Voided;
    m.serve(&p, |_| {});
    let (code, out) = m.go_code(&[]);
    assert_eq!(
        code, 3,
        "main reads the snapshot, so no answer may say otherwise: {out}"
    );
    assert!(out.contains("REFUSED") && out.contains("voided"), "{out}");
}

/// Review af3f2996 B3: a push can land and still report failure (the
/// connection drops after the ref moved). "main is NOT merged" was a claim
/// the verb never read back, and its exit 1 closed the packet refused.
/// Now a failed push reads main: at the snapshot it landed, and the run
/// proves it like any other.
#[test]
fn a_merge_that_dies_before_it_reads_anything_exits_4_not_1() {
    // Review 89d4d390 C1: `--merge` exited 1 before its exit-4 state was
    // set — an unwritable TMPDIR (mktemp, before any trap) and a sor.env
    // without the mirror (sor_require) both measured rc=1, and exit 1 on a
    // still-open merge step closes a packet whose main may have moved.
    // Nothing was read, so nothing says main did not move: exit 4.
    if !have_real_jq() {
        eprintln!("publish_github_pr_sh: SKIPPED — the merge path needs a real jq");
        return;
    }
    let m = Merge::new("merge-dies-early");
    let (code, out) = m.go_code(&[("TMPDIR", "/nonexistent/boss-publish-no-tmp".to_string())]);
    assert_eq!(code, 4, "an unwritable TMPDIR under --merge: {out}");
    let (code, out) = m.go_code(&[
        (
            "BOSS_SOR_ENV",
            "/nonexistent/boss-publish/sor.env".to_string(),
        ),
        ("BOSS_MIRROR_SLUG", "UNSET".to_string()),
    ]);
    assert_eq!(code, 4, "a sor.env without the mirror under --merge: {out}");
    assert!(
        !out.contains("nothing was merged") && !out.contains("Nothing was merged"),
        "an exit-4 line must not also claim nothing merged: {out}"
    );
}

/// Review 89d4d390 C2: EQUALITY IS NOT CONTAINMENT. A main that advanced
/// past the approved snapshot (a superseding publish built on it, merged)
/// carries the commit, and a re-run read it as "not merged" — exit 2 with
/// the branch present, 1 with it deleted — which closed the packet "Not
/// merged". Now a main that is not the snapshot is FETCHED and asked
/// whether it contains it: contained is exit 3 (open), and only a
/// snapshot provably not on main — a sibling built on the same main —
/// earns 2.
#[test]
fn a_main_that_advanced_past_the_snapshot_is_never_read_as_not_merged() {
    if !have_real_jq() {
        eprintln!("publish_github_pr_sh: SKIPPED — the merge path needs a real jq");
        return;
    }
    for delete_branch in [false, true] {
        let m = Merge::new(&format!("merge-advanced-past-{delete_branch}"));
        let (code, out) = m.go_code(&[]);
        assert_eq!(code, 0, "{out}");
        let tree = git_in(&m.run.mirror, &["rev-parse", "refs/heads/main^{tree}"]);
        let child = git_in(
            &m.run.mirror,
            &[
                "commit-tree",
                &tree,
                "-p",
                &m.snapshot,
                "-m",
                "the next publish, built on this one",
            ],
        );
        git_in(&m.run.mirror, &["update-ref", "refs/heads/main", &child]);
        if delete_branch {
            git_in(
                &m.run.mirror,
                &["update-ref", "-d", &format!("refs/heads/{}", m.branch)],
            );
        }
        let (code, out) = m.go_code(&[]);
        assert_eq!(
            code, 3,
            "main contains the approved snapshot (branch deleted: {delete_branch}), so this may never read as not merged: {out}"
        );
        assert_eq!(m.main(), child, "nothing was pushed: {out}");
        assert!(out.contains("contains"), "{out}");
    }
}

/// Review 20beab5a C3: the containment answer is only as good as the
/// history of the verb's persistent clone. A SHALLOW clone whose boundary
/// sits between main and the snapshot answers "not an ancestor" (and may
/// lack the snapshot object), which read as main_still — exit 2, "closes
/// packet … at merge-refused" — while main carried the approved commit.
/// A clone whose history can lie (shallow, or an info/grafts file) is no
/// evidence either way: exit 4, left open.
#[test]
fn a_clone_whose_history_can_lie_never_reads_as_not_merged() {
    if !have_real_jq() {
        eprintln!("publish_github_pr_sh: SKIPPED — the merge path needs a real jq");
        return;
    }
    let advance = |m: &Merge, parent: &str, msg: &str| {
        let tree = git_in(&m.run.mirror, &["rev-parse", "refs/heads/main^{tree}"]);
        let next = git_in(
            &m.run.mirror,
            &["commit-tree", &tree, "-p", parent, "-m", msg],
        );
        git_in(&m.run.mirror, &["update-ref", "refs/heads/main", &next]);
        next
    };
    for delete_branch in [false, true] {
        let m = Merge::new(&format!("merge-shallow-clone-{delete_branch}"));
        let (code, out) = m.go_code(&[]);
        assert_eq!(code, 0, "{out}");
        // Main moves past the snapshot, and the verb's own clone is made
        // shallow AT that commit (a hand repair on the forge host).
        let c1 = advance(&m, &m.snapshot, "c1");
        let clone = m.run.root.join("state/boss.git");
        git_in(
            &clone,
            &[
                "fetch",
                "-q",
                "--depth=1",
                &format!("file://{}", m.run.mirror.display()),
                "+refs/heads/main:refs/remotes/mirror/main",
            ],
        );
        let c2 = advance(&m, &c1, "c2");
        if delete_branch {
            git_in(
                &m.run.mirror,
                &["update-ref", "-d", &format!("refs/heads/{}", m.branch)],
            );
        }
        let (code, out) = m.go_code(&[]);
        assert_eq!(
            code, 4,
            "a shallow clone is no evidence main lacks the snapshot (branch deleted: {delete_branch}): {out}"
        );
        assert!(out.contains("shallow"), "{out}");
        assert_eq!(m.main(), c2, "nothing was pushed: {out}");
    }

    // A grafts file rewrites the ancestry merge-base reads: no evidence either.
    let m = Merge::new("merge-grafted-clone");
    let (code, out) = m.go_code(&[]);
    assert_eq!(code, 0, "{out}");
    advance(&m, &m.snapshot, "c1");
    boss_testing::write_file(&m.run.root.join("state/boss.git/info/grafts"), "");
    let (code, out) = m.go_code(&[]);
    assert_eq!(code, 4, "a grafted clone is no evidence: {out}");
    assert!(out.contains("grafts"), "{out}");
}

#[test]
fn a_push_that_landed_but_reported_failure_is_proven_not_refused() {
    if !have_real_jq() {
        eprintln!("publish_github_pr_sh: SKIPPED — the merge path needs a real jq");
        return;
    }
    let real_git = std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .map(|d| Path::new(d).join("git"))
        .find(|p| p.is_file())
        .expect("git is on PATH");
    let m = Merge::new("merge-push-lies");
    // git itself, except that a push to main lands and then says it failed.
    boss_testing::write_exec(
        &m.run.stubs.join("git"),
        &format!(
            "#!/bin/sh\ncase \" $* \" in\n  *\" push \"*refs/heads/main*) '{g}' \"$@\"; echo 'fatal: the remote end hung up unexpectedly' >&2; exit 128 ;;\nesac\nexec '{g}' \"$@\"\n",
            g = real_git.display()
        ),
    );
    let (code, out) = m.go_code(&[]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("it landed"), "{out}");
    assert_eq!(m.main(), m.snapshot);
    assert_eq!(m.run.puts()[0]["metadata"]["merged_sha"], m.snapshot);
}

/// A run takes no argument, --merge, --measure or --check; any other word
/// is refused before anything is read, never taken for a publish.
#[test]
fn an_unknown_argument_is_refused_not_published() {
    if !have_real_jq() {
        eprintln!("publish_github_pr_sh: SKIPPED — the run path needs a real jq");
        return;
    }
    let run = Run::new("unknown-arg");
    let (ok, out) = run.go_argv("--publish-please", &[]);
    assert!(!ok, "{out}");
    assert!(
        out.contains("REFUSED") && out.contains("unknown argument"),
        "{out}"
    );
    run.assert_nothing_published(&out);
    assert!(!run.curl_log().contains("api/jobs"), "{}", run.curl_log());
}
