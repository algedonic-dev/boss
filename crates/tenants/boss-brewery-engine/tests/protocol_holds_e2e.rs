//! Layer-1 of the Job-completeness validator
//! (`docs/design/correctness-protocol.md`): every JobKindSpec in the
//! brewery seed bundle must pass the static lint. New JobKinds that bind
//! a side-effect-bearing StepType without filling required metadata fail
//! this test before publish — catching the bypass at authoring time
//! instead of waiting for a runtime "metadata X missing" log.
//!
//! Step-completion volumes, metadata shapes, and side-effect wiring are
//! validated end-to-end by the live 365-day regen, where the server
//! materializes steps and the workforce executor drives them through the
//! public API — not in-process here.

use std::path::PathBuf;

fn brewery_seeds_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
        .join("examples/brewery/seeds")
}

#[test]
fn brewery_job_kinds_pass_layer1_lint() {
    use boss_jobs::job_kind_lint::validate_all;
    use boss_jobs::seed_loader::load_job_kinds_with_owning_team;
    use boss_jobs::step_registry::StepRegistry;

    let kinds_path = brewery_seeds_dir().join("job_kinds.toml");
    let kinds = load_job_kinds_with_owning_team(&kinds_path, "brewery")
        .expect("brewery job_kinds.toml parses");
    let registry = StepRegistry::v1();
    let errs = validate_all(&kinds, &registry);
    if !errs.is_empty() {
        let mut msg = String::from("brewery JobKinds failed layer-1 lint:\n");
        for e in &errs {
            msg.push_str(&format!("  {}\n", e));
        }
        panic!("{msg}");
    }
}

/// A wholesale keg order must reference ONE consistent SKU set across its
/// whole lifecycle: confirm-order → stock-check → pull-and-stage →
/// deliver (shipment) → invoice (billing). If the shipment step's
/// `line_items` drift from the others, the brewery invoices customers for
/// kegs it never ships (revenue without fulfillment) and the unshipped
/// styles never draw down finished-goods stock — the demand-mix gap
/// behind the finished-goods glut. This guards the whole class of drift,
/// not just the instance we found (deliver omitting 3 of 5 SKUs).
#[test]
fn wholesale_order_references_one_consistent_sku_set() {
    use boss_jobs::seed_loader::load_job_kinds_with_owning_team;
    use std::collections::BTreeSet;

    let kinds_path = brewery_seeds_dir().join("job_kinds.toml");
    let kinds = load_job_kinds_with_owning_team(&kinds_path, "brewery")
        .expect("brewery job_kinds.toml parses");

    let order = kinds
        .iter()
        .find(|k| k.kind == "wholesale-keg-order")
        .expect("wholesale-keg-order JobKind present");

    // Each step that carries `line_items` → (step title, its SKU set).
    let per_step: Vec<(String, BTreeSet<String>)> = order
        .steps
        .iter()
        .filter_map(|s| {
            let skus: BTreeSet<String> = s
                .metadata_defaults
                .get("line_items")
                .and_then(|v| v.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|it| it.get("sku").and_then(|x| x.as_str()).map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            (!skus.is_empty()).then_some((s.title.clone(), skus))
        })
        .collect();

    assert!(
        per_step.len() >= 2,
        "expected several line_items-bearing steps in wholesale-keg-order, found {}",
        per_step.len()
    );

    let (ref_title, ref_skus) = &per_step[0];
    for (title, skus) in &per_step[1..] {
        assert_eq!(
            skus, ref_skus,
            "wholesale-keg-order step `{title}` references SKUs {skus:?} but step \
             `{ref_title}` references {ref_skus:?} — every step of an order must carry \
             the same SKU set so the brewery ships exactly what it bills (and every \
             style draws down stock)"
        );
    }
}
