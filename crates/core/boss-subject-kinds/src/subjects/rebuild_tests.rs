//! Two registered sources share one identity. Their file order is not
//! the order of the facts that the live write-through committed (5bdaf9d6).

use super::{IdentitySource, rebuild_with_sources, record_subject_in_tx};
use boss_testing::TestDb;
use std::collections::BTreeSet;

fn sources(reverse: bool) -> Vec<IdentitySource> {
    let mut sources = vec![
        IdentitySource {
            event_kind: "test.identity.first".into(),
            subject_kind: "vendor".into(),
            id_field: "id".into(),
            label_field: Some("name".into()),
        },
        IdentitySource {
            event_kind: "test.identity.second".into(),
            subject_kind: "vendor".into(),
            id_field: "identity".into(),
            label_field: Some("title".into()),
        },
    ];
    if reverse {
        sources.reverse();
    }
    sources
}

#[cfg(test)]
async fn append(db: &TestDb, id: &str, second: bool, label: Option<&str>) {
    let mut tx = db.pool.begin().await.unwrap();
    record_subject_in_tx(&mut tx, "vendor", id, label)
        .await
        .unwrap();
    let (kind, payload) = if second {
        (
            "test.identity.second",
            serde_json::json!({"identity":id,"title":label}),
        )
    } else {
        (
            "test.identity.first",
            serde_json::json!({"id":id,"name":label}),
        )
    };
    sqlx::query(
        "INSERT INTO audit_log (event_id, timestamp, source, kind, payload) \
         VALUES (gen_random_uuid(), '2026-07-01T00:00:00Z', 'test', $1, $2)",
    )
    .bind(kind)
    .bind(payload)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

async fn labels(db: &TestDb) -> Vec<(String, Option<String>)> {
    sqlx::query_as("SELECT id, label FROM subjects WHERE kind='vendor' ORDER BY id")
        .fetch_all(&db.pool)
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn alternating_sources_rebuild_the_live_label_in_either_registry_order() {
    let db = TestDb::new().await;
    for (id, history) in [
        (
            "second-last",
            vec![
                (false, "first-one"),
                (true, "second-one"),
                (false, "first-two"),
                (true, "second-two"),
            ],
        ),
        (
            "first-last",
            vec![
                (true, "second-one"),
                (false, "first-one"),
                (true, "second-two"),
                (false, "first-two"),
            ],
        ),
    ] {
        for (second, label) in history {
            append(&db, id, second, Some(label)).await;
        }
    }
    let live = labels(&db).await;
    assert_eq!(
        live,
        vec![
            ("first-last".into(), Some("first-two".into())),
            ("second-last".into(), Some("second-two".into()))
        ]
    );
    for reverse in [false, true] {
        rebuild_with_sources(&db.pool, &sources(reverse))
            .await
            .unwrap();
        assert_eq!(labels(&db).await, live, "registry reverse={reverse}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_later_null_retains_the_latest_non_null_and_all_null_still_exists() {
    let db = TestDb::new().await;
    append(&db, "named", false, Some("older")).await;
    append(&db, "named", true, Some("newer")).await;
    append(&db, "named", false, None).await;
    append(&db, "named", true, None).await;
    append(&db, "unnamed", true, None).await;
    let live = labels(&db).await;
    assert_eq!(
        live,
        vec![
            ("named".into(), Some("newer".into())),
            ("unnamed".into(), None)
        ]
    );
    for reverse in [false, true] {
        rebuild_with_sources(&db.pool, &sources(reverse))
            .await
            .unwrap();
        assert_eq!(labels(&db).await, live);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn random_interleavings_rebuild_equal_live_and_keep_source_pass_counts() {
    let db = TestDb::new().await;
    let reference_rows = rebuild_with_sources(&db.pool, &[]).await.unwrap();
    let mut expected = Vec::new();
    let mut source_identities = BTreeSet::new();
    // Reproducible generated histories, including null suffixes.
    // No clock, process RNG or registry order
    // chooses which committed fact owns a label.
    for seed in 1_u64..=32 {
        let id = format!("generated-{seed:02}");
        let mut state = seed;
        let mut last = None;
        for n in 0..24 {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            let second = state & (1 << 32) != 0;
            let label = (state % 4 != 0).then(|| format!("{seed}-{n}"));
            append(&db, &id, second, label.as_deref()).await;
            source_identities.insert((second, id.clone()));
            if label.is_some() {
                last = label;
            }
        }
        expected.push((id, last));
    }
    assert_eq!(
        labels(&db).await,
        expected,
        "the live port matches the fold oracle"
    );
    for reverse in [false, true] {
        let count = rebuild_with_sources(&db.pool, &sources(reverse))
            .await
            .unwrap();
        assert_eq!(labels(&db).await, expected, "registry reverse={reverse}");
        assert_eq!(
            count,
            reference_rows + source_identities.len() as u64,
            "the existing per-source distinct-identity count is conserved"
        );
    }
}
