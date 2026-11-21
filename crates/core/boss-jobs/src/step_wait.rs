//! The existing structured STEP wait declaration, separate from the scalar JOB
//! waiting_on edge. Readers share its grammar; operational wait bounds stay with
//! the observer, never with the claims projection.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Target {
    Person,
    Packet,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Declaration {
    pub on: Target,
    pub id: String,
    pub since: DateTime<Utc>,
    pub why: String,
}

impl Declaration {
    pub fn parse(value: &Value) -> Option<Self> {
        let wait: Self = serde_json::from_value(value.clone()).ok()?;
        (!wait.id.trim().is_empty() && !wait.why.trim().is_empty()).then_some(wait)
    }
}
