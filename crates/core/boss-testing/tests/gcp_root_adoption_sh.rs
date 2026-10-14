//! Adoption starts from authenticated root bytes and installs no unit on refusal.
// not a tree-wide pin: owns only the GCP adoption/launcher contract below.
use boss_testing::{copy_exec, create_dir, repo_root, scratch_dir, write_exec, write_file};
use std::process::Command;

fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
    create_dir(to);
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &to.join(entry.file_name()));
        } else if entry.file_type().unwrap().is_file() {
            copy_exec(&entry.path(), &to.join(entry.file_name()));
        }
    }
}

#[test]
fn a_failed_root_refresh_keeps_old_units_and_never_runs_the_checkout() {
    let dir = scratch_dir("gcp-root-adoption-refusal");
    let prepared = dir.join("prepared/bootstrap/infra/gcp");
    create_dir(&prepared);
    let log = dir.join("called");
    let summary = dir.join("summary.json");
    write_file(&summary, "{\"converge_sha\":\"prior-generation\"}\n");
    write_exec(
        &prepared.join("root-tree.sh"),
        "#!/bin/bash\nprintf 'refresh-refused\\n' >> \"$CALL_LOG\"\nexit 78\n",
    );
    let out = Command::new("bash")
        .arg(repo_root().join("infra/gcp/gcp-root-launch.sh"))
        .arg("converge")
        .env("BOSS_GCP_PREPARED_DIR", dir.join("prepared"))
        .env("BOSS_GCP_ROOT_TREE", dir.join("tree"))
        .env("CALL_LOG", &log)
        .env("BOSS_RUN_SUMMARY_FILE", &summary)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(78), "{out:?}");
    assert_eq!(std::fs::read_to_string(log).unwrap(), "refresh-refused\n");
    assert!(!dir.join("units").exists());
    assert!(
        !summary.exists(),
        "a refresh refusal must not pass the prior summary to ExecStopPost"
    );
    // Same record boundary as the generation's shared reset, deliberately
    // usable before that generation can be fetched or executed.
    write_file(&summary, "prior summary\n");
    let reference = Command::new("bash")
        .args(["-c", ". \"$REFERENCE_LIB\"; run_summary_reset"])
        .env("REFERENCE_LIB", repo_root().join("infra/run-summary.sh"))
        .env("BOSS_RUN_SUMMARY_FILE", &summary)
        .output()
        .unwrap();
    assert!(reference.status.success(), "{reference:?}");
    assert!(!summary.exists());
}

#[test]
fn the_stop_post_run_preserves_its_summary_for_the_reader() {
    let dir = scratch_dir("gcp-root-summary-reader");
    let prepared = dir.join("prepared/bootstrap/infra/gcp");
    create_dir(&prepared);
    write_exec(&prepared.join("root-tree.sh"), "#!/bin/bash\nexit 0\n");
    let summary = dir.join("summary.json");
    write_file(&summary, "current summary\n");
    let out = Command::new("bash")
        .arg(repo_root().join("infra/gcp/gcp-root-launch.sh"))
        .args([
            "run",
            "infra/boss-step.sh",
            "maintenance-boss-gcp-converge",
            "run",
        ])
        .env("BOSS_GCP_PREPARED_DIR", dir.join("prepared"))
        .env("BOSS_RUN_SUMMARY_FILE", &summary)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        std::fs::read_to_string(summary).unwrap(),
        "current summary\n"
    );
}

#[test]
fn silence_is_not_a_completed_credential_lifecycle() {
    let dir = scratch_dir("gcp-root-lifecycle-silence");
    let bin = dir.join("bin");
    create_dir(&bin);
    write_exec(&bin.join("curl"), "#!/bin/bash\nprintf '  \\n'\nexit 0\n");
    let out = Command::new("bash")
        .arg(repo_root().join("infra/gcp/adopt-root-tree.sh"))
        .args(["bootstrap", "fixture"])
        .env("BOSS_GCP_PREPARED_DIR", dir.join("prepared"))
        .env("BOSS_GCP_ROOT_TREE", dir.join("tree"))
        .env("BOSS_FORGE_URL", "http://forge.invalid")
        .env("BOSS_JOBS_URL", "http://sor.invalid")
        .env("BOSS_MACHINE_TOKEN_DIR", dir.join("absent-token"))
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(78), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("unfinished-human-lifecycle"),
        "{out:?}"
    );
    assert!(!dir.join("prepared").exists());
}

#[test]
fn the_launcher_uses_the_protected_tree_run_door_after_refresh() {
    let dir = scratch_dir("gcp-root-adoption-launch");
    let prepared = dir.join("prepared/bootstrap/infra/gcp");
    create_dir(&prepared);
    let log = dir.join("called");
    write_exec(
        &prepared.join("root-tree.sh"),
        "#!/bin/bash\nprintf '%s\\n' \"$*\" >> \"$CALL_LOG\"\nexit 0\n",
    );
    write_file(&dir.join("prepared/bootstrap/deposit-user"), "david\n");
    let out = Command::new("bash")
        .arg(repo_root().join("infra/gcp/gcp-root-launch.sh"))
        .arg("converge")
        .env("BOSS_GCP_PREPARED_DIR", dir.join("prepared"))
        .env("BOSS_GCP_ROOT_TREE", dir.join("tree"))
        .env("CALL_LOG", &log)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        std::fs::read_to_string(log).unwrap(),
        "refresh\nrun infra/gcp/boss-gcp-converge.sh --root-generation\n"
    );
}

#[test]
fn unfinished_human_lifecycle_refuses_bootstrap_before_any_fetch_or_unit() {
    let dir = scratch_dir("gcp-root-lifecycle-refusal");
    let bin = dir.join("bin");
    create_dir(&bin);
    write_exec(
        &bin.join("curl"),
        "#!/bin/bash\nfor arg; do case $arg in */api/jobs*) printf '{\"total\":1,\"data\":[{\"status\":\"open\"}]}'; exit 0;; esac; done\necho 'unexpected fetch' >&2\nexit 99\n",
    );
    let out = Command::new("bash")
        .arg(repo_root().join("infra/gcp/adopt-root-tree.sh"))
        .arg("bootstrap")
        .arg("david")
        .env("BOSS_GCP_PREPARED_DIR", dir.join("prepared"))
        .env("BOSS_GCP_ROOT_TREE", dir.join("tree"))
        .env("BOSS_FORGE_URL", "http://forge.invalid")
        .env("BOSS_JOBS_URL", "http://sor.invalid")
        .env("BOSS_MACHINE_TOKEN_DIR", dir.join("absent-token"))
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(78), "{out:?}");
    assert!(!dir.join("tree").exists());
    assert!(!dir.join("prepared/bootstrap").exists());
}

#[test]
fn an_unprepared_root_generation_cannot_overwrite_any_installed_unit() {
    let dir = scratch_dir("gcp-root-unit-preparation-refusal");
    let source = dir.join("source");
    for name in ["gcp", "lib", "estate", "ops", "ml"] {
        copy_dir(
            &repo_root().join("infra").join(name),
            &source.join("infra").join(name),
        );
    }
    for name in [
        "run-summary.sh",
        "journal-door-ensure.sh",
        "boss-codebase-metrics.service",
        "boss-codebase-metrics.timer",
        "boss-protocol-drift.service",
        "boss-protocol-drift.timer",
        "boss-surface-usage.service",
        "boss-surface-usage.timer",
    ] {
        std::fs::copy(
            repo_root().join("infra").join(name),
            source.join("infra").join(name),
        )
        .unwrap();
    }
    let etc = dir.join("etc");
    create_dir(&etc);
    write_file(
        &etc.join("boss-codebase-metrics.service"),
        "old unit bytes\n",
    );
    let bin = dir.join("bin");
    create_dir(&bin);
    for name in ["systemctl", "apt-get"] {
        write_exec(&bin.join(name), "#!/bin/sh\nexit 0\n");
    }
    let out = Command::new("bash")
        .arg(source.join("infra/gcp/install-units.sh"))
        .arg("units")
        .env("BOSS_REPO_ROOT", &source)
        .env(
            "BOSS_GCP_ACCEPTED_SHA",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )
        .env("BOSS_GCP_PREPARED_DIR", dir.join("missing-preparation"))
        .env(
            "BOSS_NODE_ROLES",
            "off-cluster-observer,ml-batch-host,ops-runner",
        )
        .env("INSTALL_ETC", &etc)
        .env("INSTALL_SYSTEMCTL", bin.join("systemctl"))
        .env("INSTALL_APT_GET", bin.join("apt-get"))
        .env("BOSS_OPS_RUNNER_RETIRED", dir.join("retired"))
        .env("BOSS_JOBS_URL", "http://sor.invalid")
        .env("BOSS_SOR_ENV", dir.join("absent.env"))
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(78), "{out:?}");
    assert_eq!(
        std::fs::read_to_string(etc.join("boss-codebase-metrics.service")).unwrap(),
        "old unit bytes\n"
    );
}

#[test]
fn root_generation_mode_refuses_a_script_outside_the_accepted_tree() {
    let dir = scratch_dir("gcp-root-generation-source-refusal");
    let out = Command::new("bash")
        .arg(repo_root().join("infra/gcp/boss-gcp-converge.sh"))
        .arg("--root-generation")
        .env("BOSS_GCP_PREPARED_DIR", dir.join("missing-preparation"))
        .env("BOSS_GCP_ROOT_TREE", dir.join("missing-tree"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(78), "{out:?}");
    assert!(!dir.join("missing-tree").exists());
}

#[test]
fn the_read_only_proof_refuses_missing_tree_without_reading_credentials() {
    let dir = scratch_dir("gcp-root-status-missing");
    let out = Command::new("bash")
        .arg(repo_root().join("infra/gcp/root-tree-status.sh"))
        .env("BOSS_GCP_ROOT_TREE", dir.join("missing-tree"))
        .env("BOSS_GCP_PREPARED_DIR", dir.join("missing-preparation"))
        .env(
            "BOSS_GCP_STATUS_SYSTEMCTL",
            dir.join("never-called-systemctl"),
        )
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(78), "{out:?}");
    assert!(!dir.join("missing-tree").exists());
    assert!(String::from_utf8_lossy(&out.stderr).contains("UNVERIFIED"));
}

#[test]
fn partial_preparation_keeps_units_and_complete_preparation_migrates_doors_first() {
    let dir = scratch_dir("gcp-root-install-order");
    let source = dir.join("source");
    for name in ["gcp", "lib", "estate"] {
        copy_dir(
            &repo_root().join("infra").join(name),
            &source.join("infra").join(name),
        );
    }
    copy_exec(
        &repo_root().join("infra/run-summary.sh"),
        &source.join("infra/run-summary.sh"),
    );
    write_exec(
        &source.join("infra/journal-door-ensure.sh"),
        "#!/bin/sh\nexit 0\n",
    );
    write_file(
        &source.join("infra/estate/roles.toml"),
        "[always]\nunits = [\"boss-gcp-converge\", \"boss-codebase-metrics\", \"boss-estate-observe-host\"]\n",
    );
    for ext in ["service", "timer"] {
        std::fs::copy(
            repo_root().join(format!("infra/boss-codebase-metrics.{ext}")),
            source.join(format!("infra/boss-codebase-metrics.{ext}")),
        )
        .unwrap();
    }
    create_dir(&source.join("infra/ops"));
    write_exec(
        &source.join("infra/ops/install-ops-runner.sh"),
        "#!/bin/bash\nset -eu\ngrep -q '/gcp-root-launch.sh converge' \"$INSTALL_ETC/boss-gcp-converge.service\"\ngrep -qx 'old unit bytes' \"$INSTALL_ETC/boss-codebase-metrics.service\"\nprintf '%s\\n' \"$INSTALL_OPS_RUNNER_TREE_HELPER\" > \"$ORDER_LOG\"\n",
    );
    let etc = dir.join("etc");
    create_dir(&etc);
    write_file(
        &etc.join("boss-codebase-metrics.service"),
        "old unit bytes\n",
    );
    let units = dir.join("prepared/units");
    create_dir(&units);
    let sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    write_file(&units.join("accepted-sha"), sha);
    for entry in std::fs::read_dir(source.join("infra/gcp/root-tree-units")).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().into_string().unwrap();
        if name.ends_with(".service.in") && !name.starts_with("boss-codebase-metrics") {
            std::fs::copy(entry.path(), units.join(name.trim_end_matches(".in"))).unwrap();
        }
    }
    let systemctl = dir.join("systemctl");
    write_exec(&systemctl, "#!/bin/sh\nexit 0\n");
    let run = || {
        Command::new("bash")
            .arg(source.join("infra/gcp/install-units.sh"))
            .arg("units")
            .env("BOSS_REPO_ROOT", &source)
            .env("BOSS_GCP_ACCEPTED_SHA", sha)
            .env("BOSS_GCP_PREPARED_DIR", dir.join("prepared"))
            .env("INSTALL_ETC", &etc)
            .env("INSTALL_SYSTEMCTL", &systemctl)
            .env("INSTALL_APT_GET", dir.join("apt-get"))
            .env("BOSS_OPS_RUNNER_RETIRED", dir.join("retired"))
            .env("ORDER_LOG", dir.join("order"))
            .output()
            .unwrap()
    };
    let partial = run();
    assert_eq!(partial.status.code(), Some(78), "{partial:?}");
    assert!(!dir.join("order").exists());
    assert_eq!(
        std::fs::read_to_string(etc.join("boss-codebase-metrics.service")).unwrap(),
        "old unit bytes\n"
    );
    std::fs::copy(
        source.join("infra/gcp/root-tree-units/boss-codebase-metrics.service.in"),
        units.join("boss-codebase-metrics.service"),
    )
    .unwrap();
    let complete = run();
    assert!(complete.status.success(), "{complete:?}");
    assert!(
        std::fs::read_to_string(dir.join("order"))
            .unwrap()
            .contains("/bootstrap/infra/gcp/gcp-root-launch.sh")
    );
    assert_eq!(
        std::fs::read(etc.join("boss-codebase-metrics.service")).unwrap(),
        std::fs::read(units.join("boss-codebase-metrics.service")).unwrap()
    );
    let observer = std::fs::read_to_string(etc.join("boss-estate-observe-host.service")).unwrap();
    assert!(
        observer
            .contains("/bootstrap/infra/gcp/gcp-root-launch.sh run infra/estate/observe-host.sh"),
        "{observer}"
    );
    assert!(!observer.contains("ExecStart=/opt/boss"));
}
