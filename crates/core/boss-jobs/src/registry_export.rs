//! The registries' recovery export — every row of every version of
//! the four runtime registries (workflows, stations, pools, cadence
//! rules), read through their ports into one versioned document.
//!
//! WHY (backlog c26aacaa, design 942dc0ac Q2). David, 2026-11-08:
//! "Prefer live append-only registry authority with generated
//! versioned recovery exports, conditional on tested offline restore
//! and deliberate import." Today two representations both act as the
//! current truth — the live registry row and the mutable
//! `infra/platform/<registry>/*.toml` — and a drift comparator walks
//! git history to arbitrate between them. The decision makes the live
//! row the authority after bootstrap and git a GENERATED recovery
//! artifact; this module is the first half of the condition that
//! decision is held to: the export, and the check that an export is
//! faithful to the registry it claims to copy. The restore and the
//! deliberate import read this document; retiring the comparator
//! waits until both are proven.
//!
//! WHAT AN EXPORT HOLDS. Every lineage the registry has EVER held —
//! [`crate::registry::WorkflowRegistry::lineage_names`] and its three
//! siblings, not `list_active` — so a kind an operator retired is in
//! it: a restore without that row would let the next bootstrap seed
//! re-insert the kind from the tree, reverting the retirement
//! (backlog 8b2eaff2 is that revert, once already). Every version of
//! each, oldest first, with its status, so a packet pinned to v7 of a
//! protocol finds v7 after a restore. Rows are the ports' own specs,
//! serialized as the API serves them, so the file is readable by the
//! same types that validate a publish.
//!
//! WHAT ITS DIGEST NAMES. `sha256:` over the canonical bytes
//! (`boss_core::job::canonical_json_bytes`) of the format number and
//! every row with `created_at` removed — the same "a row as a
//! declaration" `crate::bundle_seed` compares, because `created_at` is
//! when a deployment wrote the row, not what the row is. So two exports
//! of an unchanged registry carry ONE digest whatever their
//! `exported_at`, and a registry restored from an export (which stamps
//! its own clock) exports to the digest it was restored from — the
//! restore proof is an equality of two strings. [`RegistryExport::checked`]
//! recomputes it on read, so a file edited or truncated after it was
//! generated is refused, by both digests, before anything trusts it.
//!
//! WHAT IT IS NOT. Not a second authority: nothing reads an export
//! except a person verifying, restoring or deliberately importing it.
//! Not the tree bundle: an export is written by a program from the
//! live rows and is never hand-edited (its digest refuses an edit).

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::bundle_seed::{BundleRegistry, Declared, differing_fields};
use crate::cadence::{CadenceRegistry, CadenceRuleSpec};
use crate::pools::{PoolRegistry, PoolSpec};
use crate::registry::{WorkflowRegistry, WorkflowSpec};
use crate::stations::{StationRegistry, StationSpec};

/// The document format this build writes and reads. A change to what
/// an export holds is a new number, and [`RegistryExport::checked`]
/// refuses one it does not know rather than half-reading it.
pub const FORMAT: u32 = 1;

/// One recovery export: every row of every version of the four runtime
/// registries, as the ports serve them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegistryExport {
    pub format: u32,
    /// What was exported — a label the operator gives, carried so the
    /// file says which deployment it copies.
    pub source: String,
    /// When the export was read. Not part of the digest.
    pub exported_at: DateTime<Utc>,
    /// `sha256:<hex>` of the format and every row less `created_at`.
    pub digest: String,
    pub workflows: Vec<WorkflowSpec>,
    pub stations: Vec<StationSpec>,
    pub pools: Vec<PoolSpec>,
    pub cadence_rules: Vec<CadenceRuleSpec>,
}

/// The four registries an export reads — each by its port, so the
/// in-memory adapters and the Postgres ones export the same way.
pub struct Registries<'a> {
    pub workflows: &'a dyn WorkflowRegistry,
    pub stations: &'a dyn StationRegistry,
    pub pools: &'a dyn PoolRegistry,
    pub cadence: &'a dyn CadenceRegistry,
}

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("reading the {registry} registry: {error}")]
    Read {
        registry: &'static str,
        error: String,
    },
    #[error("encoding a {registry} row: {error}")]
    Encode {
        registry: &'static str,
        error: String,
    },
    #[error(
        "export format {found} is not format {FORMAT}, the one this build reads — \
         read it with the build that wrote it"
    )]
    Format { found: u32 },
    #[error(
        "the export states digest {stated} but its rows digest to {computed}: \
         it was edited or truncated after it was generated, so nothing may trust it"
    )]
    Digest { stated: String, computed: String },
    #[error("the export holds {registry} {name} v{version} twice")]
    Duplicate {
        registry: &'static str,
        name: String,
        version: i32,
    },
}

/// Read every lineage of the four registries into one export.
pub async fn export(
    registries: &Registries<'_>,
    source: &str,
    exported_at: DateTime<Utc>,
) -> Result<RegistryExport, ExportError> {
    let workflows = lineages(
        "workflows",
        registries.workflows,
        registries.workflows.lineage_names().await,
    )
    .await?;
    let stations = lineages(
        "stations",
        registries.stations,
        registries.stations.lineage_names().await,
    )
    .await?;
    let pools = lineages(
        "pools",
        registries.pools,
        registries.pools.lineage_names().await,
    )
    .await?;
    let cadence_rules = lineages(
        "cadence_rules",
        registries.cadence,
        registries.cadence.lineage_names().await,
    )
    .await?;
    let mut out = RegistryExport {
        format: FORMAT,
        source: source.to_string(),
        exported_at,
        digest: String::new(),
        workflows,
        stations,
        pools,
        cadence_rules,
    };
    out.digest = out.computed_digest()?;
    Ok(out)
}

/// Every version of every named lineage, names byte-ordered and each
/// lineage oldest first — so one registry exports to one byte sequence
/// on every adapter.
async fn lineages<R>(
    registry: &'static str,
    port: &R,
    names: Result<Vec<String>, impl std::fmt::Display>,
) -> Result<Vec<R::Spec>, ExportError>
where
    R: BundleRegistry + ?Sized,
{
    let read = |error: String| ExportError::Read { registry, error };
    let mut names = names.map_err(|e| read(e.to_string()))?;
    names.sort();
    names.dedup();
    let mut rows = Vec::new();
    for name in names {
        let mut lineage = port
            .live_versions(&name)
            .await
            .map_err(|e| read(format!("{name}: {e}")))?;
        lineage.sort_by_key(|s| s.version());
        rows.extend(lineage);
    }
    Ok(rows)
}

/// A row as a declaration — every column but `created_at` — refusing,
/// rather than defaulting, a row that will not serialize: a row
/// dropped from a digest is a row the digest does not vouch for.
fn declaration<S: Declared>(
    registry: &'static str,
    spec: &S,
) -> Result<serde_json::Value, ExportError> {
    let mut value = serde_json::to_value(spec).map_err(|e| ExportError::Encode {
        registry,
        error: format!("{} v{}: {e}", spec.name(), spec.version()),
    })?;
    if let Some(map) = value.as_object_mut() {
        map.remove("created_at");
    }
    Ok(value)
}

fn declarations<S: Declared>(
    registry: &'static str,
    rows: &[S],
) -> Result<serde_json::Value, ExportError> {
    rows.iter()
        .map(|r| declaration(registry, r))
        .collect::<Result<Vec<_>, _>>()
        .map(serde_json::Value::Array)
}

/// `(name, version)` → declaration, refusing a key held twice.
fn keyed<S: Declared>(
    registry: &'static str,
    rows: &[S],
) -> Result<BTreeMap<(String, i32), S>, ExportError> {
    let mut out = BTreeMap::new();
    for row in rows {
        let key = (row.name().to_string(), row.version());
        if out.insert(key.clone(), row.clone()).is_some() {
            return Err(ExportError::Duplicate {
                registry,
                name: key.0,
                version: key.1,
            });
        }
    }
    Ok(out)
}

impl RegistryExport {
    /// The digest these rows earn — what [`export`] stamps and
    /// [`Self::checked`] holds a read file to.
    pub fn computed_digest(&self) -> Result<String, ExportError> {
        use sha2::{Digest, Sha256};
        let body = serde_json::json!({
            "format": self.format,
            "workflows": declarations("workflows", &self.workflows)?,
            "stations": declarations("stations", &self.stations)?,
            "pools": declarations("pools", &self.pools)?,
            "cadence_rules": declarations("cadence_rules", &self.cadence_rules)?,
        });
        let bytes = boss_core::job::canonical_json_bytes(&body);
        Ok(format!("sha256:{}", hex::encode(Sha256::digest(&bytes))))
    }

    /// This export, held to what it says of itself: a format this build
    /// reads, a digest its rows earn, and no row twice. Every reader of
    /// a file goes through here before it trusts a row.
    pub fn checked(self) -> Result<Self, ExportError> {
        if self.format != FORMAT {
            return Err(ExportError::Format { found: self.format });
        }
        let computed = self.computed_digest()?;
        if computed != self.digest {
            return Err(ExportError::Digest {
                stated: self.digest,
                computed,
            });
        }
        keyed("workflows", &self.workflows)?;
        keyed("stations", &self.stations)?;
        keyed("pools", &self.pools)?;
        keyed("cadence_rules", &self.cadence_rules)?;
        Ok(self)
    }
}

/// One row on which an export and a registry disagree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Difference {
    pub registry: &'static str,
    pub name: String,
    pub version: i32,
    pub kind: DifferenceKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DifferenceKind {
    /// The export holds the row; the registry does not.
    OnlyInExport,
    /// The registry holds the row; the export does not.
    OnlyLive,
    /// Both hold `(name, version)` and disagree on these columns.
    Differs { fields: Vec<String> },
}

impl std::fmt::Display for Difference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (registry, name, version) = (self.registry, &self.name, self.version);
        match &self.kind {
            DifferenceKind::OnlyInExport => {
                write!(f, "{registry} {name} v{version}: in the export, not live")
            }
            DifferenceKind::OnlyLive => {
                write!(f, "{registry} {name} v{version}: live, not in the export")
            }
            DifferenceKind::Differs { fields } => write!(
                f,
                "{registry} {name} v{version}: differs on {}",
                fields.join(", ")
            ),
        }
    }
}

/// Every row on which `export` and `live` disagree, by registry, then
/// name, then version. Empty means the export is a faithful copy of
/// `live`'s rows (`created_at` aside). `live` is itself an export —
/// read now through [`export`] — so one comparison serves verifying a
/// fresh export and proving a restore.
pub fn diff(
    export: &RegistryExport,
    live: &RegistryExport,
) -> Result<Vec<Difference>, ExportError> {
    let mut out = Vec::new();
    out.extend(diff_rows("workflows", &export.workflows, &live.workflows)?);
    out.extend(diff_rows("stations", &export.stations, &live.stations)?);
    out.extend(diff_rows("pools", &export.pools, &live.pools)?);
    out.extend(diff_rows(
        "cadence_rules",
        &export.cadence_rules,
        &live.cadence_rules,
    )?);
    Ok(out)
}

fn diff_rows<S: Declared>(
    registry: &'static str,
    export: &[S],
    live: &[S],
) -> Result<Vec<Difference>, ExportError> {
    let (export, live) = (keyed(registry, export)?, keyed(registry, live)?);
    let keys: std::collections::BTreeSet<&(String, i32)> =
        export.keys().chain(live.keys()).collect();
    Ok(keys
        .into_iter()
        .filter_map(|key| {
            let kind = match (export.get(key), live.get(key)) {
                (Some(_), None) => DifferenceKind::OnlyInExport,
                (None, Some(_)) => DifferenceKind::OnlyLive,
                (Some(a), Some(b)) => {
                    let fields = differing_fields(a, b);
                    if fields.is_empty() {
                        return None;
                    }
                    DifferenceKind::Differs { fields }
                }
                (None, None) => return None,
            };
            Some(Difference {
                registry,
                name: key.0.clone(),
                version: key.1,
                kind,
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cadence::{CadenceRuleRow, InMemoryCadence};
    use crate::pools::InMemoryPools;
    use crate::registry::{InMemoryWorkflows, StepSpec, Terminal, WorkflowStatus};
    use crate::station_queue::StationPredicate;
    use crate::stations::{InMemoryStations, StationKind};
    use boss_core::actor::ActorId;

    fn actor() -> ActorId {
        ActorId::Automation("platform-workflow-seed".into())
    }

    fn now() -> DateTime<Utc> {
        DateTime::<Utc>::UNIX_EPOCH + chrono::Duration::days(20_000)
    }

    fn later() -> DateTime<Utc> {
        now() + chrono::Duration::days(3)
    }

    fn protocol(kind: &str, label: &str) -> WorkflowSpec {
        WorkflowSpec::platform_seed(
            kind,
            label.to_string(),
            "platform",
            vec!["asset".into()],
            vec![
                StepSpec {
                    title: "start".into(),
                    kind: "task".into(),
                    ready_when: "true".into(),
                    ..Default::default()
                },
                StepSpec {
                    title: "finish".into(),
                    kind: "task".into(),
                    ready_when: "steps.start.done".into(),
                    terminal: Some(Terminal {
                        outcome: "done".into(),
                    }),
                    ..Default::default()
                },
            ],
        )
    }

    fn station(name: &str, version: i32) -> StationSpec {
        let mut s = StationSpec::draft(
            name,
            format!("Station {name}"),
            StationKind::Group,
            StationPredicate {
                kind: Some("ship-a-change".into()),
                ..Default::default()
            },
            now(),
        );
        s.version = version;
        s.status = WorkflowStatus::Active;
        s
    }

    fn pool(name: &str, version: i32) -> PoolSpec {
        serde_json::from_value(serde_json::json!({
            "name": name, "version": version, "status": "active", "unit": "slot",
            "kind": "flow", "capacity": {"state": "unknown"}, "department": null,
            "created_at": "2026-10-31T00:00:00Z"
        }))
        .expect("a pool declaration")
    }

    fn rule(name: &str, version: i32, every: i32) -> CadenceRuleSpec {
        CadenceRuleSpec {
            version,
            status: WorkflowStatus::Active,
            row: CadenceRuleRow {
                name: name.into(),
                verb: "reconcile".into(),
                basis: "wall".into(),
                every_minutes: Some(every),
                at_times: None,
                min_dock_depth: None,
                cooldown_minutes: None,
                cadence: None,
                anchor_date: None,
                business_calendar: None,
            },
            created_at: now(),
        }
    }

    struct Fixture {
        workflows: InMemoryWorkflows,
        stations: InMemoryStations,
        pools: InMemoryPools,
        cadence: InMemoryCadence,
    }

    impl Fixture {
        fn registries(&self) -> Registries<'_> {
            Registries {
                workflows: &self.workflows,
                stations: &self.stations,
                pools: &self.pools,
                cadence: &self.cadence,
            }
        }
    }

    /// Two versions of a protocol, a RETIRED protocol, a station at a
    /// declared v3, two pool versions and a cadence rule — every shape
    /// an export has to carry.
    async fn fixture() -> Fixture {
        let f = Fixture {
            workflows: InMemoryWorkflows::for_fixture(),
            stations: InMemoryStations::new(),
            pools: InMemoryPools::default(),
            cadence: InMemoryCadence::default(),
        };
        let w: &dyn WorkflowRegistry = &f.workflows;
        for spec in [
            protocol("ship-a-change", "Ship a change"),
            protocol("ship-a-change", "Ship a change, v2"),
            protocol("old-kind", "Old kind"),
        ] {
            BundleRegistry::publish_declared(w, spec, &actor(), now())
                .await
                .expect("publish a protocol");
        }
        w.retire("old-kind", &actor(), now())
            .await
            .expect("retire the old kind");
        let s: &dyn StationRegistry = &f.stations;
        s.publish_declared(station("dock", 3), &actor(), now())
            .await
            .expect("publish a station");
        let p: &dyn PoolRegistry = &f.pools;
        for v in [1, 2] {
            p.publish_declared(pool("robots:opus", v), &actor(), now())
                .await
                .expect("publish a pool");
        }
        let c: &dyn CadenceRegistry = &f.cadence;
        c.publish_declared(rule("train-reconcile", 1, 10), &actor(), now())
            .await
            .expect("publish a rule");
        f
    }

    #[tokio::test]
    async fn an_export_holds_every_version_of_every_lineage_including_a_retired_one() {
        let f = fixture().await;
        let out = export(&f.registries(), "fixture", now())
            .await
            .expect("export");
        let kinds: Vec<(String, i32)> = out
            .workflows
            .iter()
            .map(|w| (w.kind.clone(), w.version))
            .collect();
        assert_eq!(
            kinds,
            vec![
                ("old-kind".to_string(), 1),
                ("ship-a-change".to_string(), 1),
                ("ship-a-change".to_string(), 2),
            ],
            "every kind byte-ordered, every version oldest first — the RETIRED kind too, \
             which list_active cannot see"
        );
        assert_eq!(
            out.workflows[0].status,
            WorkflowStatus::Retired,
            "a retired row is exported AS retired, so a restore keeps the decision"
        );
        assert_eq!(out.stations.len(), 1);
        assert_eq!(
            out.stations[0].version, 3,
            "the declared version, not max+1"
        );
        assert_eq!(
            out.pools.iter().map(|p| p.version).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(out.cadence_rules.len(), 1);
        assert_eq!(out.format, FORMAT);
        assert!(out.digest.starts_with("sha256:"), "{}", out.digest);
    }

    #[tokio::test]
    async fn an_unchanged_registry_exports_to_one_digest_whenever_it_is_read() {
        let f = fixture().await;
        let first = export(&f.registries(), "fixture", now())
            .await
            .expect("first");
        let second = export(&f.registries(), "another label", later())
            .await
            .expect("second");
        assert_eq!(
            first.digest, second.digest,
            "the digest names the rows, not when or by whom they were read"
        );
        assert!(diff(&first, &second).expect("diff").is_empty());
    }

    #[tokio::test]
    async fn a_file_round_trips_and_an_edited_one_is_refused_by_both_digests() {
        let f = fixture().await;
        let out = export(&f.registries(), "fixture", now())
            .await
            .expect("export");
        let text = serde_json::to_string_pretty(&out).expect("encode");
        let read: RegistryExport = serde_json::from_str(&text).expect("decode");
        let read = read.checked().expect("an untouched file checks");
        assert_eq!(read, out, "the file is the export, row for row");

        let mut edited = read.clone();
        edited.stations[0].title = "Renamed by hand".into();
        let err = edited.checked().expect_err("a hand edit is refused");
        let text = err.to_string();
        assert!(
            text.contains(&out.digest) && text.contains("edited or truncated"),
            "the refusal names the stated digest and why: {text}"
        );

        let mut truncated = read.clone();
        truncated.pools.pop();
        assert!(matches!(
            truncated.checked(),
            Err(ExportError::Digest { .. })
        ));

        let mut future = read;
        future.format = FORMAT + 1;
        assert!(matches!(future.checked(), Err(ExportError::Format { .. })));
    }

    #[tokio::test]
    async fn the_registry_moving_after_an_export_is_named_row_by_row() {
        let f = fixture().await;
        let before = export(&f.registries(), "fixture", now())
            .await
            .expect("before");
        // Three moves: a new pool version, a station retired, a rule
        // re-versioned.
        let p: &dyn PoolRegistry = &f.pools;
        p.publish_declared(pool("robots:opus", 3), &actor(), later())
            .await
            .expect("publish v3");
        let s: &dyn StationRegistry = &f.stations;
        s.retire("dock", &actor(), later()).await.expect("retire");
        let c: &dyn CadenceRegistry = &f.cadence;
        c.publish_declared(rule("train-reconcile", 2, 5), &actor(), later())
            .await
            .expect("publish v2");
        let after = export(&f.registries(), "fixture", later())
            .await
            .expect("after");
        assert_ne!(before.digest, after.digest);
        let lines: Vec<String> = diff(&before, &after)
            .expect("diff")
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            lines,
            vec![
                "stations dock v3: differs on status".to_string(),
                "pools robots:opus v2: differs on status".to_string(),
                "pools robots:opus v3: live, not in the export".to_string(),
                "cadence_rules train-reconcile v1: differs on status".to_string(),
                "cadence_rules train-reconcile v2: live, not in the export".to_string(),
            ],
            "every disagreement named by registry, name and version"
        );
    }

    #[tokio::test]
    async fn a_registry_that_cannot_name_its_lineages_refuses_the_export() {
        let f = fixture().await;
        let stations: &dyn StationRegistry = &f.stations;
        let err = lineages(
            "stations",
            stations,
            Err::<Vec<String>, _>("station adapter cannot enumerate its lineages"),
        )
        .await
        .expect_err("a short list is never an export");
        assert_eq!(
            err.to_string(),
            "reading the stations registry: station adapter cannot enumerate its lineages"
        );
    }
}
