//! Serve the frontend SPA from a static directory.
//!
//! Replaces the reverse-proxy-to-observability approach. The gateway
//! reads files from `BOSS_STATIC_DIR` (default `/var/lib/boss-web/dist`)
//! and serves them directly. Unknown paths return `index.html` so the
//! client-side router handles navigation.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::AppState;
use boss_gateway::session::{self, Session, find_cookie};

/// Resolve the static directory (cached via env on first call).
pub fn static_dir() -> &'static str {
    static DIR: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        std::env::var("BOSS_STATIC_DIR").unwrap_or_else(|_| "/var/lib/boss-web/dist".to_string())
    })
}

/// Pages served without a session.
///
/// `/login` is the load-bearing one, and its absence here was a
/// circular lock: the page that hands out a session required one to
/// load, so a signed-out visitor got a bare "authentication required"
/// and no way forward.
///
/// It went unnoticed because demo mode used to mint a session for
/// anyone arriving without a valid cookie — the minter ran ahead of
/// this gate, so in practice nobody ever reached it unauthenticated.
/// Removing the minter did not create the bug; it revealed one that
/// had been masked since the gate was written.
///
/// Anything added here is readable by the entire internet. The bar is
/// "a signed-out visitor cannot proceed without it", not "it's
/// convenient".
fn is_public_path(path: &str) -> bool {
    matches!(path, "/" | "/login" | "/login/" | "/health")
        // /auth is the SSH-CA endpoint (CLI operator flow).
        || path.starts_with("/auth")
}

/// Handle all `/dashboard/*` and root `/*` requests for the SPA.
pub async fn handle(State(state): State<Arc<AppState>>, req: Request) -> Response {
    // Session gate for /dashboard/* HTML pages.
    // Static assets (JS, CSS, fonts, images) are always served —
    // they're content-hashed and not sensitive. Only HTML pages
    // require authentication, because the SPA code inside handles
    // the auth redirect flow. If we redirect asset requests to
    // auth, the browser gets a CORS error and can't load at all.
    let path = req.uri().path();
    let is_static_asset = has_file_extension(path);
    if !is_static_asset
        && !is_public_path(path)
        && !has_valid_session(req.headers(), &state.session_key)
    {
        return unauthorized();
    }

    // Strip /dashboard prefix if present. Both / and /dashboard
    // serve the same SPA — the client-side router handles navigation.
    let stripped = path.strip_prefix("/dashboard").unwrap_or(path);
    let file_path = match stripped {
        "" | "/" => "/index.html",
        other => other,
    };

    // Resolve to a file on disk.
    let base = static_dir();
    let clean = file_path.trim_start_matches('/');
    let full_path = PathBuf::from(base).join(clean);

    // Security: don't serve files outside the static dir.
    if !full_path.starts_with(base) {
        return StatusCode::FORBIDDEN.into_response();
    }

    // Try to read the file. If it doesn't exist, serve index.html (SPA fallback).
    let (content, serving_path) = match tokio::fs::read(&full_path).await {
        Ok(bytes) => (bytes, full_path),
        Err(_) => {
            // SPA fallback: serve index.html for any non-file path.
            let index = PathBuf::from(base).join("index.html");
            match tokio::fs::read(&index).await {
                Ok(bytes) => (bytes, index),
                Err(_) => {
                    return (
                        StatusCode::NOT_FOUND,
                        "frontend not built — run: cd apps/web && bun run build",
                    )
                        .into_response();
                }
            }
        }
    };

    let content_type = guess_content_type(&serving_path);
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, content_type);

    // Cache static assets (JS, CSS) aggressively — they have content hashes in filenames.
    // Don't cache index.html *anywhere* — Cloudflare's edge will hold
    // it for minutes under plain `no-cache` (which means "store but
    // revalidate"), and a stale HTML pointing at a no-longer-current
    // chunk hash causes "I redeployed but the user sees the old app"
    // exactly because the browser then trusts its own immutable
    // cache for the stale hash. The fix is to tell every layer to
    // not store it at all.
    if !serving_path.ends_with("index.html") {
        headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=31536000, immutable"),
        );
    } else {
        headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store, no-cache, must-revalidate, max-age=0"),
        );
        // Belt-and-braces for HTTP/1.0 + CDNs that ignore Cache-Control.
        headers.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
        // Standard CDN-only directive. Cloudflare honors this even
        // when the regular Cache-Control would be edge-cached.
        headers.insert(
            axum::http::HeaderName::from_static("cdn-cache-control"),
            HeaderValue::from_static("no-store"),
        );
        // Cloudflare-specific override. Set so a future CF page rule
        // can't accidentally re-introduce HTML caching.
        headers.insert(
            axum::http::HeaderName::from_static("cloudflare-cdn-cache-control"),
            HeaderValue::from_static("no-store"),
        );
    }

    (StatusCode::OK, headers, content).into_response()
}

fn guess_content_type(path: &Path) -> HeaderValue {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let ct = match ext {
        "html" => "text/html; charset=utf-8",
        "js" => "application/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    };
    HeaderValue::from_static(ct)
}

fn has_valid_session(headers: &HeaderMap, key: &[u8]) -> bool {
    let Some(cookie_header) = headers.get(header::COOKIE).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let Some(raw) = find_cookie(cookie_header, session::COOKIE_NAME) else {
        return false;
    };
    Session::decode(raw, key).is_ok()
}

/// True if the path has a file extension (e.g., .js, .css, .woff2).
/// Used to distinguish asset requests from SPA page navigations.
fn has_file_extension(path: &str) -> bool {
    let last_segment = path.rsplit('/').next().unwrap_or("");
    last_segment.contains('.')
}

fn unauthorized() -> Response {
    (StatusCode::UNAUTHORIZED, "authentication required").into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression, and the expensive kind: a signed-out visitor asked
    /// for the sign-in page and got "authentication required". Demo
    /// mode had masked it by minting a session ahead of this gate, so
    /// the circularity only surfaced once the minter was removed.
    #[test]
    fn the_sign_in_page_is_reachable_without_a_session() {
        assert!(is_public_path("/login"));
        assert!(is_public_path("/login/"));
    }

    #[test]
    fn the_landing_surface_and_probes_stay_public() {
        assert!(is_public_path("/"));
        assert!(is_public_path("/health"));
        assert!(is_public_path("/auth/ssh-ca"));
    }

    /// The gate still has to gate. If this ever passes, the session
    /// check has been widened into a no-op.
    #[test]
    fn application_pages_are_not_public() {
        for path in ["/ux/jobs", "/system", "/me", "/finance"] {
            assert!(!is_public_path(path), "{path} must require a session");
        }
    }

    #[test]
    fn guess_content_type_for_known_extensions() {
        assert_eq!(
            guess_content_type(Path::new("app.js")),
            "application/javascript; charset=utf-8"
        );
        assert_eq!(
            guess_content_type(Path::new("style.css")),
            "text/css; charset=utf-8"
        );
        assert_eq!(
            guess_content_type(Path::new("index.html")),
            "text/html; charset=utf-8"
        );
    }

    #[test]
    fn unknown_extension_is_octet_stream() {
        assert_eq!(
            guess_content_type(Path::new("data.bin")),
            "application/octet-stream"
        );
    }
}
