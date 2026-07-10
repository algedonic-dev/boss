//! HTTP surface for `boss-products`.
//!
//! - `GET  /api/products/health`            — liveness
//! - `GET  /api/products`                   — catalog list, default active=true
//! - `GET  /api/products/:sku`              — detail + on-hand-by-location
//! - `POST /api/products`                   — upsert one (admin)
//! - `POST /api/products/batch`             — upsert N (seed loader)
//! - `PUT  /api/products/:sku/inventory`    — set on-hand for one (sku, location)

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use boss_core::publisher::DomainPublisher;
use serde::Deserialize;

use crate::events;
use crate::port::{ProductsError, ProductsRepository};
use crate::types::{Product, ProductDetail, ProductInventory};

#[derive(Clone)]
pub struct ProductsApiState<R: ProductsRepository> {
    pub products: Arc<R>,
    pub publisher: Option<Arc<DomainPublisher>>,
    /// Authoritative clock. See `boss-clock-client`. Every
    /// handler stamps dates via `state.clock.now().await`.
    pub clock: Arc<dyn boss_clock_client::ClockClient>,
}

pub fn router<R: ProductsRepository + 'static>(state: ProductsApiState<R>) -> Router {
    let shared = Arc::new(state);
    Router::new()
        .route("/api/products/health", get(health))
        .route(
            "/api/products",
            get(list_products::<R>).post(upsert_product::<R>),
        )
        .route("/api/products/batch", post(upsert_products_batch::<R>))
        .route("/api/products/{sku}", get(get_product_detail::<R>))
        .route("/api/products/{sku}/inventory", put(put_inventory::<R>))
        .route(
            "/api/products/{sku}/inventory/produce",
            post(post_produce::<R>),
        )
        .route(
            "/api/products/{sku}/inventory/consume",
            post(post_consume::<R>),
        )
        .with_state(shared)
}

async fn health() -> Response {
    Json(serde_json::json!({"status": "ok"})).into_response()
}

#[derive(Deserialize, Default)]
struct ListQuery {
    /// Default true. `?include_inactive=true` to see retired SKUs.
    #[serde(default)]
    include_inactive: bool,
}

async fn list_products<R: ProductsRepository + 'static>(
    State(state): State<Arc<ProductsApiState<R>>>,
    Query(q): Query<ListQuery>,
) -> Response {
    // Roll up on-hand per sku alongside the catalog row. Pre-fix
    // the list endpoint only returned the catalog (no total_on_hand
    // field), so the Exec dashboard's 'Finished goods on hand'
    // panel always read 0 even when the GL showed real FG. The
    // detail endpoint already does this rollup; the list endpoint
    // now does it too at small extra cost (one inventory_for call
    // per product on warm cache).
    let products = match state.products.list_products(!q.include_inactive).await {
        Ok(p) => p,
        Err(e) => return error_response(e),
    };
    let mut out: Vec<serde_json::Value> = Vec::with_capacity(products.len());
    for product in products {
        let total_on_hand: i32 = match state.products.inventory_for(&product.sku).await {
            Ok(rows) => rows.iter().map(|r| r.on_hand).sum(),
            Err(_) => 0,
        };
        let mut v = serde_json::to_value(&product).unwrap_or(serde_json::json!({}));
        if let Some(obj) = v.as_object_mut() {
            obj.insert(
                "total_on_hand".to_string(),
                serde_json::json!(total_on_hand),
            );
        }
        out.push(v);
    }
    Json(out).into_response()
}

async fn get_product_detail<R: ProductsRepository + 'static>(
    State(state): State<Arc<ProductsApiState<R>>>,
    Path(sku): Path<String>,
) -> Response {
    let product = match state.products.get_product(&sku).await {
        Ok(Some(p)) => p,
        Ok(None) => {
            return (StatusCode::NOT_FOUND, "product not found".to_string()).into_response();
        }
        Err(e) => return error_response(e),
    };
    let inventory = match state.products.inventory_for(&sku).await {
        Ok(rows) => rows,
        Err(e) => return error_response(e),
    };
    let total_on_hand: i32 = inventory.iter().map(|r| r.on_hand).sum();
    Json(ProductDetail {
        product,
        inventory,
        total_on_hand,
    })
    .into_response()
}

async fn upsert_product<R: ProductsRepository + 'static>(
    State(state): State<Arc<ProductsApiState<R>>>,
    Json(product): Json<Product>,
) -> Response {
    match state.products.upsert_product(&product).await {
        Ok(()) => {
            if let Some(pub_) = &state.publisher {
                pub_.emit_at(
                    events::PRODUCT_UPSERTED,
                    serde_json::to_value(&product).unwrap_or_default(),
                    boss_clock_client::now_from(&state.clock).await,
                )
                .await;
            }
            (StatusCode::CREATED, Json(product)).into_response()
        }
        Err(e) => error_response(e),
    }
}

async fn upsert_products_batch<R: ProductsRepository + 'static>(
    State(state): State<Arc<ProductsApiState<R>>>,
    Json(products): Json<Vec<Product>>,
) -> Response {
    let now = boss_clock_client::now_from(&state.clock).await;
    let mut inserted = 0u64;
    for p in &products {
        if state.products.upsert_product(p).await.is_ok() {
            inserted += 1;
            if let Some(pub_) = &state.publisher {
                pub_.emit_at(
                    events::PRODUCT_UPSERTED,
                    serde_json::to_value(p).unwrap_or_default(),
                    now,
                )
                .await;
            }
        }
    }
    Json(serde_json::json!({"upserted": inserted})).into_response()
}

async fn put_inventory<R: ProductsRepository + 'static>(
    State(state): State<Arc<ProductsApiState<R>>>,
    Path(sku): Path<String>,
    Json(mut row): Json<ProductInventory>,
) -> Response {
    // The path SKU is authoritative; ignore any mismatched body field.
    row.product_sku = sku;
    match state.products.upsert_inventory(&row).await {
        Ok(()) => {
            if let Some(pub_) = &state.publisher {
                pub_.emit_at(
                    events::PRODUCT_INVENTORY_UPSERTED,
                    serde_json::to_value(&row).unwrap_or_default(),
                    boss_clock_client::now_from(&state.clock).await,
                )
                .await;
            }
            // Atomic opening-balance JE. When PUT lands a row
            // carrying value, post DR 1320 (FG) / CR 3000 (Retained
            // Earnings) sized at exactly value_cents — the conserved
            // quantity the row now holds, so the BS and the
            // projection agree by construction (PR 6a;
            // production_cost_cents is derived display and never
            // sizes a JE). Idempotent on (kind, source_table,
            // source_id) — the brewery_data_seed external
            // `opening-fg-{sku}` JE collides cleanly with this atomic
            // post, so re-runs no-op.
            //
            // Sibling to the batch-upsert opening JE for raw
            // materials. Together they keep 1300 / 1320 reconciled
            // with physical inventory across seed-time + manual
            // adjustments.
            let total_cost = row.value_cents;
            if total_cost > 0 {
                let memo = format!(
                    "Opening balance — {} × {} (FG ← retained earnings)",
                    row.on_hand, row.product_sku
                );
                let source_id = format!("opening-fg-{}", row.product_sku);
                let now = boss_clock_client::now_from(&state.clock).await;
                match state
                    .products
                    .record_inventory_je(
                        total_cost,
                        "1320",
                        "3000",
                        &memo,
                        "brewery_seed_opening_balance",
                        &source_id,
                        now.date_naive(),
                    )
                    .await
                {
                    // The fact's WRITER owns its rebuild source: emit
                    // the in-tx payload verbatim, only when THIS call
                    // inserted it (see the inventory batch-upsert twin
                    // for the 2026-07-09 regression this closes).
                    Ok(je) => {
                        if je.inserted
                            && let Some(pub_) = &state.publisher
                        {
                            pub_.emit_at(
                                crate::events::LEDGER_INVENTORY_TRANSFERRED,
                                je.payload,
                                now,
                            )
                            .await;
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            sku = %row.product_sku,
                            error = %e,
                            "put_inventory: opening JE post failed (row upserted)"
                        );
                    }
                }
            }
            Json(row).into_response()
        }
        Err(e) => error_response(e),
    }
}

#[derive(Deserialize)]
struct DeltaBody {
    location_id: String,
    qty: i32,
    /// The line's TOTAL cost in cents — the exact WIP share this
    /// produce drains (PR 6a: line totals, not unit costs, so the
    /// largest-remainder allocation posts un-rounded). Only used by
    /// `produce`; `consume` drains the row's conserved value
    /// proportionally. None = no value added + no GL JE.
    #[serde(default)]
    total_cost_cents: Option<i64>,
    /// Optional revenue category tag for consume — passed through
    /// to the emitted `finance.cogs.recognized` payload so per-
    /// category gross margin rolls up exactly rather than via
    /// pro-rating against the period revenue mix. Omitted on
    /// produce calls and on consume calls that aren't sale-tied.
    #[serde(default)]
    revenue_category: Option<String>,
    /// Deterministic idempotency key from the triggering step
    /// (`{step_id}:{sku}`). Becomes the produce/consume `source_id` so a
    /// redelivered step-effect event resolves to the same fact and the
    /// relative `on_hand ± qty` is applied exactly once. Absent for direct
    /// callers → a random source_id (no cross-call dedup).
    #[serde(default)]
    idempotency_key: Option<String>,
}

async fn post_produce<R: ProductsRepository + 'static>(
    State(state): State<Arc<ProductsApiState<R>>>,
    Path(sku): Path<String>,
    Json(body): Json<DeltaBody>,
) -> Response {
    let now = boss_clock_client::now_from(&state.clock).await;
    let source_id = match &body.idempotency_key {
        Some(key) => format!("produce:{key}"),
        None => format!("produce:{sku}:{}", uuid::Uuid::new_v4()),
    };
    match state
        .products
        .produce(
            &sku,
            &body.location_id,
            body.qty,
            body.total_cost_cents,
            now,
            source_id,
        )
        .await
    {
        Ok(result) => {
            if let Some(pub_) = &state.publisher {
                pub_.emit_at(
                    events::PRODUCT_INVENTORY_UPSERTED,
                    serde_json::to_value(&result.inventory).unwrap_or_default(),
                    now,
                )
                .await;
                // Emit the auditable WIP→FG GL move so bundle
                // export + rebuild reproduces the financial_fact
                // through `gl_fact_projection_rules`. The payload
                // IS the projected fact payload — no transform.
                if let Some(gl) = &result.gl_move {
                    pub_.emit_at(events::PRODUCT_PRODUCED, gl.payload.clone(), now)
                        .await;
                }
            }
            Json(result.inventory).into_response()
        }
        Err(e) => error_response(e),
    }
}

async fn post_consume<R: ProductsRepository + 'static>(
    State(state): State<Arc<ProductsApiState<R>>>,
    Path(sku): Path<String>,
    Json(body): Json<DeltaBody>,
) -> Response {
    let now = boss_clock_client::now_from(&state.clock).await;
    let source_id = match &body.idempotency_key {
        Some(key) => format!("consume:{key}"),
        None => format!("consume:{sku}:{}", uuid::Uuid::new_v4()),
    };
    match state
        .products
        .consume(
            &sku,
            &body.location_id,
            body.qty,
            body.revenue_category.as_deref(),
            now,
            source_id,
        )
        .await
    {
        Ok(result) => {
            if let Some(pub_) = &state.publisher {
                pub_.emit_at(
                    events::PRODUCT_INVENTORY_UPSERTED,
                    serde_json::to_value(&result.inventory).unwrap_or_default(),
                    now,
                )
                .await;
                // Emit the auditable FG→COGS GL move. Symmetric
                // to PRODUCT_PRODUCED above — pure passthrough to
                // `finance.cogs.recognized` via the projection rule.
                if let Some(gl) = &result.gl_move {
                    pub_.emit_at(events::PRODUCT_CONSUMED, gl.payload.clone(), now)
                        .await;
                }
            }
            Json(result.inventory).into_response()
        }
        Err(e) => error_response(e),
    }
}

fn error_response(e: ProductsError) -> Response {
    let status = match &e {
        ProductsError::NotFound(_) => StatusCode::NOT_FOUND,
        ProductsError::Invalid(_) => StatusCode::BAD_REQUEST,
        ProductsError::Storage(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, e.to_string()).into_response()
}
