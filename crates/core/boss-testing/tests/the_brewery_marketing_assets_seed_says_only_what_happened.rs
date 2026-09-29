//! The brewery's marketing-asset library — `examples/brewery/data/
//! marketing-assets.json` — carries the assets and nothing a step would
//! have had to do, in the platform's words and not a retired tenant's,
//! and wears only kinds its own tenant declares. Three items from the
//! /ux/marketing-assets page audit (7cdb095b), decided 2026-09-28:
//!
//!   * NO SEEDED SIGN-OFF (d9bce191). 43 of 43 rows carried
//!     `brand_reviewed_by`, so the detail page named a reviewer for
//!     every asset — a sign-off with no packet, no step and no event
//!     behind it. A review is a step's output; a seed that writes one
//!     is the anti-pattern docs/design/seed-vs-emergent-state.md names,
//!     and the demo is the last place to teach it. The column stays;
//!     the page says "not reviewed", which is true.
//!   * NO DEVICE-SHOP NAME (f925b58b). The field was
//!     `linked_device_skus`, from the used-device shop retired on
//!     2026-09-24 (a8991c86); every SKU the seed links is a product.
//!     It is `linked_skus` in the schema, boss-catalog, the web and
//!     here.
//!   * THE TENANT DECLARES ITS KINDS (9b28f849). The nine
//!     (marketing-asset, kind) Classes were seeded into every instance
//!     by 01-registries.sql — a Tier-2 module's taxonomy in the Tier-1
//!     core seed, live on instances that never turn the module on. The
//!     kinds the brewery uses now ride its own seeds/classes.json, so a
//!     fresh company instance evicts the core's copy at boot
//!     (infra/postgres/example-reference-rows.sh; held there by
//!     example_reference_rows_sql.rs). `brief-body` and `retro`, which
//!     no row and no code used, are declared by no live tenant: they are
//!     retired-examples candidates, and the running instances retire
//!     them through the Class registry's evented door.
//!
//! tree-wide pin — it reads examples/, which no changed-file map
//! attributes to this crate, so every scoped gate runs it whatever its
//! scope (`tree_wide_pins` in infra/gate.sh; backlog c87ad472).

use boss_testing::repo_root;
use serde_json::Value;
use std::collections::BTreeSet;

fn read_json(rel: &str) -> Value {
    let p = repo_root().join(rel);
    let s = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
    serde_json::from_str(&s).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn assets() -> Vec<Value> {
    let rows = read_json("examples/brewery/data/marketing-assets.json")
        .as_array()
        .expect("marketing-assets.json is an array")
        .clone();
    assert!(
        rows.len() > 10,
        "the reader found the library ({} rows) — a reader that found none would certify nothing",
        rows.len()
    );
    rows
}

#[test]
fn no_seeded_asset_carries_a_brand_review_no_step_performed() {
    let reviewed: Vec<String> = assets()
        .iter()
        .filter(|a| a.get("brand_reviewed_by").is_some() || a.get("brand_reviewed_at").is_some())
        .map(|a| a["id"].as_str().unwrap_or("?").to_string())
        .collect();
    assert!(
        reviewed.is_empty(),
        "a brand review is a step's output, and these seed rows assert one: {reviewed:?}"
    );
}

#[test]
fn every_seeded_asset_links_skus_by_the_platforms_name() {
    for a in assets() {
        let id = a["id"].as_str().unwrap_or("?");
        assert!(
            a.get("linked_device_skus").is_none(),
            "{id} carries the retired device shop's field name"
        );
        assert!(
            a["linked_skus"].is_array(),
            "{id} carries linked_skus, the field boss-catalog reads"
        );
    }
}

#[test]
fn every_kind_a_seeded_asset_wears_is_one_its_tenant_declares() {
    let declared: BTreeSet<String> = read_json("examples/brewery/seeds/classes.json")
        .as_array()
        .expect("classes.json is an array")
        .iter()
        .filter(|c| c["subject_kind"] == "marketing-asset" && c["member_attribute"] == "kind")
        .filter_map(|c| c["code"].as_str().map(str::to_string))
        .collect();
    let worn: BTreeSet<String> = assets()
        .iter()
        .filter_map(|a| a["kind"].as_str().map(str::to_string))
        .collect();
    assert!(!worn.is_empty(), "the library wears kinds");
    let undeclared: Vec<_> = worn.difference(&declared).collect();
    assert!(
        undeclared.is_empty(),
        "the brewery's assets wear kinds its seeds/classes.json does not declare, so a fresh \
         instance's boot would evict them from under it: {undeclared:?}"
    );
    for retired in ["brief-body", "retro"] {
        assert!(
            !declared.contains(retired),
            "{retired} is retired (9b28f849) and the brewery declares it"
        );
    }
}
