//! End-to-end tests against the products HTTP API + Postgres adapter.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_products::http::{ProductsApiState, router};
use boss_products::postgres::PgProducts;
use boss_products::types::{Product, ProductInventory};
use boss_testing::TestDb;
use http_body_util::BodyExt;
use tower::util::ServiceExt;

/// SKU car1 is a generic catalog foundation. Legacy goods remain on
/// their existing route; a SKU named `catalog` must not be reserved by
/// adding the new version registry's collection read.
#[tokio::test]
async fn sku_versions_have_a_read_surface_separate_from_legacy_goods() {
    use boss_products::ProductsRepository;
    let repo = Arc::new(boss_products::InMemoryProducts::new());
    let mut goods = fp_pale();
    goods.sku = "catalog".into();
    repo.upsert_product(&goods, &stamp()).await.unwrap();
    let r = router(ProductsApiState {
        products: repo,
        publisher: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
        classes_client: None,
    });
    let response = r
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/products/catalog/versions")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await, serde_json::json!([]));
    let legacy = r
        .oneshot(
            Request::builder()
                .uri("/api/products/catalog")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(legacy.status(), StatusCode::OK);
    assert_eq!(body_json(legacy).await["sku"], "catalog");
}

#[tokio::test]
async fn sku_versions_preserve_history_and_refuse_conflicting_redelivery() {
    use boss_products::ProductsRepository;
    let repo = boss_products::InMemoryProducts::new();
    let first = sku_fixture();
    assert!(repo.append_sku_version(&first, &stamp()).await.unwrap());
    let facts = serde_json::to_value(repo.recorded_events()).unwrap();
    assert!(!repo.append_sku_version(&first, &stamp()).await.unwrap());
    assert_eq!(serde_json::to_value(repo.recorded_events()).unwrap(), facts);
    let mut conflict = first.clone();
    conflict.name = "Cannot overwrite history".into();
    assert!(repo.append_sku_version(&conflict, &stamp()).await.is_err());
    assert_eq!(serde_json::to_value(repo.recorded_events()).unwrap(), facts);
    let mut second = first.clone();
    second.version = 2;
    second.active = false;
    assert!(repo.append_sku_version(&second, &stamp()).await.unwrap());
    assert_eq!(repo.sku_versions().await.unwrap(), vec![first, second]);
    assert_eq!(
        repo.recorded_events().len(),
        facts.as_array().unwrap().len() + 1
    );
}

fn sku_fixture() -> boss_products::sku::SkuVersion {
    serde_json::from_value(serde_json::json!({
        "sku": "fixture-membership", "name": "Fixture membership", "kind": "fixture-kind",
        "description": "A versioned promise", "price": {"amount_cents": 5000, "currency": "USD", "basis": "monthly"},
        "includes": [{"code": "fixture-support", "text": "Fixture support"}], "active": true, "version": 1
    })).unwrap()
}

#[tokio::test]
async fn postgres_records_each_sku_version_and_its_fact_atomically() {
    use boss_products::ProductsRepository;
    let db = TestDb::new().await;
    let repo = PgProducts::new(db.pool.clone());
    let first = sku_fixture();
    assert!(repo.append_sku_version(&first, &stamp()).await.unwrap());
    assert!(!repo.append_sku_version(&first, &stamp()).await.unwrap());
    assert_eq!(repo.sku_versions().await.unwrap(), vec![first]);
    let facts: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT payload FROM event_outbox WHERE kind = 'products.sku.version.appended' ORDER BY id",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(facts.len(), 1);
    assert_eq!(
        facts[0]["data"],
        serde_json::to_value(sku_fixture()).unwrap()
    );
    assert_eq!(facts[0]["name"], facts[0]["data"]["name"]);
    assert_eq!(facts[0]["sku"], "fixture-membership");
    assert_eq!(facts[0]["version"], 1);
}

#[tokio::test]
async fn invalid_sku_versions_leave_both_adapters_and_facts_unchanged() {
    use boss_products::ProductsRepository;
    async fn check(repo: &impl ProductsRepository) {
        let original = sku_fixture();
        let mut rows = Vec::new();
        let mut row = original.clone();
        row.version = 0;
        rows.push(row);
        let mut row = original.clone();
        row.version = 2;
        rows.push(row);
        let mut row = original.clone();
        row.name.clear();
        rows.push(row);
        let mut row = original.clone();
        row.price.amount_cents = -1;
        rows.push(row);
        let mut row = original.clone();
        row.kind.push('\0');
        rows.push(row);
        let mut row = original.clone();
        row.includes.push(row.includes[0].clone());
        rows.push(row);
        for row in rows {
            assert!(repo.append_sku_version(&row, &stamp()).await.is_err());
            assert!(repo.sku_versions().await.unwrap().is_empty());
        }
    }
    let memory = boss_products::InMemoryProducts::new();
    check(&memory).await;
    assert!(memory.recorded_events().is_empty());
    let db = TestDb::new().await;
    check(&PgProducts::new(db.pool.clone())).await;
    let facts: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM event_outbox WHERE kind = 'products.sku.version.appended'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(facts, 0);
}

#[tokio::test]
async fn a_failed_sku_fact_rolls_back_version_and_subject_then_retries() {
    use boss_products::ProductsRepository;
    let db = TestDb::new().await;
    let repo = PgProducts::new(db.pool.clone());
    sqlx::raw_sql("CREATE FUNCTION fixture_refuse_sku_fact() RETURNS TRIGGER LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind = 'products.sku.version.appended' THEN RAISE EXCEPTION 'fixture SKU fact refused'; END IF; RETURN NEW; END $$; CREATE TRIGGER fixture_refuse_sku_fact BEFORE INSERT ON event_outbox FOR EACH ROW EXECUTE FUNCTION fixture_refuse_sku_fact();").execute(&db.pool).await.unwrap();
    let row = sku_fixture();
    let error = repo.append_sku_version(&row, &stamp()).await.unwrap_err();
    assert!(
        error.to_string().contains("fixture SKU fact refused"),
        "{error}"
    );
    assert!(repo.sku_versions().await.unwrap().is_empty());
    let subjects: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM subjects WHERE kind = 'product' AND id = $1")
            .bind(&row.sku)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(subjects, 0);
    let facts: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM event_outbox WHERE kind = 'products.sku.version.appended'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(facts, 0);
    sqlx::query("DROP TRIGGER fixture_refuse_sku_fact ON event_outbox")
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(repo.append_sku_version(&row, &stamp()).await.unwrap());
    assert_eq!(repo.sku_versions().await.unwrap(), vec![row]);
}

#[tokio::test(flavor = "multi_thread")]
async fn simultaneous_sku_versions_commit_one_payload_and_one_fact() {
    use boss_products::ProductsRepository;
    let db = TestDb::new().await;
    let repo = Arc::new(PgProducts::new(db.pool.clone()));
    let mut blocker = db.pool.begin().await.unwrap();
    sqlx::query("LOCK TABLE product_sku_versions IN SHARE MODE")
        .execute(&mut *blocker)
        .await
        .unwrap();
    let mut workers = Vec::new();
    for name in ["First contender", "Second contender"] {
        let repo = repo.clone();
        let mut row = sku_fixture();
        row.name = name.into();
        workers.push(tokio::spawn(async move {
            repo.append_sku_version(&row, &stamp()).await
        }));
    }
    // Native lock state is the arrival witness, never a sleep.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let waiting: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pg_locks WHERE database = (SELECT oid FROM pg_database WHERE datname = current_database()) AND relation = 'product_sku_versions'::regclass AND mode = 'RowExclusiveLock' AND NOT granted").fetch_one(&db.pool).await.unwrap();
            if waiting == 2 { break; }
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    blocker.commit().await.unwrap();
    let mut inserted = 0;
    let mut refused = 0;
    for worker in workers {
        match worker.await.unwrap() {
            Ok(true) => inserted += 1,
            Err(boss_products::ProductsError::Invalid(_)) => refused += 1,
            other => panic!("unexpected append outcome: {other:?}"),
        }
    }
    assert_eq!((inserted, refused), (1, 1));
    assert_eq!(repo.sku_versions().await.unwrap().len(), 1);
    let facts: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM event_outbox WHERE kind = 'products.sku.version.appended'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(facts, 1);
}

fn stamp() -> boss_core::publisher::EventStamp {
    boss_core::publisher::EventStamp::new(
        "products",
        boss_core::actor::ActorId::Automation("test".into()),
    )
    .with_timestamp(chrono::Utc::now())
}

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
}

fn fp_pale() -> Product {
    Product {
        sku: "FP-PALE-1-2-BBL".into(),
        name: "Pale Ale 1/2-BBL Keg".into(),
        product_kind: "beer".into(),
        package_unit: "1/2-bbl-keg".into(),
        description: Some("Flagship pale ale, 5.4% ABV".into()),
        metadata: serde_json::json!({"abv_pct": 5.4, "ibu": 38, "style": "American Pale Ale"}),
        active: true,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn upsert_then_list_returns_the_product() {
    let db = TestDb::new().await;
    let state = ProductsApiState {
        products: Arc::new(PgProducts::new(db.pool.clone())),
        publisher: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
        classes_client: None,
    };
    let r = router(state);

    let resp = r
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/products")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&fp_pale()).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let resp = r
        .oneshot(
            Request::builder()
                .uri("/api/products")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let products = body.as_array().unwrap();
    assert_eq!(products.len(), 1);
    assert_eq!(products[0]["sku"], "FP-PALE-1-2-BBL");
    assert_eq!(products[0]["product_kind"], "beer");
}

#[tokio::test(flavor = "multi_thread")]
async fn detail_rolls_up_inventory_across_locations() {
    let db = TestDb::new().await;
    let repo = Arc::new(PgProducts::new(db.pool.clone()));
    let state = ProductsApiState {
        products: repo.clone(),
        publisher: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
        classes_client: None,
    };
    let r = router(state);

    use boss_products::ProductsRepository;
    repo.upsert_product(&fp_pale(), &stamp()).await.unwrap();
    repo.upsert_inventory(
        &ProductInventory {
            product_sku: "FP-PALE-1-2-BBL".into(),
            location_id: "loc-brewhouse".into(),
            on_hand: 145,
            reserved: 0,
            value_cents: 0,
            production_cost_cents: 0,
            updated_at: None,
        },
        &stamp(),
    )
    .await
    .unwrap();
    repo.upsert_inventory(
        &ProductInventory {
            product_sku: "FP-PALE-1-2-BBL".into(),
            location_id: "loc-taproom".into(),
            on_hand: 12,
            reserved: 0,
            value_cents: 0,
            production_cost_cents: 0,
            updated_at: None,
        },
        &stamp(),
    )
    .await
    .unwrap();

    let resp = r
        .oneshot(
            Request::builder()
                .uri("/api/products/FP-PALE-1-2-BBL")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["sku"], "FP-PALE-1-2-BBL");
    assert_eq!(body["total_on_hand"], 157);
    let inv = body["inventory"].as_array().unwrap();
    assert_eq!(inv.len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn upsert_is_idempotent_on_sku() {
    let db = TestDb::new().await;
    let state = ProductsApiState {
        products: Arc::new(PgProducts::new(db.pool.clone())),
        publisher: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
        classes_client: None,
    };
    let r = router(state);

    let initial = fp_pale();
    let mut updated = initial.clone();
    updated.name = "Pale Ale 1/2-BBL Keg (renamed)".into();

    for body in [&initial, &updated] {
        let resp = r
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/products")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
    }

    let resp = r
        .oneshot(
            Request::builder()
                .uri("/api/products")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = body_json(resp).await;
    let products = body.as_array().unwrap();
    assert_eq!(
        products.len(),
        1,
        "second upsert overwrites, doesn't create new row"
    );
    assert_eq!(products[0]["name"], "Pale Ale 1/2-BBL Keg (renamed)");
}

#[tokio::test(flavor = "multi_thread")]
async fn put_inventory_uses_path_sku_as_authoritative() {
    let db = TestDb::new().await;
    let repo = Arc::new(PgProducts::new(db.pool.clone()));
    let state = ProductsApiState {
        products: repo.clone(),
        publisher: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
        classes_client: None,
    };
    let r = router(state);

    use boss_products::ProductsRepository;
    repo.upsert_product(&fp_pale(), &stamp()).await.unwrap();

    let body = serde_json::json!({
        "product_sku": "WRONG-SKU",
        "location_id": "loc-brewhouse",
        "on_hand": 100,
        "reserved": 0,
    });
    let resp = r
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/products/FP-PALE-1-2-BBL/inventory")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let inv = repo.inventory_for("FP-PALE-1-2-BBL").await.unwrap();
    assert_eq!(inv.len(), 1);
    assert_eq!(inv[0].on_hand, 100);
}

#[tokio::test(flavor = "multi_thread")]
async fn produce_increments_then_consume_decrements() {
    let db = TestDb::new().await;
    let repo = Arc::new(PgProducts::new(db.pool.clone()));
    let state = ProductsApiState {
        products: repo.clone(),
        publisher: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
        classes_client: None,
    };
    let r = router(state);

    use boss_products::ProductsRepository;
    repo.upsert_product(&fp_pale(), &stamp()).await.unwrap();

    // Produce 30 (no row yet → INSERT) then 12 (row → UPDATE).
    for qty in [30, 12] {
        let resp = r
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/products/FP-PALE-1-2-BBL/inventory/produce")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"location_id": "loc-brewhouse", "qty": qty}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
    let inv = repo.inventory_for("FP-PALE-1-2-BBL").await.unwrap();
    assert_eq!(inv[0].on_hand, 42);

    // Consume 5 — should leave 37.
    let resp = r
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/products/FP-PALE-1-2-BBL/inventory/consume")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"location_id": "loc-brewhouse", "qty": 5}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let inv = repo.inventory_for("FP-PALE-1-2-BBL").await.unwrap();
    assert_eq!(inv[0].on_hand, 37);
}

#[tokio::test(flavor = "multi_thread")]
async fn consume_below_zero_is_rejected() {
    let db = TestDb::new().await;
    let repo = Arc::new(PgProducts::new(db.pool.clone()));
    let state = ProductsApiState {
        products: repo.clone(),
        publisher: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
        classes_client: None,
    };
    let r = router(state);

    use boss_products::ProductsRepository;
    repo.upsert_product(&fp_pale(), &stamp()).await.unwrap();
    repo.produce(
        "FP-PALE-1-2-BBL",
        "loc-brewhouse",
        3,
        None,
        chrono::Utc::now(),
        "test-produce".into(),
        &stamp(),
    )
    .await
    .unwrap();

    let resp = r
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/products/FP-PALE-1-2-BBL/inventory/consume")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"location_id": "loc-brewhouse", "qty": 5}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    // on_hand unchanged
    let inv = repo.inventory_for("FP-PALE-1-2-BBL").await.unwrap();
    assert_eq!(inv[0].on_hand, 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn list_default_filters_inactive() {
    let db = TestDb::new().await;
    let repo = Arc::new(PgProducts::new(db.pool.clone()));
    let state = ProductsApiState {
        products: repo.clone(),
        publisher: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
        classes_client: None,
    };
    let r = router(state);

    use boss_products::ProductsRepository;
    let mut active = fp_pale();
    let mut retired = fp_pale();
    retired.sku = "FP-RETIRED-1-2-BBL".into();
    retired.active = false;
    repo.upsert_product(&active, &stamp()).await.unwrap();
    repo.upsert_product(&retired, &stamp()).await.unwrap();
    let _ = &mut active;

    let resp = r
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/products")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = body_json(resp).await;
    assert_eq!(body.as_array().unwrap().len(), 1);

    let resp = r
        .oneshot(
            Request::builder()
                .uri("/api/products?include_inactive=true")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = body_json(resp).await;
    assert_eq!(body.as_array().unwrap().len(), 2);
}

/// With a Class registry wired, `product_kind` + `package_unit` are
/// validated against `(subject_kind='product', code)`: registered codes
/// upsert (201), unregistered ones are rejected (400) with nothing
/// written. Mirrors the boss-inventory vendor-invoice-status gate.
#[tokio::test(flavor = "multi_thread")]
async fn upsert_gates_taxonomy_against_the_class_registry() {
    use boss_classes_client::FakeClassesClient;
    use boss_core::primitives::ClassRef;

    let db = TestDb::new().await;
    let classes = Arc::new(FakeClassesClient::with(vec![
        ClassRef::new("product", "beer"),
        ClassRef::new("product", "1/2-bbl-keg"),
    ])) as Arc<dyn boss_classes_client::ClassesClient>;
    let state = ProductsApiState {
        products: Arc::new(PgProducts::new(db.pool.clone())),
        publisher: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
        classes_client: Some(classes),
    };
    let r = router(state);

    let post = |body: Product| {
        let r = r.clone();
        async move {
            r.oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/products")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap()
        }
    };

    // Registered kind + package_unit → accepted.
    assert_eq!(post(fp_pale()).await.status(), StatusCode::CREATED);

    // Unregistered product_kind → rejected, nothing written.
    let mut wine = fp_pale();
    wine.sku = "FP-WINE-1-2-BBL".into();
    wine.product_kind = "wine".into();
    assert_eq!(post(wine).await.status(), StatusCode::BAD_REQUEST);

    // Unregistered package_unit → rejected.
    let mut drum = fp_pale();
    drum.sku = "FP-PALE-55GAL".into();
    drum.package_unit = "55gal-drum".into();
    assert_eq!(post(drum).await.status(), StatusCode::BAD_REQUEST);

    // Only the accepted product persisted.
    let resp = r
        .oneshot(
            Request::builder()
                .uri("/api/products")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = body_json(resp).await;
    let products = body.as_array().unwrap();
    assert_eq!(products.len(), 1);
    assert_eq!(products[0]["sku"], "FP-PALE-1-2-BBL");
}
