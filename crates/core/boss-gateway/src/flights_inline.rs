//! The gateway's bounded inline flights read. Kept in the library so
//! its actual HTTP failures and diagnostics can be tested in a process
//! that owns its tracing subscriber (04fcbecd).
use reqwest::header::HeaderValue;
use std::time::Instant;

use crate::machine_client::MachineClient;

/// Read the viewer's answer without extending the page's two-second
/// request budget. The static-file handler owns the upstream address.
pub async fn read_for(client: &MachineClient, url: String, viewer: HeaderValue) -> Option<String> {
    let started = Instant::now();
    // Only the session's identity is diagnostic. Never print the raw
    // role header, the machine token, or an upstream response body.
    let viewer_id = viewer
        .to_str()
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        .and_then(|answer| answer.get("id")?.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unread".into());
    let resp = match request(client, url, viewer).send().await {
        Ok(resp) => resp,
        Err(error) => return unread(&viewer_id, started, "transport", error.is_timeout(), error),
    };
    if !resp.status().is_success() {
        return unread(&viewer_id, started, "status", false, resp.status());
    }
    let answer = match resp.json::<serde_json::Value>().await {
        Ok(answer) => answer,
        Err(error) => return unread(&viewer_id, started, "body", error.is_timeout(), error),
    };
    body(&answer).or_else(|| {
        unread(
            &viewer_id,
            started,
            "shape",
            false,
            "expected flights array of strings",
        )
    })
}

fn unread(
    viewer: &str,
    started: Instant,
    reason: &str,
    timeout: bool,
    cause: impl std::fmt::Display,
) -> Option<String> {
    tracing::warn!(viewer, elapsed_ms = started.elapsed().as_millis() as u64,
        reason, timeout, cause = %cause, "inline flights unread; browser will retry");
    None
}

fn request(client: &MachineClient, url: String, viewer: HeaderValue) -> reqwest::RequestBuilder {
    client
        .get(url)
        .header("x-boss-user", viewer)
        .timeout(std::time::Duration::from_secs(2))
}

fn body(answer: &serde_json::Value) -> Option<String> {
    let codes: Vec<&str> = answer
        .get("flights")?
        .as_array()?
        .iter()
        .map(serde_json::Value::as_str)
        .collect::<Option<_>>()?;
    serde_json::to_string(&serde_json::json!({ "flights": codes })).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_flights_read_carries_the_viewer_and_the_machine_token_and_keeps_its_budget() {
        use boss_core::machine_token::{HEADER, Source};
        let client = |token: Option<&str>| {
            MachineClient::build_with_source(
                reqwest::Client::builder(),
                std::sync::Arc::new(Source::fixed(token.map(String::from))),
            )
            .unwrap()
        };
        let viewer = HeaderValue::from_static(r#"{"id":"emp-1"}"#);
        let req = request(
            &client(Some("estate-token")),
            "http://127.0.0.1:9/api/flights/mine".into(),
            viewer.clone(),
        )
        .build()
        .unwrap();
        assert_eq!(req.headers().get(HEADER).unwrap(), "estate-token");
        assert_eq!(req.headers().get("x-boss-user").unwrap(), &viewer);
        assert_eq!(req.timeout(), Some(&std::time::Duration::from_secs(2)));
        let bare = request(
            &client(None),
            "http://127.0.0.1:9/api/flights/mine".into(),
            viewer,
        )
        .build()
        .unwrap();
        assert!(bare.headers().get(HEADER).is_none());
    }

    #[test]
    fn only_a_list_of_codes_reaches_the_page() {
        let ok = serde_json::json!({"flights": ["a", "b"], "audience": {"secret": 1}});
        assert_eq!(body(&ok).as_deref(), Some(r#"{"flights":["a","b"]}"#));
        for bad in [
            serde_json::json!({}),
            serde_json::json!({"flights": "a"}),
            serde_json::json!({"flights": ["a", 1]}),
            serde_json::json!(["a"]),
        ] {
            assert_eq!(body(&bad), None, "{bad}");
        }
    }
}
