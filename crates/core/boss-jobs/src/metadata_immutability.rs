//! Generic first-write bindings. A conditional command may bind fields;
//! every later write preserves both the binding and its original value.
use serde_json::{Map, Value};
pub const KEY: &str = "_immutable_metadata";
pub const GUARDED_KEY: &str = "_guarded_metadata";

fn guarded_fields(metadata: &Value) -> Result<Vec<String>, String> {
    let Some(record) = metadata.get(GUARDED_KEY) else {
        return Ok(vec![]);
    };
    if record.get("contract_version").and_then(Value::as_u64) != Some(1)
        || record.as_object().is_none_or(|object| object.len() != 2)
    {
        return Err("guarded metadata binding version or shape is unreadable".into());
    }
    let fields = record
        .get("fields")
        .and_then(Value::as_array)
        .ok_or("guarded metadata fields are unreadable")?;
    let mut result = Vec::new();
    for field in fields {
        let field = field
            .as_str()
            .filter(|key| !key.is_empty() && *key != KEY && *key != GUARDED_KEY)
            .ok_or("invalid guarded metadata field")?;
        if result.iter().any(|old| old == field) {
            return Err("duplicate guarded metadata field".into());
        }
        result.push(field.to_owned());
    }
    if result.is_empty() {
        return Err("empty guarded metadata binding".into());
    }
    Ok(result)
}

fn preserve_guarded(
    before: &Value,
    after: &Value,
    command: Option<&crate::conditional_metadata::Command>,
) -> Result<(), String> {
    let old = guarded_fields(before)?;
    let new = guarded_fields(after)?;
    if old.iter().any(|key| !new.contains(key)) {
        return Err("guarded metadata binding cannot be removed".into());
    }
    for key in &new {
        if !old.contains(key) && !command.is_some_and(|command| command.guarded_keys.contains(key))
        {
            return Err(
                "guarded metadata binding is written only by its conditional first record".into(),
            );
        }
        let previous = before.get(key);
        let next = after
            .get(key)
            .filter(|value| !value.is_null())
            .ok_or_else(|| format!("guarded field {key} cannot be absent or null"))?;
        if previous != Some(next)
            && !command.is_some_and(|command| {
                command.contract_version == 2
                    && command.expected.get(key).is_some_and(|expected| {
                        expected.present == previous.is_some()
                            && (!expected.present || previous == Some(&expected.value))
                    })
                    && command.patch.get(key) == Some(next)
            })
        {
            return Err(format!(
                "guarded field {key} requires its complete conditional predicate and replacement"
            ));
        }
    }
    if command.is_none() && before.get(GUARDED_KEY) != after.get(GUARDED_KEY) {
        return Err("guarded metadata binding is server-owned".into());
    }
    Ok(())
}

pub fn apply_guarded(
    before: &Value,
    after: &mut Value,
    command: &crate::conditional_metadata::Command,
) -> Result<(), String> {
    let mut fields = guarded_fields(before)?;
    for key in &command.guarded_keys {
        if fields.contains(key) {
            return Err(format!("{key} already has a guarded first binding"));
        }
        fields.push(key.clone());
    }
    if !fields.is_empty() {
        after
            .as_object_mut()
            .ok_or("metadata is not an object")?
            .insert(
                GUARDED_KEY.into(),
                serde_json::json!({"contract_version":1,"fields":fields}),
            );
    }
    preserve_guarded(before, after, Some(command))
}

/// Protocol declarations are pinned with the packet. First insertion
/// requires the conditional door to bind the same field atomically.
pub fn validate_protocol(
    schema: &Value,
    before: &Value,
    after: &Value,
    command: Option<&crate::conditional_metadata::Command>,
) -> Result<(), String> {
    // `required_values` (backlog 97170186): exact top-level values the
    // pinned protocol's packets carry from admission on — the markers
    // an executor's queue selects on. Workflow-level `metadata` only
    // describes; this is the declaration admission enforces. A
    // recovery assessment admitted without its markers sat invisible
    // to the conductor until a hand PATCH (28e96fbc, 2026-10-16). Every
    // write is checked, not only the first, so no later write can
    // make a routed packet unroutable either.
    if let Some(required) = schema.get("required_values") {
        let required = required
            .as_object()
            .ok_or("required_values must be an object of field values")?;
        for (key, value) in required {
            if key.is_empty() || key == KEY || key == GUARDED_KEY {
                return Err("required_values contains an invalid field name".into());
            }
            if after.get(key) != Some(value) {
                return Err(format!(
                    "{key} must be {value} under the packet's pinned protocol"
                ));
            }
        }
    }
    let declarations = match schema.get("guarded_fields") {
        None => vec![],
        Some(fields) => {
            let mut names = Vec::new();
            for field in fields.as_array().ok_or("guarded_fields must be an array")? {
                let key = field
                    .as_str()
                    .filter(|key| !key.is_empty() && *key != KEY && *key != GUARDED_KEY)
                    .ok_or("guarded_fields contains an invalid name")?;
                if names.contains(&key) {
                    return Err("guarded_fields contains a duplicate".into());
                }
                names.push(key);
            }
            names
        }
    };
    if command.is_some_and(|command| {
        command
            .guarded_keys
            .iter()
            .any(|key| !declarations.contains(&key.as_str()))
    }) {
        return Err(
            "guarded first bindings require a field declared by the pinned protocol".into(),
        );
    }
    let already = guarded_fields(before)?;
    for key in declarations {
        if before.get(key) != after.get(key)
            && !already.iter().any(|bound| bound == key)
            && !command.is_some_and(|command| command.guarded_keys.iter().any(|bound| bound == key))
        {
            return Err(format!(
                "{key} must enter through its guarded conditional first record"
            ));
        }
    }
    if let Some(fields) = schema.get("conditional_fields") {
        for field in fields
            .as_array()
            .ok_or("conditional_fields must be an array of field names")?
        {
            let key = field
                .as_str()
                .filter(|key| !key.is_empty() && *key != KEY)
                .ok_or("conditional_fields contains an invalid field name")?;
            let old = before.get(key);
            let next = after.get(key);
            if old == next {
                continue;
            }
            let guarded = command.is_some_and(|command| {
                command.expected.get(key).is_some_and(|expected| {
                    expected.present == old.is_some()
                        && (!expected.present || old == Some(&expected.value))
                }) && command.patch.get(key) == next
            });
            if next.is_none_or(Value::is_null) || !guarded {
                return Err(format!(
                    "{key} requires an exact conditional guard and a retained non-null record"
                ));
            }
        }
    }
    let fields: &[Value] = match schema.get("immutable_fields") {
        Some(declared) => declared
            .as_array()
            .ok_or("immutable_fields must be an array of field names")?,
        None => &[],
    };
    if command.is_some_and(|command| {
        command.immutable_keys.iter().any(|key| {
            !fields
                .iter()
                .any(|field| field.as_str() == Some(key.as_str()))
        })
    }) {
        return Err(
            "immutable bindings require a field declared by the packet's pinned protocol".into(),
        );
    }
    for field in fields {
        let key = field
            .as_str()
            .filter(|key| !key.is_empty() && *key != KEY)
            .ok_or("immutable_fields contains an invalid field name")?;
        let old = before.get(key);
        let next = after.get(key);
        if old == next {
            continue;
        }
        if old.is_some() {
            return Err(format!(
                "{key} is immutable under the packet's pinned protocol"
            ));
        }
        if next.is_some()
            && !command.is_some_and(|command| {
                command.immutable_keys.iter().any(|field| field == key)
                    && command
                        .expected
                        .get(key)
                        .is_some_and(|expected| !expected.present)
            })
        {
            return Err(format!(
                "{key} must be first recorded through the conditional metadata door with its immutable binding"
            ));
        }
    }
    Ok(())
}

pub(crate) fn bindings(metadata: &Value) -> Result<Map<String, Value>, String> {
    match metadata.get(KEY) {
        None => Ok(Map::new()),
        Some(Value::Object(fields)) if fields.keys().all(|key| !key.is_empty() && key != KEY) => {
            Ok(fields.clone())
        }
        _ => Err("first-write binding record is unreadable".into()),
    }
}

pub fn preserve(before: &Value, after: &Value, allow_bind: bool) -> Result<(), String> {
    preserve_guarded(before, after, None)?;
    preserve_immutable(before, after, allow_bind)
}

#[cfg(feature = "postgres")]
pub(crate) fn preserve_replayed(
    before: &Value,
    after: &Value,
    command: Option<&crate::conditional_metadata::Command>,
) -> Result<(), String> {
    preserve_guarded(before, after, command)?;
    preserve_immutable(before, after, true)
}

fn preserve_immutable(before: &Value, after: &Value, allow_bind: bool) -> Result<(), String> {
    let old = bindings(before)?;
    let new = bindings(after)?;
    if !allow_bind && before.get(KEY) != after.get(KEY) {
        return Err(format!(
            "{KEY} is written only by a conditional first-write binding"
        ));
    }
    for (key, value) in &old {
        if new.get(key) != Some(value) || after.get(key) != Some(value) {
            return Err(format!(
                "{key} was recorded once and cannot be replaced or deleted"
            ));
        }
    }
    for (key, value) in &new {
        if after.get(key) != Some(value) {
            return Err(format!("{key} disagrees with its immutable first value"));
        }
    }
    Ok(())
}

pub fn bind(before: &Value, after: &mut Value, keys: &[String]) -> Result<(), String> {
    let mut fields = bindings(before)?;
    for key in keys {
        if key.is_empty() || key == KEY || key == GUARDED_KEY {
            return Err("invalid immutable metadata field name".into());
        }
        let value = after
            .get(key)
            .cloned()
            .ok_or_else(|| format!("{key} cannot be bound while absent"))?;
        if fields.get(key).is_some_and(|old| old != &value) {
            return Err(format!("{key} already has a different first value"));
        }
        fields.insert(key.clone(), value);
    }
    if !fields.is_empty() {
        after
            .as_object_mut()
            .ok_or("metadata is not an object")?
            .insert(KEY.into(), Value::Object(fields));
    }
    preserve_immutable(before, after, true)
}

/// Select a declared immutable source object without creating another
/// fact or write contract. Absence retains legacy interpretation;
/// malformed/unbound presence never falls back to mutable copies.
pub fn view<'a>(
    metadata: &'a Value,
    envelope: &str,
    pointer: &str,
    version: u32,
    equal: &[&str],
) -> Result<&'a Value, String> {
    let Some(value) = metadata.get(envelope) else {
        return Ok(metadata);
    };
    if value.get("contract_version").and_then(Value::as_u64) != Some(u64::from(version)) {
        return Err(format!(
            "{envelope} has an unsupported immutable source version"
        ));
    }
    if bindings(metadata)?.get(envelope) != Some(value) {
        return Err(format!("{envelope} lacks its exact immutable binding"));
    }
    let source = value
        .pointer(pointer)
        .filter(|source| source.is_object())
        .ok_or_else(|| format!("{envelope}{pointer} is not a complete source object"))?;
    for key in equal {
        if metadata.get(*key) != source.get(*key) {
            return Err(format!(
                "derived index locator {key} disagrees with {envelope}{pointer}"
            ));
        }
    }
    Ok(source)
}

pub fn patch(before: &Value, patch: &Map<String, Value>) -> Result<(), String> {
    if patch.contains_key(KEY) || patch.contains_key(GUARDED_KEY) {
        return Err(format!(
            "{KEY} is written only by a conditional first-write binding"
        ));
    }
    let mut after = before.as_object().cloned().unwrap_or_default();
    for (key, value) in patch {
        if value.is_null() {
            after.remove(key);
        } else {
            after.insert(key.clone(), value.clone());
        }
    }
    preserve(before, &Value::Object(after), false)
}

pub fn validate_protocol_patch(
    schema: &Value,
    before: &Value,
    patch: &Map<String, Value>,
) -> Result<(), String> {
    let mut after = before.as_object().cloned().unwrap_or_default();
    for (key, value) in patch {
        if value.is_null() {
            after.remove(key);
        } else {
            after.insert(key.clone(), value.clone());
        }
    }
    validate_protocol(schema, before, &Value::Object(after), None)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn immutable_view_selects_one_source_or_refuses_without_raw_fallback() {
        let source = serde_json::json!({"contract_version":1,"source":{"agent_run":"original","proof":"original proof"}});
        let bound = serde_json::json!({"agent_run":"changed","proof":"changed proof","envelope":source,"_immutable_metadata":{"envelope":source}});
        assert_eq!(
            view(&bound, "envelope", "/source", 1, &[]).unwrap()["agent_run"],
            "original"
        );
        assert!(view(&bound, "envelope", "/source", 1, &["agent_run"]).is_err());
        let legacy = serde_json::json!({"agent_run":"legacy"});
        assert_eq!(
            view(&legacy, "envelope", "/source", 1, &[]).unwrap(),
            &legacy
        );
        for alteration in ["binding", "version", "shape", "pointer"] {
            let mut changed = bound.clone();
            match alteration {
                "binding" => {
                    changed.as_object_mut().unwrap().remove(KEY);
                }
                "version" => {
                    changed["envelope"]["contract_version"] = serde_json::json!(2);
                    changed[KEY]["envelope"] = changed["envelope"].clone();
                }
                "shape" => {
                    changed["envelope"]["source"] = serde_json::json!([]);
                    changed[KEY]["envelope"] = changed["envelope"].clone();
                }
                "pointer" => {
                    changed["envelope"]
                        .as_object_mut()
                        .unwrap()
                        .remove("source");
                    changed[KEY]["envelope"] = changed["envelope"].clone();
                }
                _ => unreachable!(),
            }
            assert!(
                view(&changed, "envelope", "/source", 1, &[]).is_err(),
                "{alteration} cannot fall back to mutable fields"
            );
        }
    }
    use serde_json::json;

    #[test]
    fn shell_and_rust_immutable_source_decisions_are_equal() {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let library = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../infra/lib");
        let source = json!({"agent_run":"original","worktree":"tree","branch":"branch","sha":"head","hold":true});
        let envelope = json!({"contract_version":1,"source":source});
        let bound = json!({"agent_run":"original","worktree":"tree","branch":"branch","sha":"head","hold":false,
            "gate_submission":envelope,"_immutable_metadata":{"gate_submission":envelope}});
        let mut fixtures = vec![
            json!({"agent_run":"legacy"}),
            bound.clone(),
            json!({"gate_submission":null}),
            json!({"gate_submission":{}}),
        ];
        for key in ["agent_run", "worktree", "branch", "sha"] {
            let mut changed = bound.clone();
            changed[key] = json!("wrong");
            fixtures.push(changed);
            let mut changed = bound.clone();
            changed.as_object_mut().unwrap().remove(key);
            fixtures.push(changed);
        }
        for part in ["binding", "version", "source"] {
            let mut changed = bound.clone();
            match part {
                "binding" => {
                    changed.as_object_mut().unwrap().remove(KEY);
                }
                "version" => {
                    changed["gate_submission"]["contract_version"] = json!(2);
                    changed[KEY]["gate_submission"] = changed["gate_submission"].clone();
                }
                _ => {
                    changed["gate_submission"]["source"] = json!([]);
                    changed[KEY]["gate_submission"] = changed["gate_submission"].clone();
                }
            };
            fixtures.push(changed);
        }
        for metadata in fixtures {
            let expected = view(
                &metadata,
                "gate_submission",
                "/source",
                1,
                crate::gate_reservation::SOURCE_LOCATORS,
            );
            let mut child = Command::new("jq")
                .arg("-c")
                .arg("-L")
                .arg(&library)
                .arg("include \"immutable-metadata\"; gate_source")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(serde_json::to_string(&metadata).unwrap().as_bytes())
                .unwrap();
            let output = child.wait_with_output().unwrap();
            assert_eq!(
                output.status.success(),
                expected.is_ok(),
                "{metadata}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            if let Ok(value) = expected {
                assert_eq!(
                    &serde_json::from_slice::<Value>(&output.stdout).unwrap(),
                    value
                );
            }
        }
        let rule = std::fs::read_to_string(
            boss_testing::repo_root()
                .join("infra/dispatcher/rules/agent-run-lands-on-gate-green.toml"),
        )
        .unwrap();
        let parsed: toml::Value = toml::from_str(&rule).unwrap();
        let argument = parsed["rule"][0]["do"][0]["args"]["metadata_equal"]
            .as_str()
            .unwrap();
        let locators: String = serde_json::from_str(argument).unwrap();
        assert_eq!(locators, crate::gate_reservation::SOURCE_LOCATORS.join(","));
    }

    #[test]
    fn a_guarded_binding_survives_ordinary_writes_and_protocol_removal() {
        use crate::conditional_metadata::Command;
        use boss_core::job::{Job, Priority, Subject};
        let mut job = Job::new(
            "fixture",
            Subject::new("custom", "fixture"),
            "Guarded",
            "agent-test",
            Priority::Standard,
            chrono::NaiveDate::from_ymd_opt(2026, 10, 15).unwrap(),
        );
        job.metadata = json!({"ordinary":"kept"});
        let first: Command = serde_json::from_value(json!({"contract_version":2,
            "idempotency_key":uuid::Uuid::new_v4(),"expected":{"attempt":{"present":false}},
            "patch":{"attempt":{"state":"sending"}},"guarded_keys":["attempt"]}))
        .unwrap();
        first.validate().unwrap();
        let bound = first.apply(&job).unwrap();
        assert!(preserve(&bound.metadata, &json!({"ordinary":"kept"}), false).is_err());
        assert!(
            patch(
                &bound.metadata,
                json!({"attempt":null}).as_object().unwrap()
            )
            .is_err()
        );
        assert!(
            patch(
                &bound.metadata,
                json!({"attempt":{"state":"not_started"}})
                    .as_object()
                    .unwrap()
            )
            .is_err()
        );
        assert!(
            patch(
                &bound.metadata,
                json!({"ordinary":"changed"}).as_object().unwrap()
            )
            .is_ok()
        );
        let schema = json!({"guarded_fields":["attempt"]});
        assert!(validate_protocol(&schema, &job.metadata, &bound.metadata, Some(&first)).is_ok());
        assert!(
            validate_protocol(&json!({}), &job.metadata, &bound.metadata, Some(&first)).is_err()
        );
        let next:Command=serde_json::from_value(json!({"contract_version":2,"idempotency_key":uuid::Uuid::new_v4(),
            "expected":{"attempt":{"present":true,"value":{"state":"sending"}}},"patch":{"attempt":{"state":"not_started"}}})).unwrap();
        let changed = next.apply(&bound).unwrap();
        assert!(
            validate_protocol(&json!({}), &bound.metadata, &changed.metadata, Some(&next)).is_ok()
        );
        assert_eq!(
            changed.metadata["_guarded_metadata"],
            bound.metadata["_guarded_metadata"]
        );
        let unguarded:Command=serde_json::from_value(json!({"contract_version":2,"idempotency_key":uuid::Uuid::new_v4(),
            "expected":{"ordinary":{"present":true,"value":"kept"}},"patch":{"attempt":{"state":"not_started"}}})).unwrap();
        assert!(unguarded.apply(&bound).is_err());
    }

    #[test]
    fn conditional_records_require_an_exact_guard_and_cannot_be_erased() {
        let schema = json!({"conditional_fields":["attempt"]});
        let before = json!({"attempt":{"state":"sending"}});
        let after = json!({"attempt":{"state":"not_started"}});
        assert!(validate_protocol(&schema, &before, &after, None).is_err());
        let command = serde_json::from_value(json!({"contract_version":1,
            "idempotency_key":uuid::Uuid::new_v4(),
            "expected":{"attempt":{"present":true,"value":before["attempt"]}},
            "patch":{"attempt":after["attempt"]}}))
        .unwrap();
        assert!(validate_protocol(&schema, &before, &after, Some(&command)).is_ok());
        assert!(validate_protocol(&schema, &before, &json!({}), Some(&command)).is_err());
        let mut stale = command.clone();
        stale.expected.get_mut("attempt").unwrap().value = json!({"state":"not_started"});
        assert!(validate_protocol(&schema, &before, &after, Some(&stale)).is_err());
        assert!(validate_protocol(&schema, &before, &before, None).is_ok());
        assert!(validate_protocol(&json!({}), &before, &after, None).is_ok());
    }

    /// Backlog 97170186: a recovery assessment was admitted without the
    /// markers its executor's queue selects on, and sat invisible until
    /// a hand PATCH. A pinned protocol's `required_values` are refused
    /// absent or different at admission (before = {}) and on every
    /// later write, so a packet cannot be born or made unroutable.
    #[test]
    fn required_values_refuse_an_absent_or_different_value_on_every_write() {
        let schema = json!({"required_values":{"executor":"automation:x","marker":"v1"}});
        let routed = json!({"executor":"automation:x","marker":"v1","free":1});
        assert!(validate_protocol(&schema, &json!({}), &routed, None).is_ok());
        for (key, value) in [
            ("executor", None),
            ("executor", Some(json!("automation:y"))),
            ("marker", None),
            ("marker", Some(json!("v2"))),
            ("marker", Some(Value::Null)),
        ] {
            let mut after = routed.clone();
            match value {
                Some(value) => after[key] = value,
                None => {
                    after.as_object_mut().unwrap().remove(key);
                }
            }
            let refusal = validate_protocol(&schema, &json!({}), &after, None).unwrap_err();
            assert!(refusal.contains(key), "{refusal}");
            assert!(validate_protocol(&schema, &routed, &after, None).is_err());
        }
        let mut patch = Map::new();
        patch.insert("executor".into(), Value::Null);
        assert!(validate_protocol_patch(&schema, &routed, &patch).is_err());
        assert!(validate_protocol(&json!({}), &json!({}), &json!({}), None).is_ok());
        for malformed in [
            json!({"required_values":["executor"]}),
            json!({"required_values":{"":1}}),
        ] {
            assert!(validate_protocol(&malformed, &json!({}), &routed, None).is_err());
        }
    }
}
