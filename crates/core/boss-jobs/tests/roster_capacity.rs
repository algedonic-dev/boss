use boss_jobs::pools::PoolSpec;

#[test]
fn a_roster_pool_declares_eligibility_as_class_codes() {
    let declaration = serde_json::json!({
        "name": "people:fixture-role", "version": 1, "status": "active",
        "unit": "people", "kind": "flow", "capacity": {"state": "unknown"},
        "roster": {"eligibility": [
            {"attribute": "role", "codes": ["fixture-role"]},
            {"attribute": "status", "codes": ["working"]},
            {"attribute": "employment_type", "codes": ["staff", "contract"]}
        ], "basis": null}
    });
    let parsed = serde_json::from_value::<PoolSpec>(declaration.clone());
    assert!(
        parsed.is_ok(),
        "Class-coded roster declaration refused: {parsed:?}"
    );
    let spec = parsed.unwrap();
    assert!(spec.validate().is_ok());
    let value = serde_json::to_value(spec).unwrap();
    assert_eq!(value["roster"], declaration["roster"]);
    assert_eq!(value["capacity"], declaration["capacity"]);
    let mut spoof = declaration.clone();
    spoof["name"] = "robots:fixture-role".into();
    assert!(
        serde_json::from_value::<PoolSpec>(spoof)
            .unwrap()
            .validate()
            .is_err()
    );
    let mut mismatch = declaration;
    mismatch["roster"]["eligibility"][0]["codes"] = serde_json::json!(["another-role"]);
    assert!(
        serde_json::from_value::<PoolSpec>(mismatch)
            .unwrap()
            .validate()
            .is_err()
    );
}
