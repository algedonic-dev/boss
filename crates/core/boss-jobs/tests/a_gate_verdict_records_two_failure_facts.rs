//! A gate-run's `record-verdict` step carries an equipment fault and a
//! product defect as two fields (item c02fc3b3; docs/design/factory-
//! taxonomy.md M4; register entry an-equipment-fault-is-not-a-product-
//! defect).
//!
//! WHY BOTH HALVES OF THE CONTRACT. The step API validates a completion
//! twice: `validate_metadata` by the step's KIND (the `gate-verdict`
//! bundle in seeds/step_types.toml) and then the Workflow step's own
//! `fields` (infra/platform/workflows/gate-run.toml). Unknown keys pass
//! both, so a field declared in neither would still be written — and
//! nothing would refuse a fact written as a string, which every later
//! reader would then have to second-guess. Declared `object` in both,
//! a malformed fact is refused at the write, and a reader of either row
//! learns the field exists. `boss_jobs::failure_facts` writes it; what
//! it writes passes here.

use boss_jobs::failure_facts::{EQUIPMENT_FAULT, PRODUCT_DEFECT, failure_facts};
use boss_jobs::registry::seedable_platform_workflows;
use boss_jobs::step_registry::StepRegistry;
use serde_json::{Value, json};

const FACTS: [&str; 2] = [EQUIPMENT_FAULT, PRODUCT_DEFECT];

fn record_verdict_fields() -> Vec<boss_core::job::StepField> {
    seedable_platform_workflows()
        .into_iter()
        .find(|w| w.kind == "gate-run")
        .expect("the gate-run protocol is in the platform bundle")
        .steps
        .into_iter()
        .find(|s| s.title == "record-verdict")
        .expect("record-verdict step")
        .fields
}

#[test]
fn both_rows_declare_each_fact_as_an_optional_object() {
    let registry = StepRegistry::v1();
    let st = registry
        .get("gate-verdict")
        .expect("the gate-verdict step type ships in the v1 bundle");
    let protocol = record_verdict_fields();
    for fact in FACTS {
        let kind = st
            .fields
            .iter()
            .find(|f| f.name == fact)
            .unwrap_or_else(|| panic!("the gate-verdict step type declares no `{fact}`"));
        assert_eq!(kind.field_type, "object", "{fact} on the step type");
        assert!(!kind.required, "{fact} is absent on a green: optional");
        let own = protocol
            .iter()
            .find(|f| f.name == fact)
            .unwrap_or_else(|| panic!("gate-run's record-verdict declares no `{fact}`"));
        assert_eq!(own.field_type, "object", "{fact} on the protocol");
        assert!(!own.required, "{fact} on the protocol is optional");
    }
}

#[test]
fn what_the_writer_records_passes_both_validators_and_a_string_does_not() {
    let registry = StepRegistry::v1();
    let protocol = record_verdict_fields();
    let receipt = json!({
        "verdict": "failed",
        "checks": [{"name": "test", "result": "fail"}],
        "fails": ["test: a::b - panicked at x.rs:1:1: assertion failed: ok"],
    });
    let mut md = serde_json::Map::new();
    md.insert("verdict".into(), json!("failed"));
    md.insert("receipt".into(), json!(receipt.to_string()));
    let facts = failure_facts("failed", Some(&receipt)).fields();
    assert!(!facts.is_empty(), "a red records a fact");
    for (k, v) in facts {
        md.insert(k.into(), v);
    }
    let md = Value::Object(md);
    registry
        .validate_metadata("gate-verdict", &md)
        .unwrap_or_else(|e| panic!("the kind bundle refused the writer's facts: {e:?}"));
    StepRegistry::validate_authored_fields(&protocol, &md)
        .unwrap_or_else(|e| panic!("the protocol refused the writer's facts: {e:?}"));
    for fact in FACTS {
        let bad = json!({"verdict": "failed", fact: "equipment"});
        assert!(
            registry.validate_metadata("gate-verdict", &bad).is_err(),
            "{fact} as a string must be refused by the kind bundle"
        );
        assert!(
            StepRegistry::validate_authored_fields(&protocol, &bad).is_err(),
            "{fact} as a string must be refused by the protocol"
        );
    }
}
