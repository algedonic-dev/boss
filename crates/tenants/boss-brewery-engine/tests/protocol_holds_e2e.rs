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

/// Every `overhead_absorbed` stamp in the brewery bundle must equal the
/// model's global per-bbl driver rates × that batch's bbl, and every
/// producing JobKind must carry the stamps at all. The rates are model
/// inputs stated once (below and in the pale-ale mash-in comment); the
/// amounts in the seed are hand-multiplied products, and nothing else
/// enforces the multiplication — a re-rate or a batch resize that misses
/// one style silently ships a divergent per-bbl burden (and a memo that
/// contradicts its own amount). This test is that enforcement: change a
/// rate → change it here + regenerate every stamped amount it names.
#[test]
fn overhead_absorption_stamps_equal_rate_times_batch_bbl() {
    use boss_jobs::seed_loader::load_job_kinds_with_owning_team;

    // (credit_account, cents/bbl, driver) — derivation in the pale-ale
    // mash-in comment: $36.60 + $5.60 + $8.60 per bbl at the ~262k-bbl/yr
    // design volume.
    const DRIVERS: [(&str, i64, &str); 3] = [
        ("6100", 3660, "Direct labor"),
        ("6300", 560, "Process utilities"),
        ("6900", 860, "Production depreciation"),
    ];

    let kinds_path = brewery_seeds_dir().join("job_kinds.toml");
    let kinds = load_job_kinds_with_owning_team(&kinds_path, "brewery")
        .expect("brewery job_kinds.toml parses");

    let mut stamped_kinds = 0;
    for kind in &kinds {
        // Producing JobKinds = those with a production-produce step; the
        // drain contract makes them the only place absorption belongs.
        let produces = kind.steps.iter().any(|s| s.kind == "production-produce");

        // Batch bbl, from the model's own data: the packaging-allocate
        // step's batch_bbl (morning-brew styles) or the summed excise_bbl
        // of the produce steps (seasonal-release, single format).
        let batch_bbl: i64 = kind
            .steps
            .iter()
            .find_map(|s| s.metadata_defaults.get("batch_bbl").and_then(|v| v.as_i64()))
            .unwrap_or_else(|| {
                kind.steps
                    .iter()
                    .filter(|s| s.kind == "production-produce")
                    .filter_map(|s| s.metadata_defaults.get("excise_bbl").and_then(|v| v.as_i64()))
                    .sum()
            });

        for step in &kind.steps {
            let Some(rows) = step
                .metadata_defaults
                .get("overhead_absorbed")
                .and_then(|v| v.as_array())
            else {
                continue;
            };
            assert!(
                produces,
                "JobKind `{}` step `{}` stamps overhead_absorbed but the kind has no \
                 production-produce step — nothing would ever drain it out of WIP",
                kind.kind, step.title
            );
            assert!(
                batch_bbl > 0,
                "JobKind `{}` stamps overhead_absorbed but carries no batch_bbl / \
                 excise_bbl to derive the batch size from",
                kind.kind
            );
            // Exactly the three drivers, each amount = rate × bbl, each
            // memo restating the same bbl (so prose can't drift from the
            // number it annotates).
            assert_eq!(
                rows.len(),
                DRIVERS.len(),
                "JobKind `{}` step `{}`: expected the {} model drivers, found {}",
                kind.kind,
                step.title,
                DRIVERS.len(),
                rows.len()
            );
            for (account, rate, driver) in DRIVERS {
                let row = rows
                    .iter()
                    .find(|r| r.get("credit_account").and_then(|v| v.as_str()) == Some(account))
                    .unwrap_or_else(|| {
                        panic!(
                            "JobKind `{}` step `{}`: no overhead_absorbed row credits {account} \
                             ({driver})",
                            kind.kind, step.title
                        )
                    });
                let amount = row.get("amount_cents").and_then(|v| v.as_i64()).unwrap_or(0);
                assert_eq!(
                    amount,
                    rate * batch_bbl,
                    "JobKind `{}` step `{}` {driver} (CR {account}): amount_cents {amount} != \
                     {rate}¢/bbl × {batch_bbl} bbl = {}",
                    kind.kind,
                    step.title,
                    rate * batch_bbl
                );
                let memo = row.get("memo").and_then(|v| v.as_str()).unwrap_or("");
                assert!(
                    memo.contains(&format!("{batch_bbl} bbl")),
                    "JobKind `{}` step `{}` {driver}: memo `{memo}` doesn't restate the \
                     {batch_bbl}-bbl batch its amount was derived from",
                    kind.kind,
                    step.title
                );
            }
        }

        // Coverage: a producing kind whose consume steps carry NO
        // absorption is costing materials-only — a silently different
        // basis from its siblings. Every producing kind with a
        // production-consume step must stamp its drivers.
        if produces
            && kind
                .steps
                .iter()
                .any(|s| s.kind == "production-consume")
        {
            let stamped = kind.steps.iter().any(|s| {
                s.metadata_defaults
                    .get("overhead_absorbed")
                    .and_then(|v| v.as_array())
                    .is_some_and(|a| !a.is_empty())
            });
            assert!(
                stamped,
                "JobKind `{}` produces FG from a production-consume step but stamps no \
                 overhead_absorbed — it would cost out materials-only while sibling kinds \
                 carry the per-bbl burden",
                kind.kind
            );
            stamped_kinds += 1;
        }
    }
    // The bundle ships 5 morning-brew styles + seasonal-release; a
    // collapse to 0 here means the walk silently matched nothing.
    assert!(
        stamped_kinds >= 6,
        "expected ≥6 producing JobKinds with absorption stamps, found {stamped_kinds}"
    );
}
