//! HTTP surface for file references.
//!
//! Lives behind boss-content-api. Routes:
//! - POST   /api/files            — multipart upload (201 a new row; 200
//!   the live row this target already holds for the identical bytes,
//!   verified — `reuse_presented`, backlog e5490f2e)
//! - GET    /api/files            — list for a (target_kind, target_id)
//! - GET    /api/files/{id}       — download bytes
//! - DELETE /api/files/{id}       — soft-delete (event + row, bytes
//!   GC'd later by the Session 3 sweep)
//!
//! Per design Q4 (write-then-check): the upload streams + hashes the
//! body first, then policy-checks, then writes the row + emits the
//! event. On policy denial the just-written object is rolled back via
//! `FileStorage::delete`.
//!
//! Per design Q1 the bucket is deployment-scoped and configured at
//! startup; the handlers don't make per-request bucket decisions.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{DefaultBodyLimit, Multipart, Path, Query, State};
use axum::handler::Handler;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Files larger than this go through a signed-URL redirect on
/// download instead of streaming through the gateway.
pub const LARGE_DOWNLOAD_THRESHOLD_BYTES: i64 = 8 * 1024 * 1024;

/// The largest multipart body `POST /api/files` takes: 2,097,152 bytes
/// (2 MiB), the WHOLE body — the file plus its framing and the two
/// target fields — not the file alone.
///
/// MEASURED, NOT CHOSEN (backlog 7610dd2f, 2026-09-23). Nothing in the
/// content-api set a body limit, so the number in force was axum's
/// implicit default for the `Multipart` extractor: axum-core 0.5.6,
/// `with_limited_body`, `DEFAULT_LIMIT = 2_097_152`. An implicit number
/// can only be restated elsewhere by copying it, and `boss attach` has
/// to state it — in its help and in its refusal — so it is declared
/// here, applied to the route below, and read by the verb from this one
/// constant (CLAUDE.md §9a). Raising it is an edit here; the verb
/// follows.
pub const UPLOAD_BODY_LIMIT_BYTES: usize = 2_097_152;

/// TTL for a presigned GET URL on the large-file download path.
/// Short — the URL leaks scope by construction; the requesting user
/// has already passed policy at request time, so the URL is
/// effectively a 5-minute grant for one user's exact ask.
const SIGNED_GET_TTL: Duration = Duration::from_secs(5 * 60);

/// TTL for a presigned PUT URL on the large-file upload path.
/// Same shape: short window for one specific upload by one client.
const SIGNED_PUT_TTL: Duration = Duration::from_secs(15 * 60);

use boss_policy_client::{CurrentUser, PolicyClient};
use boss_policy_client::{Pair, controls};

use crate::files::error::FileError;
use crate::files::port::{FileRepository, FileStorage};
use crate::files::types::{FileRef, FileRefDraft, ResourceKind, ResourceRef};

/// Per-process state injected into every file-references handler.
pub struct FilesApiState {
    pub repo: Arc<dyn FileRepository>,
    pub storage: Arc<dyn FileStorage>,
    pub publisher: Option<boss_core::publisher::DomainPublisher>,
    pub policy: Arc<dyn PolicyClient>,
    /// Bucket every PUT lands in. Per design Q1 (one bucket per
    /// deployment) this is set once at startup; the handlers don't
    /// take a bucket per request.
    pub bucket: String,
    /// Optional pool for the audit endpoint. None = `/api/files/audit`
    /// returns 503 (lib has no Pg dep without the postgres feature on
    /// the binary). The bin always supplies this.
    #[cfg(feature = "postgres")]
    pub pool: Option<sqlx::PgPool>,
    /// Authoritative clock — every uploaded_at / soft-delete
    /// timestamp + every emitted audit event stamps via clock-api.
    pub clock: Arc<dyn boss_clock_client::ClockClient>,
}

pub fn router(state: FilesApiState) -> Router {
    let shared = Arc::new(state);
    let r = Router::new()
        .route(
            "/api/files",
            get(list).post(upload.layer(DefaultBodyLimit::max(UPLOAD_BODY_LIMIT_BYTES))),
        )
        .route("/api/files/{id}", get(download).delete(soft_delete))
        // Session 4 — large-file path. Both routes use `_`-prefixed
        // names so they can never collide with a Uuid that parses to
        // the same literal under /api/files/{id}.
        .route("/api/files/_upload-url", post(request_upload_url))
        .route("/api/files/_finalize", post(finalize_upload));
    // `_audit` underscore-prefix avoids any future "what if a Uuid
    // parses to the literal `audit`" question — the route lives
    // inside the /api/files namespace but can't collide with {id}.
    #[cfg(feature = "postgres")]
    let r = r.route("/api/files/_audit", get(audit));
    r.with_state(shared)
}

// ---- Policy mapping -------------------------------------------------------
//
// Files inherit from their target — read a Job, read its files; update
// a Job, attach files to it. Mapping ResourceKind to the declared
// control each asks (`boss_policy_client::controls`, backlog 47aed706):
//
//   Job   → READ_JOB / UPDATE_JOB
//   Step  → READ_STEP / UPDATE_STEP
//   Subject → READ_ACCOUNT / UPDATE_ACCOUNT (v1 simplification — Subjects are
//             generic; using the most-common kind unblocks the slice.
//             Session 3 can introduce a Subject-kind-aware mapping
//             once we wire the SPA onto Subject pages.)
//   Event → no policy check; gateway cookie auth gates access. Events
//           are append-only audit rows; their evidence is read-gated
//           by whoever already saw the event in the audit page.

/// What a file door does to its target: reads it, or writes to it
/// (attaching, detaching or replacing a file is an update of the target).
#[derive(Debug, Clone, Copy)]
enum Access {
    Read,
    Update,
}

fn target_control(kind: ResourceKind, access: Access) -> Option<Pair> {
    match (kind, access) {
        (ResourceKind::Job, Access::Read) => Some(controls::READ_JOB),
        (ResourceKind::Job, Access::Update) => Some(controls::UPDATE_JOB),
        (ResourceKind::Step, Access::Read) => Some(controls::READ_STEP),
        (ResourceKind::Step, Access::Update) => Some(controls::UPDATE_STEP),
        (ResourceKind::Subject, Access::Read) => Some(controls::READ_ACCOUNT),
        (ResourceKind::Subject, Access::Update) => Some(controls::UPDATE_ACCOUNT),
        (ResourceKind::Event, _) => None,
    }
}

async fn check_policy(
    state: &FilesApiState,
    user: &boss_policy_client::User,
    access: Access,
    target: &ResourceRef,
) -> Result<(), Response> {
    let Some(control) = target_control(target.kind, access) else {
        return Ok(()); // Event: no policy check
    };
    match state.policy.ask(user, control).await {
        Ok(boss_policy_client::Decision::Allow { .. }) => Ok(()),
        Ok(boss_policy_client::Decision::Deny { reason }) => {
            Err((StatusCode::FORBIDDEN, format!("policy denied: {reason}")).into_response())
        }
        // The one rendering every door gives: 503 + Retry-After and the
        // fixed word, never the client's text, which names the policy
        // service's internal URL (backlog fe9d212c).
        Err(e) => {
            tracing::warn!(error = %e, "files: policy check failed");
            Err(e.into_response())
        }
    }
}

fn err(e: FileError) -> Response {
    match e {
        FileError::NotFound(s) => (StatusCode::NOT_FOUND, s).into_response(),
        FileError::Validation(s) => (StatusCode::BAD_REQUEST, s).into_response(),
        FileError::DuplicateObject(s) => (StatusCode::CONFLICT, s).into_response(),
        FileError::Repository(s) => (StatusCode::INTERNAL_SERVER_ERROR, s).into_response(),
        FileError::Storage(s) => (StatusCode::INTERNAL_SERVER_ERROR, s).into_response(),
        FileError::Unsupported(s) => (StatusCode::NOT_IMPLEMENTED, s).into_response(),
    }
}

// ---- Handlers -------------------------------------------------------------

#[derive(Deserialize)]
struct ListQuery {
    target_kind: String,
    target_id: String,
}

async fn list(
    State(state): State<Arc<FilesApiState>>,
    CurrentUser(user): CurrentUser,
    Query(q): Query<ListQuery>,
) -> Response {
    let Some(kind) = ResourceKind::parse(&q.target_kind) else {
        return (
            StatusCode::BAD_REQUEST,
            format!("unknown target_kind: {}", q.target_kind),
        )
            .into_response();
    };
    let target = ResourceRef {
        kind,
        id: q.target_id,
    };
    if let Err(resp) = check_policy(&state, &user, Access::Read, &target).await {
        return resp;
    }
    match state.repo.list_for(&target).await {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => err(e),
    }
}

/// Multipart upload. Required fields: `target_kind`, `target_id`,
/// and one file part (any name; first non-text part wins). The file
/// part's `filename` + `content_type` headers populate the row.
///
/// Streaming + hashing in one pass: each chunk feeds both the SHA-256
/// hasher and an in-memory buffer. Object key is `sha256/<hash>`,
/// computed after the body is fully read; the bytes are PUT to that
/// key in one shot (v1 doesn't multipart-upload; that's Session 4).
async fn upload(
    State(state): State<Arc<FilesApiState>>,
    CurrentUser(user): CurrentUser,
    mut multipart: Multipart,
) -> Response {
    let mut target_kind: Option<String> = None;
    let mut target_id: Option<String> = None;
    let mut buffer: Option<Bytes> = None;
    let mut filename: Option<String> = None;
    let mut mime: Option<String> = None;
    let mut hasher = Sha256::new();

    // `e.status()`, not a flat 400: a body past UPLOAD_BODY_LIMIT_BYTES
    // is 413, and that status is how a caller tells "too large" from
    // "malformed" without parsing prose (backlog 7610dd2f).
    while let Some(field) = match multipart.next_field().await {
        Ok(f) => f,
        Err(e) => return (e.status(), format!("multipart: {e}")).into_response(),
    } {
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "target_kind" => {
                target_kind = field.text().await.ok();
            }
            "target_id" => {
                target_id = field.text().await.ok();
            }
            // Anything else is the file part.
            _ => {
                if buffer.is_some() {
                    return (StatusCode::BAD_REQUEST, "only one file part allowed").into_response();
                }
                filename = field.file_name().map(str::to_string);
                mime = field
                    .content_type()
                    .map(str::to_string)
                    .or(Some("application/octet-stream".to_string()));
                let bytes = match field.bytes().await {
                    Ok(b) => b,
                    Err(e) => {
                        return (e.status(), format!("body read: {e}")).into_response();
                    }
                };
                hasher.update(&bytes);
                buffer = Some(bytes);
            }
        }
    }

    let (Some(kind_str), Some(tid), Some(bytes), Some(fname), Some(mt)) =
        (target_kind, target_id, buffer, filename, mime)
    else {
        return (
            StatusCode::BAD_REQUEST,
            "missing target_kind, target_id, or file part".to_string(),
        )
            .into_response();
    };
    let Some(kind) = ResourceKind::parse(&kind_str) else {
        return (
            StatusCode::BAD_REQUEST,
            format!("unknown target_kind: {kind_str}"),
        )
            .into_response();
    };
    let target = ResourceRef { kind, id: tid };

    if let Err(resp) = check_policy(&state, &user, Access::Update, &target).await {
        return resp;
    }

    let sha256_bytes = hasher.finalize();
    let sha = hex::encode(sha256_bytes);
    let object_key = format!("sha256/{sha}");
    let size_bytes = bytes.len() as i64;

    if let Err(e) = state.storage.put(&object_key, bytes.clone(), &mt).await {
        return err(e);
    }

    // OUTBOX (phase 2) below records content.file.attached in the
    // same transaction as the row; `uploaded_at` takes the stamp's
    // wall instant so row, payload, and record stamp agree.
    let stamp = crate::events::event_stamp(&state.publisher).await;
    let now = stamp.timestamp;
    let id = Uuid::new_v4();
    let draft = FileRefDraft {
        id,
        target: target.clone(),
        bucket: state.bucket.clone(),
        object_key: object_key.clone(),
        sha256: sha,
        size_bytes,
        mime: mt,
        filename: fname,
        uploaded_by: user.id.clone(),
        uploaded_at: now,
    };

    match state.repo.insert(draft.clone(), &stamp).await {
        Ok(row) => (StatusCode::CREATED, Json(row)).into_response(),
        // The table holds one row per (bucket, object_key). This caller
        // presented the bytes and the door hashed them itself, so if the
        // row it collided with is a live attachment on the very target
        // it just passed Update for, it already holds that attachment:
        // answer the row instead of a conflict (backlog e5490f2e).
        Err(FileError::DuplicateObject(_)) => reuse_presented(&state, &user, &draft).await,
        Err(e) => {
            // Row insert failed after bytes wrote — a transient repo
            // error. The bytes are dedup-safe (same content => same
            // key) so we do NOT delete the object on this failure path;
            // another live ref might be using it. Just surface the error.
            err(e)
        }
    }
}

/// The answer to an upload whose bytes the store already holds as a row:
/// that row, 200 rather than 201, when the caller holds it already — else
/// the conflict, naming nothing but the digest the caller computed.
///
/// WHY (backlog e5490f2e, measured 2026-10-15). A second supported
/// dispatch of the same 1,010,152-byte brief to the same packet failed
/// before run admission at POST /api/files -> 409 <digest>: the table is
/// UNIQUE (bucket, object_key) and the door answered an identical upload
/// as a conflict, so a retry, or a dispatch whose first response was
/// lost, could never be admitted. A digest in a 409 is not a receipt, so
/// the callers could not safely make one of it.
///
/// THE BOUNDARY. A reuse is a reference to content the caller ALREADY
/// HOLDS: it presented every byte, which this door hashed itself — the
/// `_finalize` path, which takes a caller's hash at face value, never
/// reaches here. And it is answered only when the row is
///   - live (a detached row's download answers 410),
///   - on the SAME target this upload named and passed Update for — a row
///     elsewhere is under a scope this request's policy check never saw,
///     so neither its id nor its metadata is answered,
///   - of the presented size and digest, in this deployment's bucket,
///   - readable by this caller (Read on the target, as a download asks),
///   - and served back by storage as bytes that re-hash to the row.
///
/// It writes nothing and emits nothing — no state changed — so the row
/// keeps its original `uploaded_by` and its one `content.file.attached`
/// event: the record says who attached it, and when, once.
async fn reuse_presented(
    state: &FilesApiState,
    user: &boss_policy_client::User,
    presented: &FileRefDraft,
) -> Response {
    let no_reuse = || {
        (
            StatusCode::CONFLICT,
            format!(
                "duplicate object_key for sha256 {}: these bytes are already held by an \
                 attachment this upload may not reuse — a reuse answers only a live attachment \
                 on the target named, whose stored bytes verify",
                presented.sha256
            ),
        )
            .into_response()
    };
    let rows = match state.repo.list_for_sha256(&presented.sha256).await {
        Ok(rows) => rows,
        Err(e) => return err(e),
    };
    let Some(row) = rows.into_iter().find(|r| {
        r.deleted_at.is_none()
            && r.bucket == presented.bucket
            && r.object_key == presented.object_key
            && r.target == presented.target
            && r.sha256 == presented.sha256
            && r.size_bytes == presented.size_bytes
    }) else {
        return no_reuse();
    };
    if let Err(resp) = check_policy(state, user, Access::Read, &row.target).await {
        return resp;
    }
    match state.storage.get(&row.object_key).await {
        Ok(stored)
            if stored.len() as i64 == row.size_bytes
                && hex::encode(Sha256::digest(&stored)) == row.sha256 => {}
        Ok(_) | Err(FileError::NotFound(_)) => return no_reuse(),
        Err(e) => return err(e),
    }
    (StatusCode::OK, Json(row)).into_response()
}

async fn download(
    State(state): State<Arc<FilesApiState>>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
) -> Response {
    let row = match state.repo.get(id).await {
        Ok(Some(r)) => r,
        Ok(None) => return (StatusCode::NOT_FOUND, "file not found").into_response(),
        Err(e) => return err(e),
    };
    if row.deleted_at.is_some() {
        return (StatusCode::GONE, "file detached").into_response();
    }
    if let Err(resp) = check_policy(&state, &user, Access::Read, &row.target).await {
        return resp;
    }

    // Large files: if the storage backend can mint a presigned URL,
    // 302 to it so the bytes flow client → store directly. The
    // local-disk backend can't presign (returns Unsupported), so we
    // fall through to streaming the bytes through the content-api —
    // fine for a single-VM deployment with no separate object store.
    if row.size_bytes > LARGE_DOWNLOAD_THRESHOLD_BYTES {
        match state
            .storage
            .sign_get_url(&row.object_key, SIGNED_GET_TTL)
            .await
        {
            Ok(url) => return Redirect::temporary(&url).into_response(),
            // Backends without presigned URLs (local disk) stream the
            // bytes through the content-api instead — fall through to
            // the streaming path below.
            Err(FileError::Unsupported(_)) => {}
            Err(FileError::NotFound(_)) => {
                return (
                    StatusCode::GONE,
                    "bytes_missing — row exists but bytes were GC'd",
                )
                    .into_response();
            }
            Err(e) => return err(e),
        }
    }

    match state.storage.get(&row.object_key).await {
        Ok(bytes) => download_response(&row, bytes),
        Err(FileError::NotFound(_)) => (
            StatusCode::GONE,
            "bytes_missing — row exists but bytes were GC'd",
        )
            .into_response(),
        Err(e) => err(e),
    }
}

fn download_response(row: &FileRef, bytes: Bytes) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        row.mime
            .parse()
            .unwrap_or_else(|_| "application/octet-stream".parse().unwrap()),
    );
    // RFC-5987 filename* would be nicer for non-ASCII but `filename=`
    // is universal; stick to ASCII for v1 and let later sessions add
    // the * variant if a non-ASCII upload shows up.
    let disp = format!("attachment; filename=\"{}\"", row.filename.replace('"', ""));
    if let Ok(v) = disp.parse() {
        headers.insert(header::CONTENT_DISPOSITION, v);
    }
    (StatusCode::OK, headers, bytes).into_response()
}

// ---- Large-file upload path (presigned, optional backend) ----------------
//
// Two-phase: client asks for a presigned PUT URL, uploads bytes
// directly to the object store, then asks the service to finalize
// (insert row + emit event). This requires a storage backend that can
// mint presigned URLs. The default `LocalDiskStorage` cannot, so
// `sign_put_url` returns `Unsupported` and these endpoints are unused —
// the SPA uploads every size via the multipart `upload` handler. The
// endpoints are retained for a future object-store backend.
//
// State between phases is carried by the client — every metadata field
// the row needs gets sent on _both_ requests, and the finalize handler
// HEADs the bucket to verify the bytes actually landed before inserting.
// No server-side pending-upload table; that would require a new schema
// + GC sweep for abandoned uploads, which is overkill for v1.
//
// Trust model: the client-supplied sha256 is taken at face value
// (re-hashing >50 MiB server-side defeats the streaming-upload point).
// A wrong sha just means the bytes land at the wrong key; downloads
// 410 and the audit-sample sweep flags the divergence on the next run.

#[derive(Deserialize)]
struct UploadUrlRequest {
    target_kind: String,
    target_id: String,
    sha256: String,
    size_bytes: i64,
    mime: String,
    /// Carried for symmetry with the finalize body — the server does
    /// not key off of it during URL generation, but accepting it now
    /// means the SPA can send one canonical metadata object in both
    /// requests rather than a slim/full split. Deleting the field would
    /// silently relax the request contract (a body without `filename`
    /// is refused today); `expect` fails the build the day a handler
    /// starts reading it, which is the cue to drop this marker.
    #[expect(
        dead_code,
        reason = "required by the upload request contract, not read during URL generation"
    )]
    filename: String,
}

#[derive(Serialize)]
struct UploadUrlResponse {
    file_id: Uuid,
    upload_url: String,
    object_key: String,
    /// Seconds until `upload_url` expires. Client must complete the
    /// PUT before this elapses; a missed window is recoverable by
    /// re-requesting the URL with the same sha256.
    expires_in_secs: u64,
}

async fn request_upload_url(
    State(state): State<Arc<FilesApiState>>,
    CurrentUser(user): CurrentUser,
    Json(req): Json<UploadUrlRequest>,
) -> Response {
    let Some(kind) = ResourceKind::parse(&req.target_kind) else {
        return (
            StatusCode::BAD_REQUEST,
            format!("unknown target_kind: {}", req.target_kind),
        )
            .into_response();
    };
    if req.sha256.is_empty() || req.sha256.len() != 64 {
        return (StatusCode::BAD_REQUEST, "sha256 must be 64 hex chars").into_response();
    }
    if req.size_bytes <= 0 {
        return (StatusCode::BAD_REQUEST, "size_bytes must be positive").into_response();
    }
    let target = ResourceRef {
        kind,
        id: req.target_id,
    };
    if let Err(resp) = check_policy(&state, &user, Access::Update, &target).await {
        return resp;
    }
    let object_key = format!("sha256/{}", req.sha256);
    let upload_url = match state
        .storage
        .sign_put_url(&object_key, &req.mime, SIGNED_PUT_TTL)
        .await
    {
        Ok(u) => u,
        Err(e) => return err(e),
    };
    let _ = user; // policy passed; user id lands on the row at finalize
    let resp = UploadUrlResponse {
        file_id: Uuid::new_v4(),
        upload_url,
        object_key,
        expires_in_secs: SIGNED_PUT_TTL.as_secs(),
    };
    Json(resp).into_response()
}

#[derive(Deserialize)]
struct FinalizeRequest {
    file_id: Uuid,
    target_kind: String,
    target_id: String,
    sha256: String,
    size_bytes: i64,
    mime: String,
    filename: String,
}

async fn finalize_upload(
    State(state): State<Arc<FilesApiState>>,
    CurrentUser(user): CurrentUser,
    Json(req): Json<FinalizeRequest>,
) -> Response {
    let Some(kind) = ResourceKind::parse(&req.target_kind) else {
        return (
            StatusCode::BAD_REQUEST,
            format!("unknown target_kind: {}", req.target_kind),
        )
            .into_response();
    };
    let target = ResourceRef {
        kind,
        id: req.target_id,
    };
    if let Err(resp) = check_policy(&state, &user, Access::Update, &target).await {
        return resp;
    }
    let object_key = format!("sha256/{}", req.sha256);

    // Verify the bytes actually landed. HEAD is cheap; if it 404s
    // the client's PUT either failed or hasn't completed yet — let
    // them retry rather than insert a row that points at nothing.
    match state.storage.head(&object_key).await {
        Ok(actual_size) => {
            // Tolerance: S3's HEAD returns the stored size which
            // should match exactly. Reject mismatches so a
            // misbehaving client can't claim a different size than
            // the bytes it uploaded.
            if (actual_size as i64) != req.size_bytes {
                return (
                    StatusCode::CONFLICT,
                    format!(
                        "uploaded size {} does not match claimed size {}",
                        actual_size, req.size_bytes
                    ),
                )
                    .into_response();
            }
        }
        Err(FileError::NotFound(_)) => {
            return (
                StatusCode::CONFLICT,
                "bytes_not_found — finalize before completing the PUT",
            )
                .into_response();
        }
        Err(e) => return err(e),
    }

    // Same shape as the upload path: the stamp is the one instant.
    let stamp = crate::events::event_stamp(&state.publisher).await;
    let now = stamp.timestamp;
    let draft = FileRefDraft {
        id: req.file_id,
        target: target.clone(),
        bucket: state.bucket.clone(),
        object_key,
        sha256: req.sha256,
        size_bytes: req.size_bytes,
        mime: req.mime,
        filename: req.filename,
        uploaded_by: user.id.clone(),
        uploaded_at: now,
    };
    match state.repo.insert(draft, &stamp).await {
        Ok(row) => (StatusCode::CREATED, Json(row)).into_response(),
        Err(e) => err(e),
    }
}

/// Optional audit endpoint — re-fetches a sample of live refs and
/// re-hashes them to verify the sha256 chain. Operator-tier only;
/// boss-policy gates with PolicyRule:Read.
#[cfg(feature = "postgres")]
async fn audit(
    State(state): State<Arc<FilesApiState>>,
    CurrentUser(user): CurrentUser,
    Query(q): Query<AuditQuery>,
) -> Response {
    // Reuse the policy matrix: the audit view is platform-admin-shaped,
    // so ask READ_POLICY_RULE (the audit log surface uses the same gate
    // today). This door admits any scope; the rule-table reads admit only
    // `all`, which is why the const is declared so.
    match state.policy.ask(&user, controls::READ_POLICY_RULE).await {
        Ok(boss_policy_client::Decision::Allow { .. }) => {}
        Ok(boss_policy_client::Decision::Deny { reason }) => {
            return (StatusCode::FORBIDDEN, format!("policy denied: {reason}")).into_response();
        }
        Err(e) => {
            tracing::warn!(error = %e, "files audit: policy check failed");
            return e.into_response();
        }
    }
    let Some(pool) = state.pool.clone() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "audit endpoint requires postgres pool",
        )
            .into_response();
    };
    let sample_size = q.sample.unwrap_or(50).clamp(1, 500);
    match crate::files::rebuild::audit_sample(&pool, state.storage.clone(), sample_size).await {
        Ok(report) => Json(report).into_response(),
        Err(e) => err(e),
    }
}

#[cfg(feature = "postgres")]
#[derive(Deserialize)]
struct AuditQuery {
    sample: Option<i64>,
}

async fn soft_delete(
    State(state): State<Arc<FilesApiState>>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
) -> Response {
    let row = match state.repo.get(id).await {
        Ok(Some(r)) => r,
        Ok(None) => return (StatusCode::NOT_FOUND, "file not found").into_response(),
        Err(e) => return err(e),
    };
    if let Err(resp) = check_policy(&state, &user, Access::Update, &row.target).await {
        return resp;
    }
    // OUTBOX (phase 2): the adapter records content.file.detached in
    // the same transaction as the flip — and only on an actual flip,
    // so a repeat delete no longer publishes a duplicate event.
    let stamp = crate::events::event_stamp(&state.publisher).await;
    if let Err(e) = state
        .repo
        .soft_delete(id, stamp.timestamp, &user.id, &stamp)
        .await
    {
        return err(e);
    }
    StatusCode::NO_CONTENT.into_response()
}

#[cfg(test)]
mod tests {
    //! The upload door's body limit, measured against the router itself.
    //!
    //! Backlog 7610dd2f (2026-09-23): an actor attaching a file through
    //! `boss attach` is told the largest body this door takes BEFORE it
    //! sends one, and the number it is told must be the number the door
    //! enforces. So the limit is one constant, applied to the route and
    //! read by the verb, and these tests hold the route to it byte-exactly:
    //! a multipart body of exactly the limit is taken, and one byte more
    //! is refused with 413 — the status that names the fact — rather than
    //! the 400 the handler used to fold every multipart error into.
    use super::*;
    use crate::files::in_memory::{InMemoryFileRepository, InMemoryFileStorage};
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    const BOUNDARY: &str = "boss-attach-limit-test";

    fn app() -> Router {
        app_with(Arc::new(boss_policy_client::PermissivePolicyClient))
    }

    fn app_with(policy: Arc<dyn PolicyClient>) -> Router {
        router(FilesApiState {
            repo: Arc::new(InMemoryFileRepository::new()),
            storage: Arc::new(InMemoryFileStorage::new()),
            publisher: None,
            policy,
            bucket: "test".into(),
            #[cfg(feature = "postgres")]
            pool: None,
            clock: Arc::new(boss_clock_client::WallClockClient),
        })
    }

    /// Backlog fe9d212c: the files door answered a policy outage as 503
    /// "policy unreachable: <the client's error>" — the policy service's
    /// internal URL — with no Retry-After. It answers through
    /// `PolicyClientError`'s one rendering now. The real adapter against
    /// a port nothing listens on, so the outage is the production one.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_policy_outage_answers_503_with_retry_after_and_no_address() {
        // Held, not bound and released (review 465e723f F3).
        let dark = boss_testing::dark_port();
        let app = app_with(Arc::new(boss_policy_client::ReqwestPolicyClient::new(
            "content",
            dark.base.clone(),
        )));
        let resp = app
            .oneshot(
                Request::get("/api/files?target_kind=job&target_id=job-1")
                    .header(
                        "x-boss-user",
                        r#"{"id":"agent-test","role":"platform-admin","access_tier":"operator"}"#,
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            resp.headers()
                .get(header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok()),
            Some(
                boss_policy_client::POLICY_OUTAGE_RETRY_AFTER_SECS
                    .to_string()
                    .as_str()
            )
        );
        let body = http_body_util::BodyExt::collect(resp.into_body())
            .await
            .unwrap()
            .to_bytes();
        assert_eq!(String::from_utf8_lossy(&body), "policy-unreachable");
    }

    /// A multipart body of EXACTLY `total` bytes: the two text fields,
    /// then one file part padded so the whole body lands on `total`.
    fn body_of_length(total: usize) -> Vec<u8> {
        let head = format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"target_kind\"\r\n\r\njob\r\n\
             --{BOUNDARY}\r\nContent-Disposition: form-data; name=\"target_id\"\r\n\r\njob-1\r\n\
             --{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.bin\"\r\n\
             Content-Type: application/octet-stream\r\n\r\n"
        );
        let tail = format!("\r\n--{BOUNDARY}--\r\n");
        let fill = total - head.len() - tail.len();
        let mut out = head.into_bytes();
        out.extend(std::iter::repeat_n(b'x', fill));
        out.extend(tail.into_bytes());
        assert_eq!(out.len(), total);
        out
    }

    async fn post(total: usize) -> StatusCode {
        let req = Request::post("/api/files")
            .header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={BOUNDARY}"),
            )
            .header(
                "x-boss-user",
                r#"{"id":"agent-test","role":"platform-admin","access_tier":"operator"}"#,
            )
            .body(Body::from(body_of_length(total)))
            .unwrap();
        app().oneshot(req).await.unwrap().status()
    }

    #[tokio::test]
    async fn a_body_of_exactly_the_limit_is_taken() {
        assert_eq!(post(UPLOAD_BODY_LIMIT_BYTES).await, StatusCode::CREATED);
    }

    #[tokio::test]
    async fn one_byte_over_the_limit_is_refused_as_too_large() {
        assert_eq!(
            post(UPLOAD_BODY_LIMIT_BYTES + 1).await,
            StatusCode::PAYLOAD_TOO_LARGE
        );
    }

    /// The number is the one axum applied implicitly before it was
    /// declared (axum-core 0.5.6 `with_limited_body`, `DEFAULT_LIMIT`),
    /// so declaring it refused no upload that used to be taken.
    #[test]
    fn the_declared_limit_is_the_two_mebibytes_the_door_already_enforced() {
        assert_eq!(UPLOAD_BODY_LIMIT_BYTES, 2_097_152);
    }

    // ---- Reuse of a verified identical attachment (backlog e5490f2e) ----
    //
    // Measured 2026-10-15: a second supported dispatch of the same
    // 1,010,152-byte brief to the same packet failed BEFORE run admission
    // at POST /api/files -> 409 <digest>, because the table holds one row
    // per (bucket, object_key) and the door answered the second identical
    // upload as a conflict. The tests below hold the door to the boundary
    // the fix keeps: a reuse is answered only to a caller who PRESENTED
    // the identical bytes, to the same target it may update and read,
    // and only after the stored bytes re-hash to the row.

    use boss_policy_client::{Action, Decision, Predicate, Resource, Scope, User};

    fn user_header(id: &str) -> String {
        format!(r#"{{"id":"{id}","role":"platform-admin","access_tier":"operator"}}"#)
    }

    fn upload_body(target_id: &str, filename: &str, bytes: &[u8]) -> Vec<u8> {
        let mut out = format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"target_kind\"\r\n\r\njob\r\n\
             --{BOUNDARY}\r\nContent-Disposition: form-data; name=\"target_id\"\r\n\r\n{target_id}\r\n\
             --{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\n\
             Content-Type: text/plain\r\n\r\n"
        )
        .into_bytes();
        out.extend_from_slice(bytes);
        out.extend(format!("\r\n--{BOUNDARY}--\r\n").into_bytes());
        out
    }

    struct Store {
        repo: Arc<InMemoryFileRepository>,
        storage: Arc<dyn FileStorage>,
        app: Router,
    }

    fn store_with(storage: Arc<dyn FileStorage>, policy: Arc<dyn PolicyClient>) -> Store {
        let repo = Arc::new(InMemoryFileRepository::new());
        let app = router(FilesApiState {
            repo: repo.clone(),
            storage: storage.clone(),
            publisher: None,
            policy,
            bucket: "test".into(),
            #[cfg(feature = "postgres")]
            pool: None,
            clock: Arc::new(boss_clock_client::WallClockClient),
        });
        Store { repo, storage, app }
    }

    fn store() -> Store {
        store_with(
            Arc::new(InMemoryFileStorage::new()),
            Arc::new(boss_policy_client::PermissivePolicyClient),
        )
    }

    async fn send(app: &Router, req: Request<Body>) -> (StatusCode, String) {
        let resp = app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let body = http_body_util::BodyExt::collect(resp.into_body())
            .await
            .unwrap()
            .to_bytes();
        (status, String::from_utf8_lossy(&body).into_owned())
    }

    async fn upload_as(
        app: &Router,
        actor: &str,
        target_id: &str,
        filename: &str,
        bytes: &[u8],
    ) -> (StatusCode, String) {
        let req = Request::post("/api/files")
            .header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={BOUNDARY}"),
            )
            .header("x-boss-user", user_header(actor))
            .body(Body::from(upload_body(target_id, filename, bytes)))
            .unwrap();
        send(app, req).await
    }

    fn row_of(body: &str) -> FileRef {
        serde_json::from_str(body).unwrap_or_else(|e| panic!("not a row ({e}): {body}"))
    }

    fn digest(bytes: &[u8]) -> String {
        hex::encode(Sha256::digest(bytes))
    }

    const BRIEF: &[u8] = b"the complete dispatch brief, byte for byte\r\n\0";

    /// The measured case: the same bytes to the same packet a second
    /// time — a retried dispatch, or one whose first response was lost.
    /// The second answer is the FIRST row, 200 rather than 201, with the
    /// first uploader still on it: a reuse writes nothing, so nothing in
    /// the record is re-attributed to the second caller.
    #[tokio::test]
    async fn an_identical_upload_to_the_same_target_answers_the_live_row_it_already_has() {
        let s = store();
        let (first, body) = upload_as(&s.app, "agent-first", "job-1", "a.txt", BRIEF).await;
        assert_eq!(first, StatusCode::CREATED, "{body}");
        let original = row_of(&body);

        let (again, body) = upload_as(&s.app, "agent-second", "job-1", "b.txt", BRIEF).await;
        assert_eq!(again, StatusCode::OK, "a verified reuse, not a 409: {body}");
        let reused = row_of(&body);
        assert_eq!(reused, original, "the same row, attribution included");
        assert_eq!(reused.uploaded_by, "agent-first");

        let rows = s.repo.list_for_sha256(&digest(BRIEF)).await.unwrap();
        assert_eq!(rows.len(), 1, "one durable row");
    }

    /// The same bytes to ANOTHER target are not a reuse: the row the
    /// store holds is attached somewhere the caller did not name, and
    /// answering it would hand them a reference (and its metadata) under
    /// a scope their policy check never saw. Still the conflict, and the
    /// body names nothing but the digest the caller computed itself.
    #[tokio::test]
    async fn identical_bytes_to_another_target_get_no_reuse_and_learn_no_row() {
        let s = store();
        let (_, body) = upload_as(&s.app, "agent-a", "job-tenant-a", "a.txt", BRIEF).await;
        let original = row_of(&body);
        let (status, body) = upload_as(&s.app, "agent-b", "job-tenant-b", "a.txt", BRIEF).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(!body.contains(&original.id.to_string()), "{body}");
        assert!(
            !body.contains("job-tenant-a") && !body.contains("agent-a"),
            "{body}"
        );
        let b = ResourceRef {
            kind: ResourceKind::Job,
            id: "job-tenant-b".into(),
        };
        assert!(s.repo.list_for(&b).await.unwrap().is_empty());
    }

    /// `_finalize` takes the caller's sha256 at face value — it presents
    /// a HASH, never the bytes. A hash is not possession, so it earns no
    /// reuse even on the very target the original sits on, and the
    /// answer carries no row.
    #[tokio::test]
    async fn a_hash_alone_through_finalize_gets_no_reuse() {
        let s = store();
        let (_, body) = upload_as(&s.app, "agent-a", "job-1", "a.txt", BRIEF).await;
        let original = row_of(&body);
        for target in ["job-1", "job-other"] {
            let finalize = serde_json::json!({
                "file_id": Uuid::new_v4(),
                "target_kind": "job",
                "target_id": target,
                "sha256": digest(BRIEF),
                "size_bytes": BRIEF.len(),
                "mime": "text/plain",
                "filename": "claimed.txt",
            });
            let req = Request::post("/api/files/_finalize")
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-boss-user", user_header("agent-hash-only"))
                .body(Body::from(finalize.to_string()))
                .unwrap();
            let (status, body) = send(&s.app, req).await;
            assert_eq!(status, StatusCode::CONFLICT, "{target}: {body}");
            assert!(!body.contains(&original.id.to_string()), "{body}");
        }
        assert_eq!(
            s.repo.list_for_sha256(&digest(BRIEF)).await.unwrap().len(),
            1
        );
    }

    /// A detached attachment is not live: answering it would hand back a
    /// reference whose download answers 410.
    #[tokio::test]
    async fn a_detached_attachment_is_not_reused() {
        let s = store();
        let (_, body) = upload_as(&s.app, "agent-a", "job-1", "a.txt", BRIEF).await;
        let original = row_of(&body);
        let del = Request::delete(format!("/api/files/{}", original.id))
            .header("x-boss-user", user_header("agent-a"))
            .body(Body::empty())
            .unwrap();
        assert_eq!(send(&s.app, del).await.0, StatusCode::NO_CONTENT);
        let (status, body) = upload_as(&s.app, "agent-a", "job-1", "a.txt", BRIEF).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(!body.contains(&original.id.to_string()), "{body}");
    }

    /// Allows every Update and denies every Read: a caller who may attach
    /// to a target but not read it.
    struct WriteOnly;

    #[async_trait::async_trait]
    impl PolicyClient for WriteOnly {
        async fn check(
            &self,
            _user: &User,
            action: Action,
            _resource: Resource,
        ) -> Result<Decision, boss_policy_client::PolicyClientError> {
            Ok(match action {
                Action::Read => Decision::Deny {
                    reason: "write-only test caller".into(),
                },
                _ => Decision::Allow { scope: Scope::All },
            })
        }

        async fn scope_predicate(
            &self,
            _user: &User,
            _resource: Resource,
        ) -> Result<Predicate, boss_policy_client::PolicyClientError> {
            Ok(Predicate::None)
        }
    }

    /// A reuse hands the caller a reference it will READ, so it asks Read
    /// on the row's target, as a download would — a caller who may only
    /// write gets the refusal, never the row.
    #[tokio::test]
    async fn a_reuse_asks_read_on_the_target_and_a_denial_answers_no_row() {
        let s = store_with(Arc::new(InMemoryFileStorage::new()), Arc::new(WriteOnly));
        let (first, body) = upload_as(&s.app, "agent-a", "job-1", "a.txt", BRIEF).await;
        assert_eq!(first, StatusCode::CREATED, "{body}");
        let original = row_of(&body);
        let (status, body) = upload_as(&s.app, "agent-a", "job-1", "a.txt", BRIEF).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert!(!body.contains(&original.id.to_string()), "{body}");
    }

    /// Concurrent identical uploads to one target: one row is created and
    /// every other caller is answered that same row.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_identical_uploads_answer_one_durable_row() {
        let s = store();
        let calls = (0..8).map(|i| {
            let app = s.app.clone();
            tokio::spawn(async move {
                upload_as(&app, &format!("agent-{i}"), "job-1", "a.txt", BRIEF).await
            })
        });
        let mut created = 0;
        let mut ids = std::collections::BTreeSet::new();
        for call in calls.collect::<Vec<_>>() {
            let (status, body) = call.await.unwrap();
            match status {
                StatusCode::CREATED => created += 1,
                StatusCode::OK => {}
                other => panic!("{other}: {body}"),
            }
            ids.insert(row_of(&body).id);
        }
        assert_eq!(created, 1);
        assert_eq!(ids.len(), 1, "every caller holds the one row");
        assert_eq!(
            s.repo.list_for_sha256(&digest(BRIEF)).await.unwrap().len(),
            1
        );
    }

    /// Bytes the store serves that no longer hash to the row: the row is
    /// a claim, and a claim the bytes contradict is not reused.
    struct Tampered(InMemoryFileStorage);

    #[async_trait::async_trait]
    impl FileStorage for Tampered {
        async fn put(&self, key: &str, bytes: Bytes, mime: &str) -> Result<(), FileError> {
            self.0.put(key, bytes, mime).await
        }
        async fn get(&self, _key: &str) -> Result<Bytes, FileError> {
            Ok(Bytes::from_static(b"other bytes"))
        }
        async fn delete(&self, key: &str) -> Result<(), FileError> {
            self.0.delete(key).await
        }
        async fn sign_get_url(&self, key: &str, ttl: Duration) -> Result<String, FileError> {
            self.0.sign_get_url(key, ttl).await
        }
        async fn sign_put_url(
            &self,
            key: &str,
            mime: &str,
            ttl: Duration,
        ) -> Result<String, FileError> {
            self.0.sign_put_url(key, mime, ttl).await
        }
        async fn head(&self, key: &str) -> Result<u64, FileError> {
            self.0.head(key).await
        }
    }

    #[tokio::test]
    async fn a_stored_object_that_does_not_re_hash_to_its_row_is_not_reused() {
        let s = store_with(
            Arc::new(Tampered(InMemoryFileStorage::new())),
            Arc::new(boss_policy_client::PermissivePolicyClient),
        );
        let (first, body) = upload_as(&s.app, "agent-a", "job-1", "a.txt", BRIEF).await;
        assert_eq!(first, StatusCode::CREATED, "{body}");
        let original = row_of(&body);
        let (status, body) = upload_as(&s.app, "agent-a", "job-1", "a.txt", BRIEF).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(!body.contains(&original.id.to_string()), "{body}");
        assert!(s.storage.head(&original.object_key).await.is_ok());
    }
}
