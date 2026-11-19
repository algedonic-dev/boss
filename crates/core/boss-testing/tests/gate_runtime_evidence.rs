use boss_testing::{repo_root, scratch_dir};
use serde_json::Value;
use std::process::Command;

#[test]
fn browser_policy_requires_the_whole_bounded_exact_configuration() {
    let dir = scratch_dir("runtime-browser-policy");
    let journal = dir.join("samples.jsonl");
    std::fs::write(&journal, "").unwrap();
    let sidecar = journal.with_extension("jsonl.browser-config");
    let helper = repo_root().join("infra/gate-runner/runtime-evidence.py");
    let valid = r#"{"workers":8,"retries":0}"#;
    let bounded = format!("{valid}{}", " ".repeat(2048 - valid.len()));
    let cases = [
        (valid.to_owned(), true),
        (bounded.clone(), true),
        (format!("{bounded} "), false),
        (format!("{valid}{}NOT_JSON", " ".repeat(2049)), false),
        (
            r#"{"workers":8,"retries":0,"extra":true}"#.to_owned(),
            false,
        ),
        ("null".to_owned(), false),
        ("[]".to_owned(), false),
        ("8".to_owned(), false),
        ("true".to_owned(), false),
        (r#""configuration""#.to_owned(), false),
        ("{}".to_owned(), false),
        (r#"{"workers":8}"#.to_owned(), false),
        (r#"{"workers":true,"retries":0}"#.to_owned(), false),
        (r#"{"workers":0,"retries":0}"#.to_owned(), false),
        (r#"{"workers":-1,"retries":0}"#.to_owned(), false),
        (r#"{"workers":8,"retries":false}"#.to_owned(), false),
        (r#"{"workers":8,"retries":-1}"#.to_owned(), false),
        (r#"{"workers":8.0,"retries":0}"#.to_owned(), false),
        (r#"{"workers":8,"retries":"0"}"#.to_owned(), false),
        ("[".repeat(1000), false),
    ];
    for (index, (raw, configured)) in cases.into_iter().enumerate() {
        std::fs::write(&sidecar, &raw).unwrap();
        let out = Command::new("python3")
            .arg(&helper)
            .args(["report", journal.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(out.status.success(), "case {index}: {out:?}");
        let result: Value = serde_json::from_slice(&out.stdout).unwrap();
        let policy = &result["browser_policy"];
        if configured {
            assert_eq!(
                policy,
                &serde_json::json!({"state":"configured","workers":8,
                "retries":0,"method":"loaded Playwright configuration"})
            );
        } else {
            assert_eq!(policy["state"], "unavailable", "case {index}: {policy}");
            assert_eq!(policy.as_object().unwrap().len(), 2);
            let reason = policy["reason"].as_str().unwrap();
            assert!(!reason.is_empty() && reason.len() <= 2048);
            assert!(!reason.contains(sidecar.to_str().unwrap()));
        }
        assert_eq!(std::fs::read(&sidecar).unwrap(), raw.as_bytes());
        let receipt = dir.join("receipt.json");
        std::fs::write(&receipt, r#"{"verdict":"failed","fails":["original"]}"#).unwrap();
        let merged = Command::new("python3")
            .arg(&helper)
            .args([
                "merge",
                journal.to_str().unwrap(),
                receipt.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(merged.status.success(), "case {index}: {merged:?}");
        let merged: Value = serde_json::from_slice(&std::fs::read(&receipt).unwrap()).unwrap();
        assert_eq!(merged["runtime_evidence"], result);
        assert_eq!(merged["verdict"], "failed");
        assert_eq!(merged["fails"][0], "original");
    }
    std::fs::remove_file(&sidecar).unwrap();
    let out = Command::new("python3")
        .arg(&helper)
        .args(["report", journal.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let result: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        result["browser_policy"],
        serde_json::json!({"state":"unavailable",
        "reason":"resolved browser configuration unavailable"})
    );
}

#[test]
fn browser_receipt_validates_every_observation_variant_before_copying_it() {
    let dir = scratch_dir("runtime-browser-shapes");
    let journal = dir.join("samples.jsonl");
    std::fs::write(&journal, "").unwrap();
    std::fs::write(
        journal.with_extension("jsonl.browser-config"),
        r#"{"workers":8,"retries":0}"#,
    )
    .unwrap();
    let sidecar = journal.with_extension("jsonl.browser");
    let valid = serde_json::json!({"state":"measured","tests":12,"workers":3,
        "method":"Playwright list reporter runtime announcement","source":"browser.log"});
    let mut cases = vec![
        serde_json::json!({"state":"measured"}),
        Value::Null,
        serde_json::json!({"state":"unavailable"}),
        serde_json::json!({"state":"invalid","reason":false}),
        serde_json::json!({"state":"unavailable","reason":""}),
        serde_json::json!({"state":"invalid","reason":"   "}),
    ];
    for key in ["tests", "workers", "method", "source"] {
        let mut missing = valid.clone();
        missing.as_object_mut().unwrap().remove(key);
        cases.push(missing);
    }
    for (key, value) in [
        ("workers", serde_json::json!(-3)),
        ("workers", serde_json::json!(true)),
        ("workers", serde_json::json!(0)),
        ("tests", serde_json::json!(-1)),
        ("tests", serde_json::json!(false)),
        ("method", serde_json::json!("")),
        ("method", serde_json::json!("  ")),
        ("source", serde_json::json!("x".repeat(2049))),
    ] {
        let mut wrong = valid.clone();
        wrong[key] = value;
        cases.push(wrong);
    }
    let mut extra = valid.clone();
    extra["extra"] = serde_json::json!(true);
    cases.push(extra);
    let helper = repo_root().join("infra/gate-runner/runtime-evidence.py");
    for (index, row) in cases.iter().enumerate() {
        let bytes = row.to_string();
        std::fs::write(&sidecar, &bytes).unwrap();
        let out = Command::new("python3")
            .arg(&helper)
            .args(["report", journal.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "case {index}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let result: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(
            result["browser"]["state"], "invalid",
            "case {index}: {result}"
        );
        assert!(!result["browser"]["reason"].as_str().unwrap().is_empty());
        assert!(result["browser"].get("workers").is_none());
        assert_eq!(result["browser_policy"]["workers"], 8);
        assert_eq!(result["browser_policy"]["state"], "configured");
        assert_eq!(std::fs::read_to_string(&sidecar).unwrap(), bytes);
        let receipt = dir.join("receipt.json");
        std::fs::write(&receipt, r#"{"verdict":"failed","fails":["original"]}"#).unwrap();
        let merged = Command::new("python3")
            .arg(&helper)
            .args([
                "merge",
                journal.to_str().unwrap(),
                receipt.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(merged.status.success());
        let merged: Value = serde_json::from_slice(&std::fs::read(&receipt).unwrap()).unwrap();
        assert_eq!(merged["runtime_evidence"], result);
        assert_eq!(merged["verdict"], "failed");
        assert_eq!(merged["fails"][0], "original");
    }
    for row in [
        valid,
        serde_json::json!({"state":"unavailable","reason":"no runtime output"}),
        serde_json::json!({"state":"invalid","reason":"nonpositive worker observation"}),
    ] {
        std::fs::write(&sidecar, row.to_string()).unwrap();
        let out = Command::new("python3")
            .arg(&helper)
            .args(["report", journal.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(out.status.success());
        let result: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(result["browser"], row);
    }
}

#[test]
fn valid_json_with_invalid_sample_shape_is_diagnosed_in_report_and_merge() {
    let dir = scratch_dir("runtime-shapes");
    let helper = repo_root().join("infra/gate-runner/runtime-evidence.py");
    let journal = dir.join("samples.jsonl");
    assert!(
        Command::new("python3")
            .arg(&helper)
            .args(["sample", journal.to_str().unwrap(), "start"])
            .status()
            .unwrap()
            .success()
    );
    let original: Value =
        serde_json::from_str(std::fs::read_to_string(&journal).unwrap().trim()).unwrap();
    let mut cases = vec![
        serde_json::json!({}),
        Value::Null,
        serde_json::json!(7),
        serde_json::json!([]),
    ];
    for key in [
        "label",
        "at",
        "monotonic_ns",
        "identity",
        "lifetime",
        "method",
        "workers",
        "resources",
        "files",
        "overlap",
        "child_runtime_workers",
    ] {
        let mut missing = original.clone();
        missing.as_object_mut().unwrap().remove(key);
        cases.push(missing);
        let mut wrong = original.clone();
        wrong[key] = serde_json::json!(false);
        cases.push(wrong);
    }
    for pointer in [
        "/identity/POD_UID",
        "/workers/cargo",
        "/resources/GATE_CPU_LIMIT",
        "/files/cpu.stat",
        "/overlap",
        "/child_runtime_workers",
    ] {
        let mut wrong = original.clone();
        *wrong.pointer_mut(pointer).unwrap() = serde_json::json!({});
        cases.push(wrong);
    }
    for (pointer, value) in [
        ("/label", serde_json::json!("")),
        ("/at", serde_json::json!("yesterday")),
        ("/monotonic_ns", serde_json::json!(-1)),
        (
            "/files/cpu.stat",
            serde_json::json!({"state":"measured","raw":7}),
        ),
        (
            "/workers/cargo",
            serde_json::json!({"state":"configured","value":0,"method":"env"}),
        ),
    ] {
        let mut wrong = original.clone();
        *wrong.pointer_mut(pointer).unwrap() = value;
        cases.push(wrong);
    }
    let mut extra = original.clone();
    extra["unrecognized"] = serde_json::json!({"unbounded":"payload"});
    cases.push(extra);
    let mut extra = original.clone();
    extra["workers"]["cargo"] =
        serde_json::json!({"state":"configured","value":1,"method":"env","extra":true});
    cases.push(extra);
    for (index, row) in cases.iter().enumerate() {
        let bytes = format!("{row}\n{row}\n");
        std::fs::write(&journal, &bytes).unwrap();
        let out = Command::new("python3")
            .arg(&helper)
            .args(["report", journal.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "case {index}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let report: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(report["state"], "unavailable", "case {index}: {report}");
        assert_eq!(report["sample_count"], 0, "case {index}");
        assert!(
            !report["collection_errors"].as_array().unwrap().is_empty(),
            "case {index}"
        );
        let receipt = dir.join("receipt.json");
        std::fs::write(
            &receipt,
            "{\"verdict\":\"failed\",\"fails\":[\"original\"]}",
        )
        .unwrap();
        let out = Command::new("python3")
            .arg(&helper)
            .args([
                "merge",
                journal.to_str().unwrap(),
                receipt.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "merge case {index}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let merged: Value = serde_json::from_slice(&std::fs::read(receipt).unwrap()).unwrap();
        assert_eq!(merged["runtime_evidence"], report);
        assert_eq!(merged["verdict"], "failed");
        assert_eq!(merged["fails"][0], "original");
        assert_eq!(std::fs::read_to_string(&journal).unwrap(), bytes);
    }
}

#[test]
fn excessively_nested_json_retains_bounded_structured_diagnostics() {
    let dir = scratch_dir("runtime-deep-json");
    let journal = dir.join("samples.jsonl");
    let bytes = format!("{}0{}\n", "[".repeat(2000), "]".repeat(2000));
    std::fs::write(&journal, &bytes).unwrap();
    let out = Command::new("python3")
        .arg(repo_root().join("infra/gate-runner/runtime-evidence.py"))
        .args(["report", journal.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["state"], "unavailable");
    assert_eq!(report["invalid_rows"], 1);
    assert!(!report["collection_errors"].as_array().unwrap().is_empty());
    assert_eq!(std::fs::read_to_string(journal).unwrap(), bytes);
}

#[test]
fn shape_diagnostics_are_bounded_and_do_not_bridge_an_invalid_sample() {
    let dir = scratch_dir("runtime-shape-conservation");
    let helper = repo_root().join("infra/gate-runner/runtime-evidence.py");
    let journal = dir.join("samples.jsonl");
    assert!(
        Command::new("python3")
            .arg(&helper)
            .args(["sample", journal.to_str().unwrap(), "before"])
            .status()
            .unwrap()
            .success()
    );
    let mut row: Value =
        serde_json::from_str(std::fs::read_to_string(&journal).unwrap().trim()).unwrap();
    for value in row["identity"].as_object_mut().unwrap().values_mut() {
        *value = serde_json::json!("known");
    }
    for value in row["files"].as_object_mut().unwrap().values_mut() {
        *value = serde_json::json!({"state":"measured","raw":"0\n"});
    }
    row["files"]["cpu.stat"]["raw"] = serde_json::json!("usage_usec 100\nnr_throttled 1\n");
    // "0" is no io.stat line; a measured receipt needs a measured byte table.
    row["files"]["io.stat"]["raw"] = serde_json::json!("8:0 rbytes=0 wbytes=0\n");
    let before = row.clone();
    row["label"] = serde_json::json!("after");
    row["monotonic_ns"] = serde_json::json!(before["monotonic_ns"].as_u64().unwrap() + 1000);
    row["files"]["cpu.stat"]["raw"] = serde_json::json!("usage_usec 150\nnr_throttled 2\n");
    std::fs::write(&journal, format!("{before}\n{row}\n")).unwrap();
    let report = |journal: &std::path::Path| {
        let out = Command::new("python3")
            .arg(&helper)
            .args(["report", journal.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice::<Value>(&out.stdout).unwrap()
    };
    let positive = report(&journal);
    assert_eq!(positive["state"], "measured");
    assert_eq!(positive["sample_count"], 2);
    assert_eq!(positive["intervals"][0]["cpu_usage_usec"], 50);
    for boot in [
        serde_json::json!({"state":"unavailable","reason":"boot identity unreadable"}),
        serde_json::json!({"state":"truncated","raw":"same-prefix"}),
        serde_json::json!({"state":"measured","raw":""}),
        serde_json::json!({"state":"measured","raw":"   \n"}),
    ] {
        let mut unknown_before = before.clone();
        let mut unknown_after = row.clone();
        unknown_before["lifetime"][2] = boot.clone();
        unknown_after["lifetime"][2] = boot;
        let retained = format!("{unknown_before}\n{unknown_after}\n");
        std::fs::write(&journal, &retained).unwrap();
        let unknown = report(&journal);
        assert_eq!(unknown["state"], "partial", "{unknown}");
        assert_eq!(unknown["sample_count"], 2);
        assert_eq!(unknown["invalid_rows"], 0);
        assert_eq!(unknown["intervals"][0]["state"], "invalid");
        assert!(unknown["intervals"][0].get("cpu_usage_usec").is_none());
        let receipt = dir.join("unknown-receipt.json");
        std::fs::write(
            &receipt,
            r#"{"verdict":"failed","checks":[{"name":"original","status":"FAIL"}]}"#,
        )
        .unwrap();
        assert!(
            Command::new("python3")
                .arg(&helper)
                .args([
                    "merge",
                    journal.to_str().unwrap(),
                    receipt.to_str().unwrap()
                ])
                .status()
                .unwrap()
                .success()
        );
        let merged: Value = serde_json::from_slice(&std::fs::read(&receipt).unwrap()).unwrap();
        assert_eq!(merged["verdict"], "failed");
        assert_eq!(merged["checks"][0]["status"], "FAIL");
        assert_eq!(merged["runtime_evidence"], unknown);
        assert_eq!(std::fs::read_to_string(&journal).unwrap(), retained);
    }
    let bytes = format!("{before}\n{}{row}\n", "{}\n".repeat(100));
    std::fs::write(&journal, &bytes).unwrap();
    let damaged = report(&journal);
    assert_eq!(damaged["state"], "partial");
    assert_eq!(damaged["sample_count"], 2);
    assert_eq!(damaged["invalid_rows"], 100);
    assert!(damaged["collection_errors"].as_array().unwrap().len() <= 16);
    assert!(
        damaged["intervals"].as_array().unwrap().is_empty(),
        "invalid journal gaps must not be compared as consecutive observations"
    );
    assert_eq!(std::fs::read_to_string(&journal).unwrap(), bytes);
}

#[test]
fn nested_gate_has_its_own_journal_and_preserves_parent_phase_interval() {
    let dir = scratch_dir("runtime-nested-journal");
    let tree = dir.join("tree");
    boss_testing::copy_gate_sh(&tree);
    boss_testing::write_file(
        &tree.join("infra/lint/workspace-declares-what-it-runs.sh"),
        "#!/usr/bin/env bash\nexit 0\n",
    );
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["add", "."],
        vec![
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@test",
            "commit",
            "-qm",
            "fixture",
        ],
    ] {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(&tree)
                .status()
                .unwrap()
                .success()
        );
    }
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let parent_journal = dir.join("parent.jsonl");
    let before_child = dir.join("parent-before-child.jsonl");
    let child_receipt = dir.join("child-receipt.json");
    let child_df = dir.join("child-df");
    let counter = dir.join("child-df-count");
    boss_testing::write_exec(
        &child_df,
        &format!(
            "#!/usr/bin/env bash\nn=$(cat '{}' 2>/dev/null || echo 0)\necho $((n+1)) > '{}'\n\
         echo 'Filesystem 1024-blocks Used Available Capacity Mounted on'\n\
         if [ \"$n\" -lt 1 ]; then echo '/dev/fake 1 1 943718400 1% /'; else echo '/dev/fake 1 1 1048576 99% /'; fi\n",
            counter.display(),
            counter.display()
        ),
    );
    // cargo fmt is the outer phase's child process. Its nested gate must
    // inherit normal execution controls but never the outer journal path.
    boss_testing::write_exec(
        &bin.join("cargo"),
        &format!(
            "#!/usr/bin/env bash\nif [ \"${{1:-}}\" != fmt ]; then echo '{{\"packages\":[]}}'; exit 0; fi\n\
         cp '{}' '{}'\n\
         BOSS_GATE_DF_CMD='{}' BOSS_GATE_RECEIPT='{}' bash '{}' --quick > '{}' 2>&1\n\
         rc=$?\n[ \"$rc\" -eq 2 ] || exit 1\ncmp -s '{}' '{}' || exit 88\nexit 0\n",
            parent_journal.display(),
            before_child.display(),
            child_df.display(),
            child_receipt.display(),
            tree.join("infra/gate.sh").display(),
            dir.join("child.log").display(),
            parent_journal.display(),
            before_child.display()
        ),
    );
    let parent_df = dir.join("parent-df");
    boss_testing::write_exec(
        &parent_df,
        "#!/usr/bin/env bash\necho 'Filesystem 1024-blocks Used Available Capacity Mounted on'\necho '/dev/fake 1 1 943718400 1% /'\n",
    );
    let parent_receipt = dir.join("parent-receipt.json");
    let out = Command::new("bash")
        .arg(tree.join("infra/gate.sh"))
        .arg("--quick")
        .current_dir(&tree)
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .env("BOSS_GATE_RUNTIME_EVIDENCE", &parent_journal)
        .env("BOSS_GATE_RECEIPT", &parent_receipt)
        .env("BOSS_GATE_DF_CMD", &parent_df)
        .env("BOSS_GATE_MIN_FREE_GB", "12")
        .env("BOSS_GATE_TRUNK", "HEAD")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let rows: Vec<Value> = std::fs::read_to_string(&parent_journal)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        rows.iter()
            .filter(|row| row["label"] == "gate-start")
            .count(),
        1,
        "nested fixture appended its gate-start to the parent journal"
    );
    let report = Command::new("python3")
        .arg(tree.join("infra/gate-runner/runtime-evidence.py"))
        .args(["report", parent_journal.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        report.status.success(),
        "{}",
        String::from_utf8_lossy(&report.stderr)
    );
    let parent: Value = serde_json::from_slice(&report.stdout).unwrap();
    assert!(
        parent["intervals"]
            .as_array()
            .unwrap()
            .iter()
            .any(|interval| interval["from"] == "check-start:fmt"
                && interval["to"] == "check-finish:fmt"
                && interval["state"] == "measured"),
        "parent fmt interval was not conserved"
    );
    let child: Value = serde_json::from_slice(&std::fs::read(child_receipt).unwrap()).unwrap();
    assert_ne!(
        child["runtime_evidence"]["raw_record"],
        serde_json::json!(parent_journal)
    );
    assert!(
        child["runtime_evidence"]["sample_count"].as_u64().unwrap() > 0,
        "nested evidence must be preserved, not disabled"
    );
}

#[test]
fn copied_gate_carries_and_runs_its_actual_python_collector() {
    let dir = scratch_dir("runtime-copied-gate");
    let tree = dir.join("tree");
    boss_testing::copy_gate_sh(&tree);
    let helper = tree.join("infra/gate-runner/runtime-evidence.py");
    assert!(helper.is_file(), "the copied gate cannot run its collector");
    assert_eq!(
        std::fs::read(&helper).unwrap(),
        std::fs::read(repo_root().join("infra/gate-runner/runtime-evidence.py")).unwrap(),
        "fixture must carry the actual helper unchanged"
    );
    let journal = dir.join("samples.jsonl");
    let sample = Command::new("python3")
        .arg(&helper)
        .args(["sample", journal.to_str().unwrap(), "fixture-start"])
        .output()
        .unwrap();
    assert!(
        sample.status.success(),
        "{}",
        String::from_utf8_lossy(&sample.stderr)
    );
    let report = Command::new("python3")
        .arg(&helper)
        .args(["report", journal.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        report.status.success(),
        "{}",
        String::from_utf8_lossy(&report.stderr)
    );
    let value: Value = serde_json::from_slice(&report.stdout).unwrap();
    assert_eq!(value["samples"][0]["label"], "fixture-start");
}

#[test]
fn samples_keep_unknowns_and_reject_counter_reset() {
    let dir = scratch_dir("runtime-evidence");
    let cg = dir.join("cgroup");
    std::fs::create_dir_all(&cg).unwrap();
    std::fs::write(cg.join("cpu.stat"), "usage_usec 100\nnr_throttled 2\n").unwrap();
    let helper = repo_root().join("infra/gate-runner/runtime-evidence.py");
    let record = dir.join("samples.jsonl");
    for (label, stat) in [
        ("start", "usage_usec 100\nnr_throttled 2\n"),
        ("finish", "usage_usec 90\nnr_throttled 3\n"),
    ] {
        std::fs::write(cg.join("cpu.stat"), stat).unwrap();
        let out = Command::new("python3")
            .arg(&helper)
            .args(["sample", record.to_str().unwrap(), label])
            .env("BOSS_RUNTIME_CGROUP_ROOT", &cg)
            .env("CARGO_BUILD_JOBS", "7")
            .env("RUST_TEST_THREADS", "2")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let out = Command::new("python3")
        .arg(&helper)
        .args(["report", record.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["samples"][0]["workers"]["cargo"]["value"], 7);
    assert_eq!(
        value["samples"][0]["files"]["memory.current"]["state"],
        "unavailable"
    );
    assert_eq!(value["intervals"][0]["state"], "invalid");
    assert!(value["intervals"][0]["cpu_usage_usec"].is_null());
}

#[test]
fn production_receipts_and_check_boundaries_use_the_collector() {
    let root = repo_root();
    let gate = std::fs::read_to_string(root.join("infra/gate.sh")).unwrap();
    assert!(gate.contains("\"runtime_evidence\": $(runtime_evidence_report)"));
    assert!(gate.contains("runtime_evidence_sample \"check-start:${name}\""));
    assert!(gate.contains("runtime_evidence_sample \"check-finish:${name}\""));
    let runner = std::fs::read_to_string(root.join("infra/gate-runner/run.sh")).unwrap();
    assert!(runner.contains("runtime-evidence.py"));
    assert!(runner.contains("BOSS_GATE_RUNTIME_EVIDENCE"));
}

#[test]
fn browser_observation_cannot_be_replaced_by_configured_width() {
    let dir = scratch_dir("runtime-browser");
    let log = dir.join("browser.log");
    std::fs::write(&log, "Running 12 tests using 3 workers\n").unwrap();
    let helper = repo_root().join("infra/gate-runner/runtime-evidence.py");
    let out = Command::new("python3")
        .arg(helper)
        .args(["browser", log.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["state"], "measured");
    assert_eq!(value["workers"], 3);
    assert_eq!(value["tests"], 12);
    std::fs::write(log, "config workers=8\n").unwrap();
    let out = Command::new("python3")
        .arg(repo_root().join("infra/gate-runner/runtime-evidence.py"))
        .args(["browser", dir.join("browser.log").to_str().unwrap()])
        .output()
        .unwrap();
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["state"], "unavailable");
    assert!(value["workers"].is_null());
}

#[test]
fn runtime_identity_comes_from_read_only_downward_api() {
    let manifest =
        std::fs::read_to_string(repo_root().join("infra/gate-runner/gate-runner.yaml")).unwrap();
    for field in [
        "metadata.uid",
        "spec.nodeName",
        "requests.cpu",
        "limits.cpu",
        "requests.memory",
        "limits.memory",
    ] {
        assert!(
            manifest.contains(field),
            "missing runtime observation {field}"
        );
    }
}

#[test]
fn final_observations_survive_every_receipt_verdict() {
    let dir = scratch_dir("runtime-verdicts");
    let journal = dir.join("samples.jsonl");
    let helper = repo_root().join("infra/gate-runner/runtime-evidence.py");
    let out = Command::new("python3")
        .arg(&helper)
        .args(["sample", journal.to_str().unwrap(), "runner-finish"])
        .output()
        .unwrap();
    assert!(out.status.success());
    for verdict in ["green", "failed", "refused", "lost"] {
        let receipt = dir.join(format!("{verdict}.json"));
        std::fs::write(
            &receipt,
            format!("{{\"verdict\":\"{verdict}\",\"fails\":[\"original failure\"]}}"),
        )
        .unwrap();
        let out = Command::new("python3")
            .arg(&helper)
            .args([
                "merge",
                journal.to_str().unwrap(),
                receipt.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let value: Value = serde_json::from_slice(&std::fs::read(receipt).unwrap()).unwrap();
        assert_eq!(value["verdict"], verdict);
        assert_eq!(value["fails"][0], "original failure");
        assert_eq!(
            value["runtime_evidence"]["samples"][0]["label"],
            "runner-finish"
        );
    }
}

#[test]
fn retry_policy_is_retained_separately_from_observed_workers() {
    let dir = scratch_dir("runtime-policy");
    let journal = dir.join("samples.jsonl");
    std::fs::write(
        journal.with_extension("jsonl.browser-config"),
        "{\"workers\":8,\"retries\":0}",
    )
    .unwrap();
    let out = Command::new("python3")
        .arg(repo_root().join("infra/gate-runner/runtime-evidence.py"))
        .args(["report", journal.to_str().unwrap()])
        .output()
        .unwrap();
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["browser_policy"]["state"], "configured");
    assert_eq!(value["browser_policy"]["retries"], 0);
    assert_eq!(value["browser"]["state"], "unavailable");
    assert!(value["browser"]["workers"].is_null());
}

#[test]
fn receipt_bounds_are_explicit_and_identity_change_invalidates_deltas() {
    let dir = scratch_dir("runtime-bounds");
    let helper = repo_root().join("infra/gate-runner/runtime-evidence.py");
    let journal = dir.join("samples.jsonl");
    let out = Command::new("python3")
        .arg(&helper)
        .args(["sample", journal.to_str().unwrap(), "start"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let mut row: Value =
        serde_json::from_str(std::fs::read_to_string(&journal).unwrap().trim()).unwrap();
    row["files"]["cpu.stat"] =
        serde_json::json!({"state":"measured", "raw":"usage_usec 100\nnr_throttled 1\n"});
    let mut rows = vec![row.clone()];
    for index in 1..80 {
        row["monotonic_ns"] = serde_json::json!(row["monotonic_ns"].as_u64().unwrap() + 1000);
        row["identity"]["POD_UID"] = serde_json::json!(format!("changed-{index}"));
        rows.push(row.clone());
    }
    std::fs::write(
        &journal,
        rows.iter()
            .map(|row| format!("{row}\n"))
            .collect::<String>(),
    )
    .unwrap();
    let out = Command::new("python3")
        .arg(helper)
        .args(["report", journal.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(out.stdout.len() <= 65536);
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["state"], "partial");
    assert_eq!(value["sample_count"], 80);
    assert!(value["omitted_samples"].as_u64().unwrap() > 0);
    assert_eq!(value["intervals"][0]["state"], "invalid");
}

#[test]
fn compatible_samples_retain_monotonic_deltas_and_failed_collection() {
    let dir = scratch_dir("runtime-compatible");
    let helper = repo_root().join("infra/gate-runner/runtime-evidence.py");
    let journal = dir.join("samples.jsonl");
    assert!(
        Command::new("python3")
            .arg(&helper)
            .args(["sample", journal.to_str().unwrap(), "start"])
            .status()
            .unwrap()
            .success()
    );
    let mut row: Value =
        serde_json::from_str(std::fs::read_to_string(&journal).unwrap().trim()).unwrap();
    row["files"]["cpu.stat"] =
        serde_json::json!({"state":"measured", "raw":"usage_usec 100\nnr_throttled 1\n"});
    let before = row.clone();
    row["monotonic_ns"] = serde_json::json!(row["monotonic_ns"].as_u64().unwrap() + 1000);
    row["files"]["cpu.stat"]["raw"] = serde_json::json!("usage_usec 150\nnr_throttled 2\n");
    std::fs::write(&journal, format!("{before}\n{row}\n")).unwrap();
    std::fs::write(journal.with_extension("jsonl.failed"), "failed sample").unwrap();
    let out = Command::new("python3")
        .arg(helper)
        .args(["report", journal.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["intervals"][0]["state"], "measured");
    assert_eq!(value["intervals"][0]["cpu_usage_usec"], 50);
    assert_eq!(value["intervals"][0]["elapsed_ns"], 1000);
    assert_eq!(value["state"], "partial");
    assert!(
        value["collection_errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str().unwrap().contains("collection calls failed"))
    );
}

/// Write one cgroup state and take one sample of it through the real collector.
fn sample_pressure(
    cg: &std::path::Path,
    record: &std::path::Path,
    label: &str,
    files: &[(&str, &str)],
) {
    for (name, body) in files {
        std::fs::write(cg.join(name), body).unwrap();
    }
    let out = Command::new("python3")
        .arg(repo_root().join("infra/gate-runner/runtime-evidence.py"))
        .args(["sample", record.to_str().unwrap(), label])
        .env("BOSS_RUNTIME_CGROUP_ROOT", cg)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn psi(some: u64, full: u64) -> String {
    format!(
        "some avg10=0.00 avg60=0.00 avg300=0.00 total={some}\n\
         full avg10=0.00 avg60=0.00 avg300=0.00 total={full}\n"
    )
}

/// WHY (backlog 17f6170c, 2026-10-09). Whether a gate check waited on the
/// disk was the whole question of that item, and no receipt could answer
/// it: the collector read io.pressure, cpu.pressure and memory.pressure at
/// every check boundary, the interval kept only cpu.stat deltas, and the
/// receipt's byte bound dropped the samples (220 of 220 on gate-run
/// 5ea13e7d). The answer was recovered by decoding the raw journal out of
/// 66 gate pod logs, which the cluster keeps for about a day: the test
/// check waited on IO 43 percent of its wall on 2026-10-07 before the trim
/// and a median 9.3 percent after it. Each interval now carries the stall
/// microseconds of all three, so the next reader takes them off the record.
#[test]
fn an_interval_carries_how_long_the_check_waited_on_io_cpu_and_memory() {
    let dir = scratch_dir("runtime-pressure");
    let cg = dir.join("cgroup");
    std::fs::create_dir_all(&cg).unwrap();
    let record = dir.join("samples.jsonl");
    // memory.pressure is never written: absent evidence must not read as a zero.
    sample_pressure(
        &cg,
        &record,
        "check-start:test",
        &[
            ("cpu.stat", "usage_usec 1000\nthrottled_usec 0\n"),
            ("io.pressure", &psi(5_000, 4_000)),
            ("cpu.pressure", &psi(70, 0)),
        ],
    );
    sample_pressure(
        &cg,
        &record,
        "check-finish:test",
        &[
            ("cpu.stat", "usage_usec 9000\nthrottled_usec 0\n"),
            ("io.pressure", &psi(405_000, 304_000)),
            ("cpu.pressure", &psi(100, 0)),
        ],
    );
    let out = Command::new("python3")
        .arg(repo_root().join("infra/gate-runner/runtime-evidence.py"))
        .args(["report", record.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    let interval = &value["intervals"][0];
    assert_eq!(interval["state"], "measured", "{interval}");
    assert_eq!(interval["cpu_usage_usec"], 8000);
    let stall = &interval["stall_usec"];
    assert_eq!(stall["io.some"], 400_000, "{interval}");
    assert_eq!(stall["io.full"], 300_000, "{interval}");
    assert_eq!(stall["cpu.some"], 30, "{interval}");
    assert_eq!(stall["cpu.full"], 0, "a measured zero stays a zero");
    assert!(
        stall.get("memory.some").is_none(),
        "an unread file was reported as a stall: {interval}"
    );
    assert!(
        interval["stall_unavailable"]["memory"]
            .as_str()
            .is_some_and(|reason| !reason.is_empty()),
        "an unread pressure file must say why it has no stall: {interval}"
    );
}

/// A pressure total that went DOWN is a reset, not a negative stall; the
/// CPU delta beside it is still good evidence and stays measured.
#[test]
fn a_pressure_reset_names_itself_and_keeps_the_cpu_delta() {
    let dir = scratch_dir("runtime-pressure-reset");
    let cg = dir.join("cgroup");
    std::fs::create_dir_all(&cg).unwrap();
    let record = dir.join("samples.jsonl");
    for (label, usage, io) in [("start", 100, 9_000), ("finish", 150, 10)] {
        sample_pressure(
            &cg,
            &record,
            label,
            &[
                ("cpu.stat", &format!("usage_usec {usage}\n")),
                ("io.pressure", &psi(io, io)),
                ("cpu.pressure", &psi(1, 1)),
                ("memory.pressure", &psi(2, 2)),
            ],
        );
    }
    let out = Command::new("python3")
        .arg(repo_root().join("infra/gate-runner/runtime-evidence.py"))
        .args(["report", record.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    let interval = &value["intervals"][0];
    assert_eq!(interval["state"], "measured", "{interval}");
    assert_eq!(interval["cpu_usage_usec"], 50);
    assert!(
        interval["stall_usec"].get("io.some").is_none(),
        "{interval}"
    );
    assert!(
        interval["stall_unavailable"]["io"]
            .as_str()
            .is_some_and(|reason| reason.contains("reset")),
        "{interval}"
    );
    assert_eq!(interval["stall_usec"]["memory.some"], 0);
}

#[test]
fn malformed_journal_diagnostics_are_bounded_not_healthy() {
    let dir = scratch_dir("runtime-malformed");
    let journal = dir.join("samples.jsonl");
    std::fs::write(&journal, "bad\n".repeat(3000)).unwrap();
    let out = Command::new("python3")
        .arg(repo_root().join("infra/gate-runner/runtime-evidence.py"))
        .args(["report", journal.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(
        out.stdout.len() <= 65536,
        "diagnostics escaped receipt bound"
    );
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["state"], "unavailable");
    assert!(value["omitted_rows"].as_u64().unwrap() > 0);
}

/// Run the real collector's `report` over a journal, from `cwd`, with extra env.
fn report_from(journal: &std::path::Path, cwd: &std::path::Path, env: &[(&str, &str)]) -> Value {
    let out = Command::new("python3")
        .arg(repo_root().join("infra/gate-runner/runtime-evidence.py"))
        .args(["report", journal.to_str().unwrap()])
        .current_dir(cwd)
        .envs(env.iter().copied())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stdout.len() <= 65536,
        "receipt evidence escaped its bound"
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

/// WHY (backlog f42bd52f, car 2 of design 121f44a1, D4). The gate drive's
/// byte budget needs bytes per device per PHASE, and the receipt already
/// cannot carry them per interval: on 2026-11-12 two green receipts kept 115
/// of ~225 intervals and the last one kept was a pre-flight lint, so build
/// and test — the phases that write the drive — never reach the record. The
/// first attempt at this car (bb4eeee6) put io on every interval and kept 79
/// of 229. So the bytes are a SUMMARY over every sample, computed before the
/// reductions, like pgdata and memory: the heaviest phases named, the rest
/// folded into one count, the gaps between phases beside them, and the three
/// summing exactly to the endpoint total (conservation).
#[test]
fn a_receipt_carries_io_bytes_per_device_per_phase_over_every_sample() {
    let dir = scratch_dir("runtime-io-phases");
    let cg = dir.join("cgroup");
    std::fs::create_dir(&cg).unwrap();
    let journal = dir.join("samples.jsonl");
    let mut labels = vec!["runner-start".to_string(), "gate-start".to_string()];
    for n in 0..40 {
        labels.push(format!("check-start:c{n}"));
        labels.push(format!("check-finish:c{n}"));
    }
    labels.push("receipt:green".to_string());
    // Each gap reads 7 bytes off 259:0; phase cN writes (N+1)*1000 bytes to
    // 259:6 and reads N+1. Counters are cumulative, as the kernel's are.
    let (mut r0, w0, mut r6, mut w6) = (100u64, 50u64, 10u64, 20u64);
    for (index, label) in labels.iter().enumerate() {
        if index > 0 {
            match label.strip_prefix("check-finish:c") {
                Some(n) => {
                    let n: u64 = n.parse().unwrap();
                    r6 += n + 1;
                    w6 += (n + 1) * 1000;
                }
                None => r0 += 7,
            }
        }
        // The kernel's own line shape, in either device order.
        let a = format!("259:0 rbytes={r0} wbytes={w0} rios=1 wios=1 dbytes=0 dios=0");
        let b = format!("259:6 rbytes={r6} wbytes={w6} rios=1 wios=1 dbytes=0 dios=0");
        let io = if index % 2 == 0 {
            format!("{a}\n{b}\n")
        } else {
            format!("{b}\n{a}\n")
        };
        sample_pressure(
            &cg,
            &journal,
            label,
            &[("cpu.stat", "usage_usec 100\n"), ("io.stat", &io)],
        );
    }
    let value = report_from(&journal, &dir, &[]);
    assert!(
        value["omitted_samples"].as_u64().unwrap() > 0,
        "the fixture must exercise the receipt's reductions"
    );
    let io = &value["io"];
    assert_eq!(io["state"], "measured", "{io}");
    assert_eq!(io["from"], "runner-start");
    assert_eq!(io["to"], "receipt:green");
    assert_eq!(io["devices"]["259:0"]["rbytes"], 42 * 7);
    assert_eq!(io["devices"]["259:0"]["wbytes"], w0 - 50, "a measured zero");
    assert_eq!(
        io["devices"]["259:6"]["wbytes"],
        1000 * (1..=40).sum::<u64>()
    );
    assert_eq!(io["devices"]["259:6"]["rbytes"], (1..=40).sum::<u64>());
    assert_eq!(value["bounds"]["io_phases"], 16);
    // The sixteen heaviest phases, named, in the order they ran.
    let phases = io["phases"].as_array().unwrap();
    let names: Vec<&str> = phases
        .iter()
        .map(|p| p["phase"].as_str().unwrap())
        .collect();
    let expected: Vec<String> = (24..40).map(|n| format!("c{n}")).collect();
    assert_eq!(names, expected, "{io}");
    assert_eq!(phases[0]["devices"]["259:6"]["wbytes"], 25_000);
    assert_eq!(phases[0]["devices"]["259:6"]["rbytes"], 25);
    assert_eq!(
        phases[0]["devices"]["259:0"]["rbytes"], 0,
        "a measured zero is a zero"
    );
    assert_eq!(io["other_phases"]["count"], 24);
    assert_eq!(
        io["other_phases"]["devices"]["259:6"]["wbytes"],
        1000 * (1..=24).sum::<u64>()
    );
    assert_eq!(io["between_phases"]["devices"]["259:0"]["rbytes"], 42 * 7);
    assert_eq!(io["between_phases"]["devices"]["259:6"]["wbytes"], 0);
    // Conservation: named + other + between is the endpoint total, per key.
    for device in ["259:0", "259:6"] {
        for key in ["rbytes", "wbytes"] {
            let named: u64 = phases
                .iter()
                .map(|p| p["devices"][device][key].as_u64().unwrap())
                .sum();
            let other = io["other_phases"]["devices"][device][key].as_u64().unwrap();
            let gaps = io["between_phases"]["devices"][device][key]
                .as_u64()
                .unwrap();
            assert_eq!(
                named + other + gaps,
                io["devices"][device][key].as_u64().unwrap(),
                "{device} {key} does not conserve: {io}"
            );
        }
    }
    // Not per interval: that costs the intervals the receipt already drops.
    for interval in value["intervals"].as_array().unwrap() {
        assert!(interval.get("io").is_none(), "{interval}");
    }
    // The raw reading is retained whole in the journal.
    let first: Value = serde_json::from_str(
        std::fs::read_to_string(&journal)
            .unwrap()
            .lines()
            .next()
            .unwrap(),
    )
    .unwrap();
    assert!(
        first["files"]["io.stat"]["raw"]
            .as_str()
            .unwrap()
            .contains("dios=0")
    );
}

/// An unread, truncated, malformed, reset or re-identified io.stat is never a
/// zero, never a prefix, and never takes the CPU evidence beside it down.
#[test]
fn io_unknowns_never_become_zero_or_erase_the_cpu_sibling() {
    let dir = scratch_dir("runtime-io-unknowns");
    let cg = dir.join("cgroup");
    std::fs::create_dir(&cg).unwrap();
    let journal = dir.join("samples.jsonl");
    sample_pressure(
        &cg,
        &journal,
        "check-start:test",
        &[
            ("cpu.stat", "usage_usec 100\n"),
            ("io.stat", "8:0 rbytes=10 wbytes=20\n"),
        ],
    );
    let first: Value =
        serde_json::from_str(std::fs::read_to_string(&journal).unwrap().trim()).unwrap();
    let mut last = first.clone();
    last["label"] = serde_json::json!("check-finish:test");
    last["monotonic_ns"] = serde_json::json!(first["monotonic_ns"].as_u64().unwrap() + 1000);
    last["files"]["cpu.stat"]["raw"] = serde_json::json!("usage_usec 150\n");
    last["files"]["io.stat"]["raw"] = serde_json::json!("8:0 wbytes=40 rbytes=30\n");
    let run = |rows: &[Value]| -> Value {
        let raw = rows
            .iter()
            .map(|row| format!("{row}\n"))
            .collect::<String>();
        std::fs::write(&journal, &raw).unwrap();
        let value = report_from(&journal, &dir, &[]);
        assert_eq!(std::fs::read_to_string(&journal).unwrap(), raw);
        value
    };
    let good = run(&[first.clone(), last.clone()]);
    assert_eq!(good["io"]["state"], "measured", "{good}");
    assert_eq!(good["io"]["devices"]["8:0"]["rbytes"], 20);
    assert_eq!(good["io"]["phases"][0]["phase"], "test");
    for observation in [
        serde_json::json!({"state":"unavailable","reason":"fixture read refused"}),
        serde_json::json!({"state":"truncated","raw":"8:0 rbytes=30 wbytes=40"}),
        serde_json::json!({"state":"measured","raw":"8:0 rbytes=9 wbytes=40"}),
        serde_json::json!({"state":"measured","raw":"8:0 rbytes=30"}),
        serde_json::json!({"state":"measured","raw":"8:0 rbytes=30 wbytes=-1"}),
        serde_json::json!({"state":"measured","raw":"8:0 rbytes=30 rbytes=31 wbytes=40"}),
        serde_json::json!({"state":"measured","raw":"8:0 rbytes=30 wbytes=40\n8:0 rbytes=30 wbytes=40"}),
        serde_json::json!({"state":"measured","raw":"253:0 rbytes=30 wbytes=40"}),
    ] {
        let mut bad = last.clone();
        bad["files"]["io.stat"] = observation.clone();
        let value = run(&[first.clone(), bad]);
        assert_eq!(value["io"]["state"], "unavailable", "{observation}");
        assert!(
            value["io"]["reason"]
                .as_str()
                .is_some_and(|r| !r.is_empty()),
            "an unavailable io must say why: {observation}"
        );
        assert!(value["io"].get("devices").is_none(), "{observation}");
        assert!(value["io"].get("phases").is_none(), "{observation}");
        assert_eq!(value["state"], "partial");
        assert_eq!(value["intervals"][0]["cpu_usage_usec"], 50);
    }
    // A journal from before io.stat was read stays readable; its bytes are unknown.
    let mut legacy = first.clone();
    legacy["files"].as_object_mut().unwrap().remove("io.stat");
    let value = run(&[legacy, last.clone()]);
    assert_eq!(value["sample_count"], 2);
    assert_eq!(value["io"]["state"], "unavailable");
    // A CPU reset spoils the CPU interval and nothing of the bytes.
    let mut cpu_reset = last.clone();
    cpu_reset["files"]["cpu.stat"]["raw"] = serde_json::json!("usage_usec 1\n");
    let value = run(&[first.clone(), cpu_reset]);
    assert_eq!(value["intervals"][0]["state"], "invalid");
    assert_eq!(value["io"]["devices"]["8:0"]["rbytes"], 20);
    // Monotonic endpoints cannot conceal an intervening reset.
    let mut middle = last.clone();
    middle["label"] = serde_json::json!("middle");
    middle["monotonic_ns"] = serde_json::json!(first["monotonic_ns"].as_u64().unwrap() + 500);
    middle["files"]["io.stat"]["raw"] = serde_json::json!("8:0 rbytes=5 wbytes=5\n");
    assert_eq!(
        run(&[first.clone(), middle, last.clone()])["io"]["state"],
        "unavailable"
    );
    for field in ["identity", "lifetime"] {
        let mut changed = last.clone();
        if field == "identity" {
            changed[field]["POD_UID"] = serde_json::json!("another-pod");
        } else {
            changed[field][1] = serde_json::json!(0);
        }
        assert_eq!(run(&[first.clone(), changed])["io"]["state"], "unavailable");
    }
    // A real over-bound file is retained as truncated, never parsed as a prefix.
    std::fs::write(&journal, "").unwrap();
    let huge = "8:0 rbytes=30 wbytes=40\n".repeat(200);
    sample_pressure(&cg, &journal, "bounded", &[("io.stat", &huge)]);
    let bounded: Value =
        serde_json::from_str(std::fs::read_to_string(&journal).unwrap().trim()).unwrap();
    assert_eq!(bounded["files"]["io.stat"]["state"], "truncated");
    assert_eq!(
        bounded["files"]["io.stat"]["raw"].as_str().unwrap().len(),
        2048
    );
}

/// WHY (2026-11-12). io.stat keys a WHOLE DISK (blk-cgroup counts per
/// request queue), while the gate volume's st_dev is its PARTITION: tonight
/// it moved to w-1's nvme1n1p1, 259:7 (66311, proof 690b7fa7), and io.stat
/// counts it under nvme1n1, 259:6. A reader looking the volume's own device
/// up in the byte table finds nothing. So the receipt names the disk under
/// the gate's working directory, read from that directory's stat and sysfs
/// at report time — never typed: the last layout's numbers went stale the
/// night the volume moved.
#[test]
fn the_receipt_names_the_disk_under_the_gate_workspace_from_its_own_stat() {
    let dir = scratch_dir("runtime-io-workspace");
    let workspace = dir.join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    // The oracle is GNU stat, not the collector's own arithmetic.
    let stat = Command::new("stat")
        .args(["-c", "%Hd:%Ld"])
        .arg(&workspace)
        .output()
        .unwrap();
    assert!(stat.status.success());
    let device = String::from_utf8(stat.stdout).unwrap().trim().to_string();
    let sys = dir.join("sys");
    let part = sys.join("devices/disk/part");
    std::fs::create_dir_all(&part).unwrap();
    std::fs::create_dir_all(sys.join("block")).unwrap();
    std::fs::write(sys.join("devices/disk/dev"), "259:42\n").unwrap();
    std::fs::write(part.join("dev"), format!("{device}\n")).unwrap();
    std::fs::write(part.join("partition"), "1\n").unwrap();
    std::os::unix::fs::symlink("../devices/disk/part", sys.join("block").join(&device)).unwrap();
    let cg = dir.join("cgroup");
    std::fs::create_dir(&cg).unwrap();
    let journal = dir.join("samples.jsonl");
    for (label, bytes) in [("check-start:test", 1), ("check-finish:test", 9)] {
        sample_pressure(
            &cg,
            &journal,
            label,
            &[
                ("cpu.stat", "usage_usec 100\n"),
                (
                    "io.stat",
                    &format!("259:42 rbytes={bytes} wbytes={bytes}\n"),
                ),
            ],
        );
    }
    let block = sys.join("block");
    let env = [("BOSS_RUNTIME_SYS_BLOCK", block.to_str().unwrap())];
    let value = report_from(&journal, &workspace, &env);
    let named = &value["io"]["workspace"];
    assert_eq!(named["state"], "measured", "{named}");
    assert_eq!(named["device"], device.as_str());
    assert_eq!(
        named["disk"], "259:42",
        "a partition is counted under its disk"
    );
    assert_eq!(
        named["path"],
        workspace.canonicalize().unwrap().to_str().unwrap()
    );
    assert_eq!(
        value["io"]["devices"][named["disk"].as_str().unwrap()]["wbytes"],
        8
    );
    // A whole disk is its own key.
    std::fs::remove_file(part.join("partition")).unwrap();
    let value = report_from(&journal, &workspace, &env);
    assert_eq!(value["io"]["workspace"]["disk"], device.as_str());
    // No sysfs entry: the device is still said, the disk is not guessed.
    let empty = dir.join("empty");
    std::fs::create_dir(&empty).unwrap();
    let value = report_from(
        &journal,
        &workspace,
        &[("BOSS_RUNTIME_SYS_BLOCK", empty.to_str().unwrap())],
    );
    let named = &value["io"]["workspace"];
    assert_eq!(named["state"], "partial", "{named}");
    assert_eq!(named["device"], device.as_str());
    assert!(named.get("disk").is_none(), "{named}");
    assert!(named["reason"].as_str().is_some_and(|r| !r.is_empty()));
}
