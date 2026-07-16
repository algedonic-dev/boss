//! The `subjects` identity table — the home for `(kind, id)`.
//!
//! R1 of `docs/design/subject-identity-and-relationships.md`
//! (approved 2026-07-15). One deliberately thin row per subject:
//! identity only, no attributes — the minimal durable fact "this
//! subject exists". Domain tables and KB views keep everything else.
//!
//! Dual write contract (Q1):
//! - **write-through** — domain services call [`record_subject_in_tx`]
//!   inside the SAME transaction as their domain-row insert, so a
//!   subject's identity is durable exactly when the subject is (the
//!   `record_fact_in_tx` shape);
//! - **projection** — `boss-subjects-rebuild` reproduces every row
//!   from audit_log alone, so the deep-check discipline owns this
//!   table like any other.
//!
//! The FK onto `subject_kinds(kind)` makes the vocabulary gate
//! structural: no identity row can exist for an unregistered kind.
//!
//! Function-style like `boss_events::outbox` (the sibling in-tx
//! helper) rather than a trait port: the callers are write-through
//! sites and the HTTP mint, both Postgres-shaped by construction.

use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use sqlx::{PgPool, Postgres, Transaction};

const UPSERT_SQL: &str = "INSERT INTO subjects (kind, id, label) VALUES ($1, $2, $3) \
     ON CONFLICT (kind, id) \
     DO UPDATE SET label = COALESCE(EXCLUDED.label, subjects.label)";

/// Upsert the identity row inside the caller's transaction. A NULL
/// `label` never erases an earlier one. An unregistered `kind` is
/// rejected by the FK and surfaces as `Err` — the caller's whole
/// domain write aborts with it, which is the point.
pub async fn record_subject_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    kind: &str,
    id: &str,
    label: Option<&str>,
) -> Result<(), String> {
    sqlx::query(UPSERT_SQL)
        .bind(kind)
        .bind(id)
        .bind(label)
        .execute(&mut **tx)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Pool-level upsert for callers with no surrounding transaction
/// (the HTTP mint, seeds, backfills).
pub async fn upsert_subject(
    pool: &PgPool,
    kind: &str,
    id: &str,
    label: Option<&str>,
) -> Result<(), String> {
    sqlx::query(UPSERT_SQL)
        .bind(kind)
        .bind(id)
        .bind(label)
        .execute(pool)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// The uniform existence probe — one indexed lookup for every kind,
/// tenant-defined included. Retired subjects still exist (historical
/// jobs reference them); retirement semantics for NEW references are
/// an R2 edge-policy concern, not an identity one.
pub async fn subject_exists(pool: &PgPool, kind: &str, id: &str) -> Result<bool, String> {
    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM subjects WHERE kind = $1 AND id = $2)")
        .bind(kind)
        .bind(id)
        .fetch_one(pool)
        .await
        .map_err(|e| e.to_string())
}

#[derive(Clone)]
struct SubjectsApiState {
    pool: PgPool,
}

/// The `/api/subjects` surface, mounted by the service bin alongside
/// the read-only kinds router. POST is the mint path (the sim's
/// campaign identities, operator tooling, R3's single minting
/// authority later); GET is the cross-service existence probe.
pub fn subjects_router(pool: PgPool) -> Router {
    Router::new()
        .route("/api/subjects", post(post_subject))
        // Kind-scoped mint: the sim's birth event routes POST their
        // synthesized payload (id + label, no kind field) here.
        .route("/api/subjects/{kind}", post(post_subject_for_kind))
        .route("/api/subjects/{kind}/{id}", get(get_subject))
        .with_state(SubjectsApiState { pool })
}

#[derive(Deserialize)]
struct SubjectBody {
    kind: String,
    id: String,
    #[serde(default)]
    label: Option<String>,
}

async fn post_subject(
    State(state): State<SubjectsApiState>,
    axum::Json(body): axum::Json<SubjectBody>,
) -> Response {
    if body.kind.trim().is_empty() || body.id.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "kind and id are required").into_response();
    }
    match upsert_subject(&state.pool, &body.kind, &body.id, body.label.as_deref()).await {
        Ok(()) => StatusCode::CREATED.into_response(),
        // The FK rejection = unregistered kind → the caller's error,
        // not ours.
        Err(e) if e.contains("subjects_kind_fkey") => (
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("unregistered subject kind `{}`", body.kind),
        )
            .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

async fn get_subject(
    State(state): State<SubjectsApiState>,
    Path((kind, id)): Path<(String, String)>,
) -> Response {
    match subject_exists(&state.pool, &kind, &id).await {
        Ok(true) => {
            axum::Json(serde_json::json!({"kind": kind, "id": id, "exists": true})).into_response()
        }
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

#[derive(Deserialize)]
struct KindScopedBody {
    id: String,
    #[serde(default)]
    #[serde(alias = "name", alias = "title")]
    label: Option<String>,
}

async fn post_subject_for_kind(
    State(state): State<SubjectsApiState>,
    Path(kind): Path<String>,
    axum::Json(body): axum::Json<serde_json::Value>,
) -> Response {
    // Tolerant extraction: birth payloads are synthesized event
    // bodies; only `id` is contractual, label rides `label`/`name`/
    // `title` when present.
    let parsed: KindScopedBody = match serde_json::from_value(body) {
        Ok(p) => p,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("bad body: {e}")).into_response(),
    };
    if parsed.id.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "id is required").into_response();
    }
    match upsert_subject(&state.pool, &kind, &parsed.id, parsed.label.as_deref()).await {
        Ok(()) => StatusCode::CREATED.into_response(),
        Err(e) if e.contains("subjects_kind_fkey") => (
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("unregistered subject kind `{kind}`"),
        )
            .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

const IDENTITY_SOURCES_TOML: &str = include_str!("../seeds/subject_identity_sources.toml");

#[derive(Deserialize)]
struct SourcesToml {
    source: Vec<IdentitySource>,
}

#[derive(Deserialize)]
struct IdentitySource {
    event_kind: String,
    subject_kind: String,
    id_field: String,
    #[serde(default)]
    label_field: Option<String>,
}

/// Reproject the identity table from `audit_log` (the Q1 projection
/// half; wired into `boss-rebuild-all` like every other rebuilder).
/// Truncate-and-reproject:
///
/// 1. the TOML-registered identity-bearing events;
/// 2. every `jobs.job.created` subject pair (identity-first — a Job
///    about a subject proves it existed; homes table-less kinds);
/// 3. the `locations` reference table — locations are seed-only
///    reference rows with no create events BY DESIGN (the audit's
///    "no write path exists"), so their identity derives from the
///    reference table the same way ledger rebuilders read
///    `gl_accounts`. Everything event-sourced comes from the log.
pub async fn rebuild_subjects(pool: &PgPool) -> Result<u64, String> {
    let sources: SourcesToml = toml::from_str(IDENTITY_SOURCES_TOML).map_err(|e| e.to_string())?;
    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
    sqlx::query("TRUNCATE subjects")
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;

    let mut total = 0u64;
    for src in &sources.source {
        let label_expr = match &src.label_field {
            Some(f) => format!("payload->>'{f}'"),
            None => "NULL".to_string(),
        };
        // One row per subject id, NEWEST event (highest audit id)
        // winning the label. `*.upserted` kinds emit many events per
        // id, and a single INSERT … ON CONFLICT DO UPDATE that hits
        // the same (kind, id) twice aborts the whole rebuild
        // ("cannot affect row a second time") — dedup must happen
        // inside the statement.
        let sql = format!(
            "INSERT INTO subjects (kind, id, label) \
             SELECT $1, ev.subject_id, ev.label FROM ( \
                 SELECT DISTINCT ON (payload->>'{id}') \
                        payload->>'{id}' AS subject_id, {label_expr} AS label \
                   FROM audit_log \
                  WHERE kind = $2 AND payload->>'{id}' IS NOT NULL \
                  ORDER BY payload->>'{id}', id DESC \
             ) ev \
             ON CONFLICT (kind, id) DO UPDATE \
                SET label = COALESCE(EXCLUDED.label, subjects.label)",
            id = src.id_field,
        );
        let res = sqlx::query(&sql)
            .bind(&src.subject_kind)
            .bind(&src.event_kind)
            .execute(&mut *tx)
            .await
            .map_err(|e| format!("event pass {}: {e}", src.event_kind))?;
        total += res.rows_affected();
    }

    let res = sqlx::query(
        "INSERT INTO subjects (kind, id) \
         SELECT DISTINCT payload->>'subject_kind', payload->>'subject_id' \
         FROM audit_log \
         WHERE kind = 'jobs.job.created' \
           AND payload->>'subject_kind' IS NOT NULL \
           AND payload->>'subject_id' IS NOT NULL \
           AND EXISTS (SELECT 1 FROM subject_kinds k WHERE k.kind = payload->>'subject_kind') \
         ON CONFLICT (kind, id) DO NOTHING",
    )
    .execute(&mut *tx)
    .await
    .map_err(|e| format!("job-subject pass: {e}"))?;
    total += res.rows_affected();

    let res = sqlx::query(
        "INSERT INTO subjects (kind, id, label) \
         SELECT 'location', id, name FROM locations \
         ON CONFLICT (kind, id) DO NOTHING",
    )
    .execute(&mut *tx)
    .await
    .map_err(|e| format!("locations reference pass: {e}"))?;
    total += res.rows_affected();

    tx.commit().await.map_err(|e| e.to_string())?;
    Ok(total)
}
