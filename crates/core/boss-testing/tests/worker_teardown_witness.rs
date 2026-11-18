//! The fixed reader is exercised only through its bounded Kubernetes port.
#[test]
fn the_worker_teardown_reader_requires_complete_uid_inventories_and_controls() {
    let out = std::process::Command::new("python3")
        .arg("-B")
        .arg(boss_testing::repo_root().join("infra/forge/tests/worker-teardown-test.py"))
        .env(
            "TEARDOWN_TEST_ROOT",
            boss_testing::scratch_dir("worker-teardown-witness"),
        )
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn stock_pool_identity_activation_is_explicitly_unenforced_until_its_own_car() {
    let root = boss_testing::repo_root();
    // A004651: UNENFORCED — pools still declare v1 reclaimers; flipped by
    // the activation car. Do not add this debt as a field on a v1 bundle
    // row: that would itself change the declaration the boot seed reads.
    // The versions below are not the activation either: they added only
    // `reclaimer_kind`, the packet kind each run files (backlog 5ece507d),
    // and on disk:ci-workspaces `station` (backlog c9b9bacc), with
    // `reclaimer` and every activation key below unchanged. The activation
    // car bumps each again.
    let mut staged = Vec::new();
    for entry in std::fs::read_dir(root.join("infra/platform/pools")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|s| s.to_str()) != Some("toml") {
            continue;
        }
        let value: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        for row in value["pool"].as_array().unwrap() {
            if row["kind"].as_str() != Some("stock") {
                continue;
            }
            let name = row["name"].as_str().unwrap();
            let program = row["reclaimer"].as_str().unwrap();
            // disk:ci-workspaces is at v4 without the activation: v3 added
            // only `station = "forge-ci"` (backlog c9b9bacc), a label that
            // changes no reservation and no reclaimer authority, and v4 adds
            // `reclaimer_kind` (backlog 5ece507d) over it. The other stocks
            // are at v2, which added only `reclaimer_kind`. One more version
            // each (v5, v3) adds only `capacity_from` (backlog 84f570a1),
            // prose saying where the capacity comes from, which changes no
            // reservation and no reclaimer authority. The activation keys
            // below stay absent.
            let unpaid = if name == "disk:ci-workspaces" { 5 } else { 3 };
            assert_eq!(
                row["version"].as_integer(),
                Some(unpaid),
                "{name}: activation is unpaid"
            );
            assert!(
                program.starts_with("infra/"),
                "{name}: do not silently change old authority"
            );
            assert!(
                root.join(program).is_file(),
                "{name}: missing program {program}"
            );
            for key in ["program", "return_verifier", "identity_pin_unenforced"] {
                assert!(
                    row.get(key).is_none(),
                    "{name}: no boot-seeded row change in 2c: {key}"
                );
            }
            staged.push(name.to_owned());
        }
    }
    staged.sort();
    assert_eq!(
        staged,
        vec![
            "disk:build-cache",
            "disk:ci-workspaces",
            "disk:registry-blobs"
        ]
    );
}

#[test]
fn the_executed_envelope_is_a_runner_owned_protocol_field() {
    let root = boss_testing::repo_root();
    let value: toml::Value = toml::from_str(
        &std::fs::read_to_string(root.join("infra/platform/workflows/ops-request.toml")).unwrap(),
    )
    .unwrap();
    let steps = value["workflow"][0]["step"].as_array().unwrap();
    let execute = steps
        .iter()
        .find(|s| s["title"].as_str() == Some("execute"))
        .unwrap();
    assert_eq!(execute["executor"].as_str(), Some("runner:ops"));
    let field = execute["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"].as_str() == Some("executed"))
        .expect("the pinned protocol must protect the actual executed envelope");
    assert_eq!(field["writer"].as_str(), Some("runner:ops"));
    assert_eq!(field["field_type"].as_str(), Some("object"));
    assert_eq!(
        field["required"].as_bool(),
        Some(false),
        "a pre-execution refusal has no executed envelope"
    );
}
