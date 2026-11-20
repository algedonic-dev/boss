//! Attended browser qualification is a separate typed act, never shell proof.
use anyhow::{Context, Result, ensure};
use boss_jobs::first_record::FirstRecord;
use serde_json::{Value, json};

use boss_jobs::browser_qualification::{acceptance, contract, judge, step, text};

#[derive(clap::Subcommand)]
pub enum Cmd {
    /// Independently judge attended browser evidence, preserving shell proof semantics.
    Browser {
        #[command(subcommand)]
        action: BrowserAction,
    },
}

#[derive(clap::Subcommand)]
pub enum BrowserAction {
    /// Deliberately accept an independently qualified attended claim on a converted car.
    Accept {
        car: uuid::Uuid,
        #[arg(long)]
        observation: uuid::Uuid,
        /// Accept the stated attended assurance, never a shell proof receipt.
        #[arg(long, required = true)]
        accept_attended: bool,
    },
    /// Read exact immutable originals, then record a separate attended qualification.
    Qualify {
        /// Full browser-renderer-observation packet UUID.
        observation: uuid::Uuid,
        /// The independent review of the full originals and case assertions.
        #[arg(long)]
        findings_file: std::path::PathBuf,
        /// Accept attended independent-review assurance, not trusted remote execution.
        #[arg(long, required = true)]
        accept_attended: bool,
    },
}

async fn file_bytes(
    http: &crate::machine_http::MachineClient,
    base: &str,
    signer: &str,
    id: &str,
    limit: usize,
) -> Result<Vec<u8>> {
    ensure!(!uuid::Uuid::parse_str(id)?.is_nil(), "invalid file id");
    let path = format!("/api/files/{id}");
    let mut response = http
        .request(reqwest::Method::GET, format!("{base}{path}"))
        .header("x-boss-user", crate::identity::header(signer))
        .send()
        .await?;
    ensure!(
        response.status() == reqwest::StatusCode::OK,
        "file {id}: {}",
        response.status()
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            bytes.len().saturating_add(chunk.len()) <= limit,
            "file exceeds its byte bound"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn check_files(record: &FirstRecord, packet_id: &str, signer: &str) -> Result<()> {
    let base = crate::tenant_publish::service_on_door(&crate::sor_client::jobs_base()?, "content")?;
    let http = crate::sor_client::machine_client_with(
        reqwest::Client::builder().timeout(std::time::Duration::from_secs(15)),
    )?;
    check_files_at(&http, &base, record, packet_id, signer).await
}

async fn check_files_at(
    http: &crate::machine_http::MachineClient,
    base: &str,
    record: &FirstRecord,
    packet_id: &str,
    signer: &str,
) -> Result<()> {
    use sha2::{Digest, Sha256};
    let path = format!("/api/files?target_kind=job&target_id={packet_id}");
    let listing = crate::sor_client::api_at(http, base, reqwest::Method::GET, &path, None)
        .await?
        .context("missing attachment list")?;
    let files = listing
        .as_array()
        .context("file store did not return a complete file list")?;
    for (key, binding) in [
        ("asset", &record.value["asset"]["file"]),
        ("suite", &record.value["suite"]["file"]),
        ("report", &record.value["report"]),
    ] {
        let id = text(binding, "id")?;
        let metadata = files
            .iter()
            .find(|file| file["id"] == id)
            .context("attachment is not on this observation")?;
        ensure!(
            metadata["target"]["kind"] == "job"
                && metadata["target"]["id"] == packet_id
                && metadata["deleted_at"].is_null()
                && metadata["sha256"] == binding["sha256"]
                && metadata["size_bytes"] == binding["bytes"],
            "attachment reference differs"
        );
        let bytes = file_bytes(
            http,
            base,
            signer,
            id,
            if key == "report" { 65536 } else { 262144 },
        )
        .await?;
        ensure!(
            binding["bytes"].as_u64() == Some(bytes.len() as u64)
                && binding["sha256"] == hex::encode(Sha256::digest(&bytes)),
            "attachment bytes differ"
        );
        if key == "report" {
            ensure!(
                serde_json::from_slice::<Value>(&bytes)? == record.value["output"],
                "report differs from immutable output"
            );
        }
    }
    Ok(())
}

use boss_jobs::browser_qualification::canonical_actor;

async fn qualify(
    observation: uuid::Uuid,
    findings_file: &std::path::Path,
    accepted: bool,
) -> Result<()> {
    ensure!(
        accepted && !observation.is_nil(),
        "explicit attended qualification is required"
    );
    ensure!(
        tokio::fs::metadata(findings_file).await?.len() <= 65536,
        "review findings exceed bound"
    );
    let findings = tokio::fs::read_to_string(findings_file).await?;
    let wire = crate::steps::Wire::live()?;
    let signer = wire.caller_id().context("a named reviewer is required")?;
    let packet = wire.packet(&observation.to_string()).await?;
    let observed = step(&packet, "observe")?;
    let observe_step = uuid::Uuid::parse_str(text(observed, "id")?)?;
    let original = wire
        .immutable_record(observation, observe_step, "observation")
        .await?
        .context("no immutable observation original")?;
    ensure!(
        original.actor.is_human(),
        "attended observation requires a person as the authenticated submitting actor"
    );
    let roster = wire
        .call(reqwest::Method::GET, "/api/agents", None)
        .await?
        .context("agent roster unavailable")?;
    let agents = roster["data"]
        .as_array()
        .context("agent roster is not a complete list")?;
    ensure!(
        roster["total"].as_u64() == Some(agents.len() as u64),
        "agent roster is incomplete"
    );
    let reviewer = canonical_actor(agents, signer)?;
    ensure!(
        canonical_actor(agents, &original.actor.to_string())? != reviewer,
        "the observer cannot qualify their own observation through an alias"
    );
    let binding = contract()?;
    let car = wire.packet(text(&binding, "car")?).await?;
    let qualification = judge(&original, &packet, &car, &reviewer, &findings)?;
    check_files(&original, &observation.to_string(), signer).await?;
    // A concurrent annotation is not substituted for the immutable original.
    let current = wire
        .immutable_record(observation, observe_step, "observation")
        .await?
        .context("observation disappeared")?;
    ensure!(current == original, "observation original changed");
    let after = wire.packet(&observation.to_string()).await?;
    ensure!(
        judge(
            &current,
            &after,
            &wire.packet(text(&binding, "car")?).await?,
            &reviewer,
            &findings
        )? == qualification,
        "observation or car changed during review"
    );
    let target = step(&after, "qualify")?;
    ensure!(
        target["status"] == "ready" || target["status"] == "active",
        "qualification step is not open"
    );
    let sid = uuid::Uuid::parse_str(text(target, "id")?)?;
    let path = format!("/api/jobs/{observation}/steps/{sid}");
    wire.call(
        reqwest::Method::POST,
        &format!("{path}/metadata/records"),
        Some(json!({"key":"qualification","value":qualification,"expected_absence":true})),
    )
    .await?;
    let retained = wire
        .immutable_record(observation, sid, "qualification")
        .await?
        .context("qualification did not read back")?;
    ensure!(
        retained.value == qualification
            && canonical_actor(agents, &retained.actor.to_string())? == reviewer,
        "qualification readback differs"
    );
    wire.call(
        reqwest::Method::PUT,
        &path,
        Some(json!({"status":"completed"})),
    )
    .await?;
    let completed = wire.packet(&observation.to_string()).await?;
    ensure!(
        step(&completed, "qualify")?["status"] == "completed",
        "qualification completion did not read back"
    );
    println!(
        "{}",
        json!({"observation":observation,"car":binding["car"],"qualification":retained,
        "assurance":"attended-browser-independent-review","car_proven_completed":false})
    );
    Ok(())
}

pub async fn dispatch(command: Cmd) -> Result<()> {
    match command {
        Cmd::Browser {
            action:
                BrowserAction::Qualify {
                    observation,
                    findings_file,
                    accept_attended,
                },
        } => qualify(observation, &findings_file, accept_attended).await,
        Cmd::Browser {
            action:
                BrowserAction::Accept {
                    car,
                    observation,
                    accept_attended,
                },
        } => accept(car, observation, accept_attended).await,
    }
}

async fn accept(car_id: uuid::Uuid, observation: uuid::Uuid, accepted: bool) -> Result<()> {
    ensure!(
        accepted && !car_id.is_nil() && !observation.is_nil(),
        "explicit attended acceptance is required"
    );
    let wire = crate::steps::Wire::live()?;
    let signer = wire
        .caller_id()
        .context("a named accepting actor is required")?;
    let packet = wire.packet(&observation.to_string()).await?;
    let original = wire
        .immutable_record(
            observation,
            uuid::Uuid::parse_str(text(step(&packet, "observe")?, "id")?)?,
            "observation",
        )
        .await?
        .context("missing observation original")?;
    let qualified = wire
        .immutable_record(
            observation,
            uuid::Uuid::parse_str(text(step(&packet, "qualify")?, "id")?)?,
            "qualification",
        )
        .await?
        .context("missing qualification original")?;
    let roster = wire
        .call(reqwest::Method::GET, "/api/agents", None)
        .await?
        .context("agent roster unavailable")?;
    let agents = roster["data"]
        .as_array()
        .context("agent roster is not a complete list")?;
    ensure!(
        roster["total"].as_u64() == Some(agents.len() as u64),
        "agent roster is incomplete"
    );
    ensure!(
        canonical_actor(agents, &original.actor.to_string())?
            != canonical_actor(agents, &qualified.actor.to_string())?,
        "the observer cannot qualify their own observation through an alias"
    );
    let car = wire.packet(&car_id.to_string()).await?;
    let version = car["workflow_version"]
        .as_u64()
        .context("car lacks pinned protocol")?;
    let published = wire
        .call(
            reqwest::Method::GET,
            &format!("/api/workflows/ship-a-change/versions/{version}"),
            None,
        )
        .await?
        .context("pinned protocol unavailable")?;
    let record = acceptance(&car, &published, &packet, &original, &qualified)?;
    check_files(&original, &observation.to_string(), signer).await?;
    let fresh_packet = wire.packet(&observation.to_string()).await?;
    let fresh_car = wire.packet(&car_id.to_string()).await?;
    ensure!(
        acceptance(&fresh_car, &published, &fresh_packet, &original, &qualified)? == record,
        "bindings changed during acceptance"
    );
    let terminal = step(&fresh_car, "attended-qualified")?;
    let sid = uuid::Uuid::parse_str(text(terminal, "id")?)?;
    let path = format!("/api/jobs/{car_id}/steps/{sid}/metadata/records");
    wire.call(
        reqwest::Method::POST,
        &path,
        Some(json!({"key":"attended_qualification","value":record,"expected_absence":true})),
    )
    .await?;
    let retained = wire
        .immutable_record(car_id, sid, "attended_qualification")
        .await?
        .context("acceptance did not read back")?;
    ensure!(retained.value == record, "acceptance original differs");
    // New protocol terminal only: preserve every old proven field and receipt.
    let before_proven = step(&fresh_car, "proven")?.clone();
    wire.patch_job_metadata(&car_id.to_string(), json!({"attended_qualified":"true"}))
        .await?;
    wire.call(
        reqwest::Method::PUT,
        &format!("/api/jobs/{car_id}/steps/{sid}"),
        Some(json!({"status":"completed"})),
    )
    .await?;
    let after = wire.packet(&car_id.to_string()).await?;
    ensure!(
        after["status"] == "closed"
            && after["metadata"]["outcome"] == "attended-qualified"
            && step(&after, "attended-qualified")?["status"] == "completed",
        "attended terminal did not complete"
    );
    let after_proven = step(&after, "proven")?;
    ensure!(
        after_proven["metadata"] == before_proven["metadata"]
            && after_proven["completed_at"] == before_proven["completed_at"]
            && after_proven["completed_by"] == before_proven["completed_by"]
            && after_proven["status"] != "completed",
        "the original shell proof evidence changed"
    );
    println!(
        "{}",
        json!({"car":car_id,"observation":observation,"acceptance":retained,"assurance":"attended-browser-independent-review","shell_proof_completed":false})
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_core::{
        actor::ActorId,
        job::{JobId, StepId},
        publisher::EventStamp,
    };

    #[test]
    fn aliases_cannot_supply_an_independent_reviewer_and_ambiguous_rosters_refuse() {
        let agents = vec![json!({"id":"agent-reviewer","aliases":["emp-observer","agent-alias"]})];
        assert_eq!(
            canonical_actor(&agents, "emp-observer").unwrap(),
            canonical_actor(&agents, "agent-alias").unwrap()
        );
        assert_ne!(
            canonical_actor(&agents, "emp-other").unwrap(),
            canonical_actor(&agents, "agent-alias").unwrap()
        );
        let mut ambiguous = agents.clone();
        ambiguous.push(json!({"id":"another-agent","aliases":["emp-observer"]}));
        assert!(canonical_actor(&ambiguous, "emp-observer").is_err());
    }

    fn fixture() -> (FirstRecord, Value, Value) {
        let binding = contract().unwrap();
        let job = JobId::new();
        let step = StepId::new();
        let output = json!({"schema":1,"cases":binding["cases"].as_array().unwrap().iter().map(|id| json!({"id":id,"assertions":binding["assertions"][id.as_str().unwrap()].as_array().unwrap().iter().map(|name| json!({"name":name,"passed":true,"actual":"synthetic"})).collect::<Vec<_>>(),"requests": if id == "question" { vec![json!({"url":"/api/jobs/synthetic-question/steps/synthetic-step/metadata","method":"PATCH","body":json!({"verdict":"answered","answer":"Tuesday evening"}).to_string(),"allowed":true}),json!({"url":"/api/jobs/synthetic-question/steps/synthetic-step","method":"PUT","body":json!({"status":"completed"}).to_string(),"allowed":true})] } else {vec![]},"error":null})).collect::<Vec<_>>()});
        let file =
            |b: &Value| json!({"id":uuid::Uuid::new_v4(),"sha256":b["sha256"],"bytes":b["bytes"]});
        let value = json!({"schema":1,"assurance":"attended-browser-observation","binding":binding,
            "asset":{"url":"https://boss.algedonic.dev/plugins/answer-question.js","status":200,"headers":{},"expected_sha256":binding["asset"]["sha256"],"observed_sha256":binding["asset"]["sha256"],"file":file(&binding["asset"])},
            "suite":{"url":"https://boss.algedonic.dev/plugins/browser-renderer-cases.js","status":200,"headers":{},"expected_sha256":binding["suite"]["sha256"],"observed_sha256":binding["suite"]["sha256"],"file":file(&binding["suite"])},
            "report":{"id":uuid::Uuid::new_v4(),"sha256":"a".repeat(64),"bytes":1},"output":output});
        let stamp = EventStamp::new("synthetic-browser-test", ActorId::human("emp-observer"));
        let record = FirstRecord::new(
            job,
            step,
            "observation",
            &value,
            &stamp,
            uuid::Uuid::new_v4(),
        );
        let packet = json!({"id":job,"kind":"browser-renderer-observation","metadata":{"car":binding["car"]},"steps":[{"id":step,"spec_slug":"observe","status":"completed","fields":[{"name":"observation","field_type":"object","required":true,"writer":"signer"}],"metadata":{"observation":value}}]});
        let car = json!({"id":binding["car"],"kind":"ship-a-change","metadata":{},"steps":[{"spec_slug":"gate","metadata":{"receipt":{"head":binding["source_head"],"verdict":"green"}}}]});
        (record, packet, car)
    }

    #[test]
    fn an_independent_explicit_review_qualifies_only_attended_evidence() {
        let (record, packet, car) = fixture();
        let qualified = judge(
            &record,
            &packet,
            &car,
            "emp-reviewer",
            "I reviewed the exact original assets and every case.",
        )
        .unwrap();
        assert_eq!(
            qualified["assurance"],
            "attended-browser-independent-review"
        );
        assert_eq!(qualified["observation_event"], record.event_id.to_string());
        assert!(qualified.get("proof").is_none());
        assert!(qualified.get("proven").is_none());
    }

    #[test]
    fn a_client_pass_flag_does_not_replace_missing_case_evidence() {
        let (mut record, mut packet, car) = fixture();
        let mut value = record.value.clone();
        value["output"] = json!({"schema":1,"passed":true,"cases":[]});
        record = FirstRecord::new(
            record.job_id,
            record.step_id,
            "observation",
            &value,
            &EventStamp::new("test", ActorId::human("emp-observer")),
            uuid::Uuid::new_v4(),
        );
        packet["steps"][0]["metadata"]["observation"] = value;
        assert!(judge(&record, &packet, &car, "emp-reviewer", "reviewed").is_err());
    }

    #[test]
    fn only_a_published_versioned_attended_terminal_can_accept_a_qualification() {
        let (record, mut packet, mut car) = fixture();
        let value = judge(
            &record,
            &packet,
            &car,
            "emp-reviewer",
            "Reviewed every retained original.",
        )
        .unwrap();
        let qualification = FirstRecord::new(
            record.job_id,
            StepId::new(),
            "qualification",
            &value,
            &EventStamp::new("test", ActorId::human("emp-reviewer")),
            uuid::Uuid::new_v4(),
        );
        packet["steps"].as_array_mut().unwrap().push(json!({"id":qualification.step_id,"spec_slug":"qualify","status":"completed","metadata":{"qualification":value}}));
        car["workflow_version"] = json!(10);
        car["status"] = json!("open");
        car["metadata"]["merged"] = json!("true");
        car["steps"].as_array_mut().unwrap().push(json!({"id":StepId::new(),"spec_slug":"attended-qualified","status":"pending","metadata":{"qualification_contract":"attended-browser-v1"}}));
        car["steps"].as_array_mut().unwrap().extend([json!({"id":StepId::new(),"spec_slug":"review","status":"completed"}),json!({"id":StepId::new(),"spec_slug":"proven","status":"ready","metadata":{"proof_event":"original unpaid visual claim"}})]);
        let published = json!({"kind":"ship-a-change","version":10,"status":"active","steps":[{"title":"attended-qualified","terminal":{"outcome":"attended-qualified"},"metadata_defaults":{"qualification_contract":"attended-browser-v1","records_before_claim":["attended_qualification"]},"fields":[{"name":"attended_qualification","field_type":"object","required":true}]}]});
        let accepted = acceptance(&car, &published, &packet, &record, &qualification).unwrap();
        assert_eq!(accepted["assurance"], "attended-browser-independent-review");
        assert_eq!(
            accepted["qualification_event"],
            qualification.event_id.to_string()
        );
        let mut old = car.clone();
        old["workflow_version"] = json!(9);
        assert!(acceptance(&old, &published, &packet, &record, &qualification).is_err());
        let mut draft = published.clone();
        draft["status"] = json!("draft");
        assert!(acceptance(&car, &draft, &packet, &record, &qualification).is_err());
    }

    #[test]
    fn immutable_identity_cases_bindings_and_write_originals_are_all_required() {
        for fault in 0..15 {
            let (mut record, mut packet, mut car) = fixture();
            let mut value = record.value.clone();
            match fault {
                0 => value["binding"]["car"] = json!(uuid::Uuid::new_v4()),
                1 => value["binding"]["suite"]["sha256"] = json!("b".repeat(64)),
                2 => value["asset"]["observed_sha256"] = json!("b".repeat(64)),
                3 => {
                    value["asset"]["url"] =
                        json!("https://foreign.invalid/plugins/answer-question.js")
                }
                4 => value["suite"]["status"] = json!(302),
                5 => value["asset"]["file"]["bytes"] = json!(1),
                6 => value["output"]["cases"][0]["assertions"][0]["passed"] = json!(false),
                7 => {
                    value["output"]["cases"][0]["assertions"]
                        .as_array_mut()
                        .unwrap()
                        .pop();
                }
                8 => value["output"]["cases"][0]["error"] = json!("timeout"),
                9 => value["output"]["cases"][0]["requests"][0]["allowed"] = json!(false),
                10 => {
                    value["output"]["cases"][0]["requests"][0]["url"] =
                        json!("/api/jobs/live-question/steps/live/metadata")
                }
                11 => {
                    value["output"]["cases"][0]["requests"][1]["body"] =
                        json!(r#"{"status":"completed","metadata":{"invented":true}}"#)
                }
                12 => value["output"]["cases"][0]["requests"] = json!([]),
                13 => car["steps"][0]["metadata"]["receipt"]["head"] = json!("b".repeat(40)),
                _ => packet["steps"][0]["status"] = json!("ready"),
            }
            record = FirstRecord::new(
                record.job_id,
                record.step_id,
                "observation",
                &value,
                &EventStamp::new("test", ActorId::human("emp-observer")),
                uuid::Uuid::new_v4(),
            );
            packet["steps"][0]["metadata"]["observation"] = value;
            assert!(
                judge(
                    &record,
                    &packet,
                    &car,
                    "emp-reviewer",
                    "Reviewed originals."
                )
                .is_err(),
                "fault {fault}"
            );
        }
        let (record, packet, car) = fixture();
        assert!(judge(&record, &packet, &car, "emp-observer", "reviewed").is_err());
        assert!(judge(&record, &packet, &car, "emp-reviewer", " ").is_err());
        assert!(judge(&record, &packet, &car, "automation:test", "reviewed").is_err());
        let mut tampered = record.clone();
        tampered.value["output"]["schema"] = json!(2);
        assert!(judge(&tampered, &packet, &car, "emp-reviewer", "reviewed").is_err());
        let mut other = record.clone();
        other.step_id = StepId::new();
        assert!(judge(&other, &packet, &car, "emp-reviewer", "reviewed").is_err());
    }

    /// The contract's pinned renderer. Whether the SERVED file still matches
    /// it is one named check in boss-jobs
    /// (`the_served_renderer_and_suite_are_the_bytes_the_browser_contract_pins`),
    /// so a drift does not also fail these (review 6ac43de2 F1, 2026-10-20).
    const PINNED_RENDERER: &[u8] = include_bytes!(
        "../../../../crates/core/boss-jobs/tests/fixtures/browser-renderer/answer-question.js"
    );

    #[test]
    fn published_bindings_pin_whole_asset_bytes_and_not_comments() {
        use sha2::{Digest, Sha256};
        let binding = contract().unwrap();
        for (kind, bytes) in [
            ("asset", PINNED_RENDERER),
            (
                "suite",
                include_bytes!("../../../../infra/step-plugins/browser-renderer-cases.js")
                    .as_slice(),
            ),
        ] {
            assert_eq!(binding[kind]["bytes"], bytes.len());
            assert_eq!(binding[kind]["sha256"], hex::encode(Sha256::digest(bytes)));
        }
    }

    #[tokio::test]
    async fn attachment_transport_reads_whole_originals_and_refuses_tampering_and_redirects() {
        use axum::{Json, Router, response::IntoResponse};
        use sha2::{Digest, Sha256};
        for fault in 0..6 {
            let (mut record, _, _) = fixture();
            let packet = record.job_id.to_string();
            let report = serde_json::to_vec(&record.value["output"]).unwrap();
            record.value["report"]["bytes"] = json!(report.len());
            record.value["report"]["sha256"] = json!(hex::encode(Sha256::digest(&report)));
            let mut rows = Vec::new();
            let mut blobs = std::collections::BTreeMap::new();
            for (key, bytes) in [
                ("asset", PINNED_RENDERER.to_vec()),
                (
                    "suite",
                    include_bytes!("../../../../infra/step-plugins/browser-renderer-cases.js")
                        .to_vec(),
                ),
                ("report", report),
            ] {
                let binding = if key == "report" {
                    &record.value[key]
                } else {
                    &record.value[key]["file"]
                };
                let id = binding["id"].as_str().unwrap().to_string();
                rows.push(json!({"id":id,"target":{"kind":"job","id":packet},"deleted_at":null,"sha256":binding["sha256"],"size_bytes":binding["bytes"]}));
                blobs.insert(id, bytes);
            }
            if fault == 2 {
                rows[0]["target"]["id"] = json!(uuid::Uuid::new_v4());
            }
            if fault == 3 {
                rows[0]["deleted_at"] = json!("2026-10-15T00:00:00Z");
            }
            if fault == 4 {
                rows.pop();
            }
            let app = Router::new().fallback(move |request: axum::extract::Request| {
                let rows = rows.clone();
                let blobs = blobs.clone();
                async move {
                    assert_eq!(request.method(), axum::http::Method::GET);
                    assert!(request.headers().get("x-boss-machine-token").is_none());
                    if request.uri().path() == "/api/files" {
                        return Json(rows).into_response();
                    }
                    if fault == 5 {
                        return (
                            axum::http::StatusCode::FOUND,
                            [("location", "/api/files")],
                            "redirect",
                        )
                            .into_response();
                    }
                    let id = request.uri().path().rsplit('/').next().unwrap();
                    let bytes = if fault == 1 {
                        b"changed".to_vec()
                    } else {
                        blobs[id].clone()
                    };
                    bytes.into_response()
                }
            });
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let http = crate::sor_client::machine_client().unwrap();
            let result = check_files_at(&http, &base, &record, &packet, "agent-reviewer").await;
            assert_eq!(result.is_ok(), fault == 0, "fault {fault}: {result:?}");
            server.abort();
        }
    }
}
