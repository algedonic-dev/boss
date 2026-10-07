//! The fixed retained-source adapter is tested with credential-free native fixtures.
//! Readable retained files never establish a complete or clean historical interval.
use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use serde_json::{Value, json};
use std::process::Command;

#[test]
fn retained_access_refuses_missing_bounds_and_keeps_protected_bytes_inside() {
    let output = Command::new("python3")
        .arg("-B")
        .arg(repo_root().join("infra/forge/tests/admission-access-test.py"))
        .output()
        .expect("python3 is required; no evidence is not a pass");
    assert!(
        output.status.success(),
        "actual retained-access adapter failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn retained_streams_conserve_the_approved_work_budget_and_reap_native_children() {
    let output = Command::new("python3")
        .arg("-B")
        .arg(repo_root().join("infra/forge/tests/admission-stream-test.py"))
        .output()
        .expect("python3 is required; no evidence is not a pass");
    assert!(
        output.status.success(),
        "actual streaming adapter failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn the_no_argument_host_door_refuses_raw_reports_and_keeps_container_bounds() {
    let root = repo_root();
    let scratch = scratch_dir("admission-access-boundary");
    let bin = scratch.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    write_file(&scratch.join("estate.json"), r#"{"data":[]}"#);
    write_exec(
        &bin.join("sudo"),
        r#"#!/bin/sh
case "$*" in
 '-n docker image inspect '*|'-n docker create '*) exit 0 ;;
 '-n docker container inspect '*) exit 1 ;;
esac
printf '%s\n' "$*" > "$CALLS"
cat > "$INPUT"
echo 'PRIVATE-NATIVE-DIAGNOSTIC' >&2
cat "$ANSWER"
exit "${DOOR_RC:-4}"
"#,
    );
    let run = |answer: &Value, code: &str, arguments: &[&str]| {
        write_file(&scratch.join("answer.json"), &answer.to_string());
        Command::new("bash")
            .arg(root.join("infra/forge/probe-admission-source-access.sh"))
            .args(arguments)
            .env(
                "PATH",
                format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
            )
            .env("BOSS_SOR_ENV", scratch.join("absent.env"))
            .env(
                "BOSS_ESTATE_NODES_URL",
                format!("file://{}", scratch.join("estate.json").display()),
            )
            .env("BOSS_FORGE_REGISTRY_HOST", "registry.invalid")
            .env("CALLS", scratch.join("calls"))
            .env("INPUT", scratch.join("input"))
            .env("ANSWER", scratch.join("answer.json"))
            .env("DOOR_RC", code)
            .output()
            .unwrap()
    };
    let unavailable = json!({"schema":"boss.admission-source-access.v1","scope":"retained_source_access_probe","history_verdict":"unavailable",
        "roster":{"state":"unavailable","reason":"native_read_failed"},"nodes":[]});
    let output = run(&unavailable, "4", &[]);
    assert_eq!(output.status.code(), Some(4));
    assert!(output.stderr.is_empty());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        unavailable
    );
    let calls = std::fs::read_to_string(scratch.join("calls")).unwrap();
    for flag in [
        "--memory 256m",
        "--memory-swap 256m",
        "--pids-limit 64",
        "--rm",
        "--read-only",
        "--cap-drop ALL",
        "--security-opt no-new-privileges",
        "--ulimit core=0",
        "--tmpfs /tmp:rw,noexec,nosuid,size=8m",
        "-k 5 850 python3 -B /access.py",
        "dst=/tc,readonly",
        "dst=/kc,readonly",
        "dst=/talosctl,readonly",
        "dst=/admission-source.py,readonly",
        "dst=/target.json,readonly",
    ] {
        assert!(
            calls.contains(flag),
            "fixed boundary missing {flag}: {calls}"
        );
    }
    for (mutated, code) in [
        (json!({"raw":"PRIVATE-NATIVE-DIAGNOSTIC"}), "0"),
        (unavailable.clone(), "1"),
        (json!({"raw":"PRIVATE-OOM-DIAGNOSTIC"}), "137"),
    ] {
        let output = run(&mutated, code, &[]);
        assert_eq!(output.status.code(), Some(4));
        assert!(output.stderr.is_empty());
        assert!(!String::from_utf8_lossy(&output.stdout).contains("PRIVATE-NATIVE"));
        assert_eq!(
            serde_json::from_slice::<Value>(&output.stdout).unwrap()["roster"]["state"],
            "unavailable"
        );
    }
    let output = run(&unavailable, "4", &["/unapproved/path"]);
    assert_eq!(output.status.code(), Some(4));
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["roster"]["reason"],
        "unexpected_arguments"
    );
}
