//! A rotation must leave the machine-gate window as clean as it found it
//! (backlog 89fc511e, from review f09db7f0, F3).
//!
//! Every other test of this handler reaches the gate through
//! `MachineGate::accepts` — the answer, with no request behind it — so
//! none of them could see what the REQUEST did to the tally. Here each
//! port is the real `machine_gate::gated` router on a real socket, read
//! through `LocalGates`, the adapter production uses: the ask travels
//! the middleware, and the window is joined from the facts those gates
//! stated and from their own tallies.
//!
//! What it caught: the scope firing stages the value and asks every gate
//! about it in the same pass, about a minute before kubelet projects the
//! Secret. The gate answered `none` — and tallied the ask as a caller
//! presenting a mismatch, so ONE rotation of either credential left a
//! `would_refuse` on every gated port, and row C of the enforce order
//! (design b08725c2) flips only on 72 clean hours.
//!
//! And what the fix must keep (review 0c0e3f01, B1): the ask is not
//! erased. Each port names the asker in one `asked` row and one
//! `machine_gate.asked` fact, which is what lets a later reader — the
//! car's own probe among them — see that a rotation asked at all.

use super::*;

/// The reader fixture's gates, each behind its own listening router.
struct Routed {
    gates: Arc<Gates>,
    http: Arc<LocalGates>,
}

impl Routed {
    async fn serve(gates: &Arc<Gates>) -> Arc<Self> {
        let reals: Vec<(String, Arc<MachineGate>)> = gates
            .ports
            .lock()
            .unwrap()
            .iter()
            .map(|(name, port)| (name.clone(), Arc::clone(&port.real)))
            .collect();
        let mut services = Vec::new();
        for (name, real) in reals {
            let app = boss_core::machine_gate::gated(axum::Router::new(), real)
                .into_make_service_with_connect_info::<std::net::SocketAddr>();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            services.push((name, format!("http://{addr}")));
        }
        Arc::new(Self {
            gates: Arc::clone(gates),
            http: LocalGates::new(services),
        })
    }

    /// Kubelet refreshed the ESTATE Secret's mount on every port.
    fn refresh_estate(&self, secrets: &FakeSecrets) {
        let slots = SLOTS.map(|s| secrets.get(PRIMARY, s));
        for name in ROSTER {
            self.gates.with(name, |port| port.estate = slots.clone());
        }
    }

    /// Every caller shape `enforce` refuses that any port holds, live or
    /// on the log: `<port> row <key>` and `<port> fact <kind> <key>`.
    fn refusals(&self) -> Vec<String> {
        let mut found = Vec::new();
        for (name, port) in self.gates.ports.lock().unwrap().iter_mut() {
            for row in port.real.misses().rows {
                if row.key.presented.enforce_refuses() {
                    found.push(format!("{name} row {:?}", row.key));
                }
            }
            while let Ok(event) = port.rx.try_recv() {
                port.facts.push(event);
            }
            for fact in &port.facts {
                if fact.kind.ends_with(".would_refuse") || fact.kind.ends_with(".refused") {
                    found.push(format!("{name} fact {} {}", fact.kind, fact.payload["key"]));
                }
            }
        }
        found
    }

    /// The rotation's ask is ON THE RECORD (review 0c0e3f01, B1): every
    /// port holds exactly one `asked` row, naming the dispatcher on the
    /// accepts route, and stated it once on the log as
    /// `machine_gate.asked`.
    fn assert_the_ask_is_named(&self, when: &str) {
        for (name, port) in self.gates.ports.lock().unwrap().iter_mut() {
            let asked: Vec<MissRow> = port
                .real
                .misses()
                .rows
                .into_iter()
                .filter(|row| row.key.presented == Presented::Asked)
                .collect();
            assert_eq!(asked.len(), 1, "{when}: {name}: {asked:?}");
            let key = &asked[0].key;
            assert_eq!(
                (key.user.as_str(), key.method.as_str(), key.route.as_str()),
                (
                    "automation:dispatcher",
                    "GET",
                    boss_core::machine_gate::ACCEPTS_PATH
                ),
                "{when}: {name}"
            );
            assert_eq!(key.peer, "127.0.0.1", "{when}: {name}");
            while let Ok(event) = port.rx.try_recv() {
                port.facts.push(event);
            }
            let stated = port
                .facts
                .iter()
                .filter(|fact| fact.kind == "machine_gate.asked")
                .count();
            assert_eq!(stated, 1, "{when}: {name}: one fact per key");
        }
    }

    /// The 72-hour window row C reads, opened with an estate token the
    /// gates accept.
    async fn assert_clean(&self, estate: &str, when: &str) {
        assert_eq!(self.refusals(), Vec::<String>::new(), "{when}");
        let window = self.window(estate, 72).await.expect("the window reads");
        assert!(
            window.not_clean.is_empty(),
            "{when}: the window is not clean: {:?}",
            window.not_clean
        );
        // `not_clean` is about NOW: a fact that dirtied the window a
        // moment ago leaves it empty and moves `clean_since` instead. So
        // the whole 72 hours must be covered, with nothing dirty on the
        // log half at all.
        assert!(
            window.covers_requested_window,
            "{when}: clean only since {:?}, not for the 72 hours asked",
            window.clean_since
        );
        let dirty = window.log.as_ref().map(|log| log.dirty.len());
        assert_eq!(dirty, Some(0), "{when}: {:?}", window.log);
    }
}

#[async_trait]
impl GateReader for Routed {
    fn roster(&self) -> Vec<String> {
        self.http.roster()
    }
    async fn accepts(&self, service: &str, presented: &str) -> GateRead<Accepts> {
        self.http.accepts(service, presented).await
    }
    async fn misses(&self, service: &str, presented: &str) -> GateRead<Misses> {
        self.http.misses(service, presented).await
    }
    async fn window(
        &self,
        presented: &str,
        hours: i64,
    ) -> Result<boss_core::gate_window::JoinedWindow, String> {
        self.gates.window(presented, hours).await
    }
}

fn once() -> GatePoll {
    GatePoll {
        attempts: 1,
        interval: Duration::ZERO,
    }
}

/// The probe reader's first rotation, every firing of it, against gates
/// in `report`: the window is clean before, after the scope firing that
/// asks ahead of the projection, and after each tick.
#[tokio::test]
async fn a_reader_rotation_leaves_the_gate_window_clean() {
    let (j, s, g, _) = fresh().await;
    let routed = Routed::serve(&g).await;
    let h = CredentialRotateSelfIssued::with_estate_token(
        j.url.clone(),
        s.clone(),
        routed.clone(),
        once(),
        Arc::new(Source::fixed(Some(ESTATE.to_string()))),
    );
    routed.assert_clean(ESTATE, "before the rotation").await;

    // Staged, and asked about before any gate has read it.
    fire(&h, &rscope(JOB), &rargs(&[])).await;
    let why = j.meta(JOB, "verify", "verify_deferred").expect("deferred");
    assert!(why.contains("matched none"), "{why}");
    routed
        .assert_clean(ESTATE, "after the scope firing asked ahead of kubelet")
        .await;
    routed.assert_the_ask_is_named("after the scope firing");

    g.refresh(&s);
    rtick(&h).await.expect("the tick promotes");
    assert!(s.get("current").is_some(), "promoted");
    routed.assert_clean(ESTATE, "after the promotion").await;

    g.refresh(&s);
    rtick(&h).await.expect("the tick verifies");
    assert_eq!(j.status(JOB, "verify"), "completed");
    assert_eq!(j.status(JOB, "revoke"), "completed");
    routed.assert_clean(ESTATE, "after the rotation").await;
}

/// The estate machine token's rotation over an old value every caller
/// still sends: the same three firings, the same clean window, and the
/// old value kept as `previous` exactly as before.
#[tokio::test]
async fn an_estate_rotation_leaves_the_gate_window_clean() {
    let j = jobs(&[(JOB, packet("2026-09-30T01:00:00Z"))]).await;
    let s = Arc::new(FakeSecrets::default());
    for ns in [PRIMARY, MIRROR] {
        s.seed(ns, "current", ESTATE);
        s.seed(ns, "current.minted-for", OTHER);
    }
    let g = Gates::new(&ROSTER);
    let routed = Routed::serve(&g).await;
    let h = CredentialRotateSelfIssued::with_poll(j.url.clone(), s.clone(), routed.clone(), once());
    let tick = args(&[("phase", ADVANCE_PHASE)]);
    routed.assert_clean(ESTATE, "before the rotation").await;

    fire(&h, &scope_ctx(JOB), &args(&[])).await;
    let value = s.get(PRIMARY, "next").expect("next staged");
    let why = j.meta(JOB, "verify", "verify_deferred").expect("deferred");
    assert!(why.contains("matched none"), "{why}");
    routed
        .assert_clean(ESTATE, "after the scope firing asked ahead of kubelet")
        .await;
    routed.assert_the_ask_is_named("after the scope firing");

    routed.refresh_estate(&s);
    fire(&h, &tick_ctx(), &tick).await;
    assert_eq!(s.get(PRIMARY, "current").as_deref(), Some(value.as_str()));
    assert_eq!(s.get(PRIMARY, "previous").as_deref(), Some(ESTATE));
    routed.assert_clean(ESTATE, "after the promotion").await;

    routed.refresh_estate(&s);
    fire(&h, &tick_ctx(), &tick).await;
    assert_eq!(j.status(JOB, "verify"), "completed");
    // The drain is untouched: the old value is still `previous`, and the
    // revoke waits out its window.
    assert_ne!(j.status(JOB, "revoke"), "completed");
    let why = j.meta(JOB, "revoke", "revoke_deferred").unwrap();
    assert!(why.contains("drain window runs 1440 minutes"), "{why}");
    assert_eq!(s.get(PRIMARY, "previous").as_deref(), Some(ESTATE));
    routed.assert_clean(&value, "after the rotation").await;
}

/// Under `enforce` the ask ahead of the projection is answered 401, as
/// every unmatched value is, and the firing defers on it — named as an ask, and no
/// `refused` caller shape on any port. The mode move restarts each
/// tally, so this reads the rows and the facts, not a 72-hour verdict.
#[tokio::test]
async fn under_enforce_the_ask_is_still_refused_and_dirties_nothing() {
    let (j, s, g, _) = fresh().await;
    for name in ROSTER {
        g.with(name, |port| port.mode = Mode::Enforce);
    }
    let routed = Routed::serve(&g).await;
    let h = CredentialRotateSelfIssued::with_estate_token(
        j.url.clone(),
        s.clone(),
        routed.clone(),
        once(),
        Arc::new(Source::fixed(Some(ESTATE.to_string()))),
    );
    fire(&h, &rscope(JOB), &rargs(&[])).await;
    assert_eq!(s.get("current"), None, "nothing promoted");
    let why = j.meta(JOB, "verify", "verify_deferred").expect("deferred");
    assert!(
        why.contains("401") && why.contains("does not match"),
        "the gate still refuses the unmatched value: {why}"
    );
    assert_eq!(routed.refusals(), Vec::<String>::new());
    routed.assert_the_ask_is_named("under enforce, after the scope firing");

    g.refresh(&s);
    rtick(&h).await.expect("the tick promotes");
    assert!(
        s.get("current").is_some(),
        "promoted once every gate reads it"
    );
    assert_eq!(routed.refusals(), Vec::<String>::new());
}
