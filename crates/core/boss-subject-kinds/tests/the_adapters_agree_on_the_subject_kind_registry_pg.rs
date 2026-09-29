//! The Subject Kind registry answers the same on both
//! `SubjectKindRepository` adapters — reliability mechanism C of design
//! 3036296f, "adapters agree" (`boss_testing::adapters_agree!`), on the
//! tenth port it reaches (backlog be459ab9), after the station and
//! job-edges registries: the three routing ports the census orders
//! first. `exists_active` is the hot-path validator every write that
//! sets a Subject's kind calls, so a double that answered it
//! differently from Postgres would pass a door test production refuses.
//!
//! WHY IT EXISTS. The reads — `get`, `exists_active`, `list_active`,
//! `children_of` — were stated for the double alone
//! (`in_memory::tests`) and for Postgres by nothing. The one write,
//! `patch_metadata`, was held to both adapters by two hand-rolled
//! comparisons in `subject_kinds_pg.rs`; this file is the one statement
//! of every method of the port, each case holding each adapter to its
//! own stated answer, and those comparisons move here.
//!
//! The world: the same `suite-` rows, INSERTed into Postgres (a kind is
//! born in a migration or a tenant seed; the port has no birth door,
//! backlog ca7bf46c) and handed to the double's constructor. The
//! migrations seed the platform kinds into every Postgres database and
//! the double holds only what it is handed, so every unfiltered read is
//! judged over `suite-` kinds. Instants are whole seconds.
//!
//! What is NOT compared: the ORDER of `list_active` and `children_of`.
//! The port promises none. Both adapters happen to sort — by
//! `(sort_order, kind)` and by `kind` — but Postgres breaks a tie in
//! `kind` by the database's locale and the double by bytes, so the two
//! can disagree on a hyphenated tie; each answer is sorted here before
//! it is compared, and that latent difference is reported on the item
//! rather than turned into a promise by a test.

use boss_core::actor::ActorId;
use boss_core::publisher::EventStamp;
use boss_subject_kinds::port::{
    MetadataChange, SUBJECT_KIND_UPDATED, SubjectKindError, apply_change,
};
use boss_subject_kinds::{
    InMemorySubjectKinds, PgSubjectKinds, SubjectKind, SubjectKindRepository,
};
use chrono::{DateTime, TimeZone, Utc};
use serde_json::{Map, Value, json};

/// The registry under test and the facts its writes left, read the way
/// each adapter keeps them.
trait World {
    type R: SubjectKindRepository;
    fn repo(&self) -> &Self::R;
    /// Every `subject_kind.updated` payload about a `suite-` kind, in
    /// the order recorded.
    async fn facts(&self) -> Vec<Value>;
}

fn is_suite_fact(payload: &Value) -> bool {
    payload["kind"]
        .as_str()
        .is_some_and(|k| k.starts_with("suite-"))
}

struct InMemory(InMemorySubjectKinds);

impl World for InMemory {
    type R = InMemorySubjectKinds;
    fn repo(&self) -> &InMemorySubjectKinds {
        &self.0
    }
    async fn facts(&self) -> Vec<Value> {
        self.0
            .recorded_events()
            .into_iter()
            .filter(|e| e.kind == SUBJECT_KIND_UPDATED && is_suite_fact(&e.payload))
            .map(|e| e.payload)
            .collect()
    }
}

struct Postgres {
    repo: PgSubjectKinds,
    pool: sqlx::PgPool,
}

impl World for Postgres {
    type R = PgSubjectKinds;
    fn repo(&self) -> &PgSubjectKinds {
        &self.repo
    }
    async fn facts(&self) -> Vec<Value> {
        let rows: Vec<Value> =
            sqlx::query_scalar("SELECT payload FROM event_outbox WHERE kind = $1 ORDER BY id")
                .bind(SUBJECT_KIND_UPDATED)
                .fetch_all(&self.pool)
                .await
                .expect("read the outbox");
        rows.into_iter().filter(is_suite_fact).collect()
    }
}

/// Land the suite's rows in a fresh database the way a seed does.
async fn seed(pool: &sqlx::PgPool) {
    for k in rows() {
        sqlx::query(
            "INSERT INTO subject_kinds
                (kind, label, parent_kind, description, owning_team, metadata,
                 sort_order, retired_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(&k.kind)
        .bind(&k.label)
        .bind(&k.parent_kind)
        .bind(&k.description)
        .bind(&k.owning_team)
        .bind(&k.metadata)
        .bind(k.sort_order)
        .bind(k.retired_at)
        .execute(pool)
        .await
        .expect("a suite kind lands");
    }
}

boss_testing::adapters_agree! {
    adapters {
        in_memory => (InMemory(InMemorySubjectKinds::new(rows())), ()),
        postgres => {
            let db = boss_testing::TestDb::new().await;
            seed(&db.pool).await;
            let world = Postgres { repo: PgSubjectKinds::new(db.pool.clone()), pool: db.pool.clone() };
            (world, db)
        },
    }
    cases {
        get_answers_every_row_retired_or_not_and_none_for_a_stranger,
        exists_active_is_true_only_for_a_live_kind,
        list_active_is_every_live_kind_and_no_retired_one,
        children_of_walks_one_level_of_live_children,
        a_patch_leaves_its_change_and_one_fact_and_a_restatement_leaves_neither,
        a_reserved_key_or_a_stranger_is_refused_and_writes_nothing,
    }
}

// ----- fixtures ------------------------------------------------------------

/// A whole-second instant, `secs` after a fixed origin.
fn at(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_790_000_000 + secs, 0)
        .single()
        .expect("a representable instant")
}

fn kind(code: &str, parent: Option<&str>, sort_order: i32) -> SubjectKind {
    SubjectKind {
        kind: code.into(),
        label: format!("The {code} kind"),
        parent_kind: parent.map(String::from),
        description: None,
        owning_team: "suite".into(),
        metadata: json!({}),
        sort_order,
        retired_at: None,
    }
}

fn retired(mut k: SubjectKind) -> SubjectKind {
    k.retired_at = Some(at(0));
    k
}

/// The suite's rows: two top-level kinds, a parent with two live
/// children, one retired child and one grandchild, a retired top-level
/// kind, and a kind carrying the reserved `birth` flag.
fn rows() -> Vec<SubjectKind> {
    let mut account = kind("suite-account", None, 20);
    account.description = Some("An organisation the company deals with".into());
    account.metadata = json!({"icon": "building", "module": "crm"});
    let mut workflow = kind("suite-workflow", None, 40);
    workflow.metadata = json!({"birth": "job"});
    vec![
        kind("suite-asset", None, 10),
        account,
        kind("suite-clinic", Some("suite-account"), 30),
        kind("suite-wholesale", Some("suite-account"), 30),
        retired(kind("suite-lapsed", Some("suite-account"), 30)),
        kind("suite-grandchild", Some("suite-clinic"), 30),
        retired(kind("suite-legacy", None, 50)),
        workflow,
    ]
}

fn row(code: &str) -> SubjectKind {
    rows()
        .into_iter()
        .find(|k| k.kind == code)
        .expect("a suite row")
}

fn stamp() -> EventStamp {
    EventStamp::new(
        "subject-kinds",
        ActorId::Automation("rule:adapters-agree-suite".into()),
    )
}

fn patch(v: Value) -> Map<String, Value> {
    v.as_object().cloned().expect("a patch is an object")
}

/// The suite kinds among `rows`, sorted by code — the order is not the
/// port's to promise (see the file header).
fn codes(rows: Vec<SubjectKind>) -> Vec<String> {
    let mut out: Vec<String> = rows
        .into_iter()
        .map(|k| k.kind)
        .filter(|k| k.starts_with("suite-"))
        .collect();
    out.sort();
    out
}

// ----- cases -----------------------------------------------------------------

/// `get` answers a row whole — live or retired, since audit code
/// resolves old ids through it — and `None`, not an error, for a kind
/// no row carries.
async fn get_answers_every_row_retired_or_not_and_none_for_a_stranger<W: World>(
    w: &W,
    adapter: &str,
) {
    for code in ["suite-account", "suite-clinic", "suite-legacy"] {
        assert_eq!(
            w.repo().get(code).await.expect("get answers"),
            Some(row(code)),
            "{adapter}: {code} reads back whole"
        );
    }
    assert_eq!(
        w.repo().get("suite-stranger").await.expect("get answers"),
        None,
        "{adapter}"
    );
}

/// The hot-path validator: true for a live kind, false for a retired
/// one AND for one no row carries — a retired kind refuses a new
/// Subject exactly as a typo does.
async fn exists_active_is_true_only_for_a_live_kind<W: World>(w: &W, adapter: &str) {
    for (code, live) in [
        ("suite-asset", true),
        ("suite-grandchild", true),
        ("suite-legacy", false),
        ("suite-lapsed", false),
        ("suite-stranger", false),
    ] {
        assert_eq!(
            w.repo().exists_active(code).await.expect("answers"),
            live,
            "{adapter}: {code}"
        );
    }
}

/// `list_active` is every live kind, whole, and no retired one.
async fn list_active_is_every_live_kind_and_no_retired_one<W: World>(w: &W, adapter: &str) {
    let listed = w.repo().list_active().await.expect("list_active answers");
    assert_eq!(
        codes(listed.clone()),
        vec![
            "suite-account",
            "suite-asset",
            "suite-clinic",
            "suite-grandchild",
            "suite-wholesale",
            "suite-workflow",
        ],
        "{adapter}"
    );
    for k in listed.iter().filter(|k| k.kind.starts_with("suite-")) {
        assert_eq!(*k, row(&k.kind), "{adapter}: {} is listed whole", k.kind);
    }
}

/// `children_of` is the LIVE kinds whose parent is the one named — one
/// level, so a grandchild is not a child — and nothing for a leaf or a
/// kind no row carries.
async fn children_of_walks_one_level_of_live_children<W: World>(w: &W, adapter: &str) {
    let kids = w
        .repo()
        .children_of("suite-account")
        .await
        .expect("children_of answers");
    assert_eq!(
        codes(kids.clone()),
        vec!["suite-clinic", "suite-wholesale"],
        "{adapter}"
    );
    for k in &kids {
        assert_eq!(*k, row(&k.kind), "{adapter}: {} whole", k.kind);
    }
    assert_eq!(
        codes(w.repo().children_of("suite-clinic").await.expect("answers")),
        vec!["suite-grandchild"],
        "{adapter}"
    );
    for leaf in ["suite-asset", "suite-stranger"] {
        assert!(
            w.repo()
                .children_of(leaf)
                .await
                .expect("answers")
                .is_empty(),
            "{adapter}: {leaf}"
        );
    }
}

/// A patch merges into the held metadata (a key replaces, a `null`
/// deletes, an unnamed key is kept), answers the row as it now stands,
/// and records ONE `subject_kind.updated` naming the kind, what changed,
/// before and after, and who changed it. The same patch again changes
/// nothing and records nothing. A retired kind is patchable. The facts
/// alone, replayed onto the seeded metadata, rebuild the row.
async fn a_patch_leaves_its_change_and_one_fact_and_a_restatement_leaves_neither<W: World>(
    w: &W,
    adapter: &str,
) {
    let p = patch(json!({"module": "finance", "icon": null, "tier": 2}));
    let mut want = row("suite-account");
    want.metadata = json!({"module": "finance", "tier": 2});
    for attempt in ["first", "restated"] {
        assert_eq!(
            w.repo()
                .patch_metadata("suite-account", &p, &stamp())
                .await
                .expect("the patch answers"),
            Some(want.clone()),
            "{adapter}: {attempt}"
        );
    }
    assert_eq!(
        w.repo().get("suite-account").await.expect("get"),
        Some(want.clone()),
        "{adapter}: the row holds the change"
    );

    let mut lapsed = row("suite-legacy");
    lapsed.metadata = json!({"module": "archive"});
    assert_eq!(
        w.repo()
            .patch_metadata(
                "suite-legacy",
                &patch(json!({"module": "archive"})),
                &stamp()
            )
            .await
            .expect("a retired kind is patchable"),
        Some(lapsed),
        "{adapter}"
    );

    let facts = w.facts().await;
    let stripped: Vec<Value> = facts
        .iter()
        .map(|f| {
            let mut f = f.clone();
            if let Value::Object(m) = &mut f {
                m.retain(|k, _| !k.starts_with('_'));
            }
            f
        })
        .collect();
    assert_eq!(
        stripped,
        vec![
            json!({
                "kind": "suite-account",
                "changed": ["icon", "module", "tier"],
                "before": {"icon": "building", "module": "crm"},
                "after": {"module": "finance", "tier": 2},
                "updated_by": "automation:rule:adapters-agree-suite",
            }),
            json!({
                "kind": "suite-legacy",
                "changed": ["module"],
                "before": {},
                "after": {"module": "archive"},
                "updated_by": "automation:rule:adapters-agree-suite",
            }),
        ],
        "{adapter}: one fact per change, none for the restatement"
    );
    for f in &facts {
        assert_eq!(f["_actor"], f["updated_by"], "{adapter}: one actor");
    }
    let change: MetadataChange =
        serde_json::from_value(facts[0].clone()).expect("the payload is a change");
    assert_eq!(
        apply_change(&row("suite-account").metadata, &change),
        want.metadata,
        "{adapter}: the fact alone rebuilds the row"
    );
}

/// A patch naming a reserved key — set, delete or restate — is refused
/// as `InvalidPatch` naming it; a patch of a kind no row carries is
/// `None`. Neither moves a row or records a fact.
async fn a_reserved_key_or_a_stranger_is_refused_and_writes_nothing<W: World>(
    w: &W,
    adapter: &str,
) {
    for p in [
        json!({"birth": null}),
        json!({"birth": "job", "module": "x"}),
    ] {
        let got = w
            .repo()
            .patch_metadata("suite-workflow", &patch(p.clone()), &stamp())
            .await;
        assert!(
            matches!(got, Err(SubjectKindError::InvalidPatch(ref m)) if m.contains("birth")),
            "{adapter}: {p}: {got:?}"
        );
    }
    assert_eq!(
        w.repo()
            .patch_metadata("suite-stranger", &patch(json!({"module": "x"})), &stamp())
            .await
            .expect("a stranger answers"),
        None,
        "{adapter}"
    );
    assert_eq!(
        w.repo().get("suite-workflow").await.expect("get"),
        Some(row("suite-workflow")),
        "{adapter}: the row is untouched"
    );
    assert!(w.facts().await.is_empty(), "{adapter}: no fact");
}
