use boss_jobs::cadence::{CadenceRepository, InMemoryCadence, NewFiring};
use chrono::{DateTime, Utc};
use serde_json::json;

async fn contract(repo: &dyn CadenceRepository) {
    let at = |s: &str| s.parse::<DateTime<Utc>>().unwrap();
    let mut originals = Vec::new();
    for (id, stamp) in [
        ("history-a", "2026-10-16T00:00:00.000001Z"),
        ("history-c", "2026-10-16T00:00:00.000002Z"),
        ("history-b", "2026-10-16T00:00:00.000002Z"),
    ] {
        let row = NewFiring {
            firing_id: id.into(),
            rule_name: "history-fixture".into(),
            verb: "reconcile".into(),
            basis: "wall".into(),
            fired_at: at(stamp),
            detail: json!({"original":{"whole":[1,2,3],"id":id},"observations":{"gate_queue":{"state":"unknown"}}}),
        };
        assert!(repo.claim_firing(&row).await.unwrap());
        originals.push(row);
    }
    let mut unrelated = originals[0].clone();
    unrelated.firing_id = "history-unrelated".into();
    unrelated.fired_at = at("2026-10-16T00:00:02Z");
    unrelated.detail = json!({"observations":{"unrelated":{"state":"completed"}}});
    repo.claim_firing(&unrelated).await.unwrap();
    let first = repo
        .firing_history("history-fixture", Some("gate_queue"), None, 2)
        .await
        .unwrap();
    assert_eq!(first, vec![originals[1].clone(), originals[2].clone()]);
    let cursor = first.last().map(|f| (f.fired_at, f.firing_id.clone()));
    // A newly arriving newer row cannot shift an exclusive keyset page.
    let mut newer = originals[0].clone();
    newer.firing_id = "history-new".into();
    newer.fired_at = at("2026-10-16T00:00:01Z");
    repo.claim_firing(&newer).await.unwrap();
    let next = repo
        .firing_history("history-fixture", Some("gate_queue"), cursor, 2)
        .await
        .unwrap();
    assert_eq!(next, vec![originals[0].clone()]);
    assert!(
        repo.firing_history(
            "history-fixture",
            Some("gate_queue"),
            Some((next[0].fired_at, next[0].firing_id.clone())),
            2
        )
        .await
        .unwrap()
        .is_empty()
    );
    assert!(
        repo.firing_history("history-fixture", None, None, 0)
            .await
            .is_err()
    );
    assert!(
        repo.firing_history("history-fixture", None, None, 501)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn memory_preserves_original_history_and_exclusive_order() {
    contract(&InMemoryCadence::new(vec![])).await;
}

#[cfg(feature = "postgres")]
#[tokio::test]
async fn postgres_preserves_original_history_and_exclusive_order() {
    let db = boss_testing::TestDb::new().await;
    contract(&boss_jobs::cadence::PgCadence::new(db.pool.clone())).await;
}
