//! HTTP adapter for the registry role port (approved abf9eeae car 2).
//! Three existing authenticated read doors, never an anonymous shortcut:
//! agents with aliases, automation rows, and the active people roster.
//! A complete answer is required; errors differ from an unregistered id.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use boss_core::machine_gate::{Mode, ModeSwitch};
use boss_core::machine_token::{self, Source};
use boss_core::role_of_record::{RoleLookupError, RoleOfRecord, RoleRecord};
use serde_json::Value;

use crate::User;
use crate::role_reporting::{ReportMode, ReportModeSource};

pub const ROLE_MODE_FILE_ENV: &str = "BOSS_ACTOR_ROLE_MODE_FILE";
pub const ROLE_MODE_FILE: &str = "/etc/boss/machine-gate/actor-role";

/// Uses the machine gate's file reader and cadence. This precursor never
/// enforces: the later H car must add that behavior after its clean window.
pub struct MountedReportMode {
    switch: Arc<ModeSwitch>,
}

impl MountedReportMode {
    pub fn mount() -> Self {
        Self {
            switch: ModeSwitch::mount("actor role", ROLE_MODE_FILE_ENV, ROLE_MODE_FILE),
        }
    }

    pub fn over(switch: Arc<ModeSwitch>) -> Self {
        Self { switch }
    }
}

impl ReportModeSource for MountedReportMode {
    fn mode(&self) -> ReportMode {
        match self.switch.mode() {
            Mode::Off => ReportMode::Off,
            Mode::Report => ReportMode::Report,
            Mode::Enforce => {
                tracing::warn!(file = %self.switch.path().display(), "actor-role enforce is not implemented by this report-only precursor; report behavior retained");
                ReportMode::Report
            }
        }
    }
}

const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

pub struct HttpRoleReader {
    jobs: String,
    people: String,
    http: machine_token::Client,
}

impl HttpRoleReader {
    pub fn new(jobs: String, people: String, reader: User) -> Result<Self, RoleLookupError> {
        Self::with_source(
            jobs,
            people,
            reader,
            Duration::from_secs(2),
            boss_core::machine_token::shared(),
        )
    }

    pub fn with_source(
        jobs: String,
        people: String,
        reader: User,
        timeout: Duration,
        source: Arc<Source>,
    ) -> Result<Self, RoleLookupError> {
        let signed = serde_json::to_string(&reader)
            .map_err(|e| RoleLookupError::Unavailable(e.to_string()))?;
        let header = reqwest::header::HeaderValue::from_str(&signed)
            .map_err(|e| RoleLookupError::Unavailable(e.to_string()))?;
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("x-boss-user", header);
        let http = machine_token::Client::build_with_source(
            reqwest::Client::builder()
                .timeout(timeout)
                .default_headers(headers),
            source,
        )
        .map_err(|e| RoleLookupError::Unavailable(e.to_string()))?;
        Ok(Self {
            jobs: jobs.trim_end_matches('/').into(),
            people: people.trim_end_matches('/').into(),
            http,
        })
    }

    async fn get(&self, base: &str, path: &str) -> Result<Value, RoleLookupError> {
        let url = format!("{base}{path}");
        let mut response = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| RoleLookupError::Unavailable(format!("GET {path}: {e}")))?;
        if !response.status().is_success() {
            return Err(RoleLookupError::Unavailable(format!(
                "GET {path}: {}",
                response.status()
            )));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| RoleLookupError::Unavailable(e.to_string()))?
        {
            if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                return Err(RoleLookupError::Unavailable(format!(
                    "GET {path}: response exceeds {MAX_RESPONSE_BYTES} bytes"
                )));
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes)
            .map_err(|e| RoleLookupError::Unavailable(format!("GET {path}: malformed JSON: {e}")))
    }
}

fn complete_rows(
    value: Value,
    path: &str,
    bare_array: bool,
) -> Result<Vec<Value>, RoleLookupError> {
    if bare_array && let Value::Array(rows) = value {
        return Ok(rows);
    }
    let rows = value.get("data").and_then(Value::as_array);
    let total = value.get("total").and_then(Value::as_u64);
    match (rows, total) {
        (Some(rows), Some(total)) if total == rows.len() as u64 => Ok(rows.clone()),
        _ => Err(RoleLookupError::Unavailable(format!(
            "GET {path}: missing or incomplete row list"
        ))),
    }
}

fn matches_actor(
    row: &Value,
    actor: &str,
    aliases: bool,
    people: bool,
) -> Result<bool, RoleLookupError> {
    let id = row
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| RoleLookupError::Unavailable("registry row has no id".into()))?;
    if people {
        let status = row
            .get("status")
            .and_then(Value::as_str)
            .ok_or_else(|| RoleLookupError::Unavailable("people row has no status".into()))?;
        if status != "active" {
            return Ok(false);
        }
    }
    let mut matches = id == actor;
    if aliases {
        let aliases = row
            .get("aliases")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                RoleLookupError::Unavailable("agent registry row has no aliases array".into())
            })?;
        for alias in aliases {
            let alias = alias.as_str().ok_or_else(|| {
                RoleLookupError::Unavailable("agent alias is not a string".into())
            })?;
            matches |= alias == actor;
        }
    }
    if people {
        matches |= row
            .get("email")
            .and_then(Value::as_str)
            .is_some_and(|email| email.eq_ignore_ascii_case(actor));
    }
    Ok(matches)
}

fn record(row: &Value) -> Result<RoleRecord, RoleLookupError> {
    let role = match row.get("role") {
        Some(Value::Null) => None,
        Some(Value::String(role)) => Some(role.clone()),
        _ => {
            return Err(RoleLookupError::Unavailable(
                "registry row has no role field or a malformed role".into(),
            ));
        }
    };
    let actor_id = row
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| RoleLookupError::Unavailable("registry row has no id".into()))?
        .into();
    Ok(RoleRecord { actor_id, role })
}

#[async_trait]
impl RoleOfRecord for HttpRoleReader {
    async fn role_for(&self, actor: &str) -> Result<Option<RoleRecord>, RoleLookupError> {
        // Finite independent GETs. This reader never calls policy itself;
        // registry doors authorize its named service using their own ports.
        let (agents, automations, people) = tokio::try_join!(
            self.get(&self.jobs, "/api/agents"),
            self.get(&self.jobs, "/api/agents/automations"),
            self.get(&self.people, "/api/people?status=active"),
        )?;
        let mut found = Vec::new();
        for (body, path, aliases, people) in [
            (agents, "/api/agents", true, false),
            (automations, "/api/agents/automations", false, false),
            (people, "/api/people", false, true),
        ] {
            for row in complete_rows(body, path, people)? {
                if matches_actor(&row, actor, aliases, people)? {
                    found.push(record(&row)?);
                }
            }
        }
        if found.len() > 1 {
            return Err(RoleLookupError::Ambiguous(format!(
                "{} registry rows match {actor}",
                found.len()
            )));
        }
        Ok(found.pop())
    }
}
