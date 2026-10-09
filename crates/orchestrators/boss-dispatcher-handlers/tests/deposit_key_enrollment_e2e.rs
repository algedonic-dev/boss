//! THE ENROLMENT ROAD, END TO END: the REAL `credential.prepare.ssh-deposit`
//! handler against the REAL jobs router on loopback, with the BUNDLED
//! protocol its rule row names (backlog 88379df3; delta review 76249509,
//! B1 and question 2).
//!
//! WHY THIS FILE EXISTS. Three review rounds each found this road
//! dead-ending one step deeper, and each time for the same reason: the
//! handler's own tests (`broker_transport_key.rs`) run against a STUB jobs
//! server that enforces nothing and serves whatever its fixture spells.
//!   * round 1: the rule's receipts were refused 403 by `written_by`;
//!   * round 2: its proposal was refused 403 on the human-only step;
//!   * round 3: the handler could not READ a packet at all — it
//!     deserialized the subject as `{kind, id}` where the jobs API serves
//!     `{subject_kind, id}`, so its FIRST read answered "malformed
//!     consumed field", a Permanent error, and after the human scope
//!     nothing was minted, for ever. The stub's fixture spelled the
//!     subject the handler's way, so 25 tests were green.
//! A real-router test that replays the handler's WRITE bodies (boss-jobs,
//! `platform_bundle_prepare_a_deposit_key.rs`) cannot see a READ. So this
//! one drives the handler itself: the packet is filed over HTTP, a person
//! scopes it, and the handler is invoked with the rule row's own args and
//! actor, read from the tree.
//!
//! WHAT IS STILL A STUB, and so is NOT proven here: the Secret store, the
//! key issuer, the credential registry row (instance data the operator
//! publishes), and the dispatcher delivering the real `step.done` event —
//! the payload below is built the way the handler's other tests build it.
use async_trait::async_trait;
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_dispatcher_handlers::handlers::broker_transport_key::{
    CredentialPrepareSshDeposit, IssuedKey, KeyIssuer, authorize_preparation, authorized_keys_line,
};
use boss_dispatcher_handlers::handlers::credential_issuer::{SecretData, SecretStore, WriteAt};
use boss_jobs::http::{JobsApiState, PresenceKey, router};
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, WorkflowRegistry};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

const PRESENCE: &[u8] = b"e2e-presence-fixture-key-0123456789";
const DAVID: &str = "emp-david";
const PUBLIC: &str = "ssh-ed25519 public-test-only";
const EVIL: &str = "ssh-ed25519 AAAAattacker";

/// Every machine identity review 76249509 measured rewriting the
/// proposal (204 each, before `written_by` was declared on `enroll`):
/// another automation, the OTHER broker rule, an agent by its id and by
/// its login (the door resolves it), and the dispatcher itself.
const OTHER_MACHINES: [&str; 5] = [
    "automation:other",
    "rule:broker-prepares-the-runner-deposit-key",
    "agent-claude",
    "claude@algedonic.dev",
    "system:dispatcher",
];

const MACHINE_TOKEN_RULE: &str = "broker-prepares-the-machine-token-deposit-key";
const RUNNER_RULE: &str = "broker-prepares-the-runner-deposit-key";

#[derive(Default)]
struct Memory {
    held: Mutex<SecretData>,
}
#[async_trait]
impl SecretStore for Memory {
    async fn read_key(&self, _: &str, _: &str, key: &str) -> Result<Option<String>, String> {
        Ok(self.held.lock().unwrap().data.get(key).cloned())
    }
    async fn write_key(&self, _: &str, _: &str, _: &str, _: &str) -> Result<(), String> {
        panic!("preparation never writes a Secret unconditionally")
    }
    async fn read_secret(&self, ns: &str, _: &str) -> Result<Option<SecretData>, String> {
        assert_eq!(ns, "boss");
        Ok(Some(self.held.lock().unwrap().clone()))
    }
    async fn write_keys_at(
        &self,
        _: &str,
        _: &str,
        entries: &[(&str, &str)],
        version: &str,
    ) -> Result<WriteAt, String> {
        let mut held = self.held.lock().unwrap();
        assert_eq!(version, held.version);
        held.data
            .extend(entries.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        held.version = "next".into();
        Ok(WriteAt::Written)
    }
}

struct Issuer(Mutex<usize>);
#[async_trait]
impl KeyIssuer for Issuer {
    fn validate_pair(&self, private_key: &str, public_key: &str) -> Result<(), String> {
        if private_key == "private-test-only" && public_key == PUBLIC {
            Ok(())
        } else {
            Err("fixture pair mismatch".into())
        }
    }
    async fn mint(&self) -> Result<IssuedKey, String> {
        *self.0.lock().unwrap() += 1;
        Ok(IssuedKey::new("private-test-only".into(), PUBLIC.into()))
    }
}

struct Roster;
#[async_trait]
impl boss_jobs::owner_resolution::RosterLookup for Roster {
    async fn active_holders(&self, _: &str) -> Result<Vec<String>, String> {
        Ok(vec![DAVID.into()])
    }
    async fn is_active_employee(&self, id: &str) -> Result<bool, String> {
        Ok(id == DAVID)
    }
}

/// A rule row as the tree declares it: its name, and its handler args
/// (each a quoted string literal in the row's expression language).
struct Rule {
    name: String,
    args: Vec<(String, String)>,
}
impl Rule {
    fn read(name: &str) -> Self {
        let raw = std::fs::read_to_string(
            boss_testing::repo_root().join(format!("infra/dispatcher/rules/{name}.toml")),
        )
        .unwrap();
        let value: toml::Value = toml::from_str(&raw).unwrap();
        let row = &value["rule"][0];
        assert_eq!(row["name"].as_str(), Some(name));
        assert_eq!(
            row["do"][0]["handler"].as_str(),
            Some("credential.prepare.ssh-deposit")
        );
        let args = row["do"][0]["args"]
            .as_table()
            .unwrap()
            .iter()
            .map(|(k, v)| {
                let s = v.as_str().unwrap();
                assert!(s.starts_with('"') && s.ends_with('"'), "{k}: {s}");
                (k.clone(), s[1..s.len() - 1].to_string())
            })
            .collect();
        Self {
            name: name.into(),
            args,
        }
    }
    fn arg(&self, key: &str) -> &str {
        self.args
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
            .unwrap_or_else(|| panic!("rule {} declares no {key}", self.name))
    }
    /// The actor the handler signs its jobs-API writes as.
    fn actor(&self) -> String {
        format!("rule:{}", self.name)
    }
}

fn user(id: &str) -> String {
    json!({"id":id,"role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}).to_string()
}

struct Api {
    url: String,
    http: reqwest::Client,
}
impl Api {
    async fn call(
        &self,
        method: &str,
        path: &str,
        id: &str,
        presence: Option<&str>,
        body: Option<Value>,
    ) -> (u16, Value) {
        let mut req = self
            .http
            .request(method.parse().unwrap(), format!("{}{path}", self.url))
            .header("content-type", "application/json")
            .header("x-boss-user", user(id))
            .header("x-sim-origin", "false");
        if let Some(p) = presence {
            req = req.header("x-boss-presence", p);
        }
        if let Some(b) = body {
            req = req.body(b.to_string());
        }
        let resp = req.send().await.unwrap();
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap();
        (status, serde_json::from_str(&text).unwrap_or(json!(text)))
    }
    async fn patch(&self, step_path: &str, id: &str, body: Value) -> (u16, Value) {
        self.call(
            "PATCH",
            &format!("{step_path}/metadata"),
            id,
            None,
            Some(body),
        )
        .await
    }
    async fn finish(&self, step_path: &str, id: &str, presence: Option<&str>) -> (u16, Value) {
        self.call(
            "PUT",
            step_path,
            id,
            presence,
            Some(json!({"status":"completed"})),
        )
        .await
    }
    async fn job(&self, id: &str) -> Value {
        let (s, v) = self
            .call("GET", &format!("/api/jobs/{id}"), DAVID, None, None)
            .await;
        assert_eq!(s, 200, "{v}");
        v
    }
}

fn step<'a>(job: &'a Value, slug: &str) -> &'a Value {
    job["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["spec_slug"] == slug)
        .unwrap_or_else(|| panic!("no step {slug}"))
}

fn step_path(job: &Value, slug: &str) -> String {
    format!(
        "/api/jobs/{}/steps/{}",
        job["id"].as_str().unwrap(),
        step(job, slug)["id"].as_str().unwrap()
    )
}

fn statuses(job: &Value) -> String {
    job["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            format!(
                "{}={}",
                s["spec_slug"].as_str().unwrap_or("?"),
                s["status"].as_str().unwrap_or("?")
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A presence ticket over the step AS IT STANDS, as the passkey ceremony
/// would mint it for `who`.
fn ticket(job: &Value, slug: &str, who: &str) -> String {
    let s = step(job, slug);
    boss_core::presence::PresenceTicket {
        i: who.into(),
        s: s["id"].as_str().unwrap().into(),
        h: boss_core::job::step_shape_hash(s["title"].as_str().unwrap(), &s["metadata"]),
        n: format!("e2e-nonce-{}", uuid::Uuid::new_v4()),
        e: boss_core::presence::now_epoch() + 600,
    }
    .encode(PRESENCE)
    .unwrap()
}

/// The real jobs router with the two bundled preparation protocols, on
/// loopback. ONE stub route beside it: the credential registry row.
async fn serve() -> Api {
    let bundle =
        boss_jobs::seed_loader::load_workflows(boss_jobs::registry::platform_bundle_path())
            .expect("the platform bundle validates");
    let kinds = Arc::new(InMemoryWorkflows::for_fixture());
    for row in bundle {
        if row.kind == "prepare-the-machine-token-deposit-key"
            || row.kind == "prepare-a-deposit-key"
        {
            kinds.seed(row).unwrap();
        }
    }
    let jobs = Arc::new(InMemoryJobs::new());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let agents: Arc<dyn boss_jobs::agents::AgentsRegistry> = Arc::new(
        boss_jobs::agents::InMemoryAgents::new()
            .with_agent("agent-claude", ["claude@algedonic.dev"]),
    );
    let door = Arc::new(boss_jobs::agents::LoginDoor::new(
        agents.clone(),
        DomainPublisher::new(bus_dyn.clone(), "jobs"),
    ));
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow(
                "platform-admin",
                Action::Create,
                Resource::job(),
                Scope::All,
            )
            .allow("platform-admin", Action::Read, Resource::job(), Scope::All)
            .allow(
                "platform-admin",
                Action::Update,
                Resource::job(),
                Scope::All,
            )
            .allow(
                "platform-admin",
                Action::Update,
                Resource::step(),
                Scope::All,
            )
            .allow(
                "platform-admin",
                Action::SignOff,
                Resource::new("step-signoff:platform-admin"),
                Scope::All,
            )
            .build(),
    );
    let app = router(JobsApiState {
        kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
        roster: Some(Arc::new(Roster)),
        presence_key: Some(Arc::new(PresenceKey::fixed(PRESENCE.to_vec()))),
        agent_budget: Some(Arc::new(boss_jobs::agent_budget::BudgetDoor {
            agents,
            runs: Arc::new(boss_jobs::agent_runs::InMemoryAgentRuns::new(vec![])),
        })),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus.clone(),
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    })
    .layer(axum::middleware::from_fn(
        boss_policy_client::request_context_middleware,
    ))
    .layer(axum::middleware::from_fn_with_state(
        door,
        boss_jobs::agents::resolve_login,
    ))
    // STUB: the registry row of whichever key is asked for, stored where
    // both rule rows declare theirs (Secret boss/<credential id>).
    .route(
        "/api/credentials/{id}",
        axum::routing::get(
            |axum::extract::Path(id): axum::extract::Path<String>| async move {
                axum::Json(json!({"id": id, "kind": "ssh-transport-key",
                                  "storage_location": format!("Secret boss/{id}")}))
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Api {
        url,
        http: reqwest::Client::new(),
    }
}

struct Broker {
    rule: Rule,
    store: Arc<Memory>,
    issuer: Arc<Issuer>,
}
impl Broker {
    fn new(rule: &str) -> Self {
        Self {
            rule: Rule::read(rule),
            store: Arc::new(Memory::default()),
            issuer: Arc::new(Issuer(Mutex::new(0))),
        }
    }
    fn mints(&self) -> usize {
        *self.issuer.0.lock().unwrap()
    }
    /// The real handler, with the rule row's own args and name, on the
    /// event the scope step's completion publishes.
    async fn invoke(&self, api: &Api, job_id: &str, scope_id: &str) -> Result<(), String> {
        use boss_dispatcher::rules::{
            expr::Value as V,
            handler::{Handler, InvocationContext},
        };
        let args: Vec<(String, V)> = self
            .rule
            .args
            .iter()
            .map(|(k, v)| (k.clone(), V::String(v.clone())))
            .collect();
        CredentialPrepareSshDeposit::new(&api.url, self.store.clone(), self.issuer.clone())
            .invoke(
                &args,
                &InvocationContext {
                    rule_name: self.rule.name.clone(),
                    triggering_event_id: "event-id".into(),
                    triggering_topic: "step.done.credential-rotation".into(),
                    event_payload: json!({"job_id": job_id, "step_id": scope_id,
                        "kind": "credential-rotation", "subject_kind": "custom",
                        "subject_id": self.rule.arg("credential_id"), "metadata": {}}),
                    event_timestamp: None,
                },
            )
            .await
            .map_err(|e| e.to_string())
    }
}

/// File the rule's packet over HTTP, as a person.
async fn file(api: &Api, rule: &Rule) -> String {
    let job = boss_core::job::Job {
        status: boss_core::job::JobStatus::Open,
        ..boss_core::job::Job::new(
            rule.arg("protocol_kind"),
            boss_core::job::Subject::new("custom", rule.arg("credential_id")),
            "Prepare a deposit key",
            DAVID,
            boss_core::job::Priority::Standard,
            chrono::NaiveDate::from_ymd_opt(2026, 10, 7).unwrap(),
        )
    };
    let (s, v) = api
        .call(
            "POST",
            "/api/jobs",
            DAVID,
            None,
            Some(serde_json::to_value(&job).unwrap()),
        )
        .await;
    assert_eq!(s, 201, "filing {}: {v}", rule.arg("protocol_kind"));
    v["id"].as_str().unwrap().to_string()
}

/// Scope the packet: refused to the rule itself, done by a person.
/// Answers the scope step's id.
async fn scope(api: &Api, id: &str, rule: &Rule) -> String {
    let job = api.job(id).await;
    let path = step_path(&job, "scope");
    let fields = json!({"credential": rule.arg("credential_id"), "reason": "boss-gcp can read no Secret",
        "locations": format!("Secret boss/{}", rule.arg("secret_name")), "consumers": "the push"});
    let (s, v) = api.patch(&path, &rule.actor(), fields.clone()).await;
    assert_eq!(s, 403, "a machine scoped the preparation: {v}");
    let (s, v) = api.patch(&path, DAVID, fields).await;
    assert_eq!(s, 204, "{v}");
    // `scope` is human_only: David's asserted id alone no longer
    // completes it (item 570c66e9, 2026-10-07 — the machine door takes
    // any id on the caller's word). His passkey over the scope as he
    // wrote it does.
    let (s, v) = api.finish(&path, DAVID, None).await;
    assert_eq!(
        s, 422,
        "a human-only scope completed on an asserted id: {v}"
    );
    assert_eq!(v["human_only"], true, "{v}");
    let tap = ticket(&api.job(id).await, "scope", DAVID);
    let (s, v) = api.finish(&path, DAVID, Some(&tap)).await;
    assert_eq!(s, 204, "{v}");
    step(&job, "scope")["id"].as_str().unwrap().to_string()
}

/// THE MACHINE TOKEN'S DEPOSIT KEY, FROM SCOPE TO `prepared`, with no
/// hand-written receipt anywhere on the road.
#[tokio::test]
async fn the_machine_token_keys_enrollment_runs_from_scope_to_prepared_through_the_real_router() {
    let api = serve().await;
    let broker = Broker::new(MACHINE_TOKEN_RULE);
    let rule = &broker.rule;
    let actor = rule.actor();
    let credential = rule.arg("credential_id").to_string();
    let id = file(&api, rule).await;
    let scope_id = scope(&api, &id, rule).await;
    let scoped = api.job(&id).await;
    let enroll = step_path(&scoped, "enroll");
    assert_eq!(
        statuses(&scoped),
        "scope=completed issue=ready install=pending enroll=pending verify=pending \
         revoke=pending prepared=pending abandoned=pending"
    );

    // -- B1: THE HANDLER'S FIRST READ. The packet exactly as the real
    //    router serves it must authorize; the subject is boss-core's
    //    `Subject`, serialized `{subject_kind, id}`.
    assert_eq!(
        scoped["subject"],
        json!({"subject_kind": "custom", "id": credential}),
        "the jobs API serves the subject as boss_core::job::Subject"
    );
    if let Err(why) = authorize_preparation(
        &scoped,
        &id,
        &scope_id,
        rule.arg("protocol_kind"),
        &credential,
    ) {
        panic!("the handler cannot read the packet the jobs API serves: {why}");
    }

    // -- the broker: ONE invocation, one mint, both receipts, the proposal.
    let outcome = broker.invoke(&api, &id, &scope_id).await;
    let prepared = api.job(&id).await;
    assert_eq!(outcome, Ok(()), "after: {}", statuses(&prepared));
    assert_eq!(broker.mints(), 1);
    assert_eq!(
        statuses(&prepared),
        "scope=completed issue=completed install=completed enroll=ready verify=pending \
         revoke=pending prepared=pending abandoned=pending",
        "enroll is ready only once install is done"
    );
    for slug in ["issue", "install"] {
        let s = step(&prepared, slug);
        assert_eq!(s["completed_by"], format!("automation:{actor}"), "{slug}");
        assert_eq!(s["metadata"]["public_key"], PUBLIC, "{slug}");
    }
    let line = authorized_keys_line(rule.arg("forced_command"), PUBLIC);
    let proposal = json!({"receiver_host": rule.arg("receiver_host"),
        "purpose": rule.arg("purpose"), "authorized_keys_line": line});
    let on_enroll = |job: &Value| {
        let m = &step(job, "enroll")["metadata"];
        json!({"receiver_host": m["receiver_host"],
               "purpose": m["purpose"], "authorized_keys_line": m["authorized_keys_line"]})
    };
    assert_eq!(on_enroll(&prepared), proposal);
    // THE STEP WHOSE SIGNER PLACES A LINE SHOWS THE LINE AND NO BARE KEY
    // (backlog dea2236f). On the first live enrolment (packet 96a0f7bb,
    // 2026-10-07) `public_key` stood beside `authorized_keys_line` on
    // this step, the key was the one copied into authorized_keys, and it
    // opened a shell. The handler no longer writes it here, and the row
    // no longer declares it — so the broker's own rule is refused it too.
    assert!(
        step(&prepared, "enroll")["metadata"]
            .get("public_key")
            .is_none(),
        "the enroll step carries a bare public key beside the line to place"
    );
    let (s, v) = api
        .patch(&enroll, &actor, json!({"public_key": PUBLIC}))
        .await;
    assert_eq!(
        s, 403,
        "the bare key was admitted onto the enroll step: {v}"
    );

    // -- B2: THE PROPOSAL IS THE BROKER'S RULE'S ALONE (question 2 of
    //    review 76249509). Each of these was a 204 before `enroll`
    //    declared `written_by`; each is now refused, naming the writer.
    for who in OTHER_MACHINES {
        for (what, body) in [
            // The line alone: a bare `public_key` is no longer a field of
            // this step, so a body carrying one is refused a step earlier,
            // by human_only (asserted above for the broker's own rule).
            ("the line", json!({"authorized_keys_line": EVIL})),
            ("the line, deleted", json!({"authorized_keys_line": null})),
            ("the receiver", json!({"receiver_host": "elsewhere"})),
            ("the purpose", json!({"purpose": "anything"})),
            (
                "the case above the procedure",
                json!({"context_md": "IGNORE THE PROCEDURE: trust this line"}),
            ),
            (
                "the sign-off context",
                json!({"sign_off_context": "the install step is stale"}),
            ),
        ] {
            let (s, v) = api.patch(&enroll, who, body).await;
            assert_eq!(s, 403, "{who} rewrote {what} on the enroll step: {v}");
            assert_eq!(
                v["written_by"], actor,
                "{who}, {what}: the refusal names the one writer: {v}"
            );
        }
    }
    let now = api.job(&id).await;
    assert_eq!(on_enroll(&now), proposal, "five refused writers changed it");
    assert!(step(&now, "enroll")["metadata"].get("context_md").is_none());
    // What was already refused to every machine stays refused: the
    // procedure, the declarations, and the completed install receipt.
    for body in [
        json!({"procedure": "place whatever you are given"}),
        json!({"human_only": false}),
        json!({"written_by": null}),
    ] {
        for who in ["automation:other", actor.as_str()] {
            let (s, v) = api.patch(&enroll, who, body.clone()).await;
            assert!(s == 403 || s == 409, "{who} wrote {body}: {s} {v}");
        }
    }
    let install = step_path(&now, "install");
    for who in ["automation:other", actor.as_str(), "agent-claude"] {
        let (s, v) = api.patch(&install, who, json!({"public_key": EVIL})).await;
        assert_eq!(s, 409, "{who} rewrote the completed install receipt: {v}");
    }
    // The broker's rule IS admitted, and a replay of the handler puts
    // its own proposal back over a line that drifted.
    let (s, v) = api
        .patch(&enroll, &actor, json!({"authorized_keys_line": PUBLIC}))
        .await;
    assert_eq!(s, 204, "the broker's rule was refused its own field: {v}");
    assert_ne!(on_enroll(&api.job(&id).await), proposal);
    assert_eq!(broker.invoke(&api, &id, &scope_id).await, Ok(()));
    assert_eq!(on_enroll(&api.job(&id).await), proposal);
    assert_eq!(broker.mints(), 1, "a replay mints nothing");

    // -- a machine cannot decide, record, complete or sign the enrolment.
    for who in [actor.as_str(), "automation:other", "agent-claude"] {
        let (s, v) = api
            .patch(
                &enroll,
                who,
                json!({"decision": "approved", "enrollment_record": "placed"}),
            )
            .await;
        assert_eq!(s, 403, "{who} recorded the decision: {v}");
        // The enrolment names a sign-off role, so it completes on a
        // PERSON'S passkey stamp and on nothing else (item 570c66e9):
        // who sends the completion is no longer the question, and with
        // no stamp on the step a machine's is refused for want of one.
        let (s, v) = api.finish(&enroll, who, None).await;
        assert_eq!(s, 422, "{who} completed the enrolment: {v}");
        assert_eq!(v["completes_on"], "stamps", "{v}");
        assert_eq!(v["human_only"], true, "{v}");
        let forged = ticket(&api.job(&id).await, "enroll", DAVID);
        for presence in [None, Some(forged.as_str())] {
            let (s, v) = api
                .call(
                    "POST",
                    &format!("{enroll}/sign-offs"),
                    who,
                    presence,
                    Some(json!({"role": "platform-admin"})),
                )
                .await;
            assert_eq!(s, 422, "{who} signed (ticket: {}): {v}", presence.is_some());
        }
    }

    // -- the person: the decision, the record, then the passkey.
    let (s, v) = api
        .patch(
            &enroll,
            DAVID,
            json!({"decision": "approved",
                   "enrollment_record": "line placed in the deposit account's authorized_keys"}),
        )
        .await;
    assert_eq!(s, 204, "{v}");
    let (s, v) = api.finish(&enroll, DAVID, None).await;
    assert_eq!(s, 422, "the enrolment completed without presence: {v}");
    let tap = ticket(&api.job(&id).await, "enroll", DAVID);
    let (s, v) = api
        .call(
            "POST",
            &format!("{enroll}/sign-offs"),
            DAVID,
            Some(&tap),
            Some(json!({"role": "platform-admin"})),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    // The stamp carries the completion: it is sent bare, with no second
    // ticket (design 1ce67f7e).
    let (s, v) = api.finish(&enroll, DAVID, None).await;
    assert_eq!(s, 204, "{v}");
    let enrolled = api.job(&id).await;
    assert_eq!(step(&enrolled, "enroll")["completed_by"], DAVID);
    assert_eq!(
        on_enroll(&enrolled),
        proposal,
        "what was signed is the broker's proposal"
    );
    // Signed is frozen, for the broker's rule too; a replay is quiet.
    let (s, v) = api
        .patch(&enroll, &actor, json!({"authorized_keys_line": EVIL}))
        .await;
    assert_eq!(s, 409, "{v}");
    assert_eq!(broker.invoke(&api, &id, &scope_id).await, Ok(()));
    assert_eq!(broker.mints(), 1);

    // -- verify and revoke are a person's, each on its own effect.
    for (slug, field) in [("verify", "verified"), ("revoke", "revoked")] {
        let now = api.job(&id).await;
        assert_eq!(step(&now, slug)["status"], "ready", "{}", statuses(&now));
        let path = step_path(&now, slug);
        for who in [actor.as_str(), "agent-claude"] {
            let (s, v) = api
                .patch(&path, who, json!({field: "machine says so"}))
                .await;
            assert_eq!(s, 403, "{who} wrote {slug}.{field}: {v}");
            let (s, v) = api.finish(&path, who, None).await;
            assert_eq!(s, 403, "{who} completed {slug}: {v}");
        }
        let (s, v) = api
            .patch(&path, DAVID, json!({field: "observed effect"}))
            .await;
        assert_eq!(s, 204, "{v}");
        // Human-only, with no sign-off role: the person's own passkey
        // over the step as they wrote it, on the completing request.
        let (s, v) = api.finish(&path, DAVID, None).await;
        assert_eq!(s, 422, "{slug} completed on an asserted id: {v}");
        let tap = ticket(&api.job(&id).await, slug, DAVID);
        let (s, v) = api.finish(&path, DAVID, Some(&tap)).await;
        assert_eq!(s, 204, "{v}");
    }
    let end = api.job(&id).await;
    assert_eq!(
        statuses(&end),
        "scope=completed issue=completed install=completed enroll=completed verify=completed \
         revoke=completed prepared=ready abandoned=pending",
        "the road ends with the `prepared` terminal ready and `abandoned` never opened"
    );
    let (s, v) = api.finish(&step_path(&end, "prepared"), DAVID, None).await;
    assert_eq!(s, 204, "{v}");
    let closed = api.job(&id).await;
    assert_eq!(
        (
            closed["status"].clone(),
            closed["metadata"]["outcome"].clone()
        ),
        (json!("closed"), json!("prepared")),
        "the packet closes `prepared`"
    );
}

/// NOTHING IS PLANTED ON THE PERSON'S STEP BEFORE THE BROKER WRITES.
/// Review 76249509 planted a proposal on `enroll` before scope as
/// `automation:other` (204): the handler then stopped, nothing was
/// minted, and the attacker's line stayed on the step a person would
/// later open. With `written_by` on `enroll` the plant is refused, for
/// every machine identity that is not the broker's rule, and the road
/// then runs as if nobody had tried.
#[tokio::test]
async fn no_other_machine_plants_a_proposal_before_the_broker_writes() {
    let api = serve().await;
    let broker = Broker::new(MACHINE_TOKEN_RULE);
    let id = file(&api, &broker.rule).await;
    let enroll = step_path(&api.job(&id).await, "enroll");
    for who in OTHER_MACHINES {
        let (s, v) = api
            .patch(
                &enroll,
                who,
                json!({"authorized_keys_line": EVIL,
                       "receiver_host": "boss-gcp", "purpose": "estate machine token deposit"}),
            )
            .await;
        assert_eq!(s, 403, "{who} planted a proposal before scope: {v}");
        assert_eq!(v["written_by"], broker.rule.actor(), "{who}: {v}");
    }
    let scope_id = scope(&api, &id, &broker.rule).await;
    assert_eq!(broker.invoke(&api, &id, &scope_id).await, Ok(()));
    let job = api.job(&id).await;
    assert_eq!(step(&job, "enroll")["status"], "ready");
    assert_eq!(
        step(&job, "enroll")["metadata"]["authorized_keys_line"],
        authorized_keys_line(broker.rule.arg("forced_command"), PUBLIC)
    );
    assert_eq!(broker.mints(), 1);
}

/// THE RUNNER KEY'S ROAD, SAME HANDLER, GENERIC PROTOCOL — how far it
/// gets (1e50e66b). The subject read (B1) stopped it at its first read
/// exactly as it stopped the machine token's key; with that repaired it
/// now reaches its OWN known defect, item 3dc0b247: prepare-a-deposit-key
/// declares the three proposal fields executor-filled on the human-only
/// `enroll`, so the broker's proposal is refused there. NOT repaired
/// here — it is a new version of a row the runner's delivery owns. When
/// it is, this leg turns: delete it, and extend the leg above to the
/// runner's rule.
///
/// What it leaves, on the record: the pair IS minted into the Secret
/// before the refusal, no receipt is written, `issue` stays ready, and a
/// replay finds the same pair and mints nothing more.
#[tokio::test]
async fn the_runner_keys_road_now_reaches_the_generic_protocols_enroll_refusal() {
    let api = serve().await;
    let broker = Broker::new(RUNNER_RULE);
    let rule = &broker.rule;
    assert_eq!(rule.arg("protocol_kind"), "prepare-a-deposit-key");
    let id = file(&api, rule).await;
    let scope_id = scope(&api, &id, rule).await;
    let scoped = api.job(&id).await;
    assert!(
        authorize_preparation(
            &scoped,
            &id,
            &scope_id,
            rule.arg("protocol_kind"),
            rule.arg("credential_id")
        )
        .is_ok(),
        "the runner rule's first read"
    );
    for attempt in 1..=2 {
        let outcome = broker.invoke(&api, &id, &scope_id).await;
        let why = outcome.expect_err(
            "prepare-a-deposit-key's enroll now takes the broker's proposal — 3dc0b247 is \
             repaired: delete this leg and say so on 1e50e66b",
        );
        assert!(
            why.contains("403") && !why.contains("malformed consumed field"),
            "attempt {attempt}: the stop is the enroll refusal, not the read: {why}"
        );
        let now = api.job(&id).await;
        assert_eq!(
            statuses(&now),
            "scope=completed issue=ready install=pending enroll=pending verify=pending \
             revoke=pending prepared=pending abandoned=pending",
            "attempt {attempt}"
        );
        assert!(
            step(&now, "enroll")["metadata"].get("public_key").is_none(),
            "attempt {attempt}"
        );
        assert_eq!(broker.mints(), 1, "attempt {attempt}: one pair, never two");
    }
}
