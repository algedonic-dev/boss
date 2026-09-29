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
//! A write-through leans on its caller's domain event for the fact (an
//! `accounts.account.created` names the account; the TOML sources map
//! it to its identity row). The mint door has no domain event, so it
//! stages its own — [`SUBJECT_MINTED`] / [`SUBJECT_RELABELLED`] — in
//! the write's transaction, and the rebuild replays them (backlog
//! 92473357). Until 2026-09-27 it staged nothing, and every identity
//! it minted was gone after the first rebuild.
//!
//! The FK onto `subject_kinds(kind)` makes the vocabulary gate
//! structural: no identity row can exist for an unregistered kind.
//!
//! Function-style like `boss_events::outbox` (the sibling in-tx
//! helper) rather than a trait port: the callers are write-through
//! sites and the HTTP mint, both Postgres-shaped by construction.

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use boss_core::event::Event;
use boss_core::publish::{FieldChange, KeptRow, ModeQuery, PublishMode, UpdatedRow};
use boss_core::publisher::EventStamp;
use boss_policy_client::CurrentUser;
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Transaction};

/// A subject identity was minted through the mint door: `subject_kind`,
/// `id` and the `label` it was minted with (`null` when none).
pub const SUBJECT_MINTED: &str = "subjects.subject.minted";
/// A held subject's label was replaced under `?mode=take`: `from` the
/// label it held, to `label`.
pub const SUBJECT_RELABELLED: &str = "subjects.subject.relabelled";

/// The fact of a mint. Public so a write-through whose own domain
/// event names no identity can stage it beside [`record_subject_in_tx`]
/// and be rebuilt by the same pass.
pub fn minted_event(stamp: &EventStamp, kind: &str, id: &str, label: Option<&str>) -> Event {
    stamp.event(
        SUBJECT_MINTED,
        serde_json::json!({"subject_kind": kind, "id": id, "label": label}),
    )
}

fn relabelled_event(
    stamp: &EventStamp,
    kind: &str,
    id: &str,
    from: Option<&str>,
    label: &str,
) -> Event {
    stamp.event(
        SUBJECT_RELABELLED,
        serde_json::json!({"subject_kind": kind, "id": id, "from": from, "label": label}),
    )
}

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

/// What the mint door did with one declaration: the batch doors'
/// answer shape (`boss_core::publish`), for a batch of one.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SubjectPublishOutcome {
    pub received: usize,
    pub inserted: usize,
    pub kept: Vec<KeptRow>,
    pub updated: Vec<UpdatedRow>,
    pub unchanged: usize,
}

/// Publish one identity row — insert-if-absent by `(kind, id)` (design
/// e187198f: THE INSTANCE IS THE TRUTH). A row already there keeps its
/// label under the default and the outcome names `label` as differing
/// when the declaration disagrees; only [`PublishMode::Take`] applies
/// the declared label, naming the change. A `None` label is no claim
/// in either mode (the COALESCE rule of the upsert: a NULL never
/// erases an earlier label). Until 2026-09-18 the mint was the upsert
/// below, so the company's label was overwritten with `tenant.toml`'s
/// display name at every tenant publish — every boot.
///
/// An insert stages [`SUBJECT_MINTED`] and a take-mode relabel stages
/// [`SUBJECT_RELABELLED`], each in this transaction, signed with
/// `stamp`; a kept or unchanged declaration moves nothing and records
/// nothing (backlog 92473357).
pub async fn publish_subject(
    pool: &PgPool,
    kind: &str,
    id: &str,
    label: Option<&str>,
    mode: PublishMode,
    stamp: &EventStamp,
) -> Result<SubjectPublishOutcome, String> {
    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
    let held: Option<(Option<String>,)> =
        sqlx::query_as("SELECT label FROM subjects WHERE kind = $1 AND id = $2 FOR UPDATE")
            .bind(kind)
            .bind(id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
    let mut out = SubjectPublishOutcome {
        received: 1,
        ..Default::default()
    };
    match held {
        None => {
            sqlx::query("INSERT INTO subjects (kind, id, label) VALUES ($1, $2, $3)")
                .bind(kind)
                .bind(id)
                .bind(label)
                .execute(&mut *tx)
                .await
                .map_err(|e| e.to_string())?;
            boss_events::outbox::record_event_in_tx(&mut tx, &minted_event(stamp, kind, id, label))
                .await?;
            out.inserted = 1;
        }
        Some((current,)) => {
            let differs = label.filter(|l| current.as_deref() != Some(*l));
            if differs.is_none() {
                out.unchanged = 1;
            } else if let Some(new_label) = differs.filter(|_| mode.is_take()) {
                sqlx::query("UPDATE subjects SET label = $3 WHERE kind = $1 AND id = $2")
                    .bind(kind)
                    .bind(id)
                    .bind(new_label)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| e.to_string())?;
                boss_events::outbox::record_event_in_tx(
                    &mut tx,
                    &relabelled_event(stamp, kind, id, current.as_deref(), new_label),
                )
                .await?;
                out.updated.push(UpdatedRow {
                    id: id.to_string(),
                    changes: vec![FieldChange::new("label", &current, label)],
                });
            } else {
                out.kept.push(KeptRow {
                    id: id.to_string(),
                    differs: vec!["label".to_string()],
                });
            }
        }
    }
    tx.commit().await.map_err(|e| e.to_string())?;
    Ok(out)
}

/// One identity row as the list read answers it: the kind, the id and
/// the label the mint door landed (`None` when nothing ever labelled
/// it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubjectRow {
    pub kind: String,
    pub id: String,
    pub label: Option<String>,
}

/// Every identity row of `kind`, sorted by id, with its label — the
/// read `boss tenant export` writes the company's display name from
/// (design e187198f car 3, backlog e618f3ac). Until 2026-09-18 the
/// only read of this table was the per-id exists probe, so an export
/// could confirm a company it already knew and never learn its label.
pub async fn list_subjects(pool: &PgPool, kind: &str) -> Result<Vec<SubjectRow>, String> {
    let rows: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT id, label FROM subjects WHERE kind = $1 ORDER BY id")
            .bind(kind)
            .fetch_all(pool)
            .await
            .map_err(|e| e.to_string())?;
    Ok(rows
        .into_iter()
        .map(|(id, label)| SubjectRow {
            kind: kind.to_string(),
            id,
            label,
        })
        .collect())
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
/// the read-only kinds router. POST is the mint path (the company
/// identity at every tenant publish, operator tooling, R3's single
/// minting authority later) — insert-if-absent, `?mode=take` to
/// overwrite a held label ([`publish_subject`]); GET on a kind lists
/// its rows with their labels ([`list_subjects`]); GET on an id is the
/// cross-service existence probe.
pub fn subjects_router(pool: PgPool) -> Router {
    Router::new()
        .route("/api/subjects", post(post_subject))
        // Kind-scoped mint: the sim's birth event routes POST their
        // synthesized payload (id + label, no kind field) here. The
        // GET beside it is the export's read.
        .route(
            "/api/subjects/{kind}",
            get(list_subjects_of_kind).post(post_subject_for_kind),
        )
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
    Query(ModeQuery { mode }): Query<ModeQuery>,
    CurrentUser(user): CurrentUser,
    axum::Json(body): axum::Json<SubjectBody>,
) -> Response {
    if body.kind.trim().is_empty() || body.id.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "kind and id are required").into_response();
    }
    let stamp = mint_stamp(&user);
    mint(
        &state,
        &body.kind,
        &body.id,
        body.label.as_deref(),
        mode,
        &stamp,
    )
    .await
}

/// The stamp the mint door signs its fact with: the actor the request
/// signed with (`x-boss-user`, the id `boss tenant publish` sends); a
/// caller with no identity — the sim's birth routes — is this service's
/// own automation, never anonymous (the classes door's shape).
fn mint_stamp(user: &boss_policy_client::User) -> EventStamp {
    let actor = user
        .ambient_actor()
        .unwrap_or_else(|| boss_core::actor::ActorId::Automation("subjects".into()));
    EventStamp::new("subjects", actor)
}

/// The mint's answer: 201 with the outcome when the row was inserted,
/// 200 with it otherwise (kept, updated or unchanged — the body says
/// which), 422 for an unregistered kind.
async fn mint(
    state: &SubjectsApiState,
    kind: &str,
    id: &str,
    label: Option<&str>,
    mode: PublishMode,
    stamp: &EventStamp,
) -> Response {
    match publish_subject(&state.pool, kind, id, label, mode, stamp).await {
        Ok(out) if out.inserted == 1 => (StatusCode::CREATED, axum::Json(out)).into_response(),
        Ok(out) => axum::Json(out).into_response(),
        // The FK rejection = unregistered kind → the caller's error,
        // not ours.
        Err(e) if e.contains("subjects_kind_fkey") => (
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("unregistered subject kind `{kind}`"),
        )
            .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

async fn list_subjects_of_kind(
    State(state): State<SubjectsApiState>,
    Path(kind): Path<String>,
) -> Response {
    match list_subjects(&state.pool, &kind).await {
        Ok(rows) => axum::Json(rows).into_response(),
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
    Query(ModeQuery { mode }): Query<ModeQuery>,
    CurrentUser(user): CurrentUser,
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
    let stamp = mint_stamp(&user);
    mint(
        &state,
        &kind,
        &parsed.id,
        parsed.label.as_deref(),
        mode,
        &stamp,
    )
    .await
}

const IDENTITY_SOURCES_TOML: &str = include_str!("../seeds/subject_identity_sources.toml");

/// Held for the whole TRUNCATE-and-reproject so two rebuilds of the
/// identity table never interleave — the lock every `boss-rebuild-all`
/// step takes. This one took none until backlog 8d5ac7c5.
const REBUILD_LOCK_KEY: i64 = boss_core::rebuild::lock_key("subjects");

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
/// 4. the `companies` reference table — the tenant organization (Q6),
///    identical shape to locations: seed-only reference identity, so
///    it survives an epoch rollover's truncate-and-reproject (which a
///    prepare-only write-through does not).
/// 5. the mint door's own facts, [`SUBJECT_MINTED`] and
///    [`SUBJECT_RELABELLED`] — LAST, so a label the door set (a
///    take-mode relabel of the company included) wins over a reference
///    table's name, as it does in the live table.
pub async fn rebuild_subjects(pool: &PgPool) -> Result<u64, String> {
    let sources: SourcesToml = toml::from_str(IDENTITY_SOURCES_TOML).map_err(|e| e.to_string())?;
    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(REBUILD_LOCK_KEY)
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("taking the subjects rebuild lock: {e}"))?;
    // A write-through (`record_subject_in_tx`) commits beside its
    // caller's fact in event_outbox, and the mint door stages its own,
    // so the truncate may run only against a log that holds them all
    // (design b046f510).
    boss_events::outbox::lock_and_assert_log_complete(&mut tx, &["subjects"]).await?;
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

    // The jobs.job.created payload nests the subject:
    // `{"subject": {"id": ..., "subject_kind": ...}}` (boss-core's
    // Job serialization). Read the NESTED object — top-level
    // `subject_kind`/`subject_id` keys never existed, so the old path
    // silently homed nothing and every log-only rebuild dropped the
    // birth-by-job subjects (workflow, custom) that no TOML event
    // source carries (task #18).
    let res = sqlx::query(
        "INSERT INTO subjects (kind, id) \
         SELECT DISTINCT payload->'subject'->>'subject_kind', payload->'subject'->>'id' \
         FROM audit_log \
         WHERE kind = 'jobs.job.created' \
           AND payload->'subject'->>'subject_kind' IS NOT NULL \
           AND payload->'subject'->>'id' IS NOT NULL \
           AND EXISTS (SELECT 1 FROM subject_kinds k WHERE k.kind = payload->'subject'->>'subject_kind') \
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

    // Companies are tenant-organization reference rows (Q6), same
    // shape as locations: seed-only, no create events BY DESIGN, so
    // their identity derives from the `companies` reference table.
    // WITHOUT this pass, the company minted at prepare time is lost
    // the first time an epoch rollover reprojects subjects from the
    // trimmed log, and every org-level Job then fails the existence
    // gate (the 2026-07-17 rollover incident).
    let res = sqlx::query(
        "INSERT INTO subjects (kind, id, label) \
         SELECT 'company', id, name FROM companies \
         ON CONFLICT (kind, id) DO NOTHING",
    )
    .execute(&mut *tx)
    .await
    .map_err(|e| format!("companies reference pass: {e}"))?;
    total += res.rows_affected();

    // The mint door's facts (backlog 92473357). One row per (kind, id)
    // inside the statement — a mint and its relabels are several events
    // for one row, and ON CONFLICT cannot touch a row twice — carrying
    // the NEWEST non-null label (a mint with no label never erases one,
    // the COALESCE rule of the live upsert). A kind no longer registered
    // is skipped rather than aborting the whole rebuild on the FK, as
    // the job-subject pass does.
    let res = sqlx::query(
        "INSERT INTO subjects (kind, id, label) \
         SELECT ev.subject_kind, ev.subject_id, ev.label FROM ( \
             SELECT payload->>'subject_kind' AS subject_kind, \
                    payload->>'id' AS subject_id, \
                    (array_agg(payload->>'label' ORDER BY id DESC) \
                        FILTER (WHERE payload->>'label' IS NOT NULL))[1] AS label \
               FROM audit_log \
              WHERE kind IN ($1, $2) \
                AND payload->>'subject_kind' IS NOT NULL \
                AND payload->>'id' IS NOT NULL \
              GROUP BY 1, 2 \
         ) ev \
         WHERE EXISTS (SELECT 1 FROM subject_kinds k WHERE k.kind = ev.subject_kind) \
         ON CONFLICT (kind, id) DO UPDATE \
            SET label = COALESCE(EXCLUDED.label, subjects.label)",
    )
    .bind(SUBJECT_MINTED)
    .bind(SUBJECT_RELABELLED)
    .execute(&mut *tx)
    .await
    .map_err(|e| format!("mint door pass: {e}"))?;
    total += res.rows_affected();

    tx.commit().await.map_err(|e| e.to_string())?;
    Ok(total)
}
