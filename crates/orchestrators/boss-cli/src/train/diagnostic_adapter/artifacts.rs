//! Lossless transport for originals larger than one content-store file.
//! Only bounded references enter events; every reader verifies the whole body.
use super::*;

const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_CHUNKS: usize = 64;

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    kind: String,
    schema: u32,
    size_bytes: usize,
    sha256: String,
    chunks: Vec<Chunk>,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Chunk {
    index: usize,
    offset: usize,
    file: Value,
}

fn check(manifest: &Manifest) -> Result<()> {
    if manifest.kind != "chunked-json"
        || manifest.schema != 1
        || manifest.size_bytes == 0
        || manifest.size_bytes > MAX_BYTES
        || manifest.chunks.is_empty()
        || manifest.chunks.len() > MAX_CHUNKS
        || manifest.sha256.len() != 64
        || !manifest
            .sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        bail!("diagnostic chunk manifest identity or bound differs");
    }
    let mut offset = 0usize;
    for (index, chunk) in manifest.chunks.iter().enumerate() {
        let bytes = chunk.file["size_bytes"]
            .as_u64()
            .context("chunk byte count absent")?;
        if chunk.index != index
            || chunk.offset != offset
            || bytes == 0
            || bytes > crate::attach::largest_file()
            || chunk.file["kind"] != "attachment"
            || chunk.file["encoding"] != "utf-8"
        {
            bail!("diagnostic chunk order, offset or bound differs");
        }
        offset = offset
            .checked_add(usize::try_from(bytes)?)
            .context("chunk size overflow")?;
        if offset > manifest.size_bytes {
            bail!("diagnostic chunks exceed declared whole body");
        }
    }
    if offset != manifest.size_bytes {
        bail!("diagnostic chunk manifest has missing bytes");
    }
    Ok(())
}

async fn text(door: &NativeDoor<'_>, reference: &Value) -> Result<String> {
    crate::dispatch_brief::read_at(
        &door.conductor.http,
        &door.content,
        &crate::identity::header(&door.actor),
        reference,
    )
    .await
}

async fn existing_file(door: &NativeDoor<'_>, piece: &str, digest: &str) -> Result<Option<Value>> {
    // An upload may have committed before its part record was acknowledged.
    // Reconcile only this packet's complete native file list; never search
    // another target or treat a failed/partial listing as absence.
    let url = format!(
        "{}/api/files?target_kind=job&target_id={}",
        door.content.trim_end_matches('/'),
        door.id()?
    );
    door.journal(json!({"phase":"content-file-census-request","target":door.id()?}))
        .await?;
    let http: &boss_core::machine_token::Client = &door.conductor.http;
    let mut response = match http
        .get(url)
        .header("x-boss-user", crate::identity::header(&door.actor))
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            door.journal(json!({"phase":"content-file-census-error","target":door.id()?,"error":format!("{error:?}"),"complete":false})).await?;
            return Err(error.into());
        }
    };
    let status = response.status();
    let mut bytes = Vec::new();
    loop {
        let chunk = match response.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(error) => {
                door.journal(json!({"phase":"content-file-census-response","target":door.id()?,"status":status.as_u16(),"complete":false,"error":format!("{error:?}"),"response_b64":base64::Engine::encode(&base64::engine::general_purpose::STANDARD,&bytes)})).await?;
                return Err(error.into());
            }
        };
        if bytes.len() + chunk.len() > boss_events::outbox::MAX_STAGED_EVENT_BYTES {
            door.journal(json!({"phase":"content-file-census-response","target":door.id()?,"status":status.as_u16(),"complete":false,"reason":"response exceeds byte bound","response_b64":base64::Engine::encode(&base64::engine::general_purpose::STANDARD,&bytes)})).await?;
            bail!("diagnostic original file census exceeds bound");
        }
        bytes.extend_from_slice(&chunk);
    }
    door.journal(json!({"phase":"content-file-census-response","target":door.id()?,"status":status.as_u16(),"complete":true,"response_b64":base64::Engine::encode(&base64::engine::general_purpose::STANDARD,&bytes)})).await?;
    if !status.is_success() {
        bail!("diagnostic original file census HTTP {status}; full response retained");
    }
    let rows: Vec<Value> = serde_json::from_slice(&bytes)?;
    let matches: Vec<_> = rows.iter().filter(|row| row["sha256"] == digest).collect();
    if matches.len() > 1 {
        bail!("diagnostic original file identity is duplicated");
    }
    let Some(row) = matches.first() else {
        return Ok(None);
    };
    if row["target"]["kind"] != "job"
        || row["target"]["id"] != door.id()?
        || row["size_bytes"] != piece.len()
    {
        bail!("diagnostic original file target or byte count differs");
    }
    let reference = json!({"kind":"attachment","file_id":row["id"],"sha256":digest,"size_bytes":piece.len(),"encoding":"utf-8"});
    if text(door, &reference).await? != piece {
        bail!("diagnostic existing file differs from complete original");
    }
    Ok(Some(reference))
}

pub(super) async fn read(door: &NativeDoor<'_>, value: &Value) -> Result<Value> {
    let manifest: Manifest = serde_json::from_value(value.clone())?;
    // Validate every bound and position before the first attachment read.
    check(&manifest)?;
    let mut bytes = Vec::with_capacity(manifest.size_bytes);
    for chunk in &manifest.chunks {
        bytes.extend_from_slice(text(door, &chunk.file).await?.as_bytes());
    }
    if bytes.len() != manifest.size_bytes || hex::encode(Sha256::digest(&bytes)) != manifest.sha256
    {
        bail!("diagnostic complete chunked original hash differs");
    }
    Ok(serde_json::from_slice(&bytes)?)
}

pub(super) async fn retain(door: &NativeDoor<'_>, raw: &[u8]) -> Result<Value> {
    if raw.is_empty() || raw.len() > MAX_BYTES {
        bail!("diagnostic whole original exceeds bounded chunk transport; nothing omitted");
    }
    let body = std::str::from_utf8(raw)?;
    let sha256 = hex::encode(Sha256::digest(raw));
    let mut chunks = Vec::new();
    let mut offset = 0;
    while offset < raw.len() {
        if chunks.len() == MAX_CHUNKS {
            bail!("diagnostic chunk count bound exceeded; nothing omitted");
        }
        let mut end = raw
            .len()
            .min(offset + usize::try_from(crate::attach::largest_file())?);
        while !body.is_char_boundary(end) {
            end -= 1;
        }
        let piece = &body[offset..end];
        // Immutable part records let an interrupted upload resume without
        // replacing references already witnessed by the system of record.
        let digest = hex::encode(Sha256::digest(piece.as_bytes()));
        let key = format!("diagnostic.chunks.{digest}");
        let kept = match door.original(&key).await? {
            Some(kept) => kept,
            None => {
                let reference = match existing_file(door, piece, &digest).await? {
                    Some(reference) => reference,
                    None => door.attach_bytes(piece.as_bytes().to_vec()).await?,
                };
                Box::pin(door.keep(&key, &json!({"chunk":reference}))).await?
            }
        };
        if kept.as_object().is_none_or(|o| o.len() != 1)
            || text(door, &kept["chunk"]).await? != piece
        {
            bail!("diagnostic retained chunk differs from original bytes");
        }
        chunks.push(Chunk {
            index: chunks.len(),
            offset,
            file: kept["chunk"].clone(),
        });
        offset = end;
    }
    let manifest = Manifest {
        kind: "chunked-json".into(),
        schema: 1,
        size_bytes: raw.len(),
        sha256,
        chunks,
    };
    check(&manifest)?;
    let value = serde_json::to_value(manifest)?;
    if serde_json::to_vec(&value)?.len() > boss_events::outbox::MAX_STAGED_EVENT_BYTES / 4 {
        bail!("diagnostic chunk manifest exceeds event reference bound");
    }
    if read(door, &value).await? != serde_json::from_slice::<Value>(raw)? {
        bail!("diagnostic whole chunked original readback differs");
    }
    Ok(value)
}
