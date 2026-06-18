//! In-memory adapter for the `JobsRepository` port.
//!
//! Used by tests and by dev/demo environments that don't need persistence.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use boss_core::job::{Job, JobId, JobStatus, Step, StepId, StepStatus};

use crate::port::{JobFilter, JobScope, JobsError, JobsRepository, LaunchCalendarRow};

#[derive(Default)]
pub struct InMemoryJobs {
    inner: Mutex<State>,
}

#[derive(Default)]
struct State {
    jobs: HashMap<String, Job>,
    steps: HashMap<String, Step>,
}

impl InMemoryJobs {
    pub fn new() -> Self {
        Self::default()
    }
}

fn job_key(id: &JobId) -> String {
    id.to_string()
}

fn step_key(id: &StepId) -> String {
    id.to_string()
}

fn matches_filter(job: &Job, filter: &JobFilter) -> bool {
    if let Some(ref kind) = filter.kind
        && &job.kind != kind
    {
        return false;
    }
    if let Some(ref prefix) = filter.kind_prefix
        && !job.kind.starts_with(prefix)
    {
        return false;
    }
    if let Some(status) = filter.status
        && job.status != status
    {
        return false;
    }
    if let Some(priority) = filter.priority
        && job.priority != priority
    {
        return false;
    }
    if let Some(ref owner) = filter.owner_id
        && &job.owner_id != owner
    {
        return false;
    }
    if let Some(ref ref_id) = filter.subject_id
        && boss_core::primitives::Subject::id(&job.subject) != ref_id.as_str()
    {
        return false;
    }
    match &filter.scope {
        JobScope::All => {}
        JobScope::None => return false,
        JobScope::OwnerIs(u) => {
            if &job.owner_id != u {
                return false;
            }
        }
        JobScope::OwnerIn(us) => {
            if !us.contains(&job.owner_id) {
                return false;
            }
        }
        JobScope::AccountIn(ps) => {
            // Same territory-scope shape as policy_glue::territory_matches
            // (Wave 3): only Account/Employee subjects carry an id
            // that maps into the account list; all others deny.
            let kind = boss_core::primitives::Subject::kind(&job.subject);
            let id = boss_core::primitives::Subject::id(&job.subject);
            let matches = matches!(kind, "account" | "employee") && ps.iter().any(|p| p == id);
            if !matches {
                return false;
            }
        }
    }
    true
}

#[async_trait]
impl JobsRepository for InMemoryJobs {
    async fn create_job_at(
        &self,
        job: &Job,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), JobsError> {
        let mut state = self.inner.lock().expect("poisoned");
        state.jobs.insert(job_key(&job.id), job.clone());
        Ok(())
    }

    async fn get_job(&self, id: &JobId) -> Result<Option<Job>, JobsError> {
        let state = self.inner.lock().expect("poisoned");
        Ok(state.jobs.get(&job_key(id)).cloned())
    }

    async fn update_job_at(
        &self,
        job: &Job,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), JobsError> {
        let mut state = self.inner.lock().expect("poisoned");
        let key = job_key(&job.id);
        if !state.jobs.contains_key(&key) {
            return Err(JobsError::NotFound(job.id));
        }
        state.jobs.insert(key, job.clone());
        Ok(())
    }

    async fn list_jobs(
        &self,
        filter: &JobFilter,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<Job>, i64), JobsError> {
        let state = self.inner.lock().expect("poisoned");
        let mut jobs: Vec<&Job> = state
            .jobs
            .values()
            .filter(|j| matches_filter(j, filter))
            .collect();
        jobs.sort_by_key(|j| std::cmp::Reverse(j.opened_on));
        let total = jobs.len() as i64;
        let page = jobs
            .into_iter()
            .skip(offset as usize)
            .take(limit as usize)
            .cloned()
            .collect();
        Ok((page, total))
    }

    async fn add_step_at(
        &self,
        step: &Step,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), JobsError> {
        let mut state = self.inner.lock().expect("poisoned");
        state.steps.insert(step_key(&step.id), step.clone());
        Ok(())
    }

    async fn get_step(&self, id: &StepId) -> Result<Option<Step>, JobsError> {
        let state = self.inner.lock().expect("poisoned");
        Ok(state.steps.get(&step_key(id)).cloned())
    }

    async fn update_step_at(
        &self,
        step: &Step,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), JobsError> {
        let mut state = self.inner.lock().expect("poisoned");
        let key = step_key(&step.id);
        let Some(existing) = state.steps.get(&key) else {
            return Err(JobsError::StepNotFound(step.id));
        };
        // Mirror the SQL adapter: the generic update never writes the
        // stamp fields — stamps are append-only via append_sign_off,
        // requirements are set at materialization —
        // and terminal statuses are immutable at the row, so a write
        // merged against a stale pre-completion fetch cannot demote.
        let mut next = step.clone();
        next.sign_offs = existing.sign_offs.clone();
        next.sign_offs_required = existing.sign_offs_required.clone();
        next.fields = existing.fields.clone();
        if matches!(existing.status, StepStatus::Completed | StepStatus::Skipped) {
            next.status = existing.status;
            next.completed_on = existing.completed_on;
            next.metadata = existing.metadata.clone();
        }
        state.steps.insert(key, next);
        Ok(())
    }

    async fn append_sign_off(
        &self,
        step_id: &StepId,
        stamp: &boss_core::job::SignOffStamp,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), JobsError> {
        let mut state = self.inner.lock().expect("poisoned");
        let key = step_key(step_id);
        let Some(existing) = state.steps.get_mut(&key) else {
            return Err(JobsError::StepNotFound(*step_id));
        };
        existing.sign_offs.push(stamp.clone());
        Ok(())
    }

    async fn list_steps(&self, job_id: &JobId) -> Result<Vec<Step>, JobsError> {
        let state = self.inner.lock().expect("poisoned");
        let job_key = job_id.to_string();
        let mut steps: Vec<Step> = state
            .steps
            .values()
            .filter(|s| s.job_id.to_string() == job_key)
            .cloned()
            .collect();
        steps.sort_by_key(|s| s.sort_order);
        Ok(steps)
    }

    async fn count_in_flight_steps_by_kind(&self, step_kind: &str) -> Result<i64, JobsError> {
        let state = self.inner.lock().expect("poisoned");
        let n = state
            .steps
            .values()
            .filter(|s| s.kind == step_kind)
            .filter(|s| {
                // Non-terminal = occupies the resource. v2 has no
                // Blocked; the pre-execution + in-flight set is
                // Pending | Ready | Active.
                matches!(
                    s.status,
                    StepStatus::Pending | StepStatus::Ready | StepStatus::Active,
                )
            })
            .count();
        Ok(n as i64)
    }

    async fn count_jobs_by_kind(
        &self,
        status: Option<JobStatus>,
    ) -> Result<Vec<(String, i64)>, JobsError> {
        let state = self.inner.lock().expect("poisoned");
        let mut counts: std::collections::BTreeMap<String, i64> = std::collections::BTreeMap::new();
        for job in state.jobs.values() {
            if let Some(want) = status
                && job.status != want
            {
                continue;
            }
            *counts.entry(job.kind.clone()).or_insert(0) += 1;
        }
        Ok(counts.into_iter().collect())
    }

    async fn jobs_tier_distribution(
        &self,
        status: Option<JobStatus>,
    ) -> Result<Vec<(String, i32, i64)>, JobsError> {
        let state = self.inner.lock().expect("poisoned");
        let mut counts: std::collections::BTreeMap<(String, i32), i64> =
            std::collections::BTreeMap::new();
        for job in state.jobs.values() {
            if let Some(want) = status
                && job.status != want
            {
                continue;
            }
            let min_pending = state
                .steps
                .values()
                .filter(|s| s.job_id.to_string() == job.id.to_string())
                .filter(|s| {
                    // Tier = lowest sort_order still awaiting work
                    // (non-terminal). v2 has no Blocked.
                    matches!(
                        s.status,
                        StepStatus::Pending | StepStatus::Ready | StepStatus::Active,
                    )
                })
                .map(|s| s.sort_order)
                .min();
            let tier = min_pending.unwrap_or(-1);
            *counts.entry((job.kind.clone(), tier)).or_insert(0) += 1;
        }
        Ok(counts
            .into_iter()
            .map(|((kind, tier), n)| (kind, tier, n))
            .collect())
    }

    async fn list_launch_calendar(
        &self,
        from: chrono::NaiveDate,
        to: chrono::NaiveDate,
    ) -> Result<Vec<LaunchCalendarRow>, JobsError> {
        use boss_core::primitives::Subject as _;
        let state = self.inner.lock().expect("poisoned");
        let mut out = Vec::new();
        for job in state.jobs.values() {
            if job.kind != "marketing-motion" {
                continue;
            }
            if matches!(job.status, JobStatus::Closed | JobStatus::Cancelled) {
                continue;
            }

            // Tier = min sort_order of any non-done step, or -1.
            let (tier, launch_step) = state.steps.values().filter(|s| s.job_id == job.id).fold(
                (None::<i32>, None::<&Step>),
                |(tier, launch), s| {
                    let new_tier = if matches!(
                        s.status,
                        StepStatus::Pending | StepStatus::Ready | StepStatus::Active,
                    ) {
                        Some(tier.map_or(s.sort_order, |t| t.min(s.sort_order)))
                    } else {
                        tier
                    };
                    // property, not kind: the launch step is whichever
                    // step carries launch_date (no-step-kind-match rule)
                    let new_launch = if s.metadata.get("launch_date").is_some() {
                        Some(s)
                    } else {
                        launch
                    };
                    (new_tier, new_launch)
                },
            );
            let current_tier = Some(tier.unwrap_or(-1));

            let (launch_date, launch_channel) = match launch_step {
                Some(s) => {
                    let d = s
                        .metadata
                        .get("launch_date")
                        .and_then(|v| v.as_str())
                        .and_then(|v| chrono::NaiveDate::parse_from_str(v, "%Y-%m-%d").ok());
                    let c = s
                        .metadata
                        .get("launch_channel")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    (d, c)
                }
                None => (None, None),
            };

            if let Some(d) = launch_date
                && (d < from || d > to)
            {
                continue;
            }

            let subject_id = Some(job.subject.id().to_string());

            out.push(LaunchCalendarRow {
                job_id: job.id,
                title: job.title.clone(),
                owner_id: Some(job.owner_id.clone()),
                subject_id,
                status: job.status,
                current_tier,
                launch_date,
                launch_channel,
            });
        }
        out.sort_by(|a, b| {
            a.launch_date
                .cmp(&b.launch_date)
                .then(a.title.cmp(&b.title))
        });
        Ok(out)
    }

    async fn resolve_blockers(
        &self,
        ids: &[StepId],
    ) -> Result<Vec<(StepId, StepStatus)>, JobsError> {
        let state = self.inner.lock().expect("poisoned");
        let mut results = Vec::new();
        for id in ids {
            if let Some(step) = state.steps.get(&step_key(id)) {
                results.push((step.id, step.status));
            }
        }
        Ok(results)
    }
}

/// Check if all blockers for a step are satisfied (done or skipped).
pub fn blockers_satisfied(statuses: &[(StepId, StepStatus)]) -> bool {
    statuses
        .iter()
        .all(|(_, s)| matches!(s, StepStatus::Completed | StepStatus::Skipped))
}

/// Compute the job status from its steps.
///
/// v2 has no per-step Blocked state, so a Job is `Open` until every
/// step reaches a terminal state (`Completed` / `Skipped`), at which
/// point it's `Closed` (or `PendingSignOff` if a completed step still
/// awaits sign-off). External pauses are a dispatcher concern, not a
/// derived status here.
pub fn compute_job_status(steps: &[Step]) -> JobStatus {
    if steps.is_empty() {
        return JobStatus::Open;
    }
    let all_terminal = steps
        .iter()
        .all(|s| matches!(s.status, StepStatus::Completed | StepStatus::Skipped));
    if all_terminal {
        // Defensive: completion validation refuses to complete a step
        // with unsatisfied stamps, so this state should be unreachable
        // under the sign-off contract; kept while the PendingSignOff
        // status exists.
        let unsigned = steps.iter().any(|s| !s.sign_offs_satisfied());
        if unsigned {
            return JobStatus::PendingSignOff;
        }
        return JobStatus::Closed;
    }
    JobStatus::Open
}

#[cfg(test)]
mod tests {
    use boss_core::job::{Priority, Subject};
    use chrono::NaiveDate;

    use super::*;

    fn test_date() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 4, 16).unwrap()
    }

    fn make_job(kind: &str) -> Job {
        Job::new(
            kind,
            Subject::new("asset", "SN-001"),
            "Test job",
            "emp-1",
            Priority::Standard,
            test_date(),
        )
    }

    #[tokio::test]
    async fn create_and_get_job() {
        let repo = InMemoryJobs::new();
        let job = make_job("refurb");
        repo.create_job(&job).await.unwrap();
        let got = repo.get_job(&job.id).await.unwrap().unwrap();
        assert_eq!(got.id, job.id);
        assert_eq!(got.kind, "refurb");
    }

    #[tokio::test]
    async fn update_job() {
        let repo = InMemoryJobs::new();
        let mut job = make_job("refurb");
        repo.create_job(&job).await.unwrap();
        job.status = JobStatus::Open;
        repo.update_job(&job).await.unwrap();
        let got = repo.get_job(&job.id).await.unwrap().unwrap();
        assert_eq!(got.status, JobStatus::Open);
    }

    #[tokio::test]
    async fn update_unknown_job_errors() {
        let repo = InMemoryJobs::new();
        let job = make_job("refurb");
        let result = repo.update_job(&job).await;
        assert!(matches!(result, Err(JobsError::NotFound(_))));
    }

    #[tokio::test]
    async fn list_jobs_with_filter() {
        let repo = InMemoryJobs::new();
        let j1 = make_job("refurb");
        let j2 = make_job("sale");
        repo.create_job(&j1).await.unwrap();
        repo.create_job(&j2).await.unwrap();

        let filter = JobFilter {
            kind: Some("refurb".into()),
            ..Default::default()
        };
        let (jobs, total) = repo.list_jobs(&filter, 100, 0).await.unwrap();
        assert_eq!(total, 1);
        assert_eq!(jobs[0].kind, "refurb");
    }

    #[tokio::test]
    async fn add_and_list_steps() {
        let repo = InMemoryJobs::new();
        let job = make_job("refurb");
        repo.create_job(&job).await.unwrap();

        let s1 = Step::new(job.id, "generic", "Triage", 0).with_assignee("emp-2");
        let s2 =
            Step::new(job.id, "generic", "QA", 1).with_sign_offs_required(vec!["qa-lead".into()]);
        repo.add_step(&s1).await.unwrap();
        repo.add_step(&s2).await.unwrap();

        let steps = repo.list_steps(&job.id).await.unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].title, "Triage");
        assert_eq!(steps[1].title, "QA");
        assert_eq!(steps[1].sign_offs_required, vec!["qa-lead".to_string()]);
    }

    #[tokio::test]
    async fn list_assigned_workable_returns_only_assigned_open_workable() {
        let repo = InMemoryJobs::new();
        let mut job = make_job("ingredient-restock");
        job.status = JobStatus::Open;
        repo.create_job(&job).await.unwrap();

        // Assigned + Ready -> included.
        let mut a = Step::new(job.id, "procurement", "Place PO", 0).with_assignee("emp-1");
        a.status = StepStatus::Ready;
        repo.add_step(&a).await.unwrap();
        // Assigned + Active -> included.
        let mut b = Step::new(job.id, "billing", "Send bill", 1).with_assignee("emp-2");
        b.status = StepStatus::Active;
        repo.add_step(&b).await.unwrap();
        // Unassigned + Ready -> excluded (no assignee to attribute to).
        let mut c = Step::new(job.id, "bill-approval", "Approve", 2);
        c.status = StepStatus::Ready;
        repo.add_step(&c).await.unwrap();
        // Assigned + Completed -> excluded (terminal).
        let mut d = Step::new(job.id, "trigger", "Trig", 3).with_assignee("emp-1");
        d.status = StepStatus::Completed;
        repo.add_step(&d).await.unwrap();
        // Assigned + Pending -> excluded (not workable yet).
        let e = Step::new(job.id, "receiving", "Receive", 4).with_assignee("emp-1");
        repo.add_step(&e).await.unwrap();

        // Assigned + Ready but on a CLOSED job -> excluded.
        let mut closed = make_job("ingredient-restock");
        closed.status = JobStatus::Closed;
        repo.create_job(&closed).await.unwrap();
        let mut f = Step::new(closed.id, "billing", "Stale", 0).with_assignee("emp-1");
        f.status = StepStatus::Ready;
        repo.add_step(&f).await.unwrap();

        let rows = repo.list_assigned_workable(100).await.unwrap();
        let titles: Vec<&str> = rows.iter().map(|r| r.step.title.as_str()).collect();
        assert_eq!(titles.len(), 2, "only assigned+open+workable: {titles:?}");
        assert!(titles.contains(&"Place PO"));
        assert!(titles.contains(&"Send bill"));
    }

    #[tokio::test]
    async fn list_assignments_by_assignee_or_role_only_workable_steps() {
        let repo = InMemoryJobs::new();
        let mut job = make_job("ingredient-restock");
        job.status = JobStatus::Open;
        repo.create_job(&job).await.unwrap();

        // Assigned to emp-1, Ready -> included (assignee match).
        let mut s_assigned = Step::new(job.id, "procurement", "Place PO", 0).with_assignee("emp-1");
        s_assigned.status = StepStatus::Ready;
        repo.add_step(&s_assigned).await.unwrap();

        // Unassigned, authority_role=bookkeeper, Ready -> included (role match).
        let mut s_role = Step::new(job.id, "bill-approval", "Approve bill", 1);
        s_role.status = StepStatus::Ready;
        s_role.metadata = serde_json::json!({ "authority_role": "bookkeeper" });
        repo.add_step(&s_role).await.unwrap();

        // Assigned to emp-1, Active -> included (active is workable).
        let mut s_active = Step::new(job.id, "billing", "Send bill", 2).with_assignee("emp-1");
        s_active.status = StepStatus::Active;
        repo.add_step(&s_active).await.unwrap();

        // Active, assigned to ANOTHER bookkeeper -> included: role-match on
        // an Active step ignores assignee, so the role's workforce can
        // finish in-progress work (a worker re-finds its multi-day step).
        let mut s_active_other =
            Step::new(job.id, "bill-approval", "Approve other bill", 6).with_assignee("emp-2");
        s_active_other.status = StepStatus::Active;
        s_active_other.metadata = serde_json::json!({ "authority_role": "bookkeeper" });
        repo.add_step(&s_active_other).await.unwrap();

        // Ready, already assigned to someone else -> excluded: not poachable
        // (role-match on a Ready step requires it to be unassigned).
        let mut s_ready_other =
            Step::new(job.id, "bill-approval", "Their bill", 7).with_assignee("emp-2");
        s_ready_other.status = StepStatus::Ready;
        s_ready_other.metadata = serde_json::json!({ "authority_role": "bookkeeper" });
        repo.add_step(&s_ready_other).await.unwrap();

        // Unassigned, authority_role=brewer (not mine) -> excluded.
        let mut s_other = Step::new(job.id, "production-consume", "Mash in", 3);
        s_other.status = StepStatus::Ready;
        s_other.metadata = serde_json::json!({ "authority_role": "brewer" });
        repo.add_step(&s_other).await.unwrap();

        // Completed -> excluded.
        let mut s_done = Step::new(job.id, "trigger", "Trigger", 4).with_assignee("emp-1");
        s_done.status = StepStatus::Completed;
        repo.add_step(&s_done).await.unwrap();

        // Pending -> excluded (not yet eligible).
        let s_pending = Step::new(job.id, "receiving", "Receive", 5).with_assignee("emp-1");
        repo.add_step(&s_pending).await.unwrap();

        // A Ready step on a CLOSED job -> excluded (job not Open).
        let mut closed = make_job("ingredient-restock");
        closed.status = JobStatus::Closed;
        repo.create_job(&closed).await.unwrap();
        let mut s_closed = Step::new(closed.id, "billing", "Stale bill", 0).with_assignee("emp-1");
        s_closed.status = StepStatus::Ready;
        repo.add_step(&s_closed).await.unwrap();

        let rows = repo
            .list_assignments(Some("emp-1"), &["bookkeeper".to_string()], 100)
            .await
            .unwrap();
        let titles: Vec<&str> = rows.iter().map(|r| r.step.title.as_str()).collect();
        assert!(titles.contains(&"Place PO"), "assignee+ready: {titles:?}");
        assert!(titles.contains(&"Approve bill"), "role+ready: {titles:?}");
        assert!(titles.contains(&"Send bill"), "assignee+active: {titles:?}");
        assert!(
            titles.contains(&"Approve other bill"),
            "active role-match ignores assignee: {titles:?}"
        );
        assert!(!titles.contains(&"Mash in"), "other role excluded");
        assert!(
            !titles.contains(&"Their bill"),
            "ready+assigned-other excluded"
        );
        assert!(!titles.contains(&"Trigger"), "completed excluded");
        assert!(!titles.contains(&"Receive"), "pending excluded");
        assert!(!titles.contains(&"Stale bill"), "closed-job step excluded");
        assert_eq!(rows.len(), 4);
        assert!(rows.iter().all(|r| r.job_kind == "ingredient-restock"));
    }

    #[tokio::test]
    async fn count_in_flight_steps_by_kind_only_counts_non_terminal() {
        let repo = InMemoryJobs::new();
        let job = make_job("refurb");
        repo.create_job(&job).await.unwrap();

        // Three pending `demo-plugin` steps, one active, one done.
        for i in 0..3 {
            let s = Step::new(job.id, "demo-plugin", "pending step", i);
            repo.add_step(&s).await.unwrap();
        }
        let mut active = Step::new(job.id, "demo-plugin", "in flight", 98);
        active.status = StepStatus::Active;
        repo.add_step(&active).await.unwrap();
        let mut done = Step::new(job.id, "demo-plugin", "finished", 99);
        done.status = StepStatus::Completed;
        repo.add_step(&done).await.unwrap();

        // Pending × 3 + Active × 1 = 4 non-terminal. Done is excluded.
        let n = repo
            .count_in_flight_steps_by_kind("demo-plugin")
            .await
            .unwrap();
        assert_eq!(n, 4);

        // A different kind returns zero.
        let n = repo.count_in_flight_steps_by_kind("other").await.unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn cross_job_resolve_blockers() {
        let repo = InMemoryJobs::new();
        let job_a = make_job("refurb");
        let job_b = make_job("sale");
        repo.create_job(&job_a).await.unwrap();
        repo.create_job(&job_b).await.unwrap();

        let mut step_a = Step::new(job_a.id, "generic", "Finish refurb", 0);
        step_a.status = StepStatus::Completed;
        repo.add_step(&step_a).await.unwrap();

        let step_b =
            Step::new(job_b.id, "generic", "Ship device", 0).with_blocked_by(vec![step_a.id]);
        repo.add_step(&step_b).await.unwrap();

        let statuses = repo.resolve_blockers(&step_b.blocked_by).await.unwrap();
        assert!(blockers_satisfied(&statuses));
    }

    #[tokio::test]
    async fn cross_job_blockers_not_satisfied() {
        let repo = InMemoryJobs::new();
        let job_a = make_job("refurb");
        let job_b = make_job("sale");
        repo.create_job(&job_a).await.unwrap();
        repo.create_job(&job_b).await.unwrap();

        let step_a = Step::new(job_a.id, "generic", "Finish refurb", 0);
        // status is Pending — not done
        repo.add_step(&step_a).await.unwrap();

        let step_b =
            Step::new(job_b.id, "generic", "Ship device", 0).with_blocked_by(vec![step_a.id]);
        repo.add_step(&step_b).await.unwrap();

        let statuses = repo.resolve_blockers(&step_b.blocked_by).await.unwrap();
        assert!(!blockers_satisfied(&statuses));
    }

    #[test]
    fn compute_status_all_done_no_signoff() {
        let job_id = JobId::new();
        let mut s = Step::new(job_id, "generic", "Do it", 0);
        s.status = StepStatus::Completed;
        assert_eq!(compute_job_status(&[s]), JobStatus::Closed);
    }

    #[test]
    fn compute_status_all_done_needs_signoff() {
        let job_id = JobId::new();
        let mut s =
            Step::new(job_id, "generic", "QA", 0).with_sign_offs_required(vec!["qa-lead".into()]);
        s.status = StepStatus::Completed;
        // no stamp collected — sign-off outstanding (defensive state;
        // completion validation normally prevents reaching this)
        assert_eq!(compute_job_status(&[s]), JobStatus::PendingSignOff);
    }

    #[test]
    fn compute_status_completed_plus_skipped_is_closed() {
        // v2: a Skipped branch is terminal. A Job whose steps are all
        // Completed or Skipped (no sign-off pending) is Closed.
        let job_id = JobId::new();
        let mut s1 = Step::new(job_id, "generic", "A", 0);
        s1.status = StepStatus::Completed;
        let mut s2 = Step::new(job_id, "generic", "B", 1);
        s2.status = StepStatus::Skipped;
        assert_eq!(compute_job_status(&[s1, s2]), JobStatus::Closed);
    }

    #[test]
    fn compute_status_mixed_is_open() {
        let job_id = JobId::new();
        let mut s1 = Step::new(job_id, "generic", "A", 0);
        s1.status = StepStatus::Completed;
        let s2 = Step::new(job_id, "generic", "B", 1); // Pending
        assert_eq!(compute_job_status(&[s1, s2]), JobStatus::Open);
    }
}
