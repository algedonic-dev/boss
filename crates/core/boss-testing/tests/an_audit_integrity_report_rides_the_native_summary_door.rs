//! tree-wide: pins both native audit integrity scheduler declarations.

use boss_testing::{copy_exec, repo_root, scratch_dir, write_exec};
use std::process::Command;

#[test]
fn both_schedulers_bind_the_producer_to_the_existing_native_summary_door() {
    let root = repo_root();
    for relative in [
        "infra/cluster/manifests/boss-audit-integrity.yaml",
        "infra/boss-audit-integrity-check.service",
    ] {
        let text = std::fs::read_to_string(root.join(relative)).unwrap();
        assert!(
            text.contains("BOSS_RUN_SUMMARY_FILE") && text.contains("--summary-file"),
            "{relative} does not carry the structured integrity report through the native summary door"
        );
    }
}

fn systemd_command(name: &str) -> String {
    let source =
        std::fs::read_to_string(repo_root().join("infra/boss-audit-integrity-check.service"))
            .unwrap();
    let line = source
        .lines()
        .find_map(|line| line.strip_prefix(name))
        .unwrap();
    line.strip_prefix("/bin/bash -c '")
        .or_else(|| line.strip_prefix("-/bin/bash -c '"))
        .unwrap()
        .strip_suffix('\'')
        .unwrap()
        .replace("$$", "$")
}

#[test]
fn systemd_start_and_stop_use_the_same_unique_invocation_and_never_reuse_a_prior_report() {
    let scratch = scratch_dir("audit-summary-service");
    let producer = scratch.join("producer");
    write_exec(
        &producer,
        "#!/bin/bash\nset -eu\nwhile [ $# -gt 0 ]; do if [ \"$1\" = --summary-file ]; then shift; printf '{\"audit_integrity\":{\"version\":1,\"invocation\":\"%s\"}}' \"$INVOCATION_ID\" > \"$1\"; fi; shift; done\n",
    );
    let step = scratch.join("boss-step.sh");
    copy_exec(&repo_root().join("infra/boss-step.sh"), &step);
    let start = systemd_command("ExecStart=")
        .replace("/run/boss-audit-integrity", scratch.to_str().unwrap())
        .replace(
            "/usr/local/bin/boss-audit-integrity-check",
            producer.to_str().unwrap(),
        );
    let stop = systemd_command("ExecStopPost=")
        .replace("/run/boss-audit-integrity", scratch.to_str().unwrap())
        .replace(
            "/opt/boss/infra/boss-step.sh",
            &format!("bash {}", step.display()),
        );
    let old = "11111111111111111111111111111111";
    let current = "22222222222222222222222222222222";
    let previous = scratch.join(format!("{old}.json"));
    std::fs::write(&previous, "{\"audit_integrity\":{\"stale\":true}}").unwrap();
    let ran = Command::new("bash")
        .args(["-c", &start])
        .env("INVOCATION_ID", current)
        .output()
        .unwrap();
    assert!(
        ran.status.success(),
        "{}",
        String::from_utf8_lossy(&ran.stderr)
    );
    let reported = Command::new("bash")
        .args(["-c", &stop])
        .env("INVOCATION_ID", current)
        .env("BOSS_STEP_DRY_RUN", "1")
        .output()
        .unwrap();
    assert!(
        reported.status.success(),
        "{}",
        String::from_utf8_lossy(&reported.stderr)
    );
    let stdout = String::from_utf8_lossy(&reported.stdout);
    assert!(
        stdout.contains(current) && !stdout.contains("stale"),
        "{stdout}"
    );
    assert!(
        !scratch.join(format!("{current}.json")).exists(),
        "summary is consumed once"
    );
    assert!(previous.exists(), "an earlier invocation is not consumed");
    let missing = Command::new("bash")
        .args(["-c", &stop])
        .env("INVOCATION_ID", "33333333333333333333333333333333")
        .env("BOSS_STEP_DRY_RUN", "1")
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&missing.stdout).contains("summary_absent"));
    for command in [&start, &stop] {
        let refused = Command::new("bash")
            .args(["-c", command])
            .env_remove("INVOCATION_ID")
            .output()
            .unwrap();
        assert_eq!(refused.status.code(), Some(78));
    }
}

#[test]
fn cron_reports_typed_chain_and_drift_on_success_failure_and_missing_data_without_reusing_files() {
    let scratch = scratch_dir("audit-summary-cron");
    let manifest = std::fs::read_to_string(
        repo_root().join("infra/cluster/manifests/boss-audit-integrity.yaml"),
    )
    .unwrap();
    let code: String = manifest
        .split("                - |\n")
        .nth(1)
        .unwrap()
        .lines()
        .take_while(|line| line.starts_with("                  ") || line.is_empty())
        .map(|line| {
            format!(
                "{}\n",
                line.strip_prefix("                  ").unwrap_or(line)
            )
        })
        .collect();
    let chore = scratch.join("boss-chore.sh");
    write_exec(
        &chore,
        &std::fs::read_to_string(repo_root().join("infra/boss-chore.sh")).unwrap(),
    );
    write_exec(
        &scratch.join("boss-step.sh"),
        &std::fs::read_to_string(repo_root().join("infra/boss-step.sh")).unwrap(),
    );
    write_exec(
        &scratch.join("boss-maintenance-wrap.sh"),
        "#!/bin/bash\nexit 0\n",
    );
    let producer = scratch.join("producer");
    write_exec(
        &producer,
        "#!/bin/bash\nset -eu\ntest \"$1\" = --summary-file\ntest \"$2\" = \"$BOSS_RUN_SUMMARY_FILE\"\nprintf '%s\\n' \"$2\" >> \"$PATH_LOG\"\nif [ \"$TEST_REPORT\" != absent ]; then printf '%s' \"$TEST_REPORT\" > \"$2\"; fi\nexit \"$TEST_EXIT\"\n",
    );
    let command = code
        .replace("/usr/local/bin/boss-chore.sh", &chore.to_string_lossy())
        .replace(
            "/usr/local/bin/boss-audit-integrity-check",
            &producer.to_string_lossy(),
        );
    let paths = scratch.join("paths.log");
    let stale = scratch.join("old-run.json");
    std::fs::write(&stale, "{\"audit_integrity\":{\"stale\":true}}").unwrap();
    let typed = "{\"audit_integrity\":{\"version\":1,\"chain\":{\"state\":\"intact\"},\"drift\":{\"state\":\"unavailable\",\"error\":\"registry refused\"}}}";
    for (report, code) in [
        (typed, "0"),
        (typed, "2"),
        ("absent", "1"),
        ("not-json", "0"),
    ] {
        let ran = Command::new("bash")
            .args(["-c", &command])
            .env("TMPDIR", &scratch)
            .env("BOSS_RUN_SUMMARY_FILE", &stale)
            .env("BOSS_STEP_DRY_RUN", "1")
            .env("PATH_LOG", &paths)
            .env("TEST_REPORT", report)
            .env("TEST_EXIT", code)
            .output()
            .unwrap();
        assert_eq!(
            ran.status.code().unwrap().to_string(),
            code,
            "{}",
            String::from_utf8_lossy(&ran.stderr)
        );
        let stdout = String::from_utf8_lossy(&ran.stdout);
        if report == typed {
            assert!(
                stdout.contains("\"chain\":{\"state\":\"intact\"}")
                    && stdout.contains("\"drift\":{\"state\":\"unavailable\""),
                "{stdout}"
            );
        } else {
            assert!(stdout.contains("summary_absent"), "{stdout}");
        }
        assert!(
            !stdout.contains("stale"),
            "previous evidence is never credited: {stdout}"
        );
    }
    let paths: Vec<String> = std::fs::read_to_string(paths)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(paths.len(), 4);
    assert_eq!(
        paths
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        4
    );
    assert!(
        paths
            .iter()
            .all(|path| !std::path::Path::new(path).exists()),
        "all reports consumed/cleaned"
    );
    assert!(
        stale.exists(),
        "the inherited prior path was not read or removed"
    );
}
