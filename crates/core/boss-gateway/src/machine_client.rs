//! The gateway's machine client: the reqwest 0.13 twin of
//! `boss_core::machine_token::Client` (design 6805c764, car 2).
//!
//! The gateway builds on reqwest 0.13 and boss-core on 0.12, so the
//! gateway cannot hold boss-core's type; it reads the same process-wide
//! watched `Source` (`machine_token::shared`) and keeps the same two
//! promises, so the two cannot differ in what they send or where.
//!
//! STAMPED PER REQUEST. Every request this client builds carries the
//! token read NOW from the watched source, so a rotation reaches the
//! gateway's server-side calls without a restart (review S1/S5 of car 2
//! slice 1). It is not `Deref` to `reqwest::Client`, so no coercion
//! builds an unstamped request from it.
//!
//! NEVER FOLLOWS A REDIRECT (review of 6fbc7fc7, 2026-09-28, finding 1).
//! Until this module every gateway client that carried the token —
//! passkey, elevation, break-glass and its alarm, inquiries, visits,
//! sponsors — was a plain `reqwest::Client` on the default policy, which
//! follows up to ten redirects and, crossing hosts, strips only the
//! authorization, cookie and authenticate headers: the token would ride
//! a `302 Location:` to whatever host it named. [`MachineClient::build`]
//! finishes a `ClientBuilder` with `redirect::Policy::none()` whatever
//! the caller set, and there is no way to wrap an already-built client,
//! whose policy can no longer be changed.
//!
//! USE IT ONLY FOR THE ESTATE'S OWN SERVICES. A client that also talks
//! to a third party — the IdP, the mail relay — must not be this one;
//! those keep a plain `reqwest::Client` beside it (`local_auth`'s
//! `http` is the IdP's; its people lookup rides [`MachineClient`]).

use std::sync::Arc;

use boss_core::machine_token::{self, Source};

#[derive(Clone, Debug)]
pub struct MachineClient {
    http: reqwest::Client,
    token: Arc<Source>,
}

impl MachineClient {
    /// Finish `builder` with redirects off; stamp from the process's
    /// one watched source.
    pub fn build(builder: reqwest::ClientBuilder) -> reqwest::Result<Self> {
        Self::build_with_source(builder, machine_token::shared())
    }

    /// [`MachineClient::build`] with a given source (tests).
    pub fn build_with_source(
        builder: reqwest::ClientBuilder,
        token: Arc<Source>,
    ) -> reqwest::Result<Self> {
        let http = builder
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(MachineClient { http, token })
    }

    pub fn request(
        &self,
        method: reqwest::Method,
        url: impl reqwest::IntoUrl,
    ) -> reqwest::RequestBuilder {
        let rb = self.http.request(method, url);
        match self.token.current() {
            Some(token) => rb.header(machine_token::HEADER, token),
            None => rb,
        }
    }

    pub fn get(&self, url: impl reqwest::IntoUrl) -> reqwest::RequestBuilder {
        self.request(reqwest::Method::GET, url)
    }

    pub fn post(&self, url: impl reqwest::IntoUrl) -> reqwest::RequestBuilder {
        self.request(reqwest::Method::POST, url)
    }

    pub fn put(&self, url: impl reqwest::IntoUrl) -> reqwest::RequestBuilder {
        self.request(reqwest::Method::PUT, url)
    }

    pub fn patch(&self, url: impl reqwest::IntoUrl) -> reqwest::RequestBuilder {
        self.request(reqwest::Method::PATCH, url)
    }

    pub fn delete(&self, url: impl reqwest::IntoUrl) -> reqwest::RequestBuilder {
        self.request(reqwest::Method::DELETE, url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn every_request_carries_the_value_read_now() {
        let c = MachineClient::build_with_source(
            reqwest::Client::builder(),
            Arc::new(Source::fixed(Some("estate-token".into()))),
        )
        .unwrap();
        for rb in [
            c.get("http://127.0.0.1:9/"),
            c.post("http://127.0.0.1:9/"),
            c.put("http://127.0.0.1:9/"),
            c.patch("http://127.0.0.1:9/"),
            c.delete("http://127.0.0.1:9/"),
        ] {
            let req = rb.build().unwrap();
            assert_eq!(
                req.headers().get(machine_token::HEADER).unwrap(),
                "estate-token"
            );
        }
        let none = MachineClient::build_with_source(
            reqwest::Client::builder(),
            Arc::new(Source::fixed(None)),
        )
        .unwrap();
        let req = none.get("http://127.0.0.1:9/").build().unwrap();
        assert!(req.headers().get(machine_token::HEADER).is_none());
    }

    /// One exchange on `listener`: the request head, sent down `seen`,
    /// then `reply` verbatim.
    async fn answer_once(
        listener: tokio::net::TcpListener,
        reply: String,
        seen: tokio::sync::mpsc::UnboundedSender<String>,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let Ok((mut sock, _)) = listener.accept().await else {
            return;
        };
        let mut head = Vec::new();
        let mut buf = [0u8; 1024];
        while !head.windows(4).any(|w| w == b"\r\n\r\n") {
            match sock.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => head.extend_from_slice(&buf[..n]),
            }
        }
        let _ = seen.send(String::from_utf8_lossy(&head).to_lowercase());
        let _ = sock.write_all(reply.as_bytes()).await;
        let _ = sock.shutdown().await;
    }

    /// A 302 naming ANOTHER host (127.0.0.2 is loopback on Linux and a
    /// different host string from 127.0.0.1), and what that host saw.
    async fn redirected(
        send: impl FnOnce(String) -> reqwest::RequestBuilder,
    ) -> (u16, Vec<String>) {
        let other = tokio::net::TcpListener::bind("127.0.0.2:0").await.unwrap();
        let other_at = other.local_addr().unwrap();
        let (other_tx, mut other_rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(answer_once(
            other,
            "HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".into(),
            other_tx,
        ));
        let first = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let first_at = first.local_addr().unwrap();
        let (first_tx, _first_rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(answer_once(
            first,
            format!(
                "HTTP/1.1 302 Found\r\nlocation: http://{other_at}/elsewhere\r\n\
                 content-length: 0\r\nconnection: close\r\n\r\n"
            ),
            first_tx,
        ));
        let status = send(format!("http://{first_at}/api/x"))
            .send()
            .await
            .unwrap()
            .status()
            .as_u16();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let mut reached = Vec::new();
        while let Ok(h) = other_rx.try_recv() {
            reached.push(h);
        }
        (status, reached)
    }

    #[tokio::test]
    async fn a_redirect_to_another_host_is_not_followed_and_no_token_leaves() {
        // The control: a plain client on the default policy follows the
        // 302 and hands the header to the other host, so the fixture
        // sees a leak when there is one.
        let (status, reached) = redirected(|url| {
            reqwest::Client::new()
                .get(url)
                .header(machine_token::HEADER, "estate-token-value")
        })
        .await;
        assert_eq!(status, 200, "the control client followed the 302");
        assert!(
            reached.iter().any(|h| h.contains("estate-token-value")),
            "the control's fixture saw no leak: {reached:?}"
        );

        // The machine client, from a builder that asked for redirects.
        let c = MachineClient::build_with_source(
            reqwest::Client::builder().redirect(reqwest::redirect::Policy::limited(10)),
            Arc::new(Source::fixed(Some("estate-token-value".into()))),
        )
        .unwrap();
        let (status, reached) = redirected(|url| c.get(url)).await;
        assert_eq!(status, 302, "the 3xx comes back to the caller");
        assert!(
            reached.is_empty(),
            "the token followed a redirect: {reached:?}"
        );
    }
}
