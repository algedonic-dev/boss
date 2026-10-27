//! Large briefs remain complete attachments, rather than oversized bus events.
use std::io::Write;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

// Leave room for the server's admission stamp and Job envelope. The
// outbox still judges the actual event; this never changes its bound.
const INLINE_BODY_BYTES: usize = boss_events::outbox::MAX_STAGED_EVENT_BYTES / 2;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BriefFile {
    kind: String,
    file_id: String,
    sha256: String,
    size_bytes: u64,
    encoding: String,
}

pub(crate) async fn retain_at(
    http: &crate::machine_http::MachineClient,
    content_base: &str,
    actor: &str,
    packet: &str,
    mut body: Value,
) -> Result<Value> {
    if serde_json::to_vec(&body)?.len() <= INLINE_BODY_BYTES {
        return Ok(body);
    }
    let brief = body["metadata"]["brief"]
        .as_str()
        .context("run has no complete inline brief")?
        .to_string();
    let (reference, reused) = retain_text_held_at(http, content_base, actor, packet, &brief)
        .await
        .context("retaining the entire dispatch brief before run admission")?;
    body["metadata"]["brief"] = reference.clone();
    if serde_json::to_vec(&body)?.len() > INLINE_BODY_BYTES {
        bail!("run metadata remains oversized after retaining its full brief; no run admitted");
    }
    // A second dispatch of identical bytes holds the attachment the
    // packet already has, read back and verified (backlog e5490f2e).
    let held = if reused { "already held" } else { "retained" };
    eprintln!(
        "boss dispatch: full brief {held} as attachment {} ({} bytes, sha256 {}); the run records the verified reference, not omitted prose",
        reference["file_id"].as_str().unwrap_or("?"),
        reference["size_bytes"],
        reference["sha256"].as_str().unwrap_or("?")
    );
    Ok(body)
}

/// Retain `text` WHOLE as a verified attachment on `packet` and return the
/// immutable reference [`read_at`] verifies — the one door every original a
/// run records goes through: an oversized brief, and a bounded START's
/// packet snapshot and legacy brief (design e72dc7c9).
pub(crate) async fn retain_text_at(
    http: &crate::machine_http::MachineClient,
    content_base: &str,
    actor: &str,
    packet: &str,
    text: &str,
) -> Result<Value> {
    Ok(retain_text_held_at(http, content_base, actor, packet, text)
        .await?
        .0)
}

/// [`retain_text_at`], also saying whether the file store already held
/// these exact bytes on `packet` (backlog e5490f2e) rather than storing
/// them anew.
async fn retain_text_held_at(
    http: &crate::machine_http::MachineClient,
    content_base: &str,
    actor: &str,
    packet: &str,
    text: &str,
) -> Result<(Value, bool)> {
    let mut file = tempfile::Builder::new()
        .prefix("boss-dispatch-brief-")
        .suffix(".txt")
        .tempfile()?;
    file.write_all(text.as_bytes())?;
    file.flush()?;
    let attached = crate::attach::attach_at(
        http,
        content_base,
        &crate::attach::Target {
            kind: "job",
            id: packet.to_string(),
            label: format!("packet {packet}"),
        },
        file.path(),
        crate::identity::Signature::As(actor.to_string()),
    )
    .await?;
    if attached.size_bytes != text.len() as u64
        || attached.sha256 != hex::encode(Sha256::digest(text.as_bytes()))
    {
        bail!(
            "the file store recorded {} bytes, sha256 {} for an original of {} bytes — not the \
             bytes sent; no run admitted",
            attached.size_bytes,
            attached.sha256,
            text.len()
        );
    }
    let reused = attached.reused;
    let reference = serde_json::to_value(BriefFile {
        kind: "attachment".into(),
        file_id: attached.file_id,
        sha256: attached.sha256,
        size_bytes: attached.size_bytes,
        encoding: "utf-8".into(),
    })?;
    Ok((reference, reused))
}

/// A native executor accepts the original string or a verified complete
/// attachment. URLs never ride the reference: the existing service door
/// chooses the target, and the file id cannot select another host/path.
pub(crate) async fn read_at(
    http: &crate::machine_http::MachineClient,
    content_base: &str,
    actor: &str,
    value: &Value,
) -> Result<String> {
    if let Some(text) = value.as_str() {
        if text.trim().is_empty() {
            bail!("missing or malformed brief");
        }
        return Ok(text.to_string());
    }
    let reference: BriefFile = serde_json::from_value(value.clone())
        .context("registered brief is neither text nor an attachment reference")?;
    if reference.kind != "attachment"
        || reference.encoding != "utf-8"
        || uuid::Uuid::parse_str(&reference.file_id).is_err()
        || reference.sha256.len() != 64
        || !reference
            .sha256
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        || reference.size_bytes == 0
        || reference.size_bytes > crate::attach::largest_file()
    {
        bail!(
            "registered brief attachment has an invalid identity, digest, encoding or byte bound"
        );
    }
    let url = format!(
        "{}/api/files/{}",
        content_base.trim_end_matches('/'),
        reference.file_id
    );
    let mut response = http
        .get(&url)
        .header("x-boss-user", crate::identity::header(actor))
        .send()
        .await
        .context("reading the complete registered brief attachment")?;
    if !response.status().is_success() {
        bail!(
            "registered brief attachment {} is unavailable: GET -> {}; no executor may start",
            reference.file_id,
            response.status()
        );
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .context("reading registered brief bytes")?
    {
        if bytes.len() as u64 + chunk.len() as u64 > reference.size_bytes {
            bail!(
                "registered brief attachment exceeds its declared byte count; no executor may start"
            );
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.len() as u64 != reference.size_bytes
        || hex::encode(Sha256::digest(&bytes)) != reference.sha256
    {
        bail!(
            "registered brief attachment byte count or sha256 disagrees with its immutable reference; no executor may start"
        );
    }
    let text =
        String::from_utf8(bytes).context("registered brief attachment is not complete UTF-8")?;
    if text.trim().is_empty() {
        bail!("missing or malformed brief");
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn body(brief: &str) -> Value {
        json!({"kind":"agent-run","metadata":{"packet":"39d0b528-ff69-4cb8-ba82-408b641da66c","brief":brief,"agent":"agent-test"}})
    }
    async fn serve(router: axum::Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        format!("http://{address}")
    }
    #[tokio::test]
    async fn small_inline_dispatch_and_legacy_read_have_no_storage_dependency() {
        let original = body("full ordinary brief\n");
        let http = crate::sor_client::machine_client().unwrap();
        assert_eq!(
            retain_at(
                &http,
                "http://127.0.0.1:9",
                "agent-test",
                "packet",
                original.clone()
            )
            .await
            .unwrap(),
            original
        );
        assert_eq!(
            read_at(
                &http,
                "http://127.0.0.1:9",
                "agent-test",
                &original["metadata"]["brief"]
            )
            .await
            .unwrap(),
            "full ordinary brief\n"
        );
        for bad in [json!(null), json!(""), json!(" \n"), json!({})] {
            assert!(
                read_at(&http, "http://127.0.0.1:9", "agent-test", &bad)
                    .await
                    .is_err()
            );
        }
    }
    #[tokio::test]
    async fn full_unicode_escaped_evidence_round_trips_through_the_real_store() {
        let brief = "{\"diagnostic\":\"é — original\"}\r\n".repeat(25_000);
        let http = crate::sor_client::machine_client().unwrap();
        let base = crate::attach::tests::store().await;
        let held = retain_at(
            &http,
            &base,
            "agent-test",
            "39d0b528-ff69-4cb8-ba82-408b641da66c",
            body(&brief),
        )
        .await
        .unwrap();
        assert!(held["metadata"]["brief"].is_object());
        assert_eq!(
            read_at(&http, &base, "agent-test", &held["metadata"]["brief"])
                .await
                .unwrap(),
            brief
        );
        let mut changed = held["metadata"]["brief"].clone();
        changed["sha256"] = json!("0".repeat(64));
        assert!(read_at(&http, &base, "agent-test", &changed).await.is_err());
        changed = held["metadata"]["brief"].clone();
        changed["size_bytes"] = json!(1);
        assert!(read_at(&http, &base, "agent-test", &changed).await.is_err());
        let id = held["metadata"]["brief"]["file_id"].as_str().unwrap();
        assert!(
            http.delete(format!("{base}/api/files/{id}"))
                .header("x-boss-user", crate::identity::header("agent-test"))
                .send()
                .await
                .unwrap()
                .status()
                .is_success()
        );
        assert!(
            read_at(&http, &base, "agent-test", &held["metadata"]["brief"])
                .await
                .is_err()
        );
    }
    /// Backlog e5490f2e, the measured case: a second dispatch of the
    /// same packet renders byte-identical brief bytes, and its retention
    /// was refused `POST /api/files -> 409 <digest>` BEFORE run admission.
    /// Through the real store it now holds the one verified attachment,
    /// and each body still carries its own complete reference.
    #[tokio::test]
    async fn a_second_dispatch_of_identical_brief_bytes_holds_the_one_verified_attachment() {
        let brief = "the same complete brief, rendered twice\n".repeat(30_000);
        let http = crate::sor_client::machine_client().unwrap();
        let base = crate::attach::tests::store().await;
        let packet = "39d0b528-ff69-4cb8-ba82-408b641da66c";
        let first = retain_at(&http, &base, "agent-test", packet, body(&brief))
            .await
            .unwrap();
        let second = retain_at(&http, &base, "agent-test", packet, body(&brief))
            .await
            .expect("the identical retention is a verified reuse, not a 409");
        assert_eq!(first["metadata"]["brief"], second["metadata"]["brief"]);
        for held in [&first, &second] {
            assert_eq!(
                read_at(&http, &base, "agent-test", &held["metadata"]["brief"])
                    .await
                    .unwrap(),
                brief
            );
        }
    }
    #[tokio::test]
    async fn an_unconfigured_store_and_its_existing_file_limit_refuse_without_reduction() {
        let router = axum::Router::new().route(
            "/api/files",
            axum::routing::post(|_: axum::body::Bytes| async {
                axum::Json(json!({"kind":"unconfigured"}))
            }),
        );
        let base = serve(router).await;
        let http = crate::sor_client::machine_client().unwrap();
        let large = "full evidence\n".repeat(50_000);
        let err = retain_at(
            &http,
            &base,
            "agent-test",
            "39d0b528-ff69-4cb8-ba82-408b641da66c",
            body(&large),
        )
        .await
        .unwrap_err();
        assert!(format!("{err:#}").contains("not switched on"));
        let too_large = "x".repeat(crate::attach::largest_file() as usize + 1);
        let err = retain_at(
            &http,
            "http://127.0.0.1:9",
            "agent-test",
            "39d0b528-ff69-4cb8-ba82-408b641da66c",
            body(&too_large),
        )
        .await
        .unwrap_err();
        assert!(format!("{err:#}").contains("Nothing was sent"));
    }
    #[tokio::test]
    async fn a_reference_cannot_choose_a_url_or_enlarge_the_read_bound() {
        let reference = json!({"kind":"attachment","file_id":"39d0b528-ff69-4cb8-ba82-408b641da66c","sha256":"0".repeat(64),"size_bytes":10,"encoding":"utf-8"});
        let http = crate::sor_client::machine_client().unwrap();
        for (key, value) in [
            ("file_id", json!("https://other.invalid/secret")),
            ("size_bytes", json!(crate::attach::largest_file() + 1)),
            ("encoding", json!("unknown")),
            ("url", json!("https://other.invalid/secret")),
            ("sha256", json!("bad")),
        ] {
            let mut bad = reference.clone();
            bad[key] = value;
            let err = read_at(&http, "http://127.0.0.1:9", "agent-test", &bad)
                .await
                .unwrap_err();
            assert!(
                !format!("{err:#}").contains("Connection refused"),
                "invalid reference must refuse before socket: {err:#}"
            );
        }
    }
}
