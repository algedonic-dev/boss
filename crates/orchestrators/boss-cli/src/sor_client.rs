//! THE SYSTEM-OF-RECORD CLIENT every boss verb reaches the jobs API
//! through: which instance (`resolve_jobs_base`, no default), which
//! client (`machine_client`, the estate machine token), who signs
//! (`api_at_signed` sets `x-boss-user`), what a 2xx means
//! (`success_answer`), and the shared car reads (`all_cars_at`,
//! `all_open_cars`) and completion stamp (`stamp`).
//!
//! WHY ITS OWN MODULE (backlog 76b846cd F3, review of b089661d). These
//! lived in `gate.rs`, and `gate` is a hub the review-hold judge
//! (`mutating_verb.rs`) follows by no rule, because nearly every module
//! names it. So `boss prove --unattended` and `boss workflow publish` —
//! two subcommands an ops verb runs — reached the system of record
//! through code no car was held for: a car changing the signer or the
//! base URL here changed what both verbs do and boarded unreviewed.
//! Here, one module named by the verbs that call it, the judge follows
//! it like any other, and `gate` stays a hub. The moved code is
//! unchanged.

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;

use crate::identity;

/// The message a caller gets when `BOSS_JOBS_URL` is unset. Kept apart
/// from the lookup so a test can assert what it teaches without
/// touching process environment.
pub(crate) fn no_instance_message() -> String {
    "BOSS_JOBS_URL is not set, and this verb has no default on purpose.\n\
     Read this deployment's system-of-record URL from infra/dev/sor-url.\n\
     Set BOSS_JOBS_URL to that declared URL before running this verb.\n\
     A wrong deployment can answer with an empty queue instead of an error;\n\
     verify a known packet on the same connection before accepting absence."
        .to_string()
}

/// The jobs API this verb talks to. **NO DEFAULT, deliberately.**
///
/// It used to fall back to `http://127.0.0.1:7900`. On boss-gcp that
/// reached a second, older, complete BOSS stack (since retired), not
/// the system of record. A wrong instance does not fail, it answers. On
/// 2026-08-27 a `?kind=gate-run` read returned `total: 0` from the local
/// stack while the cluster held 51 packets; a `gate-run v1` spec was
/// then authored against that zero, which would have regressed the live
/// v2 to a worse 3-step v1 on the next bundle reconcile. It was caught
/// by noticing two packet counts disagreed — luck, not process.
///
/// The fix is not a better default. A default that is right on one host
/// and silently wrong on another IS the defect (packet aa783636), so a
/// verb that cannot reach the right instance now reaches none.
pub(crate) fn jobs_base() -> Result<String> {
    resolve_jobs_base(None)
}

/// Resolve the jobs-api base from an explicit `--jobs-url` flag, falling
/// back to `BOSS_JOBS_URL`, and REFUSING (never defaulting) when neither
/// is set. Every read verb — `gate`, `packet census`, `queue`, `prove` —
/// resolves through here so not one of them can quietly re-grow the
/// `http://127.0.0.1:7900` default that read boss-gcp's second, older
/// stack (packet aa783636). A default that is right on one host and
/// silently wrong on another IS the defect; a verb that cannot reach the
/// right instance now reaches none.
pub(crate) fn resolve_jobs_base(flag: Option<&str>) -> Result<String> {
    resolve_jobs_base_from(flag, std::env::var("BOSS_JOBS_URL").ok())
}

/// The pure core of [`resolve_jobs_base`]: flag wins, then env, then
/// refuse. Split out so a test can pin the precedence and the refusal
/// without mutating process environment — env writes are `unsafe` under
/// edition 2024, and racy across the parallel test runner.
pub(crate) fn resolve_jobs_base_from(flag: Option<&str>, env: Option<String>) -> Result<String> {
    flag.map(str::to_string)
        .filter(|v| !v.trim().is_empty())
        .or_else(|| env.filter(|v| !v.trim().is_empty()))
        .ok_or_else(|| anyhow!("{}", no_instance_message()))
}

/// The client every boss verb's BOSS-service calls go through: a
/// guarded CLI `MachineClient`, which requires the estate machine token on each
/// final request from the process's one watched source and follows no redirect
/// (design 6805c764 car 2, the CLI slice, 2026-09-29). Until then each
/// verb built a plain `reqwest::Client` and sent `x-boss-user` alone, so
/// every verb would have been a 401 the minute its port's gate enforced.
/// A third party — the forge, GitHub — is never reached through one: it
/// has its own plain client behind its port (`train::forge`), and so do
/// the install checks `boss doctor` makes of NATS and the local gateway's
/// health, which assert no identity.
pub(crate) fn machine_client() -> Result<crate::machine_http::MachineClient> {
    machine_client_with(reqwest::Client::builder())
}

/// [`machine_client`] from a builder carrying the caller's own settings
/// (a timeout); the redirect policy is the machine client's, whatever
/// the builder asked for.
///
/// UNDER `cfg(test)` IT HOLDS NO TOKEN (backlog 2ee29275, F2). 63 test
/// sites build their client here, and a failing test prints the request
/// head it captured — so a test client reading the process's shared
/// source would put a mounted Secret into the gate's log. The switch is
/// here, not at the 63 sites, so no test can forget it; the pin
/// `no_test_reads_the_live_machine_token` refuses a test that names the
/// shared source itself.
pub(crate) fn machine_client_with(
    builder: reqwest::ClientBuilder,
) -> Result<crate::machine_http::MachineClient> {
    #[cfg(not(test))]
    let client = crate::machine_http::MachineClient::build(builder);
    #[cfg(test)]
    let client = crate::machine_http::MachineClient::unstamped(builder);
    client.context("building the machine client")
}

pub(crate) async fn api(
    http: &crate::machine_http::MachineClient,
    method: reqwest::Method,
    path: &str,
    payload: Option<Value>,
) -> Result<Option<Value>> {
    api_at(http, &jobs_base()?, method, path, payload).await
}

/// Like [`api`], but against an explicit base rather than the resolved
/// one. The seam a paginating reader tests through: a stub socket can
/// answer without a `BOSS_JOBS_URL` anywhere in the environment.
///
/// EVERY operator verb — `gate`, `prove`, `park`, `job`, `orient`,
/// `rerail`, `docs` — writes through here, which is why this is where
/// the caller gets signed. A write carries the actor RUNNING the
/// command or is refused; a read carries them if known and says so
/// once if not. Neither is ever signed as the conductor: that is
/// backlog 5083d6f5, measured on the step a `boss prove` completed.
pub(crate) async fn api_at(
    http: &crate::machine_http::MachineClient,
    base: &str,
    method: reqwest::Method,
    path: &str,
    payload: Option<Value>,
) -> Result<Option<Value>> {
    let signature = identity::signature_for(&method, path, identity::caller());
    api_at_signed(http, base, method, path, payload, signature).await
}

/// What a signed door's errors call the service `base` reaches (backlog
/// 25a4f7f9, 2026-09-28): `<name> api` for a port the one port table
/// (`boss_ports`) holds, marked `(scratch)` on a scratch port, and the
/// base itself for any other — a gateway hostname, a test's stub — so
/// no error names a service it cannot show it reached. Until then every
/// error said `jobs api`, including an events-service refusal to `boss
/// events redeliver` and a ledger one to `boss ledger lock`, which is a
/// diagnosis sent to the wrong service.
pub(crate) fn service_at(base: &str) -> String {
    let base = base.trim_end_matches('/');
    let rest = base.split_once("://").map_or(base, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    // After the last `:` — which, in a bare `[::1]`, is inside the
    // brackets and does not parse as a port.
    let port = authority
        .rsplit_once(':')
        .and_then(|(_, p)| p.parse::<u16>().ok());
    let named = port.and_then(|p| {
        boss_ports::all().find_map(|s| {
            if s.prod == p {
                Some(format!("{} api", s.name))
            } else if s.scratch == Some(p) {
                Some(format!("{} api (scratch)", s.name))
            } else {
                None
            }
        })
    });
    named.unwrap_or_else(|| format!("the service at {base}"))
}

/// [`api_at`] with the signing decision already made. The seam the
/// wire tests go through: WHO signs is a pure function of the
/// environment (`identity::signature_for`), and this is the half that
/// puts it on the socket — so a test can prove the header carries the
/// caller, and that a refused write never reaches the network, without
/// mutating the process's environment.
pub(crate) async fn api_at_signed(
    http: &crate::machine_http::MachineClient,
    base: &str,
    method: reqwest::Method,
    path: &str,
    payload: Option<Value>,
    signature: identity::Signature,
) -> Result<Option<Value>> {
    let signer = identity::apply(signature)?;
    let user = identity::header(&signer);
    let url = format!("{base}{path}");
    let service = service_at(base);
    // A refused connect is waited out — the stack rolls with Recreate
    // and is dark for about a minute per converge (backlog 034002b3);
    // anything past the connect is surfaced as it always was.
    let resp = crate::train::send_through_a_roll(&format!("{service} {method} {path}"), || {
        let req = http
            .request(method.clone(), &url)
            .header("x-boss-user", user.as_str())
            .header("content-type", "application/json");
        match &payload {
            Some(p) => req.json(p),
            None => req,
        }
    })
    .await?;
    let status = resp.status();
    if status.is_redirection() {
        // The client follows no redirect, so a 3xx lands here (backlog
        // 2ee29275, F4). Name where it pointed by scheme and host only:
        // a login redirect carries its state in the query, and the page
        // body echoes the whole target, so neither is quoted.
        let location = resp
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok());
        let target = redirect_target(location)
            .unwrap_or_else(|| "a target it did not name as an absolute URL".into());
        bail!(
            "{service} {method} {path} -> {status}: refused to follow a redirect to {target} \
             (the machine client follows none; is the base pointed at an edge or another \
             instance?)"
        );
    }
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!(
            "{service} {method} {path} -> {status}: {}{}",
            body.trim(),
            ways_out(&body)
        );
    }
    success_answer(&service, &method, path, status, &body)
}

/// The ways out a refusal names, one to a line under the body.
///
/// A refusal that lists `ways_out` is one the reader can only act on by
/// reading them — the jobs API's answer to a `human_only` step completed
/// without a passkey, which is what `boss step complete` meets on such a
/// step since 2026-10-07 (item 570c66e9): no CLI carries a passkey. They
/// sat inside one line of JSON, after the detail; they are printed again
/// where a person at a terminal will see them. Empty for every other
/// body, so no other refusal reads differently.
fn ways_out(body: &str) -> String {
    let Ok(parsed) = serde_json::from_str::<Value>(body) else {
        return String::new();
    };
    let ways: Vec<&str> = parsed
        .get("ways_out")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if ways.is_empty() {
        return String::new();
    }
    let head = if parsed.get("human_only") == Some(&Value::Bool(true)) {
        "this step is human_only: it completes only on a passkey, which no CLI carries"
    } else {
        "the refusal names its ways out"
    };
    let mut text = format!("\n\n{head}. Ways out:");
    for (i, way) in ways.iter().enumerate() {
        text.push_str(&format!("\n  {}. {way}", i + 1));
    }
    text
}

/// A redirect's target as a refusal may name it: `scheme://host[:port]`,
/// never its path, query or fragment. `None` for a relative or
/// unparseable `Location`, which is then not echoed at all.
pub(crate) fn redirect_target(location: Option<&str>) -> Option<String> {
    let url = reqwest::Url::parse(location?.trim()).ok()?;
    let host = url.host_str()?;
    Some(match url.port() {
        Some(port) => format!("{}://{host}:{port}", url.scheme()),
        None => format!("{}://{host}", url.scheme()),
    })
}

/// What a 2xx body answers — pure, so each rule is pinned without a
/// socket.
///
/// A READ keeps its old contract: a body that is not JSON is `None`,
/// and the reader decides — `train::rows` refuses it for a list, and a
/// single-object read's `context` names it (backlog 7b7e0529).
///
/// A WRITE's success is its parsed answer, not its status (backlog
/// 10776b6c). It used to get the same `None`, and a `None` is also what
/// a 204 gives, so a write that met a proxy's login page — or anything
/// else answering 200 in front of the jobs API — read as done while
/// the write may never have reached the API: a silent false success,
/// the worst shape this class takes. Which empty answers are real was
/// MEASURED, not assumed: every jobs-API write handler answers either
/// JSON or `204 No Content` (the job PUT, both metadata PATCHes, the
/// step PUT, the registry DELETEs and the scheduling/sensor/station
/// writes), so on every write method a 204 is a success and nothing
/// else empty is. HEAD is a read. `service` is what the refusal calls
/// the door it reached ([`service_at`], backlog 25a4f7f9).
pub(crate) fn success_answer(
    service: &str,
    method: &reqwest::Method,
    path: &str,
    status: reqwest::StatusCode,
    body: &str,
) -> Result<Option<Value>> {
    let write = !matches!(*method, reqwest::Method::GET | reqwest::Method::HEAD);
    let trimmed = body.trim();
    if trimmed.is_empty() {
        if !write || status == reqwest::StatusCode::NO_CONTENT {
            return Ok(None);
        }
        bail!(
            "{service} {method} {path} -> {status} with an empty body: a write's success is \
             its answer, and only a 204 answers with nothing — the write may never have \
             reached {service}"
        );
    }
    match serde_json::from_str(trimmed) {
        Ok(v) => Ok(Some(v)),
        Err(_) if !write => Ok(None),
        Err(_) => {
            let cut: String = trimmed
                .chars()
                .take(crate::train::ROWS_REFUSAL_QUOTE)
                .collect();
            let more = if cut.len() < trimmed.len() { "…" } else { "" };
            bail!(
                "{service} {method} {path} -> {status}, but the body is not JSON, so the write \
                 may never have reached {service} (a proxy's login page answers this way); \
                 it answered: {cut}{more}"
            )
        }
    }
}

/// Every open `ship-a-change` car, across ALL pages — the operator-verb
/// counterpart to the conductor's `train::list_all_pages`, built on the
/// `crate::sor_client::api` client the CLI verbs speak through (a different client
/// from the conductor's, which is why the boarding-pagination car could
/// not fold these call sites into that helper directly).
///
/// A limit is not a filter (memory: a-limit-is-not-a-filter). Open cars
/// build past a page — in-flight + parked + landed-but-unclosed residue —
/// and the list is `ORDER BY opened_on DESC`, so a car opened days ago
/// but touched today sorts to the tail. Read with a bare `limit=`, that
/// car falls off page one and `boss receipt`/`rerail`/`channels` reported
/// a car that exists as "not found" — a false negative that grows as
/// closed cars accumulate. This pages on the response `total` via
/// [`train::list_all_pages`] (whose page-two behaviour is pinned there)
/// until every matching row is read.
///
/// `full=true`: its readers take the gate step's `receipt` and the
/// review step's `hold` off these rows (`receipt::select_receipt`,
/// `car_retire`), and the list's default is going slim — step metadata
/// only when asked (backlog 9b473d4a).
pub(crate) async fn all_open_cars(http: &crate::machine_http::MachineClient) -> Result<Vec<Value>> {
    crate::train::list_all_pages(|offset| async move {
        api(
            http,
            reqwest::Method::GET,
            &format!(
                "/api/jobs?kind=ship-a-change&status=open&full=true&limit={}&offset={offset}",
                crate::train::PAGE_LIMIT
            ),
            None,
        )
        .await
    })
    .await
}

/// EVERY `ship-a-change` car, open and closed, across all pages — for
/// the readers that must see landed cars too: `boss prove` (its
/// `--recheck` proves a CLOSED car) and `boss orient` (the shed is
/// landed cars not yet proven; the stranded cross-ref needs every car's
/// branch).
///
/// Paged on `total` through [`train::list_all_pages`], which REFUSES a
/// page without one (backlog 10776b6c). Before this, `boss prove` read
/// a missing total as 0 — one page then "covered" the whole population
/// — and `boss orient` read one bare `limit=800` page and never compared
/// it to `total`, so past 800 cars the oldest landed ones would have
/// dropped out of the shed silently (417 cars on 2026-09-23). A page is
/// 500 so today's population is still one call — the query and the
/// page are [`all_cars_via`]'s, the one car read every verb shares.
pub(crate) async fn all_cars_at(
    http: &crate::machine_http::MachineClient,
    base: &str,
) -> Result<Vec<Value>> {
    all_cars_via(|path| async move { api_at(http, base, reqwest::Method::GET, &path, None).await })
        .await
}

/// The one car query, paged on `total`, over whatever transport the
/// caller reads through: `fetch` is handed each page's path and returns
/// its body. [`all_cars_at`] hands it the signed jobs-API read; the
/// census hands it its own client, which counts every call it reports
/// as its cost — and until backlog 6cf47547 read one bare `limit=800`
/// page it never compared to `total`. `full=true`: `boss prove` reads
/// the proven step's `proof` and `fields`, and orient the review step's
/// `hold`, off these rows (backlog 9b473d4a).
pub(crate) async fn all_cars_via<F, Fut>(fetch: F) -> Result<Vec<Value>>
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Result<Option<Value>>>,
{
    const PAGE: usize = 500;
    crate::train::list_all_pages(|offset| {
        fetch(format!(
            "/api/jobs?kind=ship-a-change&full=true&limit={PAGE}&offset={offset}"
        ))
    })
    .await
}

/// [`all_cars_at`] against the resolved system of record.
pub(crate) async fn all_cars(http: &crate::machine_http::MachineClient) -> Result<Vec<Value>> {
    all_cars_at(http, &jobs_base()?).await
}

/// The instant a step completed, in the one format every verb writes.
///
/// RFC3339, whole seconds, `Z`. Several verbs stamp `completed_at` — the
/// conductor on the steps it completes, `boss park` on scope/build/gate,
/// `boss prove` on proven, and now the dispatcher's auto-park handler —
/// and cycle time is the difference between stamps written by
/// *different* verbs. A format that varied by writer would still look
/// right in every packet and only be wrong in the arithmetic, so it is
/// defined ONCE, in `boss_jobs::car`, and everyone delegates here.
pub(crate) fn stamp(now: chrono::DateTime<chrono::Utc>) -> String {
    boss_jobs::car::stamp(now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// What the operator meets first on a human-only step: the jobs
    /// API's refusal, with its two ways out printed one to a line.
    #[test]
    fn a_human_only_refusal_prints_its_ways_out_under_the_body() {
        let body = json!({
            "error": "step requires stronger assurance than this request carries",
            "required": "presence",
            "human_only": true,
            "ways_out": ["complete it from a surface", "give the step a sign-off role"],
        })
        .to_string();
        let said = ways_out(&body);
        assert!(said.contains("human_only"), "{said}");
        assert!(said.contains("passkey"), "{said}");
        assert!(said.contains("\n  1. complete it from a surface"), "{said}");
        assert!(
            said.contains("\n  2. give the step a sign-off role"),
            "{said}"
        );
        // Every other refusal reads exactly as it did.
        assert_eq!(ways_out(r#"{"error":"sign-offs incomplete"}"#), "");
        assert_eq!(ways_out("step not found"), "");
        assert_eq!(ways_out(r#"{"ways_out":[]}"#), "");
    }

    /// THE DEFAULT THAT WAS RIGHT ON ONE HOST AND SILENTLY WRONG ON THE
    /// OTHER (packet aa783636).
    ///
    /// A refusal is only useful if it says WHICH instance to reach. The
    /// old fallback pointed at boss-gcp's second, older stack, and a read
    /// against it does not fail — it answers `total: 0` while the system
    /// of record holds 51 packets. So this pins the two things the
    /// message has to carry: the declaration of the system of record,
    /// and the same-connection control that makes an empty answer usable.
    /// The second stack has since retired; an adopter's address is not
    /// this instance's (26f5080a, approved design c6f08b60 D3).
    #[test]
    fn refusing_without_an_instance_names_the_system_of_record() {
        let m = no_instance_message();
        assert!(
            m.contains("infra/dev/sor-url"),
            "the refusal must name the system of record, not just complain: {m}"
        );
        assert!(
            !m.contains("http://"),
            "the refusal must not prescribe an instance address: {m}"
        );
        assert!(
            m.contains("BOSS_JOBS_URL"),
            "it must name the variable to set: {m}"
        );
        assert!(
            m.contains("known packet") && m.contains("same connection"),
            "the refusal must explain how to verify an empty answer: {m}"
        );
    }

    /// An empty or whitespace value is not a configured instance. Left
    /// unguarded it would build request URLs like `/api/jobs`, which
    /// reqwest rejects as relative — a confusing failure a long way from
    /// the cause.
    #[test]
    fn an_empty_instance_is_treated_as_unset() {
        // `jobs_base` reads process env, so assert the filter directly on
        // the same predicate rather than mutating a global under test.
        for v in ["", "   ", "\t"] {
            assert!(
                Some(v.to_string())
                    .filter(|s| !s.trim().is_empty())
                    .is_none(),
                "{v:?} must not count as a configured instance"
            );
        }
    }

    /// The census read the car list as one bare `limit=800` page and
    /// never compared it to `total` (backlog 6cf47547), so past 800 cars
    /// its stranded-green note would have named a car's green gate-run
    /// stranded because the car sat on the unread tail. `all_cars_via`
    /// is the one car query, paged on `total`, handed to whatever
    /// transport the caller counts its calls through.
    #[tokio::test]
    async fn all_cars_via_reads_the_car_past_one_page() {
        let asked = std::sync::Mutex::new(Vec::<String>::new());
        let cars = all_cars_via(|path: String| {
            asked.lock().unwrap().push(path.clone());
            async move {
                let row = if path.contains("offset=0") {
                    "car-1"
                } else {
                    "car-2"
                };
                anyhow::Ok(Some(json!({"data": [{"id": row}], "total": 2})))
            }
        })
        .await
        .unwrap();
        assert_eq!(cars.len(), 2, "the car on page two is read");
        let asked = asked.into_inner().unwrap();
        assert_eq!(asked.len(), 2, "{asked:?}");
        assert!(
            asked
                .iter()
                .all(|p| p.starts_with("/api/jobs?kind=ship-a-change&")),
            "{asked:?}"
        );
        assert!(asked[1].ends_with("offset=1"), "{asked:?}");
    }

    /// EVERY read verb resolves its instance through this one function,
    /// so the `127.0.0.1` trap cannot re-grow in any single verb. Pin
    /// the precedence (flag over env) and the refusal on the pure form,
    /// so no test has to mutate process env (unsafe under edition 2024,
    /// racy in parallel). `packet census` and `queue` mirror this from
    /// their own modules to document that they wire it in.
    #[test]
    fn resolve_jobs_base_prefers_flag_then_env_then_refuses() {
        assert_eq!(
            resolve_jobs_base_from(Some("http://flag:7900"), Some("http://env:7900".into()))
                .unwrap(),
            "http://flag:7900",
            "an explicit flag beats the env"
        );
        assert_eq!(
            resolve_jobs_base_from(None, Some("http://env:7900".into())).unwrap(),
            "http://env:7900",
            "the env is used when there is no flag"
        );
        for (flag, env) in [
            (None, None),
            (Some("   "), None),
            (None, Some(String::new())),
            (Some(""), Some("\t".to_string())),
        ] {
            let m = resolve_jobs_base_from(flag, env)
                .expect_err("neither a real flag nor a real env must refuse")
                .to_string();
            assert!(
                m.contains("infra/dev/sor-url"),
                "refusal must name the record: {m}"
            );
            assert!(
                !m.contains("SECOND") && !m.contains("http://"),
                "refusal must not prescribe a retired or private deployment: {m}"
            );
        }
    }
}

/// A one-shot HTTP stub that hands back the request head it read —
/// the seam the wire tests go through, shared by the signing tests
/// (the head is where `x-boss-user` lives) and the re-gate read (the
/// head is where the query lives).
#[cfg(test)]
pub(crate) mod stub {
    pub(crate) async fn one_request(
        body: &'static str,
    ) -> (String, tokio::task::JoinHandle<Option<String>>) {
        one_response("200 OK", body).await
    }

    /// [`one_request`] with the status line chosen — a write's 204, or
    /// a 200 that is not the jobs API's answer (backlog 10776b6c).
    pub(crate) async fn one_response(
        status: &'static str,
        body: &'static str,
    ) -> (String, tokio::task::JoinHandle<Option<String>>) {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.ok()?;
            let request = read_request(&mut sock).await;
            let resp = format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\n\
                 content-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.shutdown().await;
            Some(request)
        });
        (format!("http://{addr}"), handle)
    }

    /// A one-shot `302` to `location`, its body echoing the whole target
    /// the way an HTML redirect page does — the shape Cloudflare Access
    /// answers `boss.algedonic.dev` with (backlog 2ee29275, F4).
    pub(crate) async fn one_redirect(
        location: &'static str,
    ) -> (String, tokio::task::JoinHandle<Option<String>>) {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.ok()?;
            let request = read_request(&mut sock).await;
            let body = format!("<a href=\"{location}\">Found</a>");
            let resp = format!(
                "HTTP/1.1 302 Found\r\nlocation: {location}\r\ncontent-type: text/html\r\n\
                 content-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.shutdown().await;
            Some(request)
        });
        (format!("http://{addr}"), handle)
    }

    /// Read one whole request: the head up to its blank line, then as
    /// many body bytes as its `content-length` names. Answering before
    /// the body is drained closes a socket the client is still writing
    /// to, which it sees as Broken pipe once the body outgrows a socket
    /// buffer (backlog 1fe351e8, the race the builder of cef615f6
    /// measured at 1 in 16 under load). A closed socket ends it with
    /// what arrived, so the caller's assertion names the gap.
    pub(crate) async fn read_request(sock: &mut tokio::net::TcpStream) -> String {
        use tokio::io::AsyncReadExt;
        let mut buf = Vec::new();
        let mut chunk = [0u8; 8192];
        let mut want: Option<usize> = None;
        loop {
            if want.is_none() {
                want = buf
                    .windows(4)
                    .position(|w| w == b"\r\n\r\n")
                    .map(|end| end + 4 + content_length(&buf[..end]));
            }
            if want.is_some_and(|total| buf.len() >= total) {
                break;
            }
            match sock.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        }
        String::from_utf8_lossy(&buf).into_owned()
    }

    /// The `content-length` a request head declares — zero when it
    /// declares none, which is every body-less read.
    fn content_length(head: &[u8]) -> usize {
        String::from_utf8_lossy(head)
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.trim().eq_ignore_ascii_case("content-length"))
            .and_then(|(_, value)| value.trim().parse().ok())
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod signing_tests {
    use super::stub::{one_redirect, one_request, one_response};
    use super::*;
    use crate::identity::Signature;
    use crate::train::rows;
    use serde_json::json;

    /// Backlog 2ee29275, F2: a test's client holds NO token, whatever
    /// the process's token dir holds. A failing test prints the request
    /// head it captured, so a client reading the process's shared
    /// source would put a mounted Secret into the gate's log. Proven
    /// through a wrapper, because the token dir is read from the
    /// environment and a test must not mutate its own: this test's
    /// binary re-run with `BOSS_MACHINE_TOKEN_DIR` naming a dir that
    /// holds a live-looking token sends through both CLI constructors
    /// and prints whether the header left.
    #[tokio::test]
    async fn a_test_client_never_sends_the_process_token() {
        const LIVE: &str = "live-estate-token-must-never-leave-a-test";
        if std::env::var("BOSS_CLI_TOKEN_LEAK_INNER").is_ok() {
            let clients = [
                (
                    "machine_client",
                    crate::sor_client::machine_client().unwrap(),
                ),
                (
                    "machine_client_with",
                    crate::sor_client::machine_client_with(reqwest::Client::builder()).unwrap(),
                ),
            ];
            for (name, http) in clients {
                let (base, stub) = one_request("{}").await;
                api_at_signed(
                    &http,
                    &base,
                    reqwest::Method::GET,
                    "/api/jobs",
                    None,
                    Signature::As("claude@algedonic.dev".into()),
                )
                .await
                .expect("the stub answers 200");
                let head = stub.await.unwrap().expect("the stub read a request");
                let sent = head
                    .to_ascii_lowercase()
                    .contains(boss_core::machine_token::HEADER)
                    || head.contains(LIVE);
                println!("TOKEN-SENT {name}={sent}");
            }
            return;
        }
        let dir = boss_testing::scratch_dir("cli-test-token-leak");
        std::fs::write(dir.join(boss_core::machine_token::CURRENT), LIVE).unwrap();
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "sor_client::signing_tests::a_test_client_never_sends_the_process_token",
                "--nocapture",
            ])
            .env(boss_core::machine_token::TOKEN_DIR_ENV, &dir)
            .env("BOSS_CLI_TOKEN_LEAK_INNER", "1")
            .output()
            .unwrap();
        let printed = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{printed}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        for name in ["machine_client", "machine_client_with"] {
            assert!(
                printed.contains(&format!("TOKEN-SENT {name}=false")),
                "a test's {name} must send no machine token, whatever the token dir holds: \
                 {printed}"
            );
        }
    }

    /// Backlog 2ee29275, F4: the client follows no redirect, so a 3xx
    /// comes back here — and the refusal names WHERE it was sent, by
    /// scheme and host, so an operator pointed at the Access edge reads
    /// that at once. Never the path's query or the body: a login
    /// redirect carries its state in the query, and the page echoes it.
    #[tokio::test]
    async fn a_refused_redirect_names_its_scheme_and_host_never_its_query() {
        let (base, stub) = one_redirect(
            "https://edge.example.com:8443/cdn-cgi/access/login?kid=QUERYSECRET&redirect_url=%2Fapi",
        )
        .await;
        let http = crate::sor_client::machine_client().unwrap();
        let err = api_at_signed(
            &http,
            &base,
            reqwest::Method::GET,
            "/api/jobs",
            None,
            Signature::As("claude@algedonic.dev".into()),
        )
        .await
        .expect_err("a 302 is not an answer");
        stub.await.unwrap().expect("the stub read a request");
        let msg = format!("{err:#}");
        assert!(msg.contains("302"), "{msg}");
        assert!(
            msg.contains("https://edge.example.com:8443"),
            "the refusal must name the redirect's scheme and host: {msg}"
        );
        assert!(
            !msg.contains("QUERYSECRET") && !msg.contains("kid=") && !msg.contains("/cdn-cgi"),
            "the refusal must not carry the target's path, query or the page echoing it: {msg}"
        );
    }

    /// Backlog 2ee29275, F3: no verb panics building its client. A
    /// production `machine_client().unwrap()` turns a TLS-backend or
    /// resolver failure into a panic with no verb named; `?` carries
    /// `building the machine client` to the operator. Test code is
    /// blanked first, where `.unwrap()` is the house style.
    #[test]
    fn no_verb_unwraps_its_machine_client() {
        // This crate's own sources only, so a scoped gate that runs
        // boss-cli runs every file this reads.
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        let mut dirs = vec![src.clone()];
        while let Some(d) = dirs.pop() {
            for e in std::fs::read_dir(&d).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    dirs.push(p);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    files.push(p);
                }
            }
        }
        let unwrap =
            regex::Regex::new(r"machine_client(_with)?\([^;]*?\)\s*\.(unwrap|expect)\(").unwrap();
        let mut found: Vec<String> = files
            .iter()
            .flat_map(|p| {
                let text = std::fs::read_to_string(p).unwrap();
                let prod = boss_testing::production_source::production_text(
                    &boss_testing::production_source::without_comments(&text),
                )
                .unwrap();
                let rel = p.strip_prefix(&src).unwrap().display().to_string();
                prod.lines()
                    .enumerate()
                    .filter(|(_, l)| unwrap.is_match(l))
                    .map(|(i, _)| format!("{rel}:{}", i + 1))
                    .collect::<Vec<_>>()
            })
            .collect();
        found.sort();
        assert!(
            found.is_empty(),
            "a verb unwraps its machine client — use `?`: {found:?}"
        );
    }

    /// The target as the refusal names it: scheme and authority only.
    #[test]
    fn a_redirect_target_is_named_by_scheme_and_host() {
        let named = |l: &str| redirect_target(Some(l));
        assert_eq!(
            named("https://boss.algedonic.dev/cdn-cgi/access/login?x=1#f").as_deref(),
            Some("https://boss.algedonic.dev")
        );
        assert_eq!(
            named("http://10.20.0.34:7900/api?t=s").as_deref(),
            Some("http://10.20.0.34:7900")
        );
        // Userinfo is never echoed, a password in it least of all
        // (backlog 2ee29275, car B's review 83fc0bdf). The literal is
        // split so the pre-flight's secret scan sees no URL password.
        let with_userinfo = concat!(
            "https://operator:",
            "not-a-secret",
            "@elsewhere.example/x?q=1"
        );
        assert_eq!(
            named(with_userinfo).as_deref(),
            Some("https://elsewhere.example")
        );
        assert_eq!(
            named("http://operator@elsewhere.example:8443/").as_deref(),
            Some("http://elsewhere.example:8443")
        );
        // A relative Location names no other host; an unparseable one
        // is not echoed at all.
        assert_eq!(named("/login?next=x").as_deref(), None);
        assert_eq!(named("::not a url::").as_deref(), None);
        assert_eq!(redirect_target(None), None);
    }

    /// Backlog 5083d6f5, at the wire: the step PUT a `boss prove`
    /// makes must arrive carrying the operator who ran it.
    #[tokio::test]
    async fn a_write_arrives_signed_as_its_caller() {
        let (base, stub) = one_request("{}").await;
        let http = crate::sor_client::machine_client().unwrap();
        api_at_signed(
            &http,
            &base,
            reqwest::Method::PUT,
            "/api/jobs/x/steps/y",
            Some(json!({"status": "completed"})),
            Signature::As("claude@algedonic.dev".into()),
        )
        .await
        .expect("the stub answers 200");
        let head = stub.await.unwrap().expect("the stub read a request");
        assert!(
            head.contains(r#""id":"claude@algedonic.dev""#),
            "the write must name its caller; head was:\n{head}"
        );
        assert!(
            !head.contains(crate::identity::CONDUCTOR),
            "an operator's write must not be signed as the train automation; head was:\n{head}"
        );
    }

    /// Backlog 1fe351e8: the stub answered after the request HEAD and
    /// never read the body, so a body larger than one socket buffer met
    /// a closed socket mid-write — Broken pipe instead of the answer.
    /// Small JSON bodies ride in with the head, which is why nothing
    /// failed yet; this body cannot.
    #[tokio::test]
    async fn a_write_larger_than_a_socket_buffer_reaches_the_stub_whole() {
        let (base, stub) = one_request("{}").await;
        let big = "x".repeat(4 << 20);
        let http = crate::sor_client::machine_client().unwrap();
        api_at_signed(
            &http,
            &base,
            reqwest::Method::PUT,
            "/api/jobs/x/steps/y",
            Some(json!({"filler": big, "status": "completed"})),
            Signature::As("claude@algedonic.dev".into()),
        )
        .await
        .expect("the stub reads the whole write before it answers");
        let request = stub.await.unwrap().expect("the stub read a request");
        assert!(
            request.ends_with(r#""status":"completed"}"#),
            "the stub must drain the body it was sent; it read {} bytes",
            request.len()
        );
    }

    /// Backlog 034002b3, at the wire: a write sent while the jobs API is
    /// mid-roll (nothing listening, so the connect is refused) is not a
    /// failed verb — it is sent again once the API comes back, and it
    /// arrives once. Until this, `boss gate` and `boss design` exited 1
    /// at once and the operator relaunched by hand.
    #[tokio::test]
    async fn a_write_sent_during_a_roll_arrives_once_the_api_is_back() {
        // A port nothing serves yet, HELD across the roll: bound but not
        // listening, so a connect is refused exactly as a dark API's is,
        // and no other process can take the port in the gap — freeing it
        // and rebinding 300 ms later let one (backlog 1fe351e8).
        let socket = tokio::net::TcpSocket::new_v4().unwrap();
        socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let base = format!("http://{}", socket.local_addr().unwrap());
        let back = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            let listener = socket.listen(16).unwrap();
            let (mut sock, _) = listener.accept().await.unwrap();
            use tokio::io::AsyncWriteExt;
            let head = super::stub::read_request(&mut sock).await;
            let _ = sock
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                      content-length: 2\r\nconnection: close\r\n\r\n{}",
                )
                .await;
            head
        });
        let http = crate::sor_client::machine_client().unwrap();
        api_at_signed(
            &http,
            &base,
            reqwest::Method::POST,
            "/api/jobs",
            Some(json!({"kind": "backlog-item"})),
            Signature::As("claude@algedonic.dev".into()),
        )
        .await
        .expect("a refused connect is waited out, not surfaced");
        let head = back.await.unwrap();
        assert!(
            head.starts_with("POST /api/jobs"),
            "the write reached the API once it was back; head was:\n{head}"
        );
    }

    /// The loud case. A write nobody named does not go out at all —
    /// and in particular does not go out as automation, which is the
    /// whole defect.
    #[tokio::test]
    async fn an_unnamed_write_never_reaches_the_network() {
        let (base, stub) = one_request("{}").await;
        let http = crate::sor_client::machine_client().unwrap();
        let refusal = crate::identity::refusal("POST", "/api/jobs");
        let err = api_at_signed(
            &http,
            &base,
            reqwest::Method::POST,
            "/api/jobs",
            Some(json!({"kind": "backlog-item"})),
            Signature::Refused(refusal.clone()),
        )
        .await
        .expect_err("an unnamed write is refused");
        assert_eq!(err.to_string(), refusal);
        assert!(
            err.to_string().contains(crate::identity::ACTOR_ENV),
            "{err}"
        );
        // The stub finishes only once it has ACCEPTED a connection, so
        // an unfinished stub is proof nothing was sent.
        assert!(
            !stub.is_finished(),
            "a refused write must not reach the socket"
        );
        stub.abort();
    }

    /// A DARK DOOR IS NOT AN EMPTY YARD (backlog 7b7e0529). `api_at`
    /// answers `Ok(None)` for a 200 whose body is not JSON — a proxy's
    /// login page, say — and the rows helper every operator verb read
    /// through turned that into an empty list, so `boss orient` printed
    /// "0 train(s)" for a yard it had never seen and `boss cadence`
    /// would have filed a second packet beside an open one. The one rows
    /// helper refuses it instead.
    #[tokio::test]
    async fn a_login_page_on_a_list_read_is_refused_not_read_as_empty() {
        let (base, stub) = one_request("<html><body>Sign in to continue</body></html>").await;
        let http = crate::sor_client::machine_client().unwrap();
        let body = api_at_signed(
            &http,
            &base,
            reqwest::Method::GET,
            "/api/jobs?kind=pr-train&status=open&limit=10",
            None,
            Signature::Unidentified,
        )
        .await
        .expect("a 200 is an answer, not a transport failure");
        stub.abort();
        let why = rows(body)
            .expect_err("a page that is not JSON is no list, and must not read as zero rows")
            .to_string();
        assert!(why.contains("cannot be read as zero"), "{why}");
    }

    /// THE WRITE SIDE of the same defect (backlog 10776b6c). A write
    /// that meets a proxy's login page answered `Ok(None)` — the shape
    /// of a 204 — so `boss gate`, `boss prove` and `boss park` reported
    /// success for a write that may never have reached the jobs API. A
    /// write's success is its parsed answer, not its status, and the
    /// refusal names the method, the path and what came back instead.
    #[tokio::test]
    async fn a_login_page_on_a_write_is_refused_naming_what_answered() {
        let (base, stub) = one_request("<html><body>Sign in to continue</body></html>").await;
        let http = crate::sor_client::machine_client().unwrap();
        let why = api_at_signed(
            &http,
            &base,
            reqwest::Method::PUT,
            "/api/jobs/x/steps/y",
            Some(json!({"status": "completed"})),
            Signature::As("claude@algedonic.dev".into()),
        )
        .await
        .expect_err("a 200 that is not JSON is no answer to a write")
        .to_string();
        stub.abort();
        assert!(why.contains("PUT /api/jobs/x/steps/y"), "{why}");
        assert!(why.contains("Sign in to continue"), "{why}");
        assert!(why.contains("may never have reached"), "{why}");
    }

    /// The jobs API answers several writes — the job PUT, both metadata
    /// PATCHes, the step PUT, a registry DELETE — with an empty 204.
    /// That IS the answer: the refusal must not touch it.
    #[tokio::test]
    async fn an_empty_204_is_a_writes_success() {
        for method in [
            reqwest::Method::PUT,
            reqwest::Method::PATCH,
            reqwest::Method::DELETE,
            reqwest::Method::POST,
        ] {
            let (base, stub) = one_response("204 No Content", "").await;
            let http = crate::sor_client::machine_client().unwrap();
            let answer = api_at_signed(
                &http,
                &base,
                method.clone(),
                "/api/jobs/x/metadata",
                Some(json!({"k": "v"})),
                Signature::As("claude@algedonic.dev".into()),
            )
            .await
            .unwrap_or_else(|e| panic!("{method}: a 204 is a write's success: {e:#}"));
            stub.abort();
            assert_eq!(answer, None, "{method}");
        }
    }

    /// An EMPTY 200 is not what the jobs API sends to a write — every
    /// write handler answers JSON or 204 — so it is refused the same way.
    #[tokio::test]
    async fn an_empty_200_on_a_write_is_refused() {
        let (base, stub) = one_request("").await;
        let http = crate::sor_client::machine_client().unwrap();
        let why = api_at_signed(
            &http,
            &base,
            reqwest::Method::POST,
            "/api/jobs",
            Some(json!({"kind": "backlog-item"})),
            Signature::As("claude@algedonic.dev".into()),
        )
        .await
        .expect_err("an empty 200 is no answer to a write")
        .to_string();
        stub.abort();
        assert!(why.contains("POST /api/jobs"), "{why}");
        assert!(why.contains("204"), "{why}");
    }

    /// The quote is cut, because a login page is kilobytes.
    #[test]
    fn a_refused_write_quotes_at_most_the_first_200_chars() {
        let page = format!("<html>{}</html>", "x".repeat(5_000));
        let why = success_answer(
            "jobs api",
            &reqwest::Method::POST,
            "/api/jobs",
            reqwest::StatusCode::OK,
            &page,
        )
        .expect_err("refused")
        .to_string();
        assert!(why.contains(&page[..200]), "{why}");
        assert!(!why.contains(&page[..201]), "{why}");
    }

    /// A read keeps its answer: `rows` and each caller's `context`
    /// already refuse a missing body, so this car changes writes only.
    #[test]
    fn a_read_that_is_not_json_still_answers_none() {
        let answer = success_answer(
            "jobs api",
            &reqwest::Method::GET,
            "/api/jobs",
            reqwest::StatusCode::OK,
            "<html>Sign in</html>",
        )
        .expect("a read is judged by its reader");
        assert_eq!(answer, None);
    }

    /// A read attributes nothing, so it proceeds — but marked, never
    /// as an automation slug the server would read as a process.
    #[tokio::test]
    async fn an_unnamed_read_arrives_marked() {
        let (base, stub) = one_request(r#"{"data":[]}"#).await;
        let http = crate::sor_client::machine_client().unwrap();
        api_at_signed(
            &http,
            &base,
            reqwest::Method::GET,
            "/api/jobs",
            None,
            Signature::Unidentified,
        )
        .await
        .expect("a read still works");
        let head = stub.await.unwrap().expect("the stub read a request");
        assert!(
            head.contains(crate::identity::UNIDENTIFIED),
            "an unnamed read must say so; head was:\n{head}"
        );
        assert!(!head.contains(crate::identity::CONDUCTOR), "{head}");
        // Backlog d843abf2: and it arrives under the platform's own
        // READ role, not the operator's — the header a stray shell or
        // a forge probe reads with must not be a whole-world one.
        assert!(
            head.contains(&format!(r#""role":"{}""#, crate::identity::READER_ROLE)),
            "an unnamed read must carry {}; head was:\n{head}",
            crate::identity::READER_ROLE
        );
        assert!(
            !head.contains("platform-admin"),
            "an unnamed read must not carry the operator's role; head was:\n{head}"
        );
    }

    /// Backlog 25a4f7f9: every signed door — the jobs API, and the
    /// events and ledger services the redeliver and ledger verbs reach
    /// through the same `Wire` — said `jobs api` in its errors, so an
    /// events-service refusal sent a diagnosis to the wrong service. The
    /// name comes from the port the base names, read out of the one port
    /// table; a port the table does not hold is named by the base itself,
    /// never guessed.
    #[test]
    fn a_signed_door_names_the_service_its_base_reaches() {
        let at = |svc: &str| format!("http://10.20.0.34:{}", boss_ports::prod(svc));
        assert_eq!(service_at(&at("jobs")), "jobs api");
        assert_eq!(service_at(&at("ledger")), "ledger api");
        assert_eq!(service_at(&at("events")), "events api");
        assert_eq!(service_at(&format!("{}/", at("ledger"))), "ledger api");
        let scratch = boss_ports::scratch("jobs").expect("jobs has a scratch port");
        assert_eq!(
            service_at(&format!("http://127.0.0.1:{scratch}")),
            "jobs api (scratch)"
        );
        for unknown in [
            "http://127.0.0.1:1",
            "https://boss.example.test",
            "http://[::1]",
        ] {
            assert_eq!(service_at(unknown), format!("the service at {unknown}"));
        }
    }

    /// At the wire: a refusal from a base that is not the jobs port does
    /// not claim the jobs API refused it, and names what it reached.
    #[tokio::test]
    async fn a_refusal_names_the_service_that_refused() {
        let (base, stub) = one_response("403 Forbidden", "not your period").await;
        let http = crate::sor_client::machine_client().unwrap();
        let why = api_at_signed(
            &http,
            &base,
            reqwest::Method::POST,
            "/api/ledger/periods/x/lock",
            Some(json!({})),
            Signature::As("claude@algedonic.dev".into()),
        )
        .await
        .expect_err("a 403 is a refusal")
        .to_string();
        stub.abort();
        assert!(!why.contains("jobs api"), "{why}");
        assert!(why.contains(&format!("the service at {base}")), "{why}");
        assert!(why.contains("POST /api/ledger/periods/x/lock"), "{why}");
        assert!(why.contains("not your period"), "{why}");
    }
}
