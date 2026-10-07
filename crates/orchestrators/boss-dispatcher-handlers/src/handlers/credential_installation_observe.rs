//! Read-only issuer diagnosis after a native, human-scoped request (99a2649d).
//! The immutable observation is public evidence, never a credential lifecycle phase.
use super::credential_issuer::installation_read::{
    ExpectedInstallation, InstallationReader, InstallationSnapshot, PermissionComparison,
    compare_permissions,
};
use async_trait::async_trait;
use boss_core::job::Subject;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg_string};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, sync::Arc};

#[derive(Deserialize)]
struct Packet {
    id: String,
    kind: String,
    status: String,
    partition: String,
    subject: Subject,
    steps: Vec<Step>,
}
#[derive(Deserialize)]
struct Step {
    id: String,
    spec_slug: String,
    kind: String,
    status: String,
    completed_by: Option<String>,
    metadata: serde_json::Map<String, Value>,
}

pub struct AuthorizedObservation {
    pub expected_account_login: String,
    pub expected_account_id: Option<u64>,
    pub step_id: String,
    pub status: String,
    pub existing: Option<Value>,
}

/// Check the native row before contacting the issuer. The human scope is an
/// intentional authority boundary, not a declaration that actual grants are known.
pub fn authorize_observation(
    body: &Value,
    packet_id: &str,
    scope_id: &str,
    credential: &str,
    rule: &str,
) -> Result<AuthorizedObservation, String> {
    let packet: Packet = serde_json::from_value(body.clone())
        .map_err(|_| "installation request has malformed native consumed fields")?;
    if packet.id != packet_id
        || packet.kind != "inspect-a-github-installation"
        || !matches!(packet.status.as_str(), "open" | "closed")
        || packet.partition != "real"
        || packet.subject.kind != "custom"
        || packet.subject.id != credential
    {
        return Err("installation request identity does not match its scope".into());
    }
    let mut ids = std::collections::BTreeSet::new();
    for step in &packet.steps {
        if uuid::Uuid::parse_str(&step.id).is_err() || !ids.insert(&step.id) {
            return Err("installation request repeats or omits step identity".into());
        }
    }
    let unique = |slug: &str| -> Result<&Step, String> {
        let mut rows = packet.steps.iter().filter(|s| s.spec_slug == slug);
        let step = rows
            .next()
            .ok_or_else(|| format!("installation request lacks {slug}"))?;
        if rows.next().is_some() {
            return Err(format!("installation request repeats {slug}"));
        }
        Ok(step)
    };
    let scope = unique("scope")?;
    if scope.id != scope_id
        || scope.kind != "task"
        || scope.status != "completed"
        || scope.metadata.get("human_only") != Some(&Value::Bool(true))
        || scope.metadata.get("authority_role").and_then(Value::as_str) != Some("platform-admin")
        || scope.metadata.get("credential").and_then(Value::as_str) != Some(credential)
        || !scope
            .completed_by
            .as_deref()
            .is_some_and(|id| id.starts_with("emp-") && id.len() > 4)
    {
        return Err(
            "installation observation requires its completed human platform-admin scope".into(),
        );
    }
    let login = scope
        .metadata
        .get("expected_account_login")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.trim() == *s)
        .ok_or("installation scope lacks an explicit account login")?
        .to_owned();
    let account_id = match scope.metadata.get("expected_account_id") {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            v.as_u64()
                .filter(|v| *v > 0)
                .ok_or("installation scope numeric account assertion is malformed")?,
        ),
    };
    let observe = unique("observe")?;
    if observe.kind != "task"
        || !matches!(observe.status.as_str(), "ready" | "active" | "completed")
        || observe.metadata.get("human_only") == Some(&Value::Bool(true))
        || observe.metadata.get("written_by").and_then(Value::as_str)
            != Some(format!("rule:{rule}").as_str())
    {
        return Err(
            "installation observation has a different native writer or completion contract".into(),
        );
    }
    if packet.status == "closed"
        && (observe.status != "completed" || !observe.metadata.contains_key("observation"))
    {
        return Err("closed installation request has no completed observation to replay".into());
    }
    Ok(AuthorizedObservation {
        expected_account_login: login,
        expected_account_id: account_id,
        step_id: observe.id.clone(),
        status: observe.status.clone(),
        existing: observe.metadata.get("observation").cloned(),
    })
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstallationObservation {
    pub request_id: String,
    pub credential_id: String,
    pub expected: ExpectedInstallation,
    pub required_permissions: BTreeMap<String, String>,
    pub snapshot: InstallationSnapshot,
    pub comparison: PermissionComparison,
}

/// No SecretStore or rotation issuer is present in this adapter's dependencies.
pub struct CredentialObserveInstallation {
    jobs: String,
    client: boss_core::machine_token::Client,
    reader: Arc<dyn InstallationReader>,
}
impl CredentialObserveInstallation {
    pub fn new(jobs: &str, reader: Arc<dyn InstallationReader>) -> Arc<Self> {
        Arc::new(Self {
            jobs: jobs.trim_end_matches('/').into(),
            client: super::common::api_client(),
            reader,
        })
    }
    async fn row(&self, path: &str) -> Result<Value, HandlerError> {
        let response = self
            .client
            .get(format!("{}{path}", self.jobs))
            .header("x-boss-user", super::common::dispatcher_reader_header())
            .header("x-sim-origin", super::common::sim_origin_value())
            .send()
            .await
            .map_err(|_| HandlerError::Downstream("installation native read unavailable".into()))?;
        if response.status() != reqwest::StatusCode::OK {
            return Err(HandlerError::Downstream(format!(
                "installation native GET returned {}",
                response.status()
            )));
        }
        let body = response.json().await.map_err(|_| {
            HandlerError::Downstream("installation native read is unreadable JSON".into())
        })?;
        super::common::row_or_refuse(body, "installation native read")
            .map_err(HandlerError::Permanent)
    }
}

#[async_trait]
impl Handler for CredentialObserveInstallation {
    fn name(&self) -> &'static str {
        "credential.observe.github-installation"
    }
    async fn invoke(
        &self,
        args: &[(String, boss_dispatcher::rules::expr::Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let credential = arg_string(args, "credential_id")?;
        let permissions = super::credential_rotate_github_app::parse_permissions(arg_string(
            args,
            "permissions",
        )?)
        .map_err(HandlerError::Permanent)?;
        if permissions.is_empty() {
            return Err(HandlerError::Permanent(
                "installation comparison declaration is empty".into(),
            ));
        }
        let event = super::common::StepEvent::from_payload(&ctx.event_payload)?;
        if event.kind != "task"
            || event.subject_kind != "custom"
            || event.subject_id != credential
            || uuid::Uuid::parse_str(event.job_id).is_err()
        {
            return Err(HandlerError::Permanent(
                "installation event does not match its declared request".into(),
            ));
        }
        let row = self.row(&format!("/api/jobs/{}", event.job_id)).await?;
        let authorized = authorize_observation(
            &row,
            event.job_id,
            event.step_id,
            credential,
            &ctx.rule_name,
        )
        .map_err(HandlerError::Permanent)?;
        let declared = self.row(&format!("/api/credentials/{credential}")).await?;
        // The kind is the rotate handler's own definition, never a literal
        // here: this line once spelled its own and no registry row matched
        // it (backlog 8dbef2a7). A row of any other kind is still refused.
        if declared.get("id").and_then(Value::as_str) != Some(credential)
            || declared.get("kind").and_then(Value::as_str)
                != Some(super::credential_rotate_github_app::REGISTRY_KIND)
        {
            return Err(HandlerError::Permanent(
                "installation credential declaration does not match the selected kind".into(),
            ));
        }
        let installation_id = match args.iter().find(|(k, _)| k == "installation_id") {
            Some(_) => super::credential_issuer::github_id(
                "installation observation declaration",
                arg_string(args, "installation_id")?,
            )
            .map_err(HandlerError::Permanent)?,
            None => self
                .reader
                .default_installation_id()
                .map_err(HandlerError::Permanent)?,
        };
        let expected = ExpectedInstallation {
            app_id: self.reader.app_id().map_err(HandlerError::Permanent)?,
            installation_id,
            account_login: authorized.expected_account_login,
            account_id: authorized.expected_account_id,
        };
        let observation = if let Some(existing) = authorized.existing {
            let existing: InstallationObservation =
                serde_json::from_value(existing).map_err(|_| {
                    HandlerError::Permanent(
                        "recorded installation observation is malformed; nothing reread".into(),
                    )
                })?;
            // Reuse the owning parser on replay too: typed JSON alone does not
            // establish positive identities or nonempty consumed permission data.
            let snapshot = &existing.snapshot;
            let validated = super::credential_issuer::installation_read::parse_snapshot(
                serde_json::json!({
                    "id": snapshot.installation_id,
                    "app_id": snapshot.app_id,
                    "account": {"id": snapshot.account_id, "login": snapshot.account_login},
                    "permissions": snapshot.permissions,
                    "repository_selection": snapshot.repository_selection,
                    "suspended_at": snapshot.suspended_at,
                }),
                &expected,
                snapshot.observed_at,
            )
            .map_err(|_| {
                HandlerError::Permanent(
                    "recorded installation snapshot is malformed; nothing reread".into(),
                )
            })?;
            if validated != *snapshot {
                return Err(HandlerError::Permanent(
                    "recorded installation snapshot differs from its scope; nothing reread".into(),
                ));
            }
            if existing.request_id != event.job_id
                || existing.credential_id != credential
                || existing.expected != expected
                || existing.required_permissions != permissions
                || existing.comparison != compare_permissions(&existing.snapshot, &permissions)
                || existing.snapshot.http_status != 200
                || existing.snapshot.app_id != expected.app_id
                || existing.snapshot.installation_id != expected.installation_id
                || !existing
                    .snapshot
                    .account_login
                    .eq_ignore_ascii_case(&expected.account_login)
                || existing.snapshot.expected_account_id != expected.account_id
                || expected
                    .account_id
                    .is_some_and(|id| id != existing.snapshot.account_id)
            {
                return Err(HandlerError::Permanent(
                    "recorded installation observation differs from its selection; nothing reread"
                        .into(),
                ));
            }
            existing
        } else {
            if authorized.status == "completed" {
                return Err(HandlerError::Permanent(
                    "completed observation lacks immutable evidence; nothing reread".into(),
                ));
            }
            let snapshot = self
                .reader
                .read_installation(&expected)
                .await
                .map_err(HandlerError::Downstream)?;
            InstallationObservation {
                request_id: event.job_id.into(),
                credential_id: credential.into(),
                expected,
                required_permissions: permissions.clone(),
                comparison: compare_permissions(&snapshot, &permissions),
                snapshot,
            }
        };
        let value = serde_json::to_value(&observation).map_err(|_| {
            HandlerError::Permanent("installation evidence cannot be serialized".into())
        })?;
        // The API owns receipt actor/time/event identity and commits value and receipt
        // together. A conflict is retried by rereading, never by replacing evidence.
        let response = self
            .client
            .post(format!(
                "{}/api/jobs/{}/steps/{}/metadata/records",
                self.jobs, event.job_id, authorized.step_id
            ))
            .header(
                "x-boss-user",
                super::common::dispatcher_actor_header(&ctx.rule_name),
            )
            .header("x-sim-origin", super::common::sim_origin_value())
            .json(&serde_json::json!({"key":"observation","value":value,"expected_absence":true}))
            .send()
            .await
            .map_err(|_| {
                HandlerError::Downstream("installation first record unavailable".into())
            })?;
        if !response.status().is_success() {
            return Err(HandlerError::Downstream(format!(
                "installation first record returned {}",
                response.status()
            )));
        }
        let result: boss_jobs::first_record::FirstRecordResult =
            response.json().await.map_err(|_| {
                HandlerError::Downstream("installation first record receipt is malformed".into())
            })?;
        let receipt = match result {
            boss_jobs::first_record::FirstRecordResult::Recorded(r)
            | boss_jobs::first_record::FirstRecordResult::Replayed(r) => r,
            _ => {
                return Err(HandlerError::Downstream(
                    "installation first record was not committed".into(),
                ));
            }
        };
        if receipt.job_id.to_string() != event.job_id
            || receipt.step_id.to_string() != authorized.step_id
            || receipt.key != "observation"
            || receipt.value != value
            || !receipt.matches(&value)
        {
            return Err(HandlerError::Downstream(
                "installation first record receipt does not match its value".into(),
            ));
        }
        if authorized.status == "completed" {
            return Ok(());
        }
        super::common::write_json(
            &self.client,
            reqwest::Method::PUT,
            &format!(
                "{}/api/jobs/{}/steps/{}",
                self.jobs, event.job_id, authorized.step_id
            ),
            &serde_json::json!({"status":"completed"}),
            &ctx.rule_name,
        )
        .await
    }
}
