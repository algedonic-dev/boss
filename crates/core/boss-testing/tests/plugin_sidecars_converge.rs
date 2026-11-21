//! Plugin data sidecars travel through the same converged ConfigMap as bundles.
use std::{fs, path::PathBuf, process::Command};

#[test]
fn the_actual_converge_roster_carries_js_and_json_but_not_documentation() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let script = fs::read_to_string(root.join("infra/forge/cluster-deploy-runner.sh")).unwrap();
    let start = script.find("PLUGIN_ARGS=\"\"").unwrap();
    let end = script[start..].find("\n# Per instance namespace").unwrap() + start;
    let temp = boss_testing::scratch_dir("plugin-sidecar-converge");
    let plugins = temp.join("infra/step-plugins");
    fs::create_dir_all(&plugins).unwrap();
    for name in ["renderer.js", "contract.json", "README.md"] {
        fs::write(plugins.join(name), "original").unwrap();
    }
    let output = Command::new("bash")
        .env("REPO", &temp)
        .arg("-c")
        .arg(format!(
            "{}\nprintf '%s' \"$PLUGIN_ARGS\"",
            &script[start..end]
        ))
        .output()
        .unwrap();
    assert!(output.status.success());
    let roster = String::from_utf8(output.stdout).unwrap();
    let mut actual = roster.split_whitespace().collect::<Vec<_>>();
    actual.sort_unstable();
    assert_eq!(
        actual,
        [
            "--from-file=contract.json=/plugins/contract.json",
            "--from-file=renderer.js=/plugins/renderer.js"
        ]
    );
}
