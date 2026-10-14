//! The simulator's reqwest-0.13 relay door (2710c8fc R5).
//! It carries only an inbound gate's verified credential, never the
//! process's token on behalf of an assertion admitted under report.

use boss_core::machine_gate::VerifiedEstate;

/// The only constructor of the relay's private HTTP client. A supplied
/// builder cannot enable a proxy or redirect behind a verified stamp.
fn finish(builder: reqwest::ClientBuilder) -> reqwest::Result<reqwest::Client> {
    builder
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
}

/// Private fields and no Deref/From<Client>: every stamped request
/// leaves through the bounded constructor above.
pub(super) struct RelayClient {
    http: reqwest::Client,
}

impl RelayClient {
    pub(super) fn build(builder: reqwest::ClientBuilder) -> reqwest::Result<Self> {
        Ok(Self {
            http: finish(builder)?,
        })
    }

    pub(super) fn post(
        &self,
        url: &str,
        verified: Option<&VerifiedEstate>,
    ) -> reqwest::RequestBuilder {
        let request = self.http.post(url);
        match verified.and_then(|proof| proof.header_for(url)) {
            Some(token) => request.header(boss_core::machine_token::HEADER, token),
            None => request,
        }
    }

    /// Preserve existing unstamped reads; this door grants them no
    /// process credential merely because the simulator forwards them.
    pub(super) fn get(&self, url: &str) -> reqwest::RequestBuilder {
        self.http.get(url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, routing::get};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn a_supplied_relay_builder_cannot_enable_a_proxy() {
        let received = Arc::new(AtomicUsize::new(0));
        let counted = received.clone();
        let proxy = Router::new().fallback(move || {
            let counted = counted.clone();
            async move {
                counted.fetch_add(1, Ordering::SeqCst);
                "proxy"
            }
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_url = format!("http://{}", listener.local_addr().unwrap());
        let proxy_server = tokio::spawn(async move {
            axum::serve(listener, proxy).await.unwrap();
        });
        let destination = Router::new().route("/", get(|| async { "destination" }));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let destination_url = format!("http://{}/", listener.local_addr().unwrap());
        let destination_server = tokio::spawn(async move {
            axum::serve(listener, destination).await.unwrap();
        });
        let client = RelayClient::build(
            reqwest::Client::builder().proxy(reqwest::Proxy::all(proxy_url).unwrap()),
        )
        .unwrap();
        let response = client.get(&destination_url).send().await.unwrap();
        assert_eq!(response.text().await.unwrap(), "destination");
        assert_eq!(received.load(Ordering::SeqCst), 0);
        proxy_server.abort();
        destination_server.abort();
    }
}
