//! The GitHub App installation-token handler, driven through its REAL
//! adapter (`GitHubApi`) against the GitHub stub — a JWT the stub
//! verifies against the test App key, a 201 and only a 201, tokens that
//! work only while they live — and a stateful jobs API, so a redelivery
//! reads what the first firing wrote.

use super::*;
use crate::handlers::credential_issuer::{GitHubApi, GitHubAppRoot, Unconfigured};
use crate::handlers::github_stub::{self, APP_ID, GitHubStub, INSTALLATION_ID};
use std::sync::Mutex;

const NS: &str = "boss";
const SECRET: &str = "github-dr-push-token";
const KEY: &str = "token";
const CREDENTIAL: &str = "github-dr-push-token";
const REPO: &str = "algedonic-dev/boss-dr";
const JOB: &str = "81eb6d4d-3537-4e8d-bcec-2e5ee318a77e";

// ----- the Secret store -----

#[derive(Default)]
struct FakeSecrets {
    map: Mutex<HashMap<String, String>>,
    writes: Mutex<usize>,
    /// A token to look for at GitHub on every write: whether it was
    /// still live at that moment is recorded in `held_live_at_write`.
    watch: Mutex<Option<(Arc<Mutex<github_stub::GitHubState>>, String)>>,
    held_live_at_write: Mutex<Vec<bool>>,
    /// A key whose write fails — the firing dying at that write.
    dies_writing: Mutex<Option<String>>,
}

impl FakeSecrets {
    fn watch(&self, gh: &GitHubStub, token: &str) {
        *self.watch.lock().unwrap() = Some((gh.state.clone(), token.to_string()));
    }
    fn get(&self, key: &str) -> Option<String> {
        self.map
            .lock()
            .unwrap()
            .get(&format!("{NS}/{SECRET}/{key}"))
            .cloned()
    }
    fn seed(&self, key: &str, value: &str) {
        self.map
            .lock()
            .unwrap()
            .insert(format!("{NS}/{SECRET}/{key}"), value.to_string());
    }
    fn writes(&self) -> usize {
        *self.writes.lock().unwrap()
    }
}

#[async_trait]
impl SecretStore for FakeSecrets {
    async fn read_key(&self, ns: &str, name: &str, key: &str) -> Result<Option<String>, String> {
        Ok(self
            .map
            .lock()
            .unwrap()
            .get(&format!("{ns}/{name}/{key}"))
            .cloned())
    }
    async fn write_key(&self, ns: &str, name: &str, key: &str, value: &str) -> Result<(), String> {
        if self.dies_writing.lock().unwrap().as_deref() == Some(key) {
            return Err(format!("the firing died writing {key}"));
        }
        *self.writes.lock().unwrap() += 1;
        // Only the write that REPLACES the value is judged: the expiry is
        // deliberately marked past before the revoke, while the held token
        // still lives.
        if key == KEY
            && let Some((state, token)) = self.watch.lock().unwrap().as_ref()
        {
            let live = state.lock().unwrap().live.contains(token);
            self.held_live_at_write.lock().unwrap().push(live);
        }
        self.map
            .lock()
            .unwrap()
            .insert(format!("{ns}/{name}/{key}"), value.to_string());
        Ok(())
    }
}

// ----- the jobs API: a rotation packet, stateful -----

type Captured = Arc<Mutex<Vec<(String, JsonValue)>>>;

struct Jobs {
    url: String,
    steps: Arc<Mutex<Vec<(String, String)>>>,
    writes: Captured,
    rotations: Captured,
}

impl Jobs {
    fn status(&self, slug: &str) -> String {
        self.steps
            .lock()
            .unwrap()
            .iter()
            .find(|(s, _)| s == slug)
            .map(|(_, st)| st.clone())
            .unwrap_or_default()
    }
    fn phases(&self) -> Vec<String> {
        self.rotations
            .lock()
            .unwrap()
            .iter()
            .map(|(p, _)| p.clone())
            .collect()
    }
    /// Everything the handler wrote to the system of record, flat.
    fn everything_written(&self) -> String {
        let w = self.writes.lock().unwrap();
        let r = self.rotations.lock().unwrap();
        w.iter()
            .chain(r.iter())
            .map(|(k, v)| format!("{k} {v}"))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

async fn jobs(step_statuses: &[(&str, &str)]) -> Jobs {
    use axum::extract::Path;
    use axum::response::IntoResponse;
    use axum::{Json, Router, routing::get, routing::post, routing::put};

    let steps = Arc::new(Mutex::new(
        step_statuses
            .iter()
            .map(|(s, st)| (s.to_string(), st.to_string()))
            .collect::<Vec<_>>(),
    ));
    let writes: Captured = Default::default();
    let rotations: Captured = Default::default();
    let slug_of = |sid: &str| sid.trim_start_matches("step-").to_string();
    let (st_get, st_put) = (steps.clone(), steps.clone());
    let (w_put, w_merge) = (writes.clone(), writes.clone());
    let rot = rotations.clone();
    let app = Router::new()
        .route(
            "/api/jobs/{id}",
            get(move |Path(id): Path<String>| {
                let st = st_get.clone();
                async move {
                    let steps: Vec<JsonValue> = st
                        .lock()
                        .unwrap()
                        .iter()
                        .map(|(slug, status)| {
                            json!({"id": format!("step-{slug}"), "spec_slug": slug, "status": status})
                        })
                        .collect();
                    Json(json!({"id": id, "metadata": {}, "steps": steps}))
                }
            }),
        )
        .route(
            "/api/jobs/{id}/steps/{step_id}",
            put(
                move |Path((id, sid)): Path<(String, String)>, Json(body): Json<JsonValue>| {
                    let (st, w) = (st_put.clone(), w_put.clone());
                    async move {
                        if let Some(refused) =
                            crate::handlers::listing_stub::end_state_step_put(&id, &sid, &body)
                        {
                            return refused;
                        }
                        if let Some(status) = body.get("status").and_then(|v| v.as_str()) {
                            let slug = slug_of(&sid);
                            if let Some(row) = st.lock().unwrap().iter_mut().find(|(s, _)| *s == slug)
                            {
                                row.1 = status.to_string();
                            }
                        }
                        w.lock().unwrap().push((sid, body));
                        Json(json!({"ok": true})).into_response()
                    }
                },
            ),
        )
        .route(
            "/api/jobs/{id}/steps/{step_id}/metadata",
            axum::routing::patch(
                move |Path((_id, sid)): Path<(String, String)>, Json(body): Json<JsonValue>| {
                    let w = w_merge.clone();
                    async move {
                        w.lock().unwrap().push((format!("{sid}/metadata"), body));
                        Json(json!({"ok": true}))
                    }
                },
            ),
        )
        .route(
            "/api/credentials/{id}/rotation/{phase}",
            post(
                move |Path((id, phase)): Path<(String, String)>, Json(body): Json<JsonValue>| {
                    let rot = rot.clone();
                    async move {
                        rot.lock().unwrap().push((format!("{id}/{phase}"), body));
                        Json(json!({"recorded": true}))
                    }
                },
            ),
        )
        // The registry: only the credential this suite declares has a row.
        .route(
            "/api/credentials/{id}",
            get(move |Path(id): Path<String>| async move {
                if id == CREDENTIAL {
                    Json(json!({"id": id, "kind": REGISTRY_KIND})).into_response()
                } else {
                    axum::http::StatusCode::NOT_FOUND.into_response()
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Jobs {
        url: format!("http://{addr}"),
        steps,
        writes,
        rotations,
    }
}

const PENDING: &[(&str, &str)] = &[
    ("scope", "completed"),
    ("issue", "ready"),
    ("install", "pending"),
    ("verify", "pending"),
    ("delivered", "pending"),
    ("revoke", "pending"),
];

// ----- wiring -----

fn root() -> GitHubAppRoot {
    GitHubAppRoot::from_values(
        Some(&APP_ID.to_string()),
        Some(&INSTALLATION_ID.to_string()),
        Some(&github_stub::app_key_pem()),
    )
    .expect("the test root is whole")
}

fn issuer(gh: &GitHubStub) -> Arc<dyn GitHubAppIssuer> {
    GitHubApi::new(gh.url.clone(), root()).expect("adapter")
}

fn args(extra: &[(&str, &str)]) -> Vec<(String, Value)> {
    [
        ("secret_namespace", NS),
        ("secret_name", SECRET),
        ("secret_key", KEY),
        ("credential_id", CREDENTIAL),
        ("verify_repo", REPO),
        ("repositories", "boss-dr"),
        ("permissions", "contents:write,metadata:read"),
    ]
    .iter()
    .chain(extra.iter())
    .map(|(k, v)| (k.to_string(), Value::String((*v).into())))
    .collect()
}

fn refresh_args() -> Vec<(String, Value)> {
    args(&[("phase", "refresh"), ("refresh_within_minutes", "40")])
}

fn tick_ctx() -> InvocationContext {
    InvocationContext {
        event_timestamp: None,
        rule_name: "broker-refreshes-the-github-dr-push-token".into(),
        triggering_event_id: "clock-tick:2026-09-27T22:15:00+00:00".into(),
        triggering_topic: "clock.tick".into(),
        event_payload: json!({"_day": "2026-09-27", "_at": "2026-09-27T22:15:00Z"}),
    }
}

fn scope_ctx(metadata: JsonValue) -> InvocationContext {
    InvocationContext {
        event_timestamp: None,
        rule_name: "broker-rotates-the-github-dr-push-token".into(),
        triggering_event_id: "evt-scope".into(),
        triggering_topic: "step.done.credential-rotation".into(),
        event_payload: json!({
            "job_id": JOB,
            "step_id": "step-scope",
            "kind": "credential-rotation",
            "subject_kind": "custom",
            "subject_id": CREDENTIAL,
            "metadata": metadata,
        }),
    }
}

/// A live token minted at the stub through the adapter itself — what a
/// previous refresh would have left in the Secret.
async fn a_live_token(gh: &GitHubStub) -> String {
    issuer(gh)
        .mint_installation_token(&InstallationScope::default())
        .await
        .expect("mint")
        .token
}

fn in_minutes(m: i64) -> String {
    (chrono::Utc::now() + chrono::Duration::minutes(m)).to_rfc3339()
}

// ----- pure planning -----

#[test]
fn a_refresh_mints_only_inside_its_window() {
    let now = chrono::Utc::now();
    let at = |m: i64| (now + chrono::Duration::minutes(m)).to_rfc3339();
    assert!(matches!(
        plan_refresh(None, None, now, 40),
        RefreshPlan::Mint { .. }
    ));
    assert!(matches!(
        plan_refresh(Some(""), Some(&at(55)), now, 40),
        RefreshPlan::Mint { .. }
    ));
    assert!(matches!(
        plan_refresh(Some("ghs_x"), None, now, 40),
        RefreshPlan::Mint { .. }
    ));
    assert!(matches!(
        plan_refresh(Some("ghs_x"), Some("not a time"), now, 40),
        RefreshPlan::Mint { .. }
    ));
    assert_eq!(
        plan_refresh(Some("ghs_x"), Some(&at(55)), now, 40),
        RefreshPlan::Fresh { minutes_left: 55 }
    );
    let RefreshPlan::Mint { why } = plan_refresh(Some("ghs_x"), Some(&at(20)), now, 40) else {
        panic!("20 minutes left is inside a 40-minute window");
    };
    assert!(why.contains("40-minute refresh window"), "{why}");
    let RefreshPlan::Mint { why } = plan_refresh(Some("ghs_x"), Some(&at(-5)), now, 40) else {
        panic!("an expired token is re-minted");
    };
    assert!(why.contains("expired"), "{why}");
}

#[test]
fn the_declared_narrowing_parses_or_is_refused() {
    let p = parse_permissions("contents:write, metadata:read").unwrap();
    assert_eq!(p.get("contents").map(String::as_str), Some("write"));
    assert_eq!(p.get("metadata").map(String::as_str), Some("read"));
    for bad in [
        "contents",
        "contents:owner",
        "Contents:write",
        "contents:write,contents:read",
    ] {
        assert!(parse_permissions(bad).is_err(), "{bad}");
    }
    assert_eq!(
        parse_repositories("boss-dr, boss").unwrap(),
        vec!["boss-dr".to_string(), "boss".to_string()]
    );
    assert!(parse_repositories("boss-dr,../boss").is_err());
}

#[test]
fn a_scope_can_name_only_the_value_the_secret_holds() {
    let held = "ghs_0123456789abcdefABCDEF0123456789wxyz";
    assert!(judge_scope_naming(None, None, Some(held)).is_ok());
    assert!(judge_scope_naming(None, None, None).is_ok());
    assert!(judge_scope_naming(None, Some("6789wxyz"), Some(held)).is_ok());
    let e = judge_scope_naming(None, Some("deadbeef"), Some(held)).unwrap_err();
    assert!(e.contains("6789wxyz") && e.contains("deadbeef"), "{e}");
    assert!(judge_scope_naming(None, Some("6789wxyz"), None).is_err());
    let e = judge_scope_naming(Some("the-old-one"), None, Some(held)).unwrap_err();
    assert!(e.contains("no name or id"), "{e}");
}

// ----- the refresh (clock) door -----

#[tokio::test]
async fn a_refresh_into_an_empty_secret_mints_verifies_and_installs_one_value() {
    let gh = github_stub::serve(&[REPO]).await;
    let api = jobs(&[]).await;
    let secrets = Arc::new(FakeSecrets::default());
    let h = CredentialRotateGitHubApp::new(api.url.clone(), issuer(&gh), secrets.clone());

    h.invoke(&refresh_args(), &tick_ctx())
        .await
        .expect("refresh");

    assert!(
        gh.state.lock().unwrap().jwt_refusals.is_empty(),
        "the stub accepted the App JWT: {:?}",
        gh.state.lock().unwrap().jwt_refusals
    );
    let token = secrets.get(KEY).expect("a token is installed");
    assert!(
        gh.state.lock().unwrap().live.contains(&token),
        "the installed value is a live installation token"
    );
    assert_eq!(
        secrets.get("token.minted-for").as_deref(),
        Some(MINTED_BY_REFRESH)
    );
    let expires = secrets
        .get("token.expires-at")
        .expect("its expiry beside it");
    assert!(
        chrono::DateTime::parse_from_rfc3339(&expires).is_ok(),
        "{expires}"
    );
    assert_eq!(
        gh.state.lock().unwrap().mint_bodies,
        vec![json!({
            "repositories": ["boss-dr"],
            "permissions": {"contents": "write", "metadata": "read"},
        })],
        "the exchange is narrowed to exactly what the rule declares"
    );

    // ONE event: installed, stamping rotated_at; the value nowhere.
    assert_eq!(api.phases(), vec![format!("{CREDENTIAL}/installed")]);
    let ev = api.rotations.lock().unwrap()[0].1.clone();
    assert_eq!(ev["refresh"], true);
    assert_eq!(ev["value_length"], token.len());
    assert_eq!(ev["trigger"], "clock-tick:2026-09-27T22:15:00+00:00");
    assert!(
        !api.everything_written().contains(&token),
        "the value reached the record"
    );
}

#[tokio::test]
async fn a_refresh_leaves_a_token_that_outlives_the_window_alone() {
    let gh = github_stub::serve(&[REPO]).await;
    let api = jobs(&[]).await;
    let secrets = Arc::new(FakeSecrets::default());
    secrets.seed(KEY, "ghs_still_good");
    secrets.seed("token.expires-at", &in_minutes(55));
    let h = CredentialRotateGitHubApp::new(api.url.clone(), issuer(&gh), secrets.clone());

    h.invoke(&refresh_args(), &tick_ctx())
        .await
        .expect("a fresh token is not an error");

    assert_eq!(gh.minted(), 0, "nothing minted");
    assert_eq!(secrets.writes(), 0, "nothing written");
    assert!(api.phases().is_empty(), "nothing recorded");
}

#[tokio::test]
async fn a_refresh_replaces_a_token_inside_the_window_and_leaves_the_old_one_to_expire() {
    let gh = github_stub::serve(&[REPO]).await;
    let old = a_live_token(&gh).await;
    let api = jobs(&[]).await;
    let secrets = Arc::new(FakeSecrets::default());
    secrets.seed(KEY, &old);
    secrets.seed("token.expires-at", &in_minutes(20));
    let h = CredentialRotateGitHubApp::new(api.url.clone(), issuer(&gh), secrets.clone());

    h.invoke(&refresh_args(), &tick_ctx())
        .await
        .expect("refresh");

    let new = secrets.get(KEY).unwrap();
    assert_ne!(new, old);
    let s = gh.state.lock().unwrap();
    assert!(
        s.live.contains(&old),
        "a refresh revokes nothing: an off-host consumer may still read the old value"
    );
    assert!(s.revoked.is_empty());
}

#[tokio::test]
async fn a_token_that_fails_verification_is_never_written() {
    let gh = github_stub::serve(&[REPO]).await;
    let old = a_live_token(&gh).await;
    gh.state.lock().unwrap().mints_useless_tokens = true;
    let api = jobs(PENDING).await;
    let secrets = Arc::new(FakeSecrets::default());
    secrets.seed(KEY, &old);
    secrets.seed("token.expires-at", &in_minutes(10));
    let h = CredentialRotateGitHubApp::new(api.url.clone(), issuer(&gh), secrets.clone());

    // Both doors: the refresh and the rotation packet.
    for (a, ctx) in [
        (refresh_args(), tick_ctx()),
        (args(&[]), scope_ctx(json!({}))),
    ] {
        let err = h.invoke(&a, &ctx).await.expect_err("an unverified token");
        assert!(matches!(err, HandlerError::Downstream(_)), "{err:?}");
        assert!(err.to_string().contains("NOT"), "{err}");
    }
    assert_eq!(secrets.writes(), 0, "the Secret was never written");
    assert_eq!(secrets.get(KEY).as_deref(), Some(old.as_str()));
    assert!(
        gh.state.lock().unwrap().live.contains(&old),
        "the held value is not revoked when its replacement is unproven"
    );
    assert!(
        !api.phases().iter().any(|p| p.ends_with("/installed")
            || p.ends_with("/verified")
            || p.ends_with("/revoked")),
        "{:?}",
        api.phases()
    );
    assert!(
        api.writes.lock().unwrap().is_empty(),
        "no packet step was touched"
    );
}

#[tokio::test]
async fn an_exchange_that_answers_anything_but_201_mints_and_writes_nothing() {
    for status in [200u16, 403, 422, 500] {
        let gh = github_stub::serve(&[REPO]).await;
        gh.state.lock().unwrap().mint_status = Some(status);
        let api = jobs(PENDING).await;
        let secrets = Arc::new(FakeSecrets::default());
        let h = CredentialRotateGitHubApp::new(api.url.clone(), issuer(&gh), secrets.clone());

        let err = h
            .invoke(&refresh_args(), &tick_ctx())
            .await
            .expect_err("refused");
        let text = err.to_string();
        assert!(text.contains(&status.to_string()), "{status}: {text}");
        assert!(text.contains("not 201"), "{status}: {text}");
        assert_eq!(secrets.writes(), 0, "{status}: nothing written");
        assert!(api.phases().is_empty(), "{status}: nothing recorded");
        if status == 200 {
            assert!(text.contains("body withheld"), "{text}");
        }
    }
}

/// The root is whole or the handler refuses, naming every key it lacks —
/// the dispatcher registers the handler over `Unconfigured` built from
/// exactly this refusal, so a firing before David places the root says
/// what to place.
#[tokio::test]
async fn a_root_missing_any_of_its_three_keys_is_refused_naming_it() {
    let (id, inst, pem) = (
        APP_ID.to_string(),
        INSTALLATION_ID.to_string(),
        github_stub::app_key_pem(),
    );
    let cases: [(Option<&str>, Option<&str>, Option<&str>, &str); 4] = [
        (
            None,
            Some(&inst),
            Some(&pem),
            "github-app.id (env BOSS_BROKER_GITHUB_APP_ID)",
        ),
        (
            Some(&id),
            None,
            Some(&pem),
            "github-app.installation-id (env BOSS_BROKER_GITHUB_APP_INSTALLATION_ID)",
        ),
        (
            Some(&id),
            Some(&inst),
            Some("  "),
            "github-app.private-key.pem (env BOSS_BROKER_GITHUB_APP_PRIVATE_KEY)",
        ),
        (None, None, None, "github-app.id"),
    ];
    for (a, b, c, named) in cases {
        let why = GitHubAppRoot::from_values(a, b, c).expect_err(named);
        assert!(why.contains(named), "{why}");
        assert!(why.contains("boss/boss-credential-broker-root"), "{why}");

        let gh = github_stub::serve(&[REPO]).await;
        let api = jobs(PENDING).await;
        let secrets = Arc::new(FakeSecrets::default());
        let h = CredentialRotateGitHubApp::new(
            api.url.clone(),
            Arc::new(Unconfigured(why.clone())),
            secrets.clone(),
        );
        for (a, ctx) in [
            (refresh_args(), tick_ctx()),
            (args(&[]), scope_ctx(json!({}))),
        ] {
            let err = h.invoke(&a, &ctx).await.expect_err("unconfigured");
            assert!(err.to_string().contains(named), "{err}");
        }
        assert_eq!(secrets.writes(), 0);
        assert!(gh.state.lock().unwrap().requests.is_empty());
    }
    let all = GitHubAppRoot::from_values(None, None, None).unwrap_err();
    for key in [
        "github-app.id",
        "github-app.installation-id",
        "github-app.private-key.pem",
    ] {
        assert!(
            all.contains(key),
            "every missing key is named at once: {all}"
        );
    }
}

#[test]
fn a_root_whose_values_are_not_ids_or_a_key_is_refused_without_quoting_them() {
    let pem = github_stub::app_key_pem();
    let e = GitHubAppRoot::from_values(Some("12a"), Some("7"), Some(&pem)).unwrap_err();
    assert!(
        e.contains("github-app.id") && e.contains("ASCII digits"),
        "{e}"
    );
    let e = GitHubAppRoot::from_values(Some("12"), Some("../7"), Some(&pem)).unwrap_err();
    assert!(e.contains("github-app.installation-id"), "{e}");
    // A PEM-armoured blob that is not a key. The armour label is joined at
    // run time so the tree never carries a private-key header (no-secrets).
    let label = ["RSA", "PRIVATE", "KEY"].join(" ");
    let not_a_key = format!("-----BEGIN {label}-----\nc2VjcmV0LXNoYXBlZA==\n-----END {label}-----");
    let e = GitHubAppRoot::from_values(Some("12"), Some("7"), Some(&not_a_key)).unwrap_err();
    assert!(e.contains("github-app.private-key.pem"), "{e}");
    assert!(
        !e.contains("c2VjcmV0"),
        "the refusal never quotes the key: {e}"
    );
    // PKCS#8 is accepted as well as GitHub's PKCS#1.
    use rsa::pkcs8::EncodePrivateKey as _;
    let p8 = github_stub::APP_KEY
        .to_pkcs8_pem(rsa::pkcs8::LineEnding::LF)
        .unwrap()
        .to_string();
    assert!(GitHubAppRoot::from_values(Some("12"), Some("7"), Some(&p8)).is_ok());
    let debug = format!("{:?}", root());
    assert!(
        debug.contains("<redacted>") && !debug.contains("PRIVATE"),
        "{debug}"
    );
}

// ----- the rotation (packet) door -----

#[tokio::test]
async fn a_rotation_mints_verifies_revokes_the_held_value_installs_and_records_each_phase() {
    let gh = github_stub::serve(&[REPO]).await;
    let old = a_live_token(&gh).await;
    let api = jobs(PENDING).await;
    let secrets = Arc::new(FakeSecrets::default());
    secrets.seed(KEY, &old);
    secrets.seed("token.expires-at", &in_minutes(50));
    secrets.seed("token.minted-for", MINTED_BY_REFRESH);
    secrets.watch(&gh, &old);
    let h = CredentialRotateGitHubApp::new(api.url.clone(), issuer(&gh), secrets.clone());

    h.invoke(
        &args(&[]),
        &scope_ctx(json!({"credential": CREDENTIAL, "old_token_last_eight": last_eight(&old)})),
    )
    .await
    .expect("rotation");

    let new = secrets.get(KEY).unwrap();
    assert_ne!(new, old);
    let at_write = secrets.held_live_at_write.lock().unwrap().clone();
    assert!(
        !at_write.is_empty() && at_write.iter().all(|live| !live),
        "the held value was already dead at GitHub when the Secret was overwritten — after \
         the write the broker could no longer present it to end it: {at_write:?}"
    );
    assert_eq!(secrets.get("token.minted-for").as_deref(), Some(JOB));
    {
        let s = gh.state.lock().unwrap();
        assert!(s.live.contains(&new), "the new value lives");
        assert!(
            s.revoked.contains(&old),
            "the held value was ended at GitHub"
        );
        assert!(!s.live.contains(&old));
    }
    assert_eq!(
        api.phases(),
        vec![
            format!("{CREDENTIAL}/minted"),
            format!("{CREDENTIAL}/verified"),
            format!("{CREDENTIAL}/revoked"),
            format!("{CREDENTIAL}/installed"),
        ],
        "each phase recorded as it became true — the revoke BEFORE the write"
    );
    for slug in ["issue", "install", "verify", "revoke"] {
        assert_eq!(api.status(slug), "completed", "{slug}");
    }
    assert_eq!(
        api.status("delivered"),
        "pending",
        "the revoke never waits on a host"
    );
    let flat = api.everything_written();
    assert!(
        !flat.contains(&new) && !flat.contains(&old),
        "a value reached the record"
    );
    assert!(flat.contains(DELIVERY_NOT_AWAITED), "{flat}");
    assert!(
        flat.contains(&format!("ending in {}", last_eight(&old))),
        "{flat}"
    );
    first_mint_probe_reads(&api);
}

/// The car's recorded probe finds the first real mint by these words on
/// the packet's own steps — `installed` naming token.minted-for, `verified`
/// saying the read answered 200 — so the handler is held to writing them
/// on BOTH paths (a fresh firing and a converging redelivery).
fn first_mint_probe_reads(api: &Jobs) {
    let writes = api.writes.lock().unwrap();
    let field = |step: &str, key: &str| {
        writes
            .iter()
            .filter(|(sid, _)| sid == &format!("step-{step}/metadata"))
            .find_map(|(_, body)| body[key].as_str().map(str::to_string))
            .unwrap_or_else(|| panic!("no {key} recorded on {step}"))
    };
    assert!(
        field("install", "installed").contains("token.minted-for"),
        "{}",
        field("install", "installed")
    );
    assert!(
        field("verify", "verified").contains("answered 200"),
        "{}",
        field("verify", "verified")
    );
}

/// Adversarial review of 58b00a77 (MEDIUM): a firing that dies between
/// the revoke and the install left the revoked token under its old, LIVE
/// expiry — so the refresh read it as fresh for up to twenty minutes and
/// the forge render copied a dead token to the push. The expiry is marked
/// past BEFORE the revoke, so after that death every reader sees "not
/// live": the next refresh mints, and the render removes the file.
#[tokio::test]
async fn a_death_between_the_revoke_and_the_install_leaves_the_secret_reading_not_live() {
    let gh = github_stub::serve(&[REPO]).await;
    let old = a_live_token(&gh).await;
    let api = jobs(PENDING).await;
    let secrets = Arc::new(FakeSecrets::default());
    secrets.seed(KEY, &old);
    secrets.seed("token.expires-at", &in_minutes(50));
    *secrets.dies_writing.lock().unwrap() = Some(KEY.to_string());
    let h = CredentialRotateGitHubApp::new(api.url.clone(), issuer(&gh), secrets.clone());

    h.invoke(&args(&[]), &scope_ctx(json!({})))
        .await
        .expect_err("the install write dies");

    assert!(
        gh.state.lock().unwrap().revoked.contains(&old),
        "the fixture reaches the window: the held token was revoked"
    );
    assert_eq!(
        secrets.get(KEY).as_deref(),
        Some(old.as_str()),
        "the install never happened"
    );
    assert!(
        matches!(
            plan_refresh(
                secrets.get(KEY).as_deref(),
                secrets.get("token.expires-at").as_deref(),
                chrono::Utc::now(),
                40,
            ),
            RefreshPlan::Mint { .. }
        ),
        "the dead token must not read as fresh: expiry {:?}",
        secrets.get("token.expires-at")
    );
}

#[tokio::test]
async fn a_finished_rotation_redelivered_mints_nothing() {
    let gh = github_stub::serve(&[REPO]).await;
    let api = jobs(&[
        ("scope", "completed"),
        ("issue", "completed"),
        ("install", "completed"),
        ("verify", "completed"),
        ("revoke", "completed"),
    ])
    .await;
    let secrets = Arc::new(FakeSecrets::default());
    let h = CredentialRotateGitHubApp::new(api.url.clone(), issuer(&gh), secrets.clone());
    h.invoke(&args(&[]), &scope_ctx(json!({})))
        .await
        .expect("no-op");
    assert_eq!(gh.minted(), 0);
    assert_eq!(secrets.writes(), 0);
    assert!(api.phases().is_empty());
}

/// A firing that died after the Secret write: the Secret names this
/// packet, so the redelivery mints nothing and records what is left.
#[tokio::test]
async fn a_redelivery_after_the_write_converges_without_minting() {
    let gh = github_stub::serve(&[REPO]).await;
    let mine = a_live_token(&gh).await;
    let api = jobs(PENDING).await;
    let secrets = Arc::new(FakeSecrets::default());
    secrets.seed(KEY, &mine);
    secrets.seed("token.expires-at", &in_minutes(58));
    secrets.seed("token.minted-for", JOB);
    let h = CredentialRotateGitHubApp::new(api.url.clone(), issuer(&gh), secrets.clone());

    h.invoke(&args(&[]), &scope_ctx(json!({})))
        .await
        .expect("converge");

    assert_eq!(gh.minted(), 1, "only the fixture's own mint");
    assert!(
        gh.state.lock().unwrap().live.contains(&mine),
        "its own token is not revoked"
    );
    assert_eq!(api.phases(), vec![format!("{CREDENTIAL}/installed")]);
    assert_eq!(api.rotations.lock().unwrap()[0].1["converged"], true);
    for slug in ["issue", "install", "verify", "revoke"] {
        assert_eq!(api.status(slug), "completed", "{slug}");
    }
    first_mint_probe_reads(&api);
}

#[tokio::test]
async fn a_scope_naming_a_token_the_broker_cannot_revoke_is_refused_before_the_mint() {
    let gh = github_stub::serve(&[REPO]).await;
    let old = a_live_token(&gh).await;
    for metadata in [
        json!({"old_token": "boss-dr-push"}),
        json!({"old_token_last_eight": "deadbeef"}),
    ] {
        let api = jobs(PENDING).await;
        let secrets = Arc::new(FakeSecrets::default());
        secrets.seed(KEY, &old);
        let h = CredentialRotateGitHubApp::new(api.url.clone(), issuer(&gh), secrets.clone());
        let err = h
            .invoke(&args(&[]), &scope_ctx(metadata.clone()))
            .await
            .expect_err("refused");
        assert!(
            matches!(err, HandlerError::Permanent(_)),
            "{metadata}: {err:?}"
        );
        assert_eq!(secrets.writes(), 0);
        assert!(api.phases().is_empty());
    }
    assert_eq!(gh.minted(), 1, "only the fixture's own mint");
}

/// The App is the one root; an installation is per organisation. A rule
/// row that names `installation_id` mints for THAT installation — a
/// customer's org later — with no code and no new root key; one that
/// names none mints for the root's own (algedonic-dev).
#[tokio::test]
async fn a_declaration_naming_another_installation_mints_for_that_installation() {
    const OTHER: u64 = 8_000_456;
    let gh = github_stub::serve(&[REPO]).await;
    let api = jobs(&[]).await;
    let secrets = Arc::new(FakeSecrets::default());
    let h = CredentialRotateGitHubApp::new(api.url.clone(), issuer(&gh), secrets.clone());
    let mut a = refresh_args();
    a.push(("installation_id".into(), Value::String(OTHER.to_string())));

    // An installation the App does not have: GitHub answers 404, nothing
    // is minted or written.
    let err = h
        .invoke(&a, &tick_ctx())
        .await
        .expect_err("unknown installation");
    assert!(err.to_string().contains("404"), "{err}");
    assert_eq!(secrets.writes(), 0);

    // Installed on that org: the exchange goes to that installation.
    gh.state.lock().unwrap().other_installations.push(OTHER);
    h.invoke(&a, &tick_ctx())
        .await
        .expect("minted for the other org");
    assert!(
        gh.state
            .lock()
            .unwrap()
            .requests
            .contains(&format!("POST /app/installations/{OTHER}/access_tokens")),
        "{:?}",
        gh.state.lock().unwrap().requests
    );
    assert_eq!(
        api.rotations.lock().unwrap()[0].1["installation"],
        OTHER.to_string()
    );

    // And one that names none mints for the root's own.
    let api = jobs(&[]).await;
    let secrets = Arc::new(FakeSecrets::default());
    let h = CredentialRotateGitHubApp::new(api.url.clone(), issuer(&gh), secrets.clone());
    h.invoke(&refresh_args(), &tick_ctx())
        .await
        .expect("default");
    assert_eq!(
        api.rotations.lock().unwrap()[0].1["installation"],
        INSTALLATION_ID.to_string()
    );

    // An installation id that is not digits never reaches a URL.
    let mut bad = refresh_args();
    bad.push(("installation_id".into(), Value::String("1/../2".into())));
    let before = gh.state.lock().unwrap().requests.len();
    let err = h.invoke(&bad, &tick_ctx()).await.expect_err("not an id");
    assert!(matches!(err, HandlerError::Permanent(_)), "{err:?}");
    assert_eq!(gh.state.lock().unwrap().requests.len(), before);
}

/// Every phase lands through the registry's rotation door, which 404s an
/// undeclared id — so a credential with no registry row mints nothing
/// on either door, and the refusal names the row to declare.
#[tokio::test]
async fn a_credential_with_no_registry_row_mints_nothing() {
    let gh = github_stub::serve(&[REPO]).await;
    let undeclared = |mut a: Vec<(String, Value)>| {
        for (k, v) in a.iter_mut() {
            if k == "credential_id" {
                *v = Value::String("github-undeclared-token".into());
            }
        }
        a
    };
    for (a, ctx) in [
        (undeclared(refresh_args()), tick_ctx()),
        (undeclared(args(&[])), scope_ctx(json!({}))),
    ] {
        let api = jobs(PENDING).await;
        let secrets = Arc::new(FakeSecrets::default());
        let h = CredentialRotateGitHubApp::new(api.url.clone(), issuer(&gh), secrets.clone());
        let err = h.invoke(&a, &ctx).await.expect_err("no row");
        assert!(matches!(err, HandlerError::Permanent(_)), "{err:?}");
        assert!(err.to_string().contains("github-undeclared-token"), "{err}");
        assert_eq!(secrets.writes(), 0);
        assert!(api.phases().is_empty());
    }
    assert_eq!(
        gh.minted(),
        0,
        "nothing minted without a row to record it on"
    );
}

#[tokio::test]
async fn a_clock_firing_that_does_not_say_refresh_is_refused() {
    let gh = github_stub::serve(&[REPO]).await;
    let api = jobs(&[]).await;
    let secrets = Arc::new(FakeSecrets::default());
    let h = CredentialRotateGitHubApp::new(api.url.clone(), issuer(&gh), secrets.clone());
    let err = h
        .invoke(&args(&[]), &tick_ctx())
        .await
        .expect_err("no phase");
    assert!(matches!(err, HandlerError::Permanent(_)), "{err:?}");
    let err = h
        .invoke(&args(&[("phase", "revoke")]), &tick_ctx())
        .await
        .expect_err("not a phase this handler runs");
    assert!(matches!(err, HandlerError::Permanent(_)), "{err:?}");
    let err = h
        .invoke(&args(&[("phase", "refresh")]), &tick_ctx())
        .await
        .expect_err("a refresh with no window");
    assert!(err.to_string().contains("refresh_within_minutes"), "{err}");
    assert_eq!(gh.minted(), 0);
}

#[tokio::test]
async fn a_verify_repo_that_is_not_owner_slash_name_reaches_no_url() {
    let gh = github_stub::serve(&[REPO]).await;
    let api = jobs(&[]).await;
    let secrets = Arc::new(FakeSecrets::default());
    let h = CredentialRotateGitHubApp::new(api.url.clone(), issuer(&gh), secrets.clone());
    let mut a = refresh_args();
    for (k, v) in a.iter_mut() {
        if k == "verify_repo" {
            *v = Value::String("algedonic-dev/../../app/installations".into());
        }
    }
    let err = h.invoke(&a, &tick_ctx()).await.expect_err("refused");
    assert!(matches!(err, HandlerError::Permanent(_)), "{err:?}");
    assert!(gh.state.lock().unwrap().requests.is_empty());
}

// ----- keyed per ops-request (backlog 4ce4ec55) -----
//
// One key per owner stranded the second of two approved GitHub writes:
// approval 2 found approval 1's token fresh and minted nothing, write 1
// revoked it, and write 2 had nothing to act with. The per-act rules name
// the request they fire for (`request_id`), and the refresh reads and
// writes `<secret_key>-<request id>` — that request's token alone.

const R1: &str = "11111111-1111-4111-8111-111111111111";
const R2: &str = "22222222-2222-4222-8222-222222222222";

fn request_args(request: &str) -> Vec<(String, Value)> {
    args(&[
        ("phase", "refresh"),
        ("refresh_within_minutes", "40"),
        ("request_id", request),
    ])
}

#[tokio::test]
async fn a_request_keyed_refresh_mints_into_its_own_key_beside_a_live_sibling() {
    let gh = github_stub::serve(&[REPO]).await;
    let api = jobs(&[]).await;
    let secrets = Arc::new(FakeSecrets::default());
    let sibling = a_live_token(&gh).await;
    let sibling_expires = in_minutes(55);
    secrets.seed(&format!("token-{R1}"), &sibling);
    secrets.seed(&format!("token-{R1}.expires-at"), &sibling_expires);
    let h = CredentialRotateGitHubApp::new(api.url.clone(), issuer(&gh), secrets.clone());

    h.invoke(&request_args(R2), &tick_ctx())
        .await
        .expect("refresh");

    let own = secrets
        .get(&format!("token-{R2}"))
        .expect("request 2 holds a token of its own, though request 1's is fresh");
    assert_ne!(own, sibling);
    assert!(gh.state.lock().unwrap().live.contains(&own));
    assert_eq!(
        secrets.get(&format!("token-{R2}.minted-for")).as_deref(),
        Some(R2),
        "the key's provenance names the request it was minted for"
    );
    assert!(secrets.get(&format!("token-{R2}.expires-at")).is_some());
    assert_eq!(
        (
            secrets.get(&format!("token-{R1}")),
            secrets.get(&format!("token-{R1}.expires-at"))
        ),
        (Some(sibling.clone()), Some(sibling_expires)),
        "request 1's token and expiry are left exactly as they were"
    );
    assert!(
        gh.state.lock().unwrap().live.contains(&sibling),
        "request 1's token still lives"
    );
    assert_eq!(secrets.get(KEY), None, "the shared key is never written");

    let ev = api.rotations.lock().unwrap()[0].1.clone();
    assert_eq!(ev["secret_key"], format!("token-{R2}"));
    assert_eq!(ev["request_id"], R2);
    assert!(!api.everything_written().contains(&own));

    // The request's approval, fired while its own token is fresh: nothing.
    let writes = secrets.writes();
    h.invoke(&request_args(R2), &tick_ctx())
        .await
        .expect("a fresh token is not an error");
    assert_eq!(secrets.writes(), writes, "a fresh own token is left alone");
}

#[tokio::test]
async fn a_request_id_that_is_not_a_packet_id_is_refused_before_anything_is_read() {
    let gh = github_stub::serve(&[REPO]).await;
    let api = jobs(&[]).await;
    let secrets = Arc::new(FakeSecrets::default());
    let h = CredentialRotateGitHubApp::new(api.url.clone(), issuer(&gh), secrets.clone());
    for bad in [
        "../token",
        "R2",
        "11111111-1111-4111-8111-11111111111z",
        "11111111-1111-4111-8111-11111111111",
        "11111111111141118111111111111111",
        "AAAAAAAA-1111-4111-8111-111111111111",
    ] {
        let err = h
            .invoke(&request_args(bad), &tick_ctx())
            .await
            .expect_err(bad);
        assert!(matches!(err, HandlerError::Permanent(_)), "{bad}: {err:?}");
        assert!(err.to_string().contains("request_id"), "{bad}: {err}");
    }
    assert_eq!(gh.minted(), 0);
    assert_eq!(secrets.writes(), 0);

    // The packet door keys by nothing but its declaration: a rotation
    // naming a request is an authoring fault, refused before any read.
    let err = h
        .invoke(&args(&[("request_id", R1)]), &scope_ctx(json!({})))
        .await
        .expect_err("a rotation naming a request");
    assert!(matches!(err, HandlerError::Permanent(_)), "{err:?}");
    assert!(err.to_string().contains("request_id"), "{err}");
    assert_eq!(gh.minted(), 0);
}
