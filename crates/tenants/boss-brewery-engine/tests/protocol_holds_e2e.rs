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

/// The absorption contract is rules data on both sides: the
/// `inventory.overhead.absorb` dos capitalize `rate_cents_per_bbl ×
/// batch bbl` per driver at runtime, and the produce rule's
/// `overhead_accounts` arg names the same accounts so the
/// drain-actual-wip basis reconstructs exactly those facts. The seed
/// multiplies nothing — this test is the enforcement that replaced the
/// old hand-stamped `overhead_absorbed` amounts:
///   (a) the production-consume rule carries exactly the model's three
///       canonical drivers at the canonical rates (derivation in the
///       pale-ale mash-in comment, job_kinds.toml);
///   (b) capitalize-set == drain-set (the produce rule's
///       `overhead_accounts` names the same accounts);
///   (c) no JobKind stamps `overhead_absorbed` amounts anymore; and
///   (d) every producing kind still states the batch size the runtime
///       multiplication reads (`batch_bbl` on a step, else the summed
///       `excise_bbl` of its produce steps).
/// Change a rate → change it here + in infra/dispatcher/rules.toml
/// (then regenerate 41-dispatcher.sql via gen-seed.py).
#[test]
fn overhead_absorption_rules_agree() {
    use boss_jobs::seed_loader::load_job_kinds_with_owning_team;

    const DRIVERS: [(&str, i64, &str); 3] = [
        ("6100", 3660, "Direct labor"),
        ("6300", 560, "Process utilities"),
        ("6900", 860, "Production depreciation"),
    ];

    // Rule args are expr strings: ints as digits, strings quoted.
    fn unquote(s: &str) -> &str {
        s.trim().trim_matches('"')
    }

    // --- rules.toml: both halves of the contract -------------------
    let rules_path = brewery_seeds_dir()
        .join("..")
        .join("..")
        .join("..")
        .join("infra/dispatcher/rules.toml");
    let rules_doc: toml::Value = toml::from_str(
        &std::fs::read_to_string(&rules_path).expect("read infra/dispatcher/rules.toml"),
    )
    .expect("rules.toml parses");
    let rules = rules_doc
        .get("rule")
        .and_then(|v| v.as_array())
        .expect("rules.toml has [[rule]] entries");
    let find_rule = |name: &str| {
        rules
            .iter()
            .find(|r| r.get("name").and_then(|v| v.as_str()) == Some(name))
            .unwrap_or_else(|| panic!("rules.toml missing rule `{name}`"))
    };

    // (a) capitalize-set: exactly the three canonical drivers.
    let consume_rule = find_rule("parts-consume-on-production-consume-step-done");
    let absorb_dos: Vec<&toml::Value> = consume_rule
        .get("do")
        .and_then(|v| v.as_array())
        .expect("consume rule has do entries")
        .iter()
        .filter(|d| d.get("handler").and_then(|v| v.as_str()) == Some("inventory.overhead.absorb"))
        .collect();
    assert_eq!(
        absorb_dos.len(),
        DRIVERS.len(),
        "expected the {} model drivers as inventory.overhead.absorb dos, found {}",
        DRIVERS.len(),
        absorb_dos.len()
    );
    for (account, rate, driver) in DRIVERS {
        let d = absorb_dos
            .iter()
            .find(|d| {
                d.get("args")
                    .and_then(|a| a.get("credit_account"))
                    .and_then(|v| v.as_str())
                    .map(unquote)
                    == Some(account)
            })
            .unwrap_or_else(|| {
                panic!("no inventory.overhead.absorb do credits {account} ({driver})")
            });
        let args = d.get("args").expect("absorb do has args");
        let got_rate: i64 = args
            .get("rate_cents_per_bbl")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| panic!("CR {account}: rate_cents_per_bbl missing or not an int"));
        assert_eq!(
            got_rate, rate,
            "CR {account} ({driver}): rate {got_rate}¢/bbl != canonical {rate}¢/bbl"
        );
        let got_driver = args
            .get("driver")
            .and_then(|v| v.as_str())
            .map(unquote)
            .unwrap_or("");
        assert_eq!(
            got_driver, driver,
            "CR {account}: driver label `{got_driver}` != canonical `{driver}`"
        );
    }
    // Distinct accounts — two dos crediting one account would collide
    // on the endpoint's (step, account) idempotency key and silently
    // drop the second amount.
    let mut accounts: Vec<&str> = absorb_dos
        .iter()
        .filter_map(|d| {
            d.get("args")
                .and_then(|a| a.get("credit_account"))
                .and_then(|v| v.as_str())
                .map(unquote)
        })
        .collect();
    accounts.sort_unstable();
    assert!(
        accounts.windows(2).all(|w| w[0] != w[1]),
        "absorb dos credit a duplicate account — amounts would collide on idempotency"
    );

    // (b) drain-set == capitalize-set.
    let produce_rule = find_rule("products-produce-on-production-produce-step-done");
    let produce_do = produce_rule
        .get("do")
        .and_then(|v| v.as_array())
        .expect("produce rule has do entries")
        .iter()
        .find(|d| d.get("handler").and_then(|v| v.as_str()) == Some("products.produce"))
        .expect("produce rule has a products.produce do");
    let mut drain_accounts: Vec<&str> = produce_do
        .get("args")
        .and_then(|a| a.get("overhead_accounts"))
        .and_then(|v| v.as_str())
        .map(unquote)
        .expect("products.produce do carries overhead_accounts")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    drain_accounts.sort_unstable();
    assert_eq!(
        drain_accounts, accounts,
        "drain-set (produce overhead_accounts) != capitalize-set (absorb dos) — \
         the drain would reconstruct facts the absorb side never posts (NAK loop) \
         or strand capitalized drivers in WIP"
    );

    // --- seed: no stamps; batch size derivable ---------------------
    let kinds_path = brewery_seeds_dir().join("job_kinds.toml");
    let kinds = load_job_kinds_with_owning_team(&kinds_path, "brewery")
        .expect("brewery job_kinds.toml parses");

    let mut producing_kinds = 0;
    for kind in &kinds {
        // (c) the stamp path is retired — amounts live in rules args.
        for step in &kind.steps {
            assert!(
                step.metadata_defaults.get("overhead_absorbed").is_none(),
                "JobKind `{}` step `{}` stamps overhead_absorbed — absorption amounts are \
                 rules data now (inventory.overhead.absorb args), not seed stamps",
                kind.kind,
                step.title
            );
        }

        // (d) the runtime multiplication's other input must exist:
        // batch_bbl on any step, else summed excise_bbl of the produce
        // steps — the same derivation inventory.overhead.absorb runs.
        let produces = kind.steps.iter().any(|s| s.kind == "production-produce");
        if produces && kind.steps.iter().any(|s| s.kind == "production-consume") {
            let batch_bbl: i64 = kind
                .steps
                .iter()
                .find_map(|s| {
                    s.metadata_defaults
                        .get("batch_bbl")
                        .and_then(|v| v.as_i64())
                })
                .unwrap_or_else(|| {
                    kind.steps
                        .iter()
                        .filter(|s| s.kind == "production-produce")
                        .filter_map(|s| {
                            s.metadata_defaults
                                .get("excise_bbl")
                                .and_then(|v| v.as_i64())
                        })
                        .sum()
                });
            assert!(
                batch_bbl > 0,
                "JobKind `{}` brews (production-consume → production-produce) but states no \
                 batch_bbl / excise_bbl — the absorb handler would no-op and the kind would \
                 cost out materials-only while sibling kinds carry the per-bbl burden",
                kind.kind
            );
            producing_kinds += 1;
        }
    }
    // The bundle ships 5 morning-brew styles + seasonal-release; a
    // collapse to 0 here means the walk silently matched nothing.
    assert!(
        producing_kinds >= 6,
        "expected ≥6 brewing JobKinds with a derivable batch size, found {producing_kinds}"
    );
}
