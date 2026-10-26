//! Attended qualification: shared pure judgment, enforced at the server transition.
use crate::first_record::FirstRecord;
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};

pub fn canonical_actor(agents: &[Value], actor: &str) -> Result<String> {
    let count = agents
        .iter()
        .filter(|row| {
            row["id"] == actor
                || row["aliases"]
                    .as_array()
                    .is_some_and(|aliases| aliases.iter().any(|alias| alias == actor))
        })
        .count();
    ensure!(count <= 1, "actor alias is ambiguous");
    Ok(agents
        .iter()
        .find(|row| {
            row["id"] == actor
                || row["aliases"]
                    .as_array()
                    .is_some_and(|aliases| aliases.iter().any(|alias| alias == actor))
        })
        .and_then(|row| row["id"].as_str())
        .unwrap_or(actor)
        .to_owned())
}

pub fn contract() -> Result<Value> {
    Ok(serde_json::from_str(include_str!(
        "../../../../infra/step-plugins/browser-renderer-contract.json"
    ))?)
}

pub fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .with_context(|| format!("missing {key}"))
}

pub fn step<'a>(packet: &'a Value, slug: &str) -> Result<&'a Value> {
    let rows = packet
        .get("steps")
        .and_then(Value::as_array)
        .context("missing steps")?;
    let mut found = rows.iter().filter(|row| row["spec_slug"] == slug);
    let row = found
        .next()
        .with_context(|| format!("missing {slug} step"))?;
    ensure!(found.next().is_none(), "duplicate {slug} step");
    Ok(row)
}

fn artifact(value: &Value, expected: &Value, origin: &str) -> Result<()> {
    let path = text(expected, "path")?;
    let url = reqwest::Url::parse(text(value, "url")?)?;
    ensure!(
        url.origin().ascii_serialization() == origin
            && url.path() == path
            && url.query().is_none()
            && url.fragment().is_none()
            && url.username().is_empty()
            && url.password().is_none(),
        "asset origin/path differs"
    );
    ensure!(
        value["status"] == 200
            && value["expected_sha256"] == expected["sha256"]
            && value["observed_sha256"] == expected["sha256"],
        "asset observation differs"
    );
    let file = &value["file"];
    ensure!(
        file["sha256"] == expected["sha256"] && file["bytes"] == expected["bytes"],
        "asset file binding differs"
    );
    ensure!(
        !uuid::Uuid::parse_str(text(file, "id")?)?.is_nil(),
        "nil attachment"
    );
    Ok(())
}

fn complete_output(output: &Value, binding: &Value) -> Result<()> {
    ensure!(
        serde_json::to_vec(output)?.len() <= 65536
            && output["schema"] == 1
            && output.as_object().is_some_and(|map| map.len() == 2),
        "malformed output envelope"
    );
    let cases = output["cases"]
        .as_array()
        .context("missing case evidence")?;
    let ids = binding["cases"]
        .as_array()
        .context("missing published cases")?;
    ensure!(cases.len() == ids.len(), "incomplete case evidence");
    let mut seen = std::collections::BTreeSet::new();
    for case in cases {
        let id = text(case, "id")?;
        ensure!(
            ids.iter().any(|value| value == id) && seen.insert(id),
            "foreign or repeated case"
        );
        ensure!(
            case.as_object().is_some_and(|map| map.len() == 4) && case["error"].is_null(),
            "case failed or malformed"
        );
        let assertions = case["assertions"]
            .as_array()
            .context("missing assertions")?;
        let expected = binding["assertions"][id]
            .as_array()
            .context("missing assertion contract")?;
        ensure!(
            assertions.len() == expected.len(),
            "missing assertion evidence"
        );
        for (assertion, name) in assertions.iter().zip(expected) {
            ensure!(
                assertion["name"] == *name
                    && assertion["passed"] == true
                    && assertion.as_object().is_some_and(|map| map.len() == 3)
                    && assertion["actual"]
                        .as_str()
                        .is_some_and(|value| value.len() <= 8192),
                "failed, missing or reordered assertion"
            );
        }
        let requests = case["requests"]
            .as_array()
            .context("missing transport originals")?;
        ensure!(requests.len() <= 32, "too many requests");
        let mut writes = Vec::new();
        for request in requests {
            ensure!(
                request["allowed"] == true && request.as_object().is_some_and(|map| map.len() == 4),
                "forbidden or malformed transport"
            );
            let path = text(request, "url")?;
            let method = text(request, "method")?;
            if method == "GET" {
                ensure!(
                    request["body"].is_null()
                        && matches!(
                            path,
                            "/api/jobs/synthetic-question"
                                | "/api/jobs/5da17ace-7e35-4eae-a7c6-20e063c1791a"
                        ),
                    "non-synthetic read"
                );
            } else {
                ensure!(id == "question", "a non-question case attempted a write");
                let body: Value = serde_json::from_str(text(request, "body")?)?;
                match method {
                    "PATCH" => ensure!(
                        path == "/api/jobs/synthetic-question/steps/synthetic-step/metadata"
                            && body == json!({"verdict":"answered","answer":"Tuesday evening"}),
                        "non-synthetic metadata write"
                    ),
                    "PUT" => ensure!(
                        path == "/api/jobs/synthetic-question/steps/synthetic-step"
                            && body == json!({"status":"completed"}),
                        "completion contains other fields"
                    ),
                    _ => bail!("forbidden synthetic method"),
                }
                writes.push(method);
            }
        }
        if id == "question" {
            ensure!(
                writes == ["PATCH", "PUT"],
                "missing merge/status-only transport originals"
            );
        } else {
            ensure!(writes.is_empty(), "unexpected write");
        }
    }
    Ok(())
}

pub fn judge(
    record: &FirstRecord,
    packet: &Value,
    car: &Value,
    reviewer: &str,
    findings: &str,
) -> Result<Value> {
    let binding = contract()?;
    ensure!(
        record.actor.is_human(),
        "attended observation requires a person as the authenticated submitting actor"
    );
    ensure!(
        !reviewer.trim().is_empty() && !findings.trim().is_empty() && findings.len() <= 65536,
        "an explicit named independent review is required"
    );
    let reviewer_actor: boss_core::actor::ActorId = reviewer.parse()?;
    ensure!(
        reviewer_actor.is_human() || reviewer_actor.is_agent(),
        "an independent person or agent must review attended evidence"
    );
    ensure!(
        record.actor.to_string() != reviewer,
        "the observer cannot qualify their own observation"
    );
    ensure!(
        packet["kind"] == "browser-renderer-observation"
            && packet["metadata"]["car"] == binding["car"],
        "wrong observation packet/car"
    );
    let observed = step(packet, "observe")?;
    ensure!(
        observed["status"] == "completed",
        "observation has not completed"
    );
    // `record.actor.is_human()` above is a shape test on an id the machine
    // door takes as sent; what makes the actor a present person is the
    // PINNED field's signer writer, which routes the write through a verified
    // gateway session (review 8c30cbc7 on car b16e373d, 2026-10-19). A packet
    // pinned to a row without it never qualifies.
    ensure!(
        observed["fields"]
            .as_array()
            .is_some_and(|fields| fields.iter().any(|field| {
                field["name"] == "observation"
                    && field["writer"] == crate::field_writer::SIGNER_WRITER
            })),
        "the observation step's pinned observation field is not signer-written"
    );
    let job = uuid::Uuid::parse_str(text(packet, "id")?)?;
    let sid = uuid::Uuid::parse_str(text(observed, "id")?)?;
    crate::car_release::immutable_record_resource(record, job, sid, "observation")?;
    ensure!(
        record.matches(&observed["metadata"]["observation"]),
        "observation metadata differs from its immutable original"
    );
    let value = &record.value;
    ensure!(
        serde_json::to_vec(value)?.len() <= 262144
            && value["schema"] == 1
            && value["assurance"] == "attended-browser-observation"
            && value["binding"] == binding,
        "wrong source or suite binding"
    );
    ensure!(
        car["kind"] == "ship-a-change" && car["id"] == binding["car"],
        "wrong renderer car"
    );
    let receipt = crate::car::select_receipt(car).context("car has no receipt")?;
    ensure!(
        receipt["head"] == binding["source_head"] && receipt["verdict"] == "green",
        "car gate does not vouch for the bound source"
    );
    let origin = text(&binding, "origin")?;
    artifact(&value["asset"], &binding["asset"], origin)?;
    artifact(&value["suite"], &binding["suite"], origin)?;
    complete_output(&value["output"], &binding)?;
    ensure!(
        value["report"]["bytes"]
            .as_u64()
            .is_some_and(|size| size > 0 && size <= 65536)
            && value["report"]["sha256"].as_str().is_some_and(valid_hash)
            && !uuid::Uuid::parse_str(text(&value["report"], "id")?)?.is_nil(),
        "invalid full report attachment"
    );
    // Both heads, because they differ: `source_head` is what the car's gate
    // vouched for, and `asset_head` with `asset` is the renderer the browser
    // actually ran (review 5e15da5b N1, 2026-10-20 — the record used to name
    // only the source head, bytes nobody observed since train #1279).
    Ok(
        json!({"schema":1,"assurance":"attended-browser-independent-review","car":binding["car"],
        "source_head":binding["source_head"],"asset_head":binding["asset_head"],"asset":binding["asset"],
        "suite":binding["suite"],"observation_event":record.event_id,
        "observation_digest":record.digest,"findings":findings,"reviewer":reviewer}),
    )
}

fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

pub fn acceptance(
    car: &Value,
    published: &Value,
    observation: &Value,
    original: &FirstRecord,
    qualified: &FirstRecord,
) -> Result<Value> {
    ensure!(
        car["status"] == "open"
            && car["metadata"]["merged"] == "true"
            && step(car, "review")?["status"] == "completed"
            && step(car, "proven")?["status"] != "completed",
        "car is not an open landed claim awaiting proof"
    );
    ensure!(
        published["kind"] == "ship-a-change"
            && published["version"] == car["workflow_version"]
            && matches!(published["status"].as_str(), Some("active" | "retired")),
        "car has no matching published qualification protocol"
    );
    let definitions = published["steps"]
        .as_array()
        .context("missing pinned steps")?;
    let declaration = definitions.iter().find(|definition| definition["title"] == "attended-qualified")
        .context("this pinned protocol has no attended qualification terminal; convert explicitly after publication")?;
    ensure!(
        declaration["terminal"]["outcome"] == "attended-qualified"
            && declaration["metadata_defaults"]["qualification_contract"] == "attended-browser-v1"
            && declaration["metadata_defaults"]["records_before_claim"]
                == json!(["attended_qualification"])
            && declaration["fields"]
                .as_array()
                .is_some_and(|fields| fields
                    .iter()
                    .any(|field| field["name"] == "attended_qualification"
                        && field["field_type"] == "object"
                        && field["required"] == true)),
        "different qualification contract"
    );
    let terminal = step(car, "attended-qualified")?;
    ensure!(
        terminal["metadata"]["qualification_contract"] == "attended-browser-v1"
            && matches!(terminal["status"].as_str(), Some("pending" | "ready")),
        "car's qualification step differs"
    );
    let reviewed = step(observation, "qualify")?;
    ensure!(
        reviewed["status"] == "completed",
        "independent qualification has not completed"
    );
    let job = uuid::Uuid::parse_str(text(observation, "id")?)?;
    let sid = uuid::Uuid::parse_str(text(reviewed, "id")?)?;
    crate::car_release::immutable_record_resource(qualified, job, sid, "qualification")?;
    ensure!(
        qualified.matches(&reviewed["metadata"]["qualification"]),
        "qualification metadata differs from immutable record"
    );
    let reviewer = qualified.actor.to_string();
    let expected = judge(
        original,
        observation,
        car,
        &reviewer,
        text(&qualified.value, "findings")?,
    )?;
    ensure!(
        expected == qualified.value,
        "qualification does not bind this original, actor, source and suite"
    );
    Ok(
        json!({"schema":1,"assurance":"attended-browser-independent-review","observation":job,
        "observation_event":original.event_id,"observation_digest":original.digest,
        "qualification_event":qualified.event_id,"qualification_digest":qualified.digest,
        "car":car["id"],"source_head":expected["source_head"],"asset_head":expected["asset_head"],
        "asset":expected["asset"],"suite":expected["suite"]}),
    )
}

#[cfg(test)]
mod tests {
    use sha2::{Digest, Sha256};

    /// The renderer the boundary tests attach as the observed asset. It is a
    /// copy of the bytes the contract pins, not the tree's served file, so an
    /// edit to `answer-question.js` fails the ONE check below rather than
    /// every test that builds an observation (review 6ac43de2 F1 on car
    /// b16e373d, 2026-10-20: train #1279 changed the served file and seven
    /// unrelated tests failed with "asset file binding differs").
    const PINNED_RENDERER: &[u8] =
        include_bytes!("../tests/fixtures/browser-renderer/answer-question.js");
    const SERVED_RENDERER: &[u8] =
        include_bytes!("../../../../infra/step-plugins/answer-question.js");
    const SERVED_SUITE: &[u8] =
        include_bytes!("../../../../infra/step-plugins/browser-renderer-cases.js");

    fn pin(bytes: &[u8]) -> (usize, String) {
        (bytes.len(), hex::encode(Sha256::digest(bytes)))
    }

    fn pinned(binding: &serde_json::Value) -> (usize, String) {
        (
            binding["bytes"].as_u64().unwrap_or_default() as usize,
            binding["sha256"].as_str().unwrap_or_default().to_owned(),
        )
    }

    /// The gateway serves `infra/step-plugins/` from main, and the observation
    /// plugin refuses any served byte that differs from the contract
    /// (browser-observation.js `asset`). So a renderer or suite that moved
    /// without a re-pin is an observation nobody can perform — this names it.
    #[test]
    fn the_served_renderer_and_suite_are_the_bytes_the_browser_contract_pins() {
        let binding = super::contract().unwrap();
        assert_eq!(
            pin(SERVED_RENDERER),
            pinned(&binding["asset"]),
            "infra/step-plugins/answer-question.js drifted from browser-renderer-contract.json: \
             re-pin asset.bytes/sha256 and asset_head at the served file, copy it over \
             crates/core/boss-jobs/tests/fixtures/browser-renderer/answer-question.js, and say \
             what the move means for the car the contract names"
        );
        assert_eq!(
            pin(PINNED_RENDERER),
            pinned(&binding["asset"]),
            "the boundary fixture renderer is not the contract's pinned asset"
        );
        assert_eq!(
            pin(SERVED_SUITE),
            pinned(&binding["suite"]),
            "infra/step-plugins/browser-renderer-cases.js drifted from browser-renderer-contract.json"
        );
    }
}
