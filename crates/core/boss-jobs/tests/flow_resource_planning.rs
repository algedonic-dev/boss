use boss_jobs::flow_resources::{Amount, Capacity, Demand, plan};

#[test]
fn exact_amounts_do_not_admit_a_rounded_last_unit() {
    let cap = Amount::parse("0.3").unwrap();
    let held = Amount::parse("0.2").unwrap();
    let need = Amount::parse("0.1000000000000000000000000001").unwrap();
    assert!(plan(&[Demand::flow("vcpu", 1, need, Capacity::Known(cap), held)]).is_err());
    assert!(
        Amount::parse("0.00000000000000000000000000001").is_err(),
        "unrepresentable refuses"
    );
}

#[test]
fn an_unrepresentable_sum_refuses_instead_of_rounding_a_small_demand_away() {
    let full = Amount::parse("10000000000000000000000000000").unwrap();
    let smallest = Amount::parse("0.0000000000000000000000000001").unwrap();
    assert!(
        full.checked_add(&smallest).is_err(),
        "decimal arithmetic must not silently discard a positive dimension"
    );
    assert!(
        plan(&[Demand::flow(
            "vcpu",
            1,
            smallest,
            Capacity::Known(full.clone()),
            full
        )])
        .is_err()
    );
}

#[test]
fn a_refused_vector_names_the_first_dimension_without_a_partial_plan() {
    let one = Amount::parse("1").unwrap();
    let zero = Amount::parse("0").unwrap();
    let demands = vec![
        Demand::flow("vcpu", 1, one.clone(), Capacity::Unknown, zero.clone()),
        Demand::flow(
            "ram_gb",
            1,
            one.clone(),
            Capacity::Known(zero.clone()),
            zero,
        ),
    ];
    let refusal = plan(&demands).unwrap_err();
    assert_eq!(refusal.pool, "ram_gb");
    assert_eq!(refusal.reason, "capacity-exhausted");
    let mut reversed = demands;
    reversed.reverse();
    assert_eq!(plan(&reversed).unwrap_err(), refusal);
}

#[test]
fn unknown_capacity_is_never_free() {
    let one = Amount::parse("1").unwrap();
    let zero = Amount::parse("0").unwrap();
    let unknown = Demand::flow("vcpu", 1, one.clone(), Capacity::Unknown, zero.clone());
    assert_eq!(plan(&[unknown]).unwrap_err().reason, "capacity-unknown");
}

#[test]
fn supported_stock_counts_held_obligations_against_exact_capacity() {
    let one = Amount::parse("1").unwrap();
    let zero = Amount::zero();
    let mut stock = Demand::flow(
        "disk:ci-workspaces",
        1,
        one.clone(),
        Capacity::Known(one.clone()),
        zero,
    );
    stock.stock = true;
    // Car 2b implements STOCK admission and keeps physical obligations charged.
    // Lifecycle conservation is shared by the Memory/Pg claim contract suite.
    let allocated = plan(&[stock.clone()]).unwrap();
    assert_eq!(allocated.len(), 1);
    assert_eq!(allocated[0].pool, stock.pool);
    assert_eq!(allocated[0].version, stock.version);
    assert_eq!(allocated[0].amount, one.clone());

    stock.held = one;
    let refusal = plan(&[stock.clone()]).unwrap_err();
    assert_eq!(refusal.pool, stock.pool);
    assert_eq!(refusal.reason, "capacity-exhausted");
    assert_eq!(refusal.observed, vec![stock.clone()]);

    stock.capacity = Capacity::Unknown;
    assert_eq!(plan(&[stock]).unwrap_err().reason, "capacity-unknown");
}
