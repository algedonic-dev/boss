//! Real shell boundaries with local evidence; no host or provider effects.
use boss_testing::{repo_root, scratch_dir};
use std::process::Command;

#[test]
fn deployment_distinguishes_arrival_pending_equality_and_unknown_eligibility() {
    let root = scratch_dir("delivery-stage");
    let image = "a".repeat(40);
    let head = "b".repeat(40);
    let store = root.join("image-cli");
    std::fs::create_dir_all(store.join(&image)).unwrap();
    std::os::unix::fs::symlink(&image, store.join("current")).unwrap();
    std::fs::write(
        store.join(&image).join("boss-inputs.json"),
        "published receipt",
    )
    .unwrap();
    boss_testing::write_exec(
        &store.join(&image).join("boss"),
        "#!/bin/sh\ncat \"$LOCAL_OBSERVATION\"\n",
    );
    let observation = root.join("native-observation.json");
    let summary = root.join("summary.json");
    for (compatible, selection_ready, inputs_equal, route, expected) in [
        (true, true, true, "data", 0),
        (false, true, true, "data", 75),
        (false, false, true, "data", 1),
        (false, true, false, "data", 1),
        (false, true, true, "software", 1),
    ] {
        let original = serde_json::json!({"schema":1,"compatible":compatible,
            "binary_image":image,"compatible_source":head,"compiler_inputs_equal":inputs_equal,
            "plan":{"route":route},"delivery_evidence":{"arrived":compatible,"selection_ready":selection_ready,"native_original":"all native bytes"}});
        std::fs::write(&observation, serde_json::to_vec_pretty(&original).unwrap()).unwrap();
        let output = Command::new("bash").args(["-c",". \"$SUMMARY_SOURCE\"; . \"$STAGE_SOURCE\"; delivery_channel_stage \"$SOURCE_REPO\" \"$HEAD\" \"$LAST\" \"$CLI_STORE\""])
            .env("SUMMARY_SOURCE",repo_root().join("infra/run-summary.sh"))
            .env("STAGE_SOURCE",repo_root().join("infra/forge/delivery-stage.sh"))
            .env("SOURCE_REPO",&root).env("HEAD",&head).env("LAST",&image[..7])
            .env("CLI_STORE",&store).env("LOCAL_OBSERVATION",&observation)
            .env("BOSS_RUN_SUMMARY_FILE",&summary).output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(expected),
            "{original}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let actual: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&summary).unwrap()).unwrap();
        assert_eq!(actual["delivery_evidence"], original);
    }
    // An empty successful process is not a native observation. Cardinality,
    // identity and decoding refusals must all retain the software route.
    let valid = serde_json::json!({"schema":1,"compatible":true,
        "binary_image":image,"compatible_source":head,"compiler_inputs_equal":true,
        "plan":{"route":"data"},"delivery_evidence":{"arrived":true,"selection_ready":true}})
    .to_string();
    for original in [
        String::new(),
        " \n\t".into(),
        "{".into(),
        format!("{valid}\n{valid}"),
        valid.replace(&head, &"c".repeat(40)),
    ] {
        std::fs::write(&observation, &original).unwrap();
        let output = Command::new("bash").args(["-c",". \"$SUMMARY_SOURCE\"; . \"$STAGE_SOURCE\"; delivery_channel_stage \"$SOURCE_REPO\" \"$HEAD\" \"$LAST\" \"$CLI_STORE\""])
            .env("SUMMARY_SOURCE",repo_root().join("infra/run-summary.sh"))
            .env("STAGE_SOURCE",repo_root().join("infra/forge/delivery-stage.sh"))
            .env("SOURCE_REPO",&root).env("HEAD",&head).env("LAST",&image[..7])
            .env("CLI_STORE",&store).env("LOCAL_OBSERVATION",&observation)
            .env("BOSS_RUN_SUMMARY_FILE",&summary).output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(1),
            "invalid original {original:?}: {output:?}"
        );
        let actual: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&summary).unwrap()).unwrap();
        assert!(
            actual["delivery_channel"]
                .as_str()
                .unwrap()
                .starts_with("software:")
        );
        let captures = std::fs::read_dir(store.join("compatibility"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|s| s == "json"))
            .map(|entry| std::fs::read(entry.path()).unwrap())
            .collect::<Vec<_>>();
        assert!(
            captures.contains(&original.into_bytes()),
            "the refused original must remain whole"
        );
    }
}

#[test]
fn whole_native_evidence_reaches_the_existing_summary_without_an_argv_limit() {
    let root = scratch_dir("delivery-summary-file");
    let evidence = root.join("evidence.json");
    let summary = root.join("summary.json");
    let original =
        serde_json::json!({"schema":1,"arrived":true,"native_original":"x".repeat(262144)});
    std::fs::write(&evidence, serde_json::to_vec_pretty(&original).unwrap()).unwrap();
    let output = Command::new("bash")
        .args([
            "-c",
            ". \"$SUMMARY_SOURCE\"; run_summary_json_file delivery_evidence \"$EVIDENCE\"",
        ])
        .env("SUMMARY_SOURCE", repo_root().join("infra/run-summary.sh"))
        .env("EVIDENCE", &evidence)
        .env("BOSS_RUN_SUMMARY_FILE", &summary)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let actual: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&summary).unwrap()).unwrap();
    assert_eq!(actual["delivery_evidence"], original);
}
