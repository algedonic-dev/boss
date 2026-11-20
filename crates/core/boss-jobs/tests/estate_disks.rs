//! A node's DISKS are declared in the estate registry beside its
//! capacity (backlog ba0b7308; design f29798c0 §7, car 1: "Disk model
//! ... go[es] into the estate registry beside each node. That closes the
//! 'disk models are not in the estate registry' gap").
//!
//! WHY. On 2026-10-29 the question "which drive is under etcd and the
//! database on cp-2" was answered by an operator running `talos-get
//! <node> disks` five times and copying models into a packet
//! (d9b39685 `disks_20261029T1957Z`), because the registry knew a node's
//! disk SIZE and nothing about what the disk is. The drive class was the
//! whole diagnosis — a DRAM-less OEM client NVMe stalls for seconds under
//! sustained fsync — so it is a declared fact of the machine, as its
//! capacity is: reviewed in the tree, published by the launcher, and
//! refreshed on a node that already exists the way `disk_gb` is
//! (c426bc8a), because a drive is replaced in place (w-2's, 2026-10-19).
//!
//! The contract mirrors disk_gb's: an explicit list refreshes a kept
//! node and its fact carries the before and after; an omitted list keeps
//! what is there; an identical repeat lands nothing and says nothing.

use boss_core::{actor::ActorId, publisher::EventStamp};
use boss_jobs::port::{EstateDisk, EstateNodeInput, JobsRepository};
use serde_json::json;

fn disk(model: &str, size_bytes: i64) -> EstateDisk {
    EstateDisk {
        model: model.into(),
        size_bytes,
    }
}

/// w-2 as `talos-get w-2 disks` read it (ops-request 247a3be9).
fn nv3() -> Vec<EstateDisk> {
    vec![disk("KINGSTON SNV3S1000G", 1_000_204_886_016)]
}

/// The power-loss-protected drive design f29798c0 Q1 recommends for w-2.
fn plp() -> Vec<EstateDisk> {
    vec![disk("Micron_7450_MTFDKBA960TFR", 960_197_124_096)]
}

fn node(disks: Option<Vec<EstateDisk>>) -> EstateNodeInput {
    EstateNodeInput {
        id: "w-2".into(),
        label: "w-2".into(),
        address: "192.0.2.16".into(),
        role: "talos-worker".into(),
        roles: vec![],
        cpu: Some(4),
        memory_gb: Some(15),
        disk_gb: Some(929),
        notes: None,
        disks,
    }
}

fn stamp() -> EventStamp {
    EventStamp::new("jobs", ActorId::Automation("estate-seed".into()))
}

#[test]
fn a_declared_disk_names_its_model_and_a_positive_size() {
    boss_jobs::port::validate_estate_node(&node(Some(nv3()))).unwrap();
    boss_jobs::port::validate_estate_node(&node(None)).unwrap();
    for (bad, why) in [
        (disk("  ", 1_000), "model"),
        (disk("KINGSTON SNV3S1000G", 0), "size_bytes"),
        (disk("KINGSTON SNV3S1000G", -1), "size_bytes"),
    ] {
        let err = boss_jobs::port::validate_estate_node(&node(Some(vec![bad])))
            .expect_err("a disk with no model or no size is not a declaration");
        assert!(err.contains("w-2") && err.contains(why), "{err}");
    }
}

#[test]
fn the_estate_file_declares_disks_and_refuses_an_observed_field_on_one() {
    let file = r#"
[[node]]
id = "w-2"
label = "w-2"
address = "192.0.2.16"
role = "talos-worker"
disks = [{ model = "KINGSTON SNV3S1000G", size_bytes = 1000204886016 }]
"#;
    let rows = boss_jobs::estate_seed::parse_estate_toml(file).unwrap();
    assert_eq!(rows[0].disks, Some(nv3()));
    // Latency is a measurement, and measurements ride the log.
    let observed = file.replace(
        "size_bytes = 1000204886016 }",
        "size_bytes = 1000204886016, mean_flush_ms = 90 }",
    );
    let why = boss_jobs::estate_seed::parse_estate_toml(&observed).unwrap_err();
    assert!(why.contains("mean_flush_ms"), "{why}");
}

#[test]
fn the_trees_estate_names_the_disks_of_every_talos_node() {
    let file = boss_testing::repo_root().join("infra/estate/estate.toml");
    let rows = boss_jobs::estate_seed::load_estate_toml(&file).expect("the tree's estate parses");
    let talos: Vec<_> = rows
        .iter()
        .filter(|r| r.role.starts_with("talos-"))
        .collect();
    assert!(talos.len() >= 5, "the cluster's nodes: {}", talos.len());
    for n in talos {
        let disks = n.disks.as_deref().unwrap_or_default();
        assert!(
            !disks.is_empty(),
            "{} is a Talos node and declares no disks in {} — read them with \
             `boss ops forge talos-get {} disks` and declare each physical drive",
            n.id,
            file.display(),
            n.id
        );
    }
}

/// What both adapters owe.
async fn disks_contract(repo: &impl JobsRepository) {
    let out = repo
        .declare_estate_nodes(&[node(Some(nv3()))], &stamp())
        .await
        .unwrap();
    assert_eq!((out.inserted, out.updated), (1, 0));
    assert_eq!(repo.list_estate_nodes().await.unwrap()[0].disks, nv3());

    // The drive is replaced in place: an explicit list refreshes.
    let out = repo
        .declare_estate_nodes(&[node(Some(plp()))], &stamp())
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(out).unwrap(),
        json!({"received":1,"inserted":0,"updated":1,"roles_inserted":0})
    );
    assert_eq!(repo.list_estate_nodes().await.unwrap()[0].disks, plp());

    // The same list again lands nothing; an omitted list keeps it.
    for again in [Some(plp()), None] {
        let out = repo
            .declare_estate_nodes(&[node(again)], &stamp())
            .await
            .unwrap();
        assert_eq!(out.updated, 0);
    }
    let rows = repo.list_estate_nodes().await.unwrap();
    assert_eq!(rows[0].disks, plp());
    assert_eq!(rows[0].disk_gb, Some(929), "capacity is untouched");
}

fn landed_change() -> serde_json::Value {
    json!({
        "node": false,
        "roles": [],
        "disks": {
            "before": [{"model": "KINGSTON SNV3S1000G", "size_bytes": 1_000_204_886_016_i64}],
            "after": [{"model": "Micron_7450_MTFDKBA960TFR", "size_bytes": 960_197_124_096_i64}]
        }
    })
}

#[tokio::test]
async fn memory_declares_and_refreshes_a_nodes_disks_with_one_fact_per_change() {
    let repo = boss_jobs::InMemoryJobs::new();
    disks_contract(&repo).await;
    let facts: Vec<_> = repo
        .recorded_events()
        .into_iter()
        .filter(|e| e.kind == "node.declared")
        .collect();
    assert_eq!(
        facts.len(),
        2,
        "insert and one refresh; repeats say nothing"
    );
    assert_eq!(
        facts[0].payload["landed"],
        json!({"node": true, "roles": []})
    );
    assert_eq!(
        facts[0].payload["disks"],
        json!([{"model": "KINGSTON SNV3S1000G", "size_bytes": 1_000_204_886_016_i64}])
    );
    assert_eq!(facts[1].payload["landed"], landed_change());
}

#[cfg(feature = "postgres")]
#[tokio::test(flavor = "multi_thread")]
async fn postgres_declares_and_refreshes_a_nodes_disks_with_one_fact_per_change() {
    let db = boss_testing::TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    disks_contract(&repo).await;
    let payloads: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT payload FROM event_outbox WHERE kind = 'node.declared' ORDER BY id",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(payloads.len(), 2, "{payloads:?}");
    assert_eq!(payloads[1]["landed"], landed_change());
}

#[test]
fn a_node_without_disks_serializes_as_it_did_before() {
    // Facts already in the log carry no `disks` key, and a declaration
    // that names none must not start writing `"disks": null` beside them.
    let v = serde_json::to_value(node(None)).unwrap();
    assert!(v.get("disks").is_none(), "{v}");
}
