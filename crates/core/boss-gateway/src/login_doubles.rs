//! Test doubles for the two upstreams a login reads — people-api (the
//! employee row) and the Class registry (whether its role is live) — and
//! the one lock over the process environment that points a login at
//! them. Shared by the password tests in `local_auth` and the OIDC tests
//! in `oidc`, because both logins mint from an employee row through the
//! same `session_role` and read the same two variables.
//!
//! Until backlog 8f45e0b4 (2026-09-27) the doubles and the lock lived in
//! `oidc`'s test module, the password login had no test reaching past
//! its credential check, and the OIDC tests set `BOSS_CLASSES_URL` and
//! `BOSS_PEOPLE_UPSTREAM` process-wide and never reset them — so every
//! later test in the binary inherited the address of a double whose
//! runtime had already gone. [`UpstreamEnv`] restores what it set.

use std::ffi::OsString;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::http::StatusCode;
use axum::response::IntoResponse;

/// The login's people-api address (`local_auth::bootstrap_email`).
pub(crate) const PEOPLE_VAR: &str = "BOSS_PEOPLE_UPSTREAM";
/// The login's Class registry address (`local_auth::session_role`).
pub(crate) const CLASSES_VAR: &str = "BOSS_CLASSES_URL";

/// Serialises every test that points a login at a double: the addresses
/// are process environment, and tests run in parallel threads.
static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The environment lock, held, plus every variable set through it — each
/// put back as it was found when this drops, while the lock is still
/// held (fields drop after `Drop::drop` runs).
pub(crate) struct UpstreamEnv {
    saved: Vec<(&'static str, Option<OsString>)>,
    _lock: tokio::sync::MutexGuard<'static, ()>,
}

impl UpstreamEnv {
    pub(crate) async fn lock() -> Self {
        Self {
            _lock: ENV_LOCK.lock().await,
            saved: Vec::new(),
        }
    }

    /// Set `key` for the life of this guard. The first value seen is the
    /// one restored, so setting a key twice restores the original.
    pub(crate) fn set(&mut self, key: &'static str, value: &str) {
        if !self.saved.iter().any(|(k, _)| *k == key) {
            self.saved.push((key, std::env::var_os(key)));
        }
        // SAFETY: every writer of these variables holds ENV_LOCK.
        unsafe { std::env::set_var(key, value) };
    }
}

impl Drop for UpstreamEnv {
    fn drop(&mut self) {
        for (key, before) in self.saved.drain(..).rev() {
            // SAFETY: ENV_LOCK is still held; `_lock` drops after this.
            unsafe {
                match before {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
        }
    }
}

/// people-api double whose one employee row, `op@example.com`'s
/// (`emp-op`), carries `role` exactly as the row holds it, blank
/// included. Every other email is a 404.
pub(crate) async fn mock_people_as(role: &'static str) -> String {
    use axum::routing::get;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = axum::Router::new().route(
        "/api/people/by-email/{email}/bootstrap",
        get(
            move |axum::extract::Path(email): axum::extract::Path<String>| async move {
                if email == "op@example.com" {
                    axum::Json(serde_json::json!({
                        "id": "emp-op",
                        "role": role,
                        "department": "platform",
                    }))
                    .into_response()
                } else {
                    StatusCode::NOT_FOUND.into_response()
                }
            },
        ),
    );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}

/// Class registry double: `GET /api/classes?subject_kind=employee`
/// answers `rows` as `(member_attribute, code)` pairs, or a 500 when
/// `rows` is `None` — a registry that is up but cannot answer. It
/// counts every request, so a test can prove none was made.
pub(crate) async fn mock_classes(
    rows: Option<Vec<(&'static str, &'static str)>>,
) -> (String, Arc<AtomicUsize>) {
    use axum::routing::get;
    let asked = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let counter = asked.clone();
    let app = axum::Router::new().route(
        "/api/classes",
        get(move || {
            let rows = rows.clone();
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                let Some(rows) = rows else {
                    return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                };
                let body: Vec<serde_json::Value> = rows
                    .iter()
                    .map(|(attribute, code)| {
                        serde_json::json!({
                            "subject_kind": "employee",
                            "code": code,
                            "display_name": code,
                            "parent_code": null,
                            "member_attribute": attribute,
                            "metadata": {},
                            "sort_order": 0,
                            "retired_at": null,
                        })
                    })
                    .collect();
                axum::Json(body).into_response()
            }
        }),
    );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, asked)
}

/// How many requests a [`mock_classes`] double has answered.
pub(crate) fn asked(counter: &AtomicUsize) -> usize {
    counter.load(Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Backlog 8f45e0b4 item (5): a variable the guard set is put back
    /// as it was — restored when it held a value, removed when it held
    /// none — so no later test inherits a dead double's address.
    #[tokio::test]
    async fn the_guard_restores_what_it_set() {
        const KEY: &str = "BOSS_LOGIN_DOUBLES_GUARD_UNDER_TEST";
        {
            let mut env = UpstreamEnv::lock().await;
            assert_eq!(std::env::var_os(KEY), None);
            env.set(KEY, "first");
            env.set(KEY, "second");
            assert_eq!(std::env::var(KEY).as_deref(), Ok("second"));
        }
        assert_eq!(std::env::var_os(KEY), None, "an unset key is unset again");

        let mut env = UpstreamEnv::lock().await;
        // A value the guard did not set, written under the same lock.
        unsafe { std::env::set_var(KEY, "before") };
        env.set(KEY, "during");
        assert_eq!(std::env::var(KEY).as_deref(), Ok("during"));
        drop(env);
        assert_eq!(
            std::env::var(KEY).as_deref(),
            Ok("before"),
            "a held value is restored"
        );

        let _env = UpstreamEnv::lock().await;
        unsafe { std::env::remove_var(KEY) };
    }
}
