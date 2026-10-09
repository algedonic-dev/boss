//! The probe-reader credential's rotation (design b35c22b4; backlog
//! d26515c5): the SAME handler, declared with `gate_slots = "reader"`,
//! walked against gates that hold BOTH slot sets — the estate token's and
//! the reader's — so every test here can ask what the estate token's own
//! tests cannot: which set a gate named a value in, and which token a
//! tally was read with.
//!
//! The fixtures are this module's own because the estate's are shaped
//! for one Secret name and one slot set, and those tests are left exactly
//! as they were: this file is reached by one `mod` line at the foot of
//! `tests.rs`, and changes nothing above it.

use super::*;
use boss_core::machine_token::Source;

const NS: &str = "boss";
const READER_SECRET: &str = "boss-probe-reader";
const READER: &str = "boss-probe-reader";
const ESTATE_SECRET: &str = "boss-machine-token";
const ROTATE_RULE: &str = "broker-rotates-the-probe-reader";
const READER_ADVANCE: &str = "broker-advances-the-probe-reader-rotation";
/// The estate token every gate here accepts as `current`, and the one
/// the dispatcher's own mount would hand it. Never a reader value.
const ESTATE: &str = "the-estate-token-every-machine-caller-sends";
const OLD_READER: &str = "the-reader-value-the-forge-still-holds-xxxxx";

// ----- the Secret store: any Secret, and a record of which were touched -----

#[derive(Default)]
struct Store {
    map: Mutex<BTreeMap<String, String>>,
    versions: Mutex<BTreeMap<String, u64>>,
    /// `ns/name` of every Secret read or written, in order.
    touched: Mutex<Vec<String>>,
    /// The reader Secret does not exist: the converge has not created it.
    absent: Mutex<bool>,
}

impl Store {
    fn get(&self, key: &str) -> Option<String> {
        self.map
            .lock()
            .unwrap()
            .get(&format!("{NS}/{READER_SECRET}/{key}"))
            .cloned()
            .filter(|v| !v.is_empty())
    }
    fn seed(&self, key: &str, value: &str) {
        self.map
            .lock()
            .unwrap()
            .insert(format!("{NS}/{READER_SECRET}/{key}"), value.to_string());
        *self
            .versions
            .lock()
            .unwrap()
            .entry(format!("{NS}/{READER_SECRET}"))
            .or_default() += 1;
    }
    fn written(&self) -> Vec<String> {
        self.touched
            .lock()
            .unwrap()
            .iter()
            .filter(|t| t.starts_with("write "))
            .cloned()
            .collect()
    }
    fn touched(&self) -> Vec<String> {
        self.touched.lock().unwrap().clone()
    }
}

#[async_trait]
impl SecretStore for Store {
    async fn read_key(&self, ns: &str, name: &str, key: &str) -> Result<Option<String>, String> {
        Err(format!(
            "{ns}/{name}/{key}: this handler reads a Secret whole"
        ))
    }
    async fn write_key(&self, ns: &str, name: &str, key: &str, _: &str) -> Result<(), String> {
        Err(format!(
            "{ns}/{name}/{key}: this handler writes only at a version"
        ))
    }
    async fn read_secret(&self, ns: &str, name: &str) -> Result<Option<SecretData>, String> {
        self.touched
            .lock()
            .unwrap()
            .push(format!("read {ns}/{name}"));
        if *self.absent.lock().unwrap() {
            return Ok(None);
        }
        let prefix = format!("{ns}/{name}/");
        let data = self
            .map
            .lock()
            .unwrap()
            .iter()
            .filter_map(|(k, v)| Some((k.strip_prefix(&prefix)?.to_string(), v.clone())))
            .collect();
        let version = self
            .versions
            .lock()
            .unwrap()
            .get(&format!("{ns}/{name}"))
            .copied()
            .unwrap_or_default()
            .to_string();
        Ok(Some(SecretData {
            uid: "probe-reader-fixture".into(),
            version,
            data,
        }))
    }
    async fn write_keys_at(
        &self,
        ns: &str,
        name: &str,
        entries: &[(&str, &str)],
        version: &str,
    ) -> Result<WriteAt, String> {
        self.touched
            .lock()
            .unwrap()
            .push(format!("write {ns}/{name}"));
        let mut versions = self.versions.lock().unwrap();
        let at = versions.entry(format!("{ns}/{name}")).or_default();
        if at.to_string() != version {
            return Ok(WriteAt::Moved);
        }
        *at += 1;
        let mut map = self.map.lock().unwrap();
        for (key, value) in entries {
            map.insert(format!("{ns}/{name}/{key}"), value.to_string());
        }
        Ok(WriteAt::Written)
    }
}

// ----- the gates: the REAL machine gate, holding both slot sets -----

struct Port {
    real: Arc<MachineGate>,
    served: bool,
    mode: Mode,
    estate: [Option<String>; 3],
    reader: [Option<String>; 3],
    extra_rows: Vec<MissRow>,
    facts: Vec<boss_core::event::Event>,
    rx: tokio::sync::mpsc::Receiver<boss_core::event::Event>,
}

impl Port {
    fn reading(&self) -> Reading {
        let [c, n, p] = self.estate.clone();
        let [rc, rn, rp] = self.reader.clone();
        Reading::new(self.mode, Slots::new(c, n, p)).with_reader(Slots::new(rc, rn, rp))
    }
    fn reread(&self) {
        self.real.observe(self.reading());
    }
}

/// What was presented to which route of which port — lengths never
/// needed: the values are this file's own fixtures.
type Asked = Mutex<Vec<(&'static str, String, String)>>;

struct Gates {
    ports: Mutex<BTreeMap<String, Port>>,
    asked: Asked,
}

impl Gates {
    /// Every port in `report`, recording since three days ago, accepting
    /// [`ESTATE`] as the estate token's `current` and no reader value.
    fn new(served: &[&str]) -> Arc<Self> {
        let since = Utc::now() - chrono::Duration::days(3);
        let ports = served
            .iter()
            .map(|name| {
                let (evidence, mut rx) = boss_core::gate_evidence::Evidence::channel(
                    boss_core::gate_evidence::Gate::MachineGate,
                    name,
                );
                let estate = [Some(ESTATE.to_string()), None, None];
                let [c, n, p] = estate.clone();
                let real = Arc::new(
                    MachineGate::starting_at(
                        name,
                        &[],
                        Reading::new(Mode::Report, Slots::new(c, n, p)),
                        since,
                    )
                    .with_evidence(evidence),
                );
                let mut began = rx.try_recv().unwrap();
                began.timestamp = since; // The fixture declares a historical first statement.
                (
                    name.to_string(),
                    Port {
                        real,
                        served: true,
                        mode: Mode::Report,
                        estate,
                        reader: [None, None, None],
                        extra_rows: Vec::new(),
                        facts: vec![began],
                        rx,
                    },
                )
            })
            .collect();
        Arc::new(Self {
            ports: Mutex::new(ports),
            asked: Mutex::new(Vec::new()),
        })
    }

    /// Kubelet refreshed the pod's mount of the READER Secret: every
    /// gate's reader slots are now that Secret's. The estate slots are a
    /// different Secret's and do not move.
    fn refresh(&self, store: &Store) {
        self.refresh_only(store, &[]);
    }

    /// The same, but the named ports' mounts are still stale.
    fn refresh_only(&self, store: &Store, stale: &[&str]) {
        let slots = SLOTS.map(|s| store.get(s));
        for (name, port) in self.ports.lock().unwrap().iter_mut() {
            if stale.contains(&name.as_str()) {
                continue;
            }
            port.reader = slots.clone();
            port.reread();
        }
    }

    fn with(&self, service: &str, f: impl FnOnce(&mut Port)) {
        let mut ports = self.ports.lock().unwrap();
        let port = ports.get_mut(service).expect("a port");
        f(port);
        port.reread();
    }

    fn asked(&self, route: &str) -> Vec<String> {
        self.asked
            .lock()
            .unwrap()
            .iter()
            .filter(|(r, _, _)| *r == route)
            .map(|(_, _, presented)| presented.clone())
            .collect()
    }
}

#[async_trait]
impl GateReader for Gates {
    fn roster(&self) -> Vec<String> {
        self.ports.lock().unwrap().keys().cloned().collect()
    }
    async fn accepts(&self, service: &str, presented: &str) -> GateRead<Accepts> {
        self.asked
            .lock()
            .unwrap()
            .push(("accepts", service.into(), presented.into()));
        let ports = self.ports.lock().unwrap();
        let port = &ports[service];
        if !port.served {
            return GateRead::NotServed;
        }
        GateRead::Answered(port.real.accepts(Some(presented)))
    }
    /// The gate's own rule (`misses_route`): the tally answers an ESTATE
    /// slot and nothing else — a reader value is a 401.
    async fn misses(&self, service: &str, presented: &str) -> GateRead<Misses> {
        self.asked
            .lock()
            .unwrap()
            .push(("misses", service.into(), presented.into()));
        let ports = self.ports.lock().unwrap();
        let port = &ports[service];
        if !port.served {
            return GateRead::NotServed;
        }
        if port.real.reading().slots.matched(Some(presented)).is_none() {
            return GateRead::Failed("answered 401: the tally wants an accepted token".into());
        }
        let mut m = port.real.misses();
        m.rows.extend(port.extra_rows.iter().cloned());
        GateRead::Answered(m)
    }
    async fn window(
        &self,
        presented: &str,
        hours: i64,
    ) -> Result<boss_core::gate_window::JoinedWindow, String> {
        self.asked
            .lock()
            .unwrap()
            .push(("window", "events".into(), presented.into()));
        let roster = self.roster();
        let mut reads = Vec::new();
        for service in &roster {
            let answer = match self.misses(service, presented).await {
                GateRead::Answered(misses) => Ok(serde_json::to_value(misses).unwrap()),
                GateRead::NotServed => Err("connection refused".into()),
                GateRead::Failed(error) | GateRead::Unasked(error) => Err(error),
            };
            reads.push(boss_core::gate_window::LiveRead {
                service: service.clone(),
                answer,
            });
        }
        let mut facts = Vec::new();
        for port in self.ports.lock().unwrap().values_mut() {
            while let Ok(event) = port.rx.try_recv() {
                port.facts.push(event);
            }
            facts.extend(port.facts.iter().cloned());
        }
        facts.sort_by_key(|event| (event.timestamp, event.id));
        let now = Utc::now();
        Ok(boss_core::gate_window::join_window(
            boss_core::gate_evidence::Gate::MachineGate,
            &roster,
            now - chrono::Duration::hours(hours),
            now,
            Ok(facts),
            reads,
        ))
    }
}

fn row(presented: Presented, last_seen: DateTime<Utc>) -> MissRow {
    MissRow {
        key: MissKey {
            peer: "192.0.2.15".into(),
            user: boss_core::roles::PROBE_READER_ACTOR.into(),
            method: "GET".into(),
            route: "/api/jobs/{id}".into(),
            presented,
        },
        count: 3,
        first_seen: last_seen,
        last_seen,
    }
}

// ----- the jobs API: the estate's stub, behind a registry that may lack the row -----

/// The estate tests' stateful jobs API answers `GET /api/credentials/<id>`
/// for the machine token alone. This is that stub with one route in
/// front of it: the credentials registry, holding the probe-reader row
/// or not; everything else is handed through unchanged, so the packets,
/// the step writes and the rotation phases are recorded exactly where
/// the estate's tests read them.
async fn reader_jobs(packets: &[(&str, StubJob)], declared: bool) -> Jobs {
    use axum::extract::{Path, Request, State};
    use axum::response::IntoResponse;

    async fn hand_through(State(inner): State<String>, req: Request) -> axum::response::Response {
        let (parts, body) = req.into_parts();
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        let url = format!(
            "{inner}{}",
            parts.uri.path_and_query().map_or("/", |p| p.as_str())
        );
        let client =
            boss_core::machine_token::Client::unstamped(reqwest::Client::builder()).unwrap();
        let mut out = client.request(parts.method, url).body(bytes);
        if let Some(kind) = parts.headers.get("content-type") {
            out = out.header("content-type", kind);
        }
        let answer = out.send().await.unwrap();
        let status = answer.status();
        let body = answer.bytes().await.unwrap();
        (
            status,
            [("content-type", "application/json")],
            axum::body::Body::from(body),
        )
            .into_response()
    }

    let inner = jobs(packets).await;
    let app = axum::Router::new()
        .route(
            "/api/credentials/{id}",
            axum::routing::get(move |Path(id): Path<String>| async move {
                if declared && id == READER {
                    axum::Json(json!({"id": id, "kind": "machine-token"})).into_response()
                } else {
                    axum::http::StatusCode::NOT_FOUND.into_response()
                }
            }),
        )
        .fallback(hand_through)
        .with_state(inner.url.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Jobs {
        url: format!("http://{addr}"),
        packets: inner.packets,
        writes: inner.writes,
        rotations: inner.rotations,
    }
}

fn reader_packet(opened_at: &str) -> StubJob {
    let mut p = packet(opened_at);
    p.about = READER.into();
    p
}

// ----- wiring -----

/// The reader rule's declaration: one Secret in one namespace, the
/// reader slot set, and its own clock rule's name.
fn rargs(extra: &[(&str, &str)]) -> Vec<(String, Value)> {
    [
        ("secret_namespace", NS),
        ("secret_name", READER_SECRET),
        ("credential_id", READER),
        ("drain_minutes", "1440"),
        ("gate_slots", "reader"),
        ("advance_rule", READER_ADVANCE),
    ]
    .iter()
    .chain(extra.iter())
    .map(|(k, v)| (k.to_string(), Value::String((*v).into())))
    .collect()
}

fn rscope(job: &str) -> InvocationContext {
    InvocationContext {
        event_timestamp: None,
        rule_name: ROTATE_RULE.into(),
        triggering_event_id: "evt-scope".into(),
        triggering_topic: "step.done.credential-rotation".into(),
        event_payload: json!({
            "job_id": job,
            "step_id": "step-scope",
            "kind": "credential-rotation",
            "subject_kind": "custom",
            "subject_id": READER,
            "metadata": {"credential": READER},
        }),
    }
}

fn rtick_ctx() -> InvocationContext {
    InvocationContext {
        rule_name: READER_ADVANCE.into(),
        ..tick_ctx()
    }
}

/// The handler as the dispatcher builds it, holding `estate` as the
/// token its own mount hands it.
fn rhandler(
    j: &Jobs,
    s: &Arc<Store>,
    g: &Arc<Gates>,
    estate: Option<&str>,
) -> Arc<CredentialRotateSelfIssued> {
    CredentialRotateSelfIssued::with_estate_token(
        j.url.clone(),
        s.clone(),
        g.clone(),
        GatePoll {
            attempts: 1,
            interval: Duration::ZERO,
        },
        Arc::new(Source::fixed(estate.map(str::to_string))),
    )
}

async fn rtick(h: &CredentialRotateSelfIssued) -> Result<(), HandlerError> {
    h.invoke(&rargs(&[("phase", ADVANCE_PHASE)]), &rtick_ctx())
        .await
}

type Walk = (
    Jobs,
    Arc<Store>,
    Arc<Gates>,
    Arc<CredentialRotateSelfIssued>,
);

async fn fresh() -> Walk {
    let j = reader_jobs(&[(JOB, reader_packet("2026-10-07T01:00:00Z"))], true).await;
    let s = Arc::new(Store::default());
    let g = Gates::new(&ROSTER);
    let h = rhandler(&j, &s, &g, Some(ESTATE));
    (j, s, g, h)
}

// ----- the declaration -----

/// The slot set is the rule's to declare, never the handler's to guess
/// from a credential id: absent is the estate set (the machine token's
/// two rules say nothing and read as they always did), `reader` is the
/// probe reader's, and anything else is a refusal every redelivery
/// repeats — a typo must not fall back to the set that can write.
#[test]
fn the_slot_set_is_declared_by_the_rule_and_defaults_to_the_estates() {
    let estate = args(&[]);
    let d = Declaration::parse(&estate).unwrap();
    assert_eq!(d.slots, GateSlots::Estate);
    assert_eq!(d.advance_rule, ADVANCE_RULE);
    assert_eq!(
        [Slot::Current, Slot::Next, Slot::Previous].map(|s| d.slots.answers(s)),
        SLOTS,
        "the estate set answers the bare slot names the handler always asked for"
    );
    let named = args(&[("gate_slots", "estate")]);
    assert_eq!(Declaration::parse(&named).unwrap().slots, GateSlots::Estate);

    let reader = rargs(&[]);
    let d = Declaration::parse(&reader).unwrap();
    assert_eq!(d.slots, GateSlots::Reader);
    assert_eq!(d.advance_rule, READER_ADVANCE);
    assert_eq!(
        [Slot::Current, Slot::Next, Slot::Previous].map(|s| d.slots.answers(s)),
        ["reader.current", "reader.next", "reader.previous"]
    );
    assert!(d.mirrors.is_empty(), "one namespace: {:?}", d.targets());

    for typo in ["Reader", "probe-reader", "reader.", ""] {
        let bad = args(&[("gate_slots", typo)]);
        match Declaration::parse(&bad) {
            Err(HandlerError::Permanent(e)) => assert!(e.contains("gate_slots"), "{e}"),
            Err(other) => panic!("{typo:?}: {other}"),
            Ok(d) => {
                // A blank arg is an absent one, as every optional arg is.
                assert!(typo.is_empty(), "{typo:?} was read as {:?}", d.slots);
                assert_eq!(d.slots, GateSlots::Estate);
            }
        }
    }
}

/// The two rule files, read as the dispatcher reads them, parse as the
/// handler's declaration — so a slot set the handler does not know is
/// refused here and not at the first firing of a rotation David scoped.
#[test]
fn the_shipped_reader_rules_parse_as_this_handlers_declaration() {
    use boss_dispatcher::rules::registry::parse_raw_path;
    let reg = parse_raw_path(boss_testing::dispatcher_rules_dir()).expect("the rule directory");
    for (name, phase) in [(ROTATE_RULE, None), (READER_ADVANCE, Some(ADVANCE_PHASE))] {
        let rule = reg
            .rules
            .iter()
            .find(|r| r.name == name)
            .unwrap_or_else(|| panic!("{name} is in the authored directory"));
        assert_eq!(rule.do_steps.len(), 1);
        assert_eq!(rule.do_steps[0].handler, HANDLER);
        // Every arg is a quoted string literal in the rule's expr syntax.
        let literal: Vec<(String, Value)> = rule.do_steps[0]
            .args
            .iter()
            .map(|(k, v)| {
                let text = v
                    .strip_prefix('"')
                    .and_then(|v| v.strip_suffix('"'))
                    .unwrap_or_else(|| panic!("{name}: `{k}` is not a string literal: {v}"));
                (k.clone(), Value::String(text.into()))
            })
            .collect();
        let d = Declaration::parse(&literal).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(d.slots, GateSlots::Reader, "{name}");
        assert_eq!((d.secret_namespace, d.secret_name), (NS, READER_SECRET));
        assert_eq!(d.credential_id, READER);
        assert_eq!(d.advance_rule, READER_ADVANCE, "{name}");
        assert_eq!(d.phase, phase, "{name}");
        assert!(
            d.mirrors.is_empty(),
            "{name}: no other namespace holds a copy"
        );
    }
}

// ----- the walk -----

/// The first mint, start to finish, against gates that name the value
/// `reader.next` and then `reader.current`. Until the handler took the
/// slot set from its rule it asked for the bare names, so this rotation
/// staged and then deferred for ever: "jobs answers matched reader.next".
#[tokio::test]
async fn a_first_reader_mint_walks_every_phase_on_the_reader_slot_names() {
    let (j, s, g, h) = fresh().await;

    // The scope step fires: staged in next of the ONE Secret; the gates
    // have not refreshed, and the deferral names THIS credential's clock
    // rule, not the machine token's.
    fire(&h, &rscope(JOB), &rargs(&[])).await;
    let value = s.get("next").expect("next staged");
    assert_eq!(value.len(), 43);
    assert_ne!(value, ESTATE);
    assert_eq!(s.get("next.minted-for").as_deref(), Some(JOB));
    assert_eq!(s.get("current"), None);
    assert_eq!(j.status(JOB, "install"), "completed");
    let why = j.meta(JOB, "verify", "verify_deferred").expect("deferred");
    assert!(why.contains("matched none"), "{why}");
    assert!(why.contains(READER_ADVANCE), "{why}");
    assert!(
        !why.contains(ADVANCE_RULE),
        "the machine token's clock rule does not resume this: {why}"
    );

    // Kubelet refreshes: every gate names it reader.next. Promoted.
    g.refresh(&s);
    rtick(&h).await.expect("the tick promotes");
    assert_eq!(s.get("current").as_deref(), Some(value.as_str()));
    assert_eq!(s.get("next"), None);
    assert_eq!(s.get("previous"), None, "a first mint has no old value");
    assert!(s.get(PROMOTED_AT).is_some());
    assert_ne!(j.status(JOB, "verify"), "completed");
    let why = j.meta(JOB, "verify", "verify_deferred").unwrap();
    assert!(why.contains("matched reader.next"), "{why}");

    // Refreshed again: every gate names it reader.current.
    g.refresh(&s);
    rtick(&h).await.expect("the tick verifies");
    assert_eq!(j.status(JOB, "verify"), "completed");
    assert_eq!(j.status(JOB, "revoke"), "completed");
    let verified = j.meta(JOB, "verify", "verified").unwrap();
    assert!(
        verified.contains("matched reader.current") && verified.contains("3 port(s)"),
        "{verified}"
    );
    assert!(
        j.meta(JOB, "revoke", "revoked")
            .unwrap()
            .contains("nothing to revoke")
    );
    assert_eq!(
        j.phases(),
        vec![
            format!("{READER}/minted"),
            format!("{READER}/installed"),
            format!("{READER}/verified"),
            format!("{READER}/revoked"),
        ]
    );

    // The value is in no record, and the estate token's Secret was
    // neither read nor written: this rule's grant is its one Secret.
    assert!(!j.everything_written().contains(&value));
    assert!(!j.everything_written().contains(ESTATE));
    for touch in s.touched() {
        assert!(
            touch.ends_with(&format!(" {NS}/{READER_SECRET}")) && !touch.contains(ESTATE_SECRET),
            "{touch}"
        );
    }
    // Every ask of a gate about the value presented the value, alone:
    // the estate token was never the thing asked about.
    let asked = g.asked("accepts");
    assert!(!asked.is_empty());
    assert!(asked.iter().all(|presented| *presented == value));
}

/// An estate-declared rule pointed at a value the gates name in the
/// READER set never promotes it: the names differ, and a bare `next` is
/// what that rule waits for. The machine token's two rules declare no
/// slot set, so this is their reading, unchanged.
#[tokio::test]
async fn an_estate_declaration_never_promotes_on_a_reader_slot_name() {
    let (j, s, g, _) = fresh().await;
    let h = rhandler(&j, &s, &g, Some(ESTATE));
    let estate_set: Vec<(String, Value)> = rargs(&[])
        .into_iter()
        .filter(|(k, _)| k != "gate_slots")
        .collect();
    fire(&h, &rscope(JOB), &estate_set).await;
    g.refresh(&s);
    let tick: Vec<(String, Value)> = rargs(&[("phase", ADVANCE_PHASE)])
        .into_iter()
        .filter(|(k, _)| k != "gate_slots")
        .collect();
    h.invoke(&tick, &rtick_ctx()).await.expect("deferred");
    assert_eq!(s.get("current"), None, "nothing promoted");
    let why = j.meta(JOB, "verify", "verify_deferred").unwrap();
    assert!(why.contains("answers matched reader.next"), "{why}");
}

/// One port whose mount is stale stops the promotion of the whole
/// credential — a probe reads every port, and an unvouched one mid-probe
/// is a 502 the probe is judged FAILED on (review 991bb439, N1).
#[tokio::test]
async fn one_port_that_does_not_vouch_stops_the_promotion() {
    let (j, s, g, h) = fresh().await;
    fire(&h, &rscope(JOB), &rargs(&[])).await;
    g.refresh_only(&s, &["policy"]);
    rtick(&h).await.expect("deferred, not failed");
    assert_eq!(s.get("current"), None, "nothing promoted on two of three");
    assert!(s.get("next").is_some());
    let why = j.meta(JOB, "verify", "verify_deferred").unwrap();
    assert!(
        why.contains("policy answers matched none") && !why.contains("jobs answers"),
        "the lagging port is named, and only it: {why}"
    );
    assert!(!j.phases().iter().any(|p| p.ends_with("/verified")));

    // A port that is not there at all is not a port that vouches either
    // way; the control port not answering means nothing counts.
    g.refresh(&s);
    g.with(CONTROL_SERVICE, |port| port.served = false);
    rtick(&h).await.expect("deferred, not failed");
    assert_eq!(s.get("current"), None);
    g.with(CONTROL_SERVICE, |port| port.served = true);

    rtick(&h).await.expect("promoted");
    assert!(s.get("current").is_some());
}

// ----- distinctness (review 177b4976, N2) -----

/// A reader value must never be an estate token: every gate would admit
/// its holder for every method under any identity it asserts, and the
/// holder is builder-written probe text. The gate says so itself — the
/// estate set wins a match, so it names the value by a BARE slot — and
/// that answer is a refusal to go on, before the promotion and after it,
/// recorded for a hand. Until the handler knew which set it was filling,
/// a bare `next` was exactly what it waited for, and it promoted.
#[tokio::test]
async fn a_reader_value_a_gate_names_as_an_estate_slot_is_never_promoted() {
    let (j, s, g, h) = fresh().await;
    fire(&h, &rscope(JOB), &rargs(&[])).await;
    let value = s.get("next").unwrap();
    g.refresh(&s);
    // The estate Secret's `next` holds the same value on one port.
    g.with("jobs", |port| port.estate[1] = Some(value.clone()));

    let result = rtick(&h).await;
    let error = result.expect_err("a refusal, not a deferral").to_string();
    assert_eq!(s.get("current"), None, "nothing promoted");
    assert_eq!(s.get("next").as_deref(), Some(value.as_str()));
    assert!(!j.phases().iter().any(|p| p.ends_with("/verified")));
    let refused = j.meta(JOB, "verify", "verify_refused").expect("recorded");
    for text in [&refused, &error] {
        assert!(text.contains("jobs answers matched next"), "{text}");
        assert!(text.contains("ESTATE"), "{text}");
        assert!(text.contains("a hand is needed"), "{text}");
        assert!(!text.contains(RESUMES), "{text}");
        assert!(!text.contains(&value), "the record carries the value");
    }
    assert_ne!(j.status(JOB, "verify"), "completed");
}

#[tokio::test]
async fn a_promoted_reader_value_a_gate_names_as_an_estate_slot_is_not_verified() {
    let (j, s, g, h) = fresh().await;
    fire(&h, &rscope(JOB), &rargs(&[])).await;
    g.refresh(&s);
    rtick(&h).await.expect("promoted");
    let value = s.get("current").unwrap();
    g.refresh(&s);
    g.with("policy", |port| port.estate[0] = Some(value.clone()));

    let error = rtick(&h).await.expect_err("refused").to_string();
    assert!(error.contains("policy answers matched current"), "{error}");
    assert_ne!(j.status(JOB, "verify"), "completed");
    assert!(!j.phases().iter().any(|p| p.ends_with("/verified")));
    assert!(j.meta(JOB, "verify", "verify_refused").is_some());
}

// ----- the registry row -----

/// The credentials-registry row is instance data the operator publishes;
/// until it exists nothing is minted, nothing is written to the Secret,
/// no gate is asked — and the packet's own issue step says why and what
/// lands the row, because a firing that only failed on the dispatcher's
/// shelf left the packet David scoped saying nothing at all.
#[tokio::test]
async fn an_undeclared_credential_mints_nothing_and_says_so_on_the_step() {
    let j = reader_jobs(&[(JOB, reader_packet("2026-10-07T01:00:00Z"))], false).await;
    let s = Arc::new(Store::default());
    let g = Gates::new(&ROSTER);
    let h = rhandler(&j, &s, &g, Some(ESTATE));
    for firing in 0..2 {
        let error = match firing {
            0 => h.invoke(&rargs(&[]), &rscope(JOB)).await,
            _ => rtick(&h).await,
        }
        .expect_err("the firing fails")
        .to_string();
        assert!(error.contains("no registry row"), "{error}");
        assert!(error.contains(READER), "{error}");
    }
    assert!(s.written().is_empty(), "{:?}", s.written());
    assert_eq!(s.get("next"), None);
    assert!(g.asked("accepts").is_empty(), "no gate was asked");
    assert!(j.phases().is_empty(), "{:?}", j.phases());
    assert_ne!(j.status(JOB, "issue"), "completed");
    let refused = j
        .meta(JOB, "issue", "issue_refused")
        .expect("issue_refused");
    assert!(
        refused.contains("no registry row") && refused.contains("boss tenant publish"),
        "{refused}"
    );
}

/// The Secret itself is the converge's to create, empty, from this
/// rule's declaration; until it has, the rotation writes nothing.
#[tokio::test]
async fn a_secret_the_converge_has_not_created_is_named_and_nothing_is_written() {
    let (_, s, _, h) = fresh().await;
    *s.absent.lock().unwrap() = true;
    let error = h
        .invoke(&rargs(&[]), &rscope(JOB))
        .await
        .expect_err("fails")
        .to_string();
    assert!(
        error.contains(&format!("{NS}/{READER_SECRET} does not exist")),
        "{error}"
    );
    assert!(s.written().is_empty());
}

// ----- landing this car mints nothing -----

/// With no rotation packet about the probe reader, the clock rule reads
/// the packet list and nothing else: no Secret is read, none is written,
/// no gate is asked. A rotation of ANOTHER credential is not this one's.
#[tokio::test]
async fn with_no_packet_about_the_reader_nothing_is_minted_read_or_asked() {
    for packets in [vec![], vec![(OTHER, packet("2026-10-07T01:00:00Z"))]] {
        let j = reader_jobs(&packets, true).await;
        let s = Arc::new(Store::default());
        let g = Gates::new(&ROSTER);
        let h = rhandler(&j, &s, &g, Some(ESTATE));
        rtick(&h).await.expect("an idle tick succeeds");
        assert!(s.touched().is_empty(), "{:?}", s.touched());
        assert!(g.asked.lock().unwrap().is_empty());
        assert!(j.writes.lock().unwrap().is_empty());
        assert!(j.phases().is_empty());
        // And the scope rule's firing is a packet's or it is refused.
        assert!(matches!(
            h.invoke(&rargs(&[]), &rtick_ctx()).await,
            Err(HandlerError::Permanent(e)) if e.contains("advance")
        ));
        assert!(s.touched().is_empty());
    }
}

/// A packet whose scope step is not done is not a rotation yet.
#[tokio::test]
async fn a_packet_whose_scope_is_not_done_mints_nothing() {
    let mut unscoped = reader_packet("2026-10-07T01:00:00Z");
    unscoped.steps.get_mut("scope").unwrap().status = "ready".into();
    let j = reader_jobs(&[(JOB, unscoped)], true).await;
    let s = Arc::new(Store::default());
    let g = Gates::new(&ROSTER);
    let h = rhandler(&j, &s, &g, Some(ESTATE));
    rtick(&h).await.expect("idle");
    fire(&h, &rscope(JOB), &rargs(&[])).await;
    assert!(s.written().is_empty(), "{:?}", s.written());
    assert_eq!(s.get("next"), None);
}

// ----- the drain -----

/// A second rotation: the forge's client file still holds the old value
/// until its next deposit, and every probe it runs presents it. The gates
/// name that `reader.previous`, and the revoke waits a whole window in
/// which none did — read from tallies the dispatcher opens with the
/// ESTATE token, because a gate's tally refuses a reader value.
#[tokio::test]
async fn the_reader_drain_waits_out_reader_previous_and_reads_tallies_as_the_estate() {
    let (j, s, g, h) = fresh().await;
    s.seed("current", OLD_READER);
    s.seed("current.minted-for", OTHER);
    g.refresh(&s);

    fire(&h, &rscope(JOB), &rargs(&[])).await;
    g.refresh(&s);
    rtick(&h).await.expect("promoted");
    g.refresh(&s);
    rtick(&h).await.expect("verified");
    assert_eq!(j.status(JOB, "verify"), "completed");
    let value = s.get("current").unwrap();
    assert_eq!(s.get("previous").as_deref(), Some(OLD_READER));
    assert_ne!(j.status(JOB, "revoke"), "completed");
    let why = j.meta(JOB, "revoke", "revoke_deferred").unwrap();
    assert!(why.contains("drain window runs 1440 minutes"), "{why}");
    assert!(why.contains(READER_ADVANCE), "{why}");

    // The window has passed, but a probe presented the old value twenty
    // minutes ago: held, the caller named, by the slot's own name.
    s.seed(
        PROMOTED_AT,
        &(Utc::now() - chrono::Duration::days(2)).to_rfc3339(),
    );
    g.with("jobs", |port| {
        port.extra_rows.push(row(
            Presented::ReaderPrevious,
            Utc::now() - chrono::Duration::minutes(20),
        ))
    });
    rtick(&h).await.expect("deferred");
    let why = j.meta(JOB, "revoke", "revoke_deferred").unwrap();
    assert!(
        why.contains("192.0.2.15") && why.contains("still presents reader.previous"),
        "{why}"
    );
    assert_eq!(s.get("previous").as_deref(), Some(OLD_READER));

    // That probe's last use is two days old; and a caller still on the
    // ESTATE token's previous is another credential's business entirely.
    g.with("jobs", |port| {
        port.extra_rows = vec![
            row(
                Presented::ReaderPrevious,
                Utc::now() - chrono::Duration::days(2),
            ),
            row(
                Presented::Previous,
                Utc::now() - chrono::Duration::minutes(5),
            ),
            row(
                Presented::ReaderCurrent,
                Utc::now() - chrono::Duration::minutes(1),
            ),
        ]
    });
    rtick(&h).await.expect("revoked");
    assert_eq!(j.status(JOB, "revoke"), "completed");
    assert_eq!(s.get("previous"), None, "previous blanked");
    assert_eq!(s.get("current").as_deref(), Some(value.as_str()));
    let revoked = j.meta(JOB, "revoke", "revoked").unwrap();
    assert!(revoked.contains("reader.previous"), "{revoked}");

    // Every tally and every window was opened with the estate token, and
    // never with a reader value — which the gate would have refused, and
    // which would have tallied the dispatcher itself as a reader.
    for route in ["misses", "window"] {
        let presented = g.asked(route);
        assert!(!presented.is_empty(), "{route} was read");
        assert!(
            presented.iter().all(|p| p == ESTATE),
            "{route} was opened with something other than the estate token"
        );
    }
    let record = j.everything_written();
    for secret in [value.as_str(), OLD_READER, ESTATE] {
        assert!(!record.contains(secret), "a value reached the record");
    }
}

/// The estate token is never required for the dispatcher's real work —
/// but a drain with no token to open a tally has no evidence, and no
/// evidence is not a pass: the old value stays accepted, and the step
/// says what is missing.
#[tokio::test]
async fn with_no_estate_token_the_reader_drain_holds_and_names_what_is_missing() {
    let j = reader_jobs(&[(JOB, reader_packet("2026-10-07T01:00:00Z"))], true).await;
    let s = Arc::new(Store::default());
    s.seed("current", "the-new-reader-value-this-packet-promoted-x");
    s.seed("current.minted-for", JOB);
    s.seed("previous", OLD_READER);
    s.seed(
        PROMOTED_AT,
        &(Utc::now() - chrono::Duration::days(2)).to_rfc3339(),
    );
    let g = Gates::new(&ROSTER);
    g.refresh(&s);
    {
        let mut p = j.packets.lock().unwrap();
        let steps = &mut p.get_mut(JOB).unwrap().steps;
        for slug in ["issue", "install", "verify"] {
            steps.get_mut(slug).unwrap().status = "completed".into();
        }
        steps.get_mut("revoke").unwrap().status = "ready".into();
    }
    let h = rhandler(&j, &s, &g, None);
    rtick(&h).await.expect("deferred");
    assert_ne!(j.status(JOB, "revoke"), "completed");
    assert_eq!(s.get("previous").as_deref(), Some(OLD_READER));
    let why = j.meta(JOB, "revoke", "revoke_deferred").unwrap();
    assert!(why.contains("holds no estate machine token"), "{why}");
    assert!(g.asked("misses").is_empty(), "nothing was presented");

    // The same state with the token: the drain is judged, and is clean.
    let h = rhandler(&j, &s, &g, Some(ESTATE));
    rtick(&h).await.expect("revoked");
    assert_eq!(j.status(JOB, "revoke"), "completed");
    assert_eq!(s.get("previous"), None);
}

// ----- the diagnostic judgement, by slot set -----

#[test]
fn the_drain_names_the_slot_it_waits_out() {
    let now = Utc::now();
    let window = chrono::Duration::days(1);
    let tally = |presented: Presented, minutes: i64| {
        let mut m = misses("dispatcher", |_| {}).1;
        if let GateRead::Answered(m) = &mut m {
            m.rows
                .push(row(presented, now - chrono::Duration::minutes(minutes)));
        }
        vec![("dispatcher".to_string(), m)]
    };
    // A reader.previous inside the window holds the reader's drain and
    // not the estate's; a previous, the other way round.
    let held = judge_drain_of(
        &tally(Presented::ReaderPrevious, 20),
        now,
        window,
        &[],
        Presented::ReaderPrevious,
    )
    .expect_err("held");
    assert!(
        held.iter()
            .any(|h| h.contains("still presents reader.previous")),
        "{held:?}"
    );
    assert!(judge_drain(&tally(Presented::ReaderPrevious, 20), now, window, &[]).is_ok());
    assert!(
        judge_drain_of(
            &tally(Presented::Previous, 20),
            now,
            window,
            &[],
            Presented::ReaderPrevious
        )
        .is_ok()
    );
    assert!(judge_drain(&tally(Presented::Previous, 20), now, window, &[]).is_err());
    // Outside the window it has aged out, as an estate row does.
    let clean = judge_drain_of(
        &tally(Presented::ReaderPrevious, 60 * 30),
        now,
        window,
        &[],
        Presented::ReaderPrevious,
    )
    .expect("clean");
    assert!(
        clean.contains("no port recorded a reader.previous match"),
        "{clean}"
    );
}

// ----- the HTTP adapter: one token header, and it is the value asked about -----

/// THE TRAP this car was sent to look for: the gate refuses a
/// probe-reader value presented beside any other machine-token value
/// (400, review 177b4976 N1), and the dispatcher's ordinary client
/// stamps the estate token on everything it sends. The gate reads go
/// through `LocalGates`, whose client holds the presented value ALONE —
/// so the ask arrives with one `x-boss-machine-token`, and the real gate
/// names it `reader.<slot>` in every mode, with the estate token mounted
/// beside it.
#[tokio::test]
async fn a_gate_ask_carries_the_presented_value_as_the_only_machine_token() {
    use boss_core::machine_gate::gated;
    // What arrives: a server that answers with the header values it saw.
    let seen: Arc<Mutex<Vec<Vec<String>>>> = Default::default();
    let capture = {
        let seen = seen.clone();
        axum::Router::new().fallback(move |headers: axum::http::HeaderMap| {
            let seen = seen.clone();
            async move {
                seen.lock().unwrap().push(
                    headers
                        .get_all(boss_core::machine_token::HEADER)
                        .iter()
                        .map(|v| v.to_str().unwrap_or("").to_string())
                        .collect(),
                );
                axum::Json(json!({"service":"jobs","mode":"report","matched":"none",
                    "degraded":false}))
            }
        })
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let captured = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, capture).await.unwrap() });
    let gates = LocalGates::new(vec![("jobs".into(), format!("http://{captured}"))]);
    assert!(matches!(
        gates.accepts("jobs", "rd-next").await,
        GateRead::Answered(_)
    ));
    assert_eq!(
        *seen.lock().unwrap(),
        vec![vec!["rd-next".to_string()]],
        "exactly one machine-token value, and it is the one asked about"
    );

    // What the real gate makes of it, in every mode.
    for mode in [Mode::Off, Mode::Report, Mode::Enforce] {
        let gate = Arc::new(MachineGate::new(
            "jobs",
            &[],
            Reading::new(mode, Slots::new(Some(ESTATE.into()), None, None)).with_reader(
                Slots::new(
                    Some("rd-current".into()),
                    Some("rd-next".into()),
                    Some("rd-previous".into()),
                ),
            ),
        ));
        let app = gated(axum::Router::new(), gate)
            .into_make_service_with_connect_info::<std::net::SocketAddr>();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let served = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let gates = LocalGates::new(vec![("jobs".into(), format!("http://{served}"))]);
        for (value, want) in [
            ("rd-next", "reader.next"),
            ("rd-current", "reader.current"),
            ("rd-previous", "reader.previous"),
            (ESTATE, "current"),
        ] {
            match gates.accepts("jobs", value).await {
                GateRead::Answered(a) => assert_eq!(a.matched, want, "{mode:?}"),
                other => panic!("{mode:?} {want}: {other:?}"),
            }
        }
        // The tally answers the estate token and refuses a reader value,
        // which is why the drain is read as the estate.
        assert!(
            matches!(gates.misses("jobs", "rd-current").await, GateRead::Failed(e) if e.contains("401")),
            "{mode:?}"
        );
        assert!(matches!(
            gates.misses("jobs", ESTATE).await,
            GateRead::Answered(_)
        ));
    }
}

/// Every port a recorded probe can read (`infra/forge/sor-ports.env`) is
/// a port the rotation verifies on, or one that mounts no machine gate
/// at all and so refuses no probe — the gateway, today.
#[test]
fn every_gated_port_a_probe_reads_is_in_the_verify_roster() {
    let table =
        std::fs::read_to_string(boss_testing::repo_root().join("infra/forge/sor-ports.env"))
            .expect("the probe's port table");
    let roster = LocalGates::from_ports().roster();
    let names: Vec<&str> = table
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once('=').map(|(name, _)| name))
        .collect();
    assert!(names.len() >= 10, "the table was read: {names:?}");
    let ungated: Vec<&str> = names
        .iter()
        .copied()
        .filter(|name| !roster.iter().any(|r| r == name))
        .collect();
    for name in &ungated {
        assert!(
            !boss_core::machine_gate::is_gated(name),
            "{name} mounts a machine gate, is read by probes, and is not verified by the rotation"
        );
    }
    assert_eq!(
        ungated,
        vec!["gateway"],
        "the one probe-read port with no gate"
    );
}

// The gate window a rotation leaves behind, against real gated routers
// (backlog 89fc511e) — a child of this module for its fixtures.
mod window;
