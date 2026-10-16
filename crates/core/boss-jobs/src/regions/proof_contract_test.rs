//! One recorded-evidence corpus pins the Rust and web readers (df8fe965).

use super::*;
use crate::regions::fixtures::*;

#[test]
fn the_shared_proof_contract_matches_the_actual_backend() {
    let cases: Vec<Value> = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../apps/web/src/it/yard/proof-contract.json"
    )))
    .unwrap();
    assert!(!cases.is_empty(), "an empty contract proves no reader");
    for (index, case) in cases.iter().enumerate() {
        assert_eq!(case["index"], json!(index));
        let name = case["name"].as_str().unwrap();
        let actual = actual_contract(case);
        assert_eq!(actual, case["expected"], "case {index}: {name}");
        if let Some(expected) = case.get("expected_streak") {
            let streak = crate::car::not_yet_streak(&case["metadata"]).map(|streak| {
                json!({"hours": streak.hours.to_string(), "runs": streak.runs.to_string()})
            });
            assert_eq!(json!(streak), *expected, "streak case {index}: {name}");
        }
    }
}

fn actual_contract(case: &Value) -> Value {
    let now = t(case["now"].as_str().unwrap());
    let mut packet = job(
        "ship-a-change",
        case["name"].as_str().unwrap(),
        JobStatus::Open,
        case["metadata"].clone(),
    );
    packet.opened_at = match &case["opened_at"] {
        Value::Null => None,
        Value::String(value) => Some(t(value)),
        other => panic!("unread native opening time: {other}"),
    };
    let mut proven = step(&packet, "proven", StepStatus::Ready, None);
    match &case["claim"] {
        Value::Null => {}
        Value::Object(claim) => {
            proven.status = match claim["status"].as_str().unwrap() {
                "active" => StepStatus::Active,
                "ready" => StepStatus::Ready,
                other => panic!("unread claim status: {other}"),
            };
            proven.assignee_id = Some(claim["assignee"].as_str().unwrap().to_string());
        }
        other => panic!("unread native claim: {other}"),
    }
    let steps = vec![proven];
    let age = packet.opened_at.map(|at| (now - at).num_seconds());
    let wait = recorded_proof_wait(&packet, &steps, now);
    let fault = format!("{:?}", proof_fault(&packet.metadata, age));
    let action = proof_requires_action(&packet.metadata, age) || wait.is_err();
    let wait_reason = match wait {
        Err(reason) => reason,
        Ok(Some(_)) => "qualified".into(),
        Ok(None) => "none".into(),
    };
    let cars = vec![(packet, steps)];
    let status = empty_status();
    let mut input = inputs(&status, &[], &[], &cars, &[], Some(&[]), Some(&[]));
    input.now = now;
    let map = regions(&input);
    json!({
        "fault": fault,
        "action": action,
        "wait": wait_reason,
        "state": format!("{:?}", by_name(&map, "shed").state),
    })
}

#[test]
fn invalid_wire_numeric_documents_are_refused_by_the_actual_json_reader() {
    let documents: Vec<String> = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../apps/web/src/it/yard/proof-invalid-json.json"
    )))
    .unwrap();
    assert!(!documents.is_empty());
    for document in documents {
        assert!(
            serde_json::from_str::<Value>(&document).is_err(),
            "{document}"
        );
    }
}

#[test]
fn the_shared_string_contract_is_the_actual_rust_whitespace_set() {
    let contract: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../apps/web/src/it/yard/proof-strings.json"
    )))
    .unwrap();
    let actual: Vec<u32> = (0..=0x10ffff)
        .filter_map(char::from_u32)
        .filter(|character| character.is_whitespace())
        .map(u32::from)
        .collect();
    assert_eq!(json!(actual), contract["white_space"]);
    for code in contract["non_white_space"].as_array().unwrap() {
        let character = char::from_u32(code.as_u64().unwrap() as u32).unwrap();
        assert!(!character.is_whitespace());
        assert_eq!(character.to_string().trim(), character.to_string());
    }
}
