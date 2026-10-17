//! CLI machine requests are refused before transport when the trusted door's
//! current credential cannot authenticate their final target (98361745).
use anyhow::Result;
use boss_core::machine_token::{HEADER, Hosts, Source, TokenSource, token_for};
use std::sync::Arc;

#[derive(Debug)]
pub(crate) enum CredentialRefusal {
    CallerHeader,
    Unavailable { scheme: String, host: String },
}
impl std::fmt::Display for CredentialRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CallerHeader => {
                f.write_str("caller-supplied machine credential refused; use the trusted boss door")
            }
            Self::Unavailable { scheme, host } => write!(
                f,
                "machine credential unavailable for {scheme}://{host}; refused before sending. Use the supported trusted boss or boss-api door; do not copy or export its credential"
            ),
        }
    }
}
impl std::error::Error for CredentialRefusal {}

#[derive(Clone, Debug)]
pub(crate) struct MachineClient {
    http: reqwest::Client,
    authority: Authority,
}

#[derive(Clone, Debug)]
struct Authority {
    source: Arc<dyn TokenSource>,
    hosts: Arc<Hosts>,
    required: bool,
}

impl Authority {
    fn authenticate(&self, request: &mut reqwest::Request) -> Result<()> {
        self.authenticate_headers(&request.url().clone(), request.headers_mut())
    }
    fn authenticate_headers(
        &self,
        url: &reqwest::Url,
        headers: &mut reqwest::header::HeaderMap,
    ) -> Result<()> {
        if headers.contains_key(HEADER) {
            return Err(CredentialRefusal::CallerHeader.into());
        }
        // One observation authorizes AND supplies the header. Checking a source
        // and reading it again to stamp would race a withdrawal or rotation.
        match token_for(&self.source, &self.hosts, url.as_str()) {
            Some(value) => {
                headers.insert(HEADER, value);
            }
            None if self.required => {
                return Err(CredentialRefusal::Unavailable {
                    scheme: url.scheme().into(),
                    host: url.host_str().unwrap_or("<invalid host>").into(),
                }
                .into());
            }
            None => {}
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub(crate) struct BlockingMachineClient {
    http: reqwest::blocking::Client,
    authority: Authority,
}
impl BlockingMachineClient {
    #[cfg(test)]
    pub(crate) fn unstamped(builder: reqwest::blocking::ClientBuilder) -> Result<Self> {
        Self::gateway(builder)
    }
    pub(crate) fn build_with_source(
        builder: reqwest::blocking::ClientBuilder,
        source: Arc<Source>,
    ) -> Result<Self> {
        Self::with_authority(
            builder,
            Authority {
                source,
                hosts: boss_core::machine_token::hosts(),
                required: true,
            },
        )
    }
    fn with_authority(
        builder: reqwest::blocking::ClientBuilder,
        authority: Authority,
    ) -> Result<Self> {
        Ok(Self {
            http: builder
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .build()?,
            authority,
        })
    }
    pub(crate) fn gateway(builder: reqwest::blocking::ClientBuilder) -> Result<Self> {
        Self::with_authority(
            builder,
            Authority {
                source: Arc::new(Source::fixed(None)),
                hosts: Arc::new(Hosts::default()),
                required: false,
            },
        )
    }
    pub(crate) fn request(
        &self,
        method: reqwest::Method,
        url: impl AsRef<str>,
    ) -> BlockingMachineRequest {
        BlockingMachineRequest {
            request: self.http.request(method, url.as_ref()),
            authority: self.authority.clone(),
        }
    }
    pub(crate) fn get(&self, url: impl AsRef<str>) -> BlockingMachineRequest {
        self.request(reqwest::Method::GET, url)
    }
    pub(crate) fn post(&self, url: impl AsRef<str>) -> BlockingMachineRequest {
        self.request(reqwest::Method::POST, url)
    }
    pub(crate) fn put(&self, url: impl AsRef<str>) -> BlockingMachineRequest {
        self.request(reqwest::Method::PUT, url)
    }
}
#[derive(Debug)]
pub(crate) struct BlockingMachineRequest {
    request: reqwest::blocking::RequestBuilder,
    authority: Authority,
}
impl BlockingMachineRequest {
    pub(crate) fn body(mut self, value: impl Into<reqwest::blocking::Body>) -> Self {
        self.request = self.request.body(value);
        self
    }
    pub(crate) fn json<T: serde::Serialize + ?Sized>(mut self, value: &T) -> Self {
        self.request = self.request.json(value);
        self
    }
    pub(crate) fn send(self) -> Result<reqwest::blocking::Response> {
        let (http, request) = self.request.build_split();
        let mut request = request?;
        self.authority
            .authenticate_headers(&request.url().clone(), request.headers_mut())?;
        Ok(http.execute(request)?)
    }
    #[cfg(test)]
    pub(crate) fn build(self) -> Result<reqwest::blocking::Request> {
        let mut request = self.request.build()?;
        self.authority
            .authenticate_headers(&request.url().clone(), request.headers_mut())?;
        Ok(request)
    }
}

impl boss_core::machine_token::BlockingHttp for BlockingMachineClient {
    type Request = BlockingMachineRequest;
    fn request(&self, method: reqwest::Method, url: &str) -> Self::Request {
        BlockingMachineClient::request(self, method, url)
    }
}
impl boss_core::machine_token::BlockingHttpRequest for BlockingMachineRequest {
    fn headers(mut self, headers: reqwest::header::HeaderMap) -> Self {
        self.request = self.request.headers(headers);
        self
    }
    fn json<T: serde::Serialize + ?Sized>(self, value: &T) -> Self {
        BlockingMachineRequest::json(self, value)
    }
    fn send(self) -> Result<reqwest::blocking::Response> {
        BlockingMachineRequest::send(self)
    }
}

impl MachineClient {
    #[cfg(not(test))]
    pub(crate) fn build(builder: reqwest::ClientBuilder) -> Result<Self> {
        Self::guarded_with_source(
            builder,
            boss_core::machine_token::shared(),
            boss_core::machine_token::hosts(),
        )
    }

    fn with_authority(builder: reqwest::ClientBuilder, authority: Authority) -> Result<Self> {
        Ok(Self {
            http: builder
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .build()?,
            authority,
        })
    }

    /// Explicit public-gateway authentication is a different door. It never
    /// receives the estate credential, just as the existing gateway walk.
    pub(crate) fn gateway(builder: reqwest::ClientBuilder) -> Result<Self> {
        Self::with_authority(
            builder,
            Authority {
                source: Arc::new(Source::fixed(None)),
                hosts: Arc::new(Hosts::default()),
                required: false,
            },
        )
    }

    #[cfg(test)]
    pub(crate) fn unstamped(builder: reqwest::ClientBuilder) -> Result<Self> {
        Self::gateway(builder)
    }

    fn guarded_with_source(
        builder: reqwest::ClientBuilder,
        source: Arc<dyn TokenSource>,
        hosts: Arc<Hosts>,
    ) -> Result<Self> {
        Self::with_authority(
            builder,
            Authority {
                source,
                hosts,
                required: true,
            },
        )
    }

    pub(crate) fn request(&self, method: reqwest::Method, url: impl AsRef<str>) -> MachineRequest {
        MachineRequest {
            request: self.http.request(method, url.as_ref()),
            authority: self.authority.clone(),
        }
    }
    pub(crate) fn get(&self, url: impl AsRef<str>) -> MachineRequest {
        self.request(reqwest::Method::GET, url)
    }
    pub(crate) fn post(&self, url: impl AsRef<str>) -> MachineRequest {
        self.request(reqwest::Method::POST, url)
    }
    pub(crate) fn delete(&self, url: impl AsRef<str>) -> MachineRequest {
        self.request(reqwest::Method::DELETE, url)
    }
}

/// No raw request or client escapes in production. Fluent edits precede the
/// credential decision; retries build and authenticate another opaque request.
#[derive(Debug)]
pub(crate) struct MachineRequest {
    request: reqwest::RequestBuilder,
    authority: Authority,
}
impl MachineRequest {
    pub(crate) fn header<K, V>(mut self, key: K, value: V) -> Self
    where
        reqwest::header::HeaderName: TryFrom<K>,
        <reqwest::header::HeaderName as TryFrom<K>>::Error: Into<http::Error>,
        reqwest::header::HeaderValue: TryFrom<V>,
        <reqwest::header::HeaderValue as TryFrom<V>>::Error: Into<http::Error>,
    {
        self.request = self.request.header(key, value);
        self
    }
    pub(crate) fn json<T: serde::Serialize + ?Sized>(mut self, value: &T) -> Self {
        self.request = self.request.json(value);
        self
    }
    pub(crate) fn query<T: serde::Serialize + ?Sized>(mut self, value: &T) -> Self {
        self.request = self.request.query(value);
        self
    }
    pub(crate) fn timeout(mut self, duration: std::time::Duration) -> Self {
        self.request = self.request.timeout(duration);
        self
    }
    pub(crate) fn multipart(mut self, form: reqwest::multipart::Form) -> Self {
        self.request = self.request.multipart(form);
        self
    }
    pub(crate) async fn send(self) -> Result<reqwest::Response> {
        self.send_with_method().await.map(|(_, response)| response)
    }
    pub(crate) async fn send_with_method(self) -> Result<(reqwest::Method, reqwest::Response)> {
        let (http, request) = self.request.build_split();
        let mut request = request?;
        self.authority.authenticate(&mut request)?;
        let method = request.method().clone();
        Ok((method, http.execute(request).await?))
    }
    #[cfg(test)]
    pub(crate) fn build(self) -> Result<reqwest::Request> {
        let mut request = self.request.build()?;
        self.authority.authenticate(&mut request)?;
        Ok(request)
    }
}

#[cfg(test)]
impl From<reqwest::RequestBuilder> for MachineRequest {
    fn from(request: reqwest::RequestBuilder) -> Self {
        Self {
            request,
            authority: Authority {
                source: Arc::new(Source::fixed(None)),
                hosts: Arc::new(Hosts::default()),
                required: false,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_machine_senders_cannot_return_to_the_optional_core_transport() {
        fn visit(directory: &std::path::Path, found: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(directory).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    visit(&path, found);
                } else if path.extension().is_some_and(|extension| extension == "rs") {
                    found.push(path);
                }
            }
        }
        let mut files = Vec::new();
        visit(
            &boss_testing::repo_root().join("crates/orchestrators/boss-cli/src"),
            &mut files,
        );
        let optional = regex::Regex::new(r"machine_token\s*::\s*(?:(?:Client|BlockingClient)\b|\{[^}]*\b(?:Client|BlockingClient)\b)").unwrap();
        let constructor = regex::Regex::new(r"MachineClient\s*::\s*build\s*\(").unwrap();
        for bypass in [
            &["boss_core::machine_token::Client", "::build(builder)"].concat(),
            "use boss_core::machine_token::{Source, BlockingClient as Http};",
            "use boss_core::machine_token::Client as Http;",
        ] {
            assert!(optional.is_match(bypass), "the pin missed {bypass}");
        }
        for file in files {
            // The recorded reader uses its separately vouched reader credential,
            // never the estate credential. Its forwarding contract remains distinct.
            if file
                .file_name()
                .is_some_and(|name| name == "probe_reader.rs")
            {
                continue;
            }
            let source = std::fs::read_to_string(&file).unwrap();
            let source = boss_testing::production_source::production_text(&source).unwrap();
            let source = boss_testing::production_source::without_comments(&source);
            assert!(
                !optional.is_match(&source),
                "{} bypasses the CLI final-send guard",
                file.display()
            );
            if file.file_name().is_none_or(|name| name != "sor_client.rs") {
                assert!(
                    !constructor.is_match(&source),
                    "{} bypasses the shared CLI constructor and its hermetic test contract",
                    file.display()
                );
            }
        }
    }

    fn strict(source: Arc<dyn TokenSource>, hosts: &[&str]) -> MachineClient {
        MachineClient::guarded_with_source(
            reqwest::Client::builder(),
            source,
            Arc::new(Hosts::listed(hosts)),
        )
        .unwrap()
    }

    #[derive(Debug, Default)]
    struct FixtureSource(std::sync::Mutex<Option<String>>);
    impl FixtureSource {
        fn set(&self, value: Option<&str>) {
            *self.0.lock().unwrap() = value.map(str::to_owned);
        }
    }
    impl TokenSource for FixtureSource {
        fn current(&self) -> Option<String> {
            self.0.lock().unwrap().clone()
        }
    }

    #[test]
    fn strict_machine_requests_refuse_absence_and_unapproved_hosts() {
        let absent = strict(Arc::new(Source::fixed(None)), &[]);
        let error = absent.get("http://127.0.0.1/api/jobs").build().unwrap_err();
        assert!(error.to_string().contains("machine credential unavailable"));
        let credential = Arc::new(Source::fixed(Some("fixture-machine-value".into())));
        let client = strict(credential, &["record.example"]);
        let error = client
            .get("https://unapproved.example/api/jobs?private=query")
            .build()
            .unwrap_err();
        assert!(error.to_string().contains("machine credential unavailable"));
        assert!(!error.to_string().contains("private=query"));
        let request = client
            .get("https://RECORD.example./api/jobs")
            .build()
            .unwrap();
        assert_eq!(
            request.headers().get(HEADER).unwrap(),
            "fixture-machine-value"
        );
        assert!(request.headers().get(HEADER).unwrap().is_sensitive());
    }

    #[test]
    fn callers_cannot_override_or_duplicate_the_machine_header() {
        let client = strict(
            Arc::new(Source::fixed(Some("fixture-machine-value".into()))),
            &[],
        );
        for request in [
            client
                .get("http://127.0.0.1/")
                .header(HEADER, "caller-value"),
            client
                .get("http://127.0.0.1/")
                .header(HEADER, "a")
                .header(HEADER, "b"),
        ] {
            assert!(
                request
                    .build()
                    .unwrap_err()
                    .to_string()
                    .contains("caller-supplied machine credential refused")
            );
        }
    }

    #[test]
    fn a_request_uses_the_current_value_at_final_build_not_creation() {
        let source = Arc::new(FixtureSource::default());
        source.set(Some("before"));
        let client = strict(source.clone(), &[]);
        let request = client.get("http://127.0.0.1/");
        source.set(Some("after"));
        assert_eq!(
            request.build().unwrap().headers().get(HEADER).unwrap(),
            "after"
        );
        let request = client.get("http://127.0.0.1/");
        source.set(None);
        assert!(
            request
                .build()
                .unwrap_err()
                .to_string()
                .contains("machine credential unavailable")
        );
    }

    #[tokio::test]
    async fn the_wire_carries_one_current_credential_and_never_follows_a_redirect() {
        let client = strict(
            Arc::new(Source::fixed(Some("fixture-machine-value".into()))),
            &[],
        );
        let (base, server) = crate::sor_client::stub::one_redirect(
            "https://unapproved.example/private?state=hidden",
        )
        .await;
        let response = client.get(format!("{base}/api/jobs")).send().await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::FOUND);
        let request = server.await.unwrap().unwrap().to_lowercase();
        assert_eq!(request.matches("x-boss-machine-token:").count(), 1);
        assert!(request.contains("fixture-machine-value"));
    }

    #[tokio::test]
    async fn an_unapproved_target_sends_nothing_even_when_it_resolves_to_a_fake_server() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let client = MachineClient::with_authority(
            reqwest::Client::builder().resolve("unapproved.example", address),
            Authority {
                source: Arc::new(Source::fixed(Some("fixture-machine-value".into()))),
                hosts: Arc::new(Hosts::default()),
                required: true,
            },
        )
        .unwrap();
        let error = client
            .get(format!(
                "http://unapproved.example:{}/api/jobs",
                address.port()
            ))
            .send()
            .await
            .unwrap_err();
        assert!(error.downcast_ref::<CredentialRefusal>().is_some());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), listener.accept())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn retry_after_credential_withdrawal_sends_no_second_request() {
        use tokio::io::AsyncWriteExt;
        let source = Arc::new(FixtureSource::default());
        source.set(Some("fixture-machine-value"));
        let client = strict(source.clone(), &[]);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let first = crate::sor_client::stub::read_request(&mut socket).await;
            assert!(first.contains("fixture-machine-value"));
            source.set(None);
            socket.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 18\r\nConnection: close\r\n\r\npolicy-unreachable").await.unwrap();
            socket.shutdown().await.unwrap();
            assert!(
                tokio::time::timeout(std::time::Duration::from_secs(3), listener.accept())
                    .await
                    .is_err(),
                "retry sent a tokenless request"
            );
        });
        let error =
            crate::train::send_through_a_roll("fixture GET /api/jobs", || client.get(&base))
                .await
                .unwrap_err();
        assert!(
            error.downcast_ref::<CredentialRefusal>().is_some(),
            "{error:#}"
        );
        server.await.unwrap();
    }

    #[test]
    fn blocking_requests_have_the_same_final_credential_boundary() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/api/jobs", listener.local_addr().unwrap());
        let client = BlockingMachineClient::with_authority(
            reqwest::blocking::Client::builder(),
            Authority {
                source: Arc::new(Source::fixed(None)),
                hosts: Arc::new(Hosts::default()),
                required: true,
            },
        )
        .unwrap();
        assert!(
            client
                .get(&url)
                .send()
                .unwrap_err()
                .downcast_ref::<CredentialRefusal>()
                .is_some()
        );
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        let client = BlockingMachineClient::with_authority(
            reqwest::blocking::Client::builder(),
            Authority {
                source: Arc::new(Source::fixed(Some("fixture-machine-value".into()))),
                hosts: Arc::new(Hosts::default()),
                required: true,
            },
        )
        .unwrap();
        assert_eq!(
            client
                .get(&url)
                .build()
                .unwrap()
                .headers()
                .get(HEADER)
                .unwrap(),
            "fixture-machine-value"
        );
        let server = std::thread::spawn(move || {
            use std::io::{Read, Write};
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            let (mut socket, _) = loop {
                match listener.accept() {
                    Ok(pair) => break pair,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(std::time::Instant::now() < deadline);
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(error) => panic!("fixture accept: {error}"),
                }
            };
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(1)))
                .unwrap();
            let mut bytes = [0; 4096];
            let length = socket.read(&mut bytes).unwrap();
            let head = String::from_utf8_lossy(&bytes[..length]).to_lowercase();
            assert_eq!(head.matches("x-boss-machine-token:").count(), 1);
            assert!(head.contains("fixture-machine-value"));
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                .unwrap();
        });
        assert!(client.get(&url).send().unwrap().status().is_success());
        server.join().unwrap();
        assert!(
            client
                .get("http://unapproved.example/")
                .build()
                .unwrap_err()
                .downcast_ref::<CredentialRefusal>()
                .is_some()
        );
        assert!(
            BlockingMachineClient::gateway(reqwest::blocking::Client::builder())
                .unwrap()
                .get(&url)
                .build()
                .unwrap()
                .headers()
                .get(HEADER)
                .is_none()
        );
    }

    #[test]
    fn both_registry_bootstrap_walks_retain_the_callers_final_send_guard() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let client = BlockingMachineClient::with_authority(
            reqwest::blocking::Client::builder(),
            Authority {
                source: Arc::new(Source::fixed(None)),
                hosts: Arc::new(Hosts::default()),
                required: true,
            },
        )
        .unwrap();
        let seeds = boss_testing::repo_root().join("examples/brewery/seeds");
        let error = boss_policy::bootstrap::publish_policy_rules(
            &client,
            &base,
            &seeds.join("policy_rules.toml"),
            false,
            Some(r#"{"id":"fixture-builder"}"#),
        )
        .unwrap_err();
        assert!(
            error.downcast_ref::<CredentialRefusal>().is_some(),
            "{error:#}"
        );
        let error = boss_jobs::bootstrap::publish_workflows(
            &client,
            &base,
            &seeds.join("workflows.toml"),
            "fixture",
            false,
            false,
            Some(r#"{"id":"fixture-builder"}"#),
        )
        .unwrap_err();
        assert!(
            error.downcast_ref::<CredentialRefusal>().is_some(),
            "{error:#}"
        );
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
}
