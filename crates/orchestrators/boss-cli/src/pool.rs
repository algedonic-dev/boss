//! `boss pool publish <name> <file.toml | pools-dir> [--dry-run]` — publish
//! one resource-pool row through the live pools door (backlog f399458f).
//!
//! WHY THIS IS A VERB. Until the door, a pool row reached the registry
//! only through the platform seed on a converge, so a value change —
//! raising `robots:opus` from 4 to 6 (item 89b17d21) — rode a code car, a
//! gate and a train. David, 2026-11-08: "we should be able to make these
//! registry changes via a different protocol than the code changes." This
//! verb is the row-change path a builder reaches for, the way `boss
//! workflow publish` is for a protocol.
//!
//! It mirrors `boss workflow publish`, step for step, where the registry
//! allows: the row is read with the seed's own loader (`load_pools`), so
//! what publishes here is what the seed would land; it REFUSES INTO A
//! MOVED REGISTRY before writing anything — a live lineage already at or
//! past the file's version (a roster observation bumps it, so does
//! another author) is named with the version to bump to; it lints with
//! the door's `_validate`, which is the publish's own check; and it reads
//! the lineage back after, so a publish that answered 200 and landed
//! nothing is a refusal, never a success.
//!
//! There is no draft step, and that is the registry's shape, not a
//! shortcut: `resource_pools` holds `active` and `retired` rows only, and
//! the publish carries its row in the body at its declared version. The
//! stale-draft promotion `boss workflow publish` guards against (a
//! bodiless publish promoting whatever draft sat there, 2026-08-28) has
//! nothing to promote here.

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;

use boss_jobs::pools::PoolSpec;

/// `boss pool publish` against the system of record.
pub async fn publish(
    name: &str,
    path: &std::path::Path,
    dry: bool,
    over: Option<i64>,
) -> Result<()> {
    let wire = crate::steps::Wire::live()?;
    print!("{}", publish_on(&wire, name, path, dry, over).await?);
    Ok(())
}

/// The row named `name` in the file or directory, read by the seed's own
/// loader — a key this build does not know, an invalid shape and a row
/// that is not active are refused there, by name.
pub(crate) fn row_from(name: &str, path: &std::path::Path) -> Result<PoolSpec> {
    let rows = boss_jobs::pools::load_pools(path)
        .with_context(|| format!("reading {}", path.display()))?;
    let names: Vec<String> = rows.iter().map(|p| p.name.clone()).collect();
    rows.into_iter().find(|p| p.name == name).ok_or_else(|| {
        anyhow!(
            "{} declares no pool named `{name}` (it declares: {})",
            path.display(),
            names.join(", ")
        )
    })
}

fn capacity(row: &Value) -> String {
    match row["capacity"]["state"].as_str() {
        Some("configured") => row["capacity"]["amount"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| row["capacity"]["amount"].to_string()),
        Some(state) => state.to_owned(),
        None => "?".to_owned(),
    }
}

/// Every version of the lineage, oldest first, one line each.
pub(crate) fn render_lineage(name: &str, rows: &[Value]) -> String {
    let mut out = format!("{name}\n");
    for row in rows {
        out.push_str(&format!(
            "  v{:<3} {:<8} capacity={}\n",
            // An integer, not the `Value`: a `Value` ignores the width.
            row["version"].as_i64().unwrap_or_default(),
            row["status"].as_str().unwrap_or("?"),
            capacity(row)
        ));
    }
    out
}

/// The newest version a lineage holds; 0 for a name never published.
pub(crate) fn newest(rows: &[Value]) -> i64 {
    rows.iter()
        .filter_map(|r| r["version"].as_i64())
        .max()
        .unwrap_or(0)
}

async fn lineage(wire: &crate::steps::Wire, name: &str) -> Result<Vec<Value>> {
    let body = wire
        .call(
            reqwest::Method::GET,
            &format!("/api/pools/{name}/versions"),
            None,
        )
        .await
        .with_context(|| format!("reading the live lineage of {name}"))?;
    // An unreadable lineage refuses: read as empty it would call any
    // version new, and say a publish that landed nothing had landed.
    crate::train::rows(body)
}

/// The write's path. With `--over`, the signed live version rides the
/// write as `?over=<version>`, so the door judges it inside the publish's
/// own transaction — review R1 of car 4019ba69 (2026-11-08): the verb's
/// lineage read above cannot hold the baseline still, and a v2 landed
/// between that read and this write was replaced by a v3 whose approval
/// was a diff against v1.
pub(crate) fn publish_path(name: &str, over: Option<i64>) -> String {
    match over {
        Some(signed) => format!("/api/pools/{name}/publish?over={signed}"),
        None => format!("/api/pools/{name}/publish"),
    }
}

/// The verb against an explicit wire — the seam its test drives.
pub(crate) async fn publish_on(
    wire: &crate::steps::Wire,
    name: &str,
    path: &std::path::Path,
    dry: bool,
    over: Option<i64>,
) -> Result<String> {
    let spec = row_from(name, path)?;
    let version = i64::from(spec.version);
    let sent = serde_json::to_value(&spec)?;

    // Lint before persisting — the publish path's own check. It goes
    // first because it is a signed write-shaped call: an unnamed runner
    // is refused here, naming BOSS_ACTOR, before any read goes out.
    let verdict = wire
        .call(
            reqwest::Method::POST,
            "/api/pools/_validate",
            Some(sent.clone()),
        )
        .await
        .context("linting the row through POST /api/pools/_validate")?
        .ok_or_else(|| anyhow!("the lint answered no body — refusing to publish on that"))?;
    if verdict["ok"] != Value::Bool(true) {
        bail!(
            "{name} v{version} does not lint clean, so nothing was written:\n  {}",
            verdict["problems"]
        );
    }

    // REFUSE INTO A MOVED REGISTRY, before writing anything.
    let before = lineage(wire, name).await?;
    let live = newest(&before);
    // `--over`: the publish a signed packet asks for is held to the live
    // version its diff was signed against (backlog f399458f ask 2). A
    // lineage that moved since is no approval to publish over, even when
    // the file's version is still above it. This read is the EARLY
    // refusal, with the lineage rendered; the one that holds is the
    // door's, which judges the same `over` inside the write's own
    // transaction (`publish_path` below; review R1 of car 4019ba69).
    if let Some(signed) = over
        && live != signed
    {
        bail!(
            "the live lineage of {name} is at v{live}, and this publish was asked over v{signed}: \
             the diff that was approved is not a diff against what is live now, so nothing was \
             written\n{}",
            render_lineage(name, &before)
        );
    }
    if live >= version {
        bail!(
            "the live lineage of {name} is already at v{live} and {} declares v{version}: a \
             publish lands only above the newest, so set `version = {}` in the file (and \
             check what v{live} changed before overwriting it)\n{}",
            path.display(),
            live + 1,
            render_lineage(name, &before)
        );
    }
    let active = before.iter().find(|r| r["status"] == "active");
    let change = format!(
        "capacity {} -> {}",
        active.map(capacity).unwrap_or_else(|| "(none live)".into()),
        capacity(&sent)
    );
    let mut out = format!("boss pool: {name} v{version} lints clean ({change})\n");
    if dry {
        out.push_str(&format!(
            "boss pool: DRY — would publish {name} v{version} over v{live}\n"
        ));
        return Ok(out);
    }

    let written = wire
        .call(
            reqwest::Method::POST,
            &publish_path(name, over),
            Some(sent.clone()),
        )
        .await
        .with_context(|| format!("publishing {name} v{version}"))?
        .ok_or_else(|| anyhow!("the publish returned no body"))?;
    if written["version"].as_i64() != Some(version) {
        bail!("the publish answered a row that is not v{version}: {written}");
    }

    // The status code said yes; the lineage is the authority.
    let after = lineage(wire, name).await?;
    let landed = after.iter().any(|r| {
        r["version"].as_i64() == Some(version)
            && r["status"] == "active"
            && capacity(r) == capacity(&sent)
    });
    if !landed {
        bail!(
            "the API answered 200 but the lineage does not show {name} v{version} active with \
             the capacity sent — a silent no-op:\n{}",
            render_lineage(name, &after)
        );
    }
    out.push_str(&format!(
        "boss pool: published {name} v{version} — confirmed by reading the lineage back\n{}",
        render_lineage(name, &after)
    ));
    Ok(out)
}

// ---------------------------------------------------------------------------
// `boss pool request` — FILE the publish instead of making it.
// ---------------------------------------------------------------------------
//
// Backlog f399458f ask 2: a pool row change is approved by the platform
// owner from the website, then published by the automaton step behind
// that approval (infra/gcp/publish-pool.sh), with no train. This verb is
// the filing half a builder runs: it reads the row exactly as `publish`
// does (the seed's loader, the door's lint, the moved-lineage refusal —
// `publish_on` in its dry mode IS the dry-run receipt), renders the diff
// against the live row field by field, and files ONE publish-a-protocol
// packet carrying the row's own bytes as `row_toml` and their sha256.
// The passkey signs the sha on `approve`; the publish reads the bytes
// back from the packet and refuses any that do not hash to it.

/// The protocol whose packet this files, and the step it adds for pools.
const REQUEST_KIND: &str = "publish-a-protocol";
const POOL_STEP: &str = "publish-pool";

/// `boss pool request <name> <file>` against the system of record.
pub async fn request(name: &str, path: &std::path::Path) -> Result<()> {
    let wire = crate::steps::Wire::live()?;
    print!("{}", request_on(&wire, name, path).await?);
    Ok(())
}

/// Where the row lives in the tree — the path the write-back car edits
/// and the path the publish verb holds the signature to.
pub(crate) fn tree_path(name: &str) -> String {
    format!("infra/platform/pools/{name}.toml")
}

/// The live row against the file's row, one line per key that differs,
/// `key: live <a> -> row <b>`; `version` is left to the title.
pub(crate) fn row_diff(live: Option<&Value>, sent: &Value) -> String {
    let Some(live) = live else {
        return "no live row: every field is new\n".to_owned();
    };
    let empty = serde_json::Map::new();
    let (l, r) = (
        live.as_object().unwrap_or(&empty),
        sent.as_object().unwrap_or(&empty),
    );
    let mut keys: Vec<&String> = l.keys().chain(r.keys()).collect();
    keys.sort();
    keys.dedup();
    let lines: Vec<String> = keys
        .into_iter()
        // Bookkeeping the door writes, not the author's row.
        .filter(|k| {
            !matches!(
                k.as_str(),
                "version" | "status" | "created_at" | "published_at" | "published_by"
            )
        })
        .filter(|k| r.contains_key(k.as_str()) && l.get(k.as_str()) != r.get(k.as_str()))
        .map(|k| {
            let show = |v: Option<&Value>| v.map_or("(absent)".to_owned(), Value::to_string);
            format!("{k}: live {} -> row {}", show(l.get(k)), show(r.get(k)))
        })
        .collect();
    if lines.is_empty() {
        "no field differs from live but the version\n".to_owned()
    } else {
        lines.join("\n") + "\n"
    }
}

/// The packet `request` files. Pure, so its shape is tested against the
/// tree's publish-a-protocol row without a server.
#[allow(clippy::too_many_arguments)]
pub(crate) fn request_body(
    name: &str,
    version: i64,
    live: i64,
    row_toml: &str,
    sha256: &str,
    tree_sha: &str,
    dry_run: &str,
    diff: &str,
    actor: &str,
) -> Value {
    serde_json::json!({
        "kind": REQUEST_KIND,
        "status": "open",
        "owner_id": actor,
        "priority": "standard",
        "tags": [],
        "subject": {"subject_kind": "custom", "id": name},
        "title": format!("Publish pool {name} v{version} over live v{live}"),
        "metadata": {
            "registry": "pool",
            "row_name": name,
            "row_toml": row_toml,
            "spec_path": tree_path(name),
            "spec_sha256": sha256,
            "live_version": live.to_string(),
            "tree_sha": tree_sha,
            "dry_run": dry_run,
            "diff": diff,
            "filed_by": actor,
        },
    })
}

fn hex_sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The commit the file's checkout stands at, or why there is none.
fn tree_sha_of(path: &std::path::Path) -> String {
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    std::process::Command::new("git")
        .args(["-C", &dir.display().to_string(), "rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "none: the file is not in a git checkout".to_owned())
}

/// The verb against an explicit wire — the seam its test drives.
pub(crate) async fn request_on(
    wire: &crate::steps::Wire,
    name: &str,
    path: &std::path::Path,
) -> Result<String> {
    if !path.is_file() || path.file_name().and_then(|f| f.to_str()) != Some(&format!("{name}.toml"))
    {
        bail!(
            "{} is not {name}.toml: a request names ONE row file, the one {} holds in the tree, \
             so the bytes signed are the bytes the write-back car writes",
            path.display(),
            tree_path(name)
        );
    }
    // The road exists only once the platform owner has published the
    // widened row: a live publish-a-protocol with no publish-pool step
    // would file a packet nothing can publish.
    let live_kind = wire
        .call(
            reqwest::Method::GET,
            &format!("/api/workflows/{REQUEST_KIND}"),
            None,
        )
        .await
        .with_context(|| format!("reading the live {REQUEST_KIND} row"))?
        .ok_or_else(|| anyhow!("GET /api/workflows/{REQUEST_KIND} answered no body"))?;
    let has_road = live_kind["steps"]
        .as_array()
        .is_some_and(|steps| steps.iter().any(|s| s["title"] == POOL_STEP));
    if !has_road {
        bail!(
            "the live {REQUEST_KIND} (v{}) has no `{POOL_STEP}` step, so a pool packet would wait \
             on a door nothing opens: the road goes live when the platform owner publishes the \
             tree's row, from the packet the held-kind filer opens for it. Until then, `boss pool \
             publish {name} {}` is the door",
            live_kind["version"],
            path.display()
        );
    }

    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let row_toml = String::from_utf8(bytes.clone())
        .map_err(|_| anyhow!("{} is not UTF-8 text", path.display()))?;
    let sha = hex_sha256(&bytes);
    // The dry run IS the receipt: the loader, the lint, the moved-lineage
    // refusal, exactly as the publish will run them.
    let dry_run = publish_on(wire, name, path, true, None).await?;
    let spec = row_from(name, path)?;
    let version = i64::from(spec.version);
    let before = lineage(wire, name).await?;
    let live = newest(&before);
    let active = before.iter().find(|r| r["status"] == "active");
    let diff = row_diff(active, &serde_json::to_value(&spec)?);
    let actor = wire.caller_id().ok_or_else(|| {
        anyhow!("no actor to file as: name yourself with BOSS_ACTOR (CLAUDE.md §Doors)")
    })?;
    let body = request_body(
        name,
        version,
        live,
        &row_toml,
        &sha,
        &tree_sha_of(path),
        dry_run.trim_end(),
        &diff,
        actor,
    );
    let filed = wire
        .call(reqwest::Method::POST, "/api/jobs", Some(body))
        .await
        .context("filing the publish-a-protocol packet")?
        .ok_or_else(|| anyhow!("POST /api/jobs answered no body"))?;
    let id = filed["id"]
        .as_str()
        .ok_or_else(|| anyhow!("POST /api/jobs answered no id: {filed}"))?;
    Ok(format!(
        "{dry_run}boss pool: filed {REQUEST_KIND} {id} — pool {name} v{version} over live v{live}, \
         sha256 {} — it waits for the platform owner's passkey at /ux/jobs/{id}\n{diff}",
        &sha[..12]
    ))
}

#[cfg(test)]
mod door_tests {
    //! The verb against the REAL jobs router over the in-memory pool
    //! registry — the handlers production mounts, on an ephemeral port —
    //! driven with a named caller as a value, never the environment.

    use super::*;
    use boss_jobs::pools::{InMemoryPools, PoolRegistry};
    use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
    use std::sync::Arc;

    async fn serve(pools: Arc<InMemoryPools>) -> String {
        let policy: Arc<dyn PolicyClient> = Arc::new(
            FakePolicyClient::builder()
                .allow("platform-admin", Action::Read, Resource::pool(), Scope::All)
                .allow(
                    "platform-admin",
                    Action::Publish,
                    Resource::pool(),
                    Scope::All,
                )
                .build(),
        );
        let bus = boss_testing::RecordingEventBus::new();
        let bus_dyn: Arc<dyn boss_core::port::EventBus> = bus.clone();
        let state = boss_jobs::http::JobsApiState {
            pools: Some(pools as Arc<dyn PoolRegistry>),
            stations: Some(Arc::new(boss_jobs::InMemoryStations::new())),
            ..boss_jobs::http::JobsApiState::minimal(
                Arc::new(boss_jobs::InMemoryJobs::new()),
                bus,
                boss_core::publisher::DomainPublisher::new(bus_dyn, "jobs"),
                policy,
                Arc::new(boss_clock_client::WallClockClient),
            )
        };
        let app = boss_jobs::http::router(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    fn named() -> Option<crate::identity::Caller> {
        Some(crate::identity::Caller {
            id: "emp-david".into(),
            source: crate::identity::Source::Env,
        })
    }

    /// One pool file in the bundle's one-row-per-file shape.
    fn write(file: &std::path::Path, version: i32, amount: u32, extra: &str) {
        std::fs::write(
            file,
            format!(
                "[[pool]]\nname = \"robots:door\"\nversion = {version}\nstatus = \"active\"\n\
                 unit = \"slot\"\nkind = \"flow\"\ncapacity_from = \"the verb's test\"\n\
                 capacity = {{ state = \"configured\", amount = {amount} }}\n{extra}"
            ),
        )
        .unwrap();
    }

    async fn seeded() -> (Arc<InMemoryPools>, crate::steps::Wire) {
        let pools = Arc::new(InMemoryPools::default());
        let spec: PoolSpec = serde_json::from_value(serde_json::json!({
            "name":"robots:door","version":1,"status":"active","unit":"slot","kind":"flow",
            "capacity":{"state":"configured","amount":"4"}
        }))
        .unwrap();
        pools
            .publish_declared(
                spec,
                &boss_core::actor::ActorId::Automation("platform-workflow-seed".into()),
                chrono::Utc::now(),
            )
            .await
            .unwrap();
        let base = serve(pools.clone()).await;
        (pools, crate::steps::Wire::at(base, named()).unwrap())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_bumped_file_publishes_and_the_lineage_is_read_back() {
        let (pools, wire) = seeded().await;
        let dir = boss_testing::scratch_dir("boss-cli-pool-publish");
        let file = dir.join("robots:door.toml");
        write(&file, 2, 6, "");
        let out = publish_on(&wire, "robots:door", &file, false, None)
            .await
            .unwrap();
        assert!(out.contains("capacity 4 -> 6"), "{out}");
        assert!(out.contains("published robots:door v2"), "{out}");
        assert!(out.contains("v1   retired"), "{out}");
        assert!(out.contains("v2   active   capacity=6"), "{out}");
        let live = pools.list_active().await.unwrap();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].version, 2, "the claim door reads v2");
        let events = pools.recorded_events().unwrap();
        assert_eq!(events.len(), 2, "the seed's fact and this publish's");
        assert_eq!(events[1].kind, boss_jobs::pools::PUBLISHED);
        assert_eq!(
            events[1].payload["_actor"], "emp-david",
            "signed as the runner"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_file_the_live_lineage_has_passed_refuses_before_writing() {
        let (pools, wire) = seeded().await;
        let dir = boss_testing::scratch_dir("boss-cli-pool-stale");
        let file = dir.join("robots:door.toml");
        write(&file, 1, 6, "");
        let err = format!(
            "{:#}",
            publish_on(&wire, "robots:door", &file, false, None)
                .await
                .map(|_| ())
                .unwrap_err()
        );
        assert!(err.contains("already at v1"), "{err}");
        assert!(err.contains("version = 2"), "names the bump: {err}");
        assert_eq!(pools.recorded_events().unwrap().len(), 1, "nothing written");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_dry_run_and_a_lint_failure_write_nothing() {
        let (pools, wire) = seeded().await;
        let dir = boss_testing::scratch_dir("boss-cli-pool-dry");
        let file = dir.join("robots:door.toml");
        write(&file, 2, 6, "");
        let out = publish_on(&wire, "robots:door", &file, true, None)
            .await
            .unwrap();
        assert!(out.contains("DRY"), "{out}");
        write(&file, 2, 6, "station = \"no-such-station\"\n");
        let err = format!(
            "{:#}",
            publish_on(&wire, "robots:door", &file, false, None)
                .await
                .map(|_| ())
                .unwrap_err()
        );
        assert!(err.contains("no-such-station"), "{err}");
        assert_eq!(pools.recorded_events().unwrap().len(), 1, "nothing written");
        assert_eq!(pools.list_active().await.unwrap()[0].version, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unnamed_caller_is_refused_before_the_wire() {
        let (pools, _) = seeded().await;
        let base = serve(pools.clone()).await;
        let unnamed = crate::steps::Wire::at(base, None).unwrap();
        let dir = boss_testing::scratch_dir("boss-cli-pool-unnamed");
        let file = dir.join("robots:door.toml");
        write(&file, 2, 6, "");
        let err = format!(
            "{:#}",
            publish_on(&unnamed, "robots:door", &file, false, None)
                .await
                .map(|_| ())
                .unwrap_err()
        );
        assert!(err.contains("BOSS_ACTOR"), "{err}");
        assert_eq!(pools.recorded_events().unwrap().len(), 1, "nothing written");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_without_the_named_row_names_what_it_declares() {
        let dir = boss_testing::scratch_dir("boss-cli-pool-missing");
        let file = dir.join("robots:door.toml");
        write(&file, 2, 6, "");
        let err = format!("{:#}", row_from("robots:other", &file).unwrap_err());
        assert!(
            err.contains("robots:other") && err.contains("robots:door"),
            "{err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------
    // The approval road (backlog f399458f ask 2): `--over` and `request`.
    // -----------------------------------------------------------------

    /// A publish asked over a live version that has moved writes nothing:
    /// the diff that was approved is not a diff against live.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_publish_asked_over_a_moved_lineage_refuses_before_writing() {
        let (pools, wire) = seeded().await;
        let dir = boss_testing::scratch_dir("boss-cli-pool-over");
        let file = dir.join("robots:door.toml");
        write(&file, 3, 6, "");
        let err = format!(
            "{:#}",
            publish_on(&wire, "robots:door", &file, false, Some(2))
                .await
                .map(|_| ())
                .unwrap_err()
        );
        assert!(err.contains("at v1") && err.contains("over v2"), "{err}");
        assert_eq!(pools.recorded_events().unwrap().len(), 1, "nothing written");
        let out = publish_on(&wire, "robots:door", &file, false, Some(1))
            .await
            .unwrap();
        assert!(out.contains("published robots:door v3"), "{out}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Review R1 of car 4019ba69: the read above is advisory; the
    /// baseline that HOLDS is the one the door judges inside its write,
    /// so the write must carry it. `boss-jobs/tests/pools_http.rs`
    /// (`a_signed_baseline_that_moved_between_the_read_and_the_write_is_409_inside_the_write`)
    /// drives the door with it; this pins that the verb sends it, and
    /// sends nothing when no baseline was signed.
    #[test]
    fn the_signed_baseline_rides_the_write_so_the_door_holds_it_in_its_transaction() {
        assert_eq!(
            publish_path("robots:door", Some(1)),
            "/api/pools/robots:door/publish?over=1"
        );
        assert_eq!(
            publish_path("robots:door", None),
            "/api/pools/robots:door/publish"
        );
    }

    /// The tree's publish-a-protocol row, read by the seed's own loader.
    fn tree_row() -> boss_jobs::registry::WorkflowSpec {
        boss_jobs::registry::seedable_platform_workflows()
            .into_iter()
            .find(|w| w.kind == REQUEST_KIND)
            .expect("publish-a-protocol ships in the platform bundle")
    }

    fn string_leaves(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::String(s) => out.push(s.clone()),
            Value::Array(a) => a.iter().for_each(|x| string_leaves(x, out)),
            Value::Object(o) => o.values().for_each(|x| string_leaves(x, out)),
            _ => {}
        }
    }

    /// THE FILING SHAPE, against the tree's row and not a copy of it:
    /// every key its metadata_schema requires is filed, `registry` is one
    /// its enum admits, every filer field the row's steps declare is
    /// present once the steps materialize (the check POST /api/jobs runs
    /// at admission), the steps a pool packet walks render with no
    /// unexpanded token, and the sha the owner signs is the sha of the
    /// bytes the packet carries.
    #[test]
    fn the_request_files_every_fact_the_tree_row_requires() {
        let row = "[[pool]]\nname = \"robots:door\"\nversion = 2\n";
        let body = request_body(
            "robots:door",
            2,
            1,
            row,
            &hex_sha256(row.as_bytes()),
            "0123456789abcdef0123456789abcdef01234567",
            "boss pool: robots:door v2 lints clean (capacity 4 -> 6)",
            "capacity: live {\"amount\":4} -> row {\"amount\":6}\n",
            "emp-david",
        );
        let spec = tree_row();
        assert_eq!(body["kind"], REQUEST_KIND);
        let md = &body["metadata"];
        let required = spec.metadata_schema["required"]
            .as_array()
            .expect("the row's schema names its required keys");
        for key in required {
            let key = key.as_str().expect("a key name");
            assert!(
                md[key].as_str().is_some_and(|v| !v.is_empty()),
                "`{key}` is required by the row and not filed: {md}"
            );
        }
        assert!(
            spec.metadata_schema["properties"]["registry"]["enum"]
                .as_array()
                .is_some_and(|e| e.iter().any(|v| v == &md["registry"])),
            "the registry filed is one the row admits"
        );
        assert_eq!(
            md["spec_sha256"],
            hex_sha256(md["row_toml"].as_str().unwrap().as_bytes())
        );
        assert_eq!(md["spec_path"], "infra/platform/pools/robots:door.toml");

        let subject = boss_core::job::Subject::new("custom", "robots:door");
        let steps = boss_jobs::registry::materialize_steps(
            &spec,
            &subject,
            boss_core::job::JobId::new(),
            md,
            boss_core::job::StepId::new,
        );
        assert!(
            boss_jobs::registry::missing_filer_fields(&steps).is_empty(),
            "admission would refuse: {:?}",
            boss_jobs::registry::missing_filer_fields(&steps)
        );
        let walked = ["filed", "approve", POOL_STEP, "published"];
        for slug in walked {
            let step = steps
                .iter()
                .find(|s| s.spec_slug.as_deref() == Some(slug))
                .unwrap_or_else(|| panic!("the row has a `{slug}` step"));
            let mut leaves = vec![step.title.clone()];
            string_leaves(&step.metadata, &mut leaves);
            for leaf in leaves {
                assert!(
                    !leaf.contains("{metadata."),
                    "`{slug}` renders an unexpanded token for a pool packet: {leaf}"
                );
            }
        }
        let pool_step = steps
            .iter()
            .find(|s| s.spec_slug.as_deref() == Some(POOL_STEP))
            .expect("publish-pool");
        assert_eq!(pool_step.metadata["ops_verb"], "publish-pool");
        assert_eq!(pool_step.metadata["pool"], "robots:door");
        let approve = steps
            .iter()
            .find(|s| s.spec_slug.as_deref() == Some("approve"))
            .expect("approve");
        for key in [
            "registry",
            "row_name",
            "spec_sha256",
            "live_version",
            "diff",
        ] {
            assert_eq!(
                approve.metadata[key], md[key],
                "`{key}` is on the step the passkey signs"
            );
        }
    }

    async fn serve_with_kinds(
        pools: Arc<InMemoryPools>,
        spec: boss_jobs::registry::WorkflowSpec,
    ) -> (String, Arc<boss_jobs::InMemoryJobs>) {
        use boss_jobs::registry::{InMemoryWorkflows, WorkflowRegistry};
        let policy: Arc<dyn PolicyClient> = Arc::new(
            FakePolicyClient::builder()
                .with_default_rules()
                .allow("platform-admin", Action::Read, Resource::pool(), Scope::All)
                .allow(
                    "platform-admin",
                    Action::Publish,
                    Resource::pool(),
                    Scope::All,
                )
                .build(),
        );
        let kinds = Arc::new(InMemoryWorkflows::for_fixture());
        kinds.seed(spec).expect("seed publish-a-protocol");
        let jobs = Arc::new(boss_jobs::InMemoryJobs::new());
        let bus = boss_testing::RecordingEventBus::new();
        let bus_dyn: Arc<dyn boss_core::port::EventBus> = bus.clone();
        let state = boss_jobs::http::JobsApiState {
            pools: Some(pools as Arc<dyn PoolRegistry>),
            stations: Some(Arc::new(boss_jobs::InMemoryStations::new())),
            kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
            ..boss_jobs::http::JobsApiState::minimal(
                jobs.clone(),
                bus,
                boss_core::publisher::DomainPublisher::new(bus_dyn, "jobs"),
                policy,
                Arc::new(boss_clock_client::WallClockClient),
            )
        };
        let app = boss_jobs::http::router(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}"), jobs)
    }

    /// Filed through the REAL router over the tree's row: the packet is
    /// admitted, carries the signed facts on `approve`, and nothing was
    /// published. While the live row has no pool road, nothing is filed.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_request_files_one_packet_and_publishes_nothing() {
        use boss_jobs::JobsRepository;
        let (pools, _) = seeded().await;
        let dir = boss_testing::scratch_dir("boss-cli-pool-request");
        let file = dir.join("robots:door.toml");
        write(&file, 2, 6, "");

        let mut v1 = tree_row();
        v1.steps.retain(|s| s.title != POOL_STEP);
        let (base, jobs) = serve_with_kinds(pools.clone(), v1).await;
        let wire = crate::steps::Wire::at(base, named()).unwrap();
        let err = format!(
            "{:#}",
            request_on(&wire, "robots:door", &file)
                .await
                .map(|_| ())
                .unwrap_err()
        );
        assert!(err.contains("no `publish-pool` step"), "{err}");
        assert!(
            jobs.list_jobs(&boss_jobs::JobFilter::default(), 100, 0)
                .await
                .unwrap()
                .0
                .is_empty(),
            "nothing filed while the road is not live"
        );

        let (base, jobs) = serve_with_kinds(pools.clone(), tree_row()).await;
        let wire = crate::steps::Wire::at(base, named()).unwrap();
        let out = request_on(&wire, "robots:door", &file).await.unwrap();
        assert!(out.contains("filed publish-a-protocol"), "{out}");
        assert!(out.contains("over live v1"), "{out}");
        let (filed, _) = jobs
            .list_jobs(&boss_jobs::JobFilter::default(), 100, 0)
            .await
            .unwrap();
        assert_eq!(filed.len(), 1, "ONE packet");
        let packet = &filed[0];
        assert_eq!(packet.kind, REQUEST_KIND);
        let bytes = std::fs::read(&file).unwrap();
        assert_eq!(packet.metadata["spec_sha256"], hex_sha256(&bytes));
        assert_eq!(
            packet.metadata["row_toml"].as_str().map(str::as_bytes),
            Some(bytes.as_slice()),
            "the packet carries the file's bytes, exactly"
        );
        assert_eq!(packet.metadata["live_version"], "1");
        assert!(
            packet.metadata["diff"]
                .as_str()
                .is_some_and(|d| d.contains("capacity")),
            "{}",
            packet.metadata["diff"]
        );
        assert_eq!(
            pools.list_active().await.unwrap()[0].version,
            1,
            "a request publishes nothing"
        );

        // A file that is not the row's own file is refused by name.
        let other = dir.join("elsewhere.toml");
        write(&other, 2, 6, "");
        let err = format!(
            "{:#}",
            request_on(&wire, "robots:door", &other)
                .await
                .map(|_| ())
                .unwrap_err()
        );
        assert!(err.contains("is not robots:door.toml"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
