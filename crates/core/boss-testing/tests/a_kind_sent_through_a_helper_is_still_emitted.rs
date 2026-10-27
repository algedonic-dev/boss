//! `infra/lint/emitted-kinds-are-declared.sh` — every event kind emitted
//! in crates/ has an event_kinds row — is RUN against fixture trees here,
//! so the call shapes it can see are the ones it actually sees.
//!
//! WHY (backlog 7d05ded2, 2026-10-19). The lint resolved a kind only at
//! three call shapes: `.event(KIND,`, `record_ledger_event_in_tx(_, _,
//! KIND,` and `record(&mut tx, _, KIND,`, plus a literal `= Event::new(`.
//! boss-jobs builds every registry event through a HELPER —
//! `events::workflow_registry_event(kind, actor, spec)`, which hands
//! `kind` to `Event::new` — and its 14 call sites in registry.rs matched
//! none of them. So `jobs.kind.draft_discarded` was emitted from ebd7bb70
//! on with no event_kinds row while the lint printed `ok` (found building
//! car fix/a-retired-candidate-draft-is-rejected-not-blocking). A check
//! that cannot see a path is a check that is not running.
//!
//! The fix does not add a fourth call-name pattern, which the next helper
//! would walk around the same way. It DERIVES the helpers: a function
//! whose first parameter is `kind: &str` and whose body passes that
//! `kind` on as `Event::new`'s kind — or to another such function — is a
//! kind forwarder, and a call to one emits its first argument. The
//! fixtures below plant that shape the way boss-jobs writes it: a
//! wrapper (`registry_event`) over a clock-taking twin
//! (`registry_event_at`) that calls `Event::new`, called by path from a
//! different file than the one defining it, with the kind as a const.

use boss_testing::repo_root;
use boss_testing::scratch;
use std::path::PathBuf;
use std::process::{Command, Output};

const LINT: &str = "infra/lint/emitted-kinds-are-declared.sh";
const SCANNED: &str = "infra/lint/lib/scanned.sh";

/// One kind declared, in the migration shape the real schema uses.
const SCHEMA: &str = "\
INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
    ('demo.kind.declared', 'demo', 'A declared demo kind.', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;
";

/// The helper pair, in boss-jobs' own shape (events.rs:242-260 on
/// origin/main 299cc5c81): a multi-line signature, the outer helper
/// forwarding to the `_at` twin, the twin calling `Event::new` by path.
const EVENTS_RS: &str = r#"
pub const DECLARED: &str = "demo.kind.declared";
pub const UNDECLARED: &str = "demo.kind.undeclared";
pub const LOOKED_UP: &str = "demo.kind.looked_up";

pub fn registry_event(
    kind: &str,
    actor: &str,
    spec: &Spec,
) -> boss_core::event::Event {
    registry_event_at(kind, actor, spec, wall_now())
}

pub fn registry_event_at(
    kind: &str,
    actor: &str,
    spec: &Spec,
    now: DateTime,
) -> boss_core::event::Event {
    let payload = payload_of(spec, actor);
    boss_core::event::Event::new("demo", kind, payload, now)
}

/// Takes a `kind: &str` first and emits nothing: a call to it is not an
/// emit, whatever it is handed.
pub fn is_known(kind: &str, table: &Table) -> bool {
    table.contains(kind)
}
"#;

/// The callers, in another file, by path, split across lines the way
/// rustfmt splits them in registry.rs.
fn registry_rs(kinds: &[&str]) -> String {
    let calls: String = kinds
        .iter()
        .map(|k| {
            format!(
                "        self.record(crate::events::registry_event(\n            crate::events::{k},\n            &actor,\n            &spec,\n        ))?;\n"
            )
        })
        .collect();
    format!(
        "impl Registry {{\n    fn publish(&self) -> Result<()> {{\n{calls}        let _ = crate::events::is_known(crate::events::LOOKED_UP, &self.table);\n        Ok(())\n    }}\n}}\n"
    )
}

/// The forwarder shapes the fixture above does not plant, each taking the
/// kind as its ONLY argument (review 738b2811 of car 760f5766, 2026-10-19):
/// a plain helper, a `pub(crate)` one, an associated function, and a
/// three-deep chain whose last link calls `Event::new`. Each was DERIVED
/// as a forwarder, yet its call was never read, because the call scan
/// demanded a comma after the first argument and rustfmt writes a short
/// one-argument call as `audit(KIND)` — so all four kinds passed unread.
/// Two more single-argument helpers take the body shapes the derivation
/// first missed (the same review, findings 2): a source argument that is
/// itself a call, and a kind converted on its way in.
const ONE_ARG_EVENTS_RS: &str = r#"
pub const ONE_ARG: &str = "demo.one_arg.undeclared";
pub const PUB_CRATE: &str = "demo.pub_crate.undeclared";
pub const ASSOC: &str = "demo.assoc.undeclared";
pub const CHAIN: &str = "demo.chain.undeclared";
pub const SOURCE_CALL: &str = "demo.source_call.undeclared";
pub const CONVERTED: &str = "demo.converted.undeclared";

pub fn audit(kind: &str) -> Event {
    Event::new("demo", kind, payload(), wall_now())
}

pub(crate) fn audit_in_crate(kind: &str) -> Event {
    Event::new("demo", kind, payload(), wall_now())
}

pub struct Ev;

impl Ev {
    pub fn assoc(kind: &str) -> Event {
        Event::new("demo", kind, payload(), wall_now())
    }
}

pub fn chain_outer(kind: &str) -> Event {
    chain_mid(kind)
}

fn chain_mid(kind: &str) -> Event {
    chain_inner(kind)
}

fn chain_inner(kind: &str) -> Event {
    Event::new("demo", kind, payload(), wall_now())
}

pub fn audit_sourced(kind: &str) -> Event {
    Event::new(SOURCE.to_string(), kind, payload(), wall_now())
}

pub fn audit_converted(kind: &str) -> Event {
    Event::new("demo", kind.to_string(), payload(), wall_now())
}
"#;

/// Their callers, in another file, one short call per line as rustfmt
/// leaves them: no trailing comma after the kind.
const ONE_ARG_CALLERS_RS: &str = r#"
use crate::events::{self, Ev};

pub fn emit_all(out: &mut Outbox) {
    out.push(events::audit(events::ONE_ARG));
    out.push(events::audit_in_crate(events::PUB_CRATE));
    out.push(Ev::assoc(events::ASSOC));
    out.push(events::chain_outer(events::CHAIN));
    out.push(events::audit_sourced(events::SOURCE_CALL));
    out.push(events::audit_converted(events::CONVERTED));
}
"#;

/// Every kind the one-argument fixture emits, none of them declared.
const ONE_ARG_KINDS: [&str; 6] = [
    "demo.one_arg.undeclared",
    "demo.pub_crate.undeclared",
    "demo.assoc.undeclared",
    "demo.chain.undeclared",
    "demo.source_call.undeclared",
    "demo.converted.undeclared",
];

struct Tree(PathBuf);

impl Tree {
    /// The real lint and its helper, beside a one-crate fixture whose
    /// registry emits `kinds` through the helper.
    fn new(tag: &str, kinds: &[&str]) -> Tree {
        let t = Tree::bare(tag);
        t.file("crates/demo/src/events.rs", EVENTS_RS);
        t.file("crates/demo/src/registry.rs", &registry_rs(kinds));
        t
    }

    /// The real lint, its helper and the one-row schema, with no crate.
    fn bare(tag: &str) -> Tree {
        let root = scratch::scratch_dir(&format!("emitted-kinds-{tag}"));
        let t = Tree(root);
        for rel in [LINT, SCANNED] {
            let body = std::fs::read_to_string(repo_root().join(rel))
                .unwrap_or_else(|e| panic!("read {rel}: {e}"));
            t.file(rel, &body);
        }
        t.file("infra/postgres/schema/0001-demo-kinds.sql", SCHEMA);
        t
    }

    fn file(&self, rel: &str, body: &str) {
        let path = self.0.join(rel);
        if let Some(parent) = path.parent() {
            scratch::create_dir(parent);
        }
        scratch::write_file(&path, body);
    }

    fn lint(&self) -> Output {
        Command::new("bash")
            .arg(self.0.join(LINT))
            .output()
            .unwrap_or_else(|e| panic!("run {LINT} in {}: {e}", self.0.display()))
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// The packet's case: an undeclared kind sent through the helper is
/// refused, BY NAME. Before the fix the lint saw no emit at all here.
#[test]
fn an_undeclared_kind_sent_through_a_helper_is_named() {
    let t = Tree::new("undeclared", &["DECLARED", "UNDECLARED"]);
    let out = t.lint();
    let said = text(&out);
    assert_eq!(
        out.status.code(),
        Some(1),
        "an undeclared kind emitted through a kind-forwarding helper must fail the lint:\n{said}"
    );
    assert!(
        said.contains("  - demo.kind.undeclared"),
        "the refusal must name the kind the helper emitted:\n{said}"
    );
    assert!(
        !said.contains("  - demo.kind.declared"),
        "a declared kind is not an offender:\n{said}"
    );
}

/// The control: the same tree with only the declared kind passes, and
/// counts it — the helper's call is read as an emit, not skipped. A
/// `kind: &str` function that never reaches `Event::new` (`is_known`,
/// handed an undeclared dotted const) is not an emitter: no false red.
#[test]
fn a_declared_kind_sent_through_a_helper_is_counted_and_passes() {
    let t = Tree::new("declared", &["DECLARED"]);
    let out = t.lint();
    let said = text(&out);
    assert!(
        out.status.success(),
        "a declared kind through the helper must pass, and a non-emitting kind-first function must not count:\n{said}"
    );
    assert!(
        said.contains("emitted-kinds-are-declared: scanned 1 emitted kind(s)"),
        "the helper's one kind must be counted as emitted:\n{said}"
    );
}

/// Review 738b2811's blocking finding: a forwarder whose only argument is
/// the kind — plain, `pub(crate)`, associated, or the head of a three-deep
/// chain — has its call read, and so does one whose body sources its
/// event from a call or converts the kind. Every kind is named; before
/// the repair the lint printed `ok` over all six.
#[test]
fn a_kind_sent_through_a_one_argument_helper_is_named() {
    let t = Tree::bare("one-arg");
    t.file("crates/demo/src/events.rs", ONE_ARG_EVENTS_RS);
    t.file("crates/demo/src/callers.rs", ONE_ARG_CALLERS_RS);
    let out = t.lint();
    let said = text(&out);
    assert_eq!(
        out.status.code(),
        Some(1),
        "kinds sent through one-argument forwarders must fail the lint:\n{said}"
    );
    let unnamed: Vec<&str> = ONE_ARG_KINDS
        .iter()
        .copied()
        .filter(|k| !said.contains(&format!("  - {k}\n")))
        .collect();
    assert!(
        unnamed.is_empty(),
        "every kind a one-argument forwarder emits must be named; unnamed: {unnamed:?}\n{said}"
    );
    assert!(
        said.contains("emitted (literal/const): 6 |"),
        "exactly the six forwarded kinds are emitted, nothing else:\n{said}"
    );
}
