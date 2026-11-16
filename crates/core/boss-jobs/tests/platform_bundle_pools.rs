//! tree-wide pin — resource bills name pools declared by the same platform
//! bundle, and a step's executor kind names a model class or an equipment
//! station that bundle declares (factory taxonomy M3, item 0a78f8f8). A
//! role executor is judged by the seed against the live Class registry,
//! which the tree does not hold.
#[test]
fn the_platform_resource_bills_name_declared_pools_and_capacity_state_is_explicit() {
    let pools = boss_jobs::pools::load_pools(boss_jobs::pools::platform_pools_path())
        .expect("complete one-file-per-pool bundle");
    let stations =
        boss_jobs::seed_loader::load_stations(boss_jobs::station_seed::platform_stations_path())
            .expect("complete one-file-per-station bundle");
    let workflows =
        boss_jobs::seed_loader::load_workflows(boss_jobs::registry::platform_bundle_path())
            .expect("workflow bundle");
    let failures = boss_jobs::pools::validate_step_declarations(
        &workflows,
        &boss_jobs::step_executor::Declarations {
            pools: &pools,
            stations: &stations,
            roles: boss_jobs::step_executor::RoleVocabulary::NotReadHere,
        },
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert!(!pools.is_empty());
    for pool in &pools {
        pool.validate()
            .expect("typed declared capacity, never an inferred denominator");
        let wire = serde_json::to_value(&pool.capacity).expect("capacity wire");
        if pool.capacity == (boss_jobs::pools::PoolCapacity::Unknown {}) {
            assert_eq!(wire, serde_json::json!({"state":"unknown"}));
        } else {
            // A Quantity has ONE JSON spelling, the exact decimal STRING
            // (resources.rs; the TOML boundary alone takes an integer).
            // This branch read `is_number()` and ran on no row until the
            // host:* pools (backlog 3f58c823) became the first configured
            // capacities in the bundle.
            assert_eq!(wire["state"], "configured");
            assert!(
                wire["amount"]
                    .as_str()
                    .is_some_and(|a| boss_jobs::resources::Quantity::parse(a).is_ok()),
                "{}: {wire}",
                pool.name
            );
        }
        if let Some(reclaimer) = &pool.reclaimer {
            assert!(
                boss_testing::repo_root().join(reclaimer).is_file(),
                "missing stock reclaimer: {reclaimer}"
            );
        }
    }
}

/// Pass item 3 of design ccfe87ff (backlog 56efd9c2): the pin. Every
/// station a platform pool row names is an active row of the platform
/// station bundle, judged by the same check the seed runs before any
/// publish.
#[test]
fn every_station_a_platform_pool_names_is_a_declared_station() {
    let pools = boss_jobs::pools::load_pools(boss_jobs::pools::platform_pools_path())
        .expect("complete one-file-per-pool bundle");
    let stations =
        boss_jobs::seed_loader::load_stations(boss_jobs::station_seed::platform_stations_path())
            .expect("complete one-file-per-station bundle");
    let failures = boss_jobs::pools::validate_pool_stations(&pools, &stations);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The refusal names the pool and the station it could not find, and a
/// retired station row is not a station a pool may name.
#[test]
fn a_pool_naming_a_station_no_active_row_declares_is_refused_by_name() {
    let stations =
        boss_jobs::seed_loader::load_stations(boss_jobs::station_seed::platform_stations_path())
            .expect("complete one-file-per-station bundle");
    let mut pool = boss_jobs::pools::load_pools(boss_jobs::pools::platform_pools_path())
        .expect("pool bundle")
        .into_iter()
        .next()
        .expect("one pool");
    pool.station = Some("gates".into());
    assert!(
        boss_jobs::pools::validate_pool_stations(std::slice::from_ref(&pool), &stations).is_empty(),
        "gates is a declared station"
    );
    pool.station = Some("no-such-station".into());
    let failures = boss_jobs::pools::validate_pool_stations(std::slice::from_ref(&pool), &stations);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0].contains("no-such-station") && failures[0].contains(&pool.name),
        "{}",
        failures[0]
    );
    let retired: Vec<_> = stations
        .iter()
        .cloned()
        .map(|mut s| {
            s.status = boss_jobs::registry::WorkflowStatus::Retired;
            s
        })
        .collect();
    pool.station = Some("gates".into());
    assert_eq!(
        boss_jobs::pools::validate_pool_stations(std::slice::from_ref(&pool), &retired).len(),
        1,
        "a retired station row does not count"
    );
}

/// Backlog 42cd9a83: a pool's `keeper` is judged by the same check as its
/// `station`, at the seed and at the pools door, so a keeper no active
/// station row declares is refused by name — the pool, the word `keeper`
/// and the station it could not find.
#[test]
fn a_pool_kept_by_a_station_no_active_row_declares_is_refused_by_name() {
    let stations =
        boss_jobs::seed_loader::load_stations(boss_jobs::station_seed::platform_stations_path())
            .expect("complete one-file-per-station bundle");
    let mut pool = boss_jobs::pools::load_pools(boss_jobs::pools::platform_pools_path())
        .expect("pool bundle")
        .into_iter()
        .find(|p| p.name == "host:w-1")
        .expect("host:w-1 is declared");
    pool.keeper = Some("operations".into());
    assert!(
        boss_jobs::pools::validate_pool_stations(std::slice::from_ref(&pool), &stations).is_empty(),
        "operations is a declared station"
    );
    pool.keeper = Some("no-such-keeper".into());
    let failures = boss_jobs::pools::validate_pool_stations(std::slice::from_ref(&pool), &stations);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0].contains("no-such-keeper")
            && failures[0].contains("keeper")
            && failures[0].contains(&pool.name),
        "{}",
        failures[0]
    );
}

/// Backlog 42cd9a83 (station-tree.md section 3: "owning capacity is an
/// allocation; keeping the box is a mandate", and Operations keeps every
/// box): every host pool names `operations` as its keeper, so the station
/// page band reads who keeps the box off the row instead of saying no
/// read names it. A host pool is the box itself, never a station's work,
/// so none names a `station`.
#[test]
fn every_host_pool_is_kept_by_operations() {
    let pools = boss_jobs::pools::load_pools(boss_jobs::pools::platform_pools_path())
        .expect("complete one-file-per-pool bundle");
    let hosts: Vec<_> = pools
        .iter()
        .filter(|p| boss_jobs::station_tree::host_of(&p.name).is_some())
        .collect();
    assert!(!hosts.is_empty(), "the bundle declares host pools");
    let unkept: Vec<(&str, Option<&str>, Option<&str>)> = hosts
        .iter()
        .filter(|p| p.keeper.as_deref() != Some("operations") || p.station.is_some())
        .map(|p| (p.name.as_str(), p.keeper.as_deref(), p.station.as_deref()))
        .collect();
    assert!(
        unkept.is_empty(),
        "host pools not kept by operations (name, keeper, station): {unkept:?}"
    );
}

/// The first real pool-station tie (backlog c9b9bacc, conductor order
/// 20261105_2217Z): the CI workspaces are the stock of the forge's CI
/// runner, so disk:ci-workspaces names `forge-ci` and `forge-ci` is the
/// equipment station that runner operates. Measured before this car on
/// GET /api/claims (as_of 4118189, 2026-11-05): 93 open claims, no pool
/// row naming a station, so every station card read "Not yet bound".
///
/// ONE BAY, because the forge has one runner slot: .forgejo/workflows/
/// ci.yml says the jobs "run serially on one runner slot" (measured off
/// the forge's task list, backlog 47fc2bc1) and again "the forge's one
/// runner slot" where the Rust checks left for the cluster. The station
/// file carries the full evidence; this pins the number and the tie.
#[test]
fn the_ci_workspaces_are_the_stock_of_the_forge_ci_station() {
    let pools = boss_jobs::pools::load_pools(boss_jobs::pools::platform_pools_path())
        .expect("complete one-file-per-pool bundle");
    let ci = pools
        .iter()
        .find(|p| p.name == "disk:ci-workspaces")
        .expect("disk:ci-workspaces is declared");
    assert_eq!(ci.station.as_deref(), Some("forge-ci"), "{ci:?}");
    // v4: v3 added the station and was published live; v4 carries it with
    // car 5ece507d's `reclaimer_kind`, bumped from the live newest so no
    // (name, version) ever names two different rows (conductor order
    // 20261106_0829Z). v5 adds `capacity_from` (backlog 84f570a1).
    assert_eq!(ci.version, 5, "{ci:?}");
    assert_eq!(
        ci.reclaimer_kind.as_deref(),
        Some("maintenance-reap-ci-jobs"),
        "{ci:?}"
    );
    let station =
        boss_jobs::seed_loader::load_stations(boss_jobs::station_seed::platform_stations_path())
            .expect("complete one-file-per-station bundle")
            .into_iter()
            .find(|s| s.name == "forge-ci")
            .expect("forge-ci is declared in infra/platform/stations");
    assert_eq!(station.kind, boss_jobs::StationKind::Equipment);
    assert_eq!(station.status, boss_jobs::registry::WorkflowStatus::Active);
    assert_eq!(
        station.automaton.as_deref(),
        Some("automation:forge-actions-runner")
    );
    assert_eq!(station.bays, Some(1), "the forge's one runner slot");
    // Its membership is the trains waiting on forge CI: an open pr-train
    // whose `ci` step is ready or being worked.
    assert_eq!(station.predicate.kind.as_deref(), Some("pr-train"));
    let step = station.predicate.step.as_ref().expect("a step clause");
    assert_eq!(step.slug.as_deref(), Some("ci"));
    let ci_yml =
        std::fs::read_to_string(boss_testing::repo_root().join(".forgejo/workflows/ci.yml"))
            .expect("the forge's CI workflow");
    assert!(
        ci_yml.contains("one runner slot"),
        "the bay count's evidence moved out of ci.yml; re-measure the runner's capacity"
    );
}

/// Every `.service` unit under `infra/`, as (path, text).
fn units(dir: &std::path::Path, out: &mut Vec<(String, String)>) {
    for entry in std::fs::read_dir(dir).expect("readable infra tree") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            units(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("service") {
            let text = std::fs::read_to_string(&path).expect("unit text");
            out.push((path.display().to_string(), text));
        }
    }
}

/// The kind a text hands the maintenance wrap: the token after
/// `boss-maintenance-wrap.sh` on a unit's line, `None` when it names none.
fn wrap_kind(text: &str) -> Option<String> {
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .find_map(|l| {
            let (_, rest) = l.split_once("boss-maintenance-wrap.sh ")?;
            rest.split_whitespace().next().map(str::to_owned)
        })
}

/// A stock pool's reclaimer runs on a host and files a packet, and the
/// only thing that said WHICH kind was the systemd unit running it — so
/// the estate band asked GET /api/jobs?kind=<the script path>, answered
/// total 0, and drew "not read" for a reclaimer that runs hourly
/// (backlog 5ece507d). The row now names that kind as `reclaimer_kind`,
/// and this pin holds the field to the two places that decide it
/// (CLAUDE.md 9a — a fact that lives twice gets an equality test): the
/// kind is a Workflow the platform bundle declares, and whatever runs
/// the script hands that kind to boss-maintenance-wrap.sh — a unit whose
/// ExecStart is the script, on its wrap line; or, run by no unit (the
/// dev pod's reclaim sidecar), the script itself, which assigns the kind
/// and calls the wrap.
#[test]
fn every_stock_reclaimer_names_the_kind_its_runs_file() {
    let root = boss_testing::repo_root();
    let pools = boss_jobs::pools::load_pools(boss_jobs::pools::platform_pools_path())
        .expect("complete one-file-per-pool bundle");
    let workflows =
        boss_jobs::seed_loader::load_workflows(boss_jobs::registry::platform_bundle_path())
            .expect("workflow bundle");
    let mut all_units = Vec::new();
    units(&root.join("infra"), &mut all_units);
    let mut failures = Vec::new();
    let mut stocks = 0;
    for pool in pools
        .iter()
        .filter(|p| p.kind == boss_jobs::pools::PoolKind::Stock)
    {
        stocks += 1;
        let name = &pool.name;
        let script = pool.reclaimer.as_deref().unwrap_or_default();
        let Some(kind) = pool.reclaimer_kind.as_deref() else {
            failures.push(format!(
                "{name}: declares no reclaimer_kind — no reader can find {script}'s last run"
            ));
            continue;
        };
        if !workflows.iter().any(|w| w.kind == kind) {
            failures.push(format!(
                "{name}: reclaimer_kind `{kind}` is no Workflow in infra/platform/workflows/"
            ));
        }
        let runners: Vec<_> = all_units
            .iter()
            .filter(|(_, text)| {
                text.lines().any(|l| {
                    l.trim()
                        .strip_prefix("ExecStart=")
                        .and_then(|cmd| cmd.split_whitespace().next())
                        .is_some_and(|exe| exe.ends_with(&format!("/{script}")))
                })
            })
            .collect();
        if runners.is_empty() {
            let text = std::fs::read_to_string(root.join(script)).unwrap_or_default();
            let assigns = text.lines().any(|l| {
                l.split_once('=').is_some_and(|(var, value)| {
                    var.ends_with("KIND")
                        && var.bytes().all(|b| b.is_ascii_uppercase() || b == b'_')
                        && value == kind
                })
            });
            if !(assigns && text.contains("boss-maintenance-wrap.sh")) {
                failures.push(format!(
                    "{name}: no unit runs {script}, and the script neither assigns `{kind}` \
                     to a *_KIND variable nor calls boss-maintenance-wrap.sh"
                ));
            }
        }
        for (unit, text) in runners {
            let filed = wrap_kind(text);
            if filed.as_deref() != Some(kind) {
                failures.push(format!(
                    "{name}: {unit} runs {script} and files {filed:?}, the row says `{kind}`"
                ));
            }
        }
    }
    assert!(stocks > 0, "the bundle declares no stock pool to hold");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
