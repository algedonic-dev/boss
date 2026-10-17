//! Fixed native GET mechanics. Construction does not install authority.
use super::*;
use std::sync::Arc;

impl Target {
    fn path(&self) -> Result<String, Refused> {
        let named = |name: &str| {
            if installation::pod_name(name) {
                Ok(name.to_owned())
            } else {
                Err(Refused)
            }
        };
        Ok(match self {
            Target::Secret => format!("/api/v1/namespaces/{NAMESPACE}/secrets/{SECRET}"),
            Target::BrokerPod(name) => {
                format!("/api/v1/namespaces/{NAMESPACE}/pods/{}", named(name)?)
            }
            Target::PublicTrust => format!(
                "/api/v1/namespaces/boss-dev/configmaps/{}",
                installation::trust_name()?
            ),
            Target::CollectorJob(name) => {
                format!("/apis/batch/v1/namespaces/boss-dev/jobs/{}", named(name)?)
            }
            Target::CollectorPod(name) => {
                format!("/api/v1/namespaces/boss-dev/pods/{}", named(name)?)
            }
        })
    }
}
impl installation::HttpPodReader {
    // The only production caller supplies the fixed cluster origin. The
    // private base seam lets owning tests use a token-free loopback server.
    pub(super) async fn read_at(&self, target: Target, base: &str) -> Result<NativeReply, Refused> {
        let path = target.path()?;
        let mut original = NativeReply {
            target,
            status: None,
            headers: Vec::new(),
            body: Vec::new(),
            error: None,
        };
        let response = self
            .client
            .get(format!("{base}{path}"))
            .bearer_auth(&self.token)
            .send()
            .await;
        let mut response = match response {
            Ok(response) => response,
            Err(error) => {
                original.error = Some(error.to_string());
                return Ok(original);
            }
        };
        original.status = Some(response.status().as_u16());
        original.headers = response
            .headers()
            .iter()
            .map(|(key, value)| (key.to_string(), value.as_bytes().to_vec()))
            .collect();
        loop {
            match response.chunk().await {
                Ok(Some(chunk)) => {
                    let remaining = (128_usize * 1024 + 1).saturating_sub(original.body.len());
                    original
                        .body
                        .extend_from_slice(&chunk[..chunk.len().min(remaining)]);
                    if original.body.len() > 128 * 1024 {
                        original.error =
                            Some("native body exceeded its bound; original is incomplete".into());
                        return Ok(original);
                    }
                }
                Ok(None) => return Ok(original),
                Err(error) => {
                    original.error = Some(error.to_string());
                    return Ok(original);
                }
            }
        }
    }
}
#[async_trait]
impl NativeReads for installation::HttpPodReader {
    async fn read(&self, target: Target) -> Result<NativeReply, Refused> {
        self.read_at(target, "https://kubernetes.default.svc").await
    }
}
pub struct HttpCurrentTrust {
    native: Arc<dyn NativeReads>,
}
impl HttpCurrentTrust {
    pub fn new(native: Arc<dyn NativeReads>) -> Self {
        Self { native }
    }
}
#[async_trait]
impl CurrentTrustReader for HttpCurrentTrust {
    async fn observe(&self, invocation: uuid::Uuid) -> Result<CurrentTrustObservation, Refused> {
        if invocation.is_nil() {
            return Err(Refused);
        }
        let native = self.native.read(Target::PublicTrust).await?;
        Ok(CurrentTrustObservation {
            invocation,
            observation: uuid::Uuid::new_v4(),
            observed_at: evidence::wall_time(),
            native,
        })
    }
}
