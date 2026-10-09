//! The self-issued machine token rotation (design 6805c764, car 3),
//! driven against an in-memory Secret store, gates that see a Secret
//! only when the test says kubelet refreshed them, and a stateful jobs
//! API — so every firing reads what the one before it wrote, and a
//! value that reached any record would be found there.

use super::*;
use crate::handlers::credential_issuer::{SecretData, WriteAt};
use boss_core::machine_gate::{MachineGate, MissKey, MissRow, Reading, Slots};
use std::collections::BTreeMap;
use std::sync::Mutex;

const PRIMARY: &str = "boss";
const MIRROR: &str = "boss-dev";
const SECRET: &str = "boss-machine-token";
const CREDENTIAL: &str = "boss-machine-token";
const JOB: &str = "2710c8fc-0000-4000-8000-000000000001";
const OTHER: &str = "2710c8fc-0000-4000-8000-000000000002";

// ----- the Secret store -----

/// Versioned like the API server's resourceVersion: every write bumps
/// the Secret's version, and a conditional write at a stale one moves
/// nothing. Only whole-Secret reads are served — a key-by-key read is
/// counted, and the handler must make none (review 70d449f9, N5).
#[derive(Default)]
struct FakeSecrets {
    map: Mutex<BTreeMap<String, String>>,
    versions: Mutex<BTreeMap<String, u64>>,
    key_reads: Mutex<usize>,
    /// A concurrent firing of the same packet, landing its own staged
    /// value between this firing's read and its conditional write.
    races_with: Mutex<Option<String>>,
    /// The next write to this namespace's Secret fails once: a firing
    /// that stops part way through staging (review fd151a97, D4).
    fail_once_in: Mutex<Option<String>>,
    /// (reads of the PRIMARY still to serve, what then lands): once that
    /// many have been served, the next store call of any kind is preceded
    /// by it — a write placed exactly between two of a firing's own reads
    /// and writes (review 34313729, P6).
    after_primary_reads: Mutex<Option<(usize, OnMisses)>>,
    /// (namespace, the store's error): every whole read of that
    /// namespace's Secret fails with it — or, with no error, answers that
    /// the Secret does not exist (backlog 50c0d155).
    unreadable: Mutex<Option<(String, Option<String>)>>,
}

impl FakeSecrets {
    fn store_call(&self, reading: Option<&str>) {
        let lands = {
            let mut armed = self.after_primary_reads.lock().unwrap();
            match armed.as_mut() {
                Some((0, _)) => armed.take().map(|(_, lands)| lands),
                Some((left, _)) if reading == Some(PRIMARY) => {
                    *left -= 1;
                    None
                }
                _ => None,
            }
        };
        if let Some(lands) = lands {
            lands();
        }
    }
    fn bump(&self, ns: &str, name: &str) {
        *self
            .versions
            .lock()
            .unwrap()
            .entry(format!("{ns}/{name}"))
            .or_default() += 1;
    }
    fn version(&self, ns: &str, name: &str) -> String {
        self.versions
            .lock()
            .unwrap()
            .get(&format!("{ns}/{name}"))
            .copied()
            .unwrap_or_default()
            .to_string()
    }
    fn get(&self, ns: &str, key: &str) -> Option<String> {
        self.map
            .lock()
            .unwrap()
            .get(&format!("{ns}/{SECRET}/{key}"))
            .cloned()
            .filter(|v| !v.is_empty())
    }
    fn seed(&self, ns: &str, key: &str, value: &str) {
        self.map
            .lock()
            .unwrap()
            .insert(format!("{ns}/{SECRET}/{key}"), value.to_string());
    }
    fn values(&self) -> Vec<String> {
        self.map
            .lock()
            .unwrap()
            .iter()
            .filter(|(k, v)| SLOTS.iter().any(|s| k.ends_with(&format!("/{s}"))) && !v.is_empty())
            .map(|(_, v)| v.clone())
            .collect()
    }
}

#[async_trait]
impl SecretStore for FakeSecrets {
    async fn read_key(&self, ns: &str, name: &str, key: &str) -> Result<Option<String>, String> {
        *self.key_reads.lock().unwrap() += 1;
        Ok(self
            .map
            .lock()
            .unwrap()
            .get(&format!("{ns}/{name}/{key}"))
            .cloned())
    }
    async fn write_key(&self, ns: &str, name: &str, key: &str, value: &str) -> Result<(), String> {
        if name != SECRET {
            return Err(format!("the broker is granted no Secret {ns}/{name}"));
        }
        {
            let mut fail = self.fail_once_in.lock().unwrap();
            if fail.as_deref() == Some(ns) {
                *fail = None;
                return Err(format!("PATCH {ns}/{name}: the API server did not answer"));
            }
        }
        self.map
            .lock()
            .unwrap()
            .insert(format!("{ns}/{name}/{key}"), value.to_string());
        self.bump(ns, name);
        Ok(())
    }
    async fn read_secret(&self, ns: &str, name: &str) -> Result<Option<SecretData>, String> {
        self.store_call(Some(ns));
        if let Some((dark, failure)) = self.unreadable.lock().unwrap().clone()
            && dark == ns
        {
            return failure.map_or(Ok(None), Err);
        }
        let prefix = format!("{ns}/{name}/");
        let data = self
            .map
            .lock()
            .unwrap()
            .iter()
            .filter_map(|(k, v)| Some((k.strip_prefix(&prefix)?.to_string(), v.clone())))
            .collect();
        Ok(Some(SecretData {
            uid: "self-issued-fixture".into(),
            version: self.version(ns, name),
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
        self.store_call(None);
        if let Some(theirs) = self.races_with.lock().unwrap().take() {
            // The other firing's own conditional write lands first.
            for n in [PRIMARY, MIRROR] {
                self.seed(n, "next", &theirs);
                self.seed(n, "next.minted-for", JOB);
                self.bump(n, name);
            }
        }
        if self.version(ns, name) != version {
            return Ok(WriteAt::Moved);
        }
        self.write_keys(ns, name, entries).await?;
        Ok(WriteAt::Written)
    }
}

// ----- the gates: the REAL machine gate, behind a fake port -----

/// One port. The gate is `boss_core::machine_gate::MachineGate` itself,
/// so its mode, its slots and when its tally starts over are the gate's
/// own behaviour, not a model of it (review 70d449f9, B2). A test adds
/// tally rows the real gate could only record at the moment they
/// happen, so a row can be dated.
struct FakeGate {
    real: Arc<MachineGate>,
    served: bool,
    mode: Mode,
    slots: [Option<String>; 3],
    extra_rows: Vec<MissRow>,
    overflow: u64,
    facts: Vec<boss_core::event::Event>,
    rx: tokio::sync::mpsc::Receiver<boss_core::event::Event>,
}

impl FakeGate {
    fn reread(&self) {
        let [c, n, p] = self.slots.clone();
        self.real
            .observe(Reading::new(self.mode, Slots::new(c, n, p)));
    }

    fn collect(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            self.facts.push(event);
        }
    }

    fn restart(&mut self, since: DateTime<Utc>, clean_end: bool) {
        use boss_core::gate_evidence::{Evidence, Fact, Gate};
        self.collect();
        let old = self.real.misses();
        if clean_end {
            self.facts.push(boss_core::event::Event::new(old.service.clone(), Gate::MachineGate.kind(Fact::RecordingEnded), json!({"service":old.service,"instance":old.evidence.instance,"clean":true,"lost":0,"unstated":0}), since - chrono::Duration::minutes(1)));
        }
        let (evidence, mut rx) = Evidence::channel(Gate::MachineGate, &old.service);
        let [current, next, previous] = self.slots.clone();
        self.real = Arc::new(
            MachineGate::starting_at(
                &old.service,
                &[],
                Reading::new(self.mode, Slots::new(current, next, previous)),
                since,
            )
            .with_evidence(evidence),
        );
        let mut began = rx.try_recv().unwrap();
        began.timestamp = since; // Explicit synthetic historical process start.
        self.facts.push(began);
        self.rx = rx;
    }
}

/// Runs once, on the first tally read: something that lands while the
/// drain is reading the gates, before it blanks (review 1718e070, G3).
type OnMisses = Box<dyn FnOnce() + Send>;

struct FakeGates {
    gates: Mutex<BTreeMap<String, FakeGate>>,
    on_misses: Mutex<Option<OnMisses>>,
    on_accepts: Mutex<Option<OnMisses>>,
    window_change: Mutex<Option<fn(&mut boss_core::gate_window::JoinedWindow)>>,
}

impl FakeGates {
    /// Every port in `report`, each gate recording since three days ago.
    fn new(served: &[&str], not_served: &[&str]) -> Arc<Self> {
        Self::in_mode(Mode::Report, served, not_served)
    }

    fn in_mode(mode: Mode, served: &[&str], not_served: &[&str]) -> Arc<Self> {
        let since = Utc::now() - chrono::Duration::days(3);
        let gate = |name: &str, served: bool| {
            let (evidence, mut rx) = boss_core::gate_evidence::Evidence::channel(
                boss_core::gate_evidence::Gate::MachineGate,
                name,
            );
            let real = Arc::new(
                MachineGate::starting_at(name, &[], Reading::new(mode, Slots::default()), since)
                    .with_evidence(evidence),
            );
            let mut began = rx.try_recv().unwrap();
            began.timestamp = since; // The fixture declares a historical first statement.
            FakeGate {
                real,
                served,
                mode,
                slots: [None, None, None],
                extra_rows: Vec::new(),
                overflow: 0,
                facts: vec![began],
                rx,
            }
        };
        Arc::new(Self {
            gates: Mutex::new(
                served
                    .iter()
                    .map(|s| (s.to_string(), gate(s, true)))
                    .chain(not_served.iter().map(|s| (s.to_string(), gate(s, false))))
                    .collect(),
            ),
            on_misses: Mutex::new(None),
            on_accepts: Mutex::new(None),
            window_change: Mutex::new(None),
        })
    }

    /// Kubelet refreshed every pod's mount: each gate now reads the
    /// primary Secret's slots.
    fn refresh(&self, secrets: &FakeSecrets) {
        let slots = SLOTS.map(|s| secrets.get(PRIMARY, s));
        for gate in self.gates.lock().unwrap().values_mut() {
            gate.slots = slots.clone();
            gate.reread();
        }
    }

    /// The mode file changed, and every gate re-read it.
    fn set_mode(&self, mode: Mode) {
        for gate in self.gates.lock().unwrap().values_mut() {
            gate.mode = mode;
            gate.reread();
        }
    }

    fn with(&self, service: &str, f: impl FnOnce(&mut FakeGate)) {
        let mut gates = self.gates.lock().unwrap();
        let gate = gates.get_mut(service).expect("a gate");
        f(gate);
        gate.reread();
    }

    async fn window_snapshot(
        &self,
        presented: &str,
        minutes: chrono::Duration,
    ) -> boss_core::gate_window::JoinedWindow {
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
        for gate in self.gates.lock().unwrap().values_mut() {
            gate.collect();
            facts.extend(gate.facts.iter().cloned());
        }
        facts.sort_by_key(|event| (event.timestamp, event.id));
        let now = Utc::now();
        boss_core::gate_window::join_window(
            boss_core::gate_evidence::Gate::MachineGate,
            &roster,
            now - minutes,
            now,
            Ok(facts),
            reads,
        )
    }
}

#[async_trait]
impl GateReader for FakeGates {
    async fn window(
        &self,
        presented: &str,
        hours: i64,
    ) -> Result<boss_core::gate_window::JoinedWindow, String> {
        let mut snapshot = self
            .window_snapshot(presented, chrono::Duration::hours(hours))
            .await;
        if let Some(change) = self.window_change.lock().unwrap().take() {
            change(&mut snapshot);
        }
        Ok(snapshot)
    }
    fn roster(&self) -> Vec<String> {
        self.gates.lock().unwrap().keys().cloned().collect()
    }
    async fn accepts(&self, service: &str, presented: &str) -> GateRead<Accepts> {
        let hook = self.on_accepts.lock().unwrap().take();
        if let Some(hook) = hook {
            hook();
        }
        let gates = self.gates.lock().unwrap();
        let gate = &gates[service];
        if !gate.served {
            return GateRead::NotServed;
        }
        GateRead::Answered(gate.real.accepts(Some(presented)))
    }
    async fn misses(&self, service: &str, presented: &str) -> GateRead<Misses> {
        let hook = self.on_misses.lock().unwrap().take();
        if let Some(hook) = hook {
            hook();
        }
        let gates = self.gates.lock().unwrap();
        let gate = &gates[service];
        if !gate.served {
            return GateRead::NotServed;
        }
        if gate.real.reading().slots.matched(Some(presented)).is_none() {
            return GateRead::Failed("answered 401: the tally wants an accepted token".into());
        }
        let mut m = gate.real.misses();
        m.rows.extend(gate.extra_rows.iter().cloned());
        m.overflow += gate.overflow;
        GateRead::Answered(m)
    }
}

fn previous_row(last_seen: DateTime<Utc>) -> MissRow {
    MissRow {
        key: MissKey {
            peer: "10.20.0.30".into(),
            user: "automation:forge-converge".into(),
            method: "POST".into(),
            route: "/api/jobs".into(),
            presented: Presented::Previous,
        },
        count: 3,
        first_seen: last_seen,
        last_seen,
    }
}

// ----- the jobs API: rotation packets, stateful -----

#[derive(Clone)]
struct StubStep {
    status: String,
    metadata: serde_json::Map<String, JsonValue>,
}

#[derive(Clone)]
struct StubJob {
    opened_at: String,
    /// `open`, `closed` or `cancelled`, as the jobs API answers it.
    status: String,
    /// `job.metadata.abandoned = "true"`: the workflow's `abandoned`
    /// terminal closed it.
    abandoned: bool,
    /// The workflow kind and the credential the packet is about.
    kind: String,
    about: String,
    steps: BTreeMap<String, StubStep>,
}

type Packets = Arc<Mutex<BTreeMap<String, StubJob>>>;
type Captured = Arc<Mutex<Vec<(String, JsonValue)>>>;

struct Jobs {
    url: String,
    packets: Packets,
    writes: Captured,
    rotations: Captured,
}

const SLUGS: [&str; 7] = [
    "requested",
    "scope",
    "issue",
    "install",
    "verify",
    "delivered",
    "revoke",
];

/// A packet closed on its `abandoned` terminal after installing, and
/// after its verify had promoted at least one copy.
fn abandoned_packet() -> StubJob {
    let mut p = packet("2026-09-29T00:00:00Z");
    p.status = "closed".into();
    p.abandoned = true;
    for slug in ["issue", "install"] {
        p.steps.get_mut(slug).unwrap().status = "completed".into();
    }
    p
}

fn packet(opened_at: &str) -> StubJob {
    StubJob {
        opened_at: opened_at.to_string(),
        status: "open".into(),
        abandoned: false,
        kind: "rotate-a-credential".into(),
        about: CREDENTIAL.into(),
        steps: SLUGS
            .iter()
            .map(|s| {
                let status = match *s {
                    "requested" | "scope" => "completed",
                    "issue" => "ready",
                    _ => "pending",
                };
                (
                    s.to_string(),
                    StubStep {
                        status: status.into(),
                        metadata: Default::default(),
                    },
                )
            })
            .collect(),
    }
}

fn job_json(id: &str, job: &StubJob) -> JsonValue {
    let steps: Vec<JsonValue> = job
        .steps
        .iter()
        .map(|(slug, s)| {
            json!({"id": format!("step-{slug}"), "spec_slug": slug, "status": s.status,
                   "metadata": s.metadata})
        })
        .collect();
    let metadata = if job.abandoned {
        json!({"abandoned": "true"})
    } else {
        json!({})
    };
    json!({"id": id, "kind": job.kind, "status": job.status,
           "opened_at": job.opened_at, "metadata": metadata,
           "subject": {"id": job.about, "subject_kind": "custom"}, "steps": steps})
}

impl Jobs {
    fn status(&self, job: &str, slug: &str) -> String {
        self.packets.lock().unwrap()[job].steps[slug].status.clone()
    }
    fn meta(&self, job: &str, slug: &str, key: &str) -> Option<String> {
        self.packets.lock().unwrap()[job].steps[slug]
            .metadata
            .get(key)
            .and_then(|v| v.as_str())
            .map(str::to_string)
    }
    fn phases(&self) -> Vec<String> {
        self.rotations
            .lock()
            .unwrap()
            .iter()
            .map(|(p, _)| p.clone())
            .collect()
    }
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

async fn jobs(packets: &[(&str, StubJob)]) -> Jobs {
    use axum::extract::{Path, Query};
    use axum::response::IntoResponse;
    use axum::{Router, routing::get, routing::patch, routing::post, routing::put};

    let packets: Packets = Arc::new(Mutex::new(
        packets
            .iter()
            .map(|(id, j)| (id.to_string(), j.clone()))
            .collect(),
    ));
    let writes: Captured = Default::default();
    let rotations: Captured = Default::default();
    let slug_of = |sid: &str| sid.trim_start_matches("step-").to_string();
    let (p_get, p_list, p_put, p_merge) = (
        packets.clone(),
        packets.clone(),
        packets.clone(),
        packets.clone(),
    );
    let (w_put, w_merge, rot) = (writes.clone(), writes.clone(), rotations.clone());
    let app = Router::new()
        .route(
            "/api/jobs",
            get(move |Query(q): Query<HashMap<String, String>>| {
                let p = p_list.clone();
                async move {
                    assert_eq!(
                        q.get("kind").map(String::as_str),
                        Some("rotate-a-credential")
                    );
                    let rows: Vec<JsonValue> = p
                        .lock()
                        .unwrap()
                        .iter()
                        .filter(|(_, j)| q.get("status").is_none_or(|s| *s == j.status))
                        .map(|(id, j)| job_json(id, j))
                        .collect();
                    axum::Json(json!({"total": rows.len(), "data": rows}))
                }
            }),
        )
        .route(
            "/api/jobs/{id}",
            get(move |Path(id): Path<String>| {
                let p = p_get.clone();
                async move {
                    let p = p.lock().unwrap();
                    match p.get(&id) {
                        Some(j) => axum::Json(job_json(&id, j)).into_response(),
                        None => axum::http::StatusCode::NOT_FOUND.into_response(),
                    }
                }
            }),
        )
        .route(
            "/api/jobs/{id}/steps/{step_id}",
            put(
                move |Path((id, sid)): Path<(String, String)>,
                      axum::Json(body): axum::Json<JsonValue>| {
                    let (p, w) = (p_put.clone(), w_put.clone());
                    async move {
                        if let Some(refused) =
                            crate::handlers::listing_stub::end_state_step_put(&id, &sid, &body)
                        {
                            return refused;
                        }
                        if let Some(status) = body.get("status").and_then(|v| v.as_str()) {
                            let mut p = p.lock().unwrap();
                            let step = p.get_mut(&id).unwrap().steps.get_mut(&slug_of(&sid));
                            step.unwrap().status = status.to_string();
                        }
                        w.lock().unwrap().push((format!("{id}/{sid}"), body));
                        axum::Json(json!({"ok": true})).into_response()
                    }
                },
            ),
        )
        .route(
            "/api/jobs/{id}/steps/{step_id}/metadata",
            patch(
                move |Path((id, sid)): Path<(String, String)>,
                      axum::Json(body): axum::Json<JsonValue>| {
                    let (p, w) = (p_merge.clone(), w_merge.clone());
                    async move {
                        if let Some(obj) = body.as_object() {
                            let mut p = p.lock().unwrap();
                            let step = p.get_mut(&id).unwrap().steps.get_mut(&slug_of(&sid));
                            let step = step.unwrap();
                            assert_ne!(step.status, "completed", "no write to a completed step");
                            for (k, v) in obj {
                                step.metadata.insert(k.clone(), v.clone());
                            }
                        }
                        w.lock()
                            .unwrap()
                            .push((format!("{id}/{sid}/metadata"), body));
                        axum::Json(json!({"ok": true}))
                    }
                },
            ),
        )
        .route(
            "/api/credentials/{id}/rotation/{phase}",
            post(
                move |Path((id, phase)): Path<(String, String)>,
                      axum::Json(body): axum::Json<JsonValue>| {
                    let rot = rot.clone();
                    async move {
                        rot.lock().unwrap().push((format!("{id}/{phase}"), body));
                        axum::Json(json!({"recorded": true}))
                    }
                },
            ),
        )
        .route(
            "/api/credentials/{id}",
            get(move |Path(id): Path<String>| async move {
                if id == CREDENTIAL {
                    axum::Json(json!({"id": id, "kind": "machine-token"})).into_response()
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
        packets,
        writes,
        rotations,
    }
}

// ----- wiring -----

fn args(extra: &[(&str, &str)]) -> Vec<(String, Value)> {
    [
        ("secret_namespace", PRIMARY),
        ("secret_name", SECRET),
        ("also_in_namespaces", MIRROR),
        ("credential_id", CREDENTIAL),
        ("drain_minutes", "1440"),
    ]
    .iter()
    .chain(extra.iter())
    .map(|(k, v)| (k.to_string(), Value::String((*v).into())))
    .collect()
}

fn scope_ctx(job: &str) -> InvocationContext {
    InvocationContext {
        event_timestamp: None,
        rule_name: "broker-rotates-the-machine-token".into(),
        triggering_event_id: "evt-scope".into(),
        triggering_topic: "step.done.credential-rotation".into(),
        event_payload: json!({
            "job_id": job,
            "step_id": "step-scope",
            "kind": "credential-rotation",
            "subject_kind": "custom",
            "subject_id": CREDENTIAL,
            "metadata": {"credential": CREDENTIAL},
        }),
    }
}

fn tick_ctx() -> InvocationContext {
    InvocationContext {
        event_timestamp: None,
        rule_name: ADVANCE_RULE.into(),
        triggering_event_id: "clock-tick:2026-09-30T02:15:00+00:00".into(),
        triggering_topic: "clock.tick".into(),
        event_payload: json!({"_day": "2026-09-30", "_at": "2026-09-30T02:15:00Z"}),
    }
}

const ROSTER: [&str; 3] = ["dispatcher", "jobs", "policy"];

fn handler(j: &Jobs, s: &Arc<FakeSecrets>, g: &Arc<FakeGates>) -> Arc<CredentialRotateSelfIssued> {
    CredentialRotateSelfIssued::with_poll(
        j.url.clone(),
        s.clone(),
        g.clone(),
        GatePoll {
            attempts: 1,
            interval: Duration::ZERO,
        },
    )
}

async fn fire(h: &CredentialRotateSelfIssued, ctx: &InvocationContext, a: &[(String, Value)]) {
    h.invoke(a, ctx).await.expect("the firing succeeds");
}

// ----- the promotion: every copy judged before any is written -----

const COMPETING: &str = "competing-next-value-from-another-packet";
const RESTAGED: &str = "a-value-the-gates-were-never-asked-about";
/// The sentence `defer` ends every note with. A stop no later firing can
/// get past must not carry it (review 34313729, F1).
const RESUMES: &str = "no hand is needed";

type Rotation = (
    Jobs,
    Arc<FakeSecrets>,
    Arc<FakeGates>,
    Arc<CredentialRotateSelfIssued>,
);
type Snapshot = (BTreeMap<String, String>, BTreeMap<String, u64>);

/// This packet's value staged in `next` of both copies by its scope
/// firing, and read by every gate.
async fn staged() -> Rotation {
    let j = jobs(&[(JOB, packet("2026-09-30T01:00:00Z"))]).await;
    let s = Arc::new(FakeSecrets::default());
    let g = FakeGates::new(&ROSTER, &[]);
    let h = handler(&j, &s, &g);
    fire(&h, &scope_ctx(JOB), &args(&[])).await;
    g.refresh(&s);
    (j, s, g, h)
}

fn snapshot(s: &FakeSecrets) -> Snapshot {
    (
        s.map.lock().unwrap().clone(),
        s.versions.lock().unwrap().clone(),
    )
}

/// Another writer replaces `next` in `copies` WHILE the gates are being
/// asked: its value when `value` is given, its origin always, and each
/// Secret's version bumped as a real write bumps it. The promotion's
/// conditional write therefore sees a fresh version and passes; only
/// judging the reread `next` can stop it (backlog 3f44a70e). Returns what
/// that writer left, to hold every slot and every version to.
fn replace_next_while_the_gates_are_read(
    g: &FakeGates,
    s: &Arc<FakeSecrets>,
    copies: &'static [&'static str],
    value: Option<&'static str>,
    origin: &'static str,
) -> Arc<Mutex<Option<Snapshot>>> {
    let left = Arc::new(Mutex::new(None));
    let (observed, shared) = (left.clone(), s.clone());
    *g.on_accepts.lock().unwrap() = Some(Box::new(move || {
        for copy in copies {
            if let Some(value) = value {
                shared.seed(copy, "next", value);
            }
            shared.seed(copy, "next.minted-for", origin);
            shared.bump(copy, SECRET);
        }
        *observed.lock().unwrap() = Some(snapshot(&shared));
    }));
    left
}

async fn tick(h: &CredentialRotateSelfIssued) -> Result<(), HandlerError> {
    h.invoke(&args(&[("phase", ADVANCE_PHASE)]), &tick_ctx())
        .await
}

/// No copy was written since `left` was taken, nothing was promoted, the
/// verify step is not done and no Verified fact was published.
fn assert_nothing_was_promoted(j: &Jobs, s: &FakeSecrets, left: &Snapshot) {
    assert_eq!(snapshot(s), *left, "a copy was written");
    for copy in [PRIMARY, MIRROR] {
        assert!(s.get(copy, "current").is_none(), "{copy} was promoted");
    }
    assert_ne!(j.status(JOB, "verify"), "completed");
    assert!(!j.phases().iter().any(|phase| phase.ends_with("/verified")));
}

/// The firing FAILED, and the verify step says why in `verify_refused`:
/// each of `copies` by path, whom its next names, that a hand is needed
/// and that nothing resumes. No note left on the step still promises a
/// resumption, and neither value is in anything recorded or returned.
fn assert_refused(
    j: &Jobs,
    result: &Result<(), HandlerError>,
    copies: &[&str],
    names: &str,
    mine: &str,
) {
    let error = result
        .as_ref()
        .expect_err("the firing must fail")
        .to_string();
    let refused = j
        .meta(JOB, "verify", "verify_refused")
        .expect("verify_refused is recorded");
    for copy in copies {
        let path = format!("{copy}/{SECRET}");
        assert!(refused.contains(&path), "{refused}");
        assert!(error.contains(&path), "{error}");
    }
    assert!(refused.contains(names), "{refused}");
    assert!(refused.contains("a hand is needed"), "{refused}");
    assert!(refused.contains("abandoned terminal"), "{refused}");
    assert!(!refused.contains(RESUMES), "{refused}");
    assert!(!refused.contains(ADVANCE_RULE), "{refused}");
    if let Some(deferred) = j.meta(JOB, "verify", "verify_deferred") {
        assert!(!deferred.contains(RESUMES), "{deferred}");
    }
    let record = format!("{}\n{error}", j.everything_written());
    for value in [mine, COMPETING, RESTAGED] {
        assert!(!record.contains(value), "the record carries a value");
    }
}

/// The packet's headline: both copies' next replaced by another packet
/// after the gates accepted this one's. Nothing is written, and the stop
/// is a REFUSAL — every later firing is the same refusal, so a note that
/// said the clock resumes it would be false (review 34313729, F1).
#[tokio::test]
async fn promotion_conserves_a_superseding_next_after_acceptance() {
    let (j, s, g, h) = staged().await;
    let mine = s.get(PRIMARY, "next").unwrap();
    let left =
        replace_next_while_the_gates_are_read(&g, &s, &[PRIMARY, MIRROR], Some(COMPETING), OTHER);
    let first = tick(&h).await;
    let left = left.lock().unwrap().clone().expect("the gates were asked");
    for copy in [PRIMARY, MIRROR] {
        assert_eq!(s.get(copy, "next").as_deref(), Some(COMPETING));
        assert_eq!(s.get(copy, "next.minted-for").as_deref(), Some(OTHER));
    }
    assert_nothing_was_promoted(&j, &s, &left);
    assert_refused(&j, &first, &[PRIMARY, MIRROR], OTHER, &mine);
    for _ in 0..2 {
        g.refresh(&s);
        let later = tick(&h).await;
        assert_nothing_was_promoted(&j, &s, &left);
        assert_refused(&j, &later, &[PRIMARY], OTHER, &mine);
    }
}

/// EVERY unpromoted copy is judged, not the primary alone: the mirror's
/// next replaced, the primary's still this packet's. Neither is promoted,
/// and it stays a recorded refusal on every later firing rather than a
/// note that never changes (review 34313729, F1 second shape, and M3).
#[tokio::test]
async fn promotion_refuses_when_only_the_mirror_was_superseded() {
    let (j, s, g, h) = staged().await;
    let mine = s.get(PRIMARY, "next").unwrap();
    let left = replace_next_while_the_gates_are_read(&g, &s, &[MIRROR], Some(COMPETING), OTHER);
    let first = tick(&h).await;
    let left = left.lock().unwrap().clone().expect("the gates were asked");
    assert_eq!(s.get(MIRROR, "next").as_deref(), Some(COMPETING));
    assert_eq!(s.get(PRIMARY, "next").as_deref(), Some(mine.as_str()));
    assert_nothing_was_promoted(&j, &s, &left);
    assert_refused(&j, &first, &[MIRROR], OTHER, &mine);
    g.refresh(&s);
    let later = tick(&h).await;
    assert_nothing_was_promoted(&j, &s, &left);
    assert_refused(&j, &later, &[MIRROR], OTHER, &mine);
    // The refusal is not sticky. A hand puts the mirror's next back, the
    // next firing promotes, and the step stops asking for a hand.
    s.seed(MIRROR, "next", &mine);
    s.seed(MIRROR, "next.minted-for", JOB);
    s.bump(MIRROR, SECRET);
    tick(&h).await.expect("promoted, and waiting on the gates");
    for copy in [PRIMARY, MIRROR] {
        assert_eq!(s.get(copy, "current").as_deref(), Some(mine.as_str()));
    }
    let refused = j.meta(JOB, "verify", "verify_refused").unwrap();
    assert!(refused.starts_with("cleared:"), "{refused}");
}

/// The candidate is a value AND its origin: the same bytes under another
/// packet's `next.minted-for` are not this packet's to promote (M1).
#[tokio::test]
async fn promotion_refuses_a_next_that_names_another_packet_over_the_same_value() {
    let (j, s, g, h) = staged().await;
    let mine = s.get(PRIMARY, "next").unwrap();
    let left = replace_next_while_the_gates_are_read(&g, &s, &[PRIMARY, MIRROR], None, OTHER);
    let first = tick(&h).await;
    let left = left.lock().unwrap().clone().expect("the gates were asked");
    assert_eq!(s.get(PRIMARY, "next").as_deref(), Some(mine.as_str()));
    assert_nothing_was_promoted(&j, &s, &left);
    assert_refused(&j, &first, &[PRIMARY, MIRROR], OTHER, &mine);
}

/// A next still naming THIS packet but holding a value the gates were
/// never asked about is not promoted either, and that one is a defer
/// that does resume: the next firing asks the gates about the value next
/// holds now and promotes it.
#[tokio::test]
async fn promotion_defers_to_a_value_this_packet_restaged_and_then_promotes_it() {
    let (j, s, g, h) = staged().await;
    let left =
        replace_next_while_the_gates_are_read(&g, &s, &[PRIMARY, MIRROR], Some(RESTAGED), JOB);
    tick(&h).await.expect("a defer acknowledges");
    let left = left.lock().unwrap().clone().expect("the gates were asked");
    assert_nothing_was_promoted(&j, &s, &left);
    let deferred = j.meta(JOB, "verify", "verify_deferred").unwrap();
    assert!(
        deferred.contains(&format!("{PRIMARY}/{SECRET}")),
        "{deferred}"
    );
    assert!(
        deferred.contains("the next firing asks the gates about the value next holds now"),
        "{deferred}"
    );
    assert!(deferred.contains(RESUMES), "{deferred}");
    assert!(!deferred.contains(RESTAGED), "the note carries a value");
    assert!(j.meta(JOB, "verify", "verify_refused").is_none());
    g.refresh(&s);
    let _ = tick(&h).await;
    for copy in [PRIMARY, MIRROR] {
        assert_eq!(s.get(copy, "current").as_deref(), Some(RESTAGED));
        assert_eq!(s.get(copy, "current.minted-for").as_deref(), Some(JOB));
    }
}

/// One copy differing stops BOTH — the mirror, written first, used to be
/// promoted before the primary was reread. Here only the primary took
/// this packet's other value: the first firing defers to it, and the
/// second, finding the mirror still on the old one, refuses, because no
/// firing brings two disagreeing copies of next together.
#[tokio::test]
async fn promotion_of_every_copy_stops_when_one_copy_holds_another_value() {
    let (j, s, g, h) = staged().await;
    let mine = s.get(PRIMARY, "next").unwrap();
    let left = replace_next_while_the_gates_are_read(&g, &s, &[PRIMARY], Some(RESTAGED), JOB);
    tick(&h).await.expect("a defer acknowledges");
    let left = left.lock().unwrap().clone().expect("the gates were asked");
    assert_eq!(s.get(PRIMARY, "next").as_deref(), Some(RESTAGED));
    assert_nothing_was_promoted(&j, &s, &left);
    assert!(
        j.meta(JOB, "verify", "verify_deferred")
            .unwrap()
            .contains("the next firing asks the gates about the value next holds now")
    );
    g.refresh(&s);
    let second = tick(&h).await;
    assert_nothing_was_promoted(&j, &s, &left);
    assert_refused(
        &j,
        &second,
        &[MIRROR],
        "another value than the primary's",
        &mine,
    );
}

/// The write is conditional on the version that was JUDGED, not on one
/// read again just before writing: the primary's next is replaced right
/// after the judge pass read it, and the primary's write is refused as
/// moved with the superseding next intact (review 34313729, P6 and M4).
/// The mirror, written first, IS promoted here — the half-applied
/// promotion main already had, and not this car's to redesign (F4).
#[tokio::test]
async fn promotion_writes_at_the_version_it_judged() {
    let (_j, s, _g, h) = staged().await;
    let shared = s.clone();
    *s.after_primary_reads.lock().unwrap() = Some((
        2,
        Box::new(move || {
            shared.seed(PRIMARY, "next", COMPETING);
            shared.seed(PRIMARY, "next.minted-for", OTHER);
            shared.bump(PRIMARY, SECRET);
        }),
    ));
    let first = tick(&h).await;
    assert!(
        s.after_primary_reads.lock().unwrap().is_none(),
        "the primary never moved"
    );
    assert!(first.unwrap_err().to_string().contains("moved"));
    assert_eq!(s.get(PRIMARY, "next").as_deref(), Some(COMPETING));
    assert_eq!(s.get(PRIMARY, "next.minted-for").as_deref(), Some(OTHER));
    assert!(s.get(PRIMARY, "current").is_none());
}

/// `previous` still holding a value in ONE copy refuses before ANY copy
/// is written: the mirror used to be promoted and the primary then
/// refused (review 34313729, F5).
#[tokio::test]
async fn promotion_refuses_over_an_undrained_previous_before_any_copy_is_written() {
    let (j, s, _g, h) = staged().await;
    s.seed(PRIMARY, "previous", "an-old-value-not-yet-drained");
    s.bump(PRIMARY, SECRET);
    let left = snapshot(&s);
    let first = tick(&h).await;
    assert!(first.unwrap_err().to_string().contains("previous holds"));
    assert_nothing_was_promoted(&j, &s, &left);
}

/// The same refusal when the replacement landed BETWEEN firings: verify
/// finds this packet's value in neither slot at its first read. The note
/// an earlier firing left, saying the clock resumes it, is cleared.
#[tokio::test]
async fn a_next_superseded_between_firings_is_a_recorded_refusal() {
    let (j, s, _g, h) = staged().await;
    let mine = s.get(PRIMARY, "next").unwrap();
    assert!(
        j.meta(JOB, "verify", "verify_deferred")
            .unwrap()
            .contains(RESUMES),
        "the scope firing left a defer for this control to clear"
    );
    for copy in [PRIMARY, MIRROR] {
        s.seed(copy, "next", COMPETING);
        s.seed(copy, "next.minted-for", OTHER);
        s.bump(copy, SECRET);
    }
    let left = snapshot(&s);
    let first = tick(&h).await;
    assert_nothing_was_promoted(&j, &s, &left);
    assert_refused(&j, &first, &[PRIMARY], OTHER, &mine);
}

// ----- verified: every named copy still holds the candidate (50c0d155) -----

/// This packet's value promoted to `current` of both copies by a clock
/// firing, and read as `current` by every gate: the state in which the
/// next firing used to record Verified from the gates and the primary
/// alone. Returns the value.
async fn promoted() -> (Rotation, String) {
    let (j, s, g, h) = staged().await;
    let mine = s.get(PRIMARY, "next").unwrap();
    tick(&h).await.expect("promoted, and waiting on the gates");
    for copy in [PRIMARY, MIRROR] {
        assert_eq!(s.get(copy, "current").as_deref(), Some(mine.as_str()));
        assert_eq!(s.get(copy, "current.minted-for").as_deref(), Some(JOB));
    }
    assert_ne!(j.status(JOB, "verify"), "completed");
    g.refresh(&s);
    ((j, s, g, h), mine)
}

/// Nothing said verified, nothing was revoked, and no copy was written
/// since `left` was taken.
fn assert_not_verified(j: &Jobs, s: &FakeSecrets, left: &Snapshot) {
    assert_eq!(snapshot(s), *left, "a copy was written");
    assert_ne!(j.status(JOB, "verify"), "completed");
    assert_ne!(j.status(JOB, "revoke"), "completed");
    assert!(j.meta(JOB, "verify", "verified").is_none());
    let phases = j.phases();
    assert!(
        !phases.iter().any(|p| p.ends_with("/verified")),
        "{phases:?}"
    );
    assert!(
        !phases.iter().any(|p| p.ends_with("/revoked")),
        "{phases:?}"
    );
}

/// The item's headline, on the ALREADY-PROMOTED path: the mirror's
/// current replaced (value and origin) after the promotion, the gates all
/// answering `current` for the primary's value. Verify used to complete —
/// it read the gates and the primary, and a mirror is read by callers
/// only, never by a gate. Now it is a recorded refusal naming the copy,
/// on every firing, and the first firing after the copy holds the
/// candidate again verifies.
#[tokio::test]
async fn verify_refuses_while_a_mirror_no_longer_holds_the_promoted_value() {
    let ((j, s, g, h), mine) = promoted().await;
    s.seed(MIRROR, "current", COMPETING);
    s.seed(MIRROR, "current.minted-for", OTHER);
    s.bump(MIRROR, SECRET);
    let left = snapshot(&s);
    for _ in 0..2 {
        g.refresh(&s);
        let result = tick(&h).await;
        assert_not_verified(&j, &s, &left);
        assert_refused(&j, &result, &[MIRROR], OTHER, &mine);
        let refused = j.meta(JOB, "verify", "verify_refused").unwrap();
        assert!(
            !refused.contains(&format!("{PRIMARY}/{SECRET} holds")),
            "the primary agrees and is not named as astray: {refused}"
        );
        assert!(
            refused.contains("nothing is recorded verified"),
            "{refused}"
        );
    }
    // The copy holds the candidate again: verified THEN, the refusal is
    // cleared, and what is recorded names every copy it judged.
    s.seed(MIRROR, "current", &mine);
    s.seed(MIRROR, "current.minted-for", JOB);
    s.bump(MIRROR, SECRET);
    tick(&h).await.expect("every copy agrees");
    assert_eq!(j.status(JOB, "verify"), "completed");
    let refused = j.meta(JOB, "verify", "verify_refused").unwrap();
    assert!(refused.starts_with("cleared:"), "{refused}");
    let verified = j.meta(JOB, "verify", "verified").unwrap();
    for copy in [PRIMARY, MIRROR] {
        assert!(verified.contains(&format!("{copy}/{SECRET}")), "{verified}");
    }
    let rotations = j.rotations.lock().unwrap().clone();
    let facts: Vec<&JsonValue> = rotations
        .iter()
        .filter(|(phase, _)| phase.ends_with("/verified"))
        .map(|(_, evidence)| evidence)
        .collect();
    assert_eq!(facts.len(), 1, "one Verified fact, at the agreeing firing");
    assert_eq!(
        facts[0]["current_held_in"],
        json!([format!("{PRIMARY}/{SECRET}"), format!("{MIRROR}/{SECRET}")])
    );
    let record = j.everything_written();
    for value in [mine.as_str(), COMPETING] {
        assert!(!record.contains(value), "the record carries a value");
    }
}

/// The candidate is a value AND its origin, in every copy: the same
/// bytes under another packet's name, another value under this packet's
/// name, and a blank current are each not this packet's promoted value.
#[tokio::test]
async fn verify_judges_each_copys_current_by_value_and_by_origin() {
    for (value, origin, names) in [
        (None, Some(OTHER), OTHER),
        (Some(COMPETING), None, JOB),
        (Some(""), Some(""), "nothing recorded"),
    ] {
        let ((j, s, _g, h), mine) = promoted().await;
        if let Some(value) = value {
            s.seed(MIRROR, "current", value);
        }
        if let Some(origin) = origin {
            s.seed(MIRROR, "current.minted-for", origin);
        }
        s.bump(MIRROR, SECRET);
        let left = snapshot(&s);
        let result = tick(&h).await;
        assert_not_verified(&j, &s, &left);
        assert_refused(&j, &result, &[MIRROR], names, &mine);
    }
}

/// The mirror replaced right after the promotion's own writes, inside
/// the firing that promoted: the copies are judged on that path too, and
/// before the gates' answer is, so the divergence is named at once
/// rather than behind "waiting for every gate".
#[tokio::test]
async fn verify_judges_the_copies_in_the_firing_that_promoted_too() {
    let (j, s, _g, h) = staged().await;
    let mine = s.get(PRIMARY, "next").unwrap();
    let shared = s.clone();
    // The primary is read three times before the copies are judged: at
    // verify's start, in the promotion's judge pass, and for the excuse.
    *s.after_primary_reads.lock().unwrap() = Some((
        3,
        Box::new(move || {
            shared.seed(MIRROR, "current", COMPETING);
            shared.seed(MIRROR, "current.minted-for", OTHER);
            shared.bump(MIRROR, SECRET);
        }),
    ));
    let result = tick(&h).await;
    assert!(
        s.after_primary_reads.lock().unwrap().is_none(),
        "the mirror never moved"
    );
    assert_eq!(s.get(PRIMARY, "current").as_deref(), Some(mine.as_str()));
    assert_not_verified(&j, &s, &snapshot(&s));
    assert_refused(&j, &result, &[MIRROR], OTHER, &mine);
}

/// A copy that cannot be READ is not verified either, and that one is a
/// defer the clock resumes: the store's error is kept on the step beside
/// the copy's path, and the firing after the copy reads again verifies.
/// So is a copy that does not exist, which the converge creates.
#[tokio::test]
async fn verify_defers_while_a_copy_cannot_be_read_and_verifies_once_it_can() {
    const DARK: &str = "GET boss-dev/boss-machine-token returned 503 Service Unavailable";
    for (failure, says) in [(Some(DARK), DARK), (None, "does not exist")] {
        let ((j, s, g, h), mine) = promoted().await;
        *s.unreadable.lock().unwrap() = Some((MIRROR.into(), failure.map(str::to_string)));
        let left = snapshot(&s);
        for _ in 0..2 {
            g.refresh(&s);
            tick(&h).await.expect("a defer acknowledges");
            assert_not_verified(&j, &s, &left);
            let deferred = j.meta(JOB, "verify", "verify_deferred").unwrap();
            assert!(
                deferred.contains(&format!("{MIRROR}/{SECRET}")),
                "{deferred}"
            );
            assert!(deferred.contains(says), "{deferred}");
            assert!(deferred.contains("nothing is revoked"), "{deferred}");
            assert!(deferred.contains(RESUMES), "{deferred}");
            assert!(j.meta(JOB, "verify", "verify_refused").is_none());
        }
        *s.unreadable.lock().unwrap() = None;
        tick(&h).await.expect("every copy is read, and agrees");
        assert_eq!(j.status(JOB, "verify"), "completed");
        assert!(!j.everything_written().contains(&mine));
    }
}

/// A copy astray and another unreadable: the refusal wins, since it is
/// the one no later firing gets past, and it names both.
#[tokio::test]
async fn verify_refuses_naming_an_astray_copy_beside_an_unreadable_one() {
    const THIRD: &str = "boss-third";
    let ((j, s, _g, h), mine) = promoted().await;
    let a: Vec<(String, Value)> = args(&[("phase", ADVANCE_PHASE)])
        .into_iter()
        .map(|(k, v)| match k.as_str() {
            "also_in_namespaces" => (k, Value::String(format!("{MIRROR},{THIRD}"))),
            _ => (k, v),
        })
        .collect();
    s.seed(MIRROR, "current.minted-for", OTHER);
    s.bump(MIRROR, SECRET);
    *s.unreadable.lock().unwrap() = Some((THIRD.into(), Some("503".into())));
    let left = snapshot(&s);
    let result = h.invoke(&a, &tick_ctx()).await;
    assert_not_verified(&j, &s, &left);
    assert_refused(&j, &result, &[MIRROR, THIRD], OTHER, &mine);
}

// ----- pure: the value, the judgements -----

#[test]
fn a_fresh_value_is_32_random_bytes_as_base64url_and_never_repeats() {
    let (a, b) = (fresh_value().unwrap(), fresh_value().unwrap());
    assert_eq!(a.len(), 43, "32 bytes, base64url without padding");
    assert!(
        a.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "base64url only: {}",
        a.len()
    );
    assert_ne!(a, b);
    assert!(reqwest::header::HeaderValue::from_str(&a).is_ok());
}

fn accepts(service: &str, matched: &str) -> (String, GateRead<Accepts>) {
    (
        service.to_string(),
        GateRead::Answered(Accepts {
            service: service.into(),
            mode: Mode::Report,
            matched: matched.into(),
            degraded: false,
        }),
    )
}

#[test]
fn accepts_is_ready_only_when_every_served_port_agrees_and_the_control_answered() {
    let v = judge_accepts(
        &[
            accepts("dispatcher", "next"),
            accepts("jobs", "next"),
            ("sim-control".into(), GateRead::NotServed),
        ],
        "next",
    );
    assert!(v.ready, "{v:?}");
    assert_eq!(v.not_served, vec!["sim-control"]);

    let v = judge_accepts(
        &[accepts("dispatcher", "next"), accepts("jobs", "none")],
        "next",
    );
    assert!(!v.ready);
    assert_eq!(v.lagging, vec!["jobs answers matched none"]);

    // Every port refusing is the roster aimed at the wrong host, not a
    // pod with nothing to verify.
    let v = judge_accepts(
        &[
            ("dispatcher".into(), GateRead::NotServed),
            ("jobs".into(), GateRead::NotServed),
        ],
        "next",
    );
    assert!(!v.ready, "a refused control read is not a pass: {v:?}");
    assert!(v.lagging[0].contains("this process's own port"), "{v:?}");

    let v = judge_accepts(&[accepts("jobs", "next")], "next");
    assert!(!v.ready, "a roster without the control port proves nothing");
}

fn misses(service: &str, f: impl FnOnce(&mut Misses)) -> (String, GateRead<Misses>) {
    let mut m = Misses {
        service: service.into(),
        mode: Mode::Report,
        rows: Vec::new(),
        overflow: 0,
        source_overflow: Vec::new(),
        recording_since: Utc::now() - chrono::Duration::days(3),
        not_clean: Vec::new(),
        evidence: Default::default(),
    };
    f(&mut m);
    (service.to_string(), GateRead::Answered(m))
}

/// A `previous` counted past a source's share ages out of the window as
/// a row does (review ebc7b1cc, B1): it held every later revoke until the
/// pod restarted, and said a caller "still presents" long after it
/// stopped. One with no time holds — when is unknown, and a revoke is
/// not undone.
#[test]
fn a_previous_past_a_share_holds_only_inside_the_window() {
    use boss_core::machine_gate::SourceOverflow;
    let now = Utc::now();
    let day = chrono::Duration::days(1);
    let with = |last: Option<DateTime<Utc>>| {
        misses("jobs", |m| {
            m.source_overflow.push(SourceOverflow {
                source: "10.20.0.66".into(),
                presented: Presented::Previous,
                count: 1,
                first_seen: last,
                last_seen: last,
            })
        })
    };
    let aged = judge_drain(
        &[
            misses("dispatcher", |_| {}),
            with(Some(now - chrono::Duration::days(2))),
        ],
        now,
        day,
        &[],
    );
    assert!(
        aged.is_ok(),
        "a previous before the window is history: {aged:?}"
    );
    for last in [Some(now - chrono::Duration::hours(2)), None] {
        let held = judge_drain(&[misses("dispatcher", |_| {}), with(last)], now, day, &[])
            .expect_err("a previous inside the window, or at no known time, holds");
        assert!(
            held.iter()
                .any(|h| h.contains("10.20.0.66") && h.contains("previous")),
            "{last:?}: {held:?}"
        );
    }
}

/// The overflow hold says only what an overflow is on every gate that can
/// answer: requests no one caller can be charged with. A gate built
/// before per-source shares counted ALL of them there, so "from sources
/// under their own share" was untrue of it (review ebc7b1cc, N3).
#[test]
fn the_overflow_hold_claims_nothing_an_older_gate_cannot_back() {
    let held = judge_drain(
        &[
            misses("dispatcher", |_| {}),
            misses("jobs", |m| m.overflow = 4),
        ],
        Utc::now(),
        chrono::Duration::days(1),
        &[],
    )
    .unwrap_err();
    let text = held.join(" | ");
    assert!(
        text.contains("overflowed (4 request(s) not keyed"),
        "{text}"
    );
    assert!(
        !text.contains("under their own share") && !text.contains("not one noisy caller"),
        "{text}"
    );
}

#[test]
fn a_drain_is_clean_only_when_every_tally_watched_the_whole_window_and_saw_no_previous() {
    let now = Utc::now();
    let day = chrono::Duration::days(1);
    let clean = judge_drain(
        &[
            misses("dispatcher", |_| {}),
            misses("jobs", |m| {
                // A previous match BEFORE the window opened is history.
                m.rows.push(previous_row(now - chrono::Duration::days(2)));
            }),
            ("sim-control".into(), GateRead::NotServed),
        ],
        now,
        day,
        &["sim-control".to_string()],
    );
    let text = clean.expect("clean");
    assert!(
        text.contains("dispatcher, jobs") && text.contains("sim-control"),
        "{text}"
    );

    for (why, read) in [
        (
            "still presents previous",
            misses("jobs", |m| {
                m.rows.push(previous_row(now - chrono::Duration::hours(2)))
            }),
        ),
        ("mode off", misses("jobs", |m| m.mode = Mode::Off)),
        ("overflowed", misses("jobs", |m| m.overflow = 4)),
        (
            "inside the window",
            misses("jobs", |m| {
                m.recording_since = now - chrono::Duration::hours(5)
            }),
        ),
        (
            "could not be read",
            ("jobs".into(), GateRead::Failed("timed out".into())),
        ),
        // B3: down now, answered at verify — its tally went with it.
        (
            "refused the connection",
            ("jobs".into(), GateRead::NotServed),
        ),
        // B1: a port answering as another service is not its tally.
        ("answers as policy", misses("policy", |_| {})),
    ] {
        let read = if why == "answers as policy" {
            ("jobs".to_string(), read.1)
        } else {
            read
        };
        let held =
            judge_drain(&[misses("dispatcher", |_| {}), read], now, day, &[]).expect_err(why);
        assert!(held.iter().any(|h| h.contains(why)), "{why}: {held:?}");
    }
    let held = judge_drain(
        &[
            misses("dispatcher", |_| {}),
            misses("jobs", |m| {
                m.rows.push(previous_row(now - chrono::Duration::hours(2)))
            }),
        ],
        now,
        day,
        &[],
    )
    .unwrap_err();
    assert!(
        held[0].contains("10.20.0.30") && held[0].contains("automation:forge-converge"),
        "the caller still sending the old value is named: {held:?}"
    );
}

#[test]
fn the_declaration_refuses_a_drain_under_the_floor_and_a_mirror_of_itself() {
    let short = args(&[]).into_iter().map(|(k, v)| {
        if k == "drain_minutes" {
            (k, Value::String("30".into()))
        } else {
            (k, v)
        }
    });
    let short: Vec<_> = short.collect();
    assert!(matches!(
        Declaration::parse(&short),
        Err(HandlerError::Permanent(e)) if e.contains("floor")
    ));
    let selfish: Vec<_> = args(&[])
        .into_iter()
        .map(|(k, v)| {
            if k == "also_in_namespaces" {
                (k, Value::String("boss-dev, boss".into()))
            } else {
                (k, v)
            }
        })
        .collect();
    assert!(Declaration::parse(&selfish).is_err());
    let whole = args(&[]);
    let d = Declaration::parse(&whole).unwrap();
    assert_eq!(
        d.targets(),
        vec![(PRIMARY, SECRET), (MIRROR, SECRET)],
        "the primary first, then each mirror"
    );
}

// ----- the walk -----

#[tokio::test]
async fn a_first_mint_walks_every_phase_and_no_value_reaches_the_record() {
    let j = jobs(&[(JOB, packet("2026-09-30T01:00:00Z"))]).await;
    let s = Arc::new(FakeSecrets::default());
    let g = FakeGates::new(&ROSTER, &["sim-control"]);
    let h = handler(&j, &s, &g);
    let a = args(&[]);

    // The scope step fires: the value is staged in next of BOTH Secrets,
    // and the gates, not yet refreshed, do not accept it — deferred.
    fire(&h, &scope_ctx(JOB), &a).await;
    let value = s.get(PRIMARY, "next").expect("next staged in the primary");
    assert_eq!(value.len(), 43);
    assert_eq!(s.get(MIRROR, "next").as_deref(), Some(value.as_str()));
    assert_eq!(s.get(PRIMARY, "next.minted-for").as_deref(), Some(JOB));
    assert_eq!(
        s.get(PRIMARY, "current"),
        None,
        "nothing promoted before the gates accept next"
    );
    assert_eq!(j.status(JOB, "issue"), "completed");
    assert_eq!(j.status(JOB, "install"), "completed");
    assert_ne!(j.status(JOB, "verify"), "completed");
    let why = j
        .meta(JOB, "verify", "verify_deferred")
        .expect("the deferral is on the step");
    assert!(
        why.contains("matched none") && why.contains(ADVANCE_RULE),
        "{why}"
    );

    // Kubelet refreshes: every gate accepts next. The clock promotes.
    g.refresh(&s);
    fire(&h, &tick_ctx(), &args(&[("phase", ADVANCE_PHASE)])).await;
    assert_eq!(s.get(PRIMARY, "current").as_deref(), Some(value.as_str()));
    assert_eq!(s.get(MIRROR, "current").as_deref(), Some(value.as_str()));
    assert_eq!(s.get(PRIMARY, "next"), None, "next is blank once promoted");
    assert_eq!(
        s.get(PRIMARY, "previous"),
        None,
        "a first mint has no old value"
    );
    assert!(s.get(PRIMARY, PROMOTED_AT).is_some());
    assert_ne!(
        j.status(JOB, "verify"),
        "completed",
        "the gates still read the value as next until they refresh again"
    );

    // Refreshed again: every gate reads it as current. Verify completes,
    // and the revoke has nothing to drain.
    g.refresh(&s);
    fire(&h, &tick_ctx(), &args(&[("phase", ADVANCE_PHASE)])).await;
    assert_eq!(j.status(JOB, "verify"), "completed");
    assert_eq!(j.status(JOB, "revoke"), "completed");
    let verified = j.meta(JOB, "verify", "verified").unwrap();
    assert!(
        verified.contains("3 port(s)")
            && verified.contains("not served")
            && verified.contains("sim-control"),
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
            format!("{CREDENTIAL}/minted"),
            format!("{CREDENTIAL}/installed"),
            format!("{CREDENTIAL}/verified"),
            format!("{CREDENTIAL}/revoked"),
        ]
    );

    // A finished rotation redelivered does nothing.
    let before = j.writes.lock().unwrap().len();
    fire(&h, &scope_ctx(JOB), &a).await;
    assert_eq!(j.writes.lock().unwrap().len(), before);
    assert_eq!(s.get(PRIMARY, "current").as_deref(), Some(value.as_str()));

    let record = j.everything_written();
    for v in s.values() {
        assert!(!record.contains(&v), "a token value reached the record");
    }
    assert!(!record.contains(&value));
    assert_eq!(
        *s.key_reads.lock().unwrap(),
        0,
        "every read is one whole read of a Secret, never key by key (review 70d449f9, N5)"
    );
}

/// A firing that died after the primary's write and before its
/// `credential.minted` landed left a staged value no event accounts for.
/// The redelivery re-uses the value — it mints nothing twice — and
/// records the mint the crash lost, once, saying so (review fd151a97,
/// D4; until then the reuse path recorded nothing, and the value reached
/// `current` with no `minted` event at all).
#[tokio::test]
async fn a_redelivered_firing_reuses_its_staged_value_and_records_the_mint_the_crash_lost() {
    let j = jobs(&[(JOB, packet("2026-09-30T01:00:00Z"))]).await;
    let s = Arc::new(FakeSecrets::default());
    let g = FakeGates::new(&ROSTER, &[]);
    let h = handler(&j, &s, &g);
    let staged = "a-value-staged-by-the-first-firing-of-this-p";
    s.seed(PRIMARY, "next", staged);
    s.seed(PRIMARY, "next.minted-for", JOB);
    fire(&h, &scope_ctx(JOB), &args(&[])).await;
    assert_eq!(
        s.get(MIRROR, "next").as_deref(),
        Some(staged),
        "the mirror receives the staged value, not a second one"
    );
    assert_eq!(
        s.get(PRIMARY, "next").as_deref(),
        Some(staged),
        "no new value"
    );
    let minted: Vec<JsonValue> = j
        .rotations
        .lock()
        .unwrap()
        .iter()
        .filter(|(p, _)| p.ends_with("/minted"))
        .map(|(_, body)| body.clone())
        .collect();
    assert_eq!(
        minted.len(),
        1,
        "the lost mint is recorded once: {minted:?}"
    );
    assert_eq!(minted[0]["job_id"], JOB);
    assert_eq!(minted[0]["value_length"], staged.len());
    assert_eq!(minted[0]["recorded_late"], true, "{:?}", minted[0]);
    assert!(j.meta(JOB, "issue", "issued").unwrap().contains("re-used"));
    assert!(j.meta(JOB, "issue", MINT_RECORDED).is_some());
    assert!(!j.everything_written().contains(staged));
}

/// The other half of D4: a firing that minted, recorded the mint, and
/// then stopped at a mirror's write is redelivered — the mint it
/// recorded is not recorded again.
#[tokio::test]
async fn a_mint_already_recorded_is_not_recorded_again_by_the_redelivery() {
    let j = jobs(&[(JOB, packet("2026-09-30T01:00:00Z"))]).await;
    let s = Arc::new(FakeSecrets::default());
    *s.fail_once_in.lock().unwrap() = Some(MIRROR.into());
    let g = FakeGates::new(&ROSTER, &[]);
    let h = handler(&j, &s, &g);
    assert!(
        h.invoke(&args(&[]), &scope_ctx(JOB)).await.is_err(),
        "the mirror's write failed, so the firing fails"
    );
    let value = s.get(PRIMARY, "next").expect("staged in the primary");
    assert_eq!(s.get(MIRROR, "next"), None);
    fire(&h, &scope_ctx(JOB), &args(&[])).await;
    assert_eq!(s.get(MIRROR, "next").as_deref(), Some(value.as_str()));
    let minted = j.phases().iter().filter(|p| p.ends_with("/minted")).count();
    assert_eq!(minted, 1, "one mint, one event: {:?}", j.phases());
    assert_eq!(j.status(JOB, "issue"), "completed");
}

#[tokio::test]
async fn a_rotation_keeps_the_old_value_as_previous_until_nothing_presents_it() {
    let j = jobs(&[(JOB, packet("2026-09-30T01:00:00Z"))]).await;
    let s = Arc::new(FakeSecrets::default());
    for ns in [PRIMARY, MIRROR] {
        s.seed(
            ns,
            "current",
            "the-old-value-every-caller-sends-today-xxxxx",
        );
        s.seed(ns, "current.minted-for", OTHER);
    }
    let g = FakeGates::new(&ROSTER, &[]);
    g.refresh(&s);
    let h = handler(&j, &s, &g);
    let tick = args(&[("phase", ADVANCE_PHASE)]);

    fire(&h, &scope_ctx(JOB), &args(&[])).await;
    g.refresh(&s);
    fire(&h, &tick_ctx(), &tick).await;
    g.refresh(&s);
    fire(&h, &tick_ctx(), &tick).await;
    assert_eq!(j.status(JOB, "verify"), "completed");
    for ns in [PRIMARY, MIRROR] {
        assert_eq!(
            s.get(ns, "previous").as_deref(),
            Some("the-old-value-every-caller-sends-today-xxxxx"),
            "{ns}: the old value is still accepted as previous"
        );
        assert_eq!(s.get(ns, "previous.minted-for").as_deref(), Some(OTHER));
    }

    // Inside the drain window: held, naming when it ends.
    assert_ne!(j.status(JOB, "revoke"), "completed");
    let why = j.meta(JOB, "revoke", "revoke_deferred").unwrap();
    assert!(why.contains("drain window runs 1440 minutes"), "{why}");

    // The window has passed, but a caller still presents the old value:
    // held, and the caller is named.
    let long_ago = (Utc::now() - chrono::Duration::days(2)).to_rfc3339();
    s.seed(PRIMARY, PROMOTED_AT, &long_ago);
    g.with("jobs", |gate| {
        gate.extra_rows
            .push(previous_row(Utc::now() - chrono::Duration::minutes(20)))
    });
    fire(&h, &tick_ctx(), &tick).await;
    let why = j.meta(JOB, "revoke", "revoke_deferred").unwrap();
    assert!(
        why.contains("10.20.0.30") && why.contains("still presents previous"),
        "{why}"
    );
    assert!(
        s.get(PRIMARY, "previous").is_some(),
        "nothing blanked while it is presented"
    );

    // The caller switched long enough ago: previous is blanked everywhere.
    g.with("jobs", |gate| {
        gate.extra_rows = vec![previous_row(Utc::now() - chrono::Duration::days(2))]
    });
    fire(&h, &tick_ctx(), &tick).await;
    assert_eq!(j.status(JOB, "revoke"), "completed");
    for ns in [PRIMARY, MIRROR] {
        assert_eq!(s.get(ns, "previous"), None, "{ns}: previous blanked");
        assert!(s.get(ns, "current").is_some(), "{ns}: current untouched");
    }
    assert_eq!(
        j.meta(JOB, "revoke", "revoke_deferred").as_deref(),
        Some("cleared: the phase completed")
    );
}

#[tokio::test]
async fn a_gate_in_mode_off_holds_the_revoke_because_its_silence_is_not_evidence() {
    let j = jobs(&[(JOB, packet("2026-09-30T01:00:00Z"))]).await;
    let s = Arc::new(FakeSecrets::default());
    s.seed(
        PRIMARY,
        "current",
        "the-new-value-this-packet-promoted-xxxxxxxxx",
    );
    s.seed(PRIMARY, "current.minted-for", JOB);
    s.seed(
        PRIMARY,
        "previous",
        "the-old-value-xxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
    );
    s.seed(
        PRIMARY,
        PROMOTED_AT,
        &(Utc::now() - chrono::Duration::days(2)).to_rfc3339(),
    );
    let g = FakeGates::new(&ROSTER, &[]);
    g.refresh(&s);
    g.with("policy", |gate| gate.mode = Mode::Off);
    {
        let mut p = j.packets.lock().unwrap();
        let steps = &mut p.get_mut(JOB).unwrap().steps;
        for slug in ["issue", "install", "verify"] {
            steps.get_mut(slug).unwrap().status = "completed".into();
        }
        steps.get_mut("revoke").unwrap().status = "ready".into();
    }
    let h = handler(&j, &s, &g);
    fire(&h, &tick_ctx(), &args(&[("phase", ADVANCE_PHASE)])).await;
    assert_ne!(j.status(JOB, "revoke"), "completed");
    assert!(
        j.meta(JOB, "revoke", "revoke_deferred")
            .unwrap()
            .contains("policy is in mode off"),
    );
    assert!(s.get(PRIMARY, "previous").is_some());
}

#[tokio::test]
async fn a_clean_durable_restart_can_finish_the_original_one_day_drain() {
    let j = jobs(&[(JOB, packet("2026-09-30T01:00:00Z"))]).await;
    let s = Arc::new(FakeSecrets::default());
    s.seed(
        PRIMARY,
        "current",
        "the-current-value-this-packet-promoted-xxxx",
    );
    s.seed(PRIMARY, "current.minted-for", JOB);
    s.seed(
        PRIMARY,
        "previous",
        "the-old-value-still-in-the-previous-slot-xx",
    );
    s.seed(
        MIRROR,
        "current",
        "the-current-value-this-packet-promoted-xxxx",
    );
    s.seed(MIRROR, "current.minted-for", JOB);
    s.seed(
        MIRROR,
        "previous",
        "the-old-value-still-in-the-previous-slot-xx",
    );
    s.seed(
        PRIMARY,
        PROMOTED_AT,
        &(Utc::now() - chrono::Duration::days(2)).to_rfc3339(),
    );
    let g = FakeGates::new(&ROSTER, &[]);
    g.refresh(&s);
    g.with("jobs", |gate| {
        gate.restart(Utc::now() - chrono::Duration::minutes(10), true)
    });
    let snapshot = g
        .window_snapshot(
            "the-current-value-this-packet-promoted-xxxx",
            chrono::Duration::days(1),
        )
        .await;
    let observation = snapshot.observation.unwrap();
    let proof = boss_core::gate_window::join_previous_window(
        &observation.required_services,
        observation.from,
        observation.now,
        observation.facts,
        observation.reads,
    );
    assert!(
        proof.covers_requested_window,
        "the fixture has a complete clean durable day: {proof:?}"
    );
    {
        let mut packets = j.packets.lock().unwrap();
        let steps = &mut packets.get_mut(JOB).unwrap().steps;
        for slug in ["issue", "install", "verify"] {
            steps.get_mut(slug).unwrap().status = "completed".into();
        }
        steps.get_mut("revoke").unwrap().status = "ready".into();
    }
    fire(
        &handler(&j, &s, &g),
        &tick_ctx(),
        &args(&[("phase", ADVANCE_PHASE)]),
    )
    .await;
    assert_eq!(
        j.status(JOB, "revoke"),
        "completed",
        "the young live process must not erase a proven durable day: {:?}",
        j.meta(JOB, "revoke", "revoke_deferred")
    );
    assert_eq!(s.get(PRIMARY, "previous"), None);
}

/// G3 (review 1718e070): a promotion that lands while the drain reads the
/// gates moves `current`; the drain judged the old one, so it blanks
/// nothing and defers, rather than blank a `previous` the newer promotion
/// wrote.
#[tokio::test]
async fn a_drain_blanks_nothing_when_current_moved_while_it_judged() {
    let j = jobs(&[(JOB, packet("2026-09-30T01:00:00Z"))]).await;
    let s = Arc::new(FakeSecrets::default());
    for ns in [PRIMARY, MIRROR] {
        s.seed(
            ns,
            "current",
            "the-new-value-this-packet-promoted-xxxxxxxxx",
        );
        s.seed(ns, "current.minted-for", JOB);
        s.seed(
            ns,
            "previous",
            "the-old-value-xxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
        );
        s.seed(ns, "previous.minted-for", OTHER);
    }
    s.seed(
        PRIMARY,
        PROMOTED_AT,
        &(Utc::now() - chrono::Duration::days(2)).to_rfc3339(),
    );
    let g = FakeGates::new(&ROSTER, &[]);
    g.refresh(&s);
    {
        let mut p = j.packets.lock().unwrap();
        let steps = &mut p.get_mut(JOB).unwrap().steps;
        for slug in ["issue", "install", "verify"] {
            steps.get_mut(slug).unwrap().status = "completed".into();
        }
        steps.get_mut("revoke").unwrap().status = "ready".into();
    }
    let moved = Arc::clone(&s);
    *g.on_misses.lock().unwrap() = Some(Box::new(move || {
        moved.seed(
            PRIMARY,
            "current",
            "a-value-a-newer-promotion-wrote-xxxxxxxxxxx",
        );
    }));
    let h = handler(&j, &s, &g);
    fire(&h, &tick_ctx(), &args(&[("phase", ADVANCE_PHASE)])).await;
    assert_ne!(j.status(JOB, "revoke"), "completed");
    let why = j.meta(JOB, "revoke", "revoke_deferred").unwrap();
    assert!(why.contains("current changed while the drain"), "{why}");
    for ns in [PRIMARY, MIRROR] {
        assert!(s.get(ns, "previous").is_some(), "{ns}: nothing blanked");
    }
}

#[tokio::test]
async fn one_rotation_at_a_time_and_the_earlier_packet_goes_first() {
    let mut in_flight = packet("2026-09-30T00:00:00Z");
    for slug in ["issue", "install"] {
        in_flight.steps.get_mut(slug).unwrap().status = "completed".into();
    }
    let j = jobs(&[(OTHER, in_flight), (JOB, packet("2026-09-30T01:00:00Z"))]).await;
    let s = Arc::new(FakeSecrets::default());
    s.seed(
        PRIMARY,
        "next",
        "the-other-packets-staged-value-xxxxxxxxxxxxx",
    );
    s.seed(PRIMARY, "next.minted-for", OTHER);
    let g = FakeGates::new(&ROSTER, &[]);
    let h = handler(&j, &s, &g);
    fire(&h, &scope_ctx(JOB), &args(&[])).await;
    assert_eq!(
        s.get(PRIMARY, "next.minted-for").as_deref(),
        Some(OTHER),
        "the in-flight packet's staged value is not overwritten"
    );
    assert_ne!(j.status(JOB, "issue"), "completed");
    let why = j.meta(JOB, "issue", "issue_deferred").unwrap();
    assert!(why.contains(OTHER) && why.contains("in flight"), "{why}");

    // A packet opened earlier whose scope is done, not yet started, also
    // goes first.
    let j = jobs(&[
        (OTHER, packet("2026-09-30T00:00:00Z")),
        (JOB, packet("2026-09-30T01:00:00Z")),
    ])
    .await;
    let s = Arc::new(FakeSecrets::default());
    let h = handler(&j, &s, &g);
    fire(&h, &scope_ctx(JOB), &args(&[])).await;
    assert!(
        j.meta(JOB, "issue", "issue_deferred")
            .unwrap()
            .contains("opened earlier"),
    );
    assert_eq!(s.get(PRIMARY, "next"), None);
}

#[tokio::test]
async fn a_clock_firing_must_say_it_advances_and_the_drain_floor_is_the_rules() {
    let j = jobs(&[(JOB, packet("2026-09-30T01:00:00Z"))]).await;
    let s = Arc::new(FakeSecrets::default());
    let g = FakeGates::new(&ROSTER, &[]);
    let h = handler(&j, &s, &g);
    assert!(matches!(
        h.invoke(&args(&[]), &tick_ctx()).await,
        Err(HandlerError::Permanent(e)) if e.contains("advance")
    ));
    assert!(matches!(
        h.invoke(&args(&[("phase", "revoke")]), &tick_ctx()).await,
        Err(HandlerError::Permanent(_))
    ));
    assert_eq!(
        s.get(PRIMARY, "next"),
        None,
        "a refused firing writes nothing"
    );
}

// ----- the HTTP adapter, against the real gate -----

#[tokio::test]
async fn local_gates_read_the_real_gate_and_a_refused_port_is_not_served() {
    use boss_core::machine_gate::{MachineGate, Reading, gated};
    let gate = Arc::new(MachineGate::new(
        "jobs",
        &[],
        Reading::new(
            Mode::Report,
            Slots::new(Some("cur-value".into()), Some("next-value".into()), None),
        ),
    ));
    let app = gated(axum::Router::new(), gate)
        .into_make_service_with_connect_info::<std::net::SocketAddr>();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let served = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    // A port that was bound and released: nothing listens there.
    let closed = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap()
    };
    let gates = LocalGates::new(vec![
        ("jobs".into(), format!("http://{served}")),
        ("sim-control".into(), format!("http://{closed}")),
    ]);
    assert_eq!(gates.roster(), vec!["jobs", "sim-control"]);
    match gates.accepts("jobs", "next-value").await {
        GateRead::Answered(a) => assert_eq!(a.matched, "next"),
        other => panic!("{other:?}"),
    }
    match gates.misses("jobs", "cur-value").await {
        GateRead::Answered(m) => {
            assert_eq!(m.service, "jobs");
            assert_eq!(m.mode, Mode::Report);
        }
        other => panic!("{other:?}"),
    }
    assert!(
        matches!(gates.misses("jobs", "a-guess").await, GateRead::Failed(e) if e.contains("401")),
        "the tally refuses an unaccepted token, and that is a failed read, not a clean one"
    );
    assert!(matches!(
        gates.accepts("sim-control", "next-value").await,
        GateRead::NotServed
    ));
}

/// The roster is every `boss_ports` row that MOUNTS a gate — the one
/// ungated list, `machine_gate::UNGATED`, that the gate-mount pin reads
/// too. Until review 70d449f9 (B1) it was every row, the gateway among
/// them, which answers 404 on the gate's routes, so no rotation could
/// ever pass verify.
#[test]
fn the_production_roster_is_every_gated_boss_ports_row_with_the_control_among_them() {
    let roster = LocalGates::from_ports().roster();
    let want: Vec<String> = boss_ports::all()
        .map(|s| s.name.to_string())
        .filter(|n| boss_core::machine_gate::is_gated(n))
        .collect();
    assert_eq!(roster, want);
    assert!(!roster.iter().any(|s| s == "gateway"), "{roster:?}");
    assert!(roster.iter().any(|s| s == CONTROL_SERVICE));
    assert_eq!(
        roster.len()
            + boss_core::machine_gate::UNGATED
                .iter()
                .filter(|u| u.service.is_some())
                .count(),
        boss_ports::all().count()
    );
}

/// A real port with no gate routes answers 404, and that is lagging,
/// never agreement — and a port answering as ANOTHER service vouches for
/// nothing (review 70d449f9, B1).
#[tokio::test]
async fn a_port_without_the_gate_or_answering_as_another_service_does_not_agree() {
    use boss_core::machine_gate::gated;
    let no_gate = axum::Router::new().route("/health", axum::routing::get(|| async { "ok" }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bare = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, no_gate).await.unwrap() });
    // A gate that calls itself `jobs`, reached through the roster's `policy`.
    let jobs = Arc::new(MachineGate::new(
        "jobs",
        &[],
        Reading::new(Mode::Off, Slots::new(None, Some("next-value".into()), None)),
    ));
    let app = gated(axum::Router::new(), Arc::clone(&jobs))
        .into_make_service_with_connect_info::<std::net::SocketAddr>();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let other = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let app = gated(axum::Router::new(), jobs)
        .into_make_service_with_connect_info::<std::net::SocketAddr>();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dispatcher = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let gates = LocalGates::new(vec![
        ("dispatcher".into(), format!("http://{dispatcher}")),
        ("gateway".into(), format!("http://{bare}")),
        ("policy".into(), format!("http://{other}")),
    ]);
    let roster = gates.roster();
    let mut reads = Vec::new();
    for s in &roster {
        reads.push((s.clone(), gates.accepts(s, "next-value").await));
    }
    let v = judge_accepts(&reads, "next");
    assert!(!v.ready, "{v:?}");
    assert!(
        v.lagging
            .iter()
            .any(|l| l.starts_with("gateway") && l.contains("404")),
        "a 404 is lagging: {v:?}"
    );
    assert!(
        v.lagging
            .iter()
            .any(|l| l.starts_with("policy") && l.contains("answers as jobs")),
        "a port answering as another service vouches for nothing: {v:?}"
    );
    assert!(
        v.lagging.iter().any(|l| l.starts_with("dispatcher")),
        "so does the control port, when the gate there names another service: {v:?}"
    );
}

/// B2 end to end: a rotation promoted while every gate was in `off`
/// (verify reads /accepts, which answers in every mode), then the report
/// flip, and the next tick — the gates' tallies are minutes old, so the
/// revoke holds however long ago the promotion was.
#[tokio::test]
async fn a_gate_that_leaves_off_after_the_promotion_holds_the_revoke() {
    let j = jobs(&[(JOB, packet("2026-09-30T01:00:00Z"))]).await;
    let s = Arc::new(FakeSecrets::default());
    for ns in [PRIMARY, MIRROR] {
        s.seed(
            ns,
            "current",
            "the-old-value-every-caller-sends-today-xxxxx",
        );
        s.seed(ns, "current.minted-for", OTHER);
    }
    let g = FakeGates::in_mode(Mode::Off, &ROSTER, &[]);
    g.refresh(&s);
    let h = handler(&j, &s, &g);
    let tick = args(&[("phase", ADVANCE_PHASE)]);
    fire(&h, &scope_ctx(JOB), &args(&[])).await;
    g.refresh(&s);
    fire(&h, &tick_ctx(), &tick).await;
    g.refresh(&s);
    fire(&h, &tick_ctx(), &tick).await;
    assert_eq!(j.status(JOB, "verify"), "completed", "promoted in off");
    assert_ne!(j.status(JOB, "revoke"), "completed");

    // A day and more after the promotion, the report flip lands.
    s.seed(
        PRIMARY,
        PROMOTED_AT,
        &(Utc::now() - chrono::Duration::days(2)).to_rfc3339(),
    );
    g.set_mode(Mode::Report);
    fire(&h, &tick_ctx(), &tick).await;
    assert_ne!(j.status(JOB, "revoke"), "completed");
    let why = j.meta(JOB, "revoke", "revoke_deferred").unwrap();
    assert!(why.contains("inside the window"), "{why}");
    assert!(
        s.get(PRIMARY, "previous").is_some(),
        "nothing blanked on a minutes-old watch"
    );
}

/// B3: a port that refuses the connection at the drain, having answered
/// at verify, lost its tally with its process — the revoke holds. A port
/// that was down at verify too is excused, by name.
#[tokio::test]
async fn a_port_down_at_the_drain_holds_unless_it_was_down_at_verify() {
    let j = jobs(&[(JOB, packet("2026-09-30T01:00:00Z"))]).await;
    let s = Arc::new(FakeSecrets::default());
    for ns in [PRIMARY, MIRROR] {
        s.seed(
            ns,
            "current",
            "the-old-value-every-caller-sends-today-xxxxx",
        );
        s.seed(ns, "current.minted-for", OTHER);
    }
    let g = FakeGates::new(&ROSTER, &["sim-control"]);
    g.refresh(&s);
    let h = handler(&j, &s, &g);
    let tick = args(&[("phase", ADVANCE_PHASE)]);
    fire(&h, &scope_ctx(JOB), &args(&[])).await;
    g.refresh(&s);
    fire(&h, &tick_ctx(), &tick).await;
    g.refresh(&s);
    fire(&h, &tick_ctx(), &tick).await;
    assert_eq!(j.status(JOB, "verify"), "completed");
    s.seed(
        PRIMARY,
        PROMOTED_AT,
        &(Utc::now() - chrono::Duration::days(2)).to_rfc3339(),
    );

    g.with("policy", |gate| gate.served = false);
    fire(&h, &tick_ctx(), &tick).await;
    let why = j.meta(JOB, "revoke", "revoke_deferred").unwrap();
    assert!(
        why.contains("policy refused the connection") && !why.contains("sim-control"),
        "{why}"
    );
    assert!(s.get(PRIMARY, "previous").is_some());

    g.with("policy", |gate| gate.served = true);
    fire(&h, &tick_ctx(), &tick).await;
    assert_eq!(j.status(JOB, "revoke"), "completed");
    assert!(
        j.meta(JOB, "revoke", "revoked")
            .unwrap()
            .contains("sim-control"),
        "the excused port is named"
    );
}

/// D1 (review fd151a97): the B3 excuse lasts only while the port has
/// stayed down. A port down at verify that later ANSWERS a drain read —
/// even one inside the window — held a tally from then on, and callers
/// may have presented `previous` to it; if it then crashes, its silence
/// is a lost tally, not a port that never ran. It used to stay excused,
/// and `previous` was blanked over it.
#[tokio::test]
async fn a_port_excused_at_verify_loses_the_excuse_once_it_answers() {
    let j = jobs(&[(JOB, packet("2026-09-30T01:00:00Z"))]).await;
    let s = Arc::new(FakeSecrets::default());
    for ns in [PRIMARY, MIRROR] {
        s.seed(
            ns,
            "current",
            "the-old-value-every-caller-sends-today-xxxxx",
        );
        s.seed(ns, "current.minted-for", OTHER);
    }
    let g = FakeGates::new(&ROSTER, &["sim-control"]);
    g.refresh(&s);
    let h = handler(&j, &s, &g);
    let tick = args(&[("phase", ADVANCE_PHASE)]);
    fire(&h, &scope_ctx(JOB), &args(&[])).await;
    g.refresh(&s);
    fire(&h, &tick_ctx(), &tick).await;
    g.refresh(&s);
    fire(&h, &tick_ctx(), &tick).await;
    assert_eq!(j.status(JOB, "verify"), "completed");
    assert_eq!(
        s.get(PRIMARY, NOT_SERVED_AT_VERIFY).as_deref(),
        Some("sim-control")
    );

    // Inside the window sim-control boots and answers its drain read.
    g.with("sim-control", |gate| gate.served = true);
    fire(&h, &tick_ctx(), &tick).await;
    assert!(
        j.meta(JOB, "revoke", "revoke_deferred")
            .unwrap()
            .contains("drain window runs"),
    );
    assert_eq!(
        s.get(PRIMARY, NOT_SERVED_AT_VERIFY),
        None,
        "a port that answered is no longer excused"
    );

    // It crashes, taking its tally; the window has since passed.
    g.with("sim-control", |gate| gate.served = false);
    s.seed(
        PRIMARY,
        PROMOTED_AT,
        &(Utc::now() - chrono::Duration::days(2)).to_rfc3339(),
    );
    fire(&h, &tick_ctx(), &tick).await;
    let why = j.meta(JOB, "revoke", "revoke_deferred").unwrap();
    assert!(why.contains("sim-control refused the connection"), "{why}");
    assert!(s.get(PRIMARY, "previous").is_some(), "nothing blanked");

    // Back, and clean for the whole window: the revoke completes.
    g.with("sim-control", |gate| gate.served = true);
    fire(&h, &tick_ctx(), &tick).await;
    assert_eq!(j.status(JOB, "revoke"), "completed");
}

/// F2 (review 363facd0): the excuse starts at the PROMOTION, not at the
/// pass that completes verify. A port down when verify promoted, that
/// answers a later verify read while the gates still lag and then goes
/// down again, is not excused — it held a tally in between. It used to
/// be recorded from the completing pass alone, so it was.
#[tokio::test]
async fn an_answer_during_a_deferred_verify_strikes_the_excuse() {
    let j = jobs(&[(JOB, packet("2026-09-30T01:00:00Z"))]).await;
    let s = Arc::new(FakeSecrets::default());
    for ns in [PRIMARY, MIRROR] {
        s.seed(
            ns,
            "current",
            "the-old-value-every-caller-sends-today-xxxxx",
        );
        s.seed(ns, "current.minted-for", OTHER);
    }
    let g = FakeGates::new(&ROSTER, &["sim-control"]);
    g.refresh(&s);
    let h = handler(&j, &s, &g);
    let tick = args(&[("phase", ADVANCE_PHASE)]);
    fire(&h, &scope_ctx(JOB), &args(&[])).await;
    g.refresh(&s);
    // Verify promotes; the gates still read the value as next, so it
    // defers. The excuse is recorded now, from the promoting pass.
    fire(&h, &tick_ctx(), &tick).await;
    assert_ne!(j.status(JOB, "verify"), "completed");
    assert_eq!(
        s.get(PRIMARY, NOT_SERVED_AT_VERIFY).as_deref(),
        Some("sim-control"),
        "recorded on the promoting pass"
    );
    // sim-control boots and answers the next verify read.
    g.with("sim-control", |gate| gate.served = true);
    fire(&h, &tick_ctx(), &tick).await;
    assert_ne!(j.status(JOB, "verify"), "completed");
    assert_eq!(s.get(PRIMARY, NOT_SERVED_AT_VERIFY), None, "struck");
    // It goes down; the gates catch up; verify completes.
    g.with("sim-control", |gate| gate.served = false);
    g.refresh(&s);
    fire(&h, &tick_ctx(), &tick).await;
    assert_eq!(j.status(JOB, "verify"), "completed");
    assert_eq!(
        s.get(PRIMARY, NOT_SERVED_AT_VERIFY),
        None,
        "a port that answered after the promotion is not excused"
    );
}

/// F6 (review 363facd0): only evidence of a process strikes an excuse.
/// A read this dispatcher never sent (no HTTP client, a name the roster
/// cannot place) says nothing about the port.
#[test]
fn an_excuse_is_struck_only_by_evidence_of_a_process() {
    let excused = vec!["a".to_string(), "b".into(), "c".into(), "d".into()];
    let reads: Vec<(String, GateRead<Misses>)> = vec![
        ("a".into(), GateRead::NotServed),
        ("b".into(), GateRead::Unasked("http client: no TLS".into())),
        ("c".into(), GateRead::Failed("answered 500".into())),
        misses("d", |_| {}),
    ];
    assert_eq!(still_excused(&excused, &reads), vec!["a", "b"]);
}

/// N1: the copies disagree on `current` (a promotion half-applied by an
/// abandoned packet) — staging over that would leave one namespace's
/// callers sending a value no gate holds. Refused, on the step.
#[tokio::test]
async fn a_stage_refuses_while_the_copies_disagree_on_current() {
    let j = jobs(&[(JOB, packet("2026-09-30T01:00:00Z"))]).await;
    let s = Arc::new(FakeSecrets::default());
    s.seed(
        PRIMARY,
        "current",
        "value-one-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
    );
    s.seed(PRIMARY, "current.minted-for", OTHER);
    s.seed(
        MIRROR,
        "current",
        "value-two-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
    );
    s.seed(MIRROR, "current.minted-for", "some-abandoned-packet");
    let g = FakeGates::new(&ROSTER, &[]);
    let h = handler(&j, &s, &g);
    let refused = h.invoke(&args(&[]), &scope_ctx(JOB)).await;
    assert!(matches!(refused, Err(HandlerError::Permanent(ref e)) if e.contains("disagree")));
    assert_eq!(s.get(PRIMARY, "next"), None, "nothing staged");
    assert_eq!(s.get(MIRROR, "next"), None);
    let why = j
        .meta(JOB, "issue", "issue_refused")
        .expect("the refusal is on the step");
    assert!(why.contains("boss-dev/boss-machine-token"), "{why}");
    assert!(!j.phases().iter().any(|p| p.ends_with("/minted")));

    // A person makes the copies agree; the next firing stages, and the
    // issue step no longer carries a refusal that has stopped holding
    // (review fd151a97, D3).
    s.seed(
        MIRROR,
        "current",
        "value-one-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
    );
    s.seed(MIRROR, "current.minted-for", OTHER);
    fire(&h, &scope_ctx(JOB), &args(&[])).await;
    assert_eq!(j.status(JOB, "issue"), "completed");
    assert_eq!(
        j.meta(JOB, "issue", "issue_refused").as_deref(),
        Some("cleared: the phase completed")
    );
}

/// The half-applied promotion the two tests below start from: packet
/// `by` promoted the mirror (its value is the mirror's `current`, the
/// old value its `previous`) and stopped before the primary, which
/// still holds that value in `next` and the old one in `current`.
fn half_promoted(s: &FakeSecrets, by: &str) -> (&'static str, &'static str) {
    let (old, new) = (
        "the-old-value-every-caller-sends-today-xxxxx",
        "the-value-the-half-applied-packet-minted-xx",
    );
    s.seed(PRIMARY, "current", old);
    s.seed(PRIMARY, "current.minted-for", "an-earlier-rotation");
    s.seed(PRIMARY, "next", new);
    s.seed(PRIMARY, "next.minted-for", by);
    s.seed(MIRROR, "current", new);
    s.seed(MIRROR, "current.minted-for", by);
    s.seed(MIRROR, "previous", old);
    s.seed(MIRROR, "previous.minted-for", "an-earlier-rotation");
    s.seed(
        MIRROR,
        PROMOTED_AT,
        &(Utc::now() - chrono::Duration::hours(3)).to_rfc3339(),
    );
    (old, new)
}

/// D3 (review fd151a97): a younger packet that fires while an older one
/// is between its mirror's promotion and its primary's DEFERS behind it.
/// The copies-disagree check used to run first, so it refused the
/// younger packet Permanent and left `issue_refused` on its step.
#[tokio::test]
async fn a_younger_packet_defers_behind_an_older_ones_half_applied_promotion() {
    let mut in_flight = packet("2026-09-30T00:00:00Z");
    for slug in ["issue", "install"] {
        in_flight.steps.get_mut(slug).unwrap().status = "completed".into();
    }
    let j = jobs(&[(OTHER, in_flight), (JOB, packet("2026-09-30T01:00:00Z"))]).await;
    let s = Arc::new(FakeSecrets::default());
    let (old, new) = half_promoted(&s, OTHER);
    let g = FakeGates::new(&ROSTER, &[]);
    let h = handler(&j, &s, &g);
    fire(&h, &scope_ctx(JOB), &args(&[])).await;
    let why = j.meta(JOB, "issue", "issue_deferred").expect("deferred");
    assert!(why.contains(OTHER) && why.contains("in flight"), "{why}");
    assert_eq!(j.meta(JOB, "issue", "issue_refused"), None);
    assert_eq!(s.get(PRIMARY, "current").as_deref(), Some(old), "untouched");
    assert_eq!(s.get(PRIMARY, "next").as_deref(), Some(new), "untouched");
}

/// D3: the packet that half-applied its promotion is gone (abandoned or
/// closed), so no verify of its own will finish it. The data says what
/// finishing means — the primary's `next` IS the mirror's `current`, by
/// value and by packet — so the broker finishes it on the primary rather
/// than refusing every later rotation until a person edits a Secret by
/// hand. The drain window restarts from the roll-forward: the primary's
/// callers switch only now.
#[tokio::test]
async fn an_abandoned_half_applied_promotion_is_rolled_forward_from_the_data() {
    let abandoned = "2710c8fc-0000-4000-8000-00000000abcd";
    let j = jobs(&[
        (abandoned, abandoned_packet()),
        (JOB, packet("2026-09-30T01:00:00Z")),
    ])
    .await;
    let s = Arc::new(FakeSecrets::default());
    let (old, new) = half_promoted(&s, abandoned);
    let g = FakeGates::new(&ROSTER, &[]);
    g.refresh(&s);
    let h = handler(&j, &s, &g);
    let before = Utc::now();
    fire(&h, &scope_ctx(JOB), &args(&[])).await;
    assert_eq!(s.get(PRIMARY, "current").as_deref(), Some(new));
    assert_eq!(
        s.get(PRIMARY, "current.minted-for").as_deref(),
        Some(abandoned)
    );
    assert_eq!(s.get(PRIMARY, "previous").as_deref(), Some(old));
    assert_eq!(
        s.get(PRIMARY, "previous.minted-for").as_deref(),
        Some("an-earlier-rotation")
    );
    assert_eq!(s.get(PRIMARY, "next"), None);
    let promoted = s.get(PRIMARY, PROMOTED_AT).expect("promoted-at");
    let promoted = DateTime::parse_from_rfc3339(&promoted).unwrap();
    assert!(promoted >= before, "the window starts at the roll-forward");
    assert_eq!(s.get(MIRROR, "current").as_deref(), Some(new), "untouched");
    assert_eq!(j.meta(JOB, "issue", "issue_refused"), None);
    let rolled = j
        .meta(JOB, "issue", "issue_rolled_forward")
        .expect("the roll-forward is on the step");
    assert!(rolled.contains(abandoned), "{rolled}");
    // The old value now drains before this packet stages anything.
    let why = j.meta(JOB, "issue", "issue_deferred").expect("deferred");
    assert!(why.contains("drain window"), "{why}");
    assert!(!j.phases().iter().any(|p| p.ends_with("/minted")));
    // The value going live is on the record as an event, naming the
    // packet that staged it and how that packet closed (review 363facd0,
    // F3).
    let verified: Vec<JsonValue> = j
        .rotations
        .lock()
        .unwrap()
        .iter()
        .filter(|(p, _)| p.ends_with("/verified"))
        .map(|(_, b)| b.clone())
        .collect();
    assert_eq!(verified.len(), 1, "{verified:?}");
    assert_eq!(verified[0]["rolled_forward"], true);
    assert_eq!(verified[0]["for_packet"], abandoned);
    assert_eq!(verified[0]["job_id"], JOB);
    let record = j.everything_written();
    assert!(!record.contains(old) && !record.contains(new));
}

/// F3 (review 363facd0): the event lands BEFORE the primary is written,
/// so a firing that stops between the two leaves the value's going-live
/// on the record rather than nowhere.
#[tokio::test]
async fn a_roll_forward_records_its_event_before_it_writes_the_primary() {
    let abandoned = "2710c8fc-0000-4000-8000-00000000abcd";
    let j = jobs(&[
        (abandoned, abandoned_packet()),
        (JOB, packet("2026-09-30T01:00:00Z")),
    ])
    .await;
    let s = Arc::new(FakeSecrets::default());
    let (old, _) = half_promoted(&s, abandoned);
    *s.fail_once_in.lock().unwrap() = Some(PRIMARY.into());
    let g = FakeGates::new(&ROSTER, &[]);
    g.refresh(&s);
    let h = handler(&j, &s, &g);
    assert!(h.invoke(&args(&[]), &scope_ctx(JOB)).await.is_err());
    assert_eq!(
        s.get(PRIMARY, "current").as_deref(),
        Some(old),
        "not written"
    );
    assert!(
        j.phases().iter().any(|p| p.ends_with("/verified")),
        "the event came first: {:?}",
        j.phases()
    );
}

/// F1 (review 363facd0): a roll-forward reads the gates as verify does,
/// and a port refusing the connection then is recorded in
/// not-served-at-verify — so the leftover drain does not hold on a port
/// that never ran, and the step never says such a port answered.
#[tokio::test]
async fn a_roll_forward_records_the_ports_down_at_that_moment() {
    let abandoned = "2710c8fc-0000-4000-8000-00000000abcd";
    let j = jobs(&[
        (abandoned, abandoned_packet()),
        (JOB, packet("2026-09-30T01:00:00Z")),
    ])
    .await;
    let s = Arc::new(FakeSecrets::default());
    half_promoted(&s, abandoned);
    let g = FakeGates::new(&ROSTER, &["sim-control"]);
    g.refresh(&s);
    let h = handler(&j, &s, &g);
    fire(&h, &scope_ctx(JOB), &args(&[])).await;
    assert_eq!(
        s.get(PRIMARY, NOT_SERVED_AT_VERIFY).as_deref(),
        Some("sim-control")
    );
    // The window passes; sim-control never ran. The leftover drains and
    // this packet stages its own value.
    s.seed(
        PRIMARY,
        PROMOTED_AT,
        &(Utc::now() - chrono::Duration::days(2)).to_rfc3339(),
    );
    g.refresh(&s);
    fire(&h, &tick_ctx(), &args(&[("phase", ADVANCE_PHASE)])).await;
    let deferred = j.meta(JOB, "issue", "issue_deferred").unwrap_or_default();
    assert!(!deferred.contains("answered"), "{deferred}");
    assert_eq!(s.get(PRIMARY, "previous"), None, "the leftover drained");
    assert_eq!(j.status(JOB, "issue"), "completed");
}

/// The gates do not accept the value yet: a roll-forward waits, as
/// verify's promotion waits, rather than promote a value a gate would
/// refuse from a caller.
#[tokio::test]
async fn a_roll_forward_waits_for_every_gate_to_accept_the_value() {
    let abandoned = "2710c8fc-0000-4000-8000-00000000abcd";
    let j = jobs(&[
        (abandoned, abandoned_packet()),
        (JOB, packet("2026-09-30T01:00:00Z")),
    ])
    .await;
    let s = Arc::new(FakeSecrets::default());
    let (old, _) = half_promoted(&s, abandoned);
    let g = FakeGates::new(&ROSTER, &[]); // never refreshed: they hold nothing
    let h = handler(&j, &s, &g);
    fire(&h, &scope_ctx(JOB), &args(&[])).await;
    assert_eq!(s.get(PRIMARY, "current").as_deref(), Some(old));
    let why = j.meta(JOB, "issue", "issue_deferred").expect("deferred");
    assert!(why.contains("matched none"), "{why}");
}

/// F3 and F4 (review 363facd0): each clause of the roll-forward's
/// predicate that must refuse, refuses — on the step and Permanent,
/// never an endless retry — and nothing is written.
#[tokio::test]
async fn a_roll_forward_refuses_every_state_the_data_does_not_settle() {
    type Break = fn(&FakeSecrets);
    let abandoned = "2710c8fc-0000-4000-8000-00000000abcd";
    // Each row breaks ONE clause and names the phrase of the refusal that
    // clause makes, so a row cannot pass on a later guard's refusal
    // (review 1718e070, G2): the copies' own refusal says "the copies
    // disagree"; how_closed's name the packet's state.
    const COPIES: &str = "the copies disagree";
    let cases: [(&str, Break, &str, &str); 11] = [
        (
            "the primary's previous holds a value",
            |s| {
                s.seed(
                    PRIMARY,
                    "previous",
                    "an-undrained-value-xxxxxxxxxxxxxxxxxxxxxxxx",
                )
            },
            "",
            COPIES,
        ),
        (
            "an unpromoted mirror stages a different next",
            |s| {
                s.seed(
                    "boss-play",
                    "current",
                    "the-old-value-every-caller-sends-today-xxxxx",
                );
                s.seed("boss-play", "current.minted-for", "an-earlier-rotation");
                s.seed(
                    "boss-play",
                    "next",
                    "some-other-staged-value-xxxxxxxxxxxxxxxxxxx",
                );
                s.seed(
                    "boss-play",
                    "next.minted-for",
                    "2710c8fc-0000-4000-8000-00000000abcd",
                );
            },
            "boss-play",
            COPIES,
        ),
        (
            "the promoted value matches but its minted-for does not",
            |s| s.seed(MIRROR, "current.minted-for", "a-different-packet"),
            "",
            COPIES,
        ),
        (
            // Blank on both sides, so the copies otherwise match exactly
            // and only the minted-for guard refuses — never how_closed
            // asked about a packet with no id.
            "the primary's next names no packet",
            |s| {
                s.seed(PRIMARY, "next.minted-for", "");
                s.seed(MIRROR, "current.minted-for", "");
            },
            "",
            COPIES,
        ),
        (
            "a promoted mirror's previous is not the primary's current",
            |s| {
                s.seed(
                    MIRROR,
                    "previous",
                    "a-value-nothing-else-holds-xxxxxxxxxxxxxxxxx",
                )
            },
            "",
            COPIES,
        ),
        (
            "a promoted mirror still stages a next",
            |s| {
                s.seed(
                    MIRROR,
                    "next",
                    "a-value-staged-over-the-promoted-mirror-xxx",
                );
                s.seed(
                    MIRROR,
                    "next.minted-for",
                    "2710c8fc-0000-4000-8000-00000000abcd",
                );
            },
            "",
            COPIES,
        ),
        (
            "the packet was cancelled",
            |_| {},
            "cancelled",
            "was cancelled",
        ),
        ("no such packet", |_| {}, "missing", "does not hold"),
        // G1 (review 1718e070): closed by hand through a status PUT, not
        // on the abandoned terminal.
        (
            "the packet was closed by hand",
            |_| {},
            "closed-by-hand",
            "did not close on its abandoned terminal",
        ),
        // G4: the id names a packet, but not a rotation of this credential.
        (
            "the packet is another credential's rotation",
            |_| {},
            "other-credential",
            "is not a rotation of",
        ),
        (
            "the packet is not a rotation at all",
            |_| {},
            "other-kind",
            "is not a rotation of",
        ),
    ];
    for (why, break_it, variant, says) in cases {
        let mut closed = abandoned_packet();
        match variant {
            "cancelled" => {
                closed.status = "cancelled".into();
                closed.abandoned = false;
            }
            "closed-by-hand" => closed.abandoned = false,
            "other-credential" => closed.about = "boss-dev-forge-token".into(),
            "other-kind" => closed.kind = "backlog-item".into(),
            _ => {}
        }
        let mut packets = vec![(JOB, packet("2026-09-30T01:00:00Z"))];
        if variant != "missing" {
            packets.push((abandoned, closed));
        }
        let j = jobs(&packets).await;
        let s = Arc::new(FakeSecrets::default());
        let (old, new) = half_promoted(&s, abandoned);
        break_it(&s);
        let g = FakeGates::new(&ROSTER, &[]);
        g.refresh(&s);
        let h = handler(&j, &s, &g);
        let a = if variant == "boss-play" {
            args(&[])
                .into_iter()
                .map(|(k, v)| {
                    if k == "also_in_namespaces" {
                        (k, Value::String("boss-dev,boss-play".into()))
                    } else {
                        (k, v)
                    }
                })
                .collect()
        } else {
            args(&[])
        };
        let refused = h.invoke(&a, &scope_ctx(JOB)).await;
        assert!(
            matches!(refused, Err(HandlerError::Permanent(_))),
            "{why}: {refused:?}"
        );
        let refusal = j
            .meta(JOB, "issue", "issue_refused")
            .unwrap_or_else(|| panic!("{why}: the refusal is on the step"));
        assert!(
            refusal.contains(says),
            "{why}: refused by the wrong clause: {refusal}"
        );
        assert_eq!(s.get(PRIMARY, "current").as_deref(), Some(old), "{why}");
        assert_eq!(s.get(PRIMARY, "next").as_deref(), Some(new), "{why}");
        assert!(
            !j.phases().iter().any(|p| p.ends_with("/verified")),
            "{why}: nothing recorded as live"
        );
    }
}

/// N2: a concurrent firing of the same packet stages its value first.
/// This firing's conditional write finds the Secret moved, takes the
/// value that landed, and records no mint of its own.
#[tokio::test]
async fn a_concurrent_firing_stages_one_value_and_records_one_mint() {
    // The winning firing recorded its mint and said so on the step before
    // this one read it: this firing records nothing.
    let mut marked = packet("2026-09-30T01:00:00Z");
    marked.steps.get_mut("issue").unwrap().metadata.insert(
        MINT_RECORDED.into(),
        json!("recorded by the winning firing"),
    );
    let j = jobs(&[(JOB, marked)]).await;
    let s = Arc::new(FakeSecrets::default());
    *s.races_with.lock().unwrap() = Some("the-other-firings-value-xxxxxxxxxxxxxxxxxxx".into());
    let g = FakeGates::new(&ROSTER, &[]);
    let h = handler(&j, &s, &g);
    fire(&h, &scope_ctx(JOB), &args(&[])).await;
    for ns in [PRIMARY, MIRROR] {
        assert_eq!(
            s.get(ns, "next").as_deref(),
            Some("the-other-firings-value-xxxxxxxxxxxxxxxxxxx"),
            "{ns}: the value that won the write is the one staged everywhere"
        );
    }
    assert!(
        !j.phases().iter().any(|p| p.ends_with("/minted")),
        "the losing firing records no second mint: {:?}",
        j.phases()
    );
    let issued = j.meta(JOB, "issue", "issued").unwrap();
    assert!(
        issued.contains("concurrent firing") && issued.contains("dropped unwritten"),
        "{issued}"
    );
}

/// F5 (review 363facd0): the winning firing wrote its value and died
/// before recording the mint. The losing firing completes issue and
/// install, so no later firing re-enters stage — it records the lost
/// mint itself.
#[tokio::test]
async fn a_concurrent_firing_records_the_mint_a_dead_winner_lost() {
    let j = jobs(&[(JOB, packet("2026-09-30T01:00:00Z"))]).await;
    let s = Arc::new(FakeSecrets::default());
    *s.races_with.lock().unwrap() = Some("the-other-firings-value-xxxxxxxxxxxxxxxxxxx".into());
    let g = FakeGates::new(&ROSTER, &[]);
    let h = handler(&j, &s, &g);
    fire(&h, &scope_ctx(JOB), &args(&[])).await;
    let minted: Vec<JsonValue> = j
        .rotations
        .lock()
        .unwrap()
        .iter()
        .filter(|(p, _)| p.ends_with("/minted"))
        .map(|(_, b)| b.clone())
        .collect();
    assert_eq!(minted.len(), 1, "{minted:?}");
    assert_eq!(minted[0]["recorded_late"], true);
    assert!(j.meta(JOB, "issue", MINT_RECORDED).is_some());
}

/// The k8s adapter's whole-Secret read and conditional write, against a
/// stub speaking the API server's shape: one GET returns every key and
/// the resourceVersion; a merge-patch carrying a stale resourceVersion
/// answers 409, which is `Moved`, not an error.
#[tokio::test]
async fn the_kube_store_reads_a_secret_whole_and_writes_only_at_its_version() {
    use crate::handlers::credential_issuer::KubeSecretStore;
    use base64::Engine as _;
    let enc = |s: &str| base64::engine::general_purpose::STANDARD.encode(s);
    let body = json!({"metadata": {"resourceVersion": "41","uid":"fixture-secret-uid"},
        "data": {"current": enc("cur"), "current.minted-for": enc(JOB)}});
    let patches: Captured = Default::default();
    let seen = patches.clone();
    let app = axum::Router::new().route(
        "/api/v1/namespaces/boss/secrets/boss-machine-token",
        axum::routing::get(move || {
            let b = body.clone();
            async move { axum::Json(b) }
        })
        .patch(move |axum::Json(p): axum::Json<JsonValue>| {
            let seen = seen.clone();
            async move {
                seen.lock().unwrap().push(("patch".into(), p.clone()));
                if p.pointer("/metadata/resourceVersion") == Some(&json!("41"))
                    && p.pointer("/metadata/uid")
                        .is_none_or(|uid| uid == &json!("fixture-secret-uid"))
                {
                    axum::http::StatusCode::OK
                } else {
                    axum::http::StatusCode::CONFLICT
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let kube = KubeSecretStore::new(format!("http://{addr}"), "sa-token", None).unwrap();
    let got = kube.read_secret("boss", SECRET).await.unwrap().unwrap();
    assert_eq!(got.version, "41");
    assert_eq!(got.uid, "fixture-secret-uid");
    assert_eq!(got.data["current"], "cur");
    assert_eq!(got.data["current.minted-for"], JOB);
    assert!(
        !format!("{got:?}").contains("cur\""),
        "Debug names keys, never values"
    );
    assert_eq!(
        kube.write_keys_at("boss", SECRET, &[("next", "n")], "41")
            .await,
        Ok(WriteAt::Written)
    );
    assert_eq!(
        kube.write_keys_at("boss", SECRET, &[("next", "n")], "40")
            .await,
        Ok(WriteAt::Moved)
    );
    assert_eq!(
        kube.write_keys_if(
            "boss",
            SECRET,
            &[
                ("next", "candidate"),
                ("recovery.witness", "value-free-witness")
            ],
            &got
        )
        .await,
        Ok(WriteAt::Written)
    );
    let mut replaced = got.clone();
    replaced.uid = "replacement-object".into();
    assert_eq!(
        kube.write_keys_if("boss", SECRET, &[("next", "refused")], &replaced)
            .await,
        Ok(WriteAt::Moved)
    );
    let mut unknown = got.clone();
    unknown.uid.clear();
    assert!(
        kube.write_keys_if("boss", SECRET, &[("next", "refused")], &unknown)
            .await
            .is_err()
    );
    let p = patches.lock().unwrap();
    assert_eq!(p[0].1["data"]["next"], enc("n"));
    assert_eq!(p.len(), 4, "missing identity refuses before any request");
    assert_eq!(
        p[2].1["metadata"],
        json!({"uid":"fixture-secret-uid","resourceVersion":"41"}),
        "bind both actual object and observed version"
    );
    assert_eq!(p[2].1["data"]["next"], enc("candidate"));
    assert_eq!(
        p[2].1["data"]["recovery.witness"],
        enc("value-free-witness"),
        "candidate and witness share one preconditioned request"
    );
}

#[tokio::test]
async fn the_actual_drain_uses_exact_declared_minutes_inside_the_hour_reader() {
    for minutes in [59, 61, 119, 1440, 10081] {
        let j = jobs(&[(JOB, packet("2026-09-30T01:00:00Z"))]).await;
        let s = Arc::new(FakeSecrets::default());
        for ns in [PRIMARY, MIRROR] {
            s.seed(ns, "current", "the-current-value-this-packet-promoted-xxxx");
            s.seed(ns, "current.minted-for", JOB);
            s.seed(
                ns,
                "previous",
                "the-old-value-still-in-the-previous-slot-xx",
            );
        }
        s.seed(
            PRIMARY,
            PROMOTED_AT,
            &(Utc::now() - chrono::Duration::days(10)).to_rfc3339(),
        );
        let g = FakeGates::new(&ROSTER, &[]);
        g.refresh(&s);
        // A known previous use 90 minutes ago is outside a 61-minute
        // drain, inside 119 minutes, and must not be clipped by rounding.
        g.with("jobs", |gate| {
            gate.extra_rows
                .push(previous_row(Utc::now() - chrono::Duration::minutes(90)))
        });
        {
            let mut packets = j.packets.lock().unwrap();
            let steps = &mut packets.get_mut(JOB).unwrap().steps;
            for slug in ["issue", "install", "verify"] {
                steps.get_mut(slug).unwrap().status = "completed".into();
            }
            steps.get_mut("revoke").unwrap().status = "ready".into();
        }
        let arguments = args(&[("phase", ADVANCE_PHASE)])
            .into_iter()
            .map(|(key, value)| {
                if key == "drain_minutes" {
                    (key, Value::String(minutes.to_string()))
                } else {
                    (key, value)
                }
            })
            .collect::<Vec<_>>();
        if minutes == 59 {
            assert!(matches!(
                Declaration::parse(&arguments),
                Err(HandlerError::Permanent(_))
            ));
            continue;
        }
        fire(&handler(&j, &s, &g), &tick_ctx(), &arguments).await;
        assert_eq!(
            j.status(JOB, "revoke") == "completed",
            minutes == 61,
            "exact declared interval {minutes}: {:?}",
            j.meta(JOB, "revoke", "revoke_deferred")
        );
        assert_eq!(s.get(PRIMARY, "previous").is_none(), minutes == 61);
    }
}

#[tokio::test]
async fn the_actual_drain_refuses_insufficient_misaligned_and_overlong_reader_intervals() {
    fn shorter(snapshot: &mut boss_core::gate_window::JoinedWindow) {
        snapshot.from += chrono::Duration::minutes(1);
        snapshot.observation.as_mut().unwrap().from = snapshot.from;
    }
    fn longer(snapshot: &mut boss_core::gate_window::JoinedWindow) {
        snapshot.from -= chrono::Duration::hours(1);
        snapshot.observation.as_mut().unwrap().from = snapshot.from;
    }
    fn shifted(snapshot: &mut boss_core::gate_window::JoinedWindow) {
        snapshot.now += chrono::Duration::minutes(1);
        snapshot.from += chrono::Duration::minutes(1);
        let obs = snapshot.observation.as_mut().unwrap();
        obs.now = snapshot.now;
        obs.from = snapshot.from;
    }
    for change in [
        shorter as fn(&mut boss_core::gate_window::JoinedWindow),
        longer,
        shifted,
    ] {
        let j = jobs(&[(JOB, packet("2026-09-30T01:00:00Z"))]).await;
        let s = Arc::new(FakeSecrets::default());
        for ns in [PRIMARY, MIRROR] {
            s.seed(ns, "current", "the-current-value-this-packet-promoted-xxxx");
            s.seed(ns, "current.minted-for", JOB);
            s.seed(
                ns,
                "previous",
                "the-old-value-still-in-the-previous-slot-xx",
            );
        }
        s.seed(
            PRIMARY,
            PROMOTED_AT,
            &(Utc::now() - chrono::Duration::days(2)).to_rfc3339(),
        );
        let g = FakeGates::new(&ROSTER, &[]);
        g.refresh(&s);
        *g.window_change.lock().unwrap() = Some(change);
        {
            let mut packets = j.packets.lock().unwrap();
            let steps = &mut packets.get_mut(JOB).unwrap().steps;
            for slug in ["issue", "install", "verify"] {
                steps.get_mut(slug).unwrap().status = "completed".into();
            }
            steps.get_mut("revoke").unwrap().status = "ready".into();
        }
        fire(
            &handler(&j, &s, &g),
            &tick_ctx(),
            &args(&[("phase", ADVANCE_PHASE)]),
        )
        .await;
        assert_ne!(j.status(JOB, "revoke"), "completed");
        assert!(s.get(PRIMARY, "previous").is_some() && s.get(MIRROR, "previous").is_some());
        assert!(
            j.meta(JOB, "revoke", "revoke_deferred")
                .unwrap()
                .contains("interval")
        );
    }
}

#[tokio::test]
async fn an_epoch_seen_between_polls_permanently_removes_the_down_at_verify_excuse() {
    durable_down_exception_case(false).await;
}

#[tokio::test]
async fn a_raced_durable_exception_write_does_not_claim_the_exemption_was_narrowed() {
    durable_down_exception_case(true).await;
}

async fn durable_down_exception_case(raced: bool) {
    let j = jobs(&[(JOB, packet("2026-09-30T01:00:00Z"))]).await;
    let s = Arc::new(FakeSecrets::default());
    for ns in [PRIMARY, MIRROR] {
        s.seed(ns, "current", "the-current-value-this-packet-promoted-xxxx");
        s.seed(ns, "current.minted-for", JOB);
        s.seed(
            ns,
            "previous",
            "the-old-value-still-in-the-previous-slot-xx",
        );
    }
    s.seed(
        PRIMARY,
        PROMOTED_AT,
        &(Utc::now() - chrono::Duration::days(2)).to_rfc3339(),
    );
    s.seed(PRIMARY, NOT_SERVED_AT_VERIFY, "sim-control");
    let g = FakeGates::new(&["jobs", "policy"], &["sim-control"]);
    g.refresh(&s);
    g.with("sim-control", |gate| {
        gate.restart(Utc::now() - chrono::Duration::minutes(10), true)
    });
    {
        let mut packets = j.packets.lock().unwrap();
        let steps = &mut packets.get_mut(JOB).unwrap().steps;
        for slug in ["issue", "install", "verify"] {
            steps.get_mut(slug).unwrap().status = "completed".into();
        }
        steps.get_mut("revoke").unwrap().status = "ready".into();
    }
    let h = handler(&j, &s, &g);
    let tick = args(&[("phase", ADVANCE_PHASE)]);
    if raced {
        *s.races_with.lock().unwrap() = Some("the-other-firings-value-xxxxxxxxxxxxxxxxxxx".into());
    }
    fire(&h, &tick_ctx(), &tick).await;
    if raced {
        assert_eq!(
            s.get(PRIMARY, NOT_SERVED_AT_VERIFY),
            Some("sim-control".into())
        );
        assert!(s.get(PRIMARY, "previous").is_some() && s.get(MIRROR, "previous").is_some());
        assert!(
            j.meta(JOB, "revoke", "revoke_deferred")
                .unwrap()
                .contains("moved"),
            "a lost CAS cannot claim the exemption was narrowed"
        );
        return;
    }
    assert_eq!(
        s.get(PRIMARY, NOT_SERVED_AT_VERIFY),
        None,
        "a durable sourced start between polls removes the recorded exemption"
    );
    assert!(s.get(PRIMARY, "previous").is_some());
    fire(&h, &tick_ctx(), &tick).await;
    assert_ne!(j.status(JOB, "revoke"), "completed");
    assert!(s.get(PRIMARY, "previous").is_some() && s.get(MIRROR, "previous").is_some());
    assert!(
        j.meta(JOB, "revoke", "revoke_deferred")
            .unwrap()
            .contains("sim-control")
    );
}

// The probe-reader credential's rotation — the same handler under
// `gate_slots = "reader"` — with fixtures of its own (design b35c22b4).
mod reader;
