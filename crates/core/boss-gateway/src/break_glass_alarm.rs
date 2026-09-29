//! The boot alarm: a gateway that boots with no break-glass key bound to
//! its door FILES A PACKET, not only a log line (backlog 5eb583c2).
//!
//! WHY. From the 2026-09-16 cutover until 2026-09-28 both enrolled keys
//! were bound to `playground.algedonic.dev` while the door answered as
//! `boss.algedonic.dev`, so no key could open it — twelve days of an
//! emergency door that was not there, and nothing a person reads said so.
//! Car 1c4c100a made the boot SAY it, with `tracing::error`; a check
//! nobody reads is a check that is not running (CLAUDE.md §Diagnosis), so
//! the boot now files the finding as an urgent backlog-item as well.
//!
//! ONE PACKET, WHOEVER NOTICES FIRST. Design 81b514be D7: the packet
//! carries `estate_finding = break_glass_unbound:<door rp_id>`, the key
//! the estate loop's running-time finding (D5, a later car) raises under,
//! so the raiser's dedup — on the key alone — reads this packet as the
//! alarm already raised.
//!
//! NO `scope` UNTIL THAT READING EXISTS (adversarial review of this car,
//! F1). `estate.recover` closes any open `estate_finding` packet on a
//! `(scope, host)` series once the newest three comparisons after it
//! opened lack its key — and until D5 lands NO comparison can carry
//! `break_glass_unbound`, so a scoped packet would be closed as
//! "recovered" ~45 minutes after filing, a false fact, and re-filed and
//! re-closed on every gateway roll. A scope-less packet matches no
//! series, so no machine closes it; a PERSON closes it once a key is
//! verified by touch at `/break-glass`. The D5 car adds [`FINDING_SCOPE`]
//! to this packet in the same change that makes the reading exist.
//!
//! DEDUPED AGAINST AN OPEN ONE. A crash-looping gateway boots every few
//! seconds; each boot reads the open estate packets first and files only
//! when none carries the key. A read that cannot be trusted whole — the
//! jobs API dark, a listing with no `data`, a page shorter than its own
//! `total` — HOLDS rather than files, the posture `estate.alarm` takes
//! for the same reason: blind filing on a failed read is the flood.
//!
//! BEST-EFFORT, ALWAYS. The filing runs detached from the boot and
//! nothing it answers can stop the gateway serving — an arm that needs
//! the patient is not an arm, and the emergency door is the last thing
//! that may wait on the jobs API. The `tracing::error` stays beside it,
//! so a boot whose filing failed still says what it found.

use std::collections::BTreeSet;
use std::path::Path;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::break_glass::{Binding, BreakGlassCredential, Door, JOBS_TIMEOUT, binding, load_store};

/// The finding class; the key is `break_glass_unbound:<door rp_id>`
/// (design 81b514be D5/D7 — `<class>:<id>`, the estate keys' shape).
pub const FINDING_CLASS: &str = "break_glass_unbound";

/// The estate series the running-time reading rides on: the in-cluster
/// observer's `kubernetes-nodes` observation, beside the dispatcher's
/// `readyz` counters (`infra/cluster/manifests/boss-estate-observe.yaml`,
/// design 81b514be decision 2). NOT written on the boot packet today:
/// `estate.recover` would judge it against a series that cannot yet
/// carry the finding and close it falsely (review F1, module docs). It
/// is named here for the D5 car, which writes it when the reading lands
/// — the one change in which a machine close becomes true.
pub const FINDING_SCOPE: &str = "kubernetes-nodes";

/// The lane this packet entered through — `boss_jobs::channels::
/// InputChannel::Telemetry`'s label, spelled here because the gateway
/// does not link `boss-jobs`, and held equal to it by
/// `the_lane_is_the_vocabularys_telemetry_label` (CLAUDE.md §9a).
pub const LANE: &str = "telemetry/monitoring";

/// The largest page the jobs API serves (`boss-jobs` `MAX_LIMIT`); the
/// dedup read asks for one page and refuses one shorter than its total.
const DEDUP_PAGE: usize = 1000;

/// The finding's key for one door.
pub fn finding_key(door: &Door) -> String {
    format!("{FINDING_CLASS}:{}", door.rp_id)
}

/// What a boot found when no record is bound to its door: the counts the
/// packet names, so a reader learns WHY without opening the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unbound {
    pub key: String,
    pub door_rp_id: String,
    /// `label → rp_id` for each record bound to another relying party.
    pub elsewhere: Vec<String>,
    /// Labels of records committed before records named their party.
    pub unrecorded: Vec<String>,
}

/// The finding, pure: `Some` when NO record is bound to this door — an
/// empty store, every record bound elsewhere, or only records that do
/// not say. An unrecorded record does not count as bound: design
/// 81b514be D5 alarms on `bound == 0`, and the boot must judge the same
/// door the observer judges. `None` the moment one record binds here.
pub fn unbound(records: &[BreakGlassCredential], door: &Door) -> Option<Unbound> {
    let bindings: Vec<(&BreakGlassCredential, Binding)> = records
        .iter()
        .map(|r| (r, binding(r, &door.rp_id)))
        .collect();
    if bindings.iter().any(|(_, b)| *b == Binding::ThisDoor) {
        return None;
    }
    Some(Unbound {
        key: finding_key(door),
        door_rp_id: door.rp_id.clone(),
        elsewhere: bindings
            .iter()
            .filter_map(|(r, b)| match b {
                Binding::Elsewhere(rp) => Some(format!("{} → {rp}", r.label.as_str())),
                _ => None,
            })
            .collect(),
        unrecorded: bindings
            .iter()
            .filter(|(_, b)| *b == Binding::Unrecorded)
            .map(|(r, _)| r.label.as_str().to_string())
            .collect(),
    })
}

/// One line saying what the store holds, for the packet and the log.
pub fn store_words(u: &Unbound) -> String {
    if u.elsewhere.is_empty() && u.unrecorded.is_empty() {
        return "the store holds no break-glass record at all".to_string();
    }
    let mut parts = Vec::new();
    if !u.elsewhere.is_empty() {
        parts.push(format!(
            "bound to another relying party: {}",
            u.elsewhere.join(", ")
        ));
    }
    if !u.unrecorded.is_empty() {
        parts.push(format!(
            "naming no relying party: {}",
            u.unrecorded.join(", ")
        ));
    }
    format!("no record is bound to this door ({})", parts.join("; "))
}

/// The urgent packet the finding becomes. The owner is left to the jobs
/// API (`boss_core::platform_owner::NOBODY`: it resolves the kind's
/// `owner_role`, or refuses) — the gateway boots before it may reach
/// the people registry, and a named owner would be a literal.
pub fn alarm_body(u: &Unbound) -> Value {
    let door = &u.door_rp_id;
    json!({
        "kind": "backlog-item",
        "title": format!(
            "ALARM: the break-glass door {door} has no key bound to it — no emergency sign-in can open it"
        ),
        "subject": {"subject_kind": "custom", "id": "bosspipeline"},
        "owner_id": boss_core::platform_owner::NOBODY,
        "priority": "urgent",
        "status": "open",
        "tags": [],
        // No `scope` (review F1): see [`FINDING_SCOPE`] and the module docs.
        "metadata": {
            "area": "gateway",
            "estate_finding": u.key,
            "input_channel": LANE,
            "detail": format!(
                "Raised by the gateway at boot (backlog 5eb583c2): {store}. A WebAuthn \
                 credential asserts only for the relying party it was enrolled under, so \
                 the emergency door at https://{door}/break-glass cannot be opened by any \
                 key it holds. TO REPAIR IT, per key (primary, then backup): (1) find the \
                 open `break-glass-enrolment` packet for that label with rp_id {door}, or \
                 file one (boss job file --kind break-glass-enrolment --subject \
                 custom/break-glass-<label> --meta label=<label> --meta rp_id={door} \
                 --meta origin=https://{door}), and approve its `authorise` step with your \
                 presence passkey; (2) at https://{door}/break-glass, signed in as \
                 yourself, paste that packet's id under Enrollment and touch the key; \
                 (3) commit the record the page prints into \
                 infra/cluster/manifests/boss-break-glass-credentials.yaml; (4) once it \
                 has converged, verify by touching the key at https://{door}/break-glass. \
                 Close this packet by hand after (4): until the estate loop reads the \
                 store (design 81b514be D5) no machine can see the door recover, so none \
                 closes it. One packet per door: the key `{key}` dedups a crash-looping \
                 gateway, and later the estate observer, onto this one.",
                store = store_words(u),
                key = u.key,
            ),
        },
    })
}

/// The jobs API as the boot alarm uses it. A port so the filing is
/// tested against memory; [`JobsApiAlarms`] is the HTTP adapter.
#[async_trait]
pub trait AlarmFiling: Send + Sync {
    /// The `estate_finding` keys every OPEN backlog-item carries — the
    /// whole set, or an `Err` when it cannot be read whole.
    async fn open_finding_keys(&self) -> Result<BTreeSet<String>, String>;
    /// File one packet. Any refusal is an `Err`.
    async fn file(&self, body: &Value) -> Result<(), String>;
}

/// What a boot's filing came to. Never an error: the boot does not wait
/// on it and nothing it says can fail the gateway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// A record is bound to this door; there is nothing to raise.
    Bound,
    /// An open packet already carries the key — the crash-loop case.
    AlreadyOpen,
    /// The packet was filed.
    Filed,
    /// Not filed, and why: the dedup read could not be trusted (HELD),
    /// or the filing was refused.
    NotFiled(String),
}

/// Raise the finding for this store, at most once per open packet.
pub async fn raise(
    port: &dyn AlarmFiling,
    records: &[BreakGlassCredential],
    door: &Door,
) -> Outcome {
    let Some(u) = unbound(records, door) else {
        return Outcome::Bound;
    };
    match port.open_finding_keys().await {
        Err(why) => Outcome::NotFiled(format!(
            "HELD: the open-alarm read could not be trusted ({why}); filing blind is how a \
             crash-looping pod files one packet per boot"
        )),
        Ok(open) if open.contains(&u.key) => Outcome::AlreadyOpen,
        Ok(_) => match port.file(&alarm_body(&u)).await {
            Ok(()) => Outcome::Filed,
            Err(why) => Outcome::NotFiled(why),
        },
    }
}

/// The open `estate_finding` keys out of one listing body, or why the
/// listing cannot prove an alarm unraised: no `data` array, or a page
/// shorter than its own `total` (a missing `total` reads as truncated,
/// never as zero). Pure, so the refusals are pinned without a server.
pub fn open_finding_keys(listing: &Value) -> Result<BTreeSet<String>, String> {
    let rows = listing
        .get("data")
        .and_then(Value::as_array)
        .ok_or("the listing answered no `data` array")?;
    let total = listing
        .get("total")
        .and_then(Value::as_u64)
        .ok_or("the listing carries no `total`")?;
    if (rows.len() as u64) < total {
        return Err(format!(
            "the listing was truncated: {} rows of {total}",
            rows.len()
        ));
    }
    Ok(rows
        .iter()
        .filter_map(|j| j.pointer("/metadata/estate_finding")?.as_str())
        .map(String::from)
        .collect())
}

/// The dedup read: OPEN backlog-items carrying an `estate_finding`, one
/// full page. Each clause is load-bearing — without `status=open` a
/// closed alarm suppresses a new one forever; without `metadata_has` the
/// page fills with unrelated items and the truncation HOLD never lets a
/// filing through (dde64482) — so the URL is pure and pinned.
pub fn dedup_url(jobs_base: &str) -> String {
    format!(
        "{}/api/jobs?kind=backlog-item&status=open&metadata_has=estate_finding&limit={DEDUP_PAGE}",
        jobs_base.trim_end_matches('/')
    )
}

/// Where the alarm is filed.
pub fn file_url(jobs_base: &str) -> String {
    format!("{}/api/jobs", jobs_base.trim_end_matches('/'))
}

/// The HTTP adapter, reading and filing as the gateway's own service
/// identity ([`crate::passkey::sign_as_gateway`]), like the enrolment's.
pub struct JobsApiAlarms {
    pub http: crate::machine_client::MachineClient,
    pub jobs_base: String,
}

impl JobsApiAlarms {
    pub fn new(jobs_base: String) -> anyhow::Result<Self> {
        Ok(Self {
            http: crate::machine_client::MachineClient::build(
                reqwest::Client::builder().timeout(JOBS_TIMEOUT),
            )?,
            jobs_base,
        })
    }
}

#[async_trait]
impl AlarmFiling for JobsApiAlarms {
    async fn open_finding_keys(&self) -> Result<BTreeSet<String>, String> {
        // Fixed text on failure: reqwest's errors name the internal URL.
        let resp = crate::passkey::sign_as_gateway(self.http.get(dedup_url(&self.jobs_base)))
            .send()
            .await
            .map_err(|_| "jobs unreachable".to_string())?;
        if !resp.status().is_success() {
            return Err(format!("listing open packets answered {}", resp.status()));
        }
        let body: Value = resp
            .json()
            .await
            .map_err(|_| "the listing was not JSON".to_string())?;
        open_finding_keys(&body)
    }

    async fn file(&self, body: &Value) -> Result<(), String> {
        let resp = crate::passkey::sign_as_gateway(self.http.post(file_url(&self.jobs_base)))
            .json(body)
            .send()
            .await
            .map_err(|_| "jobs unreachable — the alarm was not filed".to_string())?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(format!("filing the alarm answered {}", resp.status()))
        }
    }
}

/// The boot's whole act: read the store, and when no record is bound to
/// `door`, say so in the log and file the alarm on a DETACHED task. The
/// handle is returned for tests; the boot drops it. `None` when there is
/// nothing to raise, the store cannot be read, or there is no runtime to
/// file from — each said in the log, none able to stop the boot.
pub fn alarm_if_unbound(
    dir: &Path,
    door: &Door,
    port: std::sync::Arc<dyn AlarmFiling>,
) -> Option<tokio::task::JoinHandle<Outcome>> {
    let records = match load_store(dir) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(
                dir = %dir.display(),
                error = %e,
                "break-glass store unreadable at boot — whether any key opens this door is unknown"
            );
            return None;
        }
    };
    let u = unbound(&records, door)?;
    // Said at boot as well as at the door: a store whose keys cannot open
    // this door is a total failure of the emergency path, and the one
    // moment it is certain to be read otherwise is the emergency
    // (1c4c100a). The packet below is the reader; this line survives a
    // filing that fails.
    tracing::error!(
        door = %door.rp_id,
        finding = %u.key,
        "no break-glass key can open this door: {} — re-enrol through a {} packet",
        store_words(&u),
        crate::break_glass::AUTHORISATION_KIND
    );
    let handle = match tokio::runtime::Handle::try_current() {
        Ok(h) => h,
        Err(_) => {
            tracing::error!(finding = %u.key, "no runtime at boot — the alarm was not filed");
            return None;
        }
    };
    let door = door.clone();
    Some(handle.spawn(async move {
        let outcome = raise(port.as_ref(), &records, &door).await;
        match &outcome {
            Outcome::Filed => tracing::info!(finding = %u.key, "break-glass boot alarm filed"),
            Outcome::AlreadyOpen => {
                tracing::info!(finding = %u.key, "break-glass boot alarm already open — not re-filed")
            }
            Outcome::NotFiled(why) => tracing::error!(
                finding = %u.key,
                "break-glass boot alarm NOT filed: {why} — the error above is the only record"
            ),
            Outcome::Bound => {}
        }
        outcome
    }))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::break_glass::CredentialLabel;
    use chrono::Utc;
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;
    use webauthn_rs::prelude::Url;

    const DOOR_RP: &str = "boss.test";

    fn door() -> Door {
        Door::of(&Url::parse("https://boss.test").unwrap()).unwrap()
    }

    fn record(label: CredentialLabel, rp_id: Option<&str>) -> BreakGlassCredential {
        BreakGlassCredential {
            credential_id: format!("cred-{}", label.as_str()),
            public_key: "cHVibGlj".into(),
            sign_count: 0,
            aaguid: "00000000-0000-0000-0000-000000000000".into(),
            enrolled_at: Utc::now(),
            label,
            rp_id: rp_id.map(String::from),
        }
    }

    /// The jobs API, held in memory: what is open, what was filed, how
    /// often each door was knocked on — and, on request, a refusal.
    /// A filed packet is OPEN from then on, the way the jobs API holds
    /// it, so a second boot reads the first boot's alarm.
    #[derive(Default)]
    pub(crate) struct MemoryAlarms {
        pub open: Mutex<BTreeSet<String>>,
        pub filed: Mutex<Vec<Value>>,
        pub reads: Mutex<usize>,
        pub read_fails: bool,
        pub file_fails: bool,
    }

    #[async_trait]
    impl AlarmFiling for MemoryAlarms {
        async fn open_finding_keys(&self) -> Result<BTreeSet<String>, String> {
            *self.reads.lock().unwrap() += 1;
            if self.read_fails {
                return Err("jobs unreachable".into());
            }
            Ok(self.open.lock().unwrap().clone())
        }
        async fn file(&self, body: &Value) -> Result<(), String> {
            if self.file_fails {
                return Err("jobs unreachable — the alarm was not filed".into());
            }
            if let Some(k) = body
                .pointer("/metadata/estate_finding")
                .and_then(Value::as_str)
            {
                self.open.lock().unwrap().insert(k.to_string());
            }
            self.filed.lock().unwrap().push(body.clone());
            Ok(())
        }
    }

    /// THE 2026-09-16..28 STATE (1c4c100a): both keys bound elsewhere.
    /// Two boots — a crash loop — file ONE packet, under the key design
    /// 81b514be names, on the series `estate.recover` closes it from.
    #[tokio::test]
    async fn foreign_records_file_once() {
        let port = MemoryAlarms::default();
        let records = [
            record(CredentialLabel::Primary, Some("playground.test")),
            record(CredentialLabel::Backup, Some("playground.test")),
        ];
        assert_eq!(raise(&port, &records, &door()).await, Outcome::Filed);
        assert_eq!(raise(&port, &records, &door()).await, Outcome::AlreadyOpen);
        let filed = port.filed.lock().unwrap();
        assert_eq!(filed.len(), 1, "a crash loop files one packet");
        let b = &filed[0];
        assert_eq!(b["kind"], "backlog-item");
        assert_eq!(b["priority"], "urgent");
        assert_eq!(b["status"], "open");
        assert_eq!(b["owner_id"], "", "the jobs API resolves the owner");
        let m = &b["metadata"];
        assert_eq!(
            m["estate_finding"],
            format!("break_glass_unbound:{DOOR_RP}")
        );
        assert_eq!(m["input_channel"], LANE);
        let detail = m["detail"].as_str().unwrap();
        assert!(detail.contains("primary → playground.test"), "{detail}");
        assert!(detail.contains("backup → playground.test"), "{detail}");
        assert!(b["title"].as_str().unwrap().contains(DOOR_RP));
    }

    /// Review F1: no `scope` and no `host`, so the packet sits on no
    /// estate series and `estate.recover` cannot close it as "recovered"
    /// while no comparison can yet carry the finding. The raiser's dedup
    /// reads `estate_finding` alone, so the D5 reading still finds it.
    #[test]
    fn the_boot_packet_sits_on_no_estate_series() {
        let u = unbound(&[], &door()).unwrap();
        let m = &alarm_body(&u)["metadata"];
        assert!(
            m.get("scope").is_none(),
            "a scoped packet is falsely machine-closed: {m}"
        );
        assert!(m.get("host").is_none(), "{m}");
        assert_eq!(
            m["estate_finding"],
            format!("break_glass_unbound:{DOOR_RP}")
        );
        let detail = m["detail"].as_str().unwrap();
        assert!(detail.contains("Close this packet by hand"), "{detail}");
    }

    /// Review F5: the packet leads the operator to the repair — the kind
    /// to find or file, the authorise approval, the touch at THIS door,
    /// the commit, the verifying touch — in order.
    #[test]
    fn the_packet_names_the_four_repair_steps_in_order() {
        let u = unbound(
            &[record(CredentialLabel::Primary, Some("playground.test"))],
            &door(),
        )
        .unwrap();
        let detail = alarm_body(&u)["metadata"]["detail"]
            .as_str()
            .unwrap()
            .to_string();
        let steps = [
            "(1) find the open `break-glass-enrolment` packet",
            "rp_id boss.test",
            "approve its `authorise` step with your presence passkey",
            "(2) at https://boss.test/break-glass, signed in as yourself",
            "touch the key",
            "(3) commit the record the page prints into infra/cluster/manifests/boss-break-glass-credentials.yaml",
            "(4) once it has converged, verify by touching the key at https://boss.test/break-glass",
        ];
        let mut at = 0;
        for s in steps {
            let found = detail[at..]
                .find(s)
                .unwrap_or_else(|| panic!("missing, or out of order: {s:?} in {detail}"));
            at += found + s.len();
        }
    }

    /// Review F3: each clause of the dedup read is pinned.
    #[test]
    fn the_dedup_read_is_open_estate_items_one_full_page() {
        assert_eq!(
            dedup_url("http://jobs:7900/"),
            "http://jobs:7900/api/jobs?kind=backlog-item&status=open&metadata_has=estate_finding&limit=1000"
        );
        assert_eq!(file_url("http://jobs:7900/"), "http://jobs:7900/api/jobs");
    }

    type Seen = Arc<Mutex<Vec<(String, String, String, Value)>>>;

    /// A stub jobs API: every request's method, path+query, the
    /// `x-boss-user` it carried and its body; every request is answered
    /// `status` with `listing` as the body.
    async fn stub_jobs(status: axum::http::StatusCode, listing: Value) -> (String, Seen) {
        let seen: Seen = Arc::default();
        let log = seen.clone();
        let app = axum::Router::new().fallback(
            move |method: axum::http::Method,
                  uri: axum::http::Uri,
                  headers: axum::http::HeaderMap,
                  body: axum::body::Bytes| {
                let log = log.clone();
                let listing = listing.clone();
                async move {
                    let user = headers
                        .get("x-boss-user")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or_default()
                        .to_string();
                    let v = serde_json::from_slice(&body).unwrap_or(Value::Null);
                    log.lock().unwrap().push((
                        method.to_string(),
                        uri.path_and_query()
                            .map(|p| p.to_string())
                            .unwrap_or_default(),
                        user,
                        v,
                    ));
                    (status, axum::Json(listing))
                }
            },
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), seen)
    }

    /// Review F3: the adapter reads the pinned URL and files at
    /// `/api/jobs`, both signed as the gateway.
    #[tokio::test]
    async fn the_http_adapter_reads_and_files_signed_as_the_gateway() {
        let listing = json!({"total": 1, "data": [
            {"metadata": {"estate_finding": "door_dark:dev-ssh/lan"}},
        ]});
        let (base, seen) = stub_jobs(axum::http::StatusCode::OK, listing).await;
        let adapter = JobsApiAlarms::new(base.clone()).unwrap();
        let keys = adapter.open_finding_keys().await.expect("read");
        assert!(keys.contains("door_dark:dev-ssh/lan"));
        let body = json!({"kind": "backlog-item"});
        adapter.file(&body).await.expect("filed");

        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 2, "{seen:?}");
        assert_eq!(seen[0].0, "GET");
        assert_eq!(format!("{base}{}", seen[0].1), dedup_url(&base));
        assert_eq!(seen[1].0, "POST");
        assert_eq!(seen[1].1, "/api/jobs");
        assert_eq!(seen[1].3, body);
        for (_, _, user, _) in &seen {
            let u: Value = serde_json::from_str(user).expect("x-boss-user is set");
            assert_eq!(
                u["id"],
                crate::passkey::GATEWAY_ACTOR,
                "signed as the gateway"
            );
        }
    }

    /// Review F3: a refusal is an error on both doors — a non-2xx filing
    /// is never read as filed, nor a non-2xx listing as "nothing open".
    #[tokio::test]
    async fn a_non_2xx_answer_is_an_error_not_a_success() {
        let whole = json!({"total": 0, "data": []});
        for status in [
            axum::http::StatusCode::FORBIDDEN,
            axum::http::StatusCode::CONFLICT,
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        ] {
            let (base, _seen) = stub_jobs(status, whole.clone()).await;
            let adapter = JobsApiAlarms::new(base).unwrap();
            assert!(
                adapter.file(&json!({})).await.is_err(),
                "a {status} filing read as filed"
            );
            assert!(
                adapter.open_finding_keys().await.is_err(),
                "a {status} listing read as nothing open"
            );
        }
    }

    #[tokio::test]
    async fn an_empty_store_files_once() {
        let port = MemoryAlarms::default();
        assert_eq!(raise(&port, &[], &door()).await, Outcome::Filed);
        assert_eq!(raise(&port, &[], &door()).await, Outcome::AlreadyOpen);
        let filed = port.filed.lock().unwrap();
        assert_eq!(filed.len(), 1);
        let detail = filed[0]["metadata"]["detail"].as_str().unwrap();
        assert!(detail.contains("no break-glass record at all"), "{detail}");
    }

    /// One key bound here opens the door: nothing is raised, and the
    /// jobs API is not even read — a healthy boot costs it nothing.
    #[tokio::test]
    async fn a_bound_record_files_nothing() {
        let port = MemoryAlarms::default();
        let records = [
            record(CredentialLabel::Primary, Some(DOOR_RP)),
            record(CredentialLabel::Backup, Some("playground.test")),
        ];
        assert_eq!(raise(&port, &records, &door()).await, Outcome::Bound);
        assert!(port.filed.lock().unwrap().is_empty());
        assert_eq!(*port.reads.lock().unwrap(), 0);
    }

    /// Design 81b514be D5: unknown is not bound. A store of records that
    /// name no party raises too, and says which.
    #[tokio::test]
    async fn records_naming_no_party_are_not_bound() {
        let port = MemoryAlarms::default();
        let records = [record(CredentialLabel::Primary, None)];
        assert_eq!(raise(&port, &records, &door()).await, Outcome::Filed);
        let filed = port.filed.lock().unwrap();
        let detail = filed[0]["metadata"]["detail"].as_str().unwrap();
        assert!(
            detail.contains("naming no relying party: primary"),
            "{detail}"
        );
    }

    /// An open alarm some other reader raised under the same key (the
    /// estate loop's, later) is the alarm: the boot does not add one.
    #[tokio::test]
    async fn an_alarm_already_open_under_the_key_is_not_refiled() {
        let port = MemoryAlarms::default();
        port.open
            .lock()
            .unwrap()
            .insert(format!("break_glass_unbound:{DOOR_RP}"));
        assert_eq!(raise(&port, &[], &door()).await, Outcome::AlreadyOpen);
        assert!(port.filed.lock().unwrap().is_empty());
    }

    /// A dedup read that cannot be trusted HOLDS: nothing is filed, so a
    /// crash loop against a sick jobs API cannot flood it.
    #[tokio::test]
    async fn a_failed_dedup_read_holds_rather_than_files() {
        let port = MemoryAlarms {
            read_fails: true,
            ..Default::default()
        };
        let out = raise(&port, &[], &door()).await;
        assert!(
            matches!(&out, Outcome::NotFiled(w) if w.contains("HELD")),
            "{out:?}"
        );
        assert!(port.filed.lock().unwrap().is_empty());
    }

    /// THE ARM DOES NOT NEED THE PATIENT: a refused filing is an outcome,
    /// not an error, and the boot's hook hands back only a detached task
    /// — nothing it answers reaches the gateway's start.
    #[tokio::test]
    async fn a_filing_failure_does_not_fail_boot() {
        let td = TempDir::new().unwrap();
        let port = Arc::new(MemoryAlarms {
            file_fails: true,
            ..Default::default()
        });
        let handle = alarm_if_unbound(td.path(), &door(), port.clone())
            .expect("an empty store is unbound, so the filing is attempted");
        let out = handle.await.expect("the filing task does not panic");
        assert!(
            matches!(&out, Outcome::NotFiled(w) if w.contains("not filed")),
            "{out:?}"
        );
        assert!(port.filed.lock().unwrap().is_empty());
    }

    /// The hook reads the mounted store the door reads: a key committed
    /// for this door means no task at all.
    #[tokio::test]
    async fn the_boot_hook_reads_the_mounted_store() {
        let td = TempDir::new().unwrap();
        let rec = record(CredentialLabel::Primary, Some(DOOR_RP));
        std::fs::write(
            td.path().join("primary.json"),
            serde_json::to_vec(&rec).unwrap(),
        )
        .unwrap();
        let port = Arc::new(MemoryAlarms::default());
        assert!(alarm_if_unbound(td.path(), &door(), port.clone()).is_none());
        assert_eq!(*port.reads.lock().unwrap(), 0);

        let mut foreign = rec.clone();
        foreign.rp_id = Some("playground.test".into());
        std::fs::write(
            td.path().join("primary.json"),
            serde_json::to_vec(&foreign).unwrap(),
        )
        .unwrap();
        let out = alarm_if_unbound(td.path(), &door(), port.clone())
            .expect("a foreign-only store raises")
            .await
            .unwrap();
        assert_eq!(out, Outcome::Filed);
    }

    #[test]
    fn a_listing_proves_an_alarm_unraised_only_when_whole() {
        let whole = json!({"total": 2, "data": [
            {"metadata": {"estate_finding": "door_dark:dev-ssh/lan"}},
            {"metadata": {"estate_finding": "break_glass_unbound:boss.test"}},
        ]});
        let keys = open_finding_keys(&whole).unwrap();
        assert!(keys.contains("break_glass_unbound:boss.test"));
        assert_eq!(keys.len(), 2);

        let short = json!({"total": 3, "data": [{"metadata": {}}]});
        assert!(open_finding_keys(&short).unwrap_err().contains("truncated"));
        let no_total = json!({"data": []});
        assert!(
            open_finding_keys(&no_total).is_err(),
            "no total is not zero"
        );
        let error_body = json!({"error": "forbidden", "total": 0});
        assert!(
            open_finding_keys(&error_body).is_err(),
            "no data is no answer"
        );
    }

    /// CLAUDE.md §9a: the lane is the vocabulary's, spelled once more
    /// here only because the gateway does not link `boss-jobs`.
    #[test]
    fn the_lane_is_the_vocabularys_telemetry_label() {
        assert_eq!(
            LANE,
            boss_jobs::channels::InputChannel::Telemetry.label(),
            "break_glass_alarm::LANE drifted from boss-jobs channels.rs"
        );
        assert_eq!(boss_jobs::channels::RECORDED_KEY, "input_channel");
    }
}
