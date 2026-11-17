//! One evidence reference shape, verified on read (backlog 4f8783fb,
//! design 6ab85352, car 2 — this half: the shape, the port and the
//! writer/reader helpers; the CLI adapter and `dispatch_brief.rs` moving
//! onto it are the next car).
//!
//! WHY IT EXISTS. Every Job write stages its whole row as one state
//! event, so a packet that carries large values reaches the 983,040-byte
//! event bound and then takes no write at all. Car 1 put a budget under
//! that bound. Keeping an original WHOLE but out of the row needs the
//! file store, and on 2026-11-13 about 40 open packets already carried
//! store references spelled six ways (`bytes` or `size_bytes`,
//! `readback`, `native_readback`, `all_members_verified`, ...). A
//! writer, a reader and the rebuild can agree only on ONE shape, so it
//! lives here, in the crate every one of them depends on:
//!
//! ```json
//! {"$evidence": {"file_id": "<uuid>", "sha256": "<64 hex>",
//!                "size_bytes": 123, "mime": "application/json",
//!                "encoding": "json-v1"}}
//! ```
//!
//! The hash covers the exact stored bytes. For `json-v1` those bytes are
//! `serde_json::to_vec` of the value exactly as the row held it, and a
//! reader re-parses them; for `bytes` they are the original bytes.
//!
//! The writer never writes a reference it has not read back: it uploads,
//! compares the store's own claim, downloads, and hashes what came back
//! — and only then hands the reference to the caller. A failure at any
//! leg names the file id it left behind and returns no reference, so the
//! row keeps its inline original (design 6ab85352 point 3). The server
//! never does this on a writer's behalf: the jobs service gains no
//! file-store dependency, and the store is reached through
//! [`EvidenceStore`], the port each writer adapts.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// The one key a reference object carries.
pub const EVIDENCE_KEY: &str = "$evidence";

/// A single value whose encoding is larger than this is kept in the
/// store by reference rather than inline (design 6ab85352 point 3): it
/// keeps ordinary prose inline and sits far under the store's 2 MiB
/// part limit.
pub const EXTERNALISE_ABOVE_BYTES: usize = 64 * 1024;

/// The mime a `json-v1` reference records.
pub const JSON_MIME: &str = "application/json";

/// How the stored bytes decode back into the original.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Encoding {
    /// `serde_json::to_vec` of a JSON value; read back by re-parsing.
    #[serde(rename = "json-v1")]
    JsonV1,
    /// The original bytes, as they are.
    #[serde(rename = "bytes")]
    Bytes,
}

/// The body of a `{"$evidence": …}` reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceRef {
    pub file_id: Uuid,
    pub sha256: String,
    pub size_bytes: u64,
    pub mime: String,
    pub encoding: Encoding,
}

/// What a store says it recorded for an upload — a claim, checked
/// against the bytes sent before anything trusts it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored {
    pub file_id: Uuid,
    pub sha256: String,
    pub size_bytes: u64,
}

/// The file store as an evidence writer and reader need it. An adapter
/// (the CLI's attach door, a dispatcher handler's client) implements it;
/// a store that answers a duplicate upload with the row it already holds
/// (backlog e5490f2e) returns that row here, and the read-back below is
/// what makes that answer trustworthy.
#[async_trait]
pub trait EvidenceStore: Send + Sync {
    async fn put(&self, bytes: &[u8], mime: &str) -> Result<Stored, String>;
    async fn get(&self, file_id: Uuid) -> Result<Vec<u8>, String>;
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EvidenceError {
    #[error("not an evidence reference: {0}")]
    Malformed(String),
    #[error("the value could not be encoded for the store: {0}")]
    Encode(String),
    #[error("the file store refused the upload: {0}; no reference was written")]
    StoreRefused(String),
    #[error(
        "evidence {file_id} is unreadable: {reason}; the reference names bytes the store did not return"
    )]
    Unreadable { file_id: Uuid, reason: String },
    #[error(
        "evidence {file_id} disagrees with its reference: expected {expected_size} bytes sha256 \
         {expected_sha256}, found {found_size} bytes sha256 {found_sha256}"
    )]
    Mismatch {
        file_id: Uuid,
        expected_sha256: String,
        expected_size: u64,
        found_sha256: String,
        found_size: u64,
    },
    #[error("evidence {file_id} is not the json-v1 value it claims: {reason}")]
    NotJson { file_id: Uuid, reason: String },
}

/// Lowercase hex sha256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
}

fn json_bytes(value: &Value) -> Result<Vec<u8>, EvidenceError> {
    serde_json::to_vec(value).map_err(|e| EvidenceError::Encode(e.to_string()))
}

impl EvidenceRef {
    /// `{"$evidence": {…}}`.
    pub fn to_value(&self) -> Value {
        let body = serde_json::json!({
            "file_id": self.file_id,
            "sha256": self.sha256,
            "size_bytes": self.size_bytes,
            "mime": self.mime,
            "encoding": self.encoding,
        });
        serde_json::json!({ EVIDENCE_KEY: body })
    }

    /// `None` when `value` is not a reference at all (an ordinary value,
    /// or one of the older hand spellings, which stay readable as they
    /// are); `Some(Err)` when it claims the shape and breaks it.
    pub fn from_value(value: &Value) -> Option<Result<Self, EvidenceError>> {
        let object = value.as_object()?;
        let body = object.get(EVIDENCE_KEY)?;
        Some(Self::parse(object.len(), body))
    }

    fn parse(keys: usize, body: &Value) -> Result<Self, EvidenceError> {
        if keys != 1 {
            return Err(EvidenceError::Malformed(format!(
                "a reference is the one key {EVIDENCE_KEY}, and this object has {keys}"
            )));
        }
        let reference: Self = serde_json::from_value(body.clone())
            .map_err(|e| EvidenceError::Malformed(e.to_string()))?;
        if !is_sha256_hex(&reference.sha256) {
            return Err(EvidenceError::Malformed(format!(
                "sha256 {:?} is not 64 lowercase hex digits",
                reference.sha256
            )));
        }
        if reference.mime.trim().is_empty() {
            return Err(EvidenceError::Malformed("mime is empty".into()));
        }
        Ok(reference)
    }

    /// The bytes are the referenced ones: their count, then their hash.
    pub fn verify(&self, bytes: &[u8]) -> Result<(), EvidenceError> {
        let found_sha256 = sha256_hex(bytes);
        let found_size = bytes.len() as u64;
        if found_size == self.size_bytes && found_sha256 == self.sha256 {
            return Ok(());
        }
        Err(EvidenceError::Mismatch {
            file_id: self.file_id,
            expected_sha256: self.sha256.clone(),
            expected_size: self.size_bytes,
            found_sha256,
            found_size,
        })
    }

    /// Fetch the referenced bytes and verify them.
    pub async fn read(&self, store: &dyn EvidenceStore) -> Result<Vec<u8>, EvidenceError> {
        let bytes = store
            .get(self.file_id)
            .await
            .map_err(|reason| EvidenceError::Unreadable {
                file_id: self.file_id,
                reason,
            })?;
        self.verify(&bytes)?;
        Ok(bytes)
    }

    /// Fetch, verify and re-parse a `json-v1` original.
    pub async fn read_json(&self, store: &dyn EvidenceStore) -> Result<Value, EvidenceError> {
        if self.encoding != Encoding::JsonV1 {
            return Err(EvidenceError::NotJson {
                file_id: self.file_id,
                reason: format!("its encoding is {:?}", self.encoding),
            });
        }
        let bytes = self.read(store).await?;
        serde_json::from_slice(&bytes).map_err(|e| EvidenceError::NotJson {
            file_id: self.file_id,
            reason: e.to_string(),
        })
    }
}

/// Keep `value` inline when it encodes to at most
/// [`EXTERNALISE_ABOVE_BYTES`]; otherwise store it and return its
/// verified reference.
pub async fn externalise_if_large(
    store: &dyn EvidenceStore,
    value: &Value,
) -> Result<Value, EvidenceError> {
    if json_bytes(value)?.len() <= EXTERNALISE_ABOVE_BYTES {
        return Ok(value.clone());
    }
    Ok(externalise_json(store, value).await?.to_value())
}

/// Store `value` as `json-v1` and return its reference, read back and
/// verified first.
pub async fn externalise_json(
    store: &dyn EvidenceStore,
    value: &Value,
) -> Result<EvidenceRef, EvidenceError> {
    let bytes = json_bytes(value)?;
    let reference = EvidenceRef {
        // Replaced by the store's id below; the hash and count are ours.
        file_id: Uuid::nil(),
        sha256: sha256_hex(&bytes),
        size_bytes: bytes.len() as u64,
        mime: JSON_MIME.into(),
        encoding: Encoding::JsonV1,
    };
    let stored = store
        .put(&bytes, JSON_MIME)
        .await
        .map_err(EvidenceError::StoreRefused)?;
    let reference = EvidenceRef {
        file_id: stored.file_id,
        ..reference
    };
    // The store's claim first, then the bytes themselves: a duplicate
    // upload's "already held" answer counts only once both agree.
    if stored.sha256 != reference.sha256 || stored.size_bytes != reference.size_bytes {
        return Err(EvidenceError::Mismatch {
            file_id: stored.file_id,
            expected_sha256: reference.sha256,
            expected_size: reference.size_bytes,
            found_sha256: stored.sha256,
            found_size: stored.size_bytes,
        });
    }
    reference.read(store).await?;
    Ok(reference)
}

/// The original a value stands for: itself when it is not a reference,
/// the verified, re-parsed stored value when it is.
pub async fn resolve(store: &dyn EvidenceStore, value: &Value) -> Result<Value, EvidenceError> {
    match EvidenceRef::from_value(value) {
        None => Ok(value.clone()),
        Some(reference) => reference?.read_json(store).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// The file store in memory, with the faults the writer must refuse.
    #[derive(Default)]
    struct Memory {
        files: Mutex<HashMap<Uuid, Vec<u8>>>,
        puts: Mutex<usize>,
        refuse_put: bool,
        refuse_get: bool,
        /// The store's claim after an upload is not the bytes sent.
        lie_on_put: bool,
        /// The bytes read back are not the bytes stored.
        corrupt_on_get: bool,
    }

    #[async_trait]
    impl EvidenceStore for Memory {
        async fn put(&self, bytes: &[u8], _mime: &str) -> Result<Stored, String> {
            *self.puts.lock().unwrap() += 1;
            if self.refuse_put {
                return Err("POST /api/files -> 503".into());
            }
            let file_id = Uuid::new_v4();
            self.files.lock().unwrap().insert(file_id, bytes.to_vec());
            let size_bytes = bytes.len() as u64 + u64::from(self.lie_on_put);
            Ok(Stored {
                file_id,
                sha256: sha256_hex(bytes),
                size_bytes,
            })
        }
        async fn get(&self, file_id: Uuid) -> Result<Vec<u8>, String> {
            if self.refuse_get {
                return Err("GET /api/files -> 404".into());
            }
            let mut bytes = self
                .files
                .lock()
                .unwrap()
                .get(&file_id)
                .cloned()
                .ok_or_else(|| "GET /api/files -> 404".to_string())?;
            if self.corrupt_on_get
                && let Some(b) = bytes.first_mut()
            {
                *b ^= 1;
            }
            Ok(bytes)
        }
    }

    /// A value the shape of the measured over-bound evidence on 6cd6c935
    /// (146,766 bytes, the largest of its six): one long prose string.
    fn large() -> Value {
        json!({"diagnostic": "é — original receipt line\r\n".repeat(5_500)})
    }

    #[tokio::test]
    async fn a_value_over_64_kib_is_stored_read_back_and_referenced_by_its_hash() {
        let store = Memory::default();
        let value = large();
        let bytes = serde_json::to_vec(&value).unwrap();
        assert!(bytes.len() > EXTERNALISE_ABOVE_BYTES);
        let written = externalise_if_large(&store, &value).await.unwrap();
        let reference = EvidenceRef::from_value(&written)
            .expect("the written value is a reference")
            .unwrap();
        assert_eq!(reference.sha256, sha256_hex(&bytes));
        assert_eq!(reference.size_bytes, bytes.len() as u64);
        assert_eq!(reference.encoding, Encoding::JsonV1);
        assert_eq!(reference.mime, JSON_MIME);
        assert_eq!(
            written.as_object().map(|o| o.len()),
            Some(1),
            "a reference is one key"
        );
        // The original, whole, from the store — not from the row.
        assert_eq!(resolve(&store, &written).await.unwrap(), value);
        // And the reference is small enough to change a packet's size.
        assert!(serde_json::to_vec(&written).unwrap().len() < 300);
    }

    #[tokio::test]
    async fn a_value_at_the_threshold_stays_inline_and_never_touches_the_store() {
        let store = Memory {
            refuse_put: true,
            ..Memory::default()
        };
        // `"` + n + `"` encodes to exactly the threshold.
        let value = json!("x".repeat(EXTERNALISE_ABOVE_BYTES - 2));
        assert_eq!(
            serde_json::to_vec(&value).unwrap().len(),
            EXTERNALISE_ABOVE_BYTES
        );
        assert_eq!(externalise_if_large(&store, &value).await.unwrap(), value);
        assert_eq!(*store.puts.lock().unwrap(), 0);
        assert_eq!(resolve(&store, &value).await.unwrap(), value);
    }

    #[tokio::test]
    async fn a_store_that_refuses_the_upload_writes_no_reference_and_says_so() {
        let store = Memory {
            refuse_put: true,
            ..Memory::default()
        };
        let err = externalise_if_large(&store, &large()).await.unwrap_err();
        assert!(matches!(err, EvidenceError::StoreRefused(_)), "{err}");
        assert!(err.to_string().contains("503"), "{err}");
    }

    #[tokio::test]
    async fn a_store_claim_that_is_not_the_bytes_sent_writes_no_reference() {
        let store = Memory {
            lie_on_put: true,
            ..Memory::default()
        };
        let err = externalise_if_large(&store, &large()).await.unwrap_err();
        let EvidenceError::Mismatch { file_id, .. } = err else {
            panic!("expected a mismatch, got {err}");
        };
        // The bytes it left behind are named, so they can be found.
        assert!(store.files.lock().unwrap().contains_key(&file_id));
    }

    #[tokio::test]
    async fn a_mismatched_read_back_writes_no_reference_and_names_what_it_left() {
        let store = Memory {
            corrupt_on_get: true,
            ..Memory::default()
        };
        let err = externalise_if_large(&store, &large()).await.unwrap_err();
        let EvidenceError::Mismatch { file_id, .. } = &err else {
            panic!("expected a mismatch, got {err}");
        };
        assert!(err.to_string().contains(&file_id.to_string()), "{err}");
    }

    #[tokio::test]
    async fn an_unreadable_read_back_writes_no_reference() {
        let store = Memory {
            refuse_get: true,
            ..Memory::default()
        };
        let err = externalise_if_large(&store, &large()).await.unwrap_err();
        assert!(matches!(err, EvidenceError::Unreadable { .. }), "{err}");
    }

    #[tokio::test]
    async fn a_reader_refuses_a_missing_or_changed_original_by_name() {
        let store = Memory::default();
        let written = externalise_if_large(&store, &large()).await.unwrap();
        let reference = EvidenceRef::from_value(&written).unwrap().unwrap();

        // Missing: the store no longer holds the bytes.
        store.files.lock().unwrap().clear();
        let err = resolve(&store, &written).await.unwrap_err();
        assert_eq!(
            err,
            EvidenceError::Unreadable {
                file_id: reference.file_id,
                reason: "GET /api/files -> 404".into()
            }
        );

        // Changed: the store holds other bytes under the id.
        store
            .files
            .lock()
            .unwrap()
            .insert(reference.file_id, b"{\"diagnostic\":\"edited\"}".to_vec());
        let err = resolve(&store, &written).await.unwrap_err();
        assert!(matches!(err, EvidenceError::Mismatch { .. }), "{err}");

        // Same length, one byte different: the hash, not the count, says so.
        let mut same_len = serde_json::to_vec(&large()).unwrap();
        same_len[3] ^= 1;
        assert!(matches!(
            reference.verify(&same_len),
            Err(EvidenceError::Mismatch { .. })
        ));
        assert_eq!(
            reference.verify(&serde_json::to_vec(&large()).unwrap()),
            Ok(())
        );
    }

    #[tokio::test]
    async fn bytes_that_hash_right_but_are_not_json_are_refused_as_json_v1() {
        let store = Memory::default();
        let bytes = b"not json at all".to_vec();
        let file_id = Uuid::new_v4();
        store.files.lock().unwrap().insert(file_id, bytes.clone());
        let reference = EvidenceRef {
            file_id,
            sha256: sha256_hex(&bytes),
            size_bytes: bytes.len() as u64,
            mime: JSON_MIME.into(),
            encoding: Encoding::JsonV1,
        };
        let err = resolve(&store, &reference.to_value()).await.unwrap_err();
        assert!(matches!(err, EvidenceError::NotJson { .. }), "{err}");
    }

    #[test]
    fn the_shape_round_trips_and_nothing_else_is_taken_for_it() {
        let reference = EvidenceRef {
            file_id: Uuid::nil(),
            sha256: "a".repeat(64),
            size_bytes: 10,
            mime: JSON_MIME.into(),
            encoding: Encoding::JsonV1,
        };
        let value = reference.to_value();
        assert_eq!(
            value,
            json!({"$evidence": {"file_id": Uuid::nil(), "sha256": "a".repeat(64),
                   "size_bytes": 10, "mime": "application/json", "encoding": "json-v1"}})
        );
        assert_eq!(EvidenceRef::from_value(&value), Some(Ok(reference)));

        // Ordinary values, and the hand spellings already on packets
        // (dispatch_brief.rs's `{kind: "attachment", …}` among them), are
        // not references: they stay readable as they are.
        for plain in [
            json!("text"),
            json!(null),
            json!({"kind": "attachment", "file_id": Uuid::nil(), "sha256": "a".repeat(64),
                   "size_bytes": 10, "encoding": "utf-8"}),
            json!({"evidence": {}}),
        ] {
            assert_eq!(EvidenceRef::from_value(&plain), None, "{plain}");
        }

        // Claiming the shape and breaking it is an error, not a pass.
        let good = value[EVIDENCE_KEY].clone();
        let mut broken = vec![
            json!({"$evidence": good.clone(), "other": 1}),
            json!({"$evidence": "file"}),
        ];
        for (key, bad) in [
            ("sha256", json!("A".repeat(64))),
            ("sha256", json!("a".repeat(63))),
            ("file_id", json!("https://other.invalid/x")),
            ("encoding", json!("utf-8")),
            ("mime", json!("")),
            ("url", json!("https://other.invalid/x")),
        ] {
            let mut body = good.clone();
            body[key] = bad;
            broken.push(json!({ "$evidence": body }));
        }
        for bad in broken {
            assert!(
                matches!(
                    EvidenceRef::from_value(&bad),
                    Some(Err(EvidenceError::Malformed(_)))
                ),
                "{bad}"
            );
        }
    }
}
