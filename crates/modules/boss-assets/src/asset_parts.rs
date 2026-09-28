//! Per-asset software config + accessories — the Subject-level Parts
//! on a Asset.
//!
//! Parts are Subjects in their own right
//! (docs/architecture-decisions.md §Primitives & information
//! architecture). Every installed unit
//! carries per-instance state that the catalog Model doesn't capture:
//! firmware version + module set + license tier (the "software
//! config"), and the set of attached accessories (the
//! tenant-defined unit-level installable parts — transceivers on a
//! switch, toner heads on a printer, agitators on a fermenter, etc.).
//! This module owns the two backing tables + the Parts-flavored read
//! surface.
//!
//! Exposed endpoints:
//!
//! - `GET  /api/assets/assets/{id}/parts` — merged `Vec<Part>`
//! - `GET  /api/assets/assets/{id}/software-config` — single row or 404
//! - `POST /api/assets/assets/{id}/software-config` — upsert (sim)
//! - `GET  /api/assets/assets/{id}/accessories` — installed accessories
//! - `POST /api/assets/assets/{id}/accessories` — append (sim)
//!
//! The POST endpoints were written for `boss-sim`'s `intake` generator.
//! Both writes ask Update on `asset` — the question the asset event
//! doors ask — and stage their fact on the outbox in the write's own
//! transaction (backlog d2bea664, 2026-09-27). Until then they took no
//! caller, asked no policy and recorded nothing: "No auth on writes in
//! this wave", this header said, and the log could not say who changed
//! an asset's firmware or attached an accessory, or that anyone had.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use boss_core::primitives::Part;
use boss_core::publisher::DomainPublisher;
use boss_policy_client::{CurrentUser, PolicyClient};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::http::require_asset_update_on;

/// An asset's software config was set: the full row as written.
pub const SOFTWARE_CONFIG_UPSERTED: &str = "assets.software_config.upserted";
/// An accessory was attached to an asset: the full row as written.
pub const ACCESSORY_ATTACHED: &str = "assets.accessory.attached";

#[derive(Clone)]
pub struct SystemPartsState {
    pub pool: Arc<PgPool>,
    /// Stamps each staged fact: the caller's actor, wall time and the
    /// sim-origin probe the event doors' publisher carries.
    pub publisher: DomainPublisher,
    /// The policy client both writes ask. Required: the router cannot
    /// be built without one (backlog 2b49ab60).
    pub policy: Arc<dyn PolicyClient>,
}

pub fn asset_parts_router(
    pool: PgPool,
    publisher: DomainPublisher,
    policy: Arc<dyn PolicyClient>,
) -> Router {
    let state = SystemPartsState {
        pool: Arc::new(pool),
        publisher,
        policy,
    };
    Router::new()
        .route("/api/assets/assets/{asset_id}/parts", get(list_parts))
        .route(
            "/api/assets/assets/{asset_id}/software-config",
            get(get_software_config).post(upsert_software_config),
        )
        .route(
            "/api/assets/assets/{asset_id}/accessories",
            get(list_accessories).post(append_accessory),
        )
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct SoftwareConfig {
    pub asset_id: String,
    pub firmware_version: String,
    pub modules: serde_json::Value,
    pub license_tier: String,
    pub last_updated_on: Option<NaiveDate>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Accessory {
    pub id: i64,
    pub asset_id: String,
    pub accessory_kind: String,
    pub serial: Option<String>,
    pub installed_on: Option<NaiveDate>,
    pub removed_on: Option<NaiveDate>,
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpsertSoftwareConfigRequest {
    pub firmware_version: String,
    #[serde(default)]
    pub modules: Vec<String>,
    #[serde(default = "default_license_tier")]
    pub license_tier: String,
    pub last_updated_on: Option<NaiveDate>,
}

fn default_license_tier() -> String {
    "standard".to_string()
}

#[derive(Debug, Deserialize)]
pub struct AppendAccessoryRequest {
    pub accessory_kind: String,
    pub serial: Option<String>,
    pub installed_on: Option<NaiveDate>,
    pub notes: Option<String>,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// Unified Parts view. Merges the single software_config row (as one
/// AttributePart) with the N installed accessories (one AttributePart
/// each). Returns an empty
/// `[]` for assets that haven't been seeded yet rather than a
/// 404 — the UI can show "no data" without a special case.
async fn list_parts(
    State(state): State<SystemPartsState>,
    Path(asset_id): Path<String>,
) -> Response {
    let mut parts: Vec<Part> = Vec::new();

    // Software config (0 or 1 rows).
    match fetch_software_config(&state.pool, &asset_id).await {
        Ok(Some(cfg)) => parts.push(software_config_to_part(&cfg)),
        Ok(None) => {}
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }

    // Installed accessories (0..n). Only currently-installed (not
    // removed) rows show up in the live Parts view; removed ones
    // are history for a future "swap log" endpoint.
    match fetch_installed_accessories(&state.pool, &asset_id).await {
        Ok(accs) => {
            for a in &accs {
                parts.push(accessory_to_part(a));
            }
        }
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }

    Json(parts).into_response()
}

async fn get_software_config(
    State(state): State<SystemPartsState>,
    Path(asset_id): Path<String>,
) -> Response {
    match fetch_software_config(&state.pool, &asset_id).await {
        Ok(Some(cfg)) => Json(cfg).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "no software config").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

/// The stamp for a fact this caller's write stages: their actor, or the
/// platform automation for a request that carried no identity (which
/// policy has already refused unless a rule grants the guest role).
async fn stamp_for(
    state: &SystemPartsState,
    user: &boss_policy_client::User,
) -> boss_core::publisher::EventStamp {
    let actor = user
        .ambient_actor()
        .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into()));
    state.publisher.stamp_with_actor(actor).await
}

fn storage_error(e: impl std::fmt::Display) -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
}

async fn upsert_software_config(
    State(state): State<SystemPartsState>,
    Path(asset_id): Path<String>,
    CurrentUser(user): CurrentUser,
    Json(req): Json<UpsertSoftwareConfigRequest>,
) -> Response {
    if let Err(refused) = require_asset_update_on(state.policy.as_ref(), &user).await {
        return refused;
    }
    let modules = serde_json::to_value(&req.modules).unwrap_or(serde_json::json!([]));
    let stamp = stamp_for(&state, &user).await;
    let mut tx = match state.pool.begin().await {
        Ok(tx) => tx,
        Err(e) => return storage_error(e),
    };
    // The row as written is the fact: RETURNING reads back what the
    // upsert left, so the payload is value-primary for a rebuilder.
    let row = sqlx::query_as::<_, SoftwareConfig>(
        "INSERT INTO asset_software_configs
             (asset_id, firmware_version, modules, license_tier, last_updated_on, updated_at)
         VALUES ($1, $2, $3, $4, $5, NOW())
         ON CONFLICT (asset_id) DO UPDATE SET
             firmware_version = EXCLUDED.firmware_version,
             modules = EXCLUDED.modules,
             license_tier = EXCLUDED.license_tier,
             last_updated_on = EXCLUDED.last_updated_on,
             updated_at = NOW()
         RETURNING asset_id, firmware_version, modules, license_tier, last_updated_on, updated_at",
    )
    .bind(&asset_id)
    .bind(&req.firmware_version)
    .bind(&modules)
    .bind(&req.license_tier)
    .bind(req.last_updated_on)
    .fetch_one(&mut *tx)
    .await;
    let row = match row {
        Ok(row) => row,
        Err(e) => return storage_error(e),
    };
    let payload = serde_json::to_value(&row).unwrap_or_default();
    if let Err(e) = boss_events::outbox::record_event_in_tx(
        &mut tx,
        &stamp.event(SOFTWARE_CONFIG_UPSERTED, payload),
    )
    .await
    {
        return storage_error(e);
    }
    match tx.commit().await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => storage_error(e),
    }
}

async fn list_accessories(
    State(state): State<SystemPartsState>,
    Path(asset_id): Path<String>,
) -> Response {
    match fetch_installed_accessories(&state.pool, &asset_id).await {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

async fn append_accessory(
    State(state): State<SystemPartsState>,
    Path(asset_id): Path<String>,
    CurrentUser(user): CurrentUser,
    Json(req): Json<AppendAccessoryRequest>,
) -> Response {
    if let Err(refused) = require_asset_update_on(state.policy.as_ref(), &user).await {
        return refused;
    }
    let stamp = stamp_for(&state, &user).await;
    let mut tx = match state.pool.begin().await {
        Ok(tx) => tx,
        Err(e) => return storage_error(e),
    };
    let row = sqlx::query_as::<_, Accessory>(
        "INSERT INTO asset_accessories
             (asset_id, accessory_kind, serial, installed_on, notes)
         VALUES ($1, $2, $3, $4, $5)
         RETURNING id, asset_id, accessory_kind, serial, installed_on, removed_on, notes",
    )
    .bind(&asset_id)
    .bind(&req.accessory_kind)
    .bind(&req.serial)
    .bind(req.installed_on)
    .bind(&req.notes)
    .fetch_one(&mut *tx)
    .await;
    let row = match row {
        Ok(row) => row,
        Err(e) => return storage_error(e),
    };
    let payload = serde_json::to_value(&row).unwrap_or_default();
    if let Err(e) =
        boss_events::outbox::record_event_in_tx(&mut tx, &stamp.event(ACCESSORY_ATTACHED, payload))
            .await
    {
        return storage_error(e);
    }
    match tx.commit().await {
        Ok(()) => StatusCode::CREATED.into_response(),
        Err(e) => storage_error(e),
    }
}

// ---------------------------------------------------------------------------
// Queries
// ---------------------------------------------------------------------------

async fn fetch_software_config(
    pool: &PgPool,
    asset_id: &str,
) -> Result<Option<SoftwareConfig>, String> {
    sqlx::query_as::<_, SoftwareConfig>(
        "SELECT asset_id, firmware_version, modules, license_tier, last_updated_on, updated_at
         FROM asset_software_configs
         WHERE asset_id = $1",
    )
    .bind(asset_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| e.to_string())
}

async fn fetch_installed_accessories(
    pool: &PgPool,
    asset_id: &str,
) -> Result<Vec<Accessory>, String> {
    sqlx::query_as::<_, Accessory>(
        "SELECT id, asset_id, accessory_kind, serial, installed_on, removed_on, notes
         FROM asset_accessories
         WHERE asset_id = $1 AND removed_on IS NULL
         ORDER BY installed_on NULLS LAST, id",
    )
    .bind(asset_id)
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Part conversion
// ---------------------------------------------------------------------------

pub fn software_config_to_part(cfg: &SoftwareConfig) -> Part {
    Part::attribute(
        "software_config",
        serde_json::to_value(cfg).expect("SoftwareConfig serialises"),
    )
}

pub fn accessory_to_part(acc: &Accessory) -> Part {
    Part::attribute(
        "accessory",
        serde_json::to_value(acc).expect("Accessory serialises"),
    )
}

#[cfg(test)]
mod part_conversion_tests {
    use super::*;

    #[test]
    fn software_config_becomes_attribute_part() {
        let cfg = SoftwareConfig {
            asset_id: "SYS-0001".into(),
            firmware_version: "2.3.1".into(),
            modules: serde_json::json!(["erbium-driver", "diode-driver"]),
            license_tier: "pro".into(),
            last_updated_on: NaiveDate::from_ymd_opt(2026, 3, 15),
            updated_at: chrono::Utc::now(),
        };
        let p = software_config_to_part(&cfg);
        match p {
            Part::Attribute { key, value } => {
                assert_eq!(key, "software_config");
                assert_eq!(
                    value.get("firmware_version").and_then(|v| v.as_str()),
                    Some("2.3.1"),
                );
                assert_eq!(
                    value.get("license_tier").and_then(|v| v.as_str()),
                    Some("pro"),
                );
            }
            Part::Subject { .. } => panic!("software_config should be AttributePart"),
        }
    }

    #[test]
    fn accessory_becomes_attribute_part() {
        let acc = Accessory {
            id: 42,
            asset_id: "SYS-0001".into(),
            accessory_kind: "erbium".into(),
            serial: Some("HP-ER-001".into()),
            installed_on: NaiveDate::from_ymd_opt(2026, 1, 10),
            removed_on: None,
            notes: None,
        };
        let p = accessory_to_part(&acc);
        match p {
            Part::Attribute { key, value } => {
                assert_eq!(key, "accessory");
                assert_eq!(
                    value.get("accessory_kind").and_then(|v| v.as_str()),
                    Some("erbium"),
                );
            }
            Part::Subject { .. } => panic!("accessory should be AttributePart"),
        }
    }
}
