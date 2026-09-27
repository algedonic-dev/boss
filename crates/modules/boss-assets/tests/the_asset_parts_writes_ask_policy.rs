//! The two asset Parts writes ask policy and stage their fact
//! (backlog d2bea664, 2026-09-27).
//!
//! `POST /api/assets/assets/{id}/software-config` and
//! `POST /api/assets/assets/{id}/accessories` took no caller, asked no
//! policy and ran a bare INSERT on the pool, so any caller reaching the
//! port could rewrite an asset's firmware record or attach accessories
//! to it, and the log held nothing of it. Each now asks Update on asset
//! at scope `all` — the question the asset event doors ask — and stages
//! one fact, signed by the caller, in the write's own transaction.

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use boss_assets::PgAssets;
use boss_assets::asset_parts::{ACCESSORY_ATTACHED, SOFTWARE_CONFIG_UPSERTED, asset_parts_router};
use boss_assets::port::AssetsRepository;
use boss_assets::types::{AssetEvent, AssetEventId, AssetEventKind, AssetId, IntakeSource};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::{RecordingEventBus, TestDb, TestRequest};
use chrono::NaiveDate;
use sqlx::PgPool;

const TECH: &str = "field-tech";
const SERIAL: &str = "SN-PARTS-1";

/// A database holding one asset for the Parts rows to reference.
async fn db_with_an_asset() -> TestDb {
    let db = TestDb::new().await;
    PgAssets::new(db.pool.clone())
        .append(AssetEvent {
            id: AssetEventId::new("evt-parts-received"),
            asset_id: AssetId::new(SERIAL),
            ts: NaiveDate::from_ymd_opt(2026, 4, 1).unwrap(),
            actor_id: boss_core::actor::ActorId::human("emp-intake"),
            kind: AssetEventKind::Received {
                sku: None,
                source: IntakeSource::new("oem-new"),
                oem_serial: None,
            },
        })
        .await
        .expect("seed the asset");
    db
}

fn app(pool: PgPool, policy: Arc<dyn PolicyClient>) -> Router {
    let bus = RecordingEventBus::new();
    let publisher = DomainPublisher::new(bus as Arc<dyn EventBus>, "assets");
    asset_parts_router(pool, publisher, policy)
}

fn grant(rules: &[(Action, Resource, Scope)]) -> Arc<dyn PolicyClient> {
    Arc::new(
        rules
            .iter()
            .fold(FakePolicyClient::builder(), |b, r| {
                b.allow(TECH, r.0, r.1.clone(), r.2.clone())
            })
            .build(),
    )
}

/// (software-config status, accessories status) for a tech's two writes.
async fn write_both(app: &Router) -> (StatusCode, StatusCode) {
    let config = TestRequest::post(format!("/api/assets/assets/{SERIAL}/software-config"))
        .as_user("emp-tech", TECH)
        .json(&serde_json::json!({
            "firmware_version": "2.3.1",
            "modules": ["erbium-driver"],
            "license_tier": "pro",
        }))
        .send(app)
        .await
        .status;
    let accessory = TestRequest::post(format!("/api/assets/assets/{SERIAL}/accessories"))
        .as_user("emp-tech", TECH)
        .json(&serde_json::json!({
            "accessory_kind": "transceiver",
            "serial": "TX-001",
        }))
        .send(app)
        .await
        .status;
    (config, accessory)
}

/// (software-config rows, accessory rows, facts staged of either kind).
async fn left_behind(pool: &PgPool) -> (i64, i64, i64) {
    let count = |sql: &'static str| async move {
        sqlx::query_scalar::<_, i64>(sql)
            .fetch_one(pool)
            .await
            .unwrap()
    };
    (
        count("SELECT count(*) FROM asset_software_configs").await,
        count("SELECT count(*) FROM asset_accessories").await,
        count(
            "SELECT count(*) FROM event_outbox \
             WHERE kind IN ('assets.software_config.upserted', 'assets.accessory.attached')",
        )
        .await,
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_denied_caller_writes_no_part_and_stages_no_fact() {
    let db = db_with_an_asset().await;
    let app = app(db.pool.clone(), Arc::new(FakePolicyClient::deny_all()));
    assert_eq!(
        write_both(&app).await,
        (StatusCode::FORBIDDEN, StatusCode::FORBIDDEN)
    );
    assert_eq!(left_behind(&db.pool).await, (0, 0, 0));
}

/// Another verb, another resource, or Update on asset below scope all
/// opens neither write.
#[tokio::test(flavor = "multi_thread")]
async fn only_update_on_asset_at_scope_all_opens_the_parts_writes() {
    let db = db_with_an_asset().await;
    for rules in [
        vec![(Action::Update, Resource::invoice(), Scope::All)],
        vec![(Action::Create, Resource::asset(), Scope::All)],
        vec![(Action::Update, Resource::part(), Scope::All)],
        vec![(Action::Update, Resource::asset(), Scope::Team)],
    ] {
        let app = app(db.pool.clone(), grant(&rules));
        assert_eq!(
            write_both(&app).await,
            (StatusCode::FORBIDDEN, StatusCode::FORBIDDEN),
            "{rules:?}"
        );
    }
    assert_eq!(left_behind(&db.pool).await, (0, 0, 0));
}

/// Granted Update on asset at scope all, both rows land and each write
/// stages one fact carrying the row it wrote, signed by the caller.
#[tokio::test(flavor = "multi_thread")]
async fn a_granted_caller_writes_both_parts_and_signs_both_facts() {
    let db = db_with_an_asset().await;
    let app = app(
        db.pool.clone(),
        grant(&[(Action::Update, Resource::asset(), Scope::All)]),
    );
    assert_eq!(
        write_both(&app).await,
        (StatusCode::NO_CONTENT, StatusCode::CREATED)
    );
    assert_eq!(left_behind(&db.pool).await, (1, 1, 2));

    let facts: Vec<(String, serde_json::Value)> = sqlx::query_as(
        "SELECT kind, payload FROM event_outbox \
         WHERE source = 'assets' ORDER BY kind",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(facts.len(), 2);
    let (accessory_kind, accessory) = &facts[0];
    let (config_kind, config) = &facts[1];
    assert_eq!(accessory_kind, ACCESSORY_ATTACHED);
    assert_eq!(config_kind, SOFTWARE_CONFIG_UPSERTED);
    assert_eq!(config["asset_id"], SERIAL);
    assert_eq!(config["firmware_version"], "2.3.1");
    assert_eq!(config["license_tier"], "pro");
    assert_eq!(accessory["asset_id"], SERIAL);
    assert_eq!(accessory["accessory_kind"], "transceiver");
    assert!(accessory["id"].is_i64(), "the fact carries the row's id");
    assert_eq!(config["_actor"], "emp-tech");
    assert_eq!(accessory["_actor"], "emp-tech");
}
