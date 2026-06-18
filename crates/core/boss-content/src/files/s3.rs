//! S3-compatible storage adapter.
//!
//! Speaks the AWS S3 wire protocol via `aws-sdk-s3`. Works against:
//! - AWS S3 (via standard AWS env vars)
//! - GCS via [interoperability HMAC keys](https://cloud.google.com/storage/docs/aws-simple-migration)
//!   (set `BOSS_FILE_STORAGE_ENDPOINT=https://storage.googleapis.com`)
//! - MinIO for local dev
//! - Cloudflare R2, Backblaze B2
//!
//! Per the design's no-platform-lock-in posture: no GCS-specific code,
//! no S3-specific code beyond the wire — just the S3 protocol +
//! standard AWS-style HMAC auth, and an `endpoint` override.

use std::time::Duration;

use async_trait::async_trait;
use aws_credential_types::Credentials;
use aws_sdk_s3::Client as S3Client;
use aws_sdk_s3::config::Region;
use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::primitives::ByteStream;
use bytes::Bytes;

use crate::files::error::FileError;
use crate::files::port::FileStorage;

/// S3-compatible storage. One instance is bound to one bucket; multi-
/// bucket scoping (per design Q1) is a v2 concern — for v1 each
/// deployment has one tenant bucket per environment.
pub struct S3Storage {
    client: S3Client,
    bucket: String,
}

impl S3Storage {
    /// Build an S3-compatible client from explicit config. The AWS
    /// loader still reads env vars for credentials (or a profile);
    /// the explicit `endpoint` + `region` here override the defaults
    /// so callers don't have to touch AWS_ENDPOINT_URL_S3 etc.
    pub async fn new(
        bucket: impl Into<String>,
        endpoint: Option<&str>,
        region: Option<&str>,
    ) -> Result<Self, FileError> {
        let mut loader = aws_config::defaults(aws_config::BehaviorVersion::latest());
        if let Some(r) = region {
            loader = loader.region(Region::new(r.to_string()));
        }
        if let Some(ep) = endpoint {
            loader = loader.endpoint_url(ep);
        }
        let shared = loader.load().await;

        // GCS interop + MinIO both want path-style addressing —
        // virtual-hosted-style requires per-bucket DNS. Force path
        // style so the same code works against every backend without
        // a per-provider switch.
        let s3_conf = aws_sdk_s3::config::Builder::from(&shared)
            .force_path_style(true)
            .build();

        Ok(Self {
            client: S3Client::from_conf(s3_conf),
            bucket: bucket.into(),
        })
    }

    /// Build with explicit HMAC credentials — the GCS interop path,
    /// where the access-key + secret are issued by GCP for a service
    /// account rather than picked up from `~/.aws/credentials`.
    pub async fn with_credentials(
        bucket: impl Into<String>,
        endpoint: &str,
        region: &str,
        access_key: &str,
        secret_key: &str,
    ) -> Result<Self, FileError> {
        let creds = Credentials::new(access_key, secret_key, None, None, "boss-content");
        let shared = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(Region::new(region.to_string()))
            .endpoint_url(endpoint)
            .credentials_provider(creds)
            .load()
            .await;
        let s3_conf = aws_sdk_s3::config::Builder::from(&shared)
            .force_path_style(true)
            .build();
        Ok(Self {
            client: S3Client::from_conf(s3_conf),
            bucket: bucket.into(),
        })
    }

    pub fn bucket(&self) -> &str {
        &self.bucket
    }
}

fn map_err(e: impl std::fmt::Display) -> FileError {
    FileError::Storage(e.to_string())
}

#[async_trait]
impl FileStorage for S3Storage {
    async fn put(&self, key: &str, bytes: Bytes, mime: &str) -> Result<(), FileError> {
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .body(ByteStream::from(bytes))
            .content_type(mime)
            .send()
            .await
            .map_err(map_err)?;
        Ok(())
    }

    async fn get(&self, key: &str) -> Result<Bytes, FileError> {
        let resp = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| {
                // SDK's NoSuchKey is wrapped behind ServiceError; map it
                // to NotFound so callers can branch cleanly without
                // string-matching error messages.
                let s = e.to_string();
                if s.contains("NoSuchKey") || s.contains("NotFound") {
                    FileError::NotFound(key.to_string())
                } else {
                    FileError::Storage(s)
                }
            })?;
        let bytes = resp.body.collect().await.map_err(map_err)?;
        Ok(bytes.into_bytes())
    }

    async fn delete(&self, key: &str) -> Result<(), FileError> {
        // S3 DeleteObject is idempotent — deleting a missing key is
        // a 204 with no body. No special handling needed.
        self.client
            .delete_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(map_err)?;
        Ok(())
    }

    async fn sign_get_url(&self, key: &str, ttl: Duration) -> Result<String, FileError> {
        let presign = PresigningConfig::expires_in(ttl).map_err(map_err)?;
        let req = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .presigned(presign)
            .await
            .map_err(map_err)?;
        Ok(req.uri().to_string())
    }

    async fn sign_put_url(
        &self,
        key: &str,
        mime: &str,
        ttl: Duration,
    ) -> Result<String, FileError> {
        // content_type binds the Content-Type into the signature so a
        // presigned URL can't be reused to upload a Trojan disguised
        // as the declared mime — the client must send the same header
        // at PUT time or S3 rejects with SignatureDoesNotMatch.
        let presign = PresigningConfig::expires_in(ttl).map_err(map_err)?;
        let req = self
            .client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .content_type(mime)
            .presigned(presign)
            .await
            .map_err(map_err)?;
        Ok(req.uri().to_string())
    }

    async fn head(&self, key: &str) -> Result<u64, FileError> {
        let resp = self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| {
                let s = e.to_string();
                if s.contains("NoSuchKey") || s.contains("NotFound") || s.contains("404") {
                    FileError::NotFound(key.to_string())
                } else {
                    FileError::Storage(s)
                }
            })?;
        Ok(resp.content_length().unwrap_or(0).max(0) as u64)
    }
}
