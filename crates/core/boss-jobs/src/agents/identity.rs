//! Canonical actor identity from one authoritative registry snapshot.
use anyhow::{Result, ensure};
use serde_json::Value;

pub fn canonical_actor(agents: &[Value], actor: &str) -> Result<String> {
    let matches = |row: &&Value| {
        row["id"] == actor
            || row["aliases"]
                .as_array()
                .is_some_and(|aliases| aliases.iter().any(|alias| alias == actor))
    };
    let mut rows = agents.iter().filter(matches);
    let found = rows.next();
    ensure!(rows.next().is_none(), "actor alias is ambiguous");
    Ok(found
        .and_then(|row| row["id"].as_str())
        .unwrap_or(actor)
        .to_owned())
}
