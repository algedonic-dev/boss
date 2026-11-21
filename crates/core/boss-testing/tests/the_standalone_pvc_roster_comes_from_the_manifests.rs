//! tree-wide pin — standalone PVC declarations and preservation are shared
//! by converge, approved expansion and the estate observer (43b4b5d2).
//! Run the database-free owning controls, including the real apply boundary.

use boss_testing::repo_root;
use std::process::Command;

#[test]
fn standalone_claim_controls_preserve_source_identity_and_live_growth() {
    for test in [
        "standalone-pvc-test.py",
        "standalone-pvc-apply-test.py",
        "standalone-pvc-rendered-test.py",
    ] {
        let result = Command::new("python3")
            .arg("-B")
            .arg(repo_root().join("infra/forge/tests").join(test))
            .output()
            .expect("run standalone claim controls");
        assert!(
            result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    }
}

#[test]
fn every_standalone_manifest_claim_is_in_one_nonempty_typed_inventory() {
    let result = Command::new("python3")
        .arg("-B")
        .arg(repo_root().join("infra/forge/standalone-pvc.py"))
        .arg(repo_root().join("infra/cluster/manifests"))
        .output()
        .expect("read manifest inventory");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let rows: Vec<serde_json::Value> =
        serde_json::from_slice(&result.stdout).expect("typed inventory");
    assert!(!rows.is_empty(), "no declarations is not a passing pin");
    for row in rows {
        assert!(
            row["declared_bytes"].as_u64().is_some_and(|size| size > 0),
            "{row}"
        );
        assert!(row["line"].as_u64().is_some_and(|line| line > 0), "{row}");
    }
}
