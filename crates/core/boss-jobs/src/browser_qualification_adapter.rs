//! Infrastructure composes immutable job records, the actor registry and content reads.
use crate::{first_record::FirstRecord, port::JobsRepository, registry::WorkflowRegistry};
use anyhow::{Context, Result, ensure};
use async_trait::async_trait;
use boss_core::{
    actor::ActorId,
    job::{Job, JobId, Step, StepId},
};
use serde_json::{Value, json};
use std::sync::Arc;

#[async_trait]
pub trait ObservationFiles: Send + Sync {
    /// Return the complete target listing and each complete bounded attachment.
    async fn verify(&self, record: &FirstRecord, viewer: &boss_policy_client::User) -> Result<()>;
}

pub struct BrowserQualification<R: JobsRepository> {
    pub jobs: Arc<R>,
    pub workflows: Arc<dyn WorkflowRegistry>,
    pub agents: Arc<dyn crate::agents::AgentsRegistry>,
    pub files: Arc<dyn ObservationFiles>,
    pub accepting: bool,
}

impl<R: JobsRepository> BrowserQualification<R> {
    async fn packet(&self, id: JobId) -> Result<Value> {
        let job = self
            .jobs
            .get_job(&id)
            .await?
            .context("evidence packet missing")?;
        let mut value = serde_json::to_value(job)?;
        value["steps"] = serde_json::to_value(self.jobs.list_steps(&id).await?)?;
        Ok(value)
    }
    async fn original(&self, packet: &Value, slug: &str, key: &str) -> Result<FirstRecord> {
        let step = crate::browser_qualification::step(packet, slug)?;
        let id: StepId = serde_json::from_value(step["id"].clone())?;
        self.jobs
            .first_step_record(&id, key)
            .await?
            .context("immutable evidence original missing")
    }
    async fn independent(&self, observer: &ActorId, reviewer: &ActorId) -> Result<String> {
        let rows = self.agents.list().await?;
        let rows = serde_json::to_value(rows)?;
        let rows = rows
            .as_array()
            .context("actor registry is not a complete list")?;
        let a = crate::browser_qualification::canonical_actor(rows, &observer.to_string())?;
        let b = crate::browser_qualification::canonical_actor(rows, &reviewer.to_string())?;
        ensure!(a != b, "observer cannot qualify through an alias");
        Ok(b)
    }
}

#[async_trait]
impl<R: JobsRepository + 'static> crate::completion_contract::CompletionContract
    for BrowserQualification<R>
{
    async fn validate(
        &self,
        job: &Job,
        step: &Step,
        user: &boss_policy_client::User,
    ) -> Result<()> {
        let actor = user
            .ambient_actor()
            .context("authenticated qualification actor missing")?;
        let (packet, car, accepted) = if self.accepting {
            let accepted = self
                .jobs
                .first_step_record(&step.id, "attended_qualification")
                .await?
                .context("immutable acceptance missing")?;
            ensure!(
                accepted.actor == actor,
                "acceptance must be completed by its authenticated recorder"
            );
            let id: JobId = serde_json::from_value(accepted.value["observation"].clone())?;
            (
                self.packet(id).await?,
                self.packet(job.id).await?,
                Some(accepted),
            )
        } else {
            let packet = self.packet(job.id).await?;
            let id: JobId =
                serde_json::from_value(crate::browser_qualification::contract()?["car"].clone())?;
            (packet, self.packet(id).await?, None)
        };
        let original = self.original(&packet, "observe", "observation").await?;
        let qualified = self.original(&packet, "qualify", "qualification").await?;
        let reviewer = self.independent(&original.actor, &qualified.actor).await?;
        if !self.accepting {
            ensure!(
                qualified.actor == actor,
                "qualification must be completed by its authenticated recorder"
            );
        }
        let expected = crate::browser_qualification::judge(
            &original,
            &packet,
            &car,
            &reviewer,
            crate::browser_qualification::text(&qualified.value, "findings")?,
        )?;
        let sid: uuid::Uuid = serde_json::from_value(
            crate::browser_qualification::step(&packet, "qualify")?["id"].clone(),
        )?;
        let pid: uuid::Uuid = serde_json::from_value(packet["id"].clone())?;
        crate::car_release::immutable_record_resource(&qualified, pid, sid, "qualification")?;
        ensure!(qualified.value==expected && qualified.matches(&crate::browser_qualification::step(&packet,"qualify")?["metadata"]["qualification"]),"independent qualification differs from original bindings");
        if let Some(accepted) = accepted {
            let published = serde_json::to_value(
                self.workflows
                    .get_version(&job.kind, job.workflow_version)
                    .await?,
            )?;
            let expected = crate::browser_qualification::acceptance(
                &car, &published, &packet, &original, &qualified,
            )?;
            ensure!(
                accepted.value == expected
                    && accepted.matches(&step.metadata["attended_qualification"]),
                "acceptance differs from immutable originals"
            );
            crate::car_release::immutable_record_resource(
                &accepted,
                *job.id.inner().as_uuid(),
                *step.id.inner().as_uuid(),
                "attended_qualification",
            )?;
        }
        self.files.verify(&original, user).await?;
        Ok(())
    }
}

/// Bounded, non-redirecting content reader; the authenticated caller remains the viewer.
pub struct HttpObservationFiles {
    base: String,
    http: boss_core::machine_token::Client,
}
impl HttpObservationFiles {
    pub fn with_client(base: String, http: boss_core::machine_token::Client) -> Self {
        Self {
            base: base.trim_end_matches('/').into(),
            http,
        }
    }
    pub fn new(base: String) -> Result<Self> {
        let http = boss_core::machine_token::Client::build(
            reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(5))
                .timeout(std::time::Duration::from_secs(15))
                .redirect(reqwest::redirect::Policy::none()),
        )?;
        Ok(Self::with_client(base, http))
    }
    async fn read(
        &self,
        path: &str,
        user: &boss_policy_client::User,
        limit: usize,
    ) -> Result<Vec<u8>> {
        let mut response = self
            .http
            .get(format!("{}{path}", self.base))
            .header("x-boss-user", serde_json::to_string(user)?)
            .send()
            .await?;
        ensure!(
            response.status() == reqwest::StatusCode::OK,
            "content read did not return HTTP 200"
        );
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            ensure!(
                bytes.len().saturating_add(chunk.len()) <= limit,
                "content read exceeds byte bound"
            );
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }
}
#[async_trait]
impl ObservationFiles for HttpObservationFiles {
    async fn verify(&self, record: &FirstRecord, user: &boss_policy_client::User) -> Result<()> {
        use sha2::{Digest, Sha256};
        let packet = record.job_id.to_string();
        let listing = self
            .read(
                &format!("/api/files?target_kind=job&target_id={packet}"),
                user,
                262144,
            )
            .await?;
        let listing: Value = serde_json::from_slice(&listing)?;
        let files = listing
            .as_array()
            .context("content did not return complete attachment listing")?;
        for (key, binding) in [
            ("asset", &record.value["asset"]["file"]),
            ("suite", &record.value["suite"]["file"]),
            ("report", &record.value["report"]),
        ] {
            let id = crate::browser_qualification::text(binding, "id")?;
            ensure!(
                !uuid::Uuid::parse_str(id)?.is_nil(),
                "invalid attachment id"
            );
            let matches = files.iter().filter(|f| f["id"] == id).collect::<Vec<_>>();
            ensure!(matches.len() == 1, "attachment missing or ambiguous");
            let file = matches[0];
            ensure!(
                file["target"] == json!({"kind":"job","id":packet})
                    && file["deleted_at"].is_null()
                    && file["sha256"] == binding["sha256"]
                    && file["size_bytes"] == binding["bytes"],
                "attachment target or metadata differs"
            );
            let bytes = self
                .read(
                    &format!("/api/files/{id}"),
                    user,
                    if key == "report" { 65536 } else { 262144 },
                )
                .await?;
            ensure!(
                binding["bytes"].as_u64() == Some(bytes.len() as u64)
                    && binding["sha256"] == hex::encode(Sha256::digest(&bytes)),
                "attachment bytes differ"
            );
            if key == "report" {
                ensure!(
                    serde_json::from_slice::<Value>(&bytes)? == record.value["output"],
                    "full report differs from immutable output"
                );
            }
        }
        Ok(())
    }
}
