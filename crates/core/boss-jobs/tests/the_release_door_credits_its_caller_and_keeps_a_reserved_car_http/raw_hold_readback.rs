//! Re-hold at the post-release readiness read, before actual readback.
//! Selection uses committed release state, never timing or a query count.
use super::*;
use boss_jobs::registry::{WorkflowError, WorkflowRegistry, WorkflowSpec};

struct ReholdRegistry {
    inner: Arc<boss_jobs::InMemoryWorkflows>,
    jobs: Arc<InMemoryJobs>,
    armed: ReservationRace,
}

pub(super) fn registry(
    inner: Arc<boss_jobs::InMemoryWorkflows>,
    jobs: Arc<InMemoryJobs>,
    armed: Option<ReservationRace>,
) -> Arc<dyn WorkflowRegistry> {
    match armed {
        Some(armed) => Arc::new(ReholdRegistry { inner, jobs, armed }),
        None => inner,
    }
}

#[async_trait::async_trait]
impl WorkflowRegistry for ReholdRegistry {
    async fn get_active(&self, kind: &str) -> Result<WorkflowSpec, WorkflowError> {
        self.inner.get_active(kind).await
    }
    async fn get_version(&self, kind: &str, version: i32) -> Result<WorkflowSpec, WorkflowError> {
        let step = *self.armed.lock().unwrap();
        if let Some(step) = step {
            let now = self.jobs.get_step(&step).await.unwrap().unwrap();
            if now
                .metadata
                .get(boss_jobs::car::RELEASE)
                .is_some_and(Value::is_object)
                && boss_jobs::stranded::marked(&now.metadata, "hold").is_none()
            {
                self.armed.lock().unwrap().take();
                self.jobs.merge_step_metadata_at(&step,
                    json!({"hold":"new raw operator hold","releaser":"emp-david","gate_submission":{}}).as_object().unwrap(),
                    &boss_core::publisher::EventStamp::new("test",boss_core::actor::ActorId::automation("controlled-post-release-rehold")),
                ).await.unwrap();
            }
        }
        self.inner.get_version(kind, version).await
    }
    async fn list_active(
        &self,
        category: Option<&str>,
    ) -> Result<Vec<WorkflowSpec>, WorkflowError> {
        self.inner.list_active(category).await
    }
    async fn list_versions(&self, kind: &str) -> Result<Vec<WorkflowSpec>, WorkflowError> {
        self.inner.list_versions(kind).await
    }
    async fn create_draft(
        &self,
        spec: WorkflowSpec,
        actor: &boss_core::actor::ActorId,
        now: DateTime<Utc>,
    ) -> Result<WorkflowSpec, WorkflowError> {
        self.inner.create_draft(spec, actor, now).await
    }
    async fn publish(
        &self,
        kind: &str,
        actor: &boss_core::actor::ActorId,
        now: DateTime<Utc>,
    ) -> Result<WorkflowSpec, WorkflowError> {
        self.inner.publish(kind, actor, now).await
    }
    async fn retire(
        &self,
        kind: &str,
        actor: &boss_core::actor::ActorId,
        now: DateTime<Utc>,
    ) -> Result<(), WorkflowError> {
        self.inner.retire(kind, actor, now).await
    }
    async fn discard_draft(
        &self,
        kind: &str,
        version: i32,
        actor: &boss_core::actor::ActorId,
        now: DateTime<Utc>,
    ) -> Result<(), WorkflowError> {
        self.inner.discard_draft(kind, version, actor, now).await
    }
    async fn reject_draft(
        &self,
        kind: &str,
        version: i32,
        actor: &boss_core::actor::ActorId,
        now: DateTime<Utc>,
    ) -> Result<(), WorkflowError> {
        self.inner.reject_draft(kind, version, actor, now).await
    }
    async fn publish_authored(
        &self,
        spec: WorkflowSpec,
        job: JobId,
        actor: &boss_core::actor::ActorId,
        now: DateTime<Utc>,
    ) -> Result<WorkflowSpec, WorkflowError> {
        self.inner.publish_authored(spec, job, actor, now).await
    }
    async fn bootstrap_reconcile(
        &self,
        specs: &[WorkflowSpec],
        actor: &boss_core::actor::ActorId,
        now: DateTime<Utc>,
    ) -> Result<boss_jobs::registry::KindReconcileStats, WorkflowError> {
        self.inner.bootstrap_reconcile(specs, actor, now).await
    }
}

#[tokio::test]
async fn malformed_review_gate_envelope_cannot_hide_a_post_release_rehold_from_readback() {
    let armed = Arc::new(std::sync::Mutex::new(None));
    let (app, jobs) = app_with_readback_rehold(
        None,
        Arc::new(std::sync::Mutex::new(None)),
        None,
        Some(armed.clone()),
    );
    let (car, run) = held_car(&jobs, 1500, json!({"releaser":"emp-david"})).await;
    let step = jobs
        .list_steps(&JobId::from_uuid(car))
        .await
        .unwrap()
        .remove(0);
    *armed.lock().unwrap() = Some(step.id);
    let (status, text) = release(&app, &caller("emp-david"), car, ask(run)).await;
    assert!(
        armed.lock().unwrap().is_none(),
        "actual release committed before controlled rehold"
    );
    let md = review_md(&jobs, car).await;
    assert_eq!(
        boss_jobs::stranded::marked(&md, "hold").as_deref(),
        Some("new raw operator hold")
    );
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "readback must report the actual retained hold: {text}"
    );
    assert!(text.contains("new raw operator hold"));
}
