//! No browser on another site writes through a signed-in visitor's
//! session (backlog 324fc920, 2026-09-28).
//!
//! WHY. The session cookie is `SameSite=Lax` (session.rs `set_cookie`),
//! which keeps it off a cross-site subresource or `fetch`, and until
//! this module that was the gateway's only CSRF defence. Two holes
//! remained, both found by the adversarial review of car
//! fix/passkey-owner-session-is-operator-tier: Lax still sends the
//! cookie on a top-level navigation, and "site" is the registrable
//! domain, so a SIBLING subdomain of algedonic.dev is same-site and
//! gets the cookie on a form POST too. No route checked `Origin` or
//! `Sec-Fetch-Site`, so any route taking no body or a non-JSON body
//! could be driven from a page the gateway never served — and an
//! operator-tier session made every such write an operator's.
//!
//! WHAT IS REFUSED. A request whose method is not safe (anything but
//! GET, HEAD, OPTIONS) and that a browser marked as coming from another
//! origin, answered 403 before any route, handler or other layer
//! sees it. The browser's mark, in the order it is read — the shape of
//! Go 1.25's `http.CrossOriginProtection`:
//!
//! 1. `Sec-Fetch-Site`, when present, decides alone: `same-origin` and
//!    `none` (typed, bookmarked, user-initiated) pass; `cross-site`
//!    AND `same-site` are refused. `same-site` is the packet's own
//!    attack — a page on evil.algedonic.dev is same-site to
//!    boss.algedonic.dev — so refusing only `cross-site` would leave
//!    it open. Current browsers send it to a potentially trustworthy
//!    destination — https, or localhost — and omit it to a plain-http
//!    one such as the gateway's own LAN address (`http://<host>:4443`,
//!    see `the_own_origin_carries_its_port` below), where rule 2's
//!    Origin fallback is what decides.
//! 2. Otherwise `Origin`, when present, must name this request's own
//!    host (the `Host` header the tunnel forwards unchanged, or the
//!    HTTP/2 authority); a different host, or `null`, is refused.
//! 3. Neither header is not a browser: `boss-api`, the conductor, the
//!    forge runner and every probe send neither, and pass unchanged.
//!    A browser cannot be made to omit both on a cross-origin POST.
//!
//! WHAT IT DOES NOT TOUCH. Reads: a GET is never refused, whatever it
//! carries. The LAN machine door: a service port reached directly is
//! not behind this gateway at all. The session cookie's flags, which
//! stay Lax — Strict would drop the cookie on every link into the app
//! from mail or chat, and this check is what Strict would have bought
//! for writes.
//!
//! WHERE. Around the finished router, like the dot-segment refusal
//! (dot_segments.rs): [`mount`] wraps site, inquiry door, role headers,
//! every route and the fallback, so no handler runs for a refused
//! request. `main` calls it after `inquiries::mount` and before
//! `dot_segments::mount`, which stays outermost.

use axum::extract::Request;
use axum::http::{HeaderMap, Method, StatusCode, Uri, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

/// The named refusal a cross-origin write gets, before any routing.
pub(crate) const CROSS_SITE_REFUSAL: &str = "a write from a page on another origin is refused; the gateway accepts writes only from its own pages or from non-browser callers";

/// Why this request is refused, or `None` to let it through. Pure over
/// the method, the headers and the URI, so every case is a table row.
pub(crate) fn refusal(method: &Method, headers: &HeaderMap, uri: &Uri) -> Option<&'static str> {
    if matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) {
        return None;
    }
    if let Some(site) = headers.get("sec-fetch-site") {
        return match site.to_str().map(str::to_ascii_lowercase).as_deref() {
            Ok("same-origin") | Ok("none") => None,
            _ => Some(CROSS_SITE_REFUSAL),
        };
    }
    let origin = headers.get(header::ORIGIN)?;
    let own = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .or_else(|| uri.authority().map(|a| a.as_str()));
    let origin_host = origin
        .to_str()
        .ok()
        .and_then(|o| o.split_once("://"))
        .map(|(_, host)| host);
    match (origin_host, own) {
        (Some(o), Some(h)) if o.eq_ignore_ascii_case(h) => None,
        _ => Some(CROSS_SITE_REFUSAL),
    }
}

/// The gateway, entered through the refusal: the finished router
/// becomes the only service of an otherwise empty router, and the
/// refusal is layered on that, so it runs before the inner router
/// matches anything. Called once in `main`.
pub fn mount(app: axum::Router) -> axum::Router {
    axum::Router::new()
        .fallback_service(app)
        .layer(axum::middleware::from_fn(refuse))
}

async fn refuse(req: Request, next: Next) -> Response {
    if let Some(why) = refusal(req.method(), req.headers(), req.uri()) {
        let origin = req
            .headers()
            .get(header::ORIGIN)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("-");
        let site = req
            .headers()
            .get("sec-fetch-site")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("-");
        tracing::warn!(
            method = %req.method(),
            path = %req.uri().path(),
            origin = %origin,
            sec_fetch_site = %site,
            "refused a cross-origin write before routing"
        );
        return (StatusCode::FORBIDDEN, why).into_response();
    }
    next.run(req).await
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use axum::body::Body;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tower::ServiceExt;

    const OWN: &str = "boss.algedonic.dev";

    /// A route that counts every time its handler runs, behind the
    /// refusal — so a test reads whether a request reached a handler,
    /// not only the status it got back.
    fn gateway() -> (axum::Router, Arc<AtomicUsize>) {
        let ran = Arc::new(AtomicUsize::new(0));
        let seen = ran.clone();
        let handler = move || {
            let seen = seen.clone();
            async move {
                seen.fetch_add(1, Ordering::SeqCst);
                "handled"
            }
        };
        let inner = axum::Router::new().route(
            "/api/jobs/x",
            axum::routing::get(handler.clone())
                .post(handler.clone())
                .put(handler.clone())
                .patch(handler.clone())
                .delete(handler),
        );
        (mount(inner), ran)
    }

    async fn send(method: &str, headers: &[(&str, &str)]) -> (u16, String, usize) {
        let (app, ran) = gateway();
        let mut req = axum::http::Request::builder()
            .method(method)
            .uri("/api/jobs/x")
            .header(header::HOST, OWN);
        for (n, v) in headers {
            req = req.header(*n, *v);
        }
        let resp = app.oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
        let status = resp.status().as_u16();
        let body = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        (
            status,
            String::from_utf8_lossy(&body).to_string(),
            ran.load(Ordering::SeqCst),
        )
    }

    /// The packet's acceptance: a write from evil.algedonic.dev is 403
    /// and no handler runs — in the shape a current browser sends it
    /// (a sibling subdomain is `same-site`), a cross-site page's shape,
    /// and an older browser's Origin alone — on every write method.
    #[tokio::test]
    async fn a_write_from_another_origin_is_refused_before_any_handler() {
        let shapes: [&[(&str, &str)]; 5] = [
            &[
                ("origin", "https://evil.algedonic.dev"),
                ("sec-fetch-site", "same-site"),
            ],
            &[
                ("origin", "https://evil.example"),
                ("sec-fetch-site", "cross-site"),
            ],
            &[("origin", "https://evil.algedonic.dev")],
            &[("origin", "null")],
            // A browser's mark decides alone: an Origin that looks like
            // ours does not outvote a cross-site fetch.
            &[
                ("origin", "https://boss.algedonic.dev"),
                ("sec-fetch-site", "cross-site"),
            ],
        ];
        for method in ["POST", "PUT", "PATCH", "DELETE"] {
            for shape in shapes {
                let (status, body, ran) = send(method, shape).await;
                assert_eq!(status, 403, "{method} {shape:?} -> {status} {body}");
                assert_eq!(body, CROSS_SITE_REFUSAL, "{method} {shape:?}");
                assert_eq!(ran, 0, "{method} {shape:?} reached the handler");
            }
        }
    }

    /// The gateway's own pages, a user-initiated request, and every
    /// non-browser caller (boss-api, the conductor, the forge runner —
    /// no Origin, no Sec-Fetch-Site) reach the handler unchanged.
    #[tokio::test]
    async fn same_origin_and_header_less_writes_reach_the_handler() {
        let shapes: [&[(&str, &str)]; 5] = [
            &[],
            &[("sec-fetch-site", "same-origin")],
            &[("sec-fetch-site", "none")],
            &[
                ("origin", "https://boss.algedonic.dev"),
                ("sec-fetch-site", "same-origin"),
            ],
            // An older browser: Origin alone, naming this host.
            &[("origin", "https://BOSS.algedonic.dev")],
        ];
        for method in ["POST", "PUT", "PATCH", "DELETE"] {
            for shape in shapes {
                let (status, body, ran) = send(method, shape).await;
                assert_eq!(
                    (status, body.as_str(), ran),
                    (200, "handled", 1),
                    "{method} {shape:?}"
                );
            }
        }
    }

    /// A read is never refused, whatever it carries.
    #[tokio::test]
    async fn a_read_is_never_refused() {
        for shape in [
            &[
                ("origin", "https://evil.algedonic.dev"),
                ("sec-fetch-site", "same-site"),
            ][..],
            &[("sec-fetch-site", "cross-site")][..],
            &[("origin", "null")][..],
        ] {
            let (status, body, ran) = send("GET", shape).await;
            assert_eq!(
                (status, body.as_str(), ran),
                (200, "handled", 1),
                "{shape:?}"
            );
        }
    }

    /// The tests above drive `mount` around a route; this holds `main`
    /// to doing the same around the FINISHED app — after the inquiry
    /// door, the last layer that handles a write, and before the
    /// dot-segment refusal, which stays outermost — so the site's write
    /// and every route are behind it.
    #[test]
    fn main_wraps_the_finished_app_in_the_refusal() {
        let main = include_str!("main.rs");
        let at = |needle: &str| {
            main.find(needle)
                .unwrap_or_else(|| panic!("main.rs no longer says `{needle}`"))
        };
        let inquiries = at("let app = inquiries::mount(app, door);");
        let refusal = at("let app = cross_site::mount(app);");
        let dots = at("let app = dot_segments::mount(app);");
        assert!(
            inquiries < refusal && refusal < dots,
            "the cross-site refusal must wrap the app after inquiries::mount and inside \
             dot_segments::mount"
        );
        // Order alone let a write-handling layer sit between the refusal
        // and the dots, or after the dots, and run ahead of the refusal
        // with this test green (backlog 884eee14): nothing else rebinds
        // `app` between them, and the dots' app goes straight to serve.
        let between = &main[refusal + "let app = cross_site::mount(app);".len()..dots];
        assert!(
            !between.contains("let app ="),
            "a layer between cross_site::mount and dot_segments::mount runs ahead of the \
             refusal: {between}"
        );
        assert_app_goes_straight_to_serve(main, dots + "let app = dot_segments::mount(app);".len());
    }

    /// `main`'s `app`, from byte `from` on, is next used by
    /// `axum::serve(` — no layer, rebinding or other use of it between
    /// (comment lines ignored). Shared with
    /// a_path_cannot_climb_out_of_its_route.rs, which pins the same tail.
    pub(crate) fn assert_app_goes_straight_to_serve(main: &str, from: usize) {
        let code: String = main[from..]
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let is_ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
        let next_app = code
            .match_indices("app")
            .map(|(i, _)| i)
            .find(|&i| {
                !code[..i].ends_with(is_ident) && !code[i + "app".len()..].starts_with(is_ident)
            })
            .expect("main.rs never uses `app` after the outermost layer");
        let before = &code[..next_app];
        assert!(
            before.contains("axum::serve(")
                || before.contains("boss_policy_client::role_service::serve_with_refresh("),
            "the outermost layer's `app` must go straight to the server; it is used first \
             here: {before}"
        );
    }

    #[test]
    fn the_server_reader_accepts_refresh_but_refuses_an_intervening_app_use() {
        for serve in [
            "axum::serve(",
            "boss_policy_client::role_service::serve_with_refresh(",
        ] {
            let direct = format!("{serve}listener, app.into_make_service()).await;");
            assert_app_goes_straight_to_serve(&direct, 0);
            for use_before in ["let app = unsafe_layer(app);", "inspect(app.clone());"] {
                let changed = format!("{use_before}\n{direct}");
                assert!(
                    std::panic::catch_unwind(|| assert_app_goes_straight_to_serve(&changed, 0))
                        .is_err(),
                    "the server reader accepted an app use ahead of {serve}"
                );
            }
        }
    }

    /// The LAN spelling of the gateway's own origin — host and port —
    /// is its own, and a different port on the same host is not.
    #[test]
    fn the_own_origin_carries_its_port() {
        let uri: Uri = "/api/jobs/x".parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "192.0.2.10:4443".parse().unwrap());
        headers.insert(header::ORIGIN, "http://192.0.2.10:4443".parse().unwrap());
        assert_eq!(refusal(&Method::POST, &headers, &uri), None);
        headers.insert(header::ORIGIN, "http://192.0.2.10:5173".parse().unwrap());
        assert_eq!(
            refusal(&Method::POST, &headers, &uri),
            Some(CROSS_SITE_REFUSAL)
        );
        // No Host to compare against: an Origin cannot be vouched for.
        headers.remove(header::HOST);
        headers.insert(header::ORIGIN, "http://192.0.2.10:4443".parse().unwrap());
        assert_eq!(
            refusal(&Method::POST, &headers, &uri),
            Some(CROSS_SITE_REFUSAL)
        );
        // HTTP/2 carries the host as the request's authority instead.
        let h2: Uri = "https://192.0.2.10:4443/api/jobs/x".parse().unwrap();
        assert_eq!(refusal(&Method::POST, &headers, &h2), None);
    }
}
