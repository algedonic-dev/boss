//! The private server entry cannot accept a plaintext socket. Native TLS
//! issuance/rotation, read-only key projection, listener and Service are owed.
use super::{CacheError, Endpoint};
use std::sync::Arc;

pub(crate) async fn serve(
    stream: tokio_rustls::server::TlsStream<tokio::net::TcpStream>,
    endpoint: Arc<Endpoint>,
) -> Result<(), CacheError> {
    use tracing::instrument::WithSubscriber;
    // The private connection never inherits access/debug logging. The
    // transport's error may contain private text; only a fixed refusal leaves.
    hyper::server::conn::http1::Builder::new()
        .keep_alive(false)
        .serve_connection(
            hyper_util::rt::TokioIo::new(stream),
            hyper_util::service::TowerToHyperService::new(super::router(Some(endpoint))),
        )
        .with_subscriber(tracing::subscriber::NoSubscriber::default())
        .await
        .map_err(|_| CacheError::Unavailable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_rustls::{TlsAcceptor, rustls};
    fn certificate() -> (
        Vec<rustls::pki_types::CertificateDer<'static>>,
        rustls::pki_types::PrivateKeyDer<'static>,
        Vec<u8>,
    ) {
        let root = boss_testing::scratch_dir("collector-synthetic-tls");
        let command = |arguments: &[&str]| {
            let result = std::process::Command::new("openssl")
                .current_dir(&root)
                .args(arguments)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "synthetic TLS fixture command failed"
            );
        };
        command(&[
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-subj",
            "/CN=synthetic-root",
            "-keyout",
            "ca.key",
            "-out",
            "ca.pem",
            "-days",
            "1",
            "-addext",
            "basicConstraints=critical,CA:TRUE",
        ]);
        command(&[
            "req",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-subj",
            "/CN=boss-managed-cache-collection.boss.svc",
            "-keyout",
            "server.key",
            "-out",
            "server.csr",
            "-addext",
            "subjectAltName=DNS:boss-managed-cache-collection.boss.svc",
        ]);
        command(&[
            "x509",
            "-req",
            "-in",
            "server.csr",
            "-CA",
            "ca.pem",
            "-CAkey",
            "ca.key",
            "-CAcreateserial",
            "-out",
            "server.pem",
            "-days",
            "1",
            "-copy_extensions",
            "copy",
        ]);
        command(&[
            "x509",
            "-in",
            "server.pem",
            "-outform",
            "DER",
            "-out",
            "server.der",
        ]);
        command(&[
            "pkcs8",
            "-topk8",
            "-nocrypt",
            "-in",
            "server.key",
            "-outform",
            "DER",
            "-out",
            "server-key.der",
        ]);
        let certificate = rustls::pki_types::CertificateDer::from(
            std::fs::read(root.join("server.der")).unwrap(),
        );
        let key =
            rustls::pki_types::PrivateKeyDer::Pkcs8(rustls::pki_types::PrivatePkcs8KeyDer::from(
                std::fs::read(root.join("server-key.der")).unwrap(),
            ));
        (
            vec![certificate],
            key,
            std::fs::read(root.join("ca.pem")).unwrap(),
        )
    }
    #[tokio::test]
    async fn the_private_handler_serves_a_real_trusted_tls_connection() {
        use tracing::instrument::WithSubscriber;
        #[derive(Clone)]
        struct Writer(Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for Writer {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        struct LoggingMetadata(Arc<dyn super::super::scope::Metadata>);
        #[async_trait::async_trait]
        impl super::super::scope::Metadata for LoggingMetadata {
            async fn run(&self, id: uuid::Uuid) -> Result<serde_json::Value, CacheError> {
                tracing::trace!("synthetic private port diagnostic must stay private");
                self.0.run(id).await
            }
            async fn credential(&self, id: &str) -> Result<serde_json::Value, CacheError> {
                self.0.credential(id).await
            }
            async fn native(
                &self,
                r: super::super::scope::Resource,
                n: &str,
            ) -> Result<serde_json::Value, CacheError> {
                self.0.native(r, n).await
            }
        }
        let (mut endpoint, token, identity, store) = super::super::tests::endpoint_fixture();
        let metadata = endpoint.metadata.clone();
        Arc::get_mut(&mut endpoint).unwrap().metadata = Arc::new(LoggingMetadata(metadata));
        let logs = Arc::new(std::sync::Mutex::new(Vec::new()));
        let output = logs.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .with_writer(move || Writer(output.clone()))
            .finish();
        let (certificates, key, ca) = certificate();
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(certificates, key)
        .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(
            async move {
                let (socket, _) = listener.accept().await.unwrap();
                let stream = acceptor.accept(socket).await.unwrap();
                tracing::trace!("known positive TLS log control");
                serve(stream, endpoint).await
            }
            .with_subscriber(subscriber),
        );
        let certificate = reqwest::Certificate::from_pem(&ca).unwrap();
        let client = reqwest::Client::builder()
            .no_proxy()
            .tls_built_in_root_certs(false)
            .add_root_certificate(certificate)
            .resolve("boss-managed-cache-collection.boss.svc", address)
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();
        let response = client
            .post(format!(
                "https://boss-managed-cache-collection.boss.svc:{}/v1/collect",
                address.port()
            ))
            .bearer_auth(token)
            .header("x-boss-collection-phase", "collect")
            .header("x-boss-run", identity.run.to_string())
            .header("x-boss-nonce", identity.nonce.to_string())
            .header("x-boss-pod-uid", identity.pod_uid.to_string())
            .header("x-boss-workspace-uid", &identity.workspace_uid)
            .body("synthetic private TLS cache")
            .send()
            .await;
        let served = server.await.unwrap();
        assert!(served.is_ok());
        let response = response.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let reply: serde_json::Value = response.json().await.unwrap();
        assert_eq!(reply["status"], "retained");
        assert_eq!(
            std::fs::read(store.join(format!("upload-{}-original.private", identity.nonce)))
                .unwrap(),
            b"synthetic private TLS cache"
        );
        let recorded = String::from_utf8(logs.lock().unwrap().clone()).unwrap();
        assert!(recorded.contains("known positive TLS log control"));
        assert!(!recorded.contains("synthetic private port diagnostic"));
        assert!(!recorded.contains("synthetic private TLS cache"));
    }

    #[tokio::test]
    async fn plaintext_cannot_enter_the_tls_only_private_handler() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (_, _, identity, store) = super::super::tests::endpoint_fixture();
        let (certificates, key, _) = certificate();
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(certificates, key)
        .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            acceptor.accept(socket).await.is_err()
        });
        let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
        socket
            .write_all(
                b"POST /v1/collect HTTP/1.1\r\nHost: fake\r\nContent-Length: 6\r\n\r\nsecret",
            )
            .await
            .unwrap();
        let mut response = Vec::new();
        let _ = socket.read_to_end(&mut response).await;
        assert!(server.await.unwrap());
        assert!(!response.starts_with(b"HTTP/"));
        assert!(
            !store
                .join(format!("upload-{}-intent.private", identity.nonce))
                .exists()
        );
    }
}
