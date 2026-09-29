//! A TRUST-BOUNDARY CAR IS DECLARED ON ITS ITEM, and every door
//! downstream reads the declaration (backlog 486dde37).
//!
//! WHY. Two cars made the HOLD possible: `boss gate` takes `--hold`
//! beside `--park-*` and `jobs.auto-park` files the car already held
//! (92a3346c), and rule 7 of the builder rules tells a trust-boundary
//! builder to add it (443dea31). What neither could say is WHICH car is
//! trust-boundary: nothing on a packet marked it, so the operator told
//! each builder by hand — about ten times on 2026-09-27 (mutating verbs,
//! gateway routes, policy, credentials) — and a builder who was not told
//! gated unheld, and its car boarded on depth within minutes of the
//! green, before anyone could place `boss hold`.
//!
//! THE FACT is one key on the packet, `metadata.trust_boundary = { area,
//! reason }`, and the AREAS are data: `infra/platform/trust-areas.toml`
//! lists them, and no code here names one (CLAUDE.md §9). Its doors:
//!
//! - `boss triage --trust-boundary <area> --trust-reason '<why>'` writes
//!   it, refusing an area the list does not hold ([`mark`]), and on a
//!   `build` or `design` route that carries none it names the areas
//!   ([`offer`]) — the triager is the one who measured the change.
//! - `boss mark <item> --trust-boundary <area> --trust-reason '<why>'`
//!   writes it on an item already triaged, through the same check
//!   ([`mark`], [`plan`]) and touching no step (backlog b9352041). Until
//!   it, the only door after triage was `boss job patch`, which writes
//!   the key unchecked, so an unlisted area reached the packet and was
//!   flagged only by the brief.
//! - Both read the list through [`read_areas_for_the_verb`] — the cwd's
//!   worktree, else [`TREE_ENV`], else the checkout the binary was built
//!   from — and refuse, naming each place, when none holds it; the
//!   offer's list is read before the triage writes (backlog a693cf9d).
//! - `boss brief`, and so `boss dispatch`, which renders through it,
//!   prints it with the exact `--hold` the gate needs ([`brief_section`]).
//! - `boss gate` refuses a park-intent gate whose item carries it
//!   without `--hold` ([`gate_refusal`]) — the backstop for a builder
//!   who did not read the brief, and for the item that was marked after
//!   its run was briefed.
//!
//! A mark is judged PRESENT, not valid, at the gate: an area the list
//! no longer holds, or none at all, still holds the car. The list is
//! what a triager chooses from; the mark is what the item says, and a
//! hold nobody needed costs one `boss release`, while one missed costs
//! the review.

use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The packet metadata key the mark lives under.
pub(crate) const KEY: &str = "trust_boundary";

/// The one list of trust areas, relative to the repo root.
pub(crate) const REGISTRY: &str = "infra/platform/trust-areas.toml";

/// One row of [`REGISTRY`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub(crate) struct Area {
    pub name: String,
    pub what: String,
}

#[derive(serde::Deserialize)]
struct Registry {
    #[serde(default)]
    area: Vec<Area>,
}

/// The areas [`REGISTRY`]'s text declares, in its own order. A list
/// with no rows is refused: a triage that could choose from nothing
/// would refuse every mark and read as the operator's typo.
pub(crate) fn areas(toml_text: &str) -> std::result::Result<Vec<Area>, String> {
    let parsed: Registry = toml::from_str(toml_text).map_err(|e| e.to_string())?;
    if parsed.area.is_empty() {
        return Err("declares no [[area]] row — there is nothing to mark a car with".into());
    }
    Ok(parsed.area)
}

/// An area name that can sit inside the single-quoted `--hold` the
/// brief prints. The list's own rows are pinned to this shape; a mark
/// written by hand may not be.
fn shell_safe(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn names(areas: &[Area]) -> String {
    areas
        .iter()
        .map(|a| a.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// [`areas`] read from the tree the verb stands in.
pub(crate) fn read_areas(repo: &Path) -> Result<Vec<Area>> {
    let path = repo.join(REGISTRY);
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    areas(&text).map_err(|e| anyhow::anyhow!("{REGISTRY}: {e}"))
}

// WHICH TREE THE TRUST VERBS READ (backlog a693cf9d). They took it from
// `brief::repo_root()` — the cwd's worktree — and an analyst is told to
// work in its scratchpad run directory, which is no worktree: `boss
// mark` and `boss triage --trust-boundary` refused there, and the offer
// printed one line AFTER the triage had routed the item unmarked (runs
// 233c89a2, 47fbbf64, 5c847a5e, 12b3659b, 2026-09-28). So the list is
// looked for in three places, in order, and a miss in all three is a
// refusal naming each one and why it did not answer. `repo_root()`
// itself is left as it is: its other callers derive a brief's
// invariants from the worktree a builder stands in, and want exactly it.

/// The variable naming a checkout to read the list from when the verb
/// is not run inside one. The pod's `boss` shim sets it to the checkout
/// the shim belongs to (infra/dev/boss), unless the caller already did.
pub(crate) const TREE_ENV: &str = "BOSS_TREE";

/// The checkout this binary was compiled from — the last place looked.
/// Three levels above this crate's manifest; on a pod build it is the
/// operator's checkout, and on an image build a path the pod does not
/// have, which then says so in the refusal.
pub(crate) const BUILT_FROM: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../..");

/// One place the list is looked for: what it is, as a reader meets it,
/// and the tree it names — or why it names none.
pub(crate) struct Place {
    what: String,
    tree: std::result::Result<PathBuf, String>,
}

/// The places, in order: the worktree `cwd` stands in, the tree
/// [`TREE_ENV`] names (empty is unset), the checkout at `built`.
pub(crate) fn places(cwd: &Path, named: Option<OsString>, built: &Path) -> Vec<Place> {
    let worktree = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|e| format!("could not run git: {e}"))
        .and_then(|out| {
            let top = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if out.status.success() && !top.is_empty() {
                Ok(PathBuf::from(top))
            } else {
                Err(format!(
                    "not inside a git worktree ({})",
                    String::from_utf8_lossy(&out.stderr).trim()
                ))
            }
        });
    let named = named
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| format!("{TREE_ENV} is unset"));
    vec![
        Place {
            what: format!("the worktree the verb stands in ({})", cwd.display()),
            tree: worktree,
        },
        Place {
            what: format!("${TREE_ENV}"),
            tree: named,
        },
        Place {
            what: "the checkout this binary was built from".into(),
            tree: Ok(built.to_path_buf()),
        },
    ]
}

/// The first place whose tree holds [`REGISTRY`], or a refusal naming
/// every place and why each did not answer.
pub(crate) fn locate(places: &[Place]) -> Result<PathBuf> {
    let mut misses = Vec::new();
    for p in places {
        match &p.tree {
            Ok(tree) if tree.join(REGISTRY).is_file() => return Ok(tree.clone()),
            Ok(tree) => misses.push(format!(
                "  {} — {} holds no {REGISTRY}",
                p.what,
                tree.display()
            )),
            Err(why) => misses.push(format!("  {} — {why}", p.what)),
        }
    }
    anyhow::bail!(
        "REFUSED — no {REGISTRY} to read the trust areas from. Looked in, in order:\n{}\n  \
         Run the verb from a checkout, or name one: {TREE_ENV}=<checkout> boss …",
        misses.join("\n")
    )
}

/// [`read_areas`] from the tree [`locate`] finds for this process: its
/// cwd, its [`TREE_ENV`], the checkout it was built from.
pub(crate) fn read_areas_for_the_verb() -> Result<Vec<Area>> {
    let cwd = std::env::current_dir().context("reading the current directory")?;
    let tree = locate(&places(
        &cwd,
        std::env::var_os(TREE_ENV),
        Path::new(BUILT_FROM),
    ))?;
    read_areas(&tree)
}

/// What a packet's mark says. Either half may be absent on a mark
/// written by hand; the mark is still a mark.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Mark {
    pub area: Option<String>,
    pub reason: Option<String>,
}

impl Mark {
    /// The value written under [`KEY`].
    pub(crate) fn value(&self) -> Value {
        json!({ "area": self.area, "reason": self.reason })
    }

    /// The `--hold` reason the brief and the refusal hand the builder.
    /// Built only from the area NAME and the item's short id — never
    /// the free-text reason — because it is printed inside single
    /// quotes for a shell, and a name that could break them is left
    /// out rather than escaped.
    pub(crate) fn hold_reason(&self, item: &str) -> String {
        let short: String = item.chars().take(8).collect();
        let short = if shell_safe(&short) {
            short
        } else {
            String::new()
        };
        let area = self.area.as_deref().filter(|a| shell_safe(a));
        let what = match (area, short.is_empty()) {
            (Some(a), false) => format!(" (area {a}, item {short})"),
            (Some(a), true) => format!(" (area {a})"),
            (None, false) => format!(" (item {short})"),
            (None, true) => String::new(),
        };
        format!("trust-boundary car{what}: waits for its adversarial review before it boards")
    }

    /// `area — what it covers` as a reader meets it, judged against the
    /// list when the list could be read.
    fn area_line(&self, areas: Option<&[Area]>) -> String {
        let Some(area) = self.area.as_deref() else {
            return "(none recorded — the mark names no area; the hold stands regardless)".into();
        };
        match areas {
            Some(list) => match list.iter().find(|a| a.name == area) {
                Some(a) => format!("{area} — {} ({REGISTRY})", a.what),
                None => format!(
                    "{area} — not an area {REGISTRY} lists ({}); the hold stands regardless",
                    names(list)
                ),
            },
            None => format!("{area} — {REGISTRY} could not be read to say what it covers"),
        }
    }

    fn reason_line(&self) -> &str {
        self.reason
            .as_deref()
            .filter(|r| !r.trim().is_empty())
            .unwrap_or("(none recorded)")
    }
}

/// The mark a packet carries, if any: `metadata.trust_boundary` present
/// and neither `null` nor `false`. Tolerates the `{data: …}` envelope.
pub(crate) fn declared(packet: &Value) -> Option<Mark> {
    let v = packet
        .get("data")
        .unwrap_or(packet)
        .get("metadata")?
        .get(KEY)?;
    let text = |v: Option<&Value>| {
        v.and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    match v {
        Value::Null | Value::Bool(false) => None,
        Value::Object(o) => Some(Mark {
            area: text(o.get("area")),
            reason: text(o.get("reason")),
        }),
        Value::String(_) => Some(Mark {
            area: None,
            reason: text(Some(v)),
        }),
        _ => Some(Mark {
            area: None,
            reason: None,
        }),
    }
}

/// The mark `boss triage` writes: the area must be one [`REGISTRY`]
/// lists (the refusal names them) and the reason must say something.
pub(crate) fn mark(areas: &[Area], area: &str, reason: &str) -> std::result::Result<Mark, String> {
    let area = area.trim();
    if !areas.iter().any(|a| a.name == area) {
        return Err(format!(
            "--trust-boundary {area}: not an area {REGISTRY} lists — one of [{}]. A new \
             kind of trust boundary is a row in that file, not a word typed here",
            names(areas)
        ));
    }
    let reason = reason.trim();
    if reason.is_empty() {
        return Err(format!(
            "--trust-reason is blank — the builder and the reviewer read it to know what \
             crosses `{area}`, and a mark with nothing behind it is a hold nobody can judge"
        ));
    }
    Ok(Mark {
        area: Some(area.to_string()),
        reason: Some(reason.to_string()),
    })
}

/// What `boss mark` does to a packet, given the mark asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Plan {
    /// The packet already carries a mark in the asked-for area — by
    /// `boss triage`, `boss mark`, or a hand patch — so nothing is
    /// written, and the mark it holds (reason and all) stands.
    Already(Mark),
    /// Write the mark, replacing the one named, if any.
    Write(Option<Mark>),
}

/// [`Plan`] for `want` on `packet`. ALREADY is judged on the AREA only:
/// the area is what holds the car and what the gate and the brief
/// read, and a second writer's reason overwriting the first's — or a
/// hand mark's `note` explaining why it was written by hand (b8e75382,
/// 6c9183de, 2710c8fc, 2026-09-28) — would be a record lost to a re-run.
/// A different area is a re-mark; the verb names what it replaced.
pub(crate) fn plan(packet: &Value, want: &Mark) -> Plan {
    match declared(packet) {
        Some(have) if have.area.is_some() && have.area == want.area => Plan::Already(have),
        other => Plan::Write(other),
    }
}

/// What `boss triage` says on a `build` or `design` route that carries
/// no mark: the areas, and how to mark the item before it is
/// dispatched. Said, not asked — the verb runs unattended.
pub(crate) fn offer(areas: &[Area], item: &str) -> String {
    let short: String = item.chars().take(8).collect();
    let rows: Vec<String> = areas
        .iter()
        .map(|a| format!("    {} — {}", a.name, a.what))
        .collect();
    format!(
        "boss triage: {short} is not marked trust-boundary. If its change crosses one of \
         these areas ({REGISTRY}), its car must wait at the dock HELD for an adversarial \
         review:\n{}\n  Mark it with the triage (--trust-boundary <area> --trust-reason \
         '<why>'), or after it with `boss mark {short} --trust-boundary <area> --trust-reason \
         '<why>'` — before it is dispatched. Either writes metadata.{KEY}, so the brief names \
         the hold and the gate enforces it (backlog 486dde37, b9352041).",
        rows.join("\n")
    )
}

/// The brief's section for a marked packet — `None` for an unmarked
/// one. `areas` is the list as the brief could read it; `None` says it
/// could not, and the section says so rather than guessing.
pub(crate) fn brief_section(packet: &Value, areas: Option<&[Area]>) -> Option<String> {
    let m = declared(packet)?;
    let id = packet
        .get("data")
        .unwrap_or(packet)
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("");
    Some(format!(
        "== TRUST BOUNDARY — declared on this packet (metadata.{KEY}), so its car is gated HELD \
         ==\n\n\
         area:    {}\n\
         reason:  {}\n\n\
         This car waits for an adversarial review before it may board. Your `boss gate` MUST \
         carry --hold beside every --park-* flag (builder rules, rule 7):\n\n    \
         --hold '{}'\n\n\
         `boss gate` refuses a park-intent gate on this item without --hold: an unheld car \
         boards on depth within minutes of its green, before anyone can place `boss hold` \
         (backlog 486dde37). On green the car is filed with its whole receipt and stands at \
         the dock HELD; the operator's `boss release` is the one way out.\n",
        m.area_line(areas),
        m.reason_line(),
        m.hold_reason(id),
    ))
}

/// `boss gate`'s refusal: park intent, no `--hold`, and an item the
/// park intent names carries a mark. `marked` is every (flag, full id,
/// mark) the gate read; `None` when there is nothing to refuse.
pub(crate) fn gate_refusal(hold: Option<&str>, marked: &[(&str, String, Mark)]) -> Option<String> {
    if hold.is_some() {
        return None;
    }
    let (_, first_id, first) = marked.first()?;
    let lines: Vec<String> = marked
        .iter()
        .map(|(flag, id, m)| {
            format!(
                "  {flag} {id} — area {}: {}",
                m.area.as_deref().unwrap_or("(none recorded)"),
                m.reason_line()
            )
        })
        .collect();
    Some(format!(
        "boss gate: REFUSED — the park intent names an item marked trust-boundary \
         (metadata.{KEY}), and this gate carries no --hold:\n{}\n  Its car would be filed on \
         green and board on depth within minutes, before its adversarial review (backlog \
         486dde37). Add beside the --park-* flags:\n    --hold '{}'\n  On green the car is \
         filed with its whole receipt and stands at the dock HELD until `boss release`. If the \
         mark is wrong, lift it on the item first — `boss job patch <item> <file>` carrying \
         {{\"{KEY}\": null}} — and say why on the item.",
        lines.join("\n"),
        first.hold_reason(first_id),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ITEM: &str = "486dde37-c136-43e1-934d-2b2d454f4e67";

    fn repo() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .canonicalize()
            .expect("the workspace root is above this crate")
    }

    fn live() -> Vec<Area> {
        read_areas(&repo()).expect("the trust-area list reads")
    }

    /// A directory outside any git worktree — the shape of the analyst's
    /// scratchpad run directory, where `git rev-parse --show-toplevel`
    /// exits 128 (backlog a693cf9d, measured on run a8d2c567).
    fn outside() -> PathBuf {
        boss_testing::scratch_dir("boss-trust-outside")
    }

    /// A git worktree of its own, created by this uid (so git trusts it
    /// under the gate's uid too), holding a copy of the live list, and
    /// the subdirectory a verb would be run from.
    fn a_worktree_with_the_list() -> (PathBuf, PathBuf) {
        let tree = boss_testing::scratch_dir("boss-trust-worktree");
        let init = std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(&tree)
            .status()
            .expect("run git init");
        assert!(init.success(), "git init {}", tree.display());
        let list = tree.join(REGISTRY);
        std::fs::create_dir_all(list.parent().expect("a parent")).expect("mkdir");
        std::fs::copy(repo().join(REGISTRY), &list).expect("copy the list");
        let sub = tree.join("sub");
        std::fs::create_dir_all(&sub).expect("mkdir sub");
        (tree, sub)
    }

    /// THE PACKET'S CASE (backlog a693cf9d): run from outside any
    /// worktree, the verb reads the list from the checkout the
    /// environment names — it does not give up because cwd is not a tree.
    #[test]
    fn outside_a_worktree_the_list_is_read_from_the_tree_the_environment_names() {
        let found = locate(&places(
            &outside(),
            Some(repo().into_os_string()),
            &outside().join("no-such-checkout"),
        ))
        .expect("the named tree holds the list");
        assert_eq!(found, repo());
        assert_eq!(read_areas(&found).expect("reads"), live());
    }

    /// Nothing named: the checkout this binary was built from answers —
    /// the last place, before the refusal.
    #[test]
    fn outside_a_worktree_with_nothing_named_the_checkout_the_binary_was_built_from_answers() {
        let found = locate(&places(&outside(), None, &repo())).expect("the build's checkout");
        assert_eq!(found, repo());
        assert!(
            BUILT_FROM.ends_with("/../../.."),
            "the build's checkout is three levels above this crate: {BUILT_FROM}"
        );
    }

    /// The worktree the verb stands in is read FIRST — a builder's
    /// worktree carrying a new area must offer it, not the operator's
    /// checkout's older list — and from a subdirectory of it too.
    #[test]
    fn the_worktree_the_verb_stands_in_is_read_first() {
        let (tree, sub) = a_worktree_with_the_list();
        let found = locate(&places(&sub, Some(repo().into_os_string()), &repo()))
            .expect("the cwd's worktree holds the list");
        assert_eq!(
            found.canonicalize().expect("canonical"),
            tree.canonicalize().expect("canonical")
        );
    }

    /// NO LIST ANYWHERE IS A REFUSAL THAT NAMES EVERY PLACE IT LOOKED,
    /// and why each one did not answer — never a skip. An empty
    /// variable is unset, not the empty path.
    #[test]
    fn with_no_list_anywhere_the_verb_refuses_naming_every_place_it_looked() {
        let cwd = outside();
        let named = outside();
        let built = outside().join("no-such-checkout");
        let err = locate(&places(&cwd, Some(named.clone().into_os_string()), &built))
            .expect_err("no list anywhere")
            .to_string();
        for must in [
            REGISTRY,
            TREE_ENV,
            "not inside a git worktree",
            cwd.to_str().expect("utf8"),
            named.to_str().expect("utf8"),
            built.to_str().expect("utf8"),
        ] {
            assert!(err.contains(must), "the refusal must name {must}: {err}");
        }
        let err = locate(&places(&cwd, Some("".into()), &built))
            .expect_err("empty is unset")
            .to_string();
        assert!(err.contains(&format!("{TREE_ENV} is unset")), "{err}");
    }

    fn marked(md: Value) -> Value {
        json!({ "id": ITEM, "kind": "backlog-item", "metadata": md })
    }

    #[test]
    fn the_registry_lists_the_areas_the_operator_marked_by_hand() {
        let names: Vec<String> = live().into_iter().map(|a| a.name).collect();
        // The four kinds the operator marked on 2026-09-27, plus
        // publishing — the list's first rows. A row may be added; one
        // of these going is a decision, and this names it.
        for want in [
            "mutating-verb",
            "gateway-route",
            "policy",
            "credentials",
            "publish",
        ] {
            assert!(names.iter().any(|n| n == want), "{want} in {names:?}");
        }
        for a in live() {
            assert!(!a.what.trim().is_empty(), "{} says what it covers", a.name);
            assert!(
                a.name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "{} is shell-safe inside the brief's single-quoted --hold",
                a.name
            );
        }
    }

    #[test]
    fn an_empty_or_malformed_list_is_refused_not_read_as_no_areas() {
        assert!(areas("# nothing\n").is_err(), "no rows");
        assert!(areas("[[area]]\nname = 1\n").is_err(), "malformed");
        let two =
            areas("[[area]]\nname = \"a\"\nwhat = \"x\"\n[[area]]\nname = \"b\"\nwhat = \"y\"\n")
                .expect("two rows");
        assert_eq!(two.len(), 2);
        assert_eq!(two[1].name, "b");
    }

    #[test]
    fn a_mark_names_an_area_the_list_holds_and_says_why() {
        let list = live();
        let m = mark(&list, "credentials", "  rotates the forge token  ").expect("listed");
        assert_eq!(m.area.as_deref(), Some("credentials"));
        assert_eq!(m.reason.as_deref(), Some("rotates the forge token"));
        assert_eq!(
            m.value(),
            json!({ "area": "credentials", "reason": "rotates the forge token" })
        );
        let err = mark(&list, "secrets", "x").expect_err("not listed");
        assert!(
            err.contains("secrets") && err.contains("credentials") && err.contains(REGISTRY),
            "the refusal names the areas and where they live: {err}"
        );
        let err = mark(&list, "policy", "   ").expect_err("blank reason");
        assert!(err.contains("--trust-reason"), "{err}");
    }

    #[test]
    fn a_mark_is_read_off_the_packet_present_or_absent() {
        assert_eq!(declared(&marked(json!({}))), None);
        assert_eq!(declared(&marked(json!({ KEY: null }))), None);
        assert_eq!(declared(&marked(json!({ KEY: false }))), None);
        assert_eq!(
            declared(
                &json!({ "data": marked(json!({ KEY: { "area": "policy", "reason": "grants" } })) })
            ),
            Some(Mark {
                area: Some("policy".into()),
                reason: Some("grants".into())
            }),
            "through the envelope"
        );
        // A hand-written mark is still a mark: the hold is the safe side.
        assert_eq!(
            declared(&marked(json!({ KEY: "touches the broker" }))),
            Some(Mark {
                area: None,
                reason: Some("touches the broker".into())
            })
        );
        assert_eq!(
            declared(&marked(json!({ KEY: true }))),
            Some(Mark {
                area: None,
                reason: None
            })
        );
    }

    #[test]
    fn the_hold_reason_carries_the_area_name_and_never_the_free_text() {
        let m = Mark {
            area: Some("gateway-route".into()),
            reason: Some("it's the route's auth".into()),
        };
        let r = m.hold_reason(ITEM);
        assert!(r.contains("gateway-route") && r.contains("486dde37"), "{r}");
        assert!(r.contains("adversarial review"), "{r}");
        assert!(!r.contains('\''), "safe inside single quotes: {r}");
        let odd = Mark {
            area: Some("it's".into()),
            reason: None,
        };
        assert!(
            !odd.hold_reason(ITEM).contains('\''),
            "an unsafe name is left out"
        );
    }

    /// THE PACKET (backlog 486dde37): the brief of a marked item names
    /// the hold, the exact flag, and the refusal that backs it.
    #[test]
    fn a_trust_boundary_items_brief_names_the_hold() {
        let list = live();
        let job = marked(json!({ KEY: { "area": "policy", "reason": "changes who may publish" } }));
        let s = brief_section(&job, Some(&list)).expect("a marked packet has a section");
        assert!(s.starts_with("== TRUST BOUNDARY"), "{s}");
        assert!(
            s.contains("policy") && s.contains("changes who may publish"),
            "{s}"
        );
        let policy = list.iter().find(|a| a.name == "policy").unwrap();
        assert!(
            s.contains(&policy.what),
            "the area's own sentence, read from the list: {s}"
        );
        assert!(
            s.contains(&format!(
                "--hold '{}'",
                Mark {
                    area: Some("policy".into()),
                    reason: None
                }
                .hold_reason(ITEM)
            )),
            "the exact flag to add: {s}"
        );
        assert!(s.contains("--park-") && s.contains("boss release"), "{s}");
        assert!(s.contains("refuses"), "the gate's refusal is named: {s}");

        assert_eq!(brief_section(&marked(json!({})), Some(&list)), None);

        let unknown = marked(json!({ KEY: { "area": "vibes", "reason": "x" } }));
        let s = brief_section(&unknown, Some(&list)).unwrap();
        assert!(s.contains("vibes") && s.contains("not an area"), "{s}");
        assert!(s.contains("--hold '"), "the hold stands regardless: {s}");

        let unread = brief_section(&job, None).unwrap();
        assert!(
            unread.contains(REGISTRY) && unread.contains("--hold '"),
            "{unread}"
        );
    }

    /// THE PACKET: a park-intent gate on a marked item without `--hold`
    /// is refused, naming the item, the flag and the fix; with `--hold`,
    /// or with nothing marked, it is not.
    #[test]
    fn a_gate_without_hold_on_a_trust_boundary_item_is_refused() {
        let m = Mark {
            area: Some("credentials".into()),
            reason: Some("rotates the forge token".into()),
        };
        let named = vec![("--park-backlog-item", ITEM.to_string(), m.clone())];
        let why = gate_refusal(None, &named).expect("refused");
        assert!(why.contains("REFUSED"), "{why}");
        assert!(
            why.contains("--park-backlog-item") && why.contains(ITEM),
            "{why}"
        );
        assert!(
            why.contains("credentials") && why.contains("rotates the forge token"),
            "{why}"
        );
        assert!(
            why.contains(&format!("--hold '{}'", m.hold_reason(ITEM))),
            "the fix, ready to add: {why}"
        );
        assert!(why.contains(KEY), "how to lift a wrong mark: {why}");
        assert_eq!(gate_refusal(Some("adversarial review"), &named), None);
        assert_eq!(gate_refusal(None, &[]), None);
    }

    #[test]
    fn a_build_route_with_no_mark_is_offered_the_areas() {
        let list = live();
        let o = offer(&list, ITEM);
        for a in &list {
            assert!(o.contains(&a.name), "{} offered: {o}", a.name);
        }
        assert!(o.contains("--trust-boundary") && o.contains(KEY), "{o}");
        assert!(o.contains("486dde37"), "{o}");
    }

    /// THE PACKET (backlog b9352041): an item marked after its triage is
    /// marked through the validated door, and the offer says so. It named
    /// `boss job patch`, which writes the key unchecked — so an unlisted
    /// area reached the packet and was caught only by the brief.
    #[test]
    fn the_offer_names_the_validated_mark_door_not_job_patch() {
        let o = offer(&live(), ITEM);
        assert!(
            o.contains("boss mark 486dde37 --trust-boundary <area> --trust-reason"),
            "{o}"
        );
        assert!(!o.contains("boss job patch"), "{o}");
    }

    /// The three items the coordinator marked on 2026-09-28 through
    /// `boss job patch`, before this door existed, carry the mark in
    /// this shape — `marked_at`, `marked_by` and a `note` beside the
    /// area and reason (b8e75382, read live at 05:0xZ). `boss mark` on
    /// the same area is ALREADY MARKED and writes nothing, so running it
    /// over them is safe; another area is a re-mark naming the old one.
    #[test]
    fn a_mark_in_the_same_area_is_already_marked_whoever_wrote_it() {
        let by_hand = marked(json!({ KEY: {
            "area": "policy",
            "marked_at": "2026-09-28T04:59:58Z",
            "marked_by": "agent-claude",
            "note": "marked through boss job patch because the validated mark door (b9352041) is not built yet",
            "reason": "Policy authority is not bounded",
        } }));
        let want = mark(&live(), "policy", "the fix edits the privilege model").unwrap();
        assert_eq!(
            plan(&by_hand, &want),
            Plan::Already(Mark {
                area: Some("policy".into()),
                reason: Some("Policy authority is not bounded".into()),
            }),
            "the mark it holds is kept, reason and all"
        );

        let other = mark(&live(), "credentials", "reads a token").unwrap();
        assert_eq!(
            plan(&by_hand, &other),
            Plan::Write(Some(Mark {
                area: Some("policy".into()),
                reason: Some("Policy authority is not bounded".into()),
            })),
            "a different area re-marks, naming what it replaces"
        );

        assert_eq!(plan(&marked(json!({})), &want), Plan::Write(None));
        // A hand mark with no area is not the mark asked for.
        assert_eq!(
            plan(&marked(json!({ KEY: true })), &want),
            Plan::Write(Some(Mark {
                area: None,
                reason: None
            }))
        );
    }
}
