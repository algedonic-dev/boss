//! The human ask is protocol data, carried unchanged onto admitted steps.
//! The evidence binding names an existing UI destination rather than an
//! interpolated URL or a command supplied by a filer (bf47083d).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HumanPresentation {
    pub ask: String,
    pub why_person: String,
    /// A label for opening the existing step surface; never executable text.
    pub action: String,
    pub after: String,
    pub evidence: EvidenceBinding,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EvidenceBinding {
    CurrentStep,
}

impl HumanPresentation {
    pub fn read(metadata: &serde_json::Value) -> Result<Self, String> {
        let presentation: Self = serde_json::from_value(
            metadata
                .get("human_presentation")
                .cloned()
                .unwrap_or_default(),
        )
        .map_err(|why| format!("human_presentation: {why}"))?;
        for (field, value) in [
            ("ask", &presentation.ask),
            ("why_person", &presentation.why_person),
            ("action", &presentation.action),
            ("after", &presentation.after),
        ] {
            if value.trim().is_empty() {
                return Err(format!("human_presentation.{field} must be nonempty"));
            }
        }
        Ok(presentation)
    }
}
