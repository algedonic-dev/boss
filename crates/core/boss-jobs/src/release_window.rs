//! A refusing machine-gate flip needs the whole machine-read window.
use chrono::{DateTime, Utc};
use serde_json::Value;

/// Read the actual mounted word, rather than guessing its YAML spelling
/// from a patch. Duplicate keys/maps and unresolved merge keys are refusals.
pub fn machine_gate_mode(manifest: &str) -> Result<boss_core::machine_gate::Mode, String> {
    use serde::Deserialize;
    use yaml_serde::Value as Yaml;
    fn merges(value: &Yaml) -> bool {
        match value {
            Yaml::Mapping(map) => map
                .iter()
                .any(|(key, value)| key.as_str() == Some("<<") || merges(value)),
            Yaml::Sequence(values) => values.iter().any(merges),
            Yaml::Tagged(tag) => merges(&tag.value),
            _ => false,
        }
    }
    let mut mode = None;
    for document in yaml_serde::Deserializer::from_str(manifest) {
        let value =
            Yaml::deserialize(document).map_err(|e| format!("unreadable manifest YAML: {e}"))?;
        if value["kind"].as_str() != Some("ConfigMap")
            || value["metadata"]["name"].as_str() != Some("boss-machine-gate")
        {
            continue;
        }
        if mode.is_some() {
            return Err("the manifest declares boss-machine-gate more than once".into());
        }
        if merges(&value) {
            return Err("boss-machine-gate uses an unresolved YAML merge".into());
        }
        let word = value["data"]["mode"]
            .as_str()
            .ok_or("boss-machine-gate.data.mode is missing or not a scalar string")?;
        let (parsed, error) = boss_core::machine_gate::Mode::parse(Some(word));
        if let Some(error) = error {
            return Err(format!(
                "boss-machine-gate.data.mode is unreadable: {error}"
            ));
        }
        mode = Some(parsed);
    }
    mode.ok_or_else(|| "the manifest declares no boss-machine-gate ConfigMap".into())
}

fn instant(v: &Value) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(v.as_str()?)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

fn empty(v: &Value) -> bool {
    v.as_array().is_some_and(Vec::is_empty)
}

/// The complete JSON stays on the car; the release record binds it to this head.
/// A reader copies `{head, exit_code, window}` after running the published
/// gate-window-is-clean.sh machine-gate 26. No evidence is not a pass.
pub fn judge_window_receipt(receipt: &Value, head: &str, at: &str) -> Result<(), String> {
    let refuse = |reason: &str| {
        format!(
            "machine-gate enforce flip refused: {reason}; copy a fresh passing gate-window-is-clean.sh machine-gate 26 receipt and whole window.json onto metadata.machine_gate_window, bound to this exact head"
        )
    };
    if receipt["head"].as_str() != Some(head) {
        return Err(refuse(
            "the window receipt is missing or belongs to another head",
        ));
    }
    if receipt["exit_code"].as_i64() != Some(0) {
        return Err(refuse(
            "the machine reader did not record an integer exit_code 0",
        ));
    }
    let w = &receipt["window"];
    let from =
        instant(&w["from"]).ok_or_else(|| refuse("window.from is missing or not an instant"))?;
    let now =
        instant(&w["now"]).ok_or_else(|| refuse("window.now is missing or not an instant"))?;
    let clean = instant(&w["clean_since"])
        .ok_or_else(|| refuse("window.clean_since is missing or not an instant"))?;
    let release = DateTime::parse_from_rfc3339(at)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|_| refuse("the release has no readable timestamp"))?;
    if now > release {
        return Err(refuse(
            "the window is dated in the future relative to this release",
        ));
    }
    if (release - now).num_seconds() > 15 * 60 {
        return Err(refuse(
            "the window read is older than 15 minutes; read it again before release",
        ));
    }
    if w["gate"] != "machine-gate" {
        return Err(refuse("the window is not about machine-gate"));
    }
    if w["covers_requested_window"] != true
        || (now - from).num_seconds() < 26 * 3600
        || clean > from
    {
        return Err(refuse("the window does not cover 26 clean hours"));
    }
    if !w["log_error"].is_null()
        || !w["log"].is_object()
        || !empty(&w["log"]["dirty"])
        || !empty(&w["log"]["not_clean"])
    {
        return Err(refuse(
            "the log half is unreadable, missing, dirty or names a gap",
        ));
    }
    if !empty(&w["not_clean"]) || !empty(&w["input_errors"]) {
        return Err(refuse("the joined window names a gap or unreadable input"));
    }
    let generations = w["roster_generations"]
        .as_array()
        .ok_or_else(|| refuse("the launch generations are unreadable"))?;
    if generations.len() != 1 || generations[0]["generation"].as_str().is_none() {
        return Err(refuse(
            "the window does not name exactly one stated launch generation",
        ));
    }
    let required = w["required_services"]
        .as_array()
        .filter(|r| !r.is_empty())
        .ok_or_else(|| refuse("no required service list was read"))?;
    let coverage = w["log"]["coverage"]
        .as_array()
        .ok_or_else(|| refuse("the log coverage list is unreadable"))?;
    let live = w["live"]
        .as_array()
        .ok_or_else(|| refuse("the live service list is unreadable"))?;
    for service in required {
        let service_error = |reason: &str| refuse(&format!("{service}: {reason}"));
        let c = coverage
            .iter()
            .find(|c| c["service"] == *service)
            .ok_or_else(|| service_error("no log coverage row"))?;
        let l = live
            .iter()
            .find(|l| l["service"] == *service)
            .ok_or_else(|| service_error("no live row"))?;
        let t = &l["snapshot"];
        if !service.is_string()
            || !c["gap"].is_null()
            || !l["error"].is_null()
            || !empty(&l["not_clean"])
        {
            return Err(service_error("a log/live row is unreadable or names a gap"));
        }
        if !t.is_object()
            || !matches!(t["mode"].as_str(), Some("report" | "enforce"))
            || !t["mode_error"].is_null()
            || t["overflow"] != 0
            || !empty(&t["not_clean"])
        {
            return Err(service_error(
                "the live tally is missing, not recording, overflowed or names a gap",
            ));
        }
        let rows = t["rows"]
            .as_array()
            .ok_or_else(|| service_error("the live tally rows are unreadable"))?;
        for r in rows {
            let count = r["count"].as_f64().unwrap_or_default();
            if !r.is_object()
                || count <= 0.0
                || count.fract() != 0.0
                || !["peer", "user", "method", "route"]
                    .iter()
                    .all(|k| r[k].is_string())
                || !matches!(
                    r["presented"].as_str(),
                    Some(
                        "previous" | "reader.current" | "reader.next" | "reader.previous" | "asked"
                    )
                )
            {
                return Err(service_error(
                    "the live tally holds an enforce-refused or unreadable row",
                ));
            }
        }
    }
    Ok(())
}

/// The trusted review verb copies source here before it records a verdict.
/// The release reads this original through the same port as executor evidence;
/// mutable metadata and a caller's declaration of a flip are never the source.
pub const SOURCE_KEY: &str = "review.source";
pub const MANIFEST_PATH: &str = "infra/cluster/manifests/boss.yaml";

/// Read the frozen reviewed diff and, for an enforcing flip, judge the copied
/// window at release. Old reviews need a fresh source-bearing review, not a
/// re-gate merely to populate a new field.
pub async fn judge_source_window(
    reader: &impl crate::car_release::ExecutorRecordReader,
    car: &Value,
    run: &Value,
    head: &str,
    at: &str,
) -> Result<Value, String> {
    use sha2::{Digest, Sha256};
    let refuse = |reason: &str| {
        format!(
            "release source refused: {reason}; run a fresh boss review on this exact head so its immutable review.source records the reviewed base and diff (legacy reviews need no re-gate solely for this record)"
        )
    };
    let run_id = run["id"]
        .as_str()
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| refuse("the reviewer has no full run id"))?;
    let building = run["steps"]
        .as_array()
        .and_then(|steps| {
            steps
                .iter()
                .find(|s| s["spec_slug"] == crate::car_release::BUILDING_SLUG)
        })
        .and_then(|s| s["id"].as_str())
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| refuse("the reviewer has no Building step"))?;
    let original = reader
        .executor_original(run_id, building, SOURCE_KEY)
        .await
        .map_err(|e| refuse(&format!("the original source could not be read: {e:#}")))?
        .ok_or_else(|| refuse("the reviewer carries no original source record"))?;
    crate::car_release::immutable_record_resource(&original, run_id, building, SOURCE_KEY)
        .map_err(|e| refuse(&e.to_string()))?;
    let scalar: Value = serde_json::from_str(&original.value_json)
        .map_err(|_| refuse("the original scalar is unreadable"))?;
    let canonical = boss_core::job::canonical_json_bytes(&scalar);
    let sha = |v: &Value| {
        v.as_str()
            .is_some_and(|s| s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit()))
    };
    if scalar != original.value
        || canonical != original.canonical
        || hex::encode(Sha256::digest(&canonical)) != original.digest
        || scalar["car"] != car["id"]
        || scalar["head"].as_str() != Some(head)
        || !sha(&scalar["head"])
        || !sha(&scalar["base"])
        || !sha(&scalar["main"])
        || scalar["path"] != MANIFEST_PATH
        || !scalar["diff"].is_string()
    {
        return Err(refuse(
            "the source does not conserve its original bytes, car, head, base and path",
        ));
    }
    let binding = serde_json::json!({"digest":original.digest,"event_id":original.event_id});
    if run.pointer("/metadata/review/source_binding") != Some(&binding) {
        return Err(refuse(
            "the verdict does not bind the original source record",
        ));
    }
    let mut evidence = serde_json::json!({"digest":original.digest,"event_id":original.event_id,"head":head,"base":scalar["base"],"main":scalar["main"]});
    let parsed =
        |field: &str, mode_key: &str| -> Result<Option<boss_core::machine_gate::Mode>, String> {
            match scalar[mode_key].as_str() {
                Some("absent") if scalar[field].is_null() => Ok(None),
                Some("100644" | "100755") => machine_gate_mode(
                    scalar[field]
                        .as_str()
                        .ok_or_else(|| refuse(&format!("{field} was not copied")))?,
                )
                .map(Some)
                .map_err(|e| refuse(&format!("{field}: {e}"))),
                _ => Err(refuse(&format!(
                    "{field} has no supported regular or explicitly absent file mode"
                ))),
            }
        };
    let before = parsed("base_manifest", "base_mode")?;
    let after = parsed("manifest", "mode")?;
    if before != Some(boss_core::machine_gate::Mode::Enforce)
        && after == Some(boss_core::machine_gate::Mode::Enforce)
    {
        let receipt = &car["metadata"]["machine_gate_window"];
        judge_window_receipt(receipt, head, at)?;
        evidence["machine_gate_window"] = receipt.clone();
    }
    Ok(evidence)
}
