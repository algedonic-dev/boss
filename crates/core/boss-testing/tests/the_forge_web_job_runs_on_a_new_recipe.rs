//! The forge's `web` job runs its steps only on a train that changes the
//! runner image's recipe, `infra/forge/boss-ci/` (backlog 81ee98b3).
//!
//! MEASURED on origin/main@85d1be08a (2026-10-20). The job ran web
//! install, web-kit unit, apps/web unit, build and the mocked browser
//! suite on every software train, after build-image and locomotive on
//! the forge's one runner slot, so every minute of it was track time.
//! Since a244c6e7 the train's `--auto` cluster gate runs every web unit
//! suite on every tree and the build and mocked suite whenever the train
//! changes anything they read (web-suite-reads.ts): on the same assembled
//! tree, the forge's steps re-ran the gate's checks.
//!
//! One thing still differs, and it is why the job stays: the image. The
//! forge job runs in the image built from THIS train's recipe; the cluster
//! gate runs the boss-ci:rust1.96 it pulled at launch, built from the
//! recipe main carries. On a train that changes the recipe, the forge's
//! run is the only proof the new image still runs bun, the baked browser
//! and the suite. So a `recipe` step compares the recipe directory's tree
//! at the PR's base and at its head, and every kept step runs unless it
//! answered an explicit `no`. Anything it cannot read — no base, a fetch
//! that fails, a directory missing on either side — answers `yes`: the
//! steps run, as every train ran them before. No evidence is not a skip.
//!
//! The step is EXECUTED here, lifted from the workflow, against git
//! trees shaped like the job's own checkout (a depth-1 fetch of the head,
//! so the base has to be fetched by the step itself). That the kept steps
//! carry exactly this guard is gate_sh.rs's
//! `the_forge_web_job_runs_only_what_the_train_gate_may_skip`.

use boss_testing::{create_dir, repo_root, scratch_dir, write_file};
use std::path::Path;
use std::process::{Command, Output};

/// The `run:` block of the web job's step with `id: recipe`.
fn recipe_step() -> String {
    let path = repo_root().join(".forgejo/workflows/ci.yml");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let doc: yaml_serde::Value = yaml_serde::from_str(&text).expect("ci.yml is YAML");
    let steps = doc["jobs"]["web"]["steps"]
        .as_sequence()
        .expect("ci.yml's web job has steps");
    let step = steps
        .iter()
        .find(|s| s["id"].as_str() == Some("recipe"))
        .expect("ci.yml's web job has a step with `id: recipe`");
    step["run"]
        .as_str()
        .expect("the recipe step has a run block")
        .to_string()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.invalid",
        ])
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A file to write (`Some(body)`) or delete (`None`), at a tree path.
type Change<'a> = (&'a str, Option<&'a str>);

const BASE: [Change<'static>; 3] = [
    ("infra/forge/boss-ci/Dockerfile", Some("FROM rust:1.96\n")),
    (
        "infra/forge/boss-ci/required-tools.txt",
        Some("bun\nnode\n"),
    ),
    ("apps/web/src/a.ts", Some("export const a = 1;\n")),
];

fn apply(tree: &Path, changes: &[Change<'_>]) {
    for (path, body) in changes {
        let file = tree.join(path);
        match body {
            Some(body) => {
                create_dir(file.parent().expect("a tree path has a parent"));
                write_file(&file, body);
            }
            None => std::fs::remove_file(&file)
                .unwrap_or_else(|e| panic!("remove {}: {e}", file.display())),
        }
    }
}

struct Ran {
    out: Output,
    answer: Vec<String>,
}

/// A forge-shaped remote holding `base` then `base + head`, a checkout of
/// the head the way the web job checks out (init, depth-1 fetch by sha,
/// detached), and the recipe step run in it with `BASE_SHA` set to the
/// real base, or to `base_sha` when given.
fn run_step(base: &[Change<'_>], head: &[Change<'_>], base_sha: Option<&str>) -> Ran {
    let dir = scratch_dir("forge-web-recipe");
    let forge = dir.join("forge");
    create_dir(&forge);
    git(&forge, &["init", "-q"]);
    apply(&forge, base);
    git(&forge, &["add", "-A"]);
    git(&forge, &["commit", "-q", "--allow-empty", "-m", "base"]);
    let real_base = git(&forge, &["rev-parse", "HEAD"]);
    apply(&forge, head);
    git(&forge, &["add", "-A"]);
    git(&forge, &["commit", "-q", "--allow-empty", "-m", "head"]);
    let head_sha = git(&forge, &["rev-parse", "HEAD"]);
    // The forge serves a sha it does not advertise; so must the fixture.
    git(&forge, &["config", "uploadpack.allowAnySHA1InWant", "true"]);

    let job = dir.join("job");
    create_dir(&job);
    git(&job, &["init", "-q"]);
    let url = format!("file://{}", forge.display());
    git(&job, &["remote", "add", "origin", &url]);
    git(&job, &["fetch", "-q", "--depth=1", "origin", &head_sha]);
    git(&job, &["checkout", "-q", "--force", "FETCH_HEAD"]);

    let script = dir.join("recipe.sh");
    write_file(&script, &recipe_step());
    let output = dir.join("github-output");
    write_file(&output, "");
    let out = Command::new("bash")
        .arg("-e")
        .arg(&script)
        .current_dir(&job)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", &dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("BASE_SHA", base_sha.unwrap_or(&real_base))
        .env("GITHUB_SHA", &head_sha)
        .env("GITHUB_OUTPUT", &output)
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run bash");
    let answer = std::fs::read_to_string(&output)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect();
    Ran { out, answer }
}

fn said(ran: &Ran) -> String {
    format!(
        "exit {:?}, output {:?}\nstdout: {}\nstderr: {}",
        ran.out.status.code(),
        ran.answer,
        String::from_utf8_lossy(&ran.out.stdout),
        String::from_utf8_lossy(&ran.out.stderr)
    )
}

fn assert_answer(ran: &Ran, want: &str, case: &str) {
    assert!(
        ran.out.status.success(),
        "{case}: the recipe step must answer, never fail the job — {}",
        said(ran)
    );
    assert_eq!(
        ran.answer,
        vec![format!("changed={want}")],
        "{case}: {}",
        said(ran)
    );
    assert!(
        String::from_utf8_lossy(&ran.out.stdout).contains(&format!("changed={want}")),
        "{case}: the step must print its answer in the job log — {}",
        said(ran)
    );
}

#[test]
fn a_train_that_keeps_the_recipe_skips_the_duplicate_web_steps() {
    let ran = run_step(
        &BASE,
        &[
            ("apps/web/src/a.ts", Some("export const a = 2;\n")),
            ("crates/core/x/src/lib.rs", Some("pub fn x() {}\n")),
            (
                "infra/forge/boss-ci-notes.md",
                Some("beside, not in, the recipe\n"),
            ),
        ],
        None,
    );
    assert_answer(
        &ran,
        "no",
        "a train that changes web and Rust but not the recipe",
    );
}

#[test]
fn a_train_that_changes_the_recipe_runs_them() {
    for (case, head) in [
        (
            "the Dockerfile changed",
            vec![("infra/forge/boss-ci/Dockerfile", Some("FROM rust:1.97\n"))],
        ),
        (
            "a file was added to the recipe",
            vec![("infra/forge/boss-ci/bake.sh", Some("#!/bin/sh\n"))],
        ),
        (
            "a file was removed from the recipe",
            vec![("infra/forge/boss-ci/required-tools.txt", None)],
        ),
    ] {
        assert_answer(&run_step(&BASE, &head, None), "yes", case);
    }
}

#[test]
fn a_recipe_it_cannot_read_runs_them() {
    let unchanged: [Change<'_>; 1] = [("apps/web/src/a.ts", Some("export const a = 3;\n"))];
    assert_answer(&run_step(&BASE, &unchanged, Some("")), "yes", "no base sha");
    assert_answer(
        &run_step(&BASE, &unchanged, Some(&"0".repeat(40))),
        "yes",
        "a base the forge does not have",
    );
    assert_answer(
        &run_step(&BASE[2..], &unchanged, None),
        "yes",
        "no recipe directory on either side",
    );
}
