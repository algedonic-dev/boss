//! Drift guard for the dispatcher-rules registry migration.
//!
//! `41-dispatcher.sql` seeds the `dispatcher_rules` table and is GENERATED
//! from `infra/dispatcher/rules.toml` by `gen-seed.py`. rules.toml stays
//! the human-authored source; the table is the runtime registry the
//! dispatcher loads. This test loads the seeded table via the production
//! path (`load_active_rules`) and asserts it matches `parse_raw(rules.toml)`
//! — so editing one without regenerating the other fails CI.
//!
//! Postgres-only: TestDb applies the full schema (incl 41-dispatcher.sql).

use std::collections::BTreeMap;

use boss_dispatcher::rules::registry::{load_active_rules, parse_raw};
use boss_testing::TestDb;

#[tokio::test(flavor = "multi_thread")]
async fn dispatcher_rules_seed_matches_toml() {
    let db = TestDb::new().await;

    let from_db = load_active_rules(&db.pool)
        .await
        .expect("load active rules from dispatcher_rules");
    let toml_src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../infra/dispatcher/rules.toml"
    ))
    .expect("read rules.toml");
    let from_toml = parse_raw(&toml_src).expect("parse rules.toml");

    // Compare name -> serialized RawRule (order-independent; arg maps and
    // field order don't matter, the JSON value compares structurally).
    let map_db: BTreeMap<String, serde_json::Value> = from_db
        .rules
        .iter()
        .map(|r| (r.name.clone(), serde_json::to_value(r).expect("serialize")))
        .collect();
    let map_toml: BTreeMap<String, serde_json::Value> = from_toml
        .rules
        .iter()
        .map(|r| (r.name.clone(), serde_json::to_value(r).expect("serialize")))
        .collect();

    assert_eq!(
        map_db, map_toml,
        "dispatcher_rules seed (41-dispatcher.sql) drifted from rules.toml — \
         regenerate with `python3 infra/dispatcher/gen-seed.py`"
    );
}

/// Every rule topic must be CAPTURED by the durable stream, not just
/// accepted by the consumer filter. A filter naming a subject the
/// stream doesn't ingest is legal JetStream — and delivers nothing,
/// silently: the 2026-07-10 year run shipped a rule on
/// `commerce.invoice.created` whose events were never stored, so the
/// COGS-owning consume fired zero times while every gate stayed
/// green. This test is the missing tripwire: add a rule on a new
/// topic family and the build fails until `stream_subjects()` covers
/// it.
#[test]
fn stream_covers_every_rule_topic() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../infra/dispatcher/rules.toml"
    );
    let src = std::fs::read_to_string(path).unwrap();
    let raw = boss_dispatcher::rules::registry::parse_raw(&src).expect("parse rules.toml");
    let subjects = boss_nats::durable::stream_subjects();
    for rule in &raw.rules {
        // Scheduled rules have no topic — nothing to cover.
        let Some(topic) = rule.on_event.as_deref() else {
            continue;
        };
        let covered = subjects.iter().any(|s| {
            let family = s.trim_end_matches('>').trim_end_matches('.');
            topic == family || topic.starts_with(&format!("{family}."))
        });
        assert!(
            covered,
            "rule {:?} listens on {topic:?}, which no stream subject captures              ({subjects:?}) — the consumer filter would accept it and deliver              NOTHING, silently; extend boss_nats::durable::stream_subjects()",
            rule.name
        );
    }
}
