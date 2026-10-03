//! Bounded operator-copied native receipts (daa8f63f, decision aad7c419).
//! This adapter observes a scoped roster; it launches nothing and joins no
//! path alias to a registered task. The signed operator supplies the artifact,
//! not a cryptographic attestation by the provider.
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, path::PathBuf};

const KIND: &str = "runtime-roster-capture";
const SOURCE: &str = "codex.collaboration.list_agents";
const MAX_BYTES: u64 = 4 * 1024 * 1024;

#[derive(clap::Subcommand)]
pub enum Cmd {
    /// Inspect or record a bounded copied native roster receipt; no executor starts.
    Roster {
        #[command(subcommand)]
        command: Operation,
    },
}

#[derive(clap::Subcommand)]
pub enum Operation {
    /// Validate selected native session metadata plus the exact call/output pair.
    Inspect {
        #[arg(long)]
        transcript: PathBuf,
        #[arg(long)]
        call_id: String,
    },
    /// Record through the existing signed job door and verify immutable readback.
    Record {
        #[arg(long)]
        transcript: PathBuf,
        #[arg(long)]
        call_id: String,
    },
}

fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .with_context(|| format!("native receipt lacks typed {key}"))
}

fn unique(rows: &[Value], predicate: impl Fn(&Value) -> bool) -> Result<&Value> {
    let matches = rows.iter().filter(|v| predicate(v)).collect::<Vec<_>>();
    if matches.len() != 1 {
        bail!("native receipt requires exactly one matching envelope");
    }
    Ok(matches[0])
}

fn native_rows(raw: &Value) -> Result<Vec<Value>> {
    let agents = raw
        .get("agents")
        .and_then(Value::as_array)
        .context("native roster is incomplete")?;
    if agents.len() > 1000 {
        bail!("native roster exceeds the bounded snapshot");
    }
    let mut paths = BTreeSet::new();
    let mut result = Vec::new();
    for agent in agents {
        let path = text(agent, "agent_name")?;
        if !path.starts_with("/root")
            || (path != "/root" && !path.starts_with("/root/"))
            || path.len() > 1024
            || path.chars().any(char::is_control)
            || path
                .split('/')
                .skip(1)
                .any(|part| part.is_empty() || matches!(part, "." | ".."))
            || !paths.insert(path.to_string())
        {
            bail!("native roster path is malformed or duplicated");
        }
        let raw_status = agent.get("agent_status").context("native status absent")?;
        let status = match raw_status {
            Value::String(s) if s == "running" => "running",
            Value::Object(o)
                if o.len() == 1 && o.get("completed").is_some_and(Value::is_string) =>
            {
                "finished"
            }
            Value::String(s) if !s.is_empty() => "unknown",
            Value::Object(o) if !o.is_empty() => "unknown",
            _ => bail!("native status malformed"),
        };
        result.push(json!({"path":path,"status":status,"raw_status":raw_status,
            "native_id":null,"current_task":null,"observed_model":null}));
    }
    result.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    Ok(result)
}

fn parse(input: &str, call_id: &str, now: DateTime<Utc>) -> Result<Value> {
    let rows = input
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(serde_json::from_str::<Value>)
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("native artifact is not complete JSONL")?;
    let meta = unique(&rows, |v| v["type"] == "session_meta")?;
    let session = &meta["payload"];
    let session_id = text(session, "id")?;
    uuid::Uuid::parse_str(session_id).context("native session namespace unavailable")?;
    let source = session
        .get("source")
        .context("native session source unavailable")?;
    let namespace = if let Some(parent) = source.pointer("/subagent/thread_spawn/parent_thread_id")
    {
        let parent = parent
            .as_str()
            .context("native parent namespace malformed")?;
        uuid::Uuid::parse_str(parent).context("native parent namespace unavailable")?;
        parent
    } else if source.is_string() {
        session_id
    } else {
        bail!("native session namespace provenance unavailable");
    };
    let call = unique(&rows, |v| {
        v["type"] == "response_item"
            && v["payload"]["type"] == "function_call"
            && v["payload"]["call_id"] == call_id
    })?;
    let payload = &call["payload"];
    if !(payload["name"] == "collaboration.list_agents"
        || (payload["name"] == "list_agents" && payload["namespace"] == "collaboration"))
    {
        bail!("selected call is not the native roster tool");
    }
    let arguments: Value = serde_json::from_str(text(payload, "arguments")?)?;
    if arguments != json!({}) {
        bail!("filtered roster cannot establish the whole native namespace");
    }
    let response = unique(&rows, |v| {
        v["type"] == "response_item"
            && v["payload"]["type"] == "function_call_output"
            && v["payload"]["call_id"] == call_id
    })?;
    let from = DateTime::parse_from_rfc3339(text(call, "timestamp")?)?.with_timezone(&Utc);
    let to = DateTime::parse_from_rfc3339(text(response, "timestamp")?)?.with_timezone(&Utc);
    if from > to || to > now {
        bail!("native collection interval is inverted or future");
    }
    let raw = text(&response["payload"], "output")?;
    let normalized = serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|v| native_rows(&v).ok());
    let complete = normalized.is_some();
    let receipt = json!({"session":{"id":session_id,"source":source,"timestamp":session.get("timestamp")},"call":call,"response":response});
    let hash = hex::encode(Sha256::digest(serde_json::to_vec(&receipt)?));
    let digest = Sha256::digest(format!("BOSS roster capture v1:{session_id}:{call_id}"));
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 15) | 0x80;
    bytes[8] = (bytes[8] & 63) | 0x80;
    Ok(
        json!({"schema_version":1,"capture_id":uuid::Uuid::from_bytes(bytes).to_string(),
        "source":SOURCE,"namespace":namespace,"call_id":call_id,"collected_from":from.to_rfc3339(),
        "collected_to":to.to_rfc3339(),"complete":complete,"result":if complete {"captured"} else {"failed"},
        "failure":if complete {None} else {Some("native roster unreadable, malformed or incomplete")},
        "rows":normalized.unwrap_or_default(),"native_receipt":receipt,"payload_sha256":hash,
        "provenance":"operator-copied native transcript; no provider attestation"}),
    )
}

pub async fn dispatch(cmd: Cmd) -> Result<()> {
    let Cmd::Roster { command } = cmd;
    let (path, call, record) = match command {
        Operation::Inspect {
            transcript,
            call_id,
        } => (transcript, call_id, false),
        Operation::Record {
            transcript,
            call_id,
        } => (transcript, call_id, true),
    };
    let metadata = tokio::fs::metadata(&path).await?;
    if !metadata.is_file() || metadata.len() > MAX_BYTES {
        bail!(
            "selected native artifact exceeds 4 MiB; copy only session metadata and the paired call/output"
        );
    }
    // A bounded selected artifact contains no surrounding user transcript.
    let input = tokio::fs::read_to_string(path).await?;
    if input.len() as u64 > MAX_BYTES {
        bail!("selected native artifact grew beyond 4 MiB");
    }
    let snapshot = parse(&input, &call, boss_clock_client::wall_now())?;
    if record {
        record_snapshot(snapshot).await
    } else {
        println!("{}", serde_json::to_string(&snapshot)?);
        Ok(())
    }
}

fn step<'a>(job: &'a Value, slug: &str) -> Result<&'a Value> {
    let found = crate::envelope::steps(job)
        .into_iter()
        .filter(|s| s["spec_slug"] == slug)
        .collect::<Vec<_>>();
    if found.len() != 1 {
        bail!("capture requires one declared {slug} step");
    }
    Ok(found[0])
}

async fn record_snapshot(mut snapshot: Value) -> Result<()> {
    let actor = crate::identity::sign(&reqwest::Method::POST, "/api/jobs")?;
    // Read the same canonical event identity the jobs API records; a
    // signed `rule:name` is stamped as `automation:rule:name` there.
    let user: boss_policy_client::User = serde_json::from_str(&crate::identity::header(&actor))?;
    let event_actor = user
        .ambient_actor()
        .context("native recorder has no execution identity")?
        .to_string();
    let base = crate::gate::resolve_jobs_base(None)?;
    let http = crate::gate::machine_client_with(
        reqwest::Client::builder().timeout(std::time::Duration::from_secs(10)),
    )?;
    let id = text(&snapshot, "capture_id")?.to_string();
    let signature = crate::identity::Signature::As(actor.clone());
    let body = json!({"id":id,"kind":KIND,"subject":{"subject_kind":"custom","id":snapshot["namespace"]},
        "title":"Explicit native runtime roster snapshot","owner_id":actor,"priority":"standard","status":"open","tags":["native-roster"],
        "metadata":{"source":SOURCE,"namespace":snapshot["namespace"],"call_id":snapshot["call_id"],"payload_sha256":snapshot["payload_sha256"]}});
    crate::gate::api_at_signed(
        &http,
        &base,
        reqwest::Method::POST,
        "/api/jobs",
        Some(body.clone()),
        signature.clone(),
    )
    .await?;
    let read = || async {
        let held = crate::gate::api_at_signed(
            &http,
            &base,
            reqwest::Method::GET,
            &format!("/api/jobs/{id}"),
            None,
            signature.clone(),
        )
        .await?
        .context("capture read returned no JSON object")?;
        let (_, same) = crate::job::confirm_patch(&held["metadata"], &body["metadata"]);
        if held["id"] != id
            || held["kind"] != KIND
            || held["partition"] != "real"
            // Q7 resolves accountable ownership to a human. The filer,
            // independently stamped at admission, identifies this recorder.
            || held["metadata"]["opened_by"] != event_actor
            || !same
        {
            bail!("capture identity or conflicting native receipt refused");
        }
        Ok::<Value, anyhow::Error>(held)
    };
    let mut held = read().await?;
    let capture = step(&held, "capture")?;
    if let Some(existing) = capture["metadata"].get("snapshot") {
        let mut original = existing.clone();
        original
            .as_object_mut()
            .context("stored snapshot malformed")?
            .remove("received_at");
        if original != snapshot {
            bail!("capture ID already holds a conflicting native payload");
        }
        snapshot = existing.clone();
    } else {
        snapshot["received_at"] = json!(boss_clock_client::wall_now().to_rfc3339());
    }
    let sid = text(capture, "id")?.to_string();
    let path = format!("/api/jobs/{id}/steps/{sid}");
    if capture["status"] != "completed" {
        if capture["status"] == "ready" {
            crate::gate::api_at_signed(
                &http,
                &base,
                reqwest::Method::POST,
                &format!("{path}/claim"),
                None,
                signature.clone(),
            )
            .await?;
            held = read().await?;
        }
        let active = step(&held, "capture")?;
        if active["status"] != "active" || active["assignee_id"] != actor {
            bail!("capture is not owned by this recorder");
        }
        crate::gate::api_at_signed(
            &http,
            &base,
            reqwest::Method::PATCH,
            &format!("{path}/metadata"),
            Some(json!({"snapshot":snapshot,"result":snapshot["result"]})),
            signature.clone(),
        )
        .await?;
        held = read().await?;
        if step(&held, "capture")?["metadata"]["snapshot"] != snapshot
            || step(&held, "capture")?["metadata"]["result"] != snapshot["result"]
        {
            bail!("capture evidence did not read back");
        }
        crate::gate::api_at_signed(
            &http,
            &base,
            reqwest::Method::PUT,
            &path,
            Some(json!({"status":"completed"})),
            signature.clone(),
        )
        .await?;
        held = read().await?;
    }
    let captured = step(&held, "capture")?;
    if captured["status"] != "completed"
        || captured["completed_by"] != event_actor
        || captured["metadata"]["snapshot"] != snapshot
    {
        bail!("immutable capture completion did not read back");
    }
    let terminal = step(&held, "recorded")?;
    if terminal["status"] != "completed" {
        let tid = text(terminal, "id")?;
        crate::gate::api_at_signed(
            &http,
            &base,
            reqwest::Method::PUT,
            &format!("/api/jobs/{id}/steps/{tid}"),
            Some(json!({"status":"completed"})),
            signature.clone(),
        )
        .await?;
        held = read().await?;
    }
    if held["status"] != "closed" || step(&held, "recorded")?["status"] != "completed" {
        bail!("capture terminal did not read back");
    }
    println!("{}", serde_json::to_string(&snapshot)?);
    Ok(())
}
