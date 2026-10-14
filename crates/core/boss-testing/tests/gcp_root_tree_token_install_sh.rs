//! The forced-command receiver must execute a whole root-owned closure.
use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

#[test]
fn installation_copies_the_whole_closure_and_grants_only_the_no_argument_receiver() {
    let dir = scratch_dir("gcp-root-tree-install");
    let libexec = dir.join("libexec");
    let sudoers = dir.join("sudoers");
    std::fs::create_dir(&sudoers).unwrap();
    let visudo = dir.join("visudo");
    write_exec(&visudo, "#!/bin/sh\n[ \"$1\" = -cf ] && [ -s \"$2\" ]\n");
    let out = Command::new("bash")
        .arg(repo_root().join("infra/gcp/install-root-tree-token-receiver.sh"))
        .env("INSTALL_RECV_LIBEXEC", &libexec)
        .env("INSTALL_RECV_SUDOERS_DIR", &sudoers)
        .env("INSTALL_VISUDO", &visudo)
        .env("INSTALL_RECV_USER", "fixture")
        .env("INSTALL_RECV_OWNER", "")
        .output()
        .unwrap();
    assert!(out.status.success(), "{:?}", out);
    let installed = libexec.join("root-tree-delivery/infra");
    for path in [
        "gcp/root-tree-token-recv.sh",
        "gcp/root-tree-credential.sh",
        "lib/host-check.sh",
        "lib/jq.sh",
        "lib/sor.sh",
        "lib/sor-reader.sh",
        "lib/secret-header.sh",
        "estate/estate.toml",
    ] {
        assert_eq!(
            std::fs::read(installed.join(path)).unwrap(),
            std::fs::read(repo_root().join("infra").join(path)).unwrap()
        );
    }
    let grant = std::fs::read_to_string(sudoers.join("boss-root-tree-token-recv")).unwrap();
    assert!(grant.contains("env_reset") && grant.contains("secure_path="));
    assert!(grant.contains("NOPASSWD:") && grant.contains("root-tree-token-recv.sh \"\""));
    assert!(!grant.contains("/opt/boss") && !grant.contains("authorized_keys"));
    assert_eq!(
        std::fs::metadata(installed.join("gcp/root-tree-token-recv.sh"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
}

#[test]
fn broker_preparation_is_named_and_bound_to_only_the_root_tree_receiver() {
    let rule = std::fs::read_to_string(
        repo_root().join("infra/dispatcher/rules/broker-prepares-the-root-tree-deposit-key.toml"),
    )
    .unwrap();
    let manifest = std::fs::read_to_string(
        repo_root().join("infra/cluster/manifests/boss-credential-broker.yaml"),
    )
    .unwrap();
    let sender =
        std::fs::read_to_string(repo_root().join("infra/gcp/root-tree-token-send.sh")).unwrap();
    assert!(rule.contains("credential.prepare.ssh-deposit"));
    for text in [
        "root-tree-deposit-key",
        "root-tree credential deposit",
        "exec sudo -n /usr/local/libexec/boss/root-tree-delivery/infra/gcp/root-tree-token-recv.sh",
    ] {
        assert!(rule.contains(text), "missing broker binding {text}");
        assert!(
            sender.contains(text),
            "sender drifted from broker binding {text}"
        );
    }
    assert!(manifest.contains("resourceNames: [runner-deposit-key, machine-token-deposit-key]"));
    assert!(manifest.contains("resourceNames: [root-tree-deposit-key]"));
    assert!(rule.contains("protocol_kind") && rule.contains("prepare-the-root-tree-deposit-key"));
}

#[test]
fn converges_install_and_send_without_extending_the_existing_push_deadline() {
    let gcp = std::fs::read_to_string(repo_root().join("infra/gcp/boss-gcp-converge.sh")).unwrap();
    let forge = std::fs::read_to_string(repo_root().join("infra/forge/forge-converge.sh")).unwrap();
    let push =
        std::fs::read_to_string(repo_root().join("infra/forge/root-tree-token-push.sh")).unwrap();
    assert!(gcp.contains("gcp/install-root-tree-token-receiver.sh"));
    assert!(gcp.contains("run_summary_field root_tree_token_receiver"));
    let launched = forge.find("ROOT_TREE_PUSH_PID=$!").unwrap();
    let runner = forge.find("timeout -k 5 \"$FORGE_RUNNER_PUSH_BOUND_S\" \"$INFRA/forge/runner-credential-push.sh\"").unwrap();
    let waited = forge.find("wait \"$ROOT_TREE_PUSH_PID\"").unwrap();
    assert!(launched < runner && runner < waited);
    assert!(forge.contains("BOSS_RUN_SUMMARY_FILE='' timeout -k 5 \"$FORGE_RUNNER_PUSH_BOUND_S\""));
    assert!(push.contains("root-tree-deposit-key") && push.contains("root-tree-token-send.sh"));
    assert!(push.contains("--log-driver=none"));
    assert!(forge.contains("[ \"$root_tree_push_rc\" -eq 0 ] || exit \"$root_tree_push_rc\""));
}

#[test]
fn the_deposit_account_does_not_follow_the_root_owned_source_tree() {
    let installer =
        std::fs::read_to_string(repo_root().join("infra/gcp/install-root-tree-token-receiver.sh"))
            .unwrap();
    assert!(installer.contains("BOSS_GCP_DEPOSIT_CHECKOUT:-/opt/boss"));
    assert!(!installer.contains("stat -c %U \"$INFRA/..\""));
}

#[test]
fn both_pushes_can_start_together_and_root_delivery_failure_stays_a_failure() {
    let dir = scratch_dir("gcp-root-tree-parallel-push");
    let forge_dir = dir.join("infra/forge");
    std::fs::create_dir_all(&forge_dir).unwrap();
    for (name, own, other, code) in [
        ("root-tree-token-push.sh", "root", "runner", 7),
        ("runner-credential-push.sh", "runner", "root", 0),
    ] {
        let path = forge_dir.join(name);
        write_exec(
            &path,
            &format!(
                "#!/bin/sh\ntouch \"$MARKER_DIR/{own}\"\nwhile [ ! -f \"$MARKER_DIR/{other}\" ]; do sleep 0.01; done\necho {own}-result\nexit {code}\n"
            ),
        );
    }
    let source =
        std::fs::read_to_string(repo_root().join("infra/forge/forge-converge.sh")).unwrap();
    let start = source.find("FORGE_RUNNER_PUSH_BOUND_S=\"").unwrap();
    let end = source[start..].find("# THE OFF-SITE COPY").unwrap() + start;
    let check = source
        .lines()
        .find(|l| l.starts_with("[ \"$root_tree_push_rc\" -eq 0 ] || exit"))
        .unwrap();
    let snippet = dir.join("pushes.sh");
    write_file(
        &snippet,
        &format!(
            "#!/bin/bash\nset -euo pipefail\nrun_summary_field() {{ printf '%s=%s\\n' \"$1\" \"$2\"; }}\n{}\n[ \"$gcp_runner_rc\" -eq 0 ] || exit \"$gcp_runner_rc\"\n{check}\n",
            &source[start..end]
        ),
    );
    let out = Command::new("bash")
        .arg(&snippet)
        .env("INFRA", dir.join("infra"))
        .env("MARKER_DIR", &dir)
        .env("BOSS_FORGE_RUNNER_PUSH_BOUND_S", "2")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(7), "{:?}", out);
    let output = String::from_utf8(out.stdout).unwrap();
    assert!(output.contains("runner_credential_boss_gcp_status=0"));
    assert!(output.contains("root_tree_token_boss_gcp_status=7"));
    assert!(output.contains("root-result"));
}
