//! HTTP API for the Class registry. Reads are open. Three writes, each
//! asking policy for its action on `class` (backlog 553cf479): `POST
//! /api/classes/batch` seeds the registry via the public API
//! (insert-if-absent; a held row that differs from its declaration is
//! named in the answer as `kept: [{id, differs}]` — design e187198f)
//! (replacing the direct `psql -f classes.sql` end-around), `PUT
//! /api/classes/{subject_kind}/{code}` edits a row, and `POST
//! …/retire` withdraws one. A fourth, `POST …/backfill-declared`, writes
//! no row: it records the birth a Class declared before the batch door
//! staged facts was never given (backlog 9d345f9b) — and, given a body
//! `{born, changed, source}`, the birth and the fact-less edit of a row changed
//! outside the doors (backlog 93f361af), or, given `{"observed": true,
//! "source"}`, the row observed as it stands (backlog 6c2aa86c).
//! `GET /api/classes/births` and `…/{code}/birth` read, without writing,
//! what the edit and retire doors would do to each Class.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use boss_core::primitives::{Class, ClassRef};
use boss_core::publish::KeptRow;
use boss_policy_client::{CurrentUser, Pair, PolicyClient, controls};
use serde::Deserialize;
use serde_json::Value;

use crate::port::{Backfill, ClassRepository, NamedBirth, class_differs};

#[derive(Clone)]
pub struct ClassesApiState {
    pub classes: Arc<dyn ClassRepository>,
    /// Asked by the three write doors for Create / Update / Retire on
    /// `class` (backlog 553cf479). Reads ask no one.
    pub policy: Arc<dyn PolicyClient>,
}

pub fn router(state: ClassesApiState) -> Router {
    Router::new()
        .route("/api/classes/health", get(health))
        .route("/api/classes", get(list_classes))
        .route("/api/classes/batch", post(batch_upsert))
        .route(
            "/api/classes/{subject_kind}/{code}",
            get(get_class).put(update_class),
        )
        .route(
            "/api/classes/{subject_kind}/{code}/exists",
            get(class_exists),
        )
        .route(
            "/api/classes/{subject_kind}/{code}/retire",
            axum::routing::post(retire_class),
        )
        .route(
            "/api/classes/{subject_kind}/{code}/backfill-declared",
            post(backfill_declared),
        )
        .route("/api/classes/births", get(birth_plans))
        .route("/api/classes/{subject_kind}/{code}/birth", get(birth_plan))
        .with_state(state)
}

#[derive(Deserialize)]
struct BirthsQuery {
    subject_kind: Option<String>,
}

/// `GET /api/classes/births[?subject_kind=]` — what the edit and retire
/// doors would do to each Class, retired ones included, read without
/// writing: its stamps, how many facts the log holds, and `door`
/// (`nothing` / `declare` / `refuse`) with `why` (review of backlog
/// 6c2aa86c's car: drift must be measurable before an edit meets it).
/// A read, so it asks no one.
async fn birth_plans(
    State(state): State<ClassesApiState>,
    Query(q): Query<BirthsQuery>,
) -> Response {
    match state.classes.birth_plans(q.subject_kind.as_deref()).await {
        Ok(plans) => Json(plans).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// `GET /api/classes/{subject_kind}/{code}/birth` — the one Class's
/// [`birth_plans`] row; 404 for no row.
async fn birth_plan(
    State(state): State<ClassesApiState>,
    Path((subject_kind, code)): Path<(String, String)>,
) -> Response {
    match state.classes.birth_plans(Some(&subject_kind)).await {
        Ok(plans) => match plans.into_iter().find(|p| p.code == code) {
            Some(plan) => Json(plan).into_response(),
            None => (StatusCode::NOT_FOUND, "no such class").into_response(),
        },
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[cfg(feature = "postgres")]
const STORAGE: &str = "postgres";
#[cfg(not(feature = "postgres"))]
const STORAGE: &str = "in-memory";

/// Standard health probe — every boss-*-api binary exposes one
/// at `/api/<service>/health`. The SPA's MonitoringPage polls
/// this on every page load; a missing endpoint surfaces as 404
/// console spam.
async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "service": "boss-classes-api",
        "storage": STORAGE,
    }))
}

#[derive(Deserialize)]
struct ListQuery {
    subject_kind: String,
    /// Narrows the list to one axis of the kind. One subject_kind can
    /// hold several taxonomies told apart only by `member_attribute` —
    /// the employee drawer held role, department, status and
    /// employment_type side by side on 2026-09-23 (backlog ab1e6ff8) —
    /// so a reader after one column's values asks for that axis.
    member_attribute: Option<String>,
}

async fn list_classes(
    State(state): State<ClassesApiState>,
    Query(q): Query<ListQuery>,
) -> Response {
    match state.classes.list_for_subject_kind(&q.subject_kind).await {
        Ok(rows) => Json(
            rows.into_iter()
                .filter(|c| {
                    q.member_attribute.is_none()
                        || c.member_attribute.as_deref() == q.member_attribute.as_deref()
                })
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn get_class(
    State(state): State<ClassesApiState>,
    Path((subject_kind, code)): Path<(String, String)>,
) -> Response {
    let class_ref = ClassRef::new(subject_kind, code);
    match state.classes.get(&class_ref).await {
        Ok(Some(c)) => Json(c).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "no such class").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// `PUT /api/classes/{subject_kind}/{code}` — edit an existing Class.
///
/// `batch_upsert` is insert-if-absent by design, so that a re-run of a
/// seed cannot clobber operator edits. The consequence nobody noticed
/// was that once a Class was seeded, NOTHING could change it: the
/// registry CLAUDE.md §9 calls tenant-editable data was write-once.
/// This is the edit path.
///
/// The composite key is taken from the URL and never from the body, so
/// a rename is impossible here — a code is an identity other rows point
/// at (`employees.role`), and rewriting it in place would orphan them
/// silently. Renaming is a retire-and-create, deliberately louder.
///
/// An edit of a Class the log does not declare records its birth first,
/// in the same transaction; one whose row was changed outside the doors
/// is 409, naming `POST …/backfill-declared` (backlog 6c2aa86c).
async fn update_class(
    State(state): State<ClassesApiState>,
    CurrentUser(user): CurrentUser,
    Path((subject_kind, code)): Path<(String, String)>,
    Json(body): Json<ClassInput>,
) -> Response {
    let stamp = match authorize(&state, &user, controls::UPDATE_CLASS).await {
        Ok(stamp) => stamp,
        Err(refusal) => return refusal,
    };

    let mut class: Class = body.into();
    // URL wins. A body that disagrees is a caller error, not an
    // instruction to move the row.
    class.subject_kind = subject_kind;
    class.code = code;

    // The edit's `class.updated` names the caller (backlog 10dabe13):
    // `boss tenant publish --take classes` edits through this door.
    match state.classes.update(&class, &stamp).await {
        Ok(true) => Json(class).into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, "no such class").into_response(),
        Err(e) => door_refusal(e),
    }
}

/// How the edit and retire doors answer an error. Drift is 409: a Class
/// the log does not declare, whose row was changed outside the doors, so
/// no fact may land on it until its birth is recorded (backlog 6c2aa86c)
/// — the message names the door that records it. Anything else is 500.
fn door_refusal(e: crate::port::ClassError) -> Response {
    match e {
        e @ crate::port::ClassError::Drift(_) => {
            (StatusCode::CONFLICT, e.to_string()).into_response()
        }
        e => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// Withdraw a Class from active use. POST, not DELETE: the row stays
/// (other rows point at the code), and what happens is a state
/// transition — `retired_at` gets stamped, `list` and `exists_active`
/// stop offering the code. Idempotent; 404 only when the composite
/// key names nothing; the birth first, or 409, as the edit door.
async fn retire_class(
    State(state): State<ClassesApiState>,
    CurrentUser(user): CurrentUser,
    Path((subject_kind, code)): Path<(String, String)>,
) -> Response {
    let stamp = match authorize(&state, &user, controls::RETIRE_CLASS).await {
        Ok(stamp) => stamp,
        Err(refusal) => return refusal,
    };
    let class_ref = ClassRef::new(subject_kind, code);
    match state.classes.retire(&class_ref, &stamp).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, "no such class").into_response(),
        Err(e) => door_refusal(e),
    }
}

/// Record the `class.declared` a Class was born without (backlog
/// 9d345f9b) — the batch door staged no fact before 2026-09-17, so a
/// Class declared through it then, and not seeded by a migration, is
/// missing on a fresh database and its logged retirement replays as
/// drift. Create on `class`, as the batch door asks: a backfill declares.
/// 201 with the row as it was born, 200 `already_declared` when the log
/// already declares it (a repeat is a no-op), 404 for no row, 409 when
/// the row's own log does not explain it — never a birth that would
/// rebuild some other row. Writes no row, only the fact.
///
/// With a body, `{born, changed, source}` ([`NamedBirthBody`], backlog
/// 93f361af), it records a birth the log cannot reconstruct: for a Class
/// with no fact whose row was changed outside the doors, the
/// `class.declared` of `born` AND the `class.updated` that took it to
/// the live row, both marked as backfills naming `source` and carrying
/// the row's own `born_at` / `edited_at` — 201 with `backfilled`,
/// `edited` and `backfill_source`. 409 unless that one edit, changing
/// exactly the fields `changed` names, explains the row
/// ([`crate::port::backfill_edit`]). Policy: Create, and Update as well,
/// since it records an edit. The body is read before policy is asked, as
/// a `Json` extractor would: one that does not parse, lacks `changed`,
/// names a key it does not know, or leaves `source` blank is 422.
///
/// With a body `{"observed": true, "source": "<why>"}` ([`ObservedBirthBody`],
/// review of backlog 6c2aa86c's car) it records an OBSERVED birth
/// ([`crate::port::observed_birth`]): the row as it stands with its logged
/// facts un-applied, marked `birth: "observed"` with the row's stamps and
/// the drift they show — for a Class whose stamps refuse every other
/// birth, and only as the operator's act. 201 with `backfilled`, `birth`
/// and `backfill_source`. Policy: Create. `observed` other than true, a
/// blank `source` or an unknown key is 422.
async fn backfill_declared(
    State(state): State<ClassesApiState>,
    CurrentUser(user): CurrentUser,
    Path((subject_kind, code)): Path<(String, String)>,
    body: axum::body::Bytes,
) -> Response {
    let asked = match backfill_body(&body) {
        Ok(asked) => asked,
        Err(why) => return (StatusCode::UNPROCESSABLE_ENTITY, why).into_response(),
    };
    let stamp = match authorize(&state, &user, controls::CREATE_CLASS).await {
        Ok(stamp) => stamp,
        Err(refusal) => return refusal,
    };
    // A named birth records an edit as well as a birth, so it asks for
    // both (review of car 8778f12f, finding 4): a Create-only grant must
    // not be a way to put a `class.updated` in the log.
    if matches!(asked, Asked::Named(_))
        && let Err(refusal) = authorize(&state, &user, controls::UPDATE_CLASS).await
    {
        return refusal;
    }
    let class_ref = ClassRef::new(subject_kind, code);
    let done = match &asked {
        Asked::Bodiless => state.classes.backfill_declared(&class_ref, &stamp).await,
        Asked::Named(named) => {
            state
                .classes
                .backfill_edited(&class_ref, named, &stamp)
                .await
        }
        Asked::Observed(source) => {
            state
                .classes
                .backfill_observed(&class_ref, source, &stamp)
                .await
        }
    };
    match done {
        Ok(Backfill::Recorded(born)) => (
            StatusCode::CREATED,
            Json(serde_json::json!({ "backfilled": born })),
        )
            .into_response(),
        Ok(Backfill::RecordedWithEdit { born, change }) => (
            StatusCode::CREATED,
            Json(serde_json::json!({
                "backfilled": born,
                "edited": change,
                "backfill_source": match &asked {
                    Asked::Named(n) => Some(n.source.trim().to_string()),
                    _ => None,
                },
            })),
        )
            .into_response(),
        Ok(Backfill::Observed(born)) => (
            StatusCode::CREATED,
            Json(serde_json::json!({
                "backfilled": born,
                "birth": crate::port::OBSERVED,
                "backfill_source": match &asked {
                    Asked::Observed(source) => Some(source.trim().to_string()),
                    _ => None,
                },
            })),
        )
            .into_response(),
        Ok(Backfill::AlreadyDeclared) => {
            Json(serde_json::json!({ "already_declared": true })).into_response()
        }
        Ok(Backfill::NotFound) => (StatusCode::NOT_FOUND, "no such class").into_response(),
        Err(e @ crate::port::ClassError::Drift(_)) => {
            (StatusCode::CONFLICT, e.to_string()).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn class_exists(
    State(state): State<ClassesApiState>,
    Path((subject_kind, code)): Path<(String, String)>,
) -> Response {
    let class_ref = ClassRef::new(subject_kind, code);
    match state.classes.exists_active(&class_ref).await {
        Ok(b) => Json(serde_json::json!({ "exists": b })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// One row in a `POST /api/classes/batch` body. Mirrors the `classes`
/// table's authorable columns; `retired_at` / `created_at` /
/// `updated_at` are owned by the table and not accepted here (seeded
/// rows arrive active; withdrawing one is its own action — POST
/// `{subject_kind}/{code}/retire`). Optional fields default so a
/// minimal seed row is `{"subject_kind","code","display_name"}`.
///
/// Public since fcc1d57b: `boss tenant check` parses a tenant's
/// `seeds/classes.json` / `classes.toml` rows with THIS type, so the
/// check and the batch endpoint cannot disagree about a row.
#[derive(Debug, Deserialize)]
pub struct ClassInput {
    pub subject_kind: String,
    pub code: String,
    pub display_name: String,
    #[serde(default)]
    pub parent_code: Option<String>,
    #[serde(default)]
    pub member_attribute: Option<String>,
    #[serde(default = "empty_object")]
    pub metadata: Value,
    #[serde(default)]
    pub sort_order: i32,
}

/// The optional body of `POST …/backfill-declared` (backlog 93f361af):
/// the body the Class was born with, the fields the caller says the edit
/// outside the doors `changed` (required, and held to what `born`
/// actually differs in — review of car 8778f12f, finding 3), and where
/// that body is recorded — the log cannot say, so the caller names the
/// record that does (for the LLC's invoice Classes, the tenant commit
/// before 28a7ac1). The key is the URL's; a `born` naming another is
/// refused as drift.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NamedBirthBody {
    born: ClassInput,
    changed: Vec<String>,
    source: String,
}

/// The observed-birth body of `POST …/backfill-declared` (review of
/// backlog 6c2aa86c's car): `observed` must be `true` — the caller says in
/// so many words that the birth is observed, not proved — and `source`
/// says why.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ObservedBirthBody {
    observed: bool,
    source: String,
}

/// Which backfill a `POST …/backfill-declared` body asks for.
enum Asked {
    Bodiless,
    Named(NamedBirth),
    Observed(String),
}

/// Read a backfill body: empty is the bodiless backfill, one carrying
/// `observed` the observed birth, anything else the named birth. `Err`
/// is the 422's words.
fn backfill_body(body: &[u8]) -> Result<Asked, String> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(Asked::Bodiless);
    }
    let value: Value = serde_json::from_slice(body)
        .map_err(|e| format!("the body is {{born, changed, source}}: {e}"))?;
    if value.get("observed").is_some() {
        let o: ObservedBirthBody = serde_json::from_value(value)
            .map_err(|e| format!("the body is {{\"observed\": true, \"source\"}}: {e}"))?;
        if !o.observed {
            return Err("an observed birth says so: `observed` is true".into());
        }
        if o.source.trim().is_empty() {
            return Err(
                "an observed birth names its source — why the row is recorded as it stands".into(),
            );
        }
        return Ok(Asked::Observed(o.source));
    }
    let n: NamedBirthBody = serde_json::from_value(value)
        .map_err(|e| format!("the body is {{born, changed, source}}: {e}"))?;
    if n.source.trim().is_empty() {
        return Err(
            "a named birth names its source — where the body it asserts is recorded".into(),
        );
    }
    Ok(Asked::Named(NamedBirth {
        born: Class::from(n.born),
        changed: n.changed,
        source: n.source,
    }))
}

fn empty_object() -> Value {
    serde_json::json!({})
}

impl From<ClassInput> for Class {
    fn from(i: ClassInput) -> Self {
        Class {
            subject_kind: i.subject_kind,
            code: i.code,
            display_name: i.display_name,
            parent_code: i.parent_code,
            member_attribute: i.member_attribute,
            metadata: i.metadata,
            sort_order: i.sort_order,
            retired_at: None,
        }
    }
}

/// Every write door's policy question and the stamp its fact is signed
/// with, in one function so the three doors cannot be authorised or
/// signed three ways.
///
/// WHY POLICY (backlog 553cf479, 2026-09-28). The doors checked the
/// caller's access tier (`Operator`, or a sim caller on a sim instance)
/// and never asked policy, so a rule granting a taxonomy editor had no
/// effect and a platform-admin could not be refused by one. Now each
/// asks for its action on `class` — Create for the batch, Update for
/// the edit, Retire for the retirement; platform-admin's alone in the
/// core defaults — through the registry-write ladder
/// ([`boss_policy_client::writes::require_registry_write`]). Order: a
/// body that does not parse is refused 400/422 by the `Json` extractor
/// before policy is asked (nothing is written); then no caller 401, a
/// deny or a grant narrower than `all` 403, a policy service that
/// cannot answer 503; and only then the door's own 404/422. The sim is
/// admitted the way every policy-asking service admits it: the binary
/// wraps its client in `SimBypassPolicyClient::from_env`, which exists
/// only on a sim instance and admits only a sim caller there (85e7f10f).
///
/// The stamp names the caller the request was signed as — the actor the
/// ladder resolved, never a fallback. Publisher-less (the credentials
/// door's shape): the adapter stages the event on the outbox inside the
/// write's transaction and the relay moves it on, so this service needs
/// no bus of its own.
async fn authorize(
    state: &ClassesApiState,
    user: &boss_policy_client::User,
    control: Pair,
) -> Result<boss_core::publisher::EventStamp, Response> {
    let actor =
        boss_policy_client::writes::require_registry_write(state.policy.as_ref(), user, control)
            .await?;
    Ok(boss_core::publisher::EventStamp::new("classes", actor))
}

/// Batch-upsert Class rows — the single write surface, used to seed
/// the registry from JSON instead of `psql -f classes.sql`. Each row
/// inserts `ON CONFLICT (subject_kind, code) DO NOTHING`, so the call
/// is idempotent. Create on `class` ([`authorize`]); every seed path
/// signs as platform-admin. Reads stay open; only the writes are
/// privileged.
async fn batch_upsert(
    State(state): State<ClassesApiState>,
    CurrentUser(user): CurrentUser,
    Json(rows): Json<Vec<ClassInput>>,
) -> Response {
    let stamp = match authorize(&state, &user, controls::CREATE_CLASS).await {
        Ok(stamp) => stamp,
        Err(refusal) => return refusal,
    };

    let classes: Vec<Class> = rows.into_iter().map(Into::into).collect();
    // What the registry already holds of this batch, read BEFORE the
    // insert so a kept row's disagreement is named (design e187198f:
    // the instance is the truth, and a repo row that does not land is
    // named, never silent). Insert-if-absent never touches these, so
    // the read is the whole comparison.
    let (mut kept, mut unchanged) = (Vec::new(), 0usize);
    for c in &classes {
        let class_ref = ClassRef::new(c.subject_kind.as_str(), c.code.as_str());
        match state.classes.get(&class_ref).await {
            Ok(Some(held)) => {
                let differs = class_differs(&held, c);
                if differs.is_empty() {
                    unchanged += 1;
                } else {
                    kept.push(KeptRow {
                        id: format!("{}/{}", c.subject_kind, c.code),
                        differs,
                    });
                }
            }
            Ok(None) => {}
            Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        }
    }
    // The fact each inserted row leaves is stamped with the actor the
    // request signed with ([`authorize`]).
    match state.classes.batch_upsert(&classes, &stamp).await {
        Ok(inserted) => Json(serde_json::json!({
            "received": classes.len(),
            "inserted": inserted,
            "kept": kept,
            "unchanged": unchanged,
        }))
        .into_response(),
        // A class for an unregistered kind is a caller error, not a
        // storage failure — 422 with the offending kind named.
        Err(e @ crate::port::ClassError::UnregisteredKind(_)) => {
            (StatusCode::UNPROCESSABLE_ENTITY, e.to_string()).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::in_memory::InMemoryClasses;
    use axum::body::to_bytes;
    use axum::http::Request;
    use boss_core::primitives::Class;
    use boss_policy_client::{Action, Resource, Scope};
    use serde_json::{Value, json};
    use tower::ServiceExt;

    fn employee(code: &str, sort: i32) -> Class {
        Class {
            subject_kind: "employee".into(),
            code: code.into(),
            display_name: code.to_uppercase(),
            parent_code: None,
            member_attribute: Some("role".into()),
            metadata: json!({}),
            sort_order: sort,
            retired_at: None,
        }
    }

    fn build_app(rows: Vec<Class>) -> Router {
        door(Arc::new(InMemoryClasses::new(rows)), default_policy())
    }

    /// The router over `repo`, asking `policy`.
    fn door(repo: Arc<InMemoryClasses>, policy: Arc<dyn PolicyClient>) -> Router {
        router(ClassesApiState {
            classes: repo,
            policy,
        })
    }

    /// The core default rules, as the live policy service seeds them —
    /// so these tests judge the doors against the grant that ships.
    fn default_policy() -> Arc<dyn PolicyClient> {
        Arc::new(
            boss_policy_client::FakePolicyClient::builder()
                .with_default_rules()
                .build(),
        )
    }

    /// A policy service that cannot be asked.
    struct DarkPolicy;

    #[async_trait::async_trait]
    impl PolicyClient for DarkPolicy {
        async fn check(
            &self,
            _: &boss_policy_client::User,
            _: Action,
            _: Resource,
        ) -> Result<boss_policy_client::Decision, boss_policy_client::PolicyClientError> {
            Err(boss_policy_client::PolicyClientError::Unreachable(
                "dark".into(),
            ))
        }
        async fn scope_predicate(
            &self,
            _: &boss_policy_client::User,
            _: Resource,
        ) -> Result<boss_policy_client::Predicate, boss_policy_client::PolicyClientError> {
            Err(boss_policy_client::PolicyClientError::Unreachable(
                "dark".into(),
            ))
        }
    }

    /// `x-boss-user` JSON for a caller of the given role and tier.
    fn signed(id: &str, role: &str, tier: &str) -> String {
        json!({
            "id": id,
            "role": role,
            "access_tier": tier,
            "territory_account_ids": [],
            "direct_report_ids": [],
        })
        .to_string()
    }

    /// The three write doors, each as a request against the seeded
    /// `employee/platform-admin` row (the batch declares a new code).
    fn the_three_doors(user: Option<&str>) -> [(&'static str, Request<axum::body::Body>); 3] {
        [
            (
                "batch",
                batch_request(
                    user,
                    json!([{"subject_kind": "employee", "code": "taxonomist",
                            "display_name": "Taxonomist", "member_attribute": "role",
                            "sort_order": 9}]),
                ),
            ),
            (
                "edit",
                put_request(
                    user,
                    "employee",
                    "platform-admin",
                    json!({"subject_kind": "employee", "code": "platform-admin",
                           "display_name": "Edited", "member_attribute": "role",
                           "sort_order": 3, "metadata": {}}),
                ),
            ),
            ("retire", retire_request(user, "platform-admin")),
        ]
    }

    /// What a refused door must leave behind: the seeded row exactly as
    /// it was, no new row, and no fact.
    async fn nothing_was_written(repo: &InMemoryClasses, door: &str) {
        let held = repo
            .get(&ClassRef::new("employee", "platform-admin"))
            .await
            .unwrap()
            .expect("the seeded row");
        assert_eq!(held.display_name, "Platform admin", "{door}");
        assert!(held.retired_at.is_none(), "{door}");
        assert!(
            repo.get(&ClassRef::new("employee", "taxonomist"))
                .await
                .unwrap()
                .is_none(),
            "{door}"
        );
        assert!(repo.recorded_events().is_empty(), "{door}");
    }

    // ---- the policy question (backlog 553cf479) -----------------------

    /// The acceptance the packet states, first half: a caller the tier
    /// check refused — USER tier, a role the core defaults grant nothing
    /// — writes through all three doors once a policy rule grants it
    /// Create, Update and Retire on `class`. A rule now widens who edits
    /// a taxonomy.
    #[tokio::test]
    async fn a_granting_rule_lets_a_non_admin_write_through_every_door() {
        let policy: Arc<dyn PolicyClient> = Arc::new(
            [Action::Create, Action::Update, Action::Retire]
                .into_iter()
                .fold(
                    boss_policy_client::FakePolicyClient::builder().with_default_rules(),
                    |b, a| b.allow("taxonomy-editor", a, Resource::class(), Scope::All),
                )
                .build(),
        );
        let editor = signed("emp-taxonomist", "taxonomy-editor", "user");
        let want = [StatusCode::OK, StatusCode::OK, StatusCode::NO_CONTENT];
        let repo = seeded();
        for ((name, req), want) in the_three_doors(Some(&editor)).into_iter().zip(want) {
            let resp = door(repo.clone(), policy.clone())
                .oneshot(req)
                .await
                .unwrap();
            assert_eq!(resp.status(), want, "{name}");
        }
        let edited = repo
            .get(&ClassRef::new("employee", "platform-admin"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(edited.display_name, "Edited");
        assert!(edited.retired_at.is_some());
        assert!(
            repo.get(&ClassRef::new("employee", "taxonomist"))
                .await
                .unwrap()
                .is_some()
        );
        // The batch's declare, then the seeded row's birth (it had none,
        // backlog 6c2aa86c), its edit and its retirement.
        let events = repo.recorded_events();
        assert_eq!(events.len(), 4, "{events:?}");
        assert!(
            events
                .iter()
                .all(|e| e.payload["_actor"] == json!("emp-taxonomist")),
            "every fact is signed by the caller: {events:?}"
        );
    }

    /// The acceptance, second half: a platform-admin at operator tier —
    /// everything the tier check admitted — is refused 403 at every door
    /// by a user override that denies it, and nothing is written. Policy
    /// now narrows it too.
    #[tokio::test]
    async fn a_denying_override_refuses_a_platform_admin_at_every_door() {
        let admin_id = "claude@algedonic.dev";
        let policy: Arc<dyn PolicyClient> = Arc::new(
            [Action::Create, Action::Update, Action::Retire]
                .into_iter()
                .fold(
                    boss_policy_client::FakePolicyClient::builder().with_default_rules(),
                    |b, a| {
                        b.with_override(boss_policy_client::UserOverride {
                            id: format!("deny-classes-{}", a.as_str()),
                            user_id: admin_id.into(),
                            resource: Resource::class(),
                            action: a,
                            scope: Scope::None,
                            reason: "taxonomy frozen for the audit".into(),
                            expires_at: None,
                        })
                    },
                )
                .build(),
        );
        let admin = signed(admin_id, "platform-admin", "operator");
        for (name, req) in the_three_doors(Some(&admin)) {
            let repo = seeded();
            let resp = door(repo.clone(), policy.clone())
                .oneshot(req)
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::FORBIDDEN, "{name}");
            let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
            assert!(
                String::from_utf8_lossy(&body).contains("taxonomy frozen"),
                "{name}: the refusal carries policy's reason"
            );
            nothing_was_written(&repo, name).await;
        }
    }

    /// The rest of the ladder, at every door, and each refusal writes
    /// nothing: no identity is 401 (there is no one to ask about, and a
    /// fact must name its author), a role the defaults grant nothing is
    /// 403, a policy service that cannot answer is its own 503 — never
    /// an allow.
    #[tokio::test]
    async fn every_door_refuses_before_it_writes() {
        let cases: [(Option<String>, Arc<dyn PolicyClient>, StatusCode); 4] = [
            (None, default_policy(), StatusCode::UNAUTHORIZED),
            (
                Some(signed(
                    boss_policy_client::User::ANONYMOUS_ID,
                    "platform-admin",
                    "operator",
                )),
                default_policy(),
                StatusCode::UNAUTHORIZED,
            ),
            (
                Some(signed("emp-audit", "audit-readonly", "operator")),
                default_policy(),
                StatusCode::FORBIDDEN,
            ),
            (
                Some(operator_header()),
                Arc::new(DarkPolicy),
                StatusCode::SERVICE_UNAVAILABLE,
            ),
        ];
        for (user, policy, want) in cases {
            for (name, req) in the_three_doors(user.as_deref()) {
                let repo = seeded();
                let resp = door(repo.clone(), policy.clone())
                    .oneshot(req)
                    .await
                    .unwrap();
                assert_eq!(resp.status(), want, "{name} as {user:?}");
                nothing_was_written(&repo, name).await;
            }
        }
    }

    #[tokio::test]
    async fn list_returns_classes_for_subject_kind() {
        let app = build_app(vec![employee("ceo", 10), employee("cto", 11)]);
        let req = Request::builder()
            .uri("/api/classes?subject_kind=employee")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert!(v.is_array());
        assert_eq!(v.as_array().unwrap().len(), 2);
    }

    /// One subject_kind can hold several taxonomies, told apart only by
    /// `member_attribute` — the live employee drawer held 22 codes on
    /// four axes on 2026-09-23 (backlog ab1e6ff8). `member_attribute`
    /// narrows the list to one axis; an axis nothing carries answers an
    /// empty list, not the whole drawer; and without it the list is
    /// unchanged.
    #[tokio::test]
    async fn list_narrows_to_one_member_attribute() {
        let on = |code: &str, attribute: &str| Class {
            member_attribute: Some(attribute.into()),
            ..employee(code, 10)
        };
        let rows = vec![
            on("platform-admin", "role"),
            on("owner", "role"),
            on("it", "department"),
            on("active", "status"),
        ];
        let codes = |uri: &'static str| {
            let app = build_app(rows.clone());
            async move {
                let req = Request::builder()
                    .uri(uri)
                    .body(axum::body::Body::empty())
                    .unwrap();
                let resp = app.oneshot(req).await.unwrap();
                assert_eq!(resp.status(), StatusCode::OK, "{uri}");
                let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
                let v: Value = serde_json::from_slice(&body).unwrap();
                let mut codes: Vec<String> = v
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|c| c["code"].as_str().unwrap().to_string())
                    .collect();
                codes.sort();
                codes
            }
        };
        assert_eq!(
            codes("/api/classes?subject_kind=employee&member_attribute=role").await,
            vec!["owner", "platform-admin"]
        );
        assert_eq!(
            codes("/api/classes?subject_kind=employee&member_attribute=department").await,
            vec!["it"]
        );
        assert!(
            codes("/api/classes?subject_kind=employee&member_attribute=account_team_role")
                .await
                .is_empty(),
            "an axis nothing carries is empty, not the whole drawer"
        );
        assert_eq!(
            codes("/api/classes?subject_kind=employee").await.len(),
            4,
            "no filter, no change"
        );
    }

    #[tokio::test]
    async fn get_returns_404_for_missing_class() {
        let app = build_app(vec![employee("ceo", 10)]);
        let req = Request::builder()
            .uri("/api/classes/employee/no-such-role")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn exists_returns_boolean_envelope() {
        let app = build_app(vec![employee("ceo", 10)]);
        let req = Request::builder()
            .uri("/api/classes/employee/ceo/exists")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["exists"], json!(true));
    }

    /// A Class code containing a slash (`1/2-bbl-keg`, a product
    /// package_unit) must round-trip through the `/exists` path: the
    /// client percent-encodes the slash to `%2F`, matchit keeps it in a
    /// single `{code}` segment, and the `Path` extractor decodes it back
    /// before the lookup. Without encoding the raw slash splits the path
    /// and 404s — the bug that broke the products taxonomy gate.
    #[tokio::test]
    async fn exists_resolves_a_slash_in_the_code_when_percent_encoded() {
        let class = Class {
            subject_kind: "product".into(),
            code: "1/2-bbl-keg".into(),
            display_name: "1/2 BBL Keg".into(),
            parent_code: None,
            member_attribute: Some("package_unit".into()),
            metadata: json!({}),
            sort_order: 10,
            retired_at: None,
        };
        let app = build_app(vec![class]);
        let req = Request::builder()
            .uri("/api/classes/product/1%2F2-bbl-keg/exists")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["exists"], json!(true));
    }

    /// `x-boss-user` JSON for an operator-tier caller. Mirrors the
    /// header the gateway injects + the seed binaries send.
    fn operator_header() -> String {
        json!({
            "id": "automation:test-seed",
            "role": "platform-admin",
            "access_tier": "operator",
            "territory_account_ids": [],
            "direct_report_ids": [],
        })
        .to_string()
    }

    fn batch_request(user_header: Option<&str>, body: Value) -> Request<axum::body::Body> {
        let mut b = Request::builder()
            .method("POST")
            .uri("/api/classes/batch")
            .header("content-type", "application/json");
        if let Some(h) = user_header {
            b = b.header("x-boss-user", h);
        }
        b.body(axum::body::Body::from(body.to_string())).unwrap()
    }

    fn put_request(
        user_header: Option<&str>,
        subject_kind: &str,
        code: &str,
        body: Value,
    ) -> Request<axum::body::Body> {
        let mut b = Request::builder()
            .method("PUT")
            .uri(format!("/api/classes/{subject_kind}/{code}"))
            .header("content-type", "application/json");
        if let Some(h) = user_header {
            b = b.header("x-boss-user", h);
        }
        b.body(axum::body::Body::from(body.to_string())).unwrap()
    }

    fn seeded() -> Arc<InMemoryClasses> {
        Arc::new(InMemoryClasses::new(vec![Class {
            subject_kind: "employee".into(),
            code: "platform-admin".into(),
            display_name: "Platform admin".into(),
            parent_code: None,
            member_attribute: Some("role".into()),
            metadata: json!({"is_executive": true, "is_system_role": true}),
            sort_order: 3,
            retired_at: None,
        }]))
    }

    /// The gap this closes: `batch_upsert` is insert-if-absent, so a
    /// seeded Class could never be edited by anything. A taxonomy the
    /// tenant cannot change is not data.
    #[tokio::test]
    async fn put_edits_an_existing_class() {
        let repo = seeded();
        let app = door(repo.clone(), default_policy());
        let resp = app
            .oneshot(put_request(
                Some(&operator_header()),
                "employee",
                "platform-admin",
                json!({
                    "subject_kind": "employee",
                    "code": "platform-admin",
                    "display_name": "Platform admin",
                    "member_attribute": "role",
                    "sort_order": 3,
                    "metadata": {"is_system_role": true}
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let stored = repo
            .get(&ClassRef::new("employee", "platform-admin"))
            .await
            .unwrap()
            .expect("row still present");
        assert_eq!(stored.metadata, json!({"is_system_role": true}));
    }

    /// The key comes from the URL, never the body. A code is an
    /// identity other rows point at, so honouring a body that
    /// disagreed would move the row and orphan them silently.
    #[tokio::test]
    async fn put_ignores_a_key_in_the_body() {
        let repo = seeded();
        let app = door(repo.clone(), default_policy());
        let resp = app
            .oneshot(put_request(
                Some(&operator_header()),
                "employee",
                "platform-admin",
                json!({
                    "subject_kind": "vendor",
                    "code": "somebody-else",
                    "display_name": "Renamed",
                    "member_attribute": "role",
                    "sort_order": 3,
                    "metadata": {}
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        assert!(
            repo.get(&ClassRef::new("vendor", "somebody-else"))
                .await
                .unwrap()
                .is_none(),
            "a body key must not create or move a row"
        );
        let stored = repo
            .get(&ClassRef::new("employee", "platform-admin"))
            .await
            .unwrap()
            .expect("original row edited in place");
        assert_eq!(stored.display_name, "Renamed");
    }

    /// The fact an edit leaves (backlog 10dabe13, 2026-09-27): until
    /// then only `class.declared` reached the log, so the three rows
    /// `boss tenant publish --take classes` edited through this door on
    /// 2026-09-26 left no fact at all. One `class.updated` per PUT that
    /// CHANGED the row — the key, the changed fields' before and after,
    /// and `updated_by`, the actor the request signed with — and none
    /// for a PUT that restates what the row already holds.
    #[tokio::test]
    async fn put_records_one_updated_event_naming_what_changed_and_none_for_a_restatement() {
        let repo = seeded();
        let body = json!({
            "subject_kind": "employee",
            "code": "platform-admin",
            "display_name": "Platform administrator",
            "member_attribute": "role",
            "sort_order": 3,
            "metadata": {"is_system_role": true}
        });
        for _ in 0..2 {
            let resp = door(repo.clone(), default_policy())
                .oneshot(put_request(
                    Some(&operator_header()),
                    "employee",
                    "platform-admin",
                    body.clone(),
                ))
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
        }

        // The seeded row had no birth in the log, so the edit records it
        // first — the row before the edit, marked as a backfill (backlog
        // 6c2aa86c) — then one fact for the edit, none for the repeat.
        let events = repo.recorded_events();
        assert_eq!(
            events.len(),
            2,
            "the birth, one fact for the edit, none for the identical repeat: {events:?}"
        );
        assert_eq!(events[0].kind, crate::port::CLASS_DECLARED);
        assert!(crate::port::is_backfill(&events[0].payload));
        assert_eq!(events[0].payload["display_name"], json!("Platform admin"));
        let e = &events[1];
        assert_eq!(e.kind, crate::port::CLASS_UPDATED);
        assert_eq!(e.source, "classes");
        assert_eq!(e.payload["subject_kind"], json!("employee"));
        assert_eq!(e.payload["code"], json!("platform-admin"));
        assert_eq!(e.payload["changed"], json!(["display_name", "metadata"]));
        assert_eq!(
            e.payload["before"],
            json!({
                "display_name": "Platform admin",
                "metadata": {"is_executive": true, "is_system_role": true}
            })
        );
        assert_eq!(
            e.payload["after"],
            json!({
                "display_name": "Platform administrator",
                "metadata": {"is_system_role": true}
            })
        );
        assert_eq!(e.payload["updated_by"], json!("automation:test-seed"));
        assert_eq!(
            e.payload["_actor"], e.payload["updated_by"],
            "updated_by and the stamp's actor are one value"
        );
    }

    /// The fact a retirement leaves (backlog 10dabe13): one
    /// `class.retired` when the stamp is set — carrying the stamp and
    /// `retired_by` — and none on the idempotent repeat (the stamp did
    /// not move, so no state changed) or for a code that names nothing.
    #[tokio::test]
    async fn retire_records_one_retired_event_and_none_for_a_repeat_or_a_missing_code() {
        let repo = seeded();
        let h = operator_header();
        for code in ["platform-admin", "platform-admin", "no-such"] {
            door(repo.clone(), default_policy())
                .oneshot(retire_request(Some(&h), code))
                .await
                .unwrap();
        }

        // The seeded row's birth first (backlog 6c2aa86c), then the one
        // retirement.
        let events = repo.recorded_events();
        assert_eq!(events.len(), 2, "{events:?}");
        assert_eq!(events[0].kind, crate::port::CLASS_DECLARED);
        assert_eq!(events[0].payload["retired_at"], Value::Null);
        let e = &events[1];
        assert_eq!(e.kind, crate::port::CLASS_RETIRED);
        assert_eq!(e.source, "classes");
        assert_eq!(e.payload["subject_kind"], json!("employee"));
        assert_eq!(e.payload["code"], json!("platform-admin"));
        let held = repo
            .get(&ClassRef::new("employee", "platform-admin"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            e.payload["retired_at"],
            serde_json::to_value(held.retired_at.expect("stamped")).unwrap(),
            "the fact carries the stamp the row holds"
        );
        assert_eq!(e.payload["retired_by"], json!("automation:test-seed"));
        assert_eq!(e.payload["_actor"], e.payload["retired_by"]);
    }

    #[tokio::test]
    async fn put_on_a_missing_class_is_not_found() {
        let app = door(Arc::new(InMemoryClasses::new(vec![])), default_policy());
        let resp = app
            .oneshot(put_request(
                Some(&operator_header()),
                "employee",
                "ghost",
                json!({"subject_kind": "employee", "code": "ghost", "display_name": "Ghost", "member_attribute": "role", "sort_order": 1, "metadata": {}}),
            ))
            .await
            .unwrap();
        // Not an implicit create: PUT here edits, and a silent insert
        // would let a typo'd code become a real Class.
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn batch_upsert_inserts_rows_for_operator() {
        let repo = Arc::new(InMemoryClasses::new(vec![]));
        let app = door(repo.clone(), default_policy());
        let body = json!([
            {"subject_kind": "employee", "code": "head-brewer", "display_name": "Head Brewer", "member_attribute": "role", "sort_order": 30},
            {"subject_kind": "employee", "code": "brewer", "display_name": "Brewer", "member_attribute": "role", "sort_order": 32},
        ]);
        let resp = app
            .oneshot(batch_request(Some(&operator_header()), body))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["received"], json!(2));
        assert_eq!(v["inserted"], json!(2));

        let stored = repo.list_for_subject_kind("employee").await.unwrap();
        assert_eq!(stored.len(), 2);
    }

    #[tokio::test]
    async fn batch_upsert_is_idempotent_on_conflict() {
        let repo = Arc::new(InMemoryClasses::new(vec![employee("clerk", 32)]));
        let app = door(repo.clone(), default_policy());
        // `clerk` already present → DO NOTHING; only `cellar-tech` is new.
        let body = json!([
            {"subject_kind": "employee", "code": "clerk", "display_name": "Clerk", "member_attribute": "role", "sort_order": 32},
            {"subject_kind": "employee", "code": "cellar-tech", "display_name": "Cellar Tech", "member_attribute": "role", "sort_order": 33},
        ]);
        let resp = app
            .oneshot(batch_request(Some(&operator_header()), body))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["received"], json!(2));
        assert_eq!(v["inserted"], json!(1), "conflicting row is left untouched");
        // The held row differs from the declaration (its display_name
        // is the fixture's CLERK, the file says Clerk): kept, and the
        // answer names the field (design e187198f) — until 2026-09-18
        // this door said nothing about a repo row that never landed.
        assert_eq!(v["kept"][0]["id"], "employee/clerk", "{v}");
        assert_eq!(v["kept"][0]["differs"], json!(["display_name"]));
        assert_eq!(
            repo.list_for_subject_kind("employee").await.unwrap().len(),
            2
        );
        // A row declared exactly as held is not a finding.
        let same = json!([
            {"subject_kind": "employee", "code": "clerk", "display_name": "CLERK", "member_attribute": "role", "sort_order": 32},
        ]);
        let resp = door(repo.clone(), default_policy())
            .oneshot(batch_request(Some(&operator_header()), same))
            .await
            .unwrap();
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["inserted"], json!(0));
        assert_eq!(v["kept"], json!([]), "{v}");
        assert_eq!(v["unchanged"], json!(1));
    }

    /// The fact a declaration leaves (backlog d9409039, 2026-09-17):
    /// one `class.declared` per row INSERTED — the row as inserted plus
    /// `declared_by`, the actor the request signed with — and nothing
    /// for the row the registry already held, nothing for the batch.
    #[tokio::test]
    async fn batch_records_one_declared_event_per_inserted_row_and_none_for_a_kept_row() {
        let repo = Arc::new(InMemoryClasses::new(vec![employee("clerk", 32)]));
        let app = door(repo.clone(), default_policy());
        let body = json!([
            {"subject_kind": "employee", "code": "clerk", "display_name": "Clerk", "member_attribute": "role", "sort_order": 32},
            {"subject_kind": "employee", "code": "scheduler", "display_name": "Scheduler", "member_attribute": "role", "sort_order": 33},
            {"subject_kind": "employee", "code": "auditor", "display_name": "Auditor", "member_attribute": "role", "sort_order": 34},
        ]);
        let resp = app
            .oneshot(batch_request(Some(&operator_header()), body))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let events = repo.recorded_events();
        assert_eq!(
            events.len(),
            2,
            "one event per inserted row, none for the kept `clerk`"
        );
        assert!(
            events.iter().all(|e| e.kind == crate::port::CLASS_DECLARED),
            "{events:?}"
        );
        assert!(events.iter().all(|e| e.source == "classes"), "{events:?}");
        let codes: Vec<&str> = events
            .iter()
            .map(|e| e.payload["code"].as_str().unwrap())
            .collect();
        assert_eq!(codes, ["scheduler", "auditor"]);
        assert_eq!(events[0].payload["subject_kind"], json!("employee"));
        assert_eq!(events[0].payload["display_name"], json!("Scheduler"));
        assert_eq!(events[0].payload["sort_order"], json!(33));
        assert_eq!(
            events[0].payload["declared_by"],
            json!("automation:test-seed"),
            "the actor the request signed with, named on the fact"
        );
        assert_eq!(
            events[0].payload["_actor"], events[0].payload["declared_by"],
            "declared_by and the stamp's actor are one value"
        );
    }

    #[tokio::test]
    async fn a_sim_chain_alone_is_not_a_caller() {
        // Backlog 85e7f10f (2026-09-25): `sim || tier_ok` let ANY caller
        // that reached :7800 directly write the registry by sending
        // `x-sim-origin: true`. The chain flag is set here directly (the
        // router under test omits the middleware) and the caller is
        // anonymous — no sim identity, no identity at all — so the door
        // refuses 401 and nothing lands, whatever the deployment's sim
        // switch says (the sim-on leg, through the real middleware, is
        // tests/a_sim_header_alone_writes_no_class.rs).
        let repo = Arc::new(InMemoryClasses::new(vec![]));
        let app = door(repo.clone(), default_policy());
        let body = json!([
            {"subject_kind": "employee", "code": "brewer", "display_name": "Brewer", "member_attribute": "role", "sort_order": 32},
        ]);
        let resp =
            boss_core::sim_origin::with_sim_chain(true, app.oneshot(batch_request(None, body)))
                .await
                .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            repo.list_for_subject_kind("employee").await.unwrap().len(),
            0
        );
    }
    fn retire_request(user_header: Option<&str>, code: &str) -> Request<axum::body::Body> {
        let mut b = Request::builder()
            .method("POST")
            .uri(format!("/api/classes/employee/{code}/retire"));
        if let Some(h) = user_header {
            b = b.header("x-boss-user", h);
        }
        b.body(axum::body::Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn retire_withdraws_a_code_from_active_use_but_keeps_the_row() {
        let app = build_app(vec![employee("ceo", 10), employee("cto", 11)]);
        let h = operator_header();
        let resp = app
            .clone()
            .oneshot(retire_request(Some(&h), "cto"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);

        // Withdrawn: the validation primitive refuses it…
        let req = Request::builder()
            .uri("/api/classes/employee/cto/exists")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            v["exists"],
            json!(false),
            "exists_active must refuse a retired code"
        );

        // …the list stops offering it…
        let req = Request::builder()
            .uri("/api/classes?subject_kind=employee")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            v.as_array().unwrap().len(),
            1,
            "retired codes leave the list"
        );

        // …but the row stays readable: existing rows point at it.
        let req = Request::builder()
            .uri("/api/classes/employee/cto")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "a retired Class is history, not gone"
        );
    }

    #[tokio::test]
    async fn retire_is_idempotent() {
        let app = build_app(vec![employee("ceo", 10)]);
        let h = operator_header();
        for _ in 0..2 {
            let resp = app
                .clone()
                .oneshot(retire_request(Some(&h), "ceo"))
                .await
                .unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::NO_CONTENT,
                "a repeat retire is a no-op"
            );
        }
    }

    fn backfill_request(user_header: Option<&str>, code: &str) -> Request<axum::body::Body> {
        let mut b = Request::builder()
            .method("POST")
            .uri(format!("/api/classes/employee/{code}/backfill-declared"));
        if let Some(h) = user_header {
            b = b.header("x-boss-user", h);
        }
        b.body(axum::body::Body::empty()).unwrap()
    }

    /// Backlog 9d345f9b: the door that records the `class.declared` a
    /// row was born without. A row held with no fact is backfilled ONCE
    /// as the row it is, marked as a backfill and signed by the caller;
    /// the repeat records nothing, a retirement after it records no
    /// second birth, a missing code is 404, and the door asks policy for
    /// Create on `class` before it stages anything. (A row retired
    /// through the door BEFORE its birth — the un-apply path — can no
    /// longer be made through the doors, which declare first since
    /// backlog 6c2aa86c; that shape is pinned against Postgres, in
    /// tests/classes_pg.rs.)
    #[tokio::test]
    async fn backfill_records_one_declared_birth_and_nothing_for_a_repeat() {
        let repo = Arc::new(InMemoryClasses::new(vec![employee("hosting", 60)]));
        let app = door(repo.clone(), default_policy());
        let h = operator_header();
        let resp = app
            .clone()
            .oneshot(backfill_request(Some(&h), "hosting"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["backfilled"]["code"], json!("hosting"));
        assert_eq!(v["backfilled"]["retired_at"], Value::Null);

        let events = repo.recorded_events();
        assert_eq!(events.len(), 1, "{events:?}");
        let declared = &events[0];
        assert_eq!(declared.kind, crate::port::CLASS_DECLARED);
        assert!(crate::port::is_backfill(&declared.payload), "{declared:?}");
        assert_eq!(declared.payload["_actor"], json!("automation:test-seed"));

        let resp = app
            .clone()
            .oneshot(backfill_request(Some(&h), "hosting"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["already_declared"], json!(true));
        assert_eq!(repo.recorded_events().len(), 1, "a repeat records nothing");

        let resp = app
            .clone()
            .oneshot(retire_request(Some(&h), "hosting"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        let kinds: Vec<String> = repo.recorded_events().into_iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            [crate::port::CLASS_DECLARED, crate::port::CLASS_RETIRED],
            "declared already, so the retirement records no second birth"
        );

        let resp = app
            .clone()
            .oneshot(backfill_request(Some(&h), "no-such"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        for (user, want) in [
            (None, StatusCode::UNAUTHORIZED),
            (
                Some(signed("emp-audit", "audit-readonly", "operator")),
                StatusCode::FORBIDDEN,
            ),
        ] {
            let repo = seeded();
            let resp = door(repo.clone(), default_policy())
                .oneshot(backfill_request(user.as_deref(), "platform-admin"))
                .await
                .unwrap();
            assert_eq!(resp.status(), want, "{user:?}");
            assert!(repo.recorded_events().is_empty(), "{user:?}");
        }
    }

    /// The LLC's invoice Class `hosting` as it stands live: inserted on
    /// 2026-09-16 with no metadata, then given its ledger account by a
    /// bare UPDATE on 2026-09-26 that left no fact (backlog 93f361af).
    fn invoice_hosting(metadata: Value) -> Class {
        Class {
            subject_kind: "invoice".into(),
            code: "hosting".into(),
            display_name: "Hosted instance".into(),
            parent_code: None,
            member_attribute: Some("revenue_category".into()),
            metadata,
            sort_order: 30,
            retired_at: None,
        }
    }

    fn backfill_with(user_header: Option<&str>, body: &Value) -> Request<axum::body::Body> {
        let mut b = Request::builder()
            .method("POST")
            .uri("/api/classes/invoice/hosting/backfill-declared")
            .header("content-type", "application/json");
        if let Some(h) = user_header {
            b = b.header("x-boss-user", h);
        }
        b.body(axum::body::Body::from(body.to_string())).unwrap()
    }

    /// Backlog 93f361af: a Class edited outside the doors has no fact
    /// for the edit, so its live body is not its birth. The caller names
    /// the body it was born with and where that body is recorded (the
    /// tenant commit); the door records the birth AND the edit, both
    /// marked as backfills naming that source, and only when the two
    /// replay from nothing to the live row. A repeat records nothing.
    #[tokio::test]
    async fn a_named_birth_is_backfilled_with_the_edit_that_made_the_live_row() {
        let live = invoice_hosting(json!({"gl_account": "4400"}));
        let repo = Arc::new(InMemoryClasses::new(vec![live.clone()]));
        let app = door(repo.clone(), default_policy());
        let h = operator_header();
        let body = json!({
            "born": invoice_hosting(json!({})),
            "changed": ["metadata"],
            "source": "algedonic-llc 28a7ac1^ seeds/classes.json",
        });
        let resp = app
            .clone()
            .oneshot(backfill_with(Some(&h), &body))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["backfilled"]["metadata"], json!({}), "{v}");
        assert_eq!(v["edited"]["changed"], json!(["metadata"]), "{v}");
        assert_eq!(
            v["edited"]["after"]["metadata"],
            json!({"gl_account": "4400"})
        );

        let events = repo.recorded_events();
        let kinds: Vec<&str> = events.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(
            kinds,
            vec![crate::port::CLASS_DECLARED, crate::port::CLASS_UPDATED]
        );
        for e in &events {
            assert!(crate::port::is_backfill(&e.payload), "{e:?}");
            assert_eq!(
                e.payload[crate::port::BACKFILL_SOURCE],
                json!("algedonic-llc 28a7ac1^ seeds/classes.json")
            );
            assert_eq!(e.payload["_actor"], json!("automation:test-seed"));
        }
        // Review of car 8778f12f, finding 2: when it was born and edited
        // ride on the facts (this double holds no stamps, so null), and
        // the edit is credited to no one — the signer recorded it.
        assert!(
            events[0].payload.get(crate::port::BORN_AT).is_some(),
            "{:?}",
            events[0]
        );
        assert!(
            events[1].payload.get(crate::port::EDITED_AT).is_some(),
            "{:?}",
            events[1]
        );
        assert_eq!(
            events[1].payload.get(crate::port::EDITED_BY),
            Some(&Value::Null)
        );
        assert_eq!(
            repo.get(&ClassRef::new("invoice", "hosting"))
                .await
                .unwrap(),
            Some(live)
        );

        let resp = app
            .clone()
            .oneshot(backfill_with(Some(&h), &body))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(repo.recorded_events().len(), 2, "a repeat records nothing");
    }

    /// What the door refuses, each with nothing recorded: a named birth
    /// equal to the live row (no edit to record — the bodiless backfill
    /// is that door), one whose difference from the row is not the
    /// `changed` the caller names (a typo'd display name, review of car
    /// 8778f12f finding 3), a body without `changed`, a blank source, a
    /// body it does not know, and a caller policy does not admit.
    #[tokio::test]
    async fn a_named_birth_the_door_cannot_stand_behind_records_nothing() {
        let live = invoice_hosting(json!({"gl_account": "4400"}));
        let h = operator_header();
        let born = invoice_hosting(json!({}));
        let mut typo = born.clone();
        typo.display_name = "Hosted Instance".into();
        for (user, body, want) in [
            (
                Some(h.clone()),
                json!({"born": live.clone(), "changed": [], "source": "28a7ac1"}),
                StatusCode::CONFLICT,
            ),
            (
                Some(h.clone()),
                json!({"born": typo, "changed": ["metadata"], "source": "28a7ac1"}),
                StatusCode::CONFLICT,
            ),
            (
                Some(h.clone()),
                json!({"born": born.clone(), "source": "28a7ac1"}),
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            (
                Some(h.clone()),
                json!({"born": born.clone(), "changed": ["metadata"], "source": "  "}),
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            (
                Some(h.clone()),
                json!({"born": born.clone(), "changed": ["metadata"], "source": "28a7ac1",
                       "then": "edit"}),
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            (
                None,
                json!({"born": born.clone(), "changed": ["metadata"], "source": "28a7ac1"}),
                StatusCode::UNAUTHORIZED,
            ),
        ] {
            let repo = Arc::new(InMemoryClasses::new(vec![live.clone()]));
            let resp = door(repo.clone(), default_policy())
                .oneshot(backfill_with(user.as_deref(), &body))
                .await
                .unwrap();
            assert_eq!(resp.status(), want, "{body}");
            assert!(repo.recorded_events().is_empty(), "{body}");
        }
    }

    /// Review of car 8778f12f, finding 4: a named birth records an EDIT
    /// as well as a birth, so it asks policy for Update on `class` as
    /// well as Create. A caller granted Create alone may still record a
    /// bodiless birth — the door it had — but is refused 403 a named one,
    /// with nothing recorded; granted both, it records the pair.
    #[tokio::test]
    async fn a_named_birth_asks_policy_for_update_as_well_as_create() {
        let grant = |actions: &[Action]| -> Arc<dyn PolicyClient> {
            Arc::new(
                actions
                    .iter()
                    .cloned()
                    .fold(
                        boss_policy_client::FakePolicyClient::builder().with_default_rules(),
                        |b, a| b.allow("taxonomy-editor", a, Resource::class(), Scope::All),
                    )
                    .build(),
            )
        };
        let editor = signed("emp-taxonomist", "taxonomy-editor", "user");
        let live = invoice_hosting(json!({"gl_account": "4400"}));
        let body = json!({
            "born": invoice_hosting(json!({})),
            "changed": ["metadata"],
            "source": "28a7ac1",
        });

        let repo = Arc::new(InMemoryClasses::new(vec![live.clone()]));
        let resp = door(repo.clone(), grant(&[Action::Create]))
            .oneshot(backfill_with(Some(&editor), &body))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        assert!(repo.recorded_events().is_empty());
        let bodiless = Request::builder()
            .method("POST")
            .uri("/api/classes/invoice/hosting/backfill-declared")
            .header("x-boss-user", &editor)
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = door(repo.clone(), grant(&[Action::Create]))
            .oneshot(bodiless)
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED, "Create is its door");

        let repo = Arc::new(InMemoryClasses::new(vec![live]));
        let resp = door(repo.clone(), grant(&[Action::Create, Action::Update]))
            .oneshot(backfill_with(Some(&editor), &body))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        assert_eq!(repo.recorded_events().len(), 2);
    }

    fn observed_request(body: &Value) -> Request<axum::body::Body> {
        Request::builder()
            .method("POST")
            .uri("/api/classes/employee/archived/backfill-declared")
            .header("content-type", "application/json")
            .header("x-boss-user", operator_header())
            .body(axum::body::Body::from(body.to_string()))
            .unwrap()
    }

    fn get(uri: &str) -> Request<axum::body::Body> {
        Request::builder()
            .uri(uri)
            .body(axum::body::Body::empty())
            .unwrap()
    }

    async fn json_of(resp: Response) -> Value {
        serde_json::from_slice(&to_bytes(resp.into_body(), 64 * 1024).await.unwrap()).unwrap()
    }

    /// Review of backlog 6c2aa86c's car: the observed birth. `{"observed":
    /// true, "source"}` records the row as it stands — a migration's
    /// retirement kept — marked observed, 201; a repeat is 200. The dry
    /// run reads `declare` before (this double has no stamps, so it never
    /// refuses) and `nothing` after, for the one Class and in the list;
    /// no row is 404. A body that does not say `observed: true`, gives no
    /// reason, or carries a key it does not know is 422, with nothing
    /// recorded.
    #[tokio::test]
    async fn an_observed_birth_is_recorded_on_the_operators_word_and_the_dry_run_reads_it() {
        let mut archived = employee("archived", 9);
        archived.retired_at = Some(chrono::Utc::now());
        let repo = Arc::new(InMemoryClasses::new(vec![
            archived.clone(),
            employee("ceo", 1),
        ]));
        let app = door(repo.clone(), default_policy());

        for bad in [
            json!({"observed": false, "source": "x"}),
            json!({"observed": true, "source": "  "}),
            json!({"observed": true}),
            json!({"observed": true, "source": "x", "born": {}}),
        ] {
            let resp = app.clone().oneshot(observed_request(&bad)).await.unwrap();
            assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY, "{bad}");
        }
        assert!(repo.recorded_events().is_empty());

        let v = json_of(
            app.clone()
                .oneshot(get("/api/classes/employee/archived/birth"))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(
            (v["door"].clone(), v["retired"].clone()),
            (json!("declare"), json!(true))
        );

        let body = json!({"observed": true, "source": "retired by migration 20260926061106"});
        let resp = app.clone().oneshot(observed_request(&body)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let v = json_of(resp).await;
        assert_eq!(v["birth"], json!("observed"));
        assert_eq!(
            v["backfill_source"],
            json!("retired by migration 20260926061106")
        );
        assert_eq!(
            v["backfilled"]["retired_at"],
            serde_json::to_value(archived.retired_at).unwrap()
        );
        let resp = app.clone().oneshot(observed_request(&body)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "a repeat records nothing");
        let events = repo.recorded_events();
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0].payload[crate::port::BIRTH], json!("observed"));

        let v = json_of(
            app.clone()
                .oneshot(get("/api/classes/births?subject_kind=employee"))
                .await
                .unwrap(),
        )
        .await;
        let doors: Vec<(String, String)> = v
            .as_array()
            .unwrap()
            .iter()
            .map(|p| {
                (
                    p["code"].as_str().unwrap().to_string(),
                    p["door"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        assert_eq!(
            doors,
            vec![
                ("archived".to_string(), "nothing".to_string()),
                ("ceo".to_string(), "declare".to_string())
            ]
        );
        let resp = app
            .clone()
            .oneshot(get("/api/classes/employee/no-such/birth"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn retire_of_a_missing_code_is_404() {
        let app = build_app(vec![employee("ceo", 10)]);
        let h = operator_header();
        let resp = app
            .clone()
            .oneshot(retire_request(Some(&h), "no-such"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
