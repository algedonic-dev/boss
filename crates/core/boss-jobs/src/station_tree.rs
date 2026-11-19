//! The station tree — T2 of design 4b27a65a (backlog 3f58c823; the
//! handoff's §2 primitives and §3 rules, decided by David as one bundle
//! car on 2026-11-06, `bundle_decision_20261106_1944Z`).
//!
//! ONE STATION, DRAWN RECURSIVELY. The company is a station, a
//! department is a station, a gate's own room is a station: each row
//! names the station it stands in (`parent`, by the parent's stable
//! `id`), and a station's ADDRESS — `/<root>/engineering/gates` — is
//! the chain of names from the root down to it. The address is DERIVED
//! here and stored nowhere, so moving a station is one `parent` edit on
//! one row and every address beneath it follows (handoff §3 rule 9, §10).
//! No code names an address; the rows do.
//!
//! THE RULES THE TREE KEEPS, judged by [`validate_station_tree`] at the
//! one door a platform station row is published through (the platform
//! seed, `boss-platform-workflow-seed`) and pinned on the bundle by
//! tests/station_tree.rs:
//!
//! - **one parent, one root** — a row has one `parent` field, so one
//!   parent is the shape; exactly one tree row names none;
//! - **every parent exists** — `parent` names the `id` of an active
//!   tree row;
//! - **no cycles** — following parents from any row reaches the root;
//! - **a child's allocation never exceeds its parent's** — per pool, the
//!   children's allocations summed are at most what the parent holds,
//!   and the root holds every pool whole, so the root's children are
//!   judged against the pool's own capacity. An unknown capacity cannot
//!   be allocated from: unknown is not zero, and it is not plenty either.
//!
//! What a row can be judged on alone (an id's shape, seats of at least
//! one, machines that are automation ids) is the viability lint's
//! ([`crate::station_lint`]), which every publish door runs.
//!
//! ADDITIVE ONLY. A row with no `id` is outside the tree — every row an
//! operator authored before this car, and every row a protocol projects
//! — and nothing here refuses it; the tree is the rows that declare one.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::pools::{PoolCapacity, PoolSpec};
use crate::registry::WorkflowStatus;
use crate::stations::StationSpec;

/// How a box's pool is named: `host:` and the estate node's id (handoff
/// §2: a pool is named by its box, never by a station). The one spelling
/// of the convention — the tree read joins a pool to its node by it, and
/// the pin `every_estate_node_has_its_host_pool_and_no_other` holds the
/// bundle's host pools to infra/estate/estate.toml by it.
pub const HOST_POOL_PREFIX: &str = "host:";

/// The estate node id a `host:<node>` pool is named for, or `None`.
pub fn host_of(pool: &str) -> Option<&str> {
    pool.strip_prefix(HOST_POOL_PREFIX)
        .filter(|n| !n.is_empty())
}

/// The active rows that stand in the tree: every active row declaring an
/// `id`, in the order given.
pub fn tree_rows(stations: &[StationSpec]) -> Vec<&StationSpec> {
    stations
        .iter()
        .filter(|s| s.status == WorkflowStatus::Active && s.id.is_some())
        .collect()
}

/// The tree row each id names. An id two rows declare names the first —
/// [`validate_station_tree`] refuses the pair, and a read must still end.
fn by_id<'a>(rows: &[&'a StationSpec]) -> BTreeMap<&'a str, &'a StationSpec> {
    let mut out = BTreeMap::new();
    for row in rows {
        if let Some(id) = row.id.as_deref() {
            out.entry(id).or_insert(*row);
        }
    }
    out
}

/// The chain from `start` up to the root, `start` first: `Ok` when it
/// reaches a row with no parent, `Err` with the chain walked when a
/// parent is missing (`None`) or a station repeats (`Some(name)`).
/// Bounded by the number of rows, so a cycle cannot hang a read.
#[allow(clippy::type_complexity)]
fn chain<'a>(
    start: &'a StationSpec,
    ids: &BTreeMap<&'a str, &'a StationSpec>,
) -> Result<Vec<&'a StationSpec>, (Vec<&'a StationSpec>, Option<&'a str>)> {
    let mut walked = vec![start];
    let mut at = start;
    while let Some(parent) = at.parent.as_deref() {
        let Some(next) = ids.get(parent).copied() else {
            return Err((walked, None));
        };
        if walked.iter().any(|w| std::ptr::eq(*w, next)) {
            return Err((walked, Some(next.name.as_str())));
        }
        walked.push(next);
        at = next;
    }
    Ok(walked)
}

fn address(chain: &[&StationSpec]) -> String {
    chain.iter().rev().map(|s| format!("/{}", s.name)).collect()
}

/// Every way `stations` fails to make one tree, one refusal each, naming
/// the station (and the pool) it is about. Empty when the tree holds.
/// `pools` are the declared pools an allocation may name; only active
/// ones count, as only active station rows do.
pub fn validate_station_tree(stations: &[StationSpec], pools: &[PoolSpec]) -> Vec<String> {
    let rows = tree_rows(stations);
    let ids = by_id(&rows);
    let mut out = Vec::new();

    // An id names ONE station.
    let mut seen: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for row in &rows {
        if let Some(id) = row.id.as_deref() {
            seen.entry(id).or_default().push(row.name.as_str());
        }
    }
    for (id, names) in seen.iter().filter(|(_, names)| names.len() > 1) {
        out.push(format!(
            "station id `{id}` is declared by {}; an id names one station",
            names
                .iter()
                .map(|n| format!("`{n}`"))
                .collect::<Vec<_>>()
                .join(" and ")
        ));
    }

    // One root.
    let roots: Vec<&str> = rows
        .iter()
        .filter(|r| r.parent.is_none())
        .map(|r| r.name.as_str())
        .collect();
    match roots.len() {
        0 if !rows.is_empty() => out.push(
            "the station tree has no root: every tree row names a parent, so none is the company"
                .to_string(),
        ),
        0 | 1 => {}
        _ => out.push(format!(
            "the station tree has {} roots — {} — and holds one",
            roots.len(),
            roots
                .iter()
                .map(|n| format!("`{n}`"))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }

    // Every parent exists; no cycles. A cycle is reported once, by the
    // set of stations on it, however many rows lead into it.
    let mut cycles: BTreeSet<Vec<String>> = BTreeSet::new();
    for row in &rows {
        if let Some(parent) = row.parent.as_deref()
            && !ids.contains_key(parent)
        {
            out.push(format!(
                "station `{}` names parent `{parent}`, which no active tree row declares as its id",
                row.name
            ));
            continue;
        }
        if let Err((walked, Some(repeated))) = chain(row, &ids) {
            let start = walked
                .iter()
                .position(|s| s.name == repeated)
                .unwrap_or_default();
            let mut members: Vec<String> = walked[start..].iter().map(|s| s.name.clone()).collect();
            members.sort();
            cycles.insert(members);
        }
    }
    out.extend(cycles.into_iter().map(|members| {
        format!(
            "the stations {} stand in a cycle: following their parents never reaches the root",
            members
                .iter()
                .map(|n| format!("`{n}`"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }));

    // Allocations: every pool named is declared, the root holds the pools
    // whole, and per parent and pool the children never exceed the parent.
    let active_pools: BTreeMap<&str, &PoolSpec> = pools
        .iter()
        .filter(|p| p.status == WorkflowStatus::Active)
        .map(|p| (p.name.as_str(), p))
        .collect();
    for row in &rows {
        for pool in row.allocation.keys() {
            if !active_pools.contains_key(pool.as_str()) {
                out.push(format!(
                    "station `{}` holds pool `{pool}`, which no active pool row declares",
                    row.name
                ));
            }
        }
        if row.parent.is_none() && !row.allocation.is_empty() {
            out.push(format!(
                "station `{}` is the root and declares an allocation; the root holds every \
                 pool whole, so what it holds is each pool's capacity",
                row.name
            ));
        }
    }
    for parent in &rows {
        let Some(id) = parent.id.as_deref() else {
            continue;
        };
        let children: Vec<&&StationSpec> = rows
            .iter()
            .filter(|c| c.parent.as_deref() == Some(id))
            .collect();
        let named: BTreeSet<&str> = children
            .iter()
            .flat_map(|c| c.allocation.keys().map(String::as_str))
            .collect();
        for pool in named {
            let Some(declared) = active_pools.get(pool) else {
                continue; // refused above, by the row that names it
            };
            let amounts: Vec<crate::flow_resources::Amount> = match children
                .iter()
                .filter_map(|c| c.allocation.get(pool))
                .map(|q| q.amount())
                .collect::<Result<_, _>>()
            {
                Ok(amounts) => amounts,
                Err(why) => {
                    out.push(format!(
                        "an allocation of `{pool}` under `{}`: {why}",
                        parent.name
                    ));
                    continue;
                }
            };
            let sum = match crate::flow_resources::Amount::sum(amounts.iter()) {
                Ok(sum) => sum,
                Err(why) => {
                    out.push(format!(
                        "the allocations of `{pool}` under `{}`: {why}",
                        parent.name
                    ));
                    continue;
                }
            };
            let holders = children
                .iter()
                .filter(|c| c.allocation.contains_key(pool))
                .map(|c| format!("`{}`", c.name))
                .collect::<Vec<_>>()
                .join(", ");
            let held = if parent.parent.is_none() {
                match &declared.capacity {
                    PoolCapacity::Configured { amount } => (amount.clone(), "capacity"),
                    PoolCapacity::Unknown {} => {
                        out.push(format!(
                            "{holders} hold `{pool}` from the root `{}`, and the pool's capacity \
                             is unknown — unknown is not zero, and it cannot be allocated from",
                            parent.name
                        ));
                        continue;
                    }
                }
            } else {
                match parent.allocation.get(pool) {
                    Some(amount) => (amount.clone(), "allocation"),
                    None => {
                        out.push(format!(
                            "{holders} hold `{pool}` from `{}`, which holds none of it",
                            parent.name
                        ));
                        continue;
                    }
                }
            };
            match held.0.amount() {
                Ok(limit) if sum > limit => out.push(format!(
                    "the stations in `{}` ({holders}) hold {sum} of `{pool}` together, more than \
                     the {} {} it holds",
                    parent.name, held.0, held.1
                )),
                Ok(_) => {}
                Err(why) => out.push(format!("`{}` holds `{pool}`: {why}", parent.name)),
            }
        }
    }
    out
}

/// The station's address — `/` and the names from the root down to it —
/// derived from the tree as `stations` hold it. `None` for a row outside
/// the tree, or one whose parents do not reach a root.
pub fn address_of(stations: &[StationSpec], name: &str) -> Option<String> {
    let rows = tree_rows(stations);
    let ids = by_id(&rows);
    let row = rows.iter().find(|r| r.name == name)?;
    chain(row, &ids).ok().map(|c| address(&c))
}

/// One station of a subtree as the tree read serves it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TreeNode {
    pub station: StationSpec,
    /// Derived, never stored.
    pub address: String,
    /// Steps from the root: the root is 0.
    pub depth: usize,
    /// The names of the stations standing directly in this one.
    pub children: Vec<String>,
}

/// The subtree under the station whose `id` or `name` is `under`, root of
/// the subtree first, then depth-first with siblings in name order. `Err`
/// names why there is none: no tree row is called that.
pub fn subtree(stations: &[StationSpec], under: &str) -> Result<Vec<TreeNode>, String> {
    let rows = tree_rows(stations);
    let ids = by_id(&rows);
    let top = rows
        .iter()
        .find(|r| r.id.as_deref() == Some(under))
        .or_else(|| rows.iter().find(|r| r.name == under))
        .ok_or_else(|| format!("no station of the tree has the id or name `{under}`"))?;
    let top_chain = chain(top, &ids).map_err(|(walked, _)| {
        format!(
            "station `{}` does not reach the root: its parents stop at `{}`",
            top.name,
            walked.last().map(|s| s.name.as_str()).unwrap_or_default()
        )
    })?;
    let children_of = |s: &StationSpec| -> Vec<&StationSpec> {
        let mut kids: Vec<&StationSpec> = rows
            .iter()
            .copied()
            .filter(|c| c.parent.is_some() && c.parent == s.id && !std::ptr::eq(*c, s))
            .collect();
        kids.sort_by(|a, b| a.name.cmp(&b.name));
        kids
    };
    // Depth-first, an explicit stack, and every row visited once: a cycle
    // below `under` cannot loop the read (the seed refuses one anyway).
    let mut out = Vec::new();
    let mut visited: BTreeSet<&str> = BTreeSet::new();
    let mut stack: Vec<(&StationSpec, String, usize)> =
        vec![(*top, address(&top_chain), top_chain.len() - 1)];
    while let Some((station, at, depth)) = stack.pop() {
        if !visited.insert(station.name.as_str()) {
            continue;
        }
        let kids = children_of(station);
        out.push(TreeNode {
            station: station.clone(),
            address: at.clone(),
            depth,
            children: kids.iter().map(|k| k.name.clone()).collect(),
        });
        for kid in kids.into_iter().rev() {
            stack.push((kid, format!("{at}/{}", kid.name), depth + 1));
        }
    }
    Ok(out)
}

// -- a protocol names its owner (T6, backlog baac0b65) -----------------------

/// The workflow-metadata key a protocol names its owner station under:
/// the stable `id` of a standing station, never its name or address
/// (docs/design/station-tree.md §4 and §8 rules 1-3).
pub const OWNER_KEY: &str = "owner";

/// The owner station id a workflow row names, if it names one.
pub fn protocol_owner(workflow: &crate::registry::WorkflowSpec) -> Option<&str> {
    workflow.metadata.get(OWNER_KEY).and_then(|v| v.as_str())
}

/// Every protocol names a standing station of the tree as its owner.
/// One line per row that does not, naming the kind and what it named.
/// An owner id resolves among the ACTIVE tree rows ([`tree_rows`]), the
/// set the parent validation reads: a retired row kept for its history
/// is no owner, and cannot mask an active successor listed after it.
pub fn validate_protocol_owners(
    workflows: &[crate::registry::WorkflowSpec],
    stations: &[StationSpec],
) -> Vec<String> {
    let active = by_id(&tree_rows(stations));
    workflows
        .iter()
        .filter_map(|w| {
            let Some(id) = protocol_owner(w) else {
                return Some(format!(
                    "{}: names no owner station (metadata.owner)",
                    w.kind
                ));
            };
            match active.get(id) {
                None => Some(format!(
                    "{}: owner {id} is no active station of the tree",
                    w.kind
                )),
                Some(s) if s.mode != Some(crate::stations::StationMode::Standing) => Some(format!(
                    "{}: owner {id} ({}) is not a standing station",
                    w.kind, s.name
                )),
                Some(_) => None,
            }
        })
        .collect()
}

/// Every automation names a standing station of the tree as its host
/// (backlog 6ecb5480; station-tree.md section 6). The id resolves among
/// the ACTIVE tree rows, as an owner does: a retired row is no host.
/// One line per row that does not, naming the automation and what it
/// named.
pub fn validate_automation_stations(
    automations: &[crate::agents::AutomationActor],
    stations: &[StationSpec],
) -> Vec<String> {
    let active = by_id(&tree_rows(stations));
    automations
        .iter()
        .filter_map(|a| {
            let Some(id) = a.station.as_deref() else {
                return Some(format!("{}: names no host station (station)", a.id));
            };
            match active.get(id) {
                None => Some(format!(
                    "{}: station {id} is no active station of the tree",
                    a.id
                )),
                Some(s) if s.mode != Some(crate::stations::StationMode::Standing) => Some(format!(
                    "{}: station {id} ({}) is not a standing station",
                    a.id, s.name
                )),
                Some(_) => None,
            }
        })
        .collect()
}

/// The step-metadata key a step stands at a station under: the stable
/// `id` of a station inside its protocol's owner's subtree (T6, backlog
/// baac0b65; station-tree.md §4 and §8 rule 3). Placement only: who may
/// claim the step is still its `audience` and `executor`.
pub const AT_KEY: &str = "at";

/// The step-metadata key a hand-off names the protocol that fulfils the
/// step under: a workflow kind, run under that kind's own owner
/// (station-tree.md §1 and §4). A step names a station or a hand-off,
/// never both.
pub const FULFILLED_BY_KEY: &str = "fulfilled_by";

/// A protocol names only stations in its owner's subtree, and every
/// hand-off names a protocol that exists. One line per step that does
/// not, naming the kind, the step and what it named.
pub fn validate_protocol_subtree(
    workflows: &[crate::registry::WorkflowSpec],
    stations: &[StationSpec],
) -> Vec<String> {
    // The ACTIVE tree rows only, the set an owner resolves in: a retired
    // row is no place to stand and masks no active successor.
    let rows = tree_rows(stations);
    let by_id = by_id(&rows);
    // Whether `id` is `owner` or stands under it: the parent chain from
    // `id` reaches `owner`. Bounded by the row count, so a cycle (which
    // validate_station_tree names) ends rather than loops.
    let within = |id: &str, owner: &str| {
        let mut at = Some(id);
        for _ in 0..=rows.len() {
            match at {
                Some(here) if here == owner => return true,
                Some(here) => at = by_id.get(here).and_then(|s| s.parent.as_deref()),
                None => return false,
            }
        }
        false
    };
    let kinds: BTreeSet<&str> = workflows.iter().map(|w| w.kind.as_str()).collect();
    let mut failures = Vec::new();
    for w in workflows {
        let owner = protocol_owner(w);
        for step in &w.steps {
            let here = format!("{} step `{}`", w.kind, step.title);
            let declared = |key: &str| step.metadata_defaults.get(key);
            // Both keys PRESENT, whatever their shape, is a step that
            // names both: the shape is the lesser error.
            if declared(AT_KEY).is_some() && declared(FULFILLED_BY_KEY).is_some() {
                failures.push(format!(
                    "{here}: names both a station ({AT_KEY}) and a hand-off ({FULFILLED_BY_KEY}); a step is one or the other"
                ));
                continue;
            }
            // A PRESENT key that is no non-blank string is named, never
            // read as absent: absence would drop the declaration and pass
            // the check on it (Codex review of T6 car B, 2026-11-08).
            let mut named = |key: &str, what: &str| match declared(key) {
                None => None,
                Some(v) => match v.as_str().filter(|s| !s.trim().is_empty()) {
                    Some(s) => Some(s),
                    None => {
                        failures.push(format!("{here}: {key} must name {what}, not {v}"));
                        None
                    }
                },
            };
            let at = named(AT_KEY, "a station id");
            let fulfilled_by = named(FULFILLED_BY_KEY, "a protocol kind");
            if let Some(id) = at {
                match (by_id.get(id), owner) {
                    (None, _) => failures.push(format!("{here}: at {id} is no active station of the tree")),
                    (Some(s), Some(owner)) if !within(id, owner) => failures.push(format!(
                        "{here}: at {id} ({}) is outside its owner's subtree ({owner}); move the station or make the step a {FULFILLED_BY_KEY} hand-off",
                        s.name
                    )),
                    _ => {}
                }
            }
            if let Some(kind) = fulfilled_by
                && !kinds.contains(kind)
            {
                failures.push(format!(
                    "{here}: fulfilled_by {kind} is no protocol of the bundle"
                ));
            }
        }
    }
    failures
}

// -- the station a claim stands at (D1 of design 305cf4ce) -------------------

/// **The tree station a claim stands at** — the claim door's resolution
/// (D1 of design 305cf4ce, "Claims stand at stations", backlog 5662d4b6;
/// David 2026-11-12: "Work is supposed to sit at the admission queue of a
/// station now, not at a name or robot or executor"). Measured that day
/// on `GET /api/claims`: 0 of 97 claims at a tree station, 76 at
/// `a.platform-admin.opus-5-5`, 11 at `q.platform-admin.task`.
///
/// In order:
///
/// 1. **The step's [`AT_KEY`]** — the row's, else the default its
///    protocol step declares: the two places
///    [`crate::station_queue::addressed_here`] reads. A declared `at`
///    decides alone: one that names no active tree row is no placement,
///    and the claim records none rather than guess past a declaration.
/// 2. **Else the deepest tree station whose queue holds the packet on
///    this step's account**: its predicate matches the packet with this
///    step, or this step is addressed inside its subtree — the queue's own
///    membership (`http::stations::at_station`), asked of the one step
///    claimed, so a station holding the packet only for a sibling step is
///    not where this claim stands. A depth tie goes to the first station
///    by name.
///
/// Never a generated name
/// ([`crate::station_projection::in_derived_namespace`]): the projected
/// rows stand outside the tree, and a `q.`/`a.` row authored INTO it (as
/// `q.platform-admin.task` is, under executive) is skipped. Never a
/// per-actor row (`@me`, as `my-watchlist` is): where a claim stands is a
/// pure function of the tree (D7), not of who asks, so such a row, bound
/// to nobody, holds nothing here.
///
/// `protocol` is the version the packet is pinned to; without it only a
/// row's own `at` can answer, since no address can be judged. `step` is
/// the step claimed, resolved as the queue reads it (`agent_spec`). The
/// row, not its name, is answered, so a reader that records the id (car 2
/// of the design, `_station_id`) takes it from here.
pub fn claim_station<'a>(
    stations: &'a [StationSpec],
    protocol: Option<&crate::registry::WorkflowSpec>,
    job: &boss_core::job::Job,
    step: &boss_core::job::Step,
) -> Option<&'a StationSpec> {
    resolve_claim_station(stations, protocol, job, step).map(|resolved| resolved.station)
}

/// The row and the reason the claim stands there. A legacy fallback is
/// explicit evidence, so migration readers can count it without guessing
/// from a station name that can change.
pub struct ClaimStationResolution<'a> {
    pub station: &'a StationSpec,
    pub method: &'static str,
    pub reason: Option<&'static str>,
}

pub fn resolve_claim_station<'a>(
    stations: &'a [StationSpec],
    protocol: Option<&crate::registry::WorkflowSpec>,
    job: &boss_core::job::Job,
    step: &boss_core::job::Step,
) -> Option<ClaimStationResolution<'a>> {
    let all = tree_rows(stations);
    let ids = by_id(&all);
    let places = all
        .iter()
        .copied()
        .filter(|s| !crate::station_projection::in_derived_namespace(&s.name));
    let declared = step.metadata.get(AT_KEY).or_else(|| {
        let slug = step.spec_slug.as_deref()?;
        let spec = protocol?.steps.iter().find(|s| s.title == slug)?;
        spec.metadata_defaults.get(AT_KEY)
    });
    if let Some(at) = declared {
        let id = at.as_str()?;
        return places
            .into_iter()
            .find(|s| s.id.as_deref() == Some(id))
            .map(|station| ClaimStationResolution {
                station,
                method: "declared-at",
                reason: None,
            });
    }
    let protocol = protocol?;
    let this = std::slice::from_ref(step);
    let deepest = places
        .filter_map(|row| {
            let depth = chain(row, &ids).ok()?.len();
            let bound = row.bind_self(None)?;
            let holds = bound.predicate.matches(job, this) || {
                let inside: BTreeSet<String> = subtree(stations, row.id.as_deref()?)
                    .ok()?
                    .into_iter()
                    .flat_map(|n| n.station.id.into_iter().chain([n.station.name]))
                    .collect();
                crate::station_queue::addressed_here(&inside, protocol, this)
            };
            holds.then_some((depth, row))
        })
        .min_by(|(da, a), (db, b)| db.cmp(da).then_with(|| a.name.cmp(&b.name)))
        .map(|(_, row)| row);
    if let Some(station) = deepest {
        return Some(ClaimStationResolution {
            station,
            method: "deepest-queue",
            reason: None,
        });
    }
    // A broken explicit owner is not an unaddressed legacy packet.
    if protocol.metadata.get(OWNER_KEY).is_some() {
        return None;
    }
    let mut roots = all.iter().copied().filter(|row| {
        row.parent.is_none() && !crate::station_projection::in_derived_namespace(&row.name)
    });
    let root = roots.next()?;
    if roots.next().is_some() {
        return None;
    }
    Some(ClaimStationResolution {
        station: root,
        method: "root-fallback",
        reason: Some("unaddressed legacy packet"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pools::PoolKind;
    use crate::resources::Quantity;
    use crate::station_queue::StationPredicate;
    use crate::stations::{StationKind, StationMode};

    fn row(name: &str, id: &str, parent: Option<&str>) -> StationSpec {
        let mut s = StationSpec::draft(
            name,
            name,
            StationKind::Group,
            StationPredicate {
                tags_any: vec![format!("station:{name}")],
                ..Default::default()
            },
            chrono::DateTime::<chrono::Utc>::UNIX_EPOCH,
        );
        s.status = WorkflowStatus::Active;
        s.id = Some(id.into());
        s.parent = parent.map(str::to_string);
        s.mode = Some(StationMode::Standing);
        s
    }

    fn holding(mut s: StationSpec, pool: &str, amount: u64) -> StationSpec {
        s.allocation.insert(pool.into(), Quantity::from(amount));
        s
    }

    fn pool(name: &str, capacity: Option<u64>) -> PoolSpec {
        PoolSpec {
            name: name.into(),
            version: 1,
            status: WorkflowStatus::Active,
            unit: "vCPU".into(),
            kind: PoolKind::Flow,
            shape: None,
            capacity: match capacity {
                Some(n) => PoolCapacity::Configured {
                    amount: Quantity::from(n),
                },
                None => PoolCapacity::Unknown {},
            },
            roster: None,
            reclaimer: None,
            reclaimer_kind: None,
            program: None,
            return_verifier: None,
            identity_pin_unenforced: None,
            department: None,
            models: Vec::new(),
            capacity_from: None,
            station: None,
            keeper: None,
            created_at: chrono::DateTime::<chrono::Utc>::UNIX_EPOCH,
        }
    }

    /// The company, two departments and a room in one of them.
    fn company() -> Vec<StationSpec> {
        vec![
            row("acme", "id-root", None),
            holding(row("making", "id-making", Some("id-root")), "host:a", 24),
            row("running", "id-running", Some("id-root")),
            holding(row("bench", "id-bench", Some("id-making")), "host:a", 8),
        ]
    }

    fn pools() -> Vec<PoolSpec> {
        vec![pool("host:a", Some(32)), pool("people:boss", None)]
    }

    #[test]
    fn a_tree_with_one_root_and_every_parent_declared_holds() {
        assert_eq!(
            validate_station_tree(&company(), &pools()),
            Vec::<String>::new()
        );
        assert_eq!(tree_rows(&company()).len(), 4);
    }

    #[test]
    fn the_address_is_the_chain_of_names_from_the_root() {
        let rows = company();
        assert_eq!(address_of(&rows, "acme").as_deref(), Some("/acme"));
        assert_eq!(
            address_of(&rows, "bench").as_deref(),
            Some("/acme/making/bench")
        );
        assert_eq!(address_of(&rows, "nowhere"), None);
    }

    /// Handoff §10: re-parenting is ONE field, and the address beneath
    /// follows — nothing stored had to be rewritten.
    #[test]
    fn re_parenting_is_one_field_and_the_address_follows() {
        let mut rows = company();
        rows[3].parent = Some("id-running".into());
        rows[3].allocation.clear();
        assert_eq!(validate_station_tree(&rows, &pools()), Vec::<String>::new());
        assert_eq!(
            address_of(&rows, "bench").as_deref(),
            Some("/acme/running/bench")
        );
    }

    #[test]
    fn a_second_root_is_refused_naming_both() {
        let mut rows = company();
        rows[2].parent = None;
        let refused = validate_station_tree(&rows, &pools());
        assert_eq!(refused.len(), 1, "{refused:?}");
        assert!(
            refused[0].contains("`acme`") && refused[0].contains("`running`"),
            "{}",
            refused[0]
        );
    }

    #[test]
    fn a_tree_with_no_root_is_refused() {
        let mut rows = company();
        rows[0].parent = Some("id-bench".into());
        let refused = validate_station_tree(&rows, &pools());
        assert!(refused.iter().any(|r| r.contains("no root")), "{refused:?}");
    }

    #[test]
    fn a_parent_no_tree_row_declares_is_refused_naming_it() {
        let mut rows = company();
        rows[2].parent = Some("id-gone".into());
        let refused = validate_station_tree(&rows, &pools());
        assert_eq!(refused.len(), 1, "{refused:?}");
        assert!(
            refused[0].contains("`running`") && refused[0].contains("`id-gone`"),
            "{}",
            refused[0]
        );
        // A retired row is not a parent.
        let mut rows = company();
        rows[1].status = WorkflowStatus::Retired;
        let refused = validate_station_tree(&rows, &pools());
        assert!(
            refused
                .iter()
                .any(|r| r.contains("`bench`") && r.contains("`id-making`")),
            "{refused:?}"
        );
    }

    #[test]
    fn a_cycle_is_refused_naming_its_stations() {
        let mut rows = company();
        rows.push(row("loop-a", "id-a", Some("id-b")));
        rows.push(row("loop-b", "id-b", Some("id-a")));
        let refused = validate_station_tree(&rows, &pools());
        assert!(
            refused
                .iter()
                .any(|r| r.contains("cycle") && r.contains("`loop-a`") && r.contains("`loop-b`")),
            "{refused:?}"
        );
        assert_eq!(address_of(&rows, "loop-a"), None, "a cycle has no address");
    }

    #[test]
    fn an_id_two_rows_declare_is_refused() {
        let mut rows = company();
        rows.push(row("twin", "id-bench", Some("id-root")));
        let refused = validate_station_tree(&rows, &pools());
        assert!(
            refused
                .iter()
                .any(|r| r.contains("`id-bench`") && r.contains("`bench`") && r.contains("`twin`")),
            "{refused:?}"
        );
    }

    /// Handoff §3 rule 2 between two stations: the children's allocations,
    /// summed per pool, never exceed the parent's.
    #[test]
    fn children_holding_more_than_their_parent_are_refused_naming_the_pool() {
        let mut rows = company();
        rows.push(holding(
            row("bench-2", "id-bench-2", Some("id-making")),
            "host:a",
            17,
        ));
        let refused = validate_station_tree(&rows, &pools());
        assert_eq!(refused.len(), 1, "{refused:?}");
        let r = &refused[0];
        assert!(
            r.contains("`making`")
                && r.contains("`host:a`")
                && r.contains("25")
                && r.contains("24"),
            "names the parent, the pool, the sum and the holding: {r}"
        );
        // Exactly the parent's holding is not over it.
        let mut rows = company();
        rows.push(holding(
            row("bench-2", "id-bench-2", Some("id-making")),
            "host:a",
            16,
        ));
        assert_eq!(validate_station_tree(&rows, &pools()), Vec::<String>::new());
    }

    #[test]
    fn a_child_holding_a_pool_its_parent_does_not_is_refused() {
        let mut rows = company();
        rows[3] = holding(rows[3].clone(), "people:boss", 1);
        let refused = validate_station_tree(&rows, &pools());
        assert!(
            refused
                .iter()
                .any(|r| r.contains("`making`") && r.contains("`people:boss`")),
            "{refused:?}"
        );
    }

    /// The root holds every pool whole: its children are judged against
    /// the pool's capacity, and an unknown capacity cannot be allocated.
    #[test]
    fn the_roots_children_are_judged_against_the_pool() {
        let mut rows = company();
        rows[1]
            .allocation
            .insert("host:a".into(), Quantity::from(33_u64));
        let refused = validate_station_tree(&rows, &pools());
        assert!(
            refused
                .iter()
                .any(|r| r.contains("`host:a`") && r.contains("33") && r.contains("32")),
            "{refused:?}"
        );
        let mut rows = company();
        rows[2] = holding(rows[2].clone(), "people:boss", 1);
        let refused = validate_station_tree(&rows, &pools());
        assert!(
            refused
                .iter()
                .any(|r| r.contains("`people:boss`") && r.contains("unknown")),
            "{refused:?}"
        );
        let mut rows = company();
        rows[2] = holding(rows[2].clone(), "host:undeclared", 1);
        let refused = validate_station_tree(&rows, &pools());
        assert!(
            refused
                .iter()
                .any(|r| r.contains("`running`") && r.contains("`host:undeclared`")),
            "{refused:?}"
        );
    }

    #[test]
    fn the_root_declares_no_allocation_because_it_holds_the_pools() {
        let mut rows = company();
        rows[0] = holding(rows[0].clone(), "host:a", 32);
        let refused = validate_station_tree(&rows, &pools());
        assert!(
            refused
                .iter()
                .any(|r| r.contains("`acme`") && r.contains("root")),
            "{refused:?}"
        );
    }

    /// Rows with no id stand outside the tree and are not judged by it —
    /// additive only: every row before this car is one.
    #[test]
    fn a_row_outside_the_tree_is_not_judged_by_it() {
        let mut rows = company();
        let mut legacy = row("legacy", "unused", None);
        legacy.id = None;
        legacy.mode = None;
        rows.push(legacy);
        assert_eq!(validate_station_tree(&rows, &pools()), Vec::<String>::new());
        assert_eq!(tree_rows(&rows).len(), 4);
        assert_eq!(address_of(&rows, "legacy"), None);
        assert!(
            validate_station_tree(&[], &[]).is_empty(),
            "no tree is not a broken tree"
        );
    }

    #[test]
    fn a_subtree_is_its_root_then_its_descendants_depth_first() {
        let rows = company();
        let nodes = subtree(&rows, "making").expect("making is a tree row");
        let walked: Vec<(&str, &str, usize)> = nodes
            .iter()
            .map(|n| (n.station.name.as_str(), n.address.as_str(), n.depth))
            .collect();
        assert_eq!(
            walked,
            vec![
                ("making", "/acme/making", 1),
                ("bench", "/acme/making/bench", 2)
            ]
        );
        assert_eq!(nodes[0].children, vec!["bench".to_string()]);
        let whole = subtree(&rows, "id-root").expect("an id names a row too");
        let names: Vec<&str> = whole.iter().map(|n| n.station.name.as_str()).collect();
        assert_eq!(names, vec!["acme", "making", "bench", "running"]);
        assert!(subtree(&rows, "nowhere").unwrap_err().contains("nowhere"));
    }

    // -- the station a claim stands at (D1 of design 305cf4ce) -------------

    const ROOT: &str = "id-company";
    const ENGINEERING: &str = "id-engineering";
    const BUILD: &str = "id-build";
    const REVIEW: &str = "id-review";

    /// The live shape in miniature: a company, its engineering station
    /// owning the protocol, a `build` station holding by predicate, a
    /// `review` station steps address by `at` — and, under `build` so
    /// each would be the DEEPEST holder, the two rows a claim never
    /// stands at: a generated-namespace row authored into the tree (as
    /// `q.platform-admin.task` is, under executive) and a per-actor lens
    /// (as `my-watchlist` is).
    fn claim_tree() -> Vec<StationSpec> {
        let mut build = row("build", BUILD, Some(ENGINEERING));
        build.predicate = StationPredicate {
            kind: Some("door-kind".into()),
            step: Some(crate::station_queue::StepMatch {
                slug: Some("build".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut generated = row("q.platform-admin.task", "id-q", Some(BUILD));
        generated.predicate = StationPredicate {
            step: Some(crate::station_queue::StepMatch {
                kind: Some("task".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut lens = row("my-watchlist", "id-mine", Some(BUILD));
        lens.predicate = StationPredicate {
            metadata_equals: BTreeMap::from([(
                "submitted_by".to_string(),
                crate::station_queue::SELF.to_string(),
            )]),
            ..Default::default()
        };
        vec![
            row("company", ROOT, None),
            row("engineering", ENGINEERING, Some(ROOT)),
            build,
            row("review", REVIEW, Some(ENGINEERING)),
            generated,
            lens,
        ]
    }

    /// `door-kind`, owned by engineering: `file` stands `at` review by
    /// its protocol default, `build` and `note` declare no placement.
    fn door_protocol() -> crate::registry::WorkflowSpec {
        let step = |title: &str, defaults: serde_json::Value| crate::registry::StepSpec {
            title: title.into(),
            kind: "task".into(),
            ready_when: "true".into(),
            title_template: title.into(),
            metadata_defaults: defaults,
            ..Default::default()
        };
        let mut wf = crate::registry::WorkflowSpec::platform_seed(
            "door-kind",
            "Door kind",
            "test",
            vec!["custom".into()],
            vec![
                step("build", serde_json::json!({})),
                step("file", serde_json::json!({ AT_KEY: REVIEW })),
                step("note", serde_json::json!({})),
            ],
        );
        wf.metadata = serde_json::json!({ OWNER_KEY: ENGINEERING });
        wf
    }

    fn door_packet() -> (boss_core::job::Job, Vec<boss_core::job::Step>) {
        let mut job = boss_core::job::Job::new(
            "door-kind",
            boss_core::job::Subject::new("custom", "door"),
            "A packet at the door",
            "agent-builder",
            boss_core::job::Priority::Standard,
            chrono::NaiveDate::from_ymd_opt(2026, 11, 13).expect("a date"),
        );
        job.status = boss_core::job::JobStatus::Open;
        job.metadata = serde_json::json!({ "submitted_by": "agent-builder" });
        let steps = ["build", "file", "note"]
            .into_iter()
            .map(|slug| {
                let mut s = boss_core::job::Step::new(job.id, "task", slug, 0);
                s.spec_slug = Some(slug.into());
                s.status = boss_core::job::StepStatus::Ready;
                s
            })
            .collect();
        (job, steps)
    }

    fn stands_at(
        tree: &[StationSpec],
        protocol: Option<&crate::registry::WorkflowSpec>,
        step: &boss_core::job::Step,
    ) -> Option<String> {
        let (job, _) = door_packet();
        claim_station(tree, protocol, &job, step).map(|s| s.name.clone())
    }

    #[test]
    fn unaddressed_legacy_claims_resolve_to_the_declared_company_root() {
        let tree = company();
        let mut protocol = door_protocol();
        protocol.metadata = serde_json::json!({});
        let (_, steps) = door_packet();
        assert_eq!(
            stands_at(&tree, Some(&protocol), &steps[0]).as_deref(),
            Some("acme")
        );
        let mut invalid = steps[0].clone();
        invalid.metadata = serde_json::json!({ AT_KEY: "undeclared" });
        assert_eq!(stands_at(&tree, Some(&protocol), &invalid), None);
        let mut ambiguous = tree.clone();
        ambiguous.push(row("other-company", "other-root", None));
        assert_eq!(stands_at(&ambiguous, Some(&protocol), &steps[0]), None);
        let (job, _) = door_packet();
        let resolution = resolve_claim_station(&tree, Some(&protocol), &job, &steps[0])
            .expect("the declared active root");
        assert_eq!(resolution.station.id.as_deref(), Some("id-root"));
        assert_eq!(resolution.method, "root-fallback");
        assert_eq!(resolution.reason, Some("unaddressed legacy packet"));
    }

    /// D1's order: the step's `at` first — on the row, else its protocol
    /// step's default — and only then the deepest tree station whose
    /// queue holds the packet on that step's account.
    #[test]
    fn a_claim_stands_at_its_steps_at_and_else_at_the_deepest_station_holding_it() {
        let (tree, wf) = (claim_tree(), door_protocol());
        let (_, steps) = door_packet();
        let [build, file, note] = [&steps[0], &steps[1], &steps[2]];
        // The protocol default's `at`, though `build`'s predicate holds
        // the packet deeper elsewhere would not apply to this step.
        assert_eq!(stands_at(&tree, Some(&wf), file).as_deref(), Some("review"));
        // The row's own `at` wins over the default and over a deeper
        // predicate holder (`build` holds the `build` step).
        let mut moved = build.clone();
        moved.metadata = serde_json::json!({ AT_KEY: ENGINEERING });
        assert_eq!(
            stands_at(&tree, Some(&wf), &moved).as_deref(),
            Some("engineering")
        );
        // No `at`: the deepest holder. `build` holds the build step by
        // its predicate (depth 2) below engineering and the company,
        // which hold it by the protocol's owner.
        assert_eq!(stands_at(&tree, Some(&wf), build).as_deref(), Some("build"));
        // `note` matches no predicate: the owner's address holds it, and
        // engineering is deeper than the company.
        assert_eq!(
            stands_at(&tree, Some(&wf), note).as_deref(),
            Some("engineering")
        );
    }

    /// The answer is never a generated `q.`/`a.` name — not the rows the
    /// projection writes (outside the tree), and not one authored INTO the
    /// tree, which here holds every task step deeper than `build` — and
    /// never a per-actor lens, which, bound to nobody, holds nothing.
    #[test]
    fn a_claim_never_stands_at_a_generated_name_or_a_per_actor_lens() {
        let (tree, wf) = (claim_tree(), door_protocol());
        let (_, steps) = door_packet();
        for step in &steps {
            let at = stands_at(&tree, Some(&wf), step);
            assert!(
                at.as_deref()
                    .is_some_and(|n| !n.starts_with("q.") && !n.starts_with("a.")),
                "{:?} stands at {at:?}",
                step.spec_slug
            );
            assert_ne!(at.as_deref(), Some("my-watchlist"));
        }
        // An `at` naming the generated row is no placement: nothing is
        // recorded rather than a generated name or a guess past it.
        let mut generated = steps[0].clone();
        generated.metadata = serde_json::json!({ AT_KEY: "id-q" });
        assert_eq!(stands_at(&tree, Some(&wf), &generated), None);
        assert!(crate::station_projection::in_derived_namespace(
            &crate::station_projection::station_name(&crate::station_projection::Constraint {
                step_kind: "task".into(),
                role: "platform-admin".into(),
            })
        ));
        assert!(crate::station_projection::in_derived_namespace(
            &crate::station_projection::agent_station_name(
                &crate::station_projection::AgentConstraint {
                    role: "platform-admin".into(),
                    model: "opus-5[1m]".into(),
                }
            )
        ));
        assert!(!crate::station_projection::in_derived_namespace("build"));
    }

    /// A declared `at` decides alone: one that names no ACTIVE tree row,
    /// or is no string, records nothing rather than fall through to a
    /// station the step never named. Without the pinned protocol only a
    /// row's own `at` can answer — an address cannot be judged — and with
    /// no tree there is nothing to stand at.
    #[test]
    fn a_claim_records_nothing_it_cannot_resolve_from_the_tree() {
        let (tree, wf) = (claim_tree(), door_protocol());
        let (_, steps) = door_packet();
        for bad in [
            serde_json::json!("id-retired"),
            serde_json::json!(""),
            serde_json::json!(7),
        ] {
            let mut step = steps[0].clone();
            step.metadata = serde_json::json!({ AT_KEY: bad });
            assert_eq!(stands_at(&tree, Some(&wf), &step), None, "at {bad}");
        }
        let mut retired = tree.clone();
        if let Some(review) = retired.iter_mut().find(|s| s.name == "review") {
            review.status = WorkflowStatus::Retired;
        }
        assert_eq!(stands_at(&retired, Some(&wf), &steps[1]), None);
        assert_eq!(stands_at(&tree, None, &steps[0]), None);
        let mut placed = steps[0].clone();
        placed.metadata = serde_json::json!({ AT_KEY: REVIEW });
        assert_eq!(stands_at(&tree, None, &placed).as_deref(), Some("review"));
        assert_eq!(stands_at(&[], Some(&wf), &steps[0]), None);
    }

    /// Two stations holding the step at one depth: the first by name, so
    /// the record is a function of the tree and not of the order rows
    /// were read in.
    #[test]
    fn a_depth_tie_goes_to_the_first_station_by_name() {
        let wf = door_protocol();
        let (_, steps) = door_packet();
        let mut tree = claim_tree();
        let mut twin = tree
            .iter()
            .find(|s| s.name == "build")
            .cloned()
            .expect("build is in the tree");
        twin.name = "assembly".into();
        twin.id = Some("id-assembly".into());
        tree.push(twin);
        assert_eq!(
            stands_at(&tree, Some(&wf), &steps[0]).as_deref(),
            Some("assembly")
        );
        tree.reverse();
        assert_eq!(
            stands_at(&tree, Some(&wf), &steps[0]).as_deref(),
            Some("assembly")
        );
    }
}
