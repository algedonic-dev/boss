//! The query — one round trip that returns a Subject with the Jobs
//! about it and the events behind those.
//!
//! Q3 in docs/design/global-search.md refused a Subjects-only v1: it
//! ships sooner and demonstrates nothing a conventional search box
//! does not, while setting the expectation that search IS a name
//! lookup. So the unified shape is here from the first release.

use sqlx::{PgPool, Row};

use boss_policy_client::{AccessTier, Predicate, Resource, User};

use crate::error::SearchError;
use crate::types::{RefKind, SearchResults, SearchRow, SubjectHit};

/// Cap on each result group. A dropdown shows a handful; the full
/// results surface pages. Deliberately not configurable per-caller yet
/// — one number is easier to reason about than a knob nobody tunes.
const GROUP_LIMIT: i64 = 10;
/// Event preview per Subject hit. The count comes back separately, so
/// the UI can say "3 of 214" rather than implying this is all of them.
const EVENT_PREVIEW: i64 = 5;

fn row_to_search_row(r: &sqlx::postgres::PgRow) -> Result<SearchRow, SearchError> {
    let kind: String = r.try_get("ref_kind").map_err(SearchError::storage)?;
    Ok(SearchRow {
        ref_kind: RefKind::parse(&kind)
            .ok_or_else(|| SearchError::Storage(format!("unknown ref_kind `{kind}`")))?,
        ref_id: r.try_get("ref_id").map_err(SearchError::storage)?,
        subject_kind: r.try_get("subject_kind").map_err(SearchError::storage)?,
        subject_id: r.try_get("subject_id").map_err(SearchError::storage)?,
        title: r.try_get("title").map_err(SearchError::storage)?,
        body: r.try_get("body").map_err(SearchError::storage)?,
        occurred_at: r.try_get("occurred_at").map_err(SearchError::storage)?,
    })
}

/// Run a query.
///
/// `app_subject_kinds`, when non-empty, floats Subjects of those kinds
/// to the top — Q4's "prioritise results from the app you are in". It
/// filters nothing: the whole point of a global box is that it still
/// finds the thing when you are looking in the wrong place.
/// What this caller may see, per result kind.
///
/// The design doc for this feature said it plainly: "A result the
/// caller could not open must not appear — a search box is an
/// excellent way to leak the existence of records a role cannot
/// read." It shipped without that. This is it.
pub struct SearchScope {
    /// Job owners the caller may read. `None` = unrestricted. An
    /// empty vec = nothing, which `= ANY('{}')` renders as no rows.
    pub job_owners: Option<Vec<String>>,
    /// Whether raw audit-log rows may be returned at all.
    pub may_read_events: bool,
}

impl SearchScope {
    /// Derive the caller's scope from policy.
    ///
    /// `jobs` has a real policy vocabulary, so job hits get the same
    /// read-scope predicate `/api/jobs` applies. Events have no
    /// policy resource at all — there is no `event` row in
    /// `policy_rules` — so they are gated at operator/auditor tier,
    /// the control `/system/monitoring` already applies to the audit
    /// log it surfaces. Same split, and same reasoning, as
    /// boss-views' resolver.
    pub async fn for_user(
        policy: &dyn boss_policy_client::PolicyClient,
        user: &User,
    ) -> Result<Self, SearchError> {
        let predicate = policy
            .scope_predicate(user, Resource::job())
            .await
            .map_err(|e| SearchError::storage_msg(format!("policy check failed: {e}")))?;
        let job_owners = match predicate {
            Predicate::Unrestricted => None,
            Predicate::None => Some(Vec::new()),
            Predicate::OwnerIs { user_id } => Some(vec![user_id]),
            Predicate::OwnerIn { user_ids } => Some(user_ids),
            Predicate::DepartmentIs { department } => {
                if user.department.as_deref() == Some(department.as_str()) {
                    None
                } else {
                    Some(Vec::new())
                }
            }
            // AccountIn narrows by the Job's subject, which the index
            // does not carry as an owner. Refuse rather than widen.
            Predicate::AccountIn { .. } => Some(Vec::new()),
        };
        Ok(Self {
            job_owners,
            may_read_events: matches!(user.access_tier, AccessTier::Operator | AccessTier::Auditor),
        })
    }

    /// Unrestricted — for callers that have already been authorized
    /// elsewhere (tests, the reindexer).
    pub fn unrestricted() -> Self {
        Self {
            job_owners: None,
            may_read_events: true,
        }
    }
}

pub async fn search(
    pool: &PgPool,
    query: &str,
    app_subject_kinds: &[String],
    scope: &SearchScope,
) -> Result<SearchResults, SearchError> {
    let q = query.trim();
    if q.is_empty() {
        return Err(SearchError::BadRequest("query is empty".into()));
    }

    // Full-text for prose, prefix for ids — an operator pasting
    // `inv-step-c9cd8f…` is not writing English, and `to_tsquery`
    // tokenises that into something that matches nothing.
    let sql = "\
        SELECT ref_kind, ref_id, subject_kind, subject_id, title, body, occurred_at, \
               ts_rank(tsv, plainto_tsquery('english', $1)) AS rank \
        FROM search_index \
        WHERE (tsv @@ plainto_tsquery('english', $1) \
               OR ref_id ILIKE '%' || $1 || '%' \
               OR title ILIKE '%' || $1 || '%') \
          AND ref_kind = $2 \
          AND ($5::text[] IS NULL OR ref_kind <> 'job' OR EXISTS ( \
                SELECT 1 FROM jobs j \
                WHERE j.id::text = search_index.ref_id \
                  AND j.owner_id = ANY($5))) \
        ORDER BY \
          CASE WHEN $3::text[] IS NULL OR cardinality($3::text[]) = 0 THEN 0 \
               WHEN subject_kind = ANY($3::text[]) THEN 0 ELSE 1 END, \
          rank DESC, occurred_at DESC NULLS LAST \
        LIMIT $4";

    let mut out = SearchResults {
        query: q.to_string(),
        ..Default::default()
    };

    // --- Subjects, each with its work and its history -------------
    let subject_rows = sqlx::query(sql)
        .bind(q)
        .bind(RefKind::Subject.as_str())
        .bind(app_subject_kinds)
        .bind(GROUP_LIMIT)
        .bind(scope.job_owners.as_deref())
        .fetch_all(pool)
        .await
        .map_err(SearchError::storage)?;

    for r in &subject_rows {
        let row = row_to_search_row(r)?;
        let (sk, si) = match (row.subject_kind.clone(), row.subject_id.clone()) {
            (Some(k), Some(i)) => (k, i),
            _ => continue,
        };

        // Scoped identically to the standalone job hits below —
        // without this the leak simply moves inside a SubjectHit,
        // which is the same rows in a different shape.
        let jobs = sqlx::query(
            "SELECT ref_kind, ref_id, subject_kind, subject_id, title, body, occurred_at \
             FROM search_index \
             WHERE ref_kind = 'job' AND subject_kind = $1 AND subject_id = $2 \
               AND ($4::text[] IS NULL OR EXISTS ( \
                     SELECT 1 FROM jobs j \
                     WHERE j.id::text = search_index.ref_id \
                       AND j.owner_id = ANY($4))) \
             ORDER BY occurred_at DESC NULLS LAST LIMIT $3",
        )
        .bind(&sk)
        .bind(&si)
        .bind(GROUP_LIMIT)
        .bind(scope.job_owners.as_deref())
        .fetch_all(pool)
        .await
        .map_err(SearchError::storage)?;

        let (events, event_count) = if scope.may_read_events {
            let rows = sqlx::query(
                "SELECT ref_kind, ref_id, subject_kind, subject_id, title, body, occurred_at \
                 FROM search_index \
                 WHERE ref_kind = 'event' AND subject_kind = $1 AND subject_id = $2 \
                 ORDER BY occurred_at DESC NULLS LAST LIMIT $3",
            )
            .bind(&sk)
            .bind(&si)
            .bind(EVENT_PREVIEW)
            .fetch_all(pool)
            .await
            .map_err(SearchError::storage)?;

            let count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM search_index \
                 WHERE ref_kind = 'event' AND subject_kind = $1 AND subject_id = $2",
            )
            .bind(&sk)
            .bind(&si)
            .fetch_one(pool)
            .await
            .map_err(SearchError::storage)?;
            (rows, count)
        } else {
            // The COUNT goes too. "412 events" told a caller who may
            // read none of them exactly how much happened here, which
            // is the existence disclosure the gate exists to stop.
            (Vec::new(), 0)
        };

        out.subjects.push(SubjectHit {
            subject_kind: sk,
            subject_id: si,
            title: row.title,
            jobs: jobs
                .iter()
                .map(row_to_search_row)
                .collect::<Result<Vec<_>, _>>()?,
            events: events
                .iter()
                .map(row_to_search_row)
                .collect::<Result<Vec<_>, _>>()?,
            event_count,
        });
    }

    // --- Jobs and events that matched on their own ----------------
    // A Job whose Subject also matched is already shown under that
    // Subject; repeating it here would make one thing look like two.
    let seen: Vec<String> = out
        .subjects
        .iter()
        .flat_map(|s| s.jobs.iter().map(|j| j.ref_id.clone()))
        .collect();

    for r in sqlx::query(sql)
        .bind(q)
        .bind(RefKind::Job.as_str())
        .bind(app_subject_kinds)
        .bind(GROUP_LIMIT)
        .bind(scope.job_owners.as_deref())
        .fetch_all(pool)
        .await
        .map_err(SearchError::storage)?
        .iter()
    {
        let row = row_to_search_row(r)?;
        if !seen.contains(&row.ref_id) {
            out.jobs.push(row);
        }
    }

    if scope.may_read_events {
        for r in sqlx::query(sql)
            .bind(q)
            .bind(RefKind::Event.as_str())
            .bind(app_subject_kinds)
            .bind(GROUP_LIMIT)
            .bind(scope.job_owners.as_deref())
            .fetch_all(pool)
            .await
            .map_err(SearchError::storage)?
            .iter()
        {
            out.events.push(row_to_search_row(r)?);
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_policy_client::{Action, FakePolicyClient, Scope};

    fn user(id: &str, role: &str, tier: AccessTier) -> User {
        User {
            id: id.to_string(),
            role: role.to_string(),
            access_tier: tier,
            territory_account_ids: vec![],
            direct_report_ids: vec![],
            department: None,
        }
    }

    #[tokio::test]
    async fn a_role_with_no_job_rules_sees_no_jobs() {
        // Deny-by-default has to mean an EMPTY owner list, not an
        // absent one — `None` means unrestricted, and getting that
        // backwards turns a denial into full access.
        let policy = FakePolicyClient::deny_all();
        let scope = SearchScope::for_user(&policy, &user("bob", "guest", AccessTier::User))
            .await
            .expect("scope derives");
        assert_eq!(scope.job_owners, Some(Vec::new()));
    }

    #[tokio::test]
    async fn a_self_scoped_role_sees_only_its_own_jobs() {
        let policy = FakePolicyClient::builder()
            .allow("brewer", Action::Read, Resource::job(), Scope::Self_)
            .build();
        let scope = SearchScope::for_user(&policy, &user("emp-7", "brewer", AccessTier::User))
            .await
            .expect("scope derives");
        assert_eq!(scope.job_owners, Some(vec!["emp-7".to_string()]));
    }

    #[tokio::test]
    async fn an_all_scoped_role_is_unrestricted() {
        let policy = FakePolicyClient::builder()
            .allow("coo", Action::Read, Resource::job(), Scope::All)
            .build();
        let scope = SearchScope::for_user(&policy, &user("emp-1", "coo", AccessTier::Operator))
            .await
            .expect("scope derives");
        assert_eq!(scope.job_owners, None, "None means unrestricted");
    }

    #[tokio::test]
    async fn events_need_operator_or_auditor_tier() {
        // The audit log has no policy resource, so tier is the only
        // control available. A user-tier caller must not get raw
        // event rows out of the search box.
        let policy = FakePolicyClient::builder()
            .allow("clerk", Action::Read, Resource::job(), Scope::All)
            .build();

        let low = SearchScope::for_user(&policy, &user("u", "clerk", AccessTier::User))
            .await
            .expect("scope derives");
        assert!(!low.may_read_events);

        let op = SearchScope::for_user(&policy, &user("u", "clerk", AccessTier::Operator))
            .await
            .expect("scope derives");
        assert!(op.may_read_events);

        let auditor = SearchScope::for_user(&policy, &user("u", "clerk", AccessTier::Auditor))
            .await
            .expect("scope derives");
        assert!(auditor.may_read_events);
    }

    #[test]
    fn unrestricted_is_explicit_about_being_unrestricted() {
        let s = SearchScope::unrestricted();
        assert_eq!(s.job_owners, None);
        assert!(s.may_read_events);
    }
}
