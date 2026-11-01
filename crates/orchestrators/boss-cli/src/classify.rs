//! `boss classify` — classify ONE FROZEN VALUE of a packet as an original
//! its executors' bounded START lists instead of printing (design e72dc7c9,
//! backlog 40f0436c).
//!
//! WHY. On 2026-10-15 two urgent repairs' STARTs were a megabyte each,
//! nearly all of it historical coordinator observations, and the worker
//! receipt asked for every byte to be read first. Under the opt-in
//! execution-inputs/v1 contract a value no source classifies is REQUIRED
//! and printed; this verb is the operator's door for saying that one
//! particular recorded value is a mandatory original (read in full, not
//! printed) or an available one (retained, retrievable, not required
//! reading). It is the `POST /api/jobs/{id}/input-classifications` door
//! from a terminal: the SERVER asks publish on workflow and judges every
//! refusal — a protected value, a pointer naming nothing, a digest the
//! value no longer hashes to — and this verb keeps no second copy of them.
//!
//! It READS the value first and sends that value's sha256, so what is
//! classified is exactly what the operator was shown: a value that changed
//! between the read and the write is refused, never classified blind. And
//! it reads the packet back — a 201 is a claim; the entry is the fact.
//!
//! `--purpose` and `--reason` are prose, so each has a `-file` twin
//! (backlog 2376b89e): SINGLE-quote them or pass a file.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use boss_jobs::execution_inputs as ei;

use crate::steps::Wire;

#[derive(clap::Subcommand)]
pub enum Cmd {
    /// Classify one recorded value of a packet for its executors' bounded START (asks publish on workflow).
    ///
    /// `--pointer` names one key of the packet's metadata
    /// (`/metadata/<key>`) or of a completed step's
    /// (`/steps/<slug>/metadata/<key>`). The value is read, its size and
    /// sha256 printed, and the classification is bound to exactly that
    /// value: if it changes, the classification stops applying and the
    /// value is printed in full again. A correction, a decision, the
    /// item's description and anything the current step's predicate reads
    /// are refused — they never leave the START.
    Classify {
        /// The packet: the full uuid, or 8+ characters of it.
        packet: String,
        /// The value, as an RFC 6901 pointer (`/metadata/<key>` or `/steps/<slug>/metadata/<key>`).
        #[arg(long)]
        pointer: String,
        /// `available_historical` (retained, not required reading) or `mandatory_original` (read in full, not printed).
        #[arg(long)]
        class: String,
        /// What the value is, for the reader who meets it in the manifest.
        #[arg(long, conflicts_with = "purpose_file")]
        purpose: Option<String>,
        /// The purpose, read from this file. Exclusive with --purpose.
        #[arg(long)]
        purpose_file: Option<std::path::PathBuf>,
        /// Why it need not be printed — what makes it historical rather than current.
        #[arg(long, conflicts_with = "reason_file")]
        reason: Option<String>,
        /// The reason, read from this file. Exclusive with --reason.
        #[arg(long)]
        reason_file: Option<std::path::PathBuf>,
    },
}

pub async fn dispatch(cmd: Cmd) -> Result<()> {
    let Cmd::Classify {
        packet,
        pointer,
        class,
        purpose,
        purpose_file,
        reason,
        reason_file,
    } = cmd;
    let purpose = crate::prose::text_or_file(
        "--purpose",
        "--purpose-file",
        purpose,
        purpose_file.as_deref(),
    )?;
    let reason =
        crate::prose::text_or_file("--reason", "--reason-file", reason, reason_file.as_deref())?;
    let wire = Wire::live()?;
    println!(
        "{}",
        classify(&wire, &packet, &pointer, &class, &purpose, &reason).await?
    );
    Ok(())
}

/// Read, post, read back, and say what the record now holds.
pub(crate) async fn classify(
    wire: &Wire,
    packet: &str,
    pointer: &str,
    class: &str,
    purpose: &str,
    reason: &str,
) -> Result<String> {
    if ei::Class::parse(class).is_none() {
        bail!("--class is `available_historical` or `mandatory_original`, not {class:?}");
    }
    let job = wire
        .call(reqwest::Method::GET, &format!("/api/jobs/{packet}"), None)
        .await?
        .with_context(|| format!("packet {packet} read back empty"))?;
    let job_id = crate::envelope::job_id(&job)
        .context("the packet has no id")?
        .to_string();
    let value = ei::resolve(&job, pointer).with_context(|| {
        format!(
            "packet {} holds no single value at {pointer} — name one key of its metadata or of \
             a completed step's",
            short(&job_id)
        )
    })?;
    let bytes = ei::value_bytes(value);
    let sha256 = ei::sha256_hex(&bytes);
    let answer = wire
        .call(
            reqwest::Method::POST,
            &format!("/api/jobs/{job_id}/input-classifications"),
            Some(json!({
                "pointer": pointer,
                "sha256": sha256,
                "class": class,
                "purpose": purpose,
                "reason": reason,
            })),
        )
        .await?
        .context("the classification door answered with no body")?;
    let index = answer
        .get("index")
        .and_then(Value::as_u64)
        .context("the classification door answered without an index")?;
    let after = wire
        .call(reqwest::Method::GET, &format!("/api/jobs/{job_id}"), None)
        .await?
        .context("the packet read back empty")?;
    let held = after
        .get("metadata")
        .and_then(|m| m.get(ei::CLASSIFICATIONS_KEY))
        .and_then(Value::as_array)
        .and_then(|l| l.get(index as usize));
    if held.and_then(|e| e.get("sha256")).and_then(Value::as_str) != Some(sha256.as_str())
        || held.and_then(|e| e.get("pointer")).and_then(Value::as_str) != Some(pointer)
    {
        bail!(
            "the door answered index {index}, but packet {} does not read back that \
             classification — not recorded",
            short(&job_id)
        );
    }
    Ok(format!(
        "classified {pointer} on packet {} as {class} — {} bytes, sha256 {sha256}, entry [{index}]. \
         It applies while the value stays exactly this; a bounded START lists it in THE ORIGINALS.",
        short(&job_id),
        bytes.len()
    ))
}

fn short(id: &str) -> &str {
    &id[..id.len().min(8)]
}
