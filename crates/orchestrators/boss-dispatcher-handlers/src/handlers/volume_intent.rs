//! Report-only PVC intent (design 32e7cf87). Assignment data and live
//! requests are separate inputs; neither filesystem size nor bootstrap
//! templates supplies a desired target. This module issues no commands.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};

const MAX_BYTES: u64 = (1_u64 << 53) - 1;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Assignment {
    packet: String,
    step: String,
    question: String,
    decided_by: String,
    decided_at: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    namespace: String,
    claim: String,
    desired_bytes: u64,
    assignment: Assignment,
}

fn identity_part(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 253
        && s.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                && label
                    .bytes()
                    .next()
                    .is_some_and(|b| b.is_ascii_alphanumeric())
                && label
                    .bytes()
                    .last()
                    .is_some_and(|b| b.is_ascii_alphanumeric())
        })
}

fn declarations(text: &str) -> Result<BTreeMap<String, Intent>, String> {
    #[derive(Deserialize)]
    struct File {
        #[serde(default)]
        volume_intent: Vec<Intent>,
    }
    let file: File =
        toml::from_str(text).map_err(|_| "volume intent declaration is malformed".to_string())?;
    let mut result = BTreeMap::new();
    for row in file.volume_intent {
        let id = format!("{}/{}", row.namespace, row.claim);
        let a = &row.assignment;
        if !identity_part(&row.namespace)
            || !identity_part(&row.claim)
            || row.namespace.len() > 63
            || row.namespace.contains('.')
            || row.desired_bytes == 0
            || row.desired_bytes > MAX_BYTES
            || uuid::Uuid::parse_str(&a.packet).is_err()
            || uuid::Uuid::parse_str(&a.step).is_err()
            || a.question.trim().is_empty()
            || a.decided_by.trim().is_empty()
            || chrono::DateTime::parse_from_rfc3339(&a.decided_at).is_err()
        {
            return Err(format!(
                "volume intent {id}: invalid identity, byte count or assignment provenance"
            ));
        }
        if result.insert(id.clone(), row).is_some() {
            return Err(format!("volume intent {id} is declared twice"));
        }
    }
    Ok(result)
}

/// Exact integral quantities only. Fractional bytes, unsupported suffixes,
/// overflow and values outside the JSON safe-integer range stay unknown.
fn requested_bytes(raw: &Json) -> Option<u64> {
    let text = raw.as_str()?;
    let suffixes = [
        ("Ki", 1_u128 << 10),
        ("Mi", 1 << 20),
        ("Gi", 1 << 30),
        ("Ti", 1 << 40),
        ("Pi", 1 << 50),
        ("Ei", 1 << 60),
        ("k", 1_000),
        ("K", 1_000),
        ("M", 1_000_000),
        ("G", 1_000_000_000),
        ("T", 1_000_000_000_000),
        ("P", 1_000_000_000_000_000),
        ("E", 1_000_000_000_000_000_000),
    ];
    let (number, multiplier) = suffixes
        .iter()
        .find_map(|(suffix, scale)| text.strip_suffix(suffix).map(|n| (n, *scale)))
        .unwrap_or((text, 1));
    let number = number.strip_prefix('+').unwrap_or(number);
    let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
    if whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || !fraction.bytes().all(|b| b.is_ascii_digit())
        || (number.contains('.') && fraction.is_empty())
    {
        return None;
    }
    let denominator = 10_u128.checked_pow(u32::try_from(fraction.len()).ok()?)?;
    let digits = whole
        .parse::<u128>()
        .ok()?
        .checked_mul(denominator)?
        .checked_add(if fraction.is_empty() {
            0
        } else {
            fraction.parse::<u128>().ok()?
        })?;
    let numerator = digits.checked_mul(multiplier)?;
    if numerator % denominator != 0 {
        return None;
    }
    let bytes = u64::try_from(numerator / denominator).ok()?;
    (bytes > 0 && bytes <= MAX_BYTES).then_some(bytes)
}

fn observed_id(row: &Json) -> Option<String> {
    let ns = row.get("namespace")?.as_str()?;
    let claim = row.get("claim")?.as_str()?;
    if !identity_part(ns) || ns.len() > 63 || ns.contains('.') || !identity_part(claim) {
        return None;
    }
    let id = format!("{ns}/{claim}");
    (row.get("id").and_then(Json::as_str) == Some(id.as_str())).then_some(id)
}

/// Enrich the existing filesystem judgment, retaining every identifiable
/// observed claim and every declaration missing from the observation.
pub(super) fn enrich(mut body: Json, observation: &Json, text: &str) -> Json {
    let declared = declarations(text);
    let mut source: BTreeMap<String, Vec<Json>> = BTreeMap::new();
    let mut invalid = Vec::new();
    if let Err(reason) = &declared {
        invalid.push(json!({"id":"volume-intent-declaration-unread","tight":null,"unread":reason,
            "capacity_intent":{"verdict":"unknown","desired_bytes":null,"requested_bytes":null,"assignment":null,"reason":reason}}));
    }
    if observation.get("nodes").and_then(Json::as_array).is_none() {
        invalid.push(json!({"id":"volume-observation-unread","tight":null,"unread":"volume observation rows are malformed or missing",
            "capacity_intent":{"verdict":"unknown","desired_bytes":null,"requested_bytes":null,"assignment":null,"reason":"volume observation rows are malformed or missing"}}));
    }
    if let Some(rows) = observation.get("nodes").and_then(Json::as_array) {
        for (index, row) in rows.iter().enumerate() {
            if let Some(id) = observed_id(row) {
                source.entry(id).or_default().push(row.clone());
            } else {
                invalid.push(json!({"id":format!("unidentified-volume-row-{index}"), "tight":null,
                    "unread":"volume observation identity is malformed",
                    "capacity_intent":{"verdict":"unknown","desired_bytes":null,"requested_bytes":null,"assignment":null,"reason":"volume observation identity is malformed"}}));
            }
        }
    }
    let mut seen = BTreeSet::new();
    let mut ids = observation
        .get("nodes")
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
        .filter_map(observed_id)
        .filter(|id| seen.insert(id.clone()))
        .collect::<Vec<_>>();
    if let Ok(rows) = &declared {
        ids.extend(rows.keys().filter(|id| !seen.contains(*id)).cloned());
    }
    let floors: BTreeMap<String, Json> = body["volumes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| Some((r["id"].as_str()?.to_string(), r.clone())))
        .collect();
    let mut rows = Vec::new();
    let mut drift = Vec::new();
    let mut unknown = Vec::new();
    for id in ids {
        let observed = source.get(&id);
        let intent = declared.as_ref().ok().and_then(|d| d.get(&id));
        let request = observed
            .and_then(|v| v.first())
            .and_then(|r| requested_bytes(&r["requested"]));
        let reason = if let Err(why) = &declared {
            Some(why.clone())
        } else if observed.is_some_and(|v| v.len() != 1) {
            Some("claim is observed more than once".into())
        } else if intent.is_none() {
            Some("no explicit desired-capacity assignment".into())
        } else if observed.is_none() {
            Some("declared claim is not observed".into())
        } else if request.is_none() {
            Some(
                "live requested quantity is missing, malformed, non-integral or unsupported".into(),
            )
        } else {
            None
        };
        let verdict = if reason.is_some() {
            "unknown"
        } else if intent.is_some_and(|i| Some(i.desired_bytes) == request) {
            "match"
        } else {
            "drift"
        };
        let evidence = json!({"verdict":verdict,"desired_bytes":intent.map(|i|i.desired_bytes),"requested_bytes":request,
            "assignment":intent.map(|i|&i.assignment),"reason":reason});
        let mut row = floors.get(&id).cloned().unwrap_or_else(|| {
            if let Some(i) = intent { json!({"id":id,"namespace":i.namespace,"claim":i.claim,"tight":null,"unread":"declared claim is not observed"}) }
            else { json!({"id":id,"tight":null}) }
        });
        row["capacity_intent"] = evidence.clone();
        if verdict == "drift" {
            drift.push(json!({"id":id,"capacity_intent":evidence}));
        }
        if verdict == "unknown" {
            unknown.push(json!({"id":id,"capacity_intent":evidence}));
        }
        rows.push(row);
    }
    unknown.extend(
        invalid
            .iter()
            .map(|row| json!({"id":row["id"],"capacity_intent":row["capacity_intent"]})),
    );
    rows.extend(invalid);
    body["volumes"] = json!(rows);
    body["counts"]["capacity_drift"] = json!(drift.len());
    body["counts"]["capacity_unknown"] = json!(unknown.len());
    body["findings"]["capacity_drift"] = json!(drift);
    body["findings"]["capacity_unknown"] = json!(unknown);
    body["capacity_declaration_unread"] = declared.err().map_or(Json::Null, |e| json!(e));
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    const INTENT: &str = r#"
[[volume_intent]]
namespace = "test"
claim = "data"
desired_bytes = 32212254720
assignment = { packet = "32e7cf87-7d8e-4764-94a3-3b0cfa8548c4", step = "a40668dd-27d0-467b-822c-10f5d28b657a", question = "Q1", decided_by = "test-actor", decided_at = "2026-10-02T23:53:23.553088Z" }
"#;

    fn report(text: &str, observed: Json) -> Json {
        enrich(
            json!({"volumes":observed["nodes"],"counts":{},"findings":{}}),
            &observed,
            text,
        )
    }

    #[test]
    fn capacity_declaration_refuses_bad_provenance_numbers_duplicates_and_unknown_fields() {
        assert_eq!(declarations(INTENT).unwrap().len(), 1);
        assert!(declarations("").unwrap().is_empty());
        for text in [
            INTENT.replace("32212254720", "0"),
            INTENT.replace("32212254720", "-1"),
            INTENT.replace("32212254720", "9007199254740992"),
            INTENT.replace("test-actor", " "),
            INTENT.replace("2026-10-02T23:53:23.553088Z", "not-a-time"),
            INTENT.replace("Q1", ""),
            INTENT.replace("32e7cf87-7d8e-4764-94a3-3b0cfa8548c4", "missing"),
            format!("{INTENT}\n{INTENT}"),
            format!("{INTENT}\nobserved_bytes = 1\n"),
        ] {
            assert!(declarations(&text).is_err(), "{text}");
        }
    }

    #[test]
    fn capacity_request_is_exact_checked_integral_bytes_and_unsupported_is_unknown() {
        for (text, bytes) in [
            ("30Gi", 30_u64 << 30),
            ("30720Mi", 30 << 30),
            ("1.5Gi", 3 << 29),
            ("1.000001M", 1_000_001),
            ("32212254720", 30 << 30),
        ] {
            assert_eq!(requested_bytes(&json!(text)), Some(bytes), "{text}");
        }
        for raw in [
            json!(null),
            json!(30),
            json!(""),
            json!("-30Gi"),
            json!("0Gi"),
            json!("30 Gi"),
            json!("1.1"),
            json!("1e3"),
            json!("100000Ei"),
            json!("9007199254740992"),
        ] {
            assert_eq!(requested_bytes(&raw), None, "{raw}");
        }
    }

    #[test]
    fn capacity_declaration_failure_with_empty_observation_remains_visible() {
        let body = report(&format!("{INTENT}\n{INTENT}"), json!({"nodes":[]}));
        assert_eq!(body["volumes"][0]["capacity_intent"]["verdict"], "unknown");
        assert!(
            body["volumes"][0]["capacity_intent"]["reason"]
                .as_str()
                .unwrap()
                .contains("twice")
        );
    }

    #[test]
    fn capacity_identity_requires_actual_namespace_and_claim_dns_shapes() {
        for text in [
            INTENT.replace("namespace = \"test\"", "namespace = \"test.dot\""),
            INTENT.replace("claim = \"data\"", "claim = \"data..bad\""),
            INTENT.replace("claim = \"data\"", "claim = \"data.-bad\""),
        ] {
            assert!(
                declarations(&text).is_err(),
                "malformed PVC identity admitted: {text}"
            );
        }
        assert!(declarations(&INTENT.replace("claim = \"data\"", "claim = \"data.good\"")).is_ok());
    }

    #[test]
    fn capacity_assignment_actor_is_retained_history_never_current_authority() {
        let observation = json!({"nodes":[{"id":"test/data","namespace":"test","claim":"data","requested":"30Gi"}]});
        for actor in ["historical-human", "historical-agent"] {
            let body = report(&INTENT.replace("test-actor", actor), observation.clone());
            let intent = &body["volumes"][0]["capacity_intent"];
            assert_eq!(intent["verdict"], "match");
            assert_eq!(intent["desired_bytes"], 30_i64 << 30);
            assert_eq!(intent["assignment"]["decided_by"], actor);
            assert!(body.get("authority").is_none());
        }
    }

    #[test]
    fn capacity_identity_is_exact_and_duplicate_claim_receipts_are_collapsed_unknown() {
        let row = json!({"id":"test/data","namespace":"test","claim":"data","requested":"30Gi"});
        let body = report(INTENT, json!({"nodes":[row.clone(),row]}));
        assert_eq!(body["volumes"].as_array().unwrap().len(), 1);
        assert_eq!(body["volumes"][0]["capacity_intent"]["verdict"], "unknown");
        let body = report(
            INTENT,
            json!({"nodes":[{"id":"other/data","namespace":"test","claim":"data","requested":"30Gi"}]}),
        );
        assert!(
            body["volumes"]
                .as_array()
                .unwrap()
                .iter()
                .all(|r| r["capacity_intent"]["verdict"] == "unknown")
        );
        assert_eq!(body["counts"]["capacity_unknown"], 2);
    }
}
