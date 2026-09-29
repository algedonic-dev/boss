//! The agents registry answers the same on both `AgentsRegistry`
//! adapters — reliability mechanism C of design 3036296f, "adapters
//! agree" (`boss_testing::adapters_agree!`), on the seventh port it
//! reaches (backlog be459ab9; the credentials registry, in the same car,
//! was the sixth).
//!
//! WHY THIS PORT NEXT. It is the other half of the trust boundary: the
//! jobs API's door resolves every address-shaped login through
//! `resolve_login` before a write is stamped, so the registry decides
//! WHO the audit log says did a thing. Every door test asks
//! `InMemoryAgents`; production is answered by `PgAgents`.
//!
//! The first run found three disagreements, each fixed in this car:
//! - ORDER. `list` promises rows "ordered by id" and each row's aliases
//!   "sorted". Postgres sorted both by the database's locale, which
//!   ignores `-` at first level, so `agent-suite-ab` listed before
//!   `agent-suite-a-z` there and after it in memory (and the same for
//!   aliases). The Pg adapter now sorts `COLLATE "C"` — byte order, the
//!   order the double's `BTreeMap`s hold and the only one both can hold;
//!   the Workflow registry's suite fixed the same defect the same way.
//!   Case `a_declaration_lands_whole_and_every_alias_resolves`.
//! - THE RATE CARD. `agents.default_model` is a foreign key into the
//!   rate card, so Postgres refuses a declaration whose model cannot be
//!   priced as `Unpriced` and lands nothing of the batch; the double did
//!   not mirror it ("the Pg test proves that refusal") and landed the
//!   batch. It now takes the models it prices (`with_rate_card`) and
//!   refuses the same way, at the same moment — only when a row is
//!   WRITTEN, because a foreign key is checked on the written row, so a
//!   held row kept under the default is kept whatever its declared model
//!   says, exactly as Postgres keeps it. Case
//!   `an_unpriced_model_refuses_the_batch_only_when_it_would_be_written`.
//! - THE SCHEMA'S CHECKS. Postgres refuses an id that is not
//!   `agent-<slug>` and a negative budget or run cap, on every declared
//!   row including a held one (a CHECK runs before the conflict is looked
//!   at), and rolls the batch back. The double admitted all three. It now
//!   refuses the batch before writing anything, reading the id rule from
//!   `is_agent_id` — the one spelling `validate_agent` reads too. Case
//!   `a_row_the_schema_refuses_refuses_the_whole_batch`.
//!
//! THE SHAPE, the other suites': each case states its answer and each
//! adapter is held to it, not merely to the other adapter; every write is
//! also judged by the FACTS it leaves (`agent.declared` / `agent.updated`,
//! collected by the double and staged on `event_outbox` by Postgres in
//! the row's transaction). A migration seeds `agent-claude` and its alias
//! into every Postgres database and the double starts empty, so every id
//! and alias this file writes is `suite`-named and every read is judged
//! over those alone. The double is built pricing `opus-5` and
//! `opus-5[1m]`, the two models `agents_pg.rs` already proves the card
//! prices, and every case declares one of those or the unpriced
//! `claude-opus-5`.

use boss_core::actor::ActorId;
use boss_core::publish::{KeptRow, PublishMode};
use boss_core::publisher::EventStamp;
use boss_jobs::agents::types::{AgentInput, AgentRow, AgentsBatchOutcome};
use boss_jobs::agents::{
    AGENT_DECLARED, AGENT_UPDATED, AgentsError, AgentsRegistry, InMemoryAgents, PgAgents,
};
use serde_json::Value;

const PRICED: &str = "opus-5";
const PRICED_TOO: &str = "opus-5[1m]";
const UNPRICED: &str = "claude-opus-5";

/// The registry under test and the facts its writes left, read the way
/// each adapter keeps them.
trait World {
    type R: AgentsRegistry;
    fn repo(&self) -> &Self::R;
    /// Every `agent.declared` / `agent.updated` fact recorded so far, in
    /// the order recorded: `(kind, payload)`.
    async fn facts(&self) -> Vec<(String, Value)>;
}

struct InMemory(InMemoryAgents);

impl World for InMemory {
    type R = InMemoryAgents;
    fn repo(&self) -> &InMemoryAgents {
        &self.0
    }
    async fn facts(&self) -> Vec<(String, Value)> {
        self.0
            .recorded_events()
            .into_iter()
            .filter(|e| e.kind == AGENT_DECLARED || e.kind == AGENT_UPDATED)
            .map(|e| (e.kind, e.payload))
            .collect()
    }
}

struct Postgres {
    repo: PgAgents,
    pool: sqlx::PgPool,
}

impl World for Postgres {
    type R = PgAgents;
    fn repo(&self) -> &PgAgents {
        &self.repo
    }
    async fn facts(&self) -> Vec<(String, Value)> {
        sqlx::query_as(
            "SELECT kind, payload FROM event_outbox WHERE kind = $1 OR kind = $2 ORDER BY id",
        )
        .bind(AGENT_DECLARED)
        .bind(AGENT_UPDATED)
        .fetch_all(&self.pool)
        .await
        .expect("read the outbox")
    }
}

boss_testing::adapters_agree! {
    adapters {
        in_memory => (InMemory(InMemoryAgents::new().with_rate_card([PRICED, PRICED_TOO])), ()),
        postgres => {
            let db = boss_testing::TestDb::new().await;
            let world = Postgres { repo: PgAgents::new(db.pool.clone()), pool: db.pool.clone() };
            (world, db)
        },
    }
    cases {
        a_declaration_lands_whole_and_every_alias_resolves,
        the_default_keeps_a_differing_held_row_and_names_what_differs,
        take_updates_a_held_row_and_names_each_change,
        an_alias_another_agent_holds_is_kept_by_default_and_moved_by_take,
        a_batch_naming_one_id_twice_inserts_it_once,
        an_unpriced_model_refuses_the_batch_only_when_it_would_be_written,
        a_row_the_schema_refuses_refuses_the_whole_batch,
    }
}

// ----- fixtures ------------------------------------------------------------

fn stamp() -> EventStamp {
    EventStamp::new(
        "jobs",
        ActorId::Automation("rule:adapters-agree-suite".into()),
    )
}

fn agent(id: &str, aliases: &[&str]) -> AgentInput {
    AgentInput {
        id: id.into(),
        display_name: format!("{id} as first declared"),
        default_model: PRICED.into(),
        aliases: aliases.iter().map(|a| (*a).to_string()).collect(),
        role: None,
        department: None,
        hourly_budget_usd_micros: None,
        max_concurrent_runs: None,
    }
}

/// The row a declaration lands as, with the aliases it will hold.
fn row_of(a: &AgentInput, aliases: &[&str]) -> AgentRow {
    AgentRow {
        id: a.id.clone(),
        display_name: a.display_name.clone(),
        default_model: a.default_model.clone(),
        role: a.role.clone(),
        department: a.department.clone(),
        hourly_budget_usd_micros: a.hourly_budget_usd_micros,
        max_concurrent_runs: a.max_concurrent_runs,
        aliases: aliases.iter().map(|a| (*a).to_string()).collect(),
    }
}

fn outcome(
    received: usize,
    inserted: usize,
    updated: &[(&str, &[&str])],
    kept: &[(&str, &[&str])],
    unchanged: usize,
) -> (
    usize,
    usize,
    Vec<(String, Vec<String>)>,
    Vec<KeptRow>,
    usize,
) {
    (
        received,
        inserted,
        updated
            .iter()
            .map(|(id, fields)| {
                (
                    id.to_string(),
                    fields.iter().map(|f| f.to_string()).collect(),
                )
            })
            .collect(),
        kept.iter()
            .map(|(id, differs)| KeptRow {
                id: id.to_string(),
                differs: differs.iter().map(|f| f.to_string()).collect(),
            })
            .collect(),
        unchanged,
    )
}

/// An outcome in the shape `outcome` states one: each updated row by the
/// FIELDS it changed (the from → to values are each adapter's own sorted
/// alias lists, compared where a case needs them).
fn shape(
    o: &AgentsBatchOutcome,
) -> (
    usize,
    usize,
    Vec<(String, Vec<String>)>,
    Vec<KeptRow>,
    usize,
) {
    (
        o.received,
        o.inserted,
        o.updated
            .iter()
            .map(|u| {
                (
                    u.id.clone(),
                    u.changes.iter().map(|c| c.field.clone()).collect(),
                )
            })
            .collect(),
        o.kept.clone(),
        o.unchanged,
    )
}

/// The rows this file wrote, in the order `list` answers them.
async fn suite_rows<W: World>(w: &W) -> Vec<AgentRow> {
    w.repo()
        .list()
        .await
        .expect("list answers")
        .into_iter()
        .filter(|r| r.id.starts_with("agent-suite-"))
        .collect()
}

async fn suite_ids<W: World>(w: &W) -> Vec<String> {
    suite_rows(w).await.into_iter().map(|r| r.id).collect()
}

async fn suite_row<W: World>(w: &W, id: &str) -> Option<AgentRow> {
    suite_rows(w).await.into_iter().find(|r| r.id == id)
}

async fn resolves<W: World>(w: &W, login: &str) -> Option<String> {
    w.repo()
        .resolve_login(login)
        .await
        .expect("the registry answers")
}

/// `(kind, id)` for each fact.
async fn fact_ids<W: World>(w: &W) -> Vec<(String, String)> {
    w.facts()
        .await
        .into_iter()
        .map(|(kind, p)| (kind, p["id"].as_str().unwrap_or("").to_string()))
        .collect()
}

fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
    v.iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect()
}

async fn publish<W: World>(
    w: &W,
    rows: &[AgentInput],
    mode: PublishMode,
) -> Result<AgentsBatchOutcome, AgentsError> {
    w.repo().publish(rows, mode, &stamp()).await
}

// ----- cases -----------------------------------------------------------------

/// A declaration lands every column as declared, lists in BYTE order of
/// id with each row's aliases in byte order (`-` sorts before a letter),
/// resolves every declared alias to its id and an unknown login to
/// `None`, and leaves one `agent.declared` per row carrying the declarer.
async fn a_declaration_lands_whole_and_every_alias_resolves<W: World>(w: &W, adapter: &str) {
    let mut b = agent(
        "agent-suite-b",
        &["b@suite.test", "ab@suite.test", "a-z@suite.test"],
    );
    b.role = Some("platform-admin".into());
    b.department = Some("it".into());
    b.hourly_budget_usd_micros = Some(5_000_000);
    b.max_concurrent_runs = Some(3);
    b.default_model = PRICED_TOO.into();
    let ab = agent("agent-suite-ab", &[]);
    let a_z = agent("agent-suite-a-z", &["a-z-agent@suite.test"]);
    let out = publish(
        w,
        &[b.clone(), ab.clone(), a_z.clone()],
        PublishMode::default(),
    )
    .await
    .expect("a declaration lands");
    assert_eq!(shape(&out), outcome(3, 3, &[], &[], 0), "{adapter}");

    assert_eq!(
        suite_rows(w).await,
        vec![
            row_of(&a_z, &["a-z-agent@suite.test"]),
            row_of(&ab, &[]),
            row_of(&b, &["a-z@suite.test", "ab@suite.test", "b@suite.test"]),
        ],
        "{adapter}: ids and aliases in byte order, every column as declared"
    );
    for login in ["b@suite.test", "ab@suite.test", "a-z@suite.test"] {
        assert_eq!(
            resolves(w, login).await.as_deref(),
            Some("agent-suite-b"),
            "{adapter}: {login}"
        );
    }
    assert_eq!(resolves(w, "nobody@suite.test").await, None, "{adapter}");

    let facts = w.facts().await;
    assert_eq!(
        fact_ids(w).await,
        pairs(&[
            (AGENT_DECLARED, "agent-suite-b"),
            (AGENT_DECLARED, "agent-suite-ab"),
            (AGENT_DECLARED, "agent-suite-a-z"),
        ]),
        "{adapter}: one fact per inserted row, in the batch's order"
    );
    for (_, p) in &facts {
        assert_eq!(
            p["declared_by"], "automation:rule:adapters-agree-suite",
            "{adapter}"
        );
        assert_eq!(p["declared_by"], p["_actor"], "{adapter}: one actor");
    }
    assert_eq!(facts[0].1["role"], "platform-admin", "{adapter}");
}

/// Under the default a held row keeps every column it holds and the
/// outcome names each declared field it differs on — THE INSTANCE IS THE
/// TRUTH (design e187198f). A declared alias nobody holds still lands
/// (an alias is its own row), which changes the row, so that one change
/// is reported and recorded as an update. Declaring the row as it now
/// reads is `unchanged`.
async fn the_default_keeps_a_differing_held_row_and_names_what_differs<W: World>(
    w: &W,
    adapter: &str,
) {
    let first = agent("agent-suite-held", &[]);
    publish(w, std::slice::from_ref(&first), PublishMode::default())
        .await
        .expect("the first declaration lands");

    let mut edited = agent("agent-suite-held", &["new@suite.test"]);
    edited.display_name = "renamed in the tenant's file".into();
    edited.max_concurrent_runs = Some(2);
    let out = publish(w, &[edited], PublishMode::default())
        .await
        .expect("a held row is kept");
    assert_eq!(
        shape(&out),
        outcome(
            1,
            0,
            &[("agent-suite-held", &["aliases"])],
            &[("agent-suite-held", &["display_name", "max_concurrent_runs"])],
            0
        ),
        "{adapter}"
    );
    assert_eq!(
        suite_row(w, "agent-suite-held").await,
        Some(row_of(&first, &["new@suite.test"])),
        "{adapter}: every column kept, the new alias landed"
    );

    let out = publish(w, &[first], PublishMode::default())
        .await
        .expect("a republish answers");
    assert_eq!(shape(&out), outcome(1, 0, &[], &[], 1), "{adapter}");
    assert_eq!(
        fact_ids(w).await,
        pairs(&[
            (AGENT_DECLARED, "agent-suite-held"),
            (AGENT_UPDATED, "agent-suite-held"),
        ]),
        "{adapter}: a kept row records nothing; the alias that landed is one update"
    );
}

/// Under take a held row takes every declared column, the outcome names
/// each change from → to, and the `agent.updated` fact carries the same
/// changes and the updater. Taking it again changes nothing.
async fn take_updates_a_held_row_and_names_each_change<W: World>(w: &W, adapter: &str) {
    publish(
        w,
        &[agent("agent-suite-taken", &[])],
        PublishMode::default(),
    )
    .await
    .expect("the first declaration lands");

    let mut taken = agent("agent-suite-taken", &[]);
    taken.display_name = "taken".into();
    taken.default_model = PRICED_TOO.into();
    taken.role = Some("platform-admin".into());
    taken.hourly_budget_usd_micros = Some(0);
    let out = publish(w, &[taken.clone()], PublishMode::Take)
        .await
        .expect("take updates");
    assert_eq!(
        shape(&out),
        outcome(
            1,
            0,
            &[(
                "agent-suite-taken",
                &[
                    "display_name",
                    "default_model",
                    "role",
                    "hourly_budget_usd_micros"
                ]
            )],
            &[],
            0
        ),
        "{adapter}"
    );
    let changes = &out.updated[0].changes;
    assert_eq!(changes[1].from, PRICED, "{adapter}");
    assert_eq!(changes[1].to, PRICED_TOO, "{adapter}");
    assert_eq!(changes[2].from, Value::Null, "{adapter}");
    assert_eq!(
        suite_row(w, "agent-suite-taken").await,
        Some(row_of(&taken, &[])),
        "{adapter}"
    );

    let out = publish(w, &[taken], PublishMode::Take)
        .await
        .expect("a second take answers");
    assert_eq!(shape(&out), outcome(1, 0, &[], &[], 1), "{adapter}");

    let facts = w.facts().await;
    assert_eq!(
        fact_ids(w).await,
        pairs(&[
            (AGENT_DECLARED, "agent-suite-taken"),
            (AGENT_UPDATED, "agent-suite-taken"),
        ]),
        "{adapter}: one update, none for the take that changed nothing"
    );
    let updated = &facts[1].1;
    assert_eq!(updated["display_name"], "taken", "{adapter}: the row after");
    assert_eq!(
        updated["changes"].as_array().map(Vec::len),
        Some(4),
        "{adapter}"
    );
    assert_eq!(updated["updated_by"], updated["_actor"], "{adapter}");
}

/// A declared alias another agent holds stays with that agent under the
/// default (named as a difference) and moves under take; an alias the
/// declaration does not name is never touched.
async fn an_alias_another_agent_holds_is_kept_by_default_and_moved_by_take<W: World>(
    w: &W,
    adapter: &str,
) {
    publish(
        w,
        &[
            agent("agent-suite-holder", &["shared@suite.test"]),
            agent("agent-suite-claimant", &["own@suite.test"]),
        ],
        PublishMode::default(),
    )
    .await
    .expect("both land");
    let claim = agent("agent-suite-claimant", &["shared@suite.test"]);

    let out = publish(w, std::slice::from_ref(&claim), PublishMode::default())
        .await
        .expect("the default answers");
    assert_eq!(
        shape(&out),
        outcome(1, 0, &[], &[("agent-suite-claimant", &["aliases"])], 0),
        "{adapter}"
    );
    assert_eq!(
        resolves(w, "shared@suite.test").await.as_deref(),
        Some("agent-suite-holder"),
        "{adapter}: kept where it was"
    );

    let out = publish(w, &[claim], PublishMode::Take)
        .await
        .expect("take answers");
    assert_eq!(
        shape(&out),
        outcome(1, 0, &[("agent-suite-claimant", &["aliases"])], &[], 0),
        "{adapter}"
    );
    assert_eq!(
        resolves(w, "shared@suite.test").await.as_deref(),
        Some("agent-suite-claimant"),
        "{adapter}: moved"
    );
    let rows = suite_rows(w).await;
    let aliases: Vec<(&str, Vec<String>)> = rows
        .iter()
        .map(|r| (r.id.as_str(), r.aliases.clone()))
        .collect();
    assert_eq!(
        aliases,
        vec![
            (
                "agent-suite-claimant",
                vec![
                    "own@suite.test".to_string(),
                    "shared@suite.test".to_string()
                ]
            ),
            ("agent-suite-holder", vec![]),
        ],
        "{adapter}: the undeclared alias stayed"
    );
}

/// One batch naming an id twice: the first declaration lands and the
/// second is a held row like any other — kept, its difference named.
async fn a_batch_naming_one_id_twice_inserts_it_once<W: World>(w: &W, adapter: &str) {
    let first = agent("agent-suite-twice", &[]);
    let mut second = agent("agent-suite-twice", &[]);
    second.display_name = "the second spelling in one batch".into();
    let out = publish(w, &[first.clone(), second], PublishMode::default())
        .await
        .expect("the batch lands");
    assert_eq!(
        shape(&out),
        outcome(2, 1, &[], &[("agent-suite-twice", &["display_name"])], 0),
        "{adapter}"
    );
    assert_eq!(
        suite_row(w, "agent-suite-twice").await,
        Some(row_of(&first, &[])),
        "{adapter}"
    );
    assert_eq!(
        fact_ids(w).await,
        pairs(&[(AGENT_DECLARED, "agent-suite-twice")]),
        "{adapter}"
    );
}

/// A model the rate card does not price is refused as `Unpriced` naming
/// it whenever the row would be WRITTEN — a new row, or a held one under
/// take — and the batch lands nothing, the rows before it included. A
/// held row kept under the default writes nothing, so nothing refuses
/// it: it is kept, the model named as a difference.
async fn an_unpriced_model_refuses_the_batch_only_when_it_would_be_written<W: World>(
    w: &W,
    adapter: &str,
) {
    let mut unpriced = agent("agent-suite-unpriced", &[]);
    unpriced.default_model = UNPRICED.into();
    let err = publish(
        w,
        &[
            agent("agent-suite-alongside", &["alongside@suite.test"]),
            unpriced,
        ],
        PublishMode::default(),
    )
    .await
    .expect_err("an unpriced model is refused");
    assert!(
        matches!(&err, AgentsError::Unpriced(m) if m == UNPRICED),
        "{adapter}: {err:?}"
    );
    assert!(suite_ids(w).await.is_empty(), "{adapter}: nothing landed");
    assert_eq!(resolves(w, "alongside@suite.test").await, None, "{adapter}");
    assert!(w.facts().await.is_empty(), "{adapter}: no fact");

    let held = agent("agent-suite-held", &[]);
    publish(w, std::slice::from_ref(&held), PublishMode::default())
        .await
        .expect("the held row lands");
    let mut repriced = held.clone();
    repriced.default_model = UNPRICED.into();
    let out = publish(w, &[repriced.clone()], PublishMode::default())
        .await
        .expect("a kept row writes nothing, so nothing refuses it");
    assert_eq!(
        shape(&out),
        outcome(1, 0, &[], &[("agent-suite-held", &["default_model"])], 0),
        "{adapter}"
    );
    let err = publish(w, &[repriced], PublishMode::Take)
        .await
        .expect_err("take would write the unpriced model");
    assert!(
        matches!(&err, AgentsError::Unpriced(m) if m == UNPRICED),
        "{adapter}: {err:?}"
    );
    assert_eq!(
        suite_row(w, "agent-suite-held").await,
        Some(row_of(&held, &[])),
        "{adapter}: the held row is as it was"
    );
    assert_eq!(
        fact_ids(w).await,
        pairs(&[(AGENT_DECLARED, "agent-suite-held")]),
        "{adapter}"
    );
}

/// A row the schema refuses — an id that is not `agent-<slug>`, a
/// negative budget or run cap — refuses the WHOLE batch as a storage
/// failure, the rows before it included, and for a held id as well as a
/// new one: the CHECK runs on the declared row before the conflict is
/// looked at.
async fn a_row_the_schema_refuses_refuses_the_whole_batch<W: World>(w: &W, adapter: &str) {
    let held = agent("agent-suite-held", &[]);
    publish(w, std::slice::from_ref(&held), PublishMode::default())
        .await
        .expect("the held row lands");

    let upper = agent("agent-Suite-upper", &[]);
    let unprefixed = agent("suite-unprefixed", &[]);
    let mut over_budget = agent("agent-suite-over-budget", &[]);
    over_budget.hourly_budget_usd_micros = Some(-1);
    let mut negative_cap = agent("agent-suite-negative-cap", &[]);
    negative_cap.max_concurrent_runs = Some(-1);
    let mut held_negative = held.clone();
    held_negative.hourly_budget_usd_micros = Some(-1);

    for bad in [upper, unprefixed, over_budget, negative_cap, held_negative] {
        for mode in [PublishMode::InsertIfAbsent, PublishMode::Take] {
            let err = publish(
                w,
                &[
                    agent("agent-suite-alongside", &["alongside@suite.test"]),
                    bad.clone(),
                ],
                mode,
            )
            .await
            .expect_err("the schema refuses the row");
            assert!(
                matches!(err, AgentsError::Storage(_)),
                "{adapter}: {} under {mode:?}: {err:?}",
                bad.id
            );
            assert_eq!(
                suite_rows(w).await,
                vec![row_of(&held, &[])],
                "{adapter}: {} under {mode:?} landed nothing",
                bad.id
            );
            assert_eq!(
                resolves(w, "alongside@suite.test").await,
                None,
                "{adapter}: {}",
                bad.id
            );
        }
    }
    assert_eq!(
        fact_ids(w).await,
        pairs(&[(AGENT_DECLARED, "agent-suite-held")]),
        "{adapter}: no refused batch left a fact"
    );
}
