//! Execute the real build script. An indirect forcing key must still name
//! an absent path, and each invocation must stamp its actual branch head.

use boss_testing::{repo_root, scratch_dir, write_file};
use std::path::Path;
use std::process::Command;

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.invalid",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}

fn build_output(existing_trigger: bool) -> (String, String, bool) {
    let root = scratch_dir("cli-effective-build-stamp");
    let crate_dir = root.join("crates/orchestrators/boss-cli");
    for path in [
        "infra/ops/verbs",
        "infra/platform/workflows",
        "crates/orchestrators/boss-cli/src",
    ] {
        std::fs::create_dir_all(root.join(path)).unwrap();
    }
    write_file(&root.join("infra/ops/verbs/fixture.json"), "{}");
    write_file(
        &root.join("infra/platform/workflows/fixture.toml"),
        "kind='fixture'",
    );
    for name in ["build.rs", "workflow_roster.rs"] {
        let source =
            std::fs::read_to_string(repo_root().join("crates/orchestrators/boss-cli").join(name))
                .unwrap();
        write_file(&crate_dir.join(name), &source);
    }
    write_file(
        &crate_dir.join("Cargo.toml"),
        "[package]\nname='stamp-fixture'\nversion='0.1.0'\nedition='2024'\n[workspace]\n",
    );
    write_file(
        &crate_dir.join("src/main.rs"),
        "fn main() { println!(\"{}\", env!(\"BOSS_CLI_BUILT_FROM\")); }\n",
    );
    git(&root, &["init", "-q", "-b", "main"]);
    git(
        &root,
        &["commit", "-q", "--allow-empty", "-m", "first head"],
    );
    let invoke = || {
        // A dependency-free crate runs Cargo's real incremental decision,
        // with its own target and one job, never the workspace's target.
        let output = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
            .current_dir(&crate_dir)
            .args(["run", "--offline", "--quiet", "--jobs", "1"])
            .env("CARGO_TARGET_DIR", root.join("target"))
            .env_remove("CARGO_MAKEFLAGS")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    let first = invoke();
    assert_eq!(first.trim(), git(&root, &["rev-parse", "HEAD"]));
    let records = std::fs::read_dir(root.join("target/debug/build"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("output"))
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 1);
    let emitted = std::fs::read_to_string(&records[0]).unwrap();
    let triggers = emitted
        .lines()
        .filter_map(|l| l.strip_prefix("cargo:rerun-if-changed="))
        .filter(|p| !crate_dir.join(p).exists())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert_eq!(
        triggers.len(),
        1,
        "exactly one absent forcing input: {emitted}"
    );
    let trigger = &triggers[0];
    if existing_trigger {
        write_file(&crate_dir.join(trigger), "adverse: the forcing key exists");
        // Settle that file's creation before moving HEAD. With an existing
        // key Cargo now skips the build script and reproduces the old defect.
        assert_eq!(first, invoke());
    }
    git(
        &root,
        &["commit", "-q", "--allow-empty", "-m", "second head"],
    );
    let second = invoke();
    if existing_trigger {
        assert_eq!(
            first, second,
            "existing-key control must reproduce the stale stamp"
        );
        assert_ne!(second.trim(), git(&root, &["rev-parse", "HEAD"]));
    } else {
        assert_ne!(first, second, "the new head must change its stamp");
        assert_eq!(second.trim(), git(&root, &["rev-parse", "HEAD"]));
    }
    (first, second, !crate_dir.join(trigger).exists())
}

#[test]
fn the_cli_build_script_forces_a_rerun_every_build() {
    assert!(build_output(false).2);
}

#[test]
fn a_forcing_key_that_exists_is_not_an_every_build_trigger() {
    assert!(
        !build_output(true).2,
        "the adverse control must detect a non-forcing input"
    );
}
