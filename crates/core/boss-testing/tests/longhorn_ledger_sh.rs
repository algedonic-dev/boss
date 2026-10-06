//! `infra/lib/longhorn-ledger.sh` answers what longhorn-manager v1.11.3
//! answers, disk by disk.
//!
//! WHY (backlog ab39a34e; adversarial review 091904d3). The library is
//! meant to be the one copy of Longhorn's disk arithmetic, and its first
//! version was not Longhorn's: it judged a growth's floor with the growth
//! as `requiredStorage`, where CheckReplicasSizeExpansion passes 0
//! ("requiredStorage = 0 is intentional", scheduler/replica_scheduler.go
//! :1460) and lets ValidateDiskAvailableForExpansion — which counts
//! storageReserved as free — judge the physical space; and it judged a
//! new replica's placement at twice the volume, where the scheduler's
//! filter (:578) passes the volume's actualSize. On the reviewer's disk it
//! refused a growth Longhorn admits, and a verb reading it would have
//! called a finished move unproven.
//!
//! So each row below is a disk and a question, and the answer is the one
//! Longhorn's Go gives, worked by hand from the v1.11.3 source in the
//! row's comment. Two rows are expand-instance-volume's own fixtures
//! (feat/boss-grows-its-own-instance-volumes at 4bac7954, whose tests
//! assert the same verdicts on the same figures), so the library and that
//! verb are held to one set of answers until backlog 64e00b78 puts the
//! verb on this file. The boundary rows pin `<=` against `<`, which is
//! where a hand copy drifts.

use boss_testing::repo_root;
use serde_json::{Value, json};
use std::process::Command;

const GIB: u64 = 1024 * 1024 * 1024;
/// The w-2 disk the webhook named on 2026-10-01.
const W2_MAX: u64 = 117_656_518_656;
const W2_RESERVED: u64 = 35_296_955_596;

struct Row {
    name: &'static str,
    disk: Value,
    opct: u64,
    mpct: u64,
    /// `["expansion", required]` or `["placement", size, actual]`.
    ask: Value,
    /// The reason Longhorn gives (its Go returns at the first failed
    /// condition, so at most one), as a substring; empty = it takes them.
    refuses: &'static [&'static str],
}

fn disk(max: u64, reserved: u64, scheduled: u64, available: u64) -> Value {
    json!({"max": max, "reserved": reserved, "scheduled": scheduled, "available": available})
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            // The incident's refusal, 20Gi -> 40Gi: ScheduledTotal
            // 70866960384 + 21474836480 = 92341796864 > ProvisionedLimit
            // (117656518656 - 35296955596) x 100% = 82359563060.
            name: "the incident: the ledger refuses 20 GiB more",
            disk: disk(W2_MAX, W2_RESERVED, 70_866_960_384, 50_000_000_000),
            opct: 100,
            mpct: 25,
            ask: json!(["expansion", 20 * GIB]),
            refuses: &[
                "ScheduledTotal = 92341796864 (Size 21474836480 + StorageScheduled 70866960384) \
                 is greater than ProvisionedLimit = 82359563060",
            ],
        },
        Row {
            // Review 091904d3's repro. Validate: physicalUsed =
            // 117656518656 - 50000000000 - 35296955596 = 32359563060,
            // after 64571817780 <= 82359563060, left 53084700876 >=
            // 29414129664. IsSchedulableToDisk(30Gi, 0): 50000000000 >
            // 29414129664, 64424509440 <= 82359563060. All pass; the first
            // library refused it as 50000000000 - 30Gi <= the floor.
            name: "the reviewer's disk: reserved is free to Validate, the floor takes required 0",
            disk: disk(W2_MAX, W2_RESERVED, 30 * GIB, 50_000_000_000),
            opct: 100,
            mpct: 25,
            ask: json!(["expansion", 30 * GIB]),
            refuses: &[],
        },
        Row {
            // expand-instance-volume's a_growth_the_ledger_admits_and_the_
            // disk_cannot_hold_is_refused: physicalUsed 51359563060 +
            // 32212254720 = 83571817780 > 82359563060; the ledger
            // (75161927680) and the floor pass.
            name: "the expand verb's case: the physical limit refuses",
            disk: disk(W2_MAX, W2_RESERVED, 40 * GIB, 31_000_000_000),
            opct: 100,
            mpct: 25,
            ask: json!(["expansion", 30 * GIB]),
            refuses: &["usedAfter=83571817780 > limit=82359563060"],
        },
        Row {
            // expand-instance-volume's over_provisioned_the_minimal_
            // available_form_binds: at 200% the limit is 164719126120;
            // left = 31000000000 + 35296955596 - 42949672960 = 23347282636
            // < 29414129664.
            name: "the expand verb's case: over-provisioned, the minimal-available form refuses",
            disk: disk(W2_MAX, W2_RESERVED, 81_604_378_624, 31_000_000_000),
            opct: 200,
            mpct: 25,
            ask: json!(["expansion", 40 * GIB]),
            refuses: &["left=23347282636 < minimal=29414129664"],
        },
        Row {
            // Validate's `left < minimal` admits equality: left =
            // available + reserved - required = 25 GiB = the floor.
            name: "Validate admits a disk left exactly at the floor",
            disk: disk(100 * GIB, 0, 0, 55 * GIB),
            opct: 100,
            mpct: 25,
            ask: json!(["expansion", 30 * GIB]),
            refuses: &[],
        },
        Row {
            // No growth, and the disk sits AT the floor: IsSchedulableToDisk
            // (required 0) refuses `available <= MinimalAvailable` — the
            // expand verb's `pressure`. Validate has nothing to judge.
            name: "a disk at its floor admits no growth at all",
            disk: disk(100 * GIB, 0, 0, 25 * GIB),
            opct: 100,
            mpct: 25,
            ask: json!(["expansion", 0]),
            refuses: &[
                "CurrentAvailable = 26843545600 (StorageAvailable 26843545600 - Required 0) \
                 is less than or equal to MinimalAvailable = 26843545600",
            ],
        },
        Row {
            // Placement takes actualSize: 60 GiB - 10 GiB > 25 GiB. At twice
            // the size (60 GiB) the first library refused it.
            name: "placement charges the volume's actualSize, not its size",
            disk: disk(100 * GIB, 0, 0, 60 * GIB),
            opct: 100,
            mpct: 25,
            ask: json!(["placement", 30 * GIB, 10 * GIB]),
            refuses: &[],
        },
        Row {
            // `currentAvailable <= minimalAvailable` refuses equality.
            name: "placement refuses a disk left exactly at the floor",
            disk: disk(100 * GIB, 0, 0, 35 * GIB),
            opct: 100,
            mpct: 25,
            ask: json!(["placement", 30 * GIB, 10 * GIB]),
            refuses: &["is less than or equal to MinimalAvailable"],
        },
        Row {
            // `scheduledTotal > overProvisionLimit` admits equality.
            name: "placement admits a ledger filled exactly to its limit",
            disk: disk(100 * GIB, 10 * GIB, 60 * GIB, 80 * GIB),
            opct: 100,
            mpct: 25,
            ask: json!(["placement", 30 * GIB, 10 * GIB]),
            refuses: &[],
        },
        Row {
            name: "a disk reporting no storageMaximum schedules nothing",
            disk: disk(0, 0, 0, 80 * GIB),
            opct: 100,
            mpct: 25,
            ask: json!(["placement", 30 * GIB, 10 * GIB]),
            refuses: &["Storage Max must be greater than 0"],
        },
        Row {
            name: "an unreported storageAvailable is no evidence, never a fit",
            disk: json!({"max": 100 * GIB, "reserved": 0, "scheduled": 0, "available": null}),
            opct: 100,
            mpct: 25,
            ask: json!(["expansion", 30 * GIB]),
            refuses: &["unreported"],
        },
    ]
}

#[test]
fn the_library_answers_what_longhorn_v1_11_3_answers() {
    let rows = rows();
    let input: Vec<Value> = rows
        .iter()
        .map(|r| json!({"disk": r.disk, "opct": r.opct, "mpct": r.mpct, "ask": r.ask}))
        .collect();
    let script = format!(
        ". '{}' && jq -c --argjson rows \"$ROWS\" -n \"$LONGHORN_LEDGER_JQ\"'[$rows[] | .opct as $o | .mpct as $m | .ask as $a \
         | .disk | if $a[0] == \"expansion\" then lh_expansion($a[1]; $o; $m) else lh_placement($a[1]; $a[2]; $o; $m) end]'",
        repo_root().join("infra/lib/longhorn-ledger.sh").display()
    );
    let o = Command::new("bash")
        .args(["-c", &script])
        .env("ROWS", Value::from(input).to_string())
        .output()
        .expect("bash runs");
    assert!(
        o.status.success(),
        "the library evaluates: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    let got: Vec<Vec<String>> =
        serde_json::from_slice(&o.stdout).expect("one array of reason lists");
    assert_eq!(got.len(), rows.len());
    let mut wrong = Vec::new();
    for (row, reasons) in rows.iter().zip(&got) {
        let agrees = reasons.len() == row.refuses.len()
            && row
                .refuses
                .iter()
                .all(|want| reasons.iter().any(|r| r.contains(want)));
        if !agrees {
            wrong.push(format!(
                "{}: Longhorn says {:?}, the library says {reasons:?}",
                row.name, row.refuses
            ));
        }
    }
    assert!(wrong.is_empty(), "\n  {}", wrong.join("\n  "));
}
