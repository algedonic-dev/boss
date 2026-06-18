//! End-to-end tests against the products HTTP API + Postgres adapter.

#![cfg(feature = "postgres")]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_products::http::{ProductsApiState, router};
use boss_products::postgres::PgProducts;
use boss_products::types::{Product, ProductInventory};
use boss_testing::TestDb;
use http_body_util::BodyExt;
use tower::util::ServiceExt;

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
    };
    let r = router(state);

    use boss_products::ProductsRepository;
    repo.upsert_product(&fp_pale()).await.unwrap();
    repo.upsert_inventory(&ProductInventory {
        product_sku: "FP-PALE-1-2-BBL".into(),
        location_id: "loc-brewhouse".into(),
        on_hand: 145,
        reserved: 0,
        production_cost_cents: 0,
        updated_at: None,
    })
    .await
    .unwrap();
    repo.upsert_inventory(&ProductInventory {
        product_sku: "FP-PALE-1-2-BBL".into(),
        location_id: "loc-taproom".into(),
        on_hand: 12,
        reserved: 0,
        production_cost_cents: 0,
        updated_at: None,
    })
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
    };
    let r = router(state);

    use boss_products::ProductsRepository;
    repo.upsert_product(&fp_pale()).await.unwrap();

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
    };
    let r = router(state);

    use boss_products::ProductsRepository;
    repo.upsert_product(&fp_pale()).await.unwrap();

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
    };
    let r = router(state);

    use boss_products::ProductsRepository;
    repo.upsert_product(&fp_pale()).await.unwrap();
    repo.produce(
        "FP-PALE-1-2-BBL",
        "loc-brewhouse",
        3,
        None,
        chrono::Utc::now(),
        "test-produce".into(),
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
    };
    let r = router(state);

    use boss_products::ProductsRepository;
    let mut active = fp_pale();
    let mut retired = fp_pale();
    retired.sku = "FP-RETIRED-1-2-BBL".into();
    retired.active = false;
    repo.upsert_product(&active).await.unwrap();
    repo.upsert_product(&retired).await.unwrap();
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
