//! Read-only decoding of the D1/2a window. Raw amount tokens reach the exact
//! decimal parser before any generic JSON number; original rows/facts remain
//! untouched. Public and newly written quantities still require strings.
#[cfg(feature = "postgres")]
use crate::registry::{StepSpec, WorkflowSpec};
use crate::{pools::PoolSpec, resources::Quantity};
use serde_json::value::{RawValue, to_raw_value};
use std::collections::BTreeMap;

type Object = BTreeMap<String, Box<RawValue>>;

fn object(raw: &str, context: &str) -> Result<Object, String> {
    serde_json::from_str(raw).map_err(|error| {
        format!("historical {context}: malformed or non-finite JSON refused: {error}")
    })
}

fn amount(raw: &RawValue, field: &str) -> Result<Box<RawValue>, String> {
    if raw.get() == "null" {
        return RawValue::from_string("null".into()).map_err(|e| e.to_string());
    }
    let quantity = if raw.get().starts_with('"') {
        serde_json::from_str::<Quantity>(raw.get())
            .map_err(|error| format!("historical {field}: {error}"))?
    } else {
        if raw.get().contains(['e', 'E']) {
            return Err(format!("historical {field}: exponent amount refused"));
        }
        Quantity::parse(raw.get()).map_err(|error| {
            format!(
                "historical {field}: non-finite, malformed or out-of-range amount refused: {error}"
            )
        })?
    };
    to_raw_value(&quantity).map_err(|error| format!("historical {field}: {error}"))
}

// The protocol readers serve only the Pg adapters (registry, postgres,
// flow_resources_pg) since design 7b8ed079 car 2 deleted the membership
// replay, their one reader in a build without `postgres`.
#[cfg(feature = "postgres")]
pub(crate) fn steps(raw: &str) -> Result<Vec<StepSpec>, String> {
    let rows: Vec<Box<RawValue>> =
        serde_json::from_str(raw).map_err(|error| format!("historical workflow steps: {error}"))?;
    rows.into_iter()
        .enumerate()
        .map(|(index, row)| {
            let mut step = object(row.get(), "workflow step")?;
            if let Some(raw) = step.get("resources")
                && raw.get() != "null"
            {
                let mut bill = object(raw.get(), "resource bill")?;
                for key in ["vcpu", "ram_gb", "disk_gb"] {
                    if let Some(raw) = bill.get(key) {
                        bill.insert(
                            key.into(),
                            amount(raw, &format!("steps[{index}].resources.{key}"))?,
                        );
                    }
                }
                step.insert(
                    "resources".into(),
                    to_raw_value(&bill).map_err(|e| e.to_string())?,
                );
            }
            serde_json::from_str(&serde_json::to_string(&step).map_err(|e| e.to_string())?)
                .map_err(|error| format!("historical workflow step: {error}"))
        })
        .collect()
}

#[cfg(feature = "postgres")]
pub(crate) fn workflow(raw: &str) -> Result<WorkflowSpec, String> {
    let mut body = object(raw, "workflow")?;
    let decoded = steps(
        body.get("steps")
            .ok_or("historical workflow missing steps")?
            .get(),
    )?;
    body.insert(
        "steps".into(),
        to_raw_value(&decoded).map_err(|e| e.to_string())?,
    );
    serde_json::from_str(&serde_json::to_string(&body).map_err(|e| e.to_string())?)
        .map_err(|error| format!("historical workflow: {error}"))
}

pub(crate) fn pool(raw: &str) -> Result<PoolSpec, String> {
    let (spec, unknown) = pool_read(raw)?;
    // A row a newer build published may carry a field this build does not
    // know; PoolSpec reads past it (backlog fb893bba), and says so here,
    // because a build that dropped a key without a word is a rollback
    // nobody can see in the log.
    if !unknown.is_empty() {
        tracing::warn!(
            pool = %spec.name,
            version = spec.version,
            keys = %unknown.join(", "),
            "stored resource pool row carries key(s) this build does not know; read without them"
        );
    }
    Ok(spec)
}

/// The stored row read as this build's [`PoolSpec`], and the key paths it
/// was read without.
fn pool_read(raw: &str) -> Result<(PoolSpec, Vec<String>), String> {
    let mut body = object(raw, "resource pool")?;
    body.retain(|key, _| !key.starts_with('_'));
    if let Some(raw) = body.get("capacity") {
        let mut capacity = object(raw.get(), "pool capacity")?;
        if let Some(raw) = capacity.get("amount") {
            capacity.insert("amount".into(), amount(raw, "capacity.amount")?);
        }
        body.insert(
            "capacity".into(),
            to_raw_value(&capacity).map_err(|e| e.to_string())?,
        );
    }
    let text = serde_json::to_string(&body).map_err(|e| e.to_string())?;
    let spec: PoolSpec = serde_json::from_str(&text)
        .map_err(|error| format!("historical resource pool: {error}"))?;
    // Judged key by key, so one value the generic reader cannot hold
    // (`1e400`, past f64) hides no key but itself, and is itself named
    // (backlog f043107b): a key this build does not read is read past
    // whatever its value; a key it reads whose value cannot be re-read had
    // some part of it read past.
    let unknown = match serde_json::to_value(&spec) {
        Ok(read) => body
            .iter()
            .flat_map(
                |(key, raw)| match serde_json::from_str::<serde_json::Value>(raw.get()) {
                    Ok(value) => {
                        let given =
                            serde_json::Value::Object([(key.clone(), value)].into_iter().collect());
                        crate::pools::unknown_keys(&given, &read)
                    }
                    Err(_) => vec![key.clone()],
                },
            )
            .collect(),
        Err(_) => body.keys().cloned().collect(),
    };
    Ok((spec, unknown))
}

#[cfg(test)]
mod tests {
    use super::pool_read;

    fn row(extra: &str) -> String {
        format!(
            r#"{{"name":"robots:opus","version":1,"status":"active","unit":"slot","kind":"flow","capacity":{{"state":"unknown"}},"models":["opus-5-5"]{extra}}}"#
        )
    }

    #[test]
    fn a_known_row_is_read_past_nothing() {
        let (spec, unknown) = pool_read(&row("")).expect("a known row reads");
        assert_eq!(spec.name, "robots:opus");
        assert_eq!(unknown, Vec::<String>::new());
    }

    /// The car's promise (read past, warn, never refuse; backlog
    /// fb893bba) holds for an ordinary unknown key.
    #[test]
    fn an_unknown_key_is_read_past_and_named() {
        let (_, unknown) =
            pool_read(&row(r#","future_knob":{"on":true}"#)).expect("an unknown key is read past");
        assert_eq!(unknown, vec!["future_knob".to_string()]);
    }

    /// And for one whose value the generic JSON reader cannot hold
    /// (backlog f043107b; Codex landed-review 056ee1af R1 P2 on car
    /// c765b200): `1e400` is past f64, so the whole-row re-read failed and
    /// the key was read past with no word.
    #[test]
    fn an_unknown_key_no_generic_reader_can_hold_is_named_too() {
        let (_, unknown) = pool_read(&row(r#","future_knob":1e400"#)).expect("the row still reads");
        assert_eq!(unknown, vec!["future_knob".to_string()]);
        let (_, unknown) =
            pool_read(&row(r#","future_knob":{"limit":1e400}"#)).expect("the row still reads");
        assert_eq!(unknown, vec!["future_knob".to_string()]);
    }
}
