#[test]
fn an_authored_resource_decimal_cannot_silently_round_before_admission() {
    let authored = r#"{"vcpu":0.1000000000000000000000000001}"#;
    if let Ok(bill) = serde_json::from_str::<boss_jobs::resources::ResourceBill>(authored) {
        assert_eq!(
            serde_json::to_string(&bill).unwrap(),
            authored,
            "unrepresentable input may refuse, but admission cannot compare a different quantity from the authored protocol"
        );
    }
}

#[test]
fn all_wire_quantities_are_exact_strings_including_integer_values() {
    use boss_jobs::{pools::PoolCapacity, resources::ResourceBill};
    let original = r#"{"vcpu":"0.1000000000000000000000000001","ram_gb":"1.000"}"#;
    let bill: ResourceBill = serde_json::from_str(original).expect("exact string bill");
    assert_eq!(serde_json::to_string(&bill).unwrap(), original);
    for amount in [serde_json::json!(1), serde_json::json!(0.1)] {
        assert!(
            serde_json::from_value::<ResourceBill>(serde_json::json!({"vcpu":amount})).is_err()
        );
        assert!(
            serde_json::from_value::<PoolCapacity>(
                serde_json::json!({"state":"configured","amount":amount})
            )
            .is_err()
        );
    }
    assert!(
        boss_jobs::flow_resources::Amount::parse("1.2.0").is_err(),
        "trailing-zero normalization must not repair malformed input"
    );
    assert_eq!(
        boss_jobs::flow_resources::Amount::parse("10000000000000000000000000001.0")
            .unwrap()
            .to_string(),
        "10000000000000000000000000001"
    );
}

#[test]
fn toml_integers_become_strings_but_floats_name_the_source_and_key() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("vcpu.toml");
    let source = "[[pool]]\nname='vcpu'\nversion=1\nstatus='active'\nunit='vCPU'\nkind='flow'\ncapacity={state='configured',amount=1}\n";
    std::fs::write(&file, source).unwrap();
    let pools = boss_jobs::pools::load_pools(&file).unwrap();
    assert_eq!(
        serde_json::to_value(&pools[0]).unwrap()["capacity"]["amount"],
        "1"
    );
    std::fs::write(&file, source.replace("amount=1", "amount=0.1")).unwrap();
    let error = boss_jobs::pools::load_pools(&file).unwrap_err().to_string();
    assert!(
        error.contains("vcpu.toml")
            && error.contains("capacity.amount")
            && error.contains("quoted"),
        "{error}"
    );
}
