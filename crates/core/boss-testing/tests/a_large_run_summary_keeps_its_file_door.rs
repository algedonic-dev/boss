//! Whole worktree coverage must not turn a summary file back into argv.
use boss_testing::{repo_root, scratch_dir, without_proxy, write_exec};
use std::process::Command;

#[test]
fn a_summary_larger_than_one_argument_is_merged_and_sent_whole() {
    let root = scratch_dir("boss-large-run-summary");
    let bin = root.join("bin");
    boss_testing::create_dir(&bin);
    write_exec(
        &bin.join("curl"),
        r#"#!/bin/bash
set -eu
method=GET; body=; file=
while [ $# -gt 0 ]; do
    case "$1" in
        -X) method=$2; shift;;
        -d) body=$2; shift;;
        --data-binary) file=${2#@}; shift;;
    esac
    shift
done
case "$method" in
    GET) cat "$STUB_PACKET";;
    PATCH)
        if [ -n "$file" ]; then
            stat -c %a "$file" > "$STUB_FILE_MODE"
            cat "$file" > "$STUB_PATCH"
        else printf '%s' "$body" > "$STUB_PATCH"; fi
        printf '{}';;
    PUT) printf '%s' "$body" > "$STUB_PUT"; printf '{}';;
esac
"#,
    );
    let packet = root.join("packet.json");
    std::fs::write(&packet, serde_json::json!({"data":[{"id":"job-1","status":"open","metadata":{},"steps":[{"id":"step-1","spec_slug":"run","status":"active","metadata":{"inventory":{"prior":true}}}]}],"total":1}).to_string()).unwrap();
    let rows: Vec<_> = (0..1200).map(|n| serde_json::json!({"path":format!("/fixture/worktrees/agent-{n}"),"state":"held","why":"x".repeat(180)})).collect();
    let summary = root.join("summary.json");
    let value = serde_json::json!({"inventory":{"rows":rows},"result":"failed"});
    let bytes = serde_json::to_vec(&value).unwrap();
    assert!(bytes.len() > 131072);
    std::fs::write(&summary, bytes).unwrap();
    let patch = root.join("patch.json");
    let put = root.join("put.json");
    let mode = root.join("file-mode");
    let full_output = root.join("output.txt");
    let output_text = "original unabridged finding\n".repeat(8000);
    std::fs::write(&full_output, &output_text).unwrap();
    let mut cmd = Command::new("bash");
    without_proxy(&mut cmd);
    let out = cmd
        .arg(repo_root().join("infra/boss-step.sh"))
        .args(["maintenance-summary-fixture", "run", "result=ok"])
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .env("BOSS_JOBS_URL", "http://sor.test:7900")
        .env("BOSS_STEP_ACTOR", "automation:summary-fixture")
        .env("BOSS_RUN_SUMMARY_FILE", &summary)
        .env("BOSS_MACHINE_TOKEN_DIR", root.join("no-token"))
        .env("BOSS_API_RETRY_DEADLINE", "0")
        .env("STUB_PACKET", packet)
        .env("STUB_PATCH", &patch)
        .env("STUB_PUT", &put)
        .env("STUB_FILE_MODE", &mode)
        .env_remove("BOSS_STEP_DRY_RUN")
        .env_remove("BOSS_STEP_METADATA_ONLY")
        .env("BOSS_STEP_OUTPUT_FILE", &full_output)
        .env_remove("SERVICE_RESULT")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let recorded: serde_json::Value =
        serde_json::from_slice(&std::fs::read(patch).unwrap()).unwrap();
    assert_eq!(recorded["inventory"]["rows"], value["inventory"]["rows"]);
    assert_eq!(
        recorded["inventory"]["prior"], true,
        "existing deep-merge semantics remain"
    );
    assert_eq!(recorded["result"], "ok", "explicit caller result wins");
    assert_eq!(
        recorded["output"], output_text,
        "the existing output-file door stays lossless beside the summary"
    );
    assert_eq!(std::fs::read_to_string(mode).unwrap().trim(), "600");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&std::fs::read(&put).unwrap()).unwrap(),
        serde_json::json!({"status":"completed"})
    );
    assert!(!summary.exists(), "the native summary is consumed once");
    // A required record is for this invocation, even if its bytes happen
    // to equal a completed predecessor. Status retry cannot acknowledge it.
    std::fs::write(root.join("packet.json"), serde_json::json!({"data":[{"id":"job-1","status":"open","metadata":{},"steps":[{"id":"step-1","spec_slug":"run","status":"completed","metadata":recorded}]}],"total":1}).to_string()).unwrap();
    std::fs::write(&summary, serde_json::to_vec(&value).unwrap()).unwrap();
    std::fs::remove_file(root.join("patch.json")).unwrap();
    let unchanged = cmd.env("BOSS_STEP_REQUIRE_RECORD", "1").output().unwrap();
    assert!(
        !unchanged.status.success(),
        "identical terminal facts are not a fresh required record"
    );
    assert!(!root.join("patch.json").exists());
    for invalid in ["", " \n", "{", "[]", "{}\n{}"] {
        std::fs::write(&summary, invalid).unwrap();
        std::fs::remove_file(root.join("patch.json")).ok();
        let refused = cmd.env("BOSS_STEP_REQUIRE_RECORD", "1").output().unwrap();
        assert!(
            !refused.status.success(),
            "invalid required summary cannot ACK: {invalid:?}"
        );
        assert!(!root.join("patch.json").exists());
        assert!(summary.exists(), "retain unacknowledged producer evidence");
    }
    for no_packet in [true, false] {
        let packet_value = if no_packet {
            serde_json::json!({"data":[],"total":0})
        } else {
            serde_json::json!({"data":[{"id":"job-1","status":"open","metadata":{},"steps":[{"id":"step-1","spec_slug":"run","status":"completed","metadata":{}}]}],"total":1})
        };
        std::fs::write(root.join("packet.json"), packet_value.to_string()).unwrap();
        std::fs::write(&summary, "{\"current\":true}").unwrap();
        std::fs::remove_file(root.join("patch.json")).ok();
        let refused = cmd.env("BOSS_STEP_REQUIRE_RECORD", "1").output().unwrap();
        assert!(
            !refused.status.success(),
            "a no-op cannot acknowledge new evidence"
        );
        assert!(!root.join("patch.json").exists());
        let compatible = cmd.env("BOSS_STEP_REQUIRE_RECORD", "0").output().unwrap();
        assert!(
            !compatible.status.success(),
            "best-effort cannot discard current facts either"
        );
        assert!(!root.join("patch.json").exists());
        let plain = without_proxy(&mut Command::new("bash"))
            .arg(repo_root().join("infra/boss-step.sh"))
            .args(["maintenance-summary-fixture", "run"])
            .env(
                "PATH",
                format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
            )
            .env("BOSS_JOBS_URL", "http://sor.test:7900")
            .env("BOSS_MACHINE_TOKEN_DIR", root.join("no-token"))
            .env("BOSS_API_RETRY_DEADLINE", "0")
            .env("KUBERNETES_SERVICE_HOST", "fixture")
            .env("STUB_PACKET", root.join("packet.json"))
            .env("STUB_PUT", &put)
            .env_remove("BOSS_RUN_SUMMARY_FILE")
            .env_remove("BOSS_STEP_OUTPUT_FILE")
            .env_remove("BOSS_STEP_REQUIRE_RECORD")
            .env_remove("SERVICE_RESULT")
            .output()
            .unwrap();
        assert!(
            plain.status.success(),
            "a plain status retry with no current facts remains harmless: {}",
            String::from_utf8_lossy(&plain.stderr)
        );
    }
    let refused = cmd
        .env("BOSS_STEP_REQUIRE_RECORD", "1")
        .env("BOSS_STEP_DRY_RUN", "1")
        .output()
        .unwrap();
    assert!(
        !refused.status.success(),
        "printing a plan never acknowledges evidence"
    );
}
