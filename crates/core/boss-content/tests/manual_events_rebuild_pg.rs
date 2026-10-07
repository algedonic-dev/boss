//! Manual writes leave their complete committed state on the transactional outbox.

use boss_content::types::{Audience, ManualPatch, ManualSectionDraft};
use boss_content::{ContentRepository, PgContent};
use boss_testing::TestDb;

async fn snapshot(pool: &sqlx::PgPool) -> serde_json::Value {
    let sections: Vec<serde_json::Value> =
        sqlx::query_scalar("SELECT to_jsonb(s) FROM manual_sections s ORDER BY slug")
            .fetch_all(pool)
            .await
            .unwrap();
    let history: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT to_jsonb(h) FROM manual_section_history h ORDER BY section_id, version",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    let bulletins: Vec<serde_json::Value> =
        sqlx::query_scalar("SELECT to_jsonb(b) FROM bulletins b ORDER BY id")
            .fetch_all(pool)
            .await
            .unwrap();
    let dismissals: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT to_jsonb(d) FROM bulletin_dismissals d ORDER BY bulletin_id, employee_id",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    serde_json::json!({"sections": sections, "history": history, "bulletins": bulletins, "dismissals": dismissals})
}

async fn drain(pool: &sqlx::PgPool) {
    let bus = boss_testing::RecordingEventBus::new();
    boss_events::outbox::drain_outbox_once(
        pool,
        &(bus as std::sync::Arc<dyn boss_core::port::EventBus>),
        100,
    )
    .await
    .unwrap();
}

fn draft(slug: &str, parent: Option<&str>) -> ManualSectionDraft {
    ManualSectionDraft {
        slug: slug.into(),
        parent_slug: parent.map(str::to_string),
        title: slug.into(),
        body: "body".into(),
        sort_order: 3,
        audience: Audience::all(),
        published: false,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn http_manual_writes_use_the_request_actor_and_publisher_stamp() {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;
    let db = TestDb::new().await;
    let app = boss_content::http::router(boss_content::http::ContentApiState {
        repo: std::sync::Arc::new(PgContent::new(db.pool.clone())),
        publisher: Some(boss_core::publisher::DomainPublisher::new(
            boss_testing::RecordingEventBus::new(),
            "content-http-test",
        )),
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
    });
    for (method, path, body, expected) in [
        (
            "POST",
            "/api/content/manual",
            serde_json::to_value(draft("http-section", None)).unwrap(),
            StatusCode::CREATED,
        ),
        (
            "PUT",
            "/api/content/manual/http-section",
            serde_json::json!({"body":"Updated"}),
            StatusCode::OK,
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("content-type", "application/json")
                    .header("x-boss-user", r#"{"id":"agent-reviewer","role":"hr-lead"}"#)
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
    }
    let facts: Vec<(String, serde_json::Value)> =
        sqlx::query_as("SELECT source, payload FROM event_outbox ORDER BY id")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    assert_eq!(facts.len(), 2);
    for (source, payload) in facts {
        assert_eq!(
            source, "content-http-test",
            "HTTP must use the wired publisher envelope"
        );
        assert_eq!(payload["_actor"], "agent-reviewer");
        assert_eq!(payload["version"]["edited_by"], payload["_actor"]);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn rebuild_reproduces_all_manual_fields_and_history_twice() {
    let db = TestDb::new().await;
    let repo = PgContent::new(db.pool.clone());
    repo.create_section(draft("z-parent", None), "emp-first")
        .await
        .unwrap();
    repo.create_section(draft("a-child", Some("z-parent")), "emp-second")
        .await
        .unwrap();
    repo.update_section(
        "a-child",
        ManualPatch {
            body: Some("new body".into()),
            reason: Some("changed".into()),
            ..Default::default()
        },
        "emp-third",
    )
    .await
    .unwrap();
    let before = snapshot(&db.pool).await;
    drain(&db.pool).await;
    // A rolled-back INSERT also consumes this nontransactional sequence.
    // Simulate that gap explicitly: replay must not move it backwards.
    let consumed: i64 = sqlx::query_scalar(
        "SELECT nextval(pg_get_serial_sequence('manual_section_history', 'id'))",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    sqlx::query("UPDATE manual_sections SET body = 'corrupted'")
        .execute(&db.pool)
        .await
        .unwrap();
    boss_content::rebuild_content(&db.pool).await.unwrap();
    assert_eq!(
        snapshot(&db.pool).await,
        before,
        "replay must repair full manual projection"
    );
    boss_content::rebuild_content(&db.pool).await.unwrap();
    assert_eq!(
        snapshot(&db.pool).await,
        before,
        "repeated replay is identical"
    );
    repo.update_section(
        "a-child",
        ManualPatch {
            reason: Some("after replay".into()),
            ..Default::default()
        },
        "emp-fourth",
    )
    .await
    .unwrap();
    let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM manual_section_history ORDER BY id")
        .fetch_all(&db.pool)
        .await
        .unwrap();
    assert_eq!(ids.len(), 4);
    assert!(
        ids[3] > consumed,
        "replay cannot rewind an already consumed sequence identity"
    );
    assert!(
        ids.windows(2).all(|pair| pair[0] < pair[1]),
        "next ordinary write has a fresh stored identity"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn legacy_history_without_facts_refuses_before_wiping() {
    let db = TestDb::new().await;
    sentinel_bulletin(&db.pool).await;
    // This deliberately models the old writer's projection-only transaction.
    let id = uuid::Uuid::new_v4();
    sqlx::query("INSERT INTO manual_sections (id, slug, title, body) VALUES ($1, 'legacy', 'Old title', 'Old body')")
        .bind(id).execute(&db.pool).await.unwrap();
    sqlx::query("INSERT INTO manual_section_history (section_id, version, title, body, audience, edited_by, reason) VALUES ($1, 1, 'Old title', 'Old body', '{\"all\":true}', 'emp-historical', 'Historical reason')")
        .bind(id).execute(&db.pool).await.unwrap();
    let before = snapshot(&db.pool).await;
    let error = boss_content::rebuild_content(&db.pool)
        .await
        .expect_err("unrecorded legacy history is not rebuildable");
    assert!(error.to_string().contains("manual"), "{error}");
    assert_eq!(
        snapshot(&db.pool).await,
        before,
        "refusal preserves legacy claims exactly"
    );
}

async fn sentinel_bulletin(pool: &sqlx::PgPool) {
    let repo = PgContent::new(pool.clone());
    let bulletin = repo
        .create_bulletin(
            boss_content::types::BulletinDraft {
                id: None,
                title: "Retain on refusal".into(),
                body: "Sentinel".into(),
                posted_on: None,
                expires_on: None,
                priority: Default::default(),
                audience: Audience::all(),
            },
            "emp-author",
        )
        .await
        .unwrap();
    repo.dismiss_bulletin(bulletin.id, "emp-reader")
        .await
        .unwrap();
    drain(pool).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_manual_facts_refuse_and_preserve_every_content_table() {
    for fault in [
        "malformed",
        "gap",
        "identity",
        "unknown",
        "actor",
        "timestamp",
    ] {
        let db = TestDb::new().await;
        sentinel_bulletin(&db.pool).await;
        let repo = PgContent::new(db.pool.clone());
        repo.create_section(draft("section", None), "emp-author")
            .await
            .unwrap();
        let (kind, mut payload): (String, serde_json::Value) = sqlx::query_as(
            "SELECT kind, payload FROM event_outbox WHERE kind LIKE 'content.manual.%'",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        let kind = match fault {
            "malformed" => {
                payload = serde_json::json!({"section": false});
                kind
            }
            "gap" => {
                payload["section"]["current_version"] = 2.into();
                payload["version"]["version"] = 2.into();
                "content.manual.section_updated".into()
            }
            "identity" => {
                payload["version"]["section_id"] = uuid::Uuid::new_v4().to_string().into();
                kind
            }
            "unknown" => "content.manual.unsupported".into(),
            "actor" => {
                payload["version"]["edited_by"] = "emp-invented".into();
                kind
            }
            "timestamp" => {
                payload["version"]["edited_at"] = "2000-01-01T00:00:00Z".into();
                payload["section"]["updated_at"] = "2000-01-01T00:00:00Z".into();
                payload["section"]["created_at"] = "2000-01-01T00:00:00Z".into();
                kind
            }
            _ => unreachable!(),
        };
        sqlx::query(
            "UPDATE event_outbox SET kind = $1, payload = $2 WHERE kind LIKE 'content.manual.%'",
        )
        .bind(kind)
        .bind(payload)
        .execute(&db.pool)
        .await
        .unwrap();
        drain(&db.pool).await;
        let before = snapshot(&db.pool).await;
        let error = boss_content::rebuild_content(&db.pool)
            .await
            .expect_err(fault);
        assert!(error.to_string().contains("manual"), "{fault}: {error}");
        assert_eq!(
            snapshot(&db.pool).await,
            before,
            "{fault}: every content table survives refusal"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn updating_legacy_section_does_not_invent_prior_facts() {
    let db = TestDb::new().await;
    let id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO manual_sections (id, slug, title, body) VALUES ($1, 'legacy', 'Old', 'Old')",
    )
    .bind(id)
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO manual_section_history (section_id, version, title, body, audience, edited_by) VALUES ($1,1,'Old','Old','{}','emp-prior')")
        .bind(id).execute(&db.pool).await.unwrap();
    PgContent::new(db.pool.clone())
        .update_section(
            "legacy",
            ManualPatch {
                body: Some("New".into()),
                ..Default::default()
            },
            "emp-current",
        )
        .await
        .unwrap();
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM event_outbox WHERE kind LIKE 'content.manual.%'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(count, 1, "the observed update is the sole fact");
    drain(&db.pool).await;
    let before = snapshot(&db.pool).await;
    let error = boss_content::rebuild_content(&db.pool)
        .await
        .expect_err("missing prior creation/history is refused");
    assert!(error.to_string().contains("version gap"), "{error}");
    assert_eq!(snapshot(&db.pool).await, before);
}

#[tokio::test(flavor = "multi_thread")]
async fn manual_row_history_and_fact_commit_or_rollback_together() {
    for update in [false, true] {
        let db = TestDb::new().await;
        let repo = PgContent::new(db.pool.clone());
        if update {
            repo.create_section(draft("atomic", None), "emp-first")
                .await
                .unwrap();
        }
        let before = snapshot(&db.pool).await;
        let before_facts: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        // Actual database write refusal, not an adapter double: the outbox
        // INSERT must abort the same transaction that changed both rows.
        sqlx::query("CREATE FUNCTION refuse_manual_fact() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'causal outbox refusal'; END $$")
            .execute(&db.pool).await.unwrap();
        sqlx::query("CREATE TRIGGER refuse_manual_fact BEFORE INSERT ON event_outbox FOR EACH ROW EXECUTE FUNCTION refuse_manual_fact()")
            .execute(&db.pool).await.unwrap();
        let result = if update {
            repo.update_section(
                "atomic",
                ManualPatch {
                    body: Some("Must not land".into()),
                    ..Default::default()
                },
                "emp-second",
            )
            .await
        } else {
            repo.create_section(draft("atomic", None), "emp-first")
                .await
        };
        assert!(
            result.is_err(),
            "outbox refusal must refuse the domain write"
        );
        assert_eq!(
            snapshot(&db.pool).await,
            before,
            "update={update}: rows/history roll back"
        );
        let after_facts: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(after_facts, before_facts, "no orphan fact");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn starter_sections_use_the_same_event_bearing_port_idempotently() {
    let db = TestDb::new().await;
    let repo = PgContent::new(db.pool.clone());
    let inserted = boss_content::seed::seed_starter_sections(&repo)
        .await
        .unwrap();
    assert!(inserted > 0, "the starter fixture is nonempty");
    let facts: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT payload FROM event_outbox WHERE kind = 'content.manual.section_created'",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(facts.len(), inserted);
    assert!(
        facts
            .iter()
            .all(|p| p["_actor"] == "automation:content-seed"
                && p["version"]["edited_by"] == p["_actor"])
    );
    assert_eq!(
        boss_content::seed::seed_starter_sections(&repo)
            .await
            .unwrap(),
        0
    );
    let after: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        after as usize,
        facts.len(),
        "repeat seed records no duplicate fact"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn manual_write_ports_record_the_committed_section_and_version() {
    let db = TestDb::new().await;
    let repo = PgContent::new(db.pool.clone());
    let created = repo
        .create_section(
            ManualSectionDraft {
                slug: "policy".into(),
                parent_slug: None,
                title: "Policy".into(),
                body: "Original".into(),
                sort_order: 7,
                audience: Audience::all(),
                published: false,
            },
            "emp-first",
        )
        .await
        .unwrap();
    let updated = repo
        .update_section(
            "policy",
            ManualPatch {
                body: Some("Revised".into()),
                reason: Some("Clarify the rule".into()),
                ..Default::default()
            },
            "emp-second",
        )
        .await
        .unwrap();
    let history = repo.section_history("policy").await.unwrap();
    assert_eq!(history.len(), 2, "the port committed both versions");
    let facts: Vec<(String, serde_json::Value)> = sqlx::query_as(
        "SELECT kind, payload FROM event_outbox WHERE kind LIKE 'content.manual.%' ORDER BY id",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(facts.len(), 2, "each committed manual write needs its fact");
    assert_eq!(facts[0].0, "content.manual.section_created");
    assert_eq!(facts[1].0, "content.manual.section_updated");
    assert_eq!(
        facts[0].1["section"],
        serde_json::to_value(created).unwrap()
    );
    assert_eq!(
        facts[1].1["section"],
        serde_json::to_value(updated).unwrap()
    );
    assert_eq!(
        facts[0].1["version"],
        serde_json::to_value(&history[1]).unwrap()
    );
    assert_eq!(
        facts[1].1["version"],
        serde_json::to_value(&history[0]).unwrap()
    );
}
