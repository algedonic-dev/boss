//! Serve step-plugin frontend bundles.
//!
//! Plugins are JavaScript files that register a React component via
//! `window.__boss_register_step_plugin(kind, Component)`. The gateway
//! serves them from `/var/lib/boss/step-plugins/` (configurable via
//! `BOSS_PLUGINS_DIR`), mounted under `/plugins/<filename>`.
//!
//! Session-gated: same cookie check as the SPA itself, since a plugin
//! can read step metadata via the SDK and we don't want unauth'd
//! callers grabbing them.
//!
//! Q2 of the step-ux-plugin-model design: files on disk, not DB
//! BYTEA — matches every other static asset in the repo.

use std::path::Path;
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::AppState;
use crate::static_files::{read_inside, resolve};
use boss_gateway::session::{self, find_cookie};

/// Resolve the plugins directory (cached via env on first call).
pub fn plugins_dir() -> &'static str {
    static DIR: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        std::env::var("BOSS_PLUGINS_DIR")
            .unwrap_or_else(|_| "/var/lib/boss/step-plugins".to_string())
    })
}

pub async fn handle(State(state): State<Arc<AppState>>, req: Request) -> Response {
    serve(&state.session_key, req, Path::new(plugins_dir())).await
}

/// The handler's body, with the directory it serves from as an
/// argument so a test can point it at a scratch directory — the
/// `plugins_dir()` cache is process-wide and read once.
async fn serve(session_key: &[u8], req: Request, base: &Path) -> Response {
    // Require a valid session. The SPA loads plugin scripts after
    // login; unauthenticated access returns 401 rather than a redirect
    // (browsers can't follow HTML redirects from `<script>` tags).
    if !has_valid_session(req.headers(), session_key) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let path = req.uri().path();
    let rel = match path.strip_prefix("/plugins/") {
        Some(r) if !r.is_empty() => r,
        _ => return StatusCode::NOT_FOUND.into_response(),
    };

    // Resolve through the static handler's two walls (backlog
    // a77ac150). This used to refuse by the STRING test
    // `contains("..") || starts_with('/')` and read the joined path,
    // which parses no component and resolves nothing: a symlink inside
    // the directory pointing out of it was followed. `resolve` admits
    // only plain-name components (no dot segment, no backslash, no
    // root); `read_inside` below reads only what, with every link
    // resolved by the OS, still sits inside the directory.
    let Some(file_path) = resolve(base, rel) else {
        return StatusCode::BAD_REQUEST.into_response();
    };

    // Only serve .js files. MIME sniffers treat untyped downloads as
    // text/plain; being explicit avoids browsers refusing a module.
    let is_js = file_path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("js"))
        .unwrap_or(false);
    if !is_js {
        return StatusCode::NOT_FOUND.into_response();
    }

    match read_inside(base, &file_path).await {
        Ok(bytes) => {
            let mut resp = (StatusCode::OK, bytes).into_response();
            resp.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/javascript; charset=utf-8"),
            );
            // Short cache — plugin bundles are versioned by content
            // hash at publish time; cache-bust on edit via the
            // filename, not this header.
            resp.headers_mut().insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("private, max-age=60"),
            );
            resp
        }
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

fn has_valid_session(headers: &HeaderMap, key: &[u8]) -> bool {
    let Some(cookie) = headers.get(header::COOKIE).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let Some(raw) = find_cookie(cookie, session::COOKIE_NAME) else {
        return false;
    };
    session::Session::decode(raw, key).is_ok()
}

/// A request path cannot read outside the plugins directory (backlog
/// a77ac150). Until 2026-09-27 this handler refused a path by the
/// STRING test `contains("..") || starts_with('/')` and then read
/// whatever the joined path named, so a symlink inside the directory
/// pointing out of it was followed. Every answer below is the handler's
/// own, against a scratch directory with a `.js` secret planted above it.
#[cfg(test)]
mod traversal_tests {
    use super::*;
    use axum::body::Body;
    use http_body_util::BodyExt;
    use std::path::PathBuf;

    const SECRET: &str = "SECRET-PLUGIN-BYTES-THAT-MUST-NEVER-LEAVE";
    const KEY: [u8; 32] = [9u8; 32];

    /// `<root>/plugins` is the plugins directory; `secret.js` sits in
    /// `<root>`, one climb away, and a real bundle sits inside.
    fn fixture(case: &str) -> (PathBuf, PathBuf) {
        let root = boss_testing::scratch_dir(&format!("gateway-plugins-{case}"));
        let dir = root.join("plugins");
        boss_testing::create_dir(&dir.join("nested"));
        boss_testing::write_file(&dir.join("review-design.js"), "register('review')");
        boss_testing::write_file(&dir.join("nested/inner.js"), "register('inner')");
        boss_testing::write_file(&root.join("secret.js"), SECRET);
        (root, dir)
    }

    /// The handler's answer for `path`, sent as-is, signed in unless
    /// told otherwise.
    async fn get(dir: &Path, path: &str, signed_in: bool) -> (StatusCode, HeaderMap, String) {
        let mut req = Request::builder().uri(path);
        if signed_in {
            let cookie = session::Session::new("tester", 3600).encode(&KEY);
            req = req.header(header::COOKIE, format!("{}={cookie}", session::COOKIE_NAME));
        }
        let req = req.body(Body::empty()).unwrap();
        assert_eq!(
            req.uri().path(),
            path,
            "the request must carry the path raw"
        );
        let resp = serve(&KEY, req, dir).await;
        let status = resp.status();
        let headers = resp.headers().clone();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        (status, headers, String::from_utf8_lossy(&body).into_owned())
    }

    /// A path of plain names that the OS resolves out of the directory
    /// — a symlink inside it pointing up — reads as absent. The string
    /// check this replaced passed every one of these.
    #[tokio::test]
    async fn a_symlink_out_of_the_plugins_dir_is_not_followed() {
        let (root, dir) = fixture("symlink");
        std::os::unix::fs::symlink(root.join("secret.js"), dir.join("link.js")).unwrap();
        std::os::unix::fs::symlink(root.clone(), dir.join("up")).unwrap();
        for path in ["/plugins/link.js", "/plugins/up/secret.js"] {
            let (status, _, body) = get(&dir, path, true).await;
            assert!(!body.contains(SECRET), "{path} followed a link out");
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}: {body}");
        }
    }

    /// Every other climb shape: raw and encoded dots, backslashes, and
    /// absolute paths. None may read the secret, whatever it answers.
    #[tokio::test]
    async fn no_climb_reads_a_file_outside_the_plugins_dir() {
        let (root, dir) = fixture("climb");
        let abs = root.join("secret.js");
        let abs = abs.to_str().unwrap();
        let mut paths: Vec<String> = [
            "/plugins/../secret.js",
            "/plugins/./../secret.js",
            "/plugins/nested/../../secret.js",
            "/plugins/%2e%2e/secret.js",
            "/plugins/..%2fsecret.js",
            "/plugins/..%5csecret.js",
            "/plugins/..\\secret.js",
            "/plugins/nested\\..\\..\\secret.js",
        ]
        .map(String::from)
        .to_vec();
        paths.push(format!("/plugins/{abs}"));
        paths.push(format!("/plugins//{abs}"));
        for path in paths {
            let (status, _, body) = get(&dir, &path, true).await;
            assert!(
                !body.contains(SECRET),
                "{path} read outside the plugins dir"
            );
            assert_ne!(status, StatusCode::OK, "{path}: {body}");
        }
    }

    /// A bundle inside the directory, at the top or nested, is served
    /// as JavaScript to a session and to no one else.
    #[tokio::test]
    async fn a_bundle_inside_the_dir_is_served_to_a_session_only() {
        let (_, dir) = fixture("serve");
        for (path, want) in [
            ("/plugins/review-design.js", "register('review')"),
            ("/plugins/nested/inner.js", "register('inner')"),
        ] {
            let (status, headers, body) = get(&dir, path, true).await;
            assert_eq!((status, body.as_str()), (StatusCode::OK, want), "{path}");
            assert_eq!(
                headers[header::CONTENT_TYPE],
                "application/javascript; charset=utf-8"
            );
            let (status, _, _) = get(&dir, path, false).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
        }
        let (status, _, _) = get(&dir, "/plugins/gone.js", true).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
