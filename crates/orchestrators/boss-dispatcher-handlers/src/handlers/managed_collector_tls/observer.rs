//! Inert protected observation mechanics. No installer, grant or rule is wired.
use super::*;
#[path = "observer/evidence.rs"]
mod evidence;
pub use evidence::*;
#[path = "observer/native.rs"]
mod native;
pub use native::HttpCurrentTrust;

/// The fixed endpoint never accepts a caller URL, proxy or ambient trust.
pub struct ServedLeaf;
impl ServedLeaf {
    #[cfg(test)]
    async fn observe_test(
        &self,
        trust: &str,
        address: std::net::SocketAddr,
    ) -> Result<Vec<u8>, Refused> {
        Self::connect(trust.to_owned(), address)
            .await
            .map_err(|_| Refused)
    }
    pub async fn observe(&self, trust: &str) -> Result<Vec<u8>, Refused> {
        self.original(trust).await.map_err(|_| Refused)
    }
    pub(super) async fn original(&self, trust: &str) -> Result<Vec<u8>, String> {
        let addresses = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            tokio::net::lookup_host((HOST, 7444)),
        )
        .await
        .map_err(detail)?
        .map_err(detail)?
        .collect::<Vec<_>>();
        if addresses.is_empty() || addresses.len() > 16 {
            return Err("fixed-host address bound refused".into());
        }
        // One resolved fixed-host endpoint per invocation; uncertainty is not
        // permission to fan out or substitute another destination.
        Self::connect(trust.to_owned(), addresses[0]).await
    }
    async fn connect(trust: String, address: std::net::SocketAddr) -> Result<Vec<u8>, String> {
        tokio::task::spawn_blocking(move || {
            use openssl::{
                ssl::{Ssl, SslContextBuilder, SslMethod, SslStream, SslVerifyMode, SslVersion},
                x509::X509,
            };
            // SslContextBuilder does not load default trust paths or install a
            // key-log callback. No proxy, environment CA or client credential
            // participates in this direct TLS-only observation.
            let mut context = SslContextBuilder::new(SslMethod::tls_client()).map_err(detail)?;
            context.set_verify(SslVerifyMode::PEER);
            context
                .set_min_proto_version(Some(SslVersion::TLS1_2))
                .map_err(detail)?;
            let roots = X509::stack_from_pem(trust.as_bytes()).map_err(detail)?;
            if roots.len() != 1 {
                return Err("selected public trust must contain exactly one certificate".into());
            }
            for root in roots {
                context.cert_store_mut().add_cert(root).map_err(detail)?;
            }
            let mut ssl = Ssl::new(&context.build()).map_err(detail)?;
            ssl.set_hostname(HOST).map_err(detail)?;
            ssl.param_mut().set_host(HOST).map_err(detail)?;
            let socket =
                std::net::TcpStream::connect_timeout(&address, std::time::Duration::from_secs(10))
                    .map_err(detail)?;
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(10)))
                .map_err(detail)?;
            socket
                .set_write_timeout(Some(std::time::Duration::from_secs(10)))
                .map_err(detail)?;
            let mut stream = SslStream::new(ssl, socket).map_err(detail)?;
            stream.connect().map_err(detail)?;
            stream
                .ssl()
                .peer_certificate()
                .ok_or_else(|| "peer certificate missing".to_owned())?
                .to_pem()
                .map_err(detail)
        })
        .await
        .map_err(detail)?
    }
}
fn detail(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[cfg(test)]
#[path = "observer/tests.rs"]
mod tests;
