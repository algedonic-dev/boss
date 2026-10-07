//! Public installation observations; no token or credential lifecycle operation.
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedInstallation {
    pub app_id: u64,
    pub installation_id: u64,
    pub account_login: String,
    pub account_id: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallationSnapshot {
    /// This parser's input is only the successful installation GET200 body.
    pub http_status: u16,
    pub app_id: u64,
    pub installation_id: u64,
    pub account_login: String,
    pub account_id: u64,
    pub expected_account_id: Option<u64>,
    pub permissions: BTreeMap<String, String>,
    pub repository_selection: String,
    pub suspended_at: Option<DateTime<Utc>>,
    pub observed_at: DateTime<Utc>,
}

#[async_trait]
pub trait InstallationReader: Send + Sync {
    fn app_id(&self) -> Result<u64, String>;
    fn default_installation_id(&self) -> Result<u64, String>;
    async fn read_installation(
        &self,
        expected: &ExpectedInstallation,
    ) -> Result<InstallationSnapshot, String>;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionComparison {
    pub missing: Vec<String>,
    pub insufficient: Vec<String>,
    pub unordered: Vec<String>,
}

pub fn compare_permissions(
    snapshot: &InstallationSnapshot,
    required: &BTreeMap<String, String>,
) -> PermissionComparison {
    required.iter().fold(
        PermissionComparison {
            missing: Vec::new(),
            insufficient: Vec::new(),
            unordered: Vec::new(),
        },
        |mut result, (permission, requested)| {
            match snapshot.permissions.get(permission) {
                None => result.missing.push(permission.clone()),
                Some(actual)
                    if actual == requested || (actual == "write" && requested == "read") => {}
                Some(actual) if actual == "read" && requested == "write" => {
                    result.insufficient.push(permission.clone())
                }
                Some(_) => result.unordered.push(permission.clone()),
            }
            result
        },
    )
}

fn identifier(value: &Value, key: &str) -> Result<u64, String> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .filter(|n| *n > 0)
        .ok_or_else(|| format!("installation observation lacks a positive {key}"))
}

fn text(value: &Value, key: &str) -> Result<String, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.trim() == *s)
        .map(str::to_owned)
        .ok_or_else(|| format!("installation observation lacks a nonempty {key}"))
}

pub fn parse_snapshot(
    body: Value,
    expected: &ExpectedInstallation,
    observed_at: DateTime<Utc>,
) -> Result<InstallationSnapshot, String> {
    if expected.app_id == 0
        || expected.installation_id == 0
        || expected.account_login.is_empty()
        || expected.account_login.trim() != expected.account_login
        || expected.account_id == Some(0)
    {
        return Err("installation expectation is incomplete".into());
    }
    let app_id = identifier(&body, "app_id")?;
    let installation_id = identifier(&body, "id")?;
    let account = body
        .get("account")
        .filter(|v| v.is_object())
        .ok_or("installation observation lacks account")?;
    let account_id = identifier(account, "id")?;
    let account_login = text(account, "login")?;
    if app_id != expected.app_id
        || installation_id != expected.installation_id
        || !account_login.eq_ignore_ascii_case(&expected.account_login)
        || expected.account_id.is_some_and(|id| id != account_id)
    {
        return Err("installation observation differs from expected identity".into());
    }
    let permissions = body
        .get("permissions")
        .and_then(Value::as_object)
        .ok_or("installation observation lacks a permissions object")?
        .iter()
        .map(|(name, level)| {
            if name.is_empty() || name.trim() != name {
                return Err("installation permission name is malformed".to_string());
            }
            let level = level
                .as_str()
                .filter(|s| !s.is_empty() && s.trim() == *s)
                .ok_or("installation permission level is malformed")?;
            Ok((name.clone(), level.to_owned()))
        })
        .collect::<Result<BTreeMap<_, _>, String>>()?;
    let repository_selection = text(&body, "repository_selection")?;
    let suspended_at = match body.get("suspended_at") {
        Some(Value::Null) => None,
        Some(Value::String(raw)) => Some(
            DateTime::parse_from_rfc3339(raw)
                .map_err(|_| "installation suspension date is malformed")?
                .with_timezone(&Utc),
        ),
        _ => return Err("installation observation lacks explicit suspension state".into()),
    };
    Ok(InstallationSnapshot {
        http_status: 200,
        app_id,
        installation_id,
        account_login,
        account_id,
        expected_account_id: expected.account_id,
        permissions,
        repository_selection,
        suspended_at,
        observed_at,
    })
}
