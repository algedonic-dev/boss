//! Shared base for the `boss-*-client` port crates.
//!
//! Almost every domain/registry service ships a sibling
//! `boss-<name>-client` crate: a trait describing the questions other
//! services ask it, plus a `Reqwest<Name>Client` adapter that calls the
//! HTTP API. Those adapters were ~95% identical boilerplate — the same
//! three-variant transport error, the same `reqwest::Client` built with
//! a 5-second timeout, the same "trim the trailing slash off the base
//! URL", and the same `GET → status-check → json` plumbing. This module
//! owns that shape once so each client crate keeps only its trait + URL
//! templates.
//!
//! The error type is generic over a zero-sized [`ServiceLabel`] marker
//! so each crate's `Display` text stays service-specific
//! (`"assets service unreachable: …"` vs `"locations service
//! unreachable: …"`) while the variants, their tuple constructors, and
//! the `std::error::Error` impl are shared. A crate folds in with a
//! one-line marker + a `pub type FooClientError = HttpClientError<Foo>;`
//! alias, so existing call sites that match `FooClientError::Unreachable`
//! or construct it keep compiling unchanged.

use std::marker::PhantomData;
use std::time::Duration;

use serde::de::DeserializeOwned;

/// Per-call timeout every `Reqwest*Client` adapter uses: a stuck
/// downstream registry/service can't wedge a write indefinitely.
pub const CLIENT_TIMEOUT: Duration = Duration::from_secs(5);

/// Zero-sized marker each client crate implements to name itself in
/// [`HttpClientError`]'s `Display` text. `NAME` is the bare service
/// word that fronted the per-crate error strings (`"assets"`,
/// `"locations"`, `"people"`, …).
pub trait ServiceLabel: std::fmt::Debug + Send + Sync + 'static {
    const NAME: &'static str;
}

/// Transport-layer error shared by the folded `boss-*-client` adapters.
///
/// The three inhabited variants are exactly the ones every folded crate
/// carried, with identical tuple shapes — so `HttpClientError::Unreachable(s)`
/// constructs and matches the same way the old per-crate enums did.
/// `Display` reads `S::NAME` so each alias keeps its original wording.
#[derive(Debug)]
pub enum HttpClientError<S: ServiceLabel> {
    /// Network / DNS / timeout — the service may be down.
    Unreachable(String),
    /// The service responded with an unexpected, non-success status.
    UnexpectedStatus(u16),
    /// The body parsed as JSON but didn't match the expected shape.
    MalformedBody(String),
    /// Uninhabited carrier for the `S` type parameter. Never
    /// constructed (its first field is [`std::convert::Infallible`]),
    /// so it can't appear in a value and doesn't widen real matches.
    #[doc(hidden)]
    __Service(std::convert::Infallible, PhantomData<S>),
}

impl<S: ServiceLabel> std::fmt::Display for HttpClientError<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreachable(e) => write!(f, "{} service unreachable: {e}", S::NAME),
            Self::UnexpectedStatus(code) => {
                write!(f, "{} returned unexpected status: {code}", S::NAME)
            }
            Self::MalformedBody(e) => write!(f, "{} returned malformed body: {e}", S::NAME),
            // Unreachable in practice — the variant is uninhabited.
            Self::__Service(never, _) => match *never {},
        }
    }
}

impl<S: ServiceLabel> std::error::Error for HttpClientError<S> {}

/// Build the canonical `(base_url, http)` pair a `Reqwest*Client`
/// holds: the base URL with any trailing slash trimmed (so
/// `format!("{base}/api/…")` never doubles the slash) plus a
/// `reqwest::Client` carrying the shared [`CLIENT_TIMEOUT`].
///
/// ```ignore
/// pub struct ReqwestFooClient { base_url: String, http: reqwest::Client }
/// impl ReqwestFooClient {
///     pub fn new(base_url: impl Into<String>) -> Self {
///         let (base_url, http) = boss_core::http_client::base(base_url);
///         Self { base_url, http }
///     }
/// }
/// ```
pub fn base(base_url: impl Into<String>) -> (String, reqwest::Client) {
    let base_url = base_url.into().trim_end_matches('/').to_string();
    let http = reqwest::Client::builder()
        .timeout(CLIENT_TIMEOUT)
        .build()
        .expect("building reqwest client");
    (base_url, http)
}

/// `GET url`, require a success status, and deserialize the JSON body
/// into `T`. Maps transport failures to `Unreachable`, non-2xx to
/// `UnexpectedStatus`, and decode failures to `MalformedBody` — the
/// exact ladder the folded adapters wrote by hand.
pub async fn get_json<S, T>(http: &reqwest::Client, url: &str) -> Result<T, HttpClientError<S>>
where
    S: ServiceLabel,
    T: DeserializeOwned,
{
    let resp = http
        .get(url)
        .send()
        .await
        .map_err(|e| HttpClientError::Unreachable(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(HttpClientError::UnexpectedStatus(resp.status().as_u16()));
    }
    resp.json::<T>()
        .await
        .map_err(|e| HttpClientError::MalformedBody(e.to_string()))
}

/// `GET url` against an `…/exists`-style endpoint and read the boolean
/// `exists` field out of the JSON object. The hot-path validation
/// primitive the registry clients (locations, subject-kinds, classes,
/// people) all share.
pub async fn get_exists<S>(http: &reqwest::Client, url: &str) -> Result<bool, HttpClientError<S>>
where
    S: ServiceLabel,
{
    let body: serde_json::Value = get_json(http, url).await?;
    body.get("exists")
        .and_then(|v| v.as_bool())
        .ok_or_else(|| HttpClientError::MalformedBody(format!("missing `exists` in {body}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Assets;
    impl ServiceLabel for Assets {
        const NAME: &'static str = "assets";
    }
    type AssetsError = HttpClientError<Assets>;

    #[test]
    fn display_text_is_service_specific() {
        assert_eq!(
            AssetsError::Unreachable("conn refused".into()).to_string(),
            "assets service unreachable: conn refused"
        );
        assert_eq!(
            AssetsError::UnexpectedStatus(503).to_string(),
            "assets returned unexpected status: 503"
        );
        assert_eq!(
            AssetsError::MalformedBody("bad".into()).to_string(),
            "assets returned malformed body: bad"
        );
    }

    #[test]
    fn variants_construct_and_match_like_the_old_enums() {
        let e = AssetsError::Unreachable("x".into());
        assert!(matches!(e, HttpClientError::Unreachable(_)));
    }

    #[test]
    fn base_trims_trailing_slash() {
        let (url, _http) = base("http://host:8080/");
        assert_eq!(url, "http://host:8080");
        let (url, _http) = base("http://host:8080");
        assert_eq!(url, "http://host:8080");
    }
}
