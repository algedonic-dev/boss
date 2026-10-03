//! The automation half of the agents registry: one row for every
//! `automation:*` id that writes, carrying the role it signs as today.
//!
//! WHY (backlog ddf0773e; design abf9eeae, decided 2026-10-01, car 1 of
//! 5). Every machine caller asserts `platform-admin` in its own
//! `x-boss-user`, and every service believes it — the role is a string
//! spelled in ~57 files, which nothing can audit or narrow. The design
//! moves the role onto the record: a service resolves the caller's role
//! from a registry instead of the header (car 2). People have their
//! employee row and agents their `agents` row; the `automation:*` ids —
//! 1,050 of 1,111 step completions when the item was measured — had no
//! row anywhere, so a registry read had no answer for them at all. This
//! module is that row. It refuses nothing and changes no one's access:
//! every row carries the role its caller already asserts, and nothing
//! reads a row to decide anything until car 2's resolver, which ships
//! in `report` mode.
//!
//! WHY A TABLE OF ITS OWN, NOT ROWS IN `agents`. An `agents` row is an
//! executor the dispatcher can NOMINATE: `roster_union` folds every
//! agent that holds a role into the roster, and `eligible_candidates`
//! admits it for any step whose audience is that role. An automation
//! row holding `platform-admin` there would have made the train
//! conductor a candidate for the founder's own steps — the opposite of
//! changing nothing. So the automations sit beside the agents in the
//! same registry (one port, one door), in `automation_actors`, which no
//! roster, alias door, claim gate or budget reads.
//!
//! A FAMILY IS ONE ROW (`signs_for`). Two writers sign as an id per
//! firing rather than one id: the dispatcher writes as
//! `automation:rule:<name>` for each of its rules — 74 distinct rule
//! ids in the week measured, tenant-declared rules among them (the
//! `dispatcher_rules` registry, not the tree, is where a rule's name
//! lives) — and the ops runner as `automation:ops-runner:<host>:<run>`.
//! A row per rule would copy that registry into this one and drift
//! from it the day a rule is authored (CLAUDE.md §9a). So the row of
//! the process that signs declares the prefix it signs for, and
//! [`row_of_record`] answers a family member with that row.
//!
//! The rows are DATA: the platform bundle `infra/platform/automations/`,
//! one file per row, published at every start by
//! `boss-platform-workflow-seed` insert-if-absent (the instance is the
//! truth, design e187198f: a row an operator narrowed is kept, and the
//! seed names the field it differs on). [`unregistered`] is the report
//! that names a writer with no row — `boss automations unregistered`
//! runs it over the live audit log.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Every automation id's prefix — the wire spelling of
/// `boss_core::actor::ActorId::Automation`, and what an `_actor` in the
/// audit log carries once `ambient_actor` has normalised `rule:<name>`
/// and `system:<proc>`.
pub const AUTOMATION_PREFIX: &str = "automation:";

/// The fact the platform seed leaves for each row it INSERTED: the row
/// as declared plus `declared_by`. None for a row the registry already
/// held, none per run — the `agent.declared` rule (backlog d9409039).
pub const AUTOMATION_DECLARED: &str = "automation.declared";

/// One writing automation, as its bundle file declares it and as
/// `automation_actors` holds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationActor {
    /// `automation:<slug>` — the id the caller signs as. The SQL CHECK
    /// on `automation_actors.id` spells the same shape.
    pub id: String,
    /// The role this automation signs as today — a Class code under
    /// `(employee, role)`, the vocabulary an employee's and an agent's
    /// role are spelled in.
    pub role: String,
    /// What the automation is and where it runs, for whoever narrows
    /// its role later.
    pub description: String,
    /// The id prefix this automation also signs as, one id per firing
    /// (`automation:rule:` for the dispatcher). `None` for most.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signs_for: Option<String>,
}

fn is_slug(s: &str) -> bool {
    s.chars()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Whether `id` is `automation:<slug>` — the Rust spelling of the SQL
/// CHECK on `automation_actors.id` (`^automation:[a-z0-9][a-z0-9-]*$`).
/// A colon inside the slug is a FAMILY member, declared by its signer's
/// `signs_for`, never a row of its own.
pub fn is_automation_id(id: &str) -> bool {
    id.strip_prefix(AUTOMATION_PREFIX).is_some_and(is_slug)
}

/// Whether `prefix` is `automation:<slug>:` — the shape of a
/// `signs_for` (the SQL CHECK spells it `^automation:[a-z0-9][a-z0-9-]*:$`).
pub fn is_family_prefix(prefix: &str) -> bool {
    prefix
        .strip_prefix(AUTOMATION_PREFIX)
        .and_then(|s| s.strip_suffix(':'))
        .is_some_and(is_slug)
}

/// Why a row is refused, naming it — the same check the loader and the
/// in-memory adapter run, so the tree, the double and Postgres refuse
/// the same rows.
pub fn validate_automation(a: &AutomationActor) -> Result<(), String> {
    if !is_automation_id(&a.id) {
        return Err(format!(
            "automation {}: id must be {AUTOMATION_PREFIX}<slug> (lowercase, digits, hyphens) — \
             an id signed one per firing is a family, declared by its signer's `signs_for`",
            a.id
        ));
    }
    if a.role.trim().is_empty() {
        return Err(format!(
            "automation {}: role is required — the role it signs as today, a Class code \
             under (employee, role)",
            a.id
        ));
    }
    if a.description.trim().is_empty() {
        return Err(format!("automation {}: description is required", a.id));
    }
    if let Some(prefix) = &a.signs_for
        && !is_family_prefix(prefix)
    {
        return Err(format!(
            "automation {}: signs_for {prefix:?} must be {AUTOMATION_PREFIX}<slug>: \
             (the prefix of the ids it signs one per firing)",
            a.id
        ));
    }
    Ok(())
}

/// Every row valid, no id twice, no family claimed by two signers.
pub fn validate_all(rows: &[AutomationActor]) -> Result<(), String> {
    let mut ids = BTreeSet::new();
    let mut families = BTreeMap::new();
    for a in rows {
        validate_automation(a)?;
        if !ids.insert(a.id.as_str()) {
            return Err(format!("automation {} is declared twice", a.id));
        }
        if let Some(prefix) = &a.signs_for
            && let Some(other) = families.insert(prefix.as_str(), a.id.as_str())
        {
            return Err(format!(
                "signs_for {prefix} is declared by both {other} and {} — a family signs as ONE \
                 automation",
                a.id
            ));
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AutomationFile {
    #[serde(default)]
    automation: Vec<AutomationActor>,
}

/// Parse one bundle file's text: exactly one `[[automation]]`, named
/// after the file (`stem` is the id without its prefix) — the bundle
/// convention of every `infra/platform/` registry, so a file cannot
/// say one id while its name says another.
pub fn parse_automation_file(text: &str, stem: &str) -> Result<AutomationActor, String> {
    let file: AutomationFile = toml::from_str(text).map_err(|e| e.to_string())?;
    let ids: Vec<&str> = file.automation.iter().map(|a| a.id.as_str()).collect();
    let want = format!("{AUTOMATION_PREFIX}{stem}");
    if ids != [want.as_str()] {
        return Err(format!(
            "an automation file holds exactly one [[automation]] named after the file \
             (expected id `{want}`, found {ids:?})"
        ));
    }
    let row = file
        .automation
        .into_iter()
        .next()
        .ok_or("unreachable: one row checked")?;
    validate_automation(&row)?;
    Ok(row)
}

/// Load the bundle directory: every `*.toml` in byte order, one row
/// each, then [`validate_all`] across them. A README or any other file
/// is not a row.
pub fn load_automations_dir(dir: &Path) -> Result<Vec<AutomationActor>, String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("read {}: {e}", dir.display()))?;
    let mut files: Vec<_> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .collect();
    files.sort();
    let rows = files
        .iter()
        .map(|file| {
            let stem = file
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            let text = std::fs::read_to_string(file)
                .map_err(|e| format!("read {}: {e}", file.display()))?;
            parse_automation_file(&text, stem).map_err(|e| format!("{}: {e}", file.display()))
        })
        .collect::<Result<Vec<_>, String>>()?;
    validate_all(&rows)?;
    Ok(rows)
}

/// The bundle directory beside the Workflow bundle —
/// `infra/platform/automations` for the in-tree default, found the way
/// every sibling bundle of `boss-platform-workflow-seed` is.
pub fn automations_beside(workflows: &Path) -> std::path::PathBuf {
    workflows
        .parent()
        .map(|p| p.join("automations"))
        .unwrap_or_else(|| std::path::PathBuf::from("automations"))
}

/// The in-tree bundle, resolved from this crate — the
/// `crate::station_seed::platform_stations_path` shape.
pub fn platform_automations_path() -> &'static str {
    concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../infra/platform/automations"
    )
}

/// The row that answers for `actor`: its own row, else the row whose
/// `signs_for` is a prefix of it (the longest, should two nest). `None`
/// for an id no row answers — and for anything that is not an
/// automation, whose role of record lives on an employee or agent row.
pub fn row_of_record<'a>(rows: &'a [AutomationActor], actor: &str) -> Option<&'a AutomationActor> {
    if !actor.starts_with(AUTOMATION_PREFIX) {
        return None;
    }
    rows.iter().find(|a| a.id == actor).or_else(|| {
        rows.iter()
            .filter(|a| {
                a.signs_for
                    .as_deref()
                    .is_some_and(|p| actor.starts_with(p) && actor.len() > p.len())
            })
            .max_by_key(|a| a.signs_for.as_ref().map_or(0, String::len))
    })
}

/// Every `automation:*` writer in `writers` (id → how many facts it
/// wrote) that no row answers for, most prolific first, then by id.
/// The report the registry exists to make: a new automation that
/// starts writing without a row is NAMED here, with its count, rather
/// than discovered when a resolver first has no role for it.
pub fn unregistered<'a>(
    rows: &[AutomationActor],
    writers: &'a BTreeMap<String, u64>,
) -> Vec<(&'a str, u64)> {
    let mut out: Vec<(&str, u64)> = writers
        .iter()
        .filter(|(id, _)| id.starts_with(AUTOMATION_PREFIX) && row_of_record(rows, id).is_none())
        .map(|(id, n)| (id.as_str(), *n))
        .collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    out
}

/// What the platform seed did with its bundle — or, on a dry run,
/// would do. A row the registry held is KEPT as the instance holds it
/// (design e187198f), with every declared field it differs on named.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct AutomationsSeedOutcome {
    pub inserted: Vec<String>,
    pub present: Vec<String>,
    pub kept: Vec<KeptAutomation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KeptAutomation {
    pub id: String,
    /// The declared fields the held row differs on, in field order.
    pub differs: Vec<&'static str>,
}

/// The declared fields `held` differs from `declared` on.
pub fn differs(held: &AutomationActor, declared: &AutomationActor) -> Vec<&'static str> {
    [
        ("role", held.role != declared.role),
        ("description", held.description != declared.description),
        ("signs_for", held.signs_for != declared.signs_for),
    ]
    .into_iter()
    .filter_map(|(f, d)| d.then_some(f))
    .collect()
}

/// Classify a bundle against what the registry holds, writing nothing:
/// the dry run's answer, and the same table the adapters' write
/// follows.
pub fn classify(held: &[AutomationActor], declared: &[AutomationActor]) -> AutomationsSeedOutcome {
    let by_id: BTreeMap<&str, &AutomationActor> = held.iter().map(|a| (a.id.as_str(), a)).collect();
    let mut out = AutomationsSeedOutcome::default();
    for d in declared {
        match by_id.get(d.id.as_str()) {
            None => out.inserted.push(d.id.clone()),
            Some(h) => {
                let differs = differs(h, d);
                if differs.is_empty() {
                    out.present.push(d.id.clone());
                } else {
                    out.kept.push(KeptAutomation {
                        id: d.id.clone(),
                        differs,
                    });
                }
            }
        }
    }
    out
}

impl std::fmt::Display for AutomationsSeedOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "platform-automation-seed: {} inserted, {} present, {} kept as the instance holds them",
            self.inserted.len(),
            self.present.len(),
            self.kept.len()
        )?;
        if !self.inserted.is_empty() {
            write!(f, "\n  inserted: {}", self.inserted.join(", "))?;
        }
        for k in &self.kept {
            write!(
                f,
                "\n  kept {} — the live row differs from the bundle on: {}",
                k.id,
                k.differs.join(", ")
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, signs_for: Option<&str>) -> AutomationActor {
        AutomationActor {
            id: id.to_string(),
            role: "platform-admin".to_string(),
            description: "a test automation".to_string(),
            signs_for: signs_for.map(str::to_string),
        }
    }

    fn writers(pairs: &[(&str, u64)]) -> BTreeMap<String, u64> {
        pairs.iter().map(|(id, n)| (id.to_string(), *n)).collect()
    }

    /// The report this car exists for: an automation that starts
    /// writing with no row is NAMED, with its count — and a registered
    /// one, a family member, a person and an agent are not.
    #[test]
    fn a_new_automation_writing_without_a_row_is_named() {
        let rows = vec![
            row("automation:train-conductor", None),
            row("automation:dispatcher", Some("automation:rule:")),
        ];
        let seen = writers(&[
            ("automation:train-conductor", 900),
            ("automation:rule:converge-on-merge", 48),
            ("automation:brand-new-sweep", 3),
            ("automation:another-new-one", 7),
            ("agent-claude", 2861),
            ("emp-david", 36),
        ]);
        assert_eq!(
            unregistered(&rows, &seen),
            vec![
                ("automation:another-new-one", 7),
                ("automation:brand-new-sweep", 3)
            ]
        );
        // Registering it is what silences it.
        let rows = [rows, vec![row("automation:brand-new-sweep", None)]].concat();
        assert_eq!(
            unregistered(&rows, &seen),
            vec![("automation:another-new-one", 7)]
        );
    }

    #[test]
    fn a_family_member_is_answered_by_its_signer_and_the_bare_prefix_is_not() {
        let rows = vec![
            row("automation:dispatcher", Some("automation:rule:")),
            row("automation:ops-runner", Some("automation:ops-runner:")),
        ];
        let of = |id| row_of_record(&rows, id).map(|a| a.id.as_str());
        assert_eq!(
            of("automation:rule:auto-park-on-gate-green"),
            Some("automation:dispatcher")
        );
        assert_eq!(
            of("automation:ops-runner:forge:20260930T201448Z-2040087"),
            Some("automation:ops-runner")
        );
        assert_eq!(of("automation:ops-runner"), Some("automation:ops-runner"));
        assert_eq!(of("automation:dispatcher"), Some("automation:dispatcher"));
        // The prefix alone names no member; a near-miss is no member.
        assert_eq!(of("automation:rule:"), None);
        assert_eq!(of("automation:rules-engine"), None);
        // Not an automation: its role of record is not this registry's.
        assert_eq!(of("rule:auto-park-on-gate-green"), None);
        assert_eq!(of("agent-claude"), None);
    }

    #[test]
    fn a_bad_row_a_twin_and_a_shared_family_are_refused_by_name() {
        for (bad, says) in [
            (row("automation:rule:x", None), "family"),
            (row("train-conductor", None), "automation:<slug>"),
            (row("automation:Conductor", None), "automation:<slug>"),
            (row("automation:x", Some("automation:rule")), "signs_for"),
            (row("automation:x", Some("rule:")), "signs_for"),
        ] {
            let why = validate_automation(&bad).unwrap_err();
            assert!(why.contains(says), "{bad:?}: {why}");
        }
        let blank_role = AutomationActor {
            role: " ".into(),
            ..row("automation:x", None)
        };
        assert!(
            validate_automation(&blank_role)
                .unwrap_err()
                .contains("role")
        );
        let twice = [row("automation:x", None), row("automation:x", None)];
        assert!(validate_all(&twice).unwrap_err().contains("twice"));
        let shared = [
            row("automation:a", Some("automation:rule:")),
            row("automation:b", Some("automation:rule:")),
        ];
        let why = validate_all(&shared).unwrap_err();
        assert!(why.contains("automation:a") && why.contains("ONE"), "{why}");
    }

    #[test]
    fn a_file_holds_one_row_named_after_it_and_no_unknown_key() {
        let text = "[[automation]]\nid = \"automation:gate-runner\"\nrole = \"platform-admin\"\n\
                    description = \"the gate\"\n";
        let got = parse_automation_file(text, "gate-runner").unwrap();
        assert_eq!(got, row_with("automation:gate-runner", "the gate"));
        assert!(
            parse_automation_file(text, "gate")
                .unwrap_err()
                .contains("named after the file")
        );
        let two = format!("{text}{}", text.replace("gate-runner", "other"));
        assert!(parse_automation_file(&two, "gate-runner").is_err());
        // A key the table cannot hold is refused, never dropped.
        let extra = format!("{text}department = \"it\"\n");
        assert!(
            parse_automation_file(&extra, "gate-runner")
                .unwrap_err()
                .contains("department")
        );
    }

    fn row_with(id: &str, description: &str) -> AutomationActor {
        AutomationActor {
            description: description.to_string(),
            ..row(id, None)
        }
    }

    #[test]
    fn the_seed_inserts_the_absent_and_keeps_a_held_row_naming_what_differs() {
        let held = vec![
            row("automation:a", None),
            AutomationActor {
                role: "audit-readonly".into(),
                ..row("automation:b", None)
            },
        ];
        let declared = vec![
            row("automation:a", None),
            row("automation:b", None),
            row("automation:c", None),
        ];
        let out = classify(&held, &declared);
        assert_eq!(out.inserted, ["automation:c"]);
        assert_eq!(out.present, ["automation:a"]);
        assert_eq!(
            out.kept,
            [KeptAutomation {
                id: "automation:b".into(),
                differs: vec!["role"]
            }]
        );
        let line = out.to_string();
        assert!(line.contains("1 inserted, 1 present, 1 kept"), "{line}");
        assert!(
            line.contains("kept automation:b") && line.contains("role"),
            "{line}"
        );
    }
}
