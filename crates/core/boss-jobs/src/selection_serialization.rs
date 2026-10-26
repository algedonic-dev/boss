//! Serialization and hashing share the original ingress deadline, including large scalars.
//! It is read on tokio's clock, the one every deadline reader reads (f10419f9).
use crate::port::JobsError;
use sha2::{Digest, Sha256};
use std::io::Write;
use tokio::time::Instant;

pub(crate) fn check(deadline: Option<Instant>) -> Result<(), JobsError> {
    if deadline.is_some_and(|end| Instant::now() >= end) {
        Err(JobsError::SelectionConflict(
            "selection ingress deadline expired during evidence serialization".into(),
        ))
    } else {
        Ok(())
    }
}

struct Writer {
    bytes: Option<Vec<u8>>,
    hash: Sha256,
    deadline: Option<Instant>,
}
impl Write for Writer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        for part in bytes.chunks(64 * 1024) {
            check(self.deadline).map_err(|error| {
                std::io::Error::new(std::io::ErrorKind::TimedOut, error.to_string())
            })?;
            if let Some(output) = &mut self.bytes {
                output.extend_from_slice(part);
            }
            self.hash.update(part);
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn error(error: impl std::fmt::Display, deadline: Option<Instant>) -> JobsError {
    match check(deadline) {
        Err(expired) => expired,
        Ok(()) => JobsError::Storage(error.to_string()),
    }
}
pub(crate) fn json(
    value: &serde_json::Value,
    deadline: Option<Instant>,
) -> Result<String, JobsError> {
    let mut writer = Writer {
        bytes: Some(Vec::new()),
        hash: Sha256::new(),
        deadline,
    };
    emit(value, &mut writer, false).map_err(|e| error(e, deadline))?;
    check(deadline)?;
    String::from_utf8(
        writer.bytes.take().ok_or_else(|| {
            JobsError::Storage("selection serialization lost its artifact".into())
        })?,
    )
    .map_err(|e| JobsError::Storage(e.to_string()))
}
pub(crate) fn hash(bytes: &[u8], deadline: Option<Instant>) -> Result<String, JobsError> {
    let mut writer = Writer {
        bytes: None,
        hash: Sha256::new(),
        deadline,
    };
    writer.write_all(bytes).map_err(|e| error(e, deadline))?;
    check(deadline)?;
    Ok(hex::encode(writer.hash.finalize()))
}
/// Preserve the historical canonical format, including its separators.
pub(crate) fn canonical_hash(
    value: &serde_json::Value,
    deadline: Option<Instant>,
) -> Result<String, JobsError> {
    let mut writer = Writer {
        bytes: None,
        hash: Sha256::new(),
        deadline,
    };
    emit(value, &mut writer, true).map_err(|e| error(e, deadline))?;
    check(deadline)?;
    Ok(hex::encode(writer.hash.finalize()))
}
fn quoted(text: &str, writer: &mut Writer) -> std::io::Result<()> {
    writer.write_all(b"\"")?;
    let mut rest = text;
    while !rest.is_empty() {
        let mut end = rest.len().min(4096);
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        let encoded = serde_json::to_string(&rest[..end]).map_err(std::io::Error::other)?;
        writer.write_all(&encoded.as_bytes()[1..encoded.len() - 1])?;
        rest = &rest[end..];
    }
    writer.write_all(b"\"")
}
fn emit(value: &serde_json::Value, writer: &mut Writer, trailing: bool) -> std::io::Result<()> {
    match value {
        serde_json::Value::Object(map) => {
            writer.write_all(b"{")?;
            let mut keys: Vec<_> = map.keys().collect();
            if trailing {
                keys.sort();
            }
            for (index, key) in keys.into_iter().enumerate() {
                if !trailing && index > 0 {
                    writer.write_all(b",")?;
                }
                quoted(key, writer)?;
                writer.write_all(b":")?;
                emit(&map[key], writer, trailing)?;
                if trailing {
                    writer.write_all(b",")?;
                }
            }
            writer.write_all(b"}")
        }
        serde_json::Value::Array(rows) => {
            writer.write_all(b"[")?;
            for (index, row) in rows.iter().enumerate() {
                if !trailing && index > 0 {
                    writer.write_all(b",")?;
                }
                emit(row, writer, trailing)?;
                if trailing {
                    writer.write_all(b",")?;
                }
            }
            writer.write_all(b"]")
        }
        serde_json::Value::String(text) => quoted(text, writer),
        scalar => serde_json::to_writer(writer, scalar).map_err(std::io::Error::other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn an_expired_stream_accepts_no_further_evidence_bytes() {
        let mut writer = Writer {
            bytes: Some(Vec::new()),
            hash: Sha256::new(),
            deadline: None,
        };
        writer.write_all(b"retained prefix").unwrap();
        let original_bytes = writer.bytes.clone();
        let original_digest = writer.hash.clone().finalize();
        // Expiry between writes models a descheduled producer resuming after
        // its original deadline, without making scheduler speed a test oracle.
        writer.deadline = Some(Instant::now());
        let remaining = vec![b'x'; 128 * 1024];
        assert_eq!(
            writer.write_all(&remaining).unwrap_err().kind(),
            std::io::ErrorKind::TimedOut
        );
        assert_eq!(writer.bytes, original_bytes);
        assert_eq!(writer.hash.finalize(), original_digest);
    }

    #[test]
    fn expired_serialization_and_hashing_refuse_instead_of_returning_partial_artifacts() {
        let original = serde_json::json!({"rows":["雪".repeat(8192), "retained evidence"]});
        let unchanged = original.clone();
        let deadline = Some(Instant::now());
        for result in [
            json(&original, deadline),
            canonical_hash(&original, deadline),
            hash(b"original immutable evidence", deadline),
        ] {
            assert!(matches!(result, Err(JobsError::SelectionConflict(_))));
        }
        assert_eq!(original, unchanged);
    }

    #[test]
    fn streaming_bytes_and_digest_equal_the_original_formats() {
        for value in [
            serde_json::json!(null),
            serde_json::json!({"z":[],"a":[true,1,-2,1.25,"quote\"\\\n雪"]}),
            serde_json::json!({"large":"雪\"\\\n".repeat(10000)}),
        ] {
            assert_eq!(
                json(&value, None).unwrap(),
                serde_json::to_string(&value).unwrap()
            );
            assert_eq!(
                canonical_hash(&value, None).unwrap(),
                hex::encode(Sha256::digest(boss_core::job::canonical_json_bytes(&value)))
            );
        }
    }
}
