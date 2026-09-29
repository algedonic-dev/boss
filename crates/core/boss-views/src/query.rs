//! Resolving a View to rows.
//!
//! The shape is deliberately dull: pull candidate rows from the same
//! projections every other surface reads, hand each to the filter,
//! keep what matches. No query planner, no generated SQL, no operator
//! string reaching the database.
//!
//! Filtering happens in this process rather than in SQL. That costs a
//! wider scan, and it buys the property the design cares about: the
//! filter is a `boss-expr` predicate over a JSON row, identical to the
//! predicates in dispatcher rules and step `ready_when`, and no part
//! of an operator's text is ever concatenated into a statement.

use std::sync::Arc;

use async_trait::async_trait;
use boss_policy_client::{Predicate, Resource, User};
use serde_json::{Value, json};
use sqlx::{PgPool, Row, postgres::PgRow};

use crate::error::ViewsError;
use crate::filter;
use crate::port::ViewResolver;
use crate::types::{View, ViewResults, ViewSource};

/// How a source is scoped to the caller.
///
/// A View reads `jobs`, `subjects` and `audit_log` directly, and those
/// are policy-scoped wherever else they are read. Without this the
/// feature is a way to read rows your role cannot open through any
/// surface — which is what the first version was.
///
/// There is no "nothing" scope. A caller who may read none of a source
/// is refused (`ViewsError::SourceDenied`, a 403) before any row is
/// read: it used to be a third variant here that answered zero rows,
/// and the page printed "0 matches" for a read it was never allowed to
/// make (backlog 5392cf23).
enum SourceScope {
    /// Every candidate row.
    All,
    /// Rows whose owning user is one of these. Which COLUMN carries
    /// that is the source's business — `jobs.owner_id`, but
    /// `steps.assignee_id`, because a Step's owner is whoever it is
    /// assigned to.
    Owners(Vec<String>),
}

/// Filter fields the `events` source can push into SQL, mapped to
/// their `event_facts` columns. Both halves are compile-time constants:
/// a filter selects among these, it can never name a new one, so no
/// operator text reaches the statement.
/// Every entry must name a TEXT column — see `pushdown::PushableColumns`.
/// `event_id` is deliberately absent: it is UUID, and a text bind
/// against it makes Postgres reject the statement rather than the row.
pub const EVENT_PUSHABLE: crate::pushdown::PushableColumns = &[
    ("kind", "kind", crate::pushdown::ColumnType::Text),
    ("source", "source", crate::pushdown::ColumnType::Text),
    (
        "subject_kind",
        "subject_kind",
        crate::pushdown::ColumnType::Text,
    ),
    (
        "subject_id",
        "subject_id",
        crate::pushdown::ColumnType::Text,
    ),
    // The row renders this field as `timestamp`; the column behind it
    // is `occurred_at`, which is exactly what the mapping is for.
    (
        "timestamp",
        "occurred_at",
        crate::pushdown::ColumnType::Timestamp,
    ),
    // Dotted paths reach inside the payload: `payload.sku` becomes
    // `payload #>> '{sku}'`. Without this a payload filter pushed
    // nothing, so the scan took the newest N rows and filtered them
    // in-process — `payload.sku = "FP-HAZY-1-2-BBL"` reported 0
    // against a true 351.
    ("payload", "payload", crate::pushdown::ColumnType::Json),
];

/// Filter fields the `steps` source can push into SQL.
///
/// `status`, `kind` and `assignee_id` are the three a filter actually
/// names, and Postgres already indexes them — including
/// `steps_assignee (assignee_id) WHERE status IN ('ready','active')`,
/// a partial index built for precisely the question this source
/// exists to answer. So Steps needs no projection: unlike audit_log,
/// whose subject lived inside a JSON payload, every field worth
/// filtering on is already a column.
///
/// `job_id` and `id` are absent because they are uuid, and only text
/// literals are pushed — a text bind against uuid makes Postgres
/// reject the statement rather than the row.
pub const STEP_PUSHABLE: crate::pushdown::PushableColumns = &[
    ("status", "status", crate::pushdown::ColumnType::Text),
    ("kind", "kind", crate::pushdown::ColumnType::Text),
    (
        "assignee_id",
        "assignee_id",
        crate::pushdown::ColumnType::Text,
    ),
    // Every executor-filled field and every outcome lives here, so
    // `metadata.disposition = "delivered"` is a question worth asking
    // of SQL rather than of the newest rows (backlog 2b5ad29a).
    ("metadata", "metadata", crate::pushdown::ColumnType::Json),
];

/// Filter fields the `jobs` source can push into SQL.
///
/// Every one is indexed — `jobs_kind`, `jobs_status`, `jobs_owner`,
/// `jobs_subject (subject_kind, subject_id)` — so a narrowed View is
/// an index scan rather than the newest-N slice it used to be. `id`
/// is absent: uuid, and only text literals are pushed.
pub const JOB_PUSHABLE: crate::pushdown::PushableColumns = &[
    ("kind", "kind", crate::pushdown::ColumnType::Text),
    ("status", "status", crate::pushdown::ColumnType::Text),
    ("owner_id", "owner_id", crate::pushdown::ColumnType::Text),
    (
        "subject_kind",
        "subject_kind",
        crate::pushdown::ColumnType::Text,
    ),
    (
        "subject_id",
        "subject_id",
        crate::pushdown::ColumnType::Text,
    ),
    ("priority", "priority", crate::pushdown::ColumnType::Text),
    (
        "created_at",
        "created_at",
        crate::pushdown::ColumnType::Timestamp,
    ),
    // real | simulated | shadow — indexed for the non-real partitions.
    ("partition", "partition", crate::pushdown::ColumnType::Text),
    // A department's IN / WORKING / OUT question is
    // `metadata.department = "it"`, the same key `/api/jobs?department=`
    // joins on (backlog 2b5ad29a). Pushed the way `events.payload` is.
    ("metadata", "metadata", crate::pushdown::ColumnType::Json),
];

/// Filter fields the `subjects` source can push into SQL.
///
/// `subjects` is keyed `(kind, id)` and both are TEXT, so a filter on
/// either rides the primary key. This is the source the cap bit
/// hardest: at 133,933 rows a 5,000-row scan reached under 4% of the
/// identity layer, and a View for a Subject created before that window
/// simply reported nothing.
pub const SUBJECT_PUSHABLE: crate::pushdown::PushableColumns = &[
    ("kind", "kind", crate::pushdown::ColumnType::Text),
    ("id", "id", crate::pushdown::ColumnType::Text),
    ("label", "label", crate::pushdown::ColumnType::Text),
    (
        "created_at",
        "created_at",
        crate::pushdown::ColumnType::Timestamp,
    ),
];

/// How many candidate rows a single View may scan before it stops.
///
/// The filter runs in this process, so the scan has to be bounded by
/// something. When the ceiling is hit the result says so
/// (`truncated`) rather than presenting a short answer as a complete
/// one.
pub const SCAN_CEILING: i64 = 5_000;

/// How one field decodes from its column into the JSON row.
///
/// UUIDs (`jobs.id`, `audit_log.event_id`, `steps.blocked_by`) render
/// as strings so a filter compares them the way an operator writes
/// them; timestamps as RFC3339, dates as `YYYY-MM-DD`.
#[derive(Debug, Clone, Copy)]
enum Decode {
    Text,
    OptText,
    Uuid,
    Timestamp,
    OptTimestamp,
    OptDate,
    OptTextArray,
    OptUuidArray,
    Int4,
    Int8,
    OptJson,
}

/// A source's row, as `(field a View sees, column behind it, decode)`.
///
/// ONE list per source is the SELECT, the JSON row, and the fields
/// `GET /api/views/sources` serves the column picker (backlog
/// 4a8939b5). The page used to keep its own copy "in step with the
/// SELECT lists by hand", and the events copy had lost `subject_kind`
/// and `subject_id` — so a field added here now reaches every one of
/// those three at once, and none can drift from the others.
type SourceFields = &'static [(&'static str, &'static str, Decode)];

const SUBJECT_FIELDS: SourceFields = &[
    ("kind", "kind", Decode::Text),
    ("id", "id", Decode::Text),
    ("label", "label", Decode::OptText),
    ("created_at", "created_at", Decode::Timestamp),
    ("retired_at", "retired_at", Decode::OptTimestamp),
];

const JOB_FIELDS: SourceFields = &[
    ("id", "id", Decode::Uuid),
    ("kind", "kind", Decode::Text),
    ("subject_kind", "subject_kind", Decode::OptText),
    ("subject_id", "subject_id", Decode::OptText),
    ("title", "title", Decode::OptText),
    ("owner_id", "owner_id", Decode::OptText),
    ("status", "status", Decode::Text),
    ("priority", "priority", Decode::OptText),
    ("opened_on", "opened_on", Decode::OptDate),
    ("closed_on", "closed_on", Decode::OptDate),
    ("tags", "tags", Decode::OptTextArray),
    ("created_at", "created_at", Decode::Timestamp),
    // Backlog 2b5ad29a: the packet partition and its metadata, where
    // `department` lives. Neither was a field, so no View could ask a
    // department's question.
    ("partition", "partition", Decode::Text),
    ("metadata", "metadata", Decode::OptJson),
];

const STEP_FIELDS: SourceFields = &[
    ("id", "id", Decode::Uuid),
    ("job_id", "job_id", Decode::Uuid),
    ("kind", "kind", Decode::Text),
    ("title", "title", Decode::OptText),
    ("assignee_id", "assignee_id", Decode::OptText),
    ("status", "status", Decode::Text),
    ("sort_order", "sort_order", Decode::Int4),
    ("blocked_by", "blocked_by", Decode::OptUuidArray),
    ("completed_on", "completed_on", Decode::OptDate),
    ("notes", "notes", Decode::OptText),
    ("created_at", "created_at", Decode::Timestamp),
    ("updated_at", "updated_at", Decode::Timestamp),
    // Outcomes and every executor-filled field (backlog 2b5ad29a).
    ("metadata", "metadata", Decode::OptJson),
];

/// Reads the `event_facts` projection, not audit_log: `kind` and the
/// subject columns are real columns there, so a filter naming them is
/// an index scan instead of a capped sequential read.
const EVENT_FIELDS: SourceFields = &[
    ("id", "audit_id", Decode::Int8),
    ("event_id", "event_id", Decode::Uuid),
    ("kind", "kind", Decode::Text),
    ("source", "source", Decode::OptText),
    // The row renders this field as `timestamp`; the column behind it
    // is `occurred_at`, which is exactly what the mapping is for.
    ("timestamp", "occurred_at", Decode::Timestamp),
    ("subject_kind", "subject_kind", Decode::OptText),
    ("subject_id", "subject_id", Decode::OptText),
    ("payload", "payload", Decode::OptJson),
];

/// Everything the resolver knows about one source, in one place.
struct SourceShape {
    table: &'static str,
    fields: SourceFields,
    pushable: crate::pushdown::PushableColumns,
    /// The column an owner scope lands on, for the sources that have
    /// one. `subjects` and `events` do not, so their access is
    /// all-or-nothing (see `scope_for`).
    owner_column: Option<&'static str>,
    /// Newest first. `audit_id` for events: the log's own order.
    newest_first: &'static str,
}

fn shape_of(source: ViewSource) -> SourceShape {
    match source {
        ViewSource::Subjects => SourceShape {
            table: "subjects",
            fields: SUBJECT_FIELDS,
            pushable: SUBJECT_PUSHABLE,
            owner_column: None,
            newest_first: "created_at DESC",
        },
        ViewSource::Jobs => SourceShape {
            table: "jobs",
            fields: JOB_FIELDS,
            pushable: JOB_PUSHABLE,
            owner_column: Some("owner_id"),
            newest_first: "created_at DESC",
        },
        ViewSource::Steps => SourceShape {
            table: "steps",
            fields: STEP_FIELDS,
            pushable: STEP_PUSHABLE,
            owner_column: Some("assignee_id"),
            newest_first: "created_at DESC",
        },
        ViewSource::Events => SourceShape {
            table: "event_facts",
            fields: EVENT_FIELDS,
            pushable: EVENT_PUSHABLE,
            owner_column: None,
            newest_first: "audit_id DESC",
        },
    }
}

/// What `GET /api/views/sources` serves: each source's fields and
/// pushable names, and the scan ceiling, read from the same lists the
/// resolver selects and pushes with (backlog 4a8939b5).
pub fn view_sources() -> crate::types::ViewSources {
    crate::types::ViewSources {
        scan_ceiling: SCAN_CEILING,
        sources: ViewSource::ALL
            .iter()
            .map(|&source| {
                let shape = shape_of(source);
                crate::types::SourceSchema {
                    source,
                    fields: shape.fields.iter().map(|(f, _, _)| f.to_string()).collect(),
                    pushable: shape
                        .pushable
                        .iter()
                        .map(|(f, _, t)| crate::types::PushableField {
                            field: f.to_string(),
                            column_type: *t,
                        })
                        .collect(),
                }
            })
            .collect(),
    }
}

/// A column that would not decode means this code and the schema
/// disagree. That is worth an error naming the column, never a panic
/// inside a request handler.
fn dec(e: sqlx::Error) -> ViewsError {
    ViewsError::Storage(format!("decoding view source row: {e}"))
}

fn decode(r: &PgRow, col: &str, how: Decode) -> Result<Value, ViewsError> {
    use chrono::{DateTime, NaiveDate, Utc};
    let uuids = |v: Vec<uuid::Uuid>| v.into_iter().map(|u| u.to_string()).collect::<Vec<_>>();
    Ok(match how {
        Decode::Text => json!(r.try_get::<String, _>(col).map_err(dec)?),
        Decode::OptText => json!(r.try_get::<Option<String>, _>(col).map_err(dec)?),
        Decode::Uuid => json!(r.try_get::<uuid::Uuid, _>(col).map_err(dec)?.to_string()),
        Decode::Timestamp => json!(
            r.try_get::<DateTime<Utc>, _>(col)
                .map_err(dec)?
                .to_rfc3339()
        ),
        Decode::OptTimestamp => json!(
            r.try_get::<Option<DateTime<Utc>>, _>(col)
                .map_err(dec)?
                .map(|t| t.to_rfc3339())
        ),
        Decode::OptDate => json!(
            r.try_get::<Option<NaiveDate>, _>(col)
                .map_err(dec)?
                .map(|d| d.to_string())
        ),
        Decode::OptTextArray => json!(r.try_get::<Option<Vec<String>>, _>(col).map_err(dec)?),
        Decode::OptUuidArray => json!(
            r.try_get::<Option<Vec<uuid::Uuid>>, _>(col)
                .map_err(dec)?
                .map(uuids)
        ),
        Decode::Int4 => json!(r.try_get::<i32, _>(col).map_err(dec)?),
        Decode::Int8 => json!(r.try_get::<i64, _>(col).map_err(dec)?),
        Decode::OptJson => r.try_get::<Option<Value>, _>(col).map_err(dec)?.into(),
    })
}

/// One row as the JSON object a filter and a View see.
fn render(r: &PgRow, fields: SourceFields) -> Result<Value, ViewsError> {
    fields
        .iter()
        .map(|(field, col, how)| Ok(((*field).to_string(), decode(r, col, *how)?)))
        .collect::<Result<serde_json::Map<String, Value>, ViewsError>>()
        .map(Value::Object)
}

pub struct PgViewResolver {
    pool: PgPool,
    policy: Arc<dyn boss_policy_client::PolicyClient>,
}

impl PgViewResolver {
    pub fn new(pool: PgPool, policy: Arc<dyn boss_policy_client::PolicyClient>) -> Self {
        Self { pool, policy }
    }

    /// Translate the caller's policy into a scope for this source, or
    /// refuse it.
    ///
    /// Every source is a policy question. `jobs` uses the same
    /// read-scope predicate `/api/jobs` applies; `steps` its own
    /// resource, whose owner allow-list lands on `assignee_id`.
    /// `subjects` and `events` ask about `Resource::subject()` and
    /// `Resource::event()` — registry rows like any other, so who may
    /// enumerate identity or be handed log rows is tenant-authored
    /// data rather than a tier check compiled into this crate.
    ///
    /// Those two are all-or-nothing: neither has an owner column to
    /// narrow by, so anything short of `Unrestricted` is refused. That
    /// fails closed — a tenant writing `scope = "territory"` against
    /// `event` is refused rather than handed everything.
    ///
    /// A refusal is an error, not an empty scope (backlog 5392cf23): it
    /// used to answer zero rows, which the page printed as "0 matches".
    async fn scope_for(&self, source: ViewSource, user: &User) -> Result<SourceScope, ViewsError> {
        let resource = match source {
            ViewSource::Jobs => Resource::job(),
            ViewSource::Steps => Resource::step(),
            ViewSource::Subjects => Resource::subject(),
            ViewSource::Events => Resource::event(),
        };
        let predicate = self.policy.scope_predicate(user, resource.clone()).await?;
        let scope = if shape_of(source).owner_column.is_some() {
            // One translation, shared with boss-search: `None` means
            // unrestricted and an empty list means deny, which is
            // exactly the asymmetry worth having in one place.
            match predicate.owner_allow_list(user) {
                None => Some(SourceScope::All),
                Some(ids) if ids.is_empty() => None,
                Some(ids) => Some(SourceScope::Owners(ids)),
            }
        } else {
            matches!(predicate, Predicate::Unrestricted).then_some(SourceScope::All)
        };
        scope.ok_or_else(|| ViewsError::SourceDenied {
            view_source: source.as_str(),
            resource: resource.to_string(),
            role: user.role.clone(),
        })
    }

    /// Candidate rows for a source, newest first, as JSON objects.
    ///
    /// Newest-first is the only ordering offered: it is the one an
    /// operator can predict without being told, and a View whose row
    /// order depends on an unstated rule is a View whose results
    /// change for reasons nobody can see.
    ///
    /// Constraints are bound, never interpolated. Every identifier in
    /// the statement comes from the source's compile-time shape and the
    /// values ride as parameters, so its shape is fixed no matter what
    /// an operator typed. `$1` is the scan ceiling; a source with an
    /// owner column binds the scope at `$2`, and the filter's own terms
    /// follow — getting that numbering wrong binds a filter value into
    /// the scope slot, which does not error, it returns the wrong rows
    /// (tests/all_sources_pushdown_pg.rs).
    async fn candidates(
        &self,
        source: ViewSource,
        scope: &SourceScope,
        pushdown: Option<&crate::pushdown::Pushdown>,
    ) -> Result<Vec<Value>, ViewsError> {
        let shape = shape_of(source);
        let columns: Vec<&str> = shape.fields.iter().map(|(_, col, _)| *col).collect();
        let mut clauses: Vec<String> = Vec::new();
        // The owner scope is a bound array: NULL means unrestricted,
        // otherwise the row's owner must be in it. One statement covers
        // both so the scoped path cannot drift from the unscoped one.
        let owners: Option<Option<Vec<String>>> = match (shape.owner_column, scope) {
            (Some(col), scope) => {
                clauses.push(format!("($2::text[] IS NULL OR {col} = ANY($2))"));
                Some(match scope {
                    SourceScope::All => None,
                    SourceScope::Owners(ids) => Some(ids.clone()),
                })
            }
            (None, SourceScope::All) => None,
            // `scope_for` never narrows a source with no owner column;
            // were it to, widening to every row would be the one wrong
            // answer, so this refuses instead.
            (None, SourceScope::Owners(_)) => {
                return Err(ViewsError::Storage(format!(
                    "view source {} has no owner column to scope by",
                    source.as_str()
                )));
            }
        };
        let mut binds: Vec<crate::pushdown::Bound> = Vec::new();
        if let Some(p) = pushdown {
            let mut next = if owners.is_some() { 3usize } else { 2usize };
            clauses.push(p.to_sql(&mut next, &mut binds));
        }
        let mut sql = format!("SELECT {} FROM {}", columns.join(", "), shape.table);
        if !clauses.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&clauses.join(" AND "));
        }
        sql.push_str(&format!(" ORDER BY {} LIMIT $1", shape.newest_first));

        let mut q = sqlx::query(&sql).bind(SCAN_CEILING);
        if let Some(owners) = owners {
            q = q.bind(owners);
        }
        for b in &binds {
            q = match b {
                crate::pushdown::Bound::Text(s) => q.bind(s.clone()),
                crate::pushdown::Bound::TextList(v) => q.bind(v.clone()),
                crate::pushdown::Bound::Timestamp(ts) => q.bind(*ts),
            };
        }
        let rows = q
            .fetch_all(&self.pool)
            .await
            .map_err(|e| ViewsError::Storage(e.to_string()))?;
        rows.iter().map(|r| render(r, shape.fields)).collect()
    }
}

/// Keep only the columns a View asked for. An empty column list means
/// the whole row — the source's own shape is the default, so a View
/// author who does not care about columns does not have to name them.
fn project(row: &Value, columns: &[String]) -> Value {
    if columns.is_empty() {
        return row.clone();
    }
    let mut out = serde_json::Map::new();
    for c in columns {
        // A column naming a field the row lacks yields null rather
        // than vanishing: a table whose columns appear and disappear
        // per row is unreadable, and the null is the honest answer.
        out.insert(c.clone(), row.get(c).cloned().unwrap_or(Value::Null));
    }
    Value::Object(out)
}

#[async_trait]
impl ViewResolver for PgViewResolver {
    async fn resolve(
        &self,
        view: &View,
        user: &User,
        limit: usize,
    ) -> Result<ViewResults, ViewsError> {
        let compiled = filter::compile(&view.filter)?;
        // Push what SQL can answer into the query; keep the WHOLE
        // predicate as the residual below. Pushdown is an optimization
        // and never a substitute — an extractor that misses a term
        // costs a wider scan, not a wrong answer.
        // No filter, or a filter that parsed to nothing pushable, is
        // `None` — the same list `GET /api/views/sources` serves.
        let pushdown = compiled
            .as_ref()
            .and_then(|expr| crate::pushdown::extract(expr, shape_of(view.source).pushable));
        // Scope is computed from the CALLER, not the View's author: a
        // shared View run by someone with narrower access shows them
        // their own rows, not its author's — and says so, in `scope`.
        let scope = self.scope_for(view.source, user).await?;
        let result_scope = match scope {
            SourceScope::All => crate::types::ResultScope::All,
            SourceScope::Owners(_) => crate::types::ResultScope::Owners,
        };
        let candidates = self
            .candidates(view.source, &scope, pushdown.as_ref())
            .await?;
        let scanned = candidates.len();

        let matching: Vec<Value> = candidates
            .into_iter()
            .filter(|row| match &compiled {
                Some(expr) => filter::matches(expr, row),
                None => true,
            })
            .collect();

        let matched = matching.len();
        let rows = matching
            .iter()
            .take(limit)
            .map(|r| project(r, &view.columns))
            .collect();

        Ok(ViewResults {
            view_id: view.id.clone(),
            source: view.source,
            layout: view.layout,
            rows,
            matched,
            pushed_down: pushdown.as_ref().map_or(0, |p| p.term_count()),
            truncated: scanned as i64 >= SCAN_CEILING,
            scope: result_scope,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_columns_returns_the_whole_row() {
        let row = json!({"id": "j1", "status": "open"});
        assert_eq!(project(&row, &[]), row);
    }

    #[test]
    fn named_columns_are_kept_in_order_and_others_dropped() {
        let row = json!({"id": "j1", "status": "open", "noise": 1});
        let out = project(&row, &["id".into(), "status".into()]);
        assert_eq!(out, json!({"id": "j1", "status": "open"}));
    }

    #[test]
    fn a_column_the_row_lacks_becomes_null_rather_than_disappearing() {
        // Guards a table whose column set changes row to row.
        let row = json!({"id": "j1"});
        let out = project(&row, &["id".into(), "owner_id".into()]);
        assert_eq!(out, json!({"id": "j1", "owner_id": null}));
    }

    /// Run a fresh View over `source` as `role`, through the router with
    /// the REAL resolver. The pool points at a port nothing listens on
    /// and is never reached: scope is decided before any row is read,
    /// so a refusal is the only answer these can give without erroring.
    async fn run_unreachable(
        policy: Arc<dyn boss_policy_client::PolicyClient>,
        source: &str,
        role: &str,
    ) -> (axum::http::StatusCode, String) {
        use crate::http::{ViewsApiState, router};
        use axum::body::Body;
        use axum::http::{Request, header};
        use http_body_util::BodyExt;
        use tower::ServiceExt;

        let app = router(ViewsApiState {
            repo: Arc::new(crate::in_memory::InMemoryViewsRepo::new(
                chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            )),
            resolver: Arc::new(PgViewResolver::new(
                PgPool::connect_lazy("postgres://nobody@127.0.0.1:1/none").unwrap(),
                policy,
            )),
            os_map: None,
            flow: None,
            fleet: None,
            stages: None,
        });
        let who = json!({"id": "emp-1", "role": role, "access_tier": "user"}).to_string();
        let created = app
            .clone()
            .oneshot(
                Request::post("/api/views")
                    .header("x-boss-user", &who)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({"title": "t", "source": source, "filter": "", "columns": [],
                               "layout": "count", "visibility": "private"})
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let view: Value =
            serde_json::from_slice(&created.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        let resp = app
            .oneshot(
                Request::get(format!(
                    "/api/views/{}/results",
                    view["id"].as_str().unwrap()
                ))
                .header("x-boss-user", &who)
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8_lossy(&body).to_string())
    }

    /// Backlog 5392cf23: a source the caller may not read resolved to
    /// zero candidate rows, and the page printed "0 matches" — a denied
    /// read indistinguishable from an empty one, the `total: 0` class.
    /// It is a 403 now, naming the policy resource that refused it.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_source_the_caller_may_not_read_is_refused_naming_its_resource() {
        let denied: Arc<dyn boss_policy_client::PolicyClient> =
            Arc::new(boss_policy_client::FakePolicyClient::deny_all());
        for (source, resource) in [
            ("jobs", "job"),
            ("steps", "step"),
            ("subjects", "subject"),
            ("events", "event"),
        ] {
            let (status, body) = run_unreachable(denied.clone(), source, "guest").await;
            assert_eq!(
                status,
                axum::http::StatusCode::FORBIDDEN,
                "{source}: a denied read must be refused, not answered; body: {body}"
            );
            assert!(
                body.contains(&format!("policy resource {resource}")),
                "{source}: the refusal names the resource that refused it; body: {body}"
            );
            assert!(
                body.contains("guest"),
                "and the role it refused; body: {body}"
            );
        }
    }

    /// `subjects` and `events` have no owner column, so a grant short of
    /// `Unrestricted` cannot be applied to them. That was a denial dressed
    /// as zero rows too; it is the same refusal now.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_grant_the_source_cannot_apply_is_refused_not_emptied() {
        use boss_policy_client::{Action, FakePolicyClient, Scope};
        let self_only: Arc<dyn boss_policy_client::PolicyClient> = Arc::new(
            FakePolicyClient::builder()
                .allow("clerk", Action::Read, Resource::subject(), Scope::Self_)
                .build(),
        );
        let (status, body) = run_unreachable(self_only, "subjects", "clerk").await;
        assert_eq!(status, axum::http::StatusCode::FORBIDDEN, "body: {body}");
        assert!(body.contains("policy resource subject"), "body: {body}");
    }

    /// Backlog fe9d212c: a policy outage became `ViewsError::Storage`
    /// and so a bare 500 carrying the policy client's text — the policy
    /// service's internal URL. It is its own error now, rendered the
    /// way every door renders it. Driven through the router with the
    /// REAL resolver and the real policy adapter against a port nothing
    /// listens on; the pool is lazy and never reached, because scope is
    /// decided before any row is read.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_policy_outage_answers_503_with_retry_after_and_no_address() {
        use crate::http::{ViewsApiState, router};
        use axum::body::Body;
        use axum::http::{Request, StatusCode, header};
        use http_body_util::BodyExt;
        use tower::ServiceExt;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let dark = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let app = router(ViewsApiState {
            repo: Arc::new(crate::in_memory::InMemoryViewsRepo::new(
                chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            )),
            resolver: Arc::new(PgViewResolver::new(
                PgPool::connect_lazy("postgres://nobody@127.0.0.1:1/none").unwrap(),
                Arc::new(boss_policy_client::ReqwestPolicyClient::new("views", dark)),
            )),
            os_map: None,
            flow: None,
            fleet: None,
            stages: None,
        });
        let who = json!({"id": "emp-1", "role": "operator", "access_tier": "operator"}).to_string();
        let created = app
            .clone()
            .oneshot(
                Request::post("/api/views")
                    .header("x-boss-user", &who)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({"title": "t", "source": "jobs", "filter": "", "columns": [],
                               "layout": "table", "visibility": "private"})
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::CREATED);
        let view: Value =
            serde_json::from_slice(&created.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        let id = view["id"].as_str().unwrap();

        let resp = app
            .oneshot(
                Request::get(format!("/api/views/{id}/results"))
                    .header("x-boss-user", &who)
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
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(String::from_utf8_lossy(&body), "policy-unreachable");
    }
}
