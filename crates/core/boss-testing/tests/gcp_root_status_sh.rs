//! The actual six-caller reader distinguishes systemd's empty default from silence.
use boss_testing::{create_dir, repo_root, scratch_dir, write_exec, write_file};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn git(path: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(path)
        .args([
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.invalid",
        ])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

struct Case {
    root: PathBuf,
}
impl Case {
    fn new() -> Self {
        let root = scratch_dir("gcp-root-status");
        create_dir(&root.join("tree"));
        git(&root, &["init", "--bare", "tree/repo.git"]);
        let repository = root.join("tree/repo.git");
        let tree = git(
            &repository,
            &["hash-object", "-w", "-t", "tree", "/dev/null"],
        );
        let sha = git(&repository, &["commit-tree", &tree, "-m", "fixture"]);
        git(&repository, &["update-ref", "refs/boss/accepted", &sha]);
        write_file(&root.join("tree/.lock"), "");
        create_dir(&root.join("prepared/bootstrap/infra/gcp"));
        write_exec(
            &root.join("prepared/bootstrap/infra/gcp/root-tree.sh"),
            &format!("#!/bin/sh\nprintf '%s\\n' '{sha}'\n"),
        );
        write_exec(
            &root.join("systemctl"),
            r#"#!/bin/bash
printf '%s\n' "$*" >> "$CALLS"
for argument; do case "$argument" in *.service) unit=$argument ;; esac; done
user=root
if [ "$unit" = boss-ops-runner.service ]; then user=${CASE_USER-}; fi
case "${CASE_MODE:-}" in
  missing-user) [ "$unit" != boss-ops-runner.service ] && printf 'User=%s\n' "$user" ;;
  duplicate-user) printf 'User=%s\nUser=%s\n' "$user" "$user" ;;
  *) printf 'User=%s\n' "$user" ;;
esac
printf 'LoadState=%s\nActiveState=inactive\n' "${CASE_LOAD:-loaded}"
case "$unit" in
  boss-gcp-converge.service|boss-ops-runner.service) path="$BOSS_GCP_PREPARED_DIR/bootstrap/infra/gcp/gcp-root-launch.sh" ;;
  *) path="$BOSS_GCP_ROOT_TREE/current/infra/gcp/root-tree.sh" ;;
esac
printf 'ExecStart={ path=%s ; argv[]=NEVER-PRINT-ARGUMENT ; }\n' "${CASE_EXEC:-$path}"
printf 'FragmentPath=/etc/systemd/system/%s\n' "$unit"
[ "${CASE_MODE:-}" != missing-dynamic ] && printf 'DynamicUser=%s\n' "${CASE_DYNAMIC-no}"
exit 0
"#,
        );
        Self { root }
    }
    fn run(&self, env: &[(&str, &str)]) -> Output {
        let mut command = Command::new("bash");
        command
            .arg(repo_root().join("infra/gcp/root-tree-status.sh"))
            .env("BOSS_GCP_ROOT_TREE", self.root.join("tree"))
            .env("BOSS_GCP_PREPARED_DIR", self.root.join("prepared"))
            .env("BOSS_GCP_STATUS_SYSTEMCTL", self.root.join("systemctl"))
            .env("CALLS", self.root.join("calls"));
        for (key, value) in env {
            command.env(key, value);
        }
        command.output().unwrap()
    }
}

#[test]
fn the_installed_ops_unit_without_user_uses_the_system_manager_root_default() {
    let template =
        std::fs::read_to_string(repo_root().join("infra/ops/boss-ops-runner.service")).unwrap();
    assert!(!template.lines().any(|line| line.starts_with("User=")));
    let case = Case::new();
    let out = case.run(&[]);
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8(out.stdout).unwrap();
    assert_eq!(
        text.lines()
            .filter(|line| line.contains("verdict=adopted"))
            .count(),
        6
    );
    assert!(text.contains("unit=boss-ops-runner user=root"));
    assert!(!text.contains("NEVER-PRINT-ARGUMENT"));
    let calls = std::fs::read_to_string(case.root.join("calls")).unwrap();
    assert!(
        calls
            .lines()
            .all(|line| line.contains("--system") && line.contains("--all"))
    );
}

#[test]
fn explicit_root_and_inactive_oneshots_remain_adopted() {
    assert!(Case::new().run(&[("CASE_USER", "root")]).status.success());
}

#[test]
fn empty_user_never_turns_missing_malformed_or_dynamic_properties_into_a_pass() {
    for env in [
        vec![("CASE_MODE", "missing-user")],
        vec![("CASE_MODE", "duplicate-user")],
        vec![("CASE_MODE", "missing-dynamic")],
        vec![("CASE_USER", "root bad")],
        vec![("CASE_DYNAMIC", "yes")],
        vec![("CASE_DYNAMIC", "")],
    ] {
        let out = Case::new().run(&env);
        assert_eq!(out.status.code(), Some(78), "{env:?}: {out:?}");
    }
}

#[test]
fn default_root_does_not_adopt_an_unloaded_foreign_user_or_wrong_executable() {
    for env in [
        vec![("CASE_LOAD", "not-found")],
        vec![("CASE_USER", "david")],
        vec![("CASE_EXEC", "/opt/boss/infra/ops/ops-runner.sh")],
    ] {
        let out = Case::new().run(&env);
        assert!(!out.status.success(), "{env:?}: {out:?}");
    }
}
