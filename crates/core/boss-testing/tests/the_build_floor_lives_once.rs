//! The free-space floor the gate refuses below and the /work floor the
//! dev pod's reclaim acts below are ONE definition, `infra/build-floor.env`,
//! and the reclaim's floor sits above the gate's by a margin.
//!
//! WHY (backlog 99ce8744, 2026-09-24). Measured ~23:00Z: /work had
//! 12 GB free of 40 GB. `infra/gate.sh` refused below 12
//! (`BOSS_GATE_MIN_FREE_GB:-12`) and `infra/cluster/dev-scratch-reclaim.sh`
//! acted only below 6 (`BOSS_WORK_FLOOR_GB:-6`), so the automatic pass
//! could never fire before a pre-flight was refused, and two builders
//! in one hour lowered the gate's floor by hand. The same fact lived
//! twice and had drifted into the one order that disables the reclaim
//! (CLAUDE.md §9a). Collapsed: both scripts read the file, and the
//! reclaim's floor is DERIVED from the gate's, so it cannot be set
//! below it. The reclaim's half of the pin — that its floor is the
//! gate's plus the margin, and that the retired knob the live sidecar
//! still sets moves nothing — is in `dev_scratch_reclaim_sh.rs`, beside
//! the fixtures that drive that script.
//!
//! The /scratch reclaim's floor lives in the same file, and is pinned
//! ABOVE the estate comparison's `disk_tight` floor for a cluster node
//! (backlog 4cc39798): the reclaim must act before the alarm fires, and
//! well before Longhorn refuses.

use boss_testing::repo_root;
use std::process::Command;

#[test]
fn the_floor_fixture_preserves_the_enclosing_receipt() {
    let dir = boss_testing::scratch_dir("build-floor-parent-receipt");
    let parent = dir.join("parent.json");
    let sentinel = "parent-gate-must-retain-this\n";
    boss_testing::write_file(&parent, sentinel);
    let out = Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "the_gate_refuses_below_the_floor_the_file_defines",
            "--nocapture",
        ])
        .env("BOSS_GATE_RECEIPT", &parent)
        .output()
        .expect("run actual floor fixture");
    assert!(
        out.status.success(),
        "floor fixture failed: {}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("1 passed"));
    assert_eq!(
        std::fs::read_to_string(&parent).expect("parent receipt"),
        sentinel
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// The value of `name` in `infra/build-floor.env`, read the way the two
/// scripts read it: the last `NAME=<number>` line.
fn build_floor(name: &str) -> u64 {
    let text = std::fs::read_to_string(repo_root().join("infra/build-floor.env"))
        .expect("infra/build-floor.env is the one definition of the build floor");
    text.lines()
        .rev()
        .find_map(|l| l.strip_prefix(&format!("{name}=")))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or_else(|| panic!("infra/build-floor.env defines {name} as a whole number"))
}

#[test]
fn the_reclaim_trigger_sits_above_the_gates_floor() {
    assert!(
        build_floor("GATE_MIN_FREE_GB") > 0,
        "a zero gate floor refuses nothing"
    );
    assert!(
        build_floor("WORK_RECLAIM_MARGIN_GB") > 0,
        "with no margin the reclaim fires at the same reading the gate \
         refuses at — the automatic pass must act BEFORE any pre-flight \
         is refused, which is the whole packet"
    );
}

/// The value of `const <name>: i64 = <n>;` in the estate comparison —
/// read out of the source, because the two floors below are one shell
/// file and one Rust constant and neither can include the other.
fn estate_const(name: &str) -> i64 {
    let path = "crates/orchestrators/boss-dispatcher-handlers/src/handlers/estate_compare.rs";
    let text = std::fs::read_to_string(repo_root().join(path)).expect(path);
    let decl = format!("const {name}: i64 = ");
    text.lines()
        .find_map(|l| l.trim().strip_prefix(decl.as_str()))
        .and_then(|v| v.trim_end_matches(';').trim().parse().ok())
        .unwrap_or_else(|| panic!("{path} declares `{decl}<whole number>;`"))
}

/// The five constants `disk_tight_finding` builds a cluster node's floor
/// from, each read out of estate_compare.rs.
struct EstateFloor {
    floor_gb: i64,
    floor_pct: i64,
    ceiling_gb: i64,
    longhorn_pct: i64,
    margin_pct: i64,
}

impl EstateFloor {
    fn read() -> Self {
        Self {
            floor_gb: estate_const("DISK_TIGHT_FLOOR_GB"),
            floor_pct: estate_const("DISK_TIGHT_FLOOR_PCT"),
            ceiling_gb: estate_const("DISK_TIGHT_HEADROOM_CEILING_GB"),
            longhorn_pct: estate_const("LONGHORN_MINIMAL_AVAILABLE_PCT"),
            margin_pct: estate_const("LONGHORN_FLOOR_MARGIN_PCT"),
        }
    }

    /// `disk_tight_finding` for `DiskScope::ClusterNode`, term by term:
    /// `disk_floor_gb(total).max(longhorn_alarm_gb(total))`, each rounded
    /// up as estate_compare.rs rounds it.
    fn cluster_node_gb(&self, total_gb: i64) -> i64 {
        assert!(
            self.floor_gb <= self.ceiling_gb,
            "estate_compare.rs clamps between DISK_TIGHT_FLOOR_GB and \
             DISK_TIGHT_HEADROOM_CEILING_GB, so the first must not exceed the second"
        );
        let disk_floor =
            ((total_gb * self.floor_pct + 99) / 100).clamp(self.floor_gb, self.ceiling_gb);
        let longhorn_alarm =
            (total_gb * self.longhorn_pct * (100 + self.margin_pct) + 9_999) / 10_000;
        disk_floor.max(longhorn_alarm)
    }
}

/// The three `SCRATCH_RECLAIM_*` keys of infra/build-floor.env, and the
/// floor dev-scratch-reclaim.sh's `scratch_floor_gb` builds from them
/// when its knob is unset, as the live sidecar runs it.
struct ReclaimFloor {
    pct: i64,
    headroom_pct: i64,
    headroom_ceiling_gb: i64,
}

impl ReclaimFloor {
    fn read() -> Self {
        Self {
            pct: build_floor("SCRATCH_RECLAIM_FLOOR_PCT") as i64,
            headroom_pct: build_floor("SCRATCH_RECLAIM_HEADROOM_PCT") as i64,
            headroom_ceiling_gb: build_floor("SCRATCH_RECLAIM_HEADROOM_CEILING_GB") as i64,
        }
    }

    /// Shell integer math, term by term: each share floored, the
    /// headroom share capped, the higher of the two.
    fn gb(&self, total_gb: i64) -> i64 {
        let share = total_gb * self.pct / 100;
        let headroom = (total_gb * self.headroom_pct / 100).min(self.headroom_ceiling_gb);
        share.max(headroom)
    }
}

/// THE SCRATCH FLOOR SITS ABOVE THE ESTATE'S ALARM (backlog 4cc39798,
/// 2026-10-20). On a Talos node /scratch is the kubelet's nodefs, the
/// disk the estate comparison judges `disk_tight` for a cluster node at
/// `disk_floor_gb(total).max(longhorn_alarm_gb(total))`: 35% of the disk
/// held between 16 and 200 GB, or Longhorn's 25% refusal plus a 10%
/// margin (27.5%), whichever is higher. The reclaim's floor was 25%: on
/// w-1 (929 GB) it acted below 232 GB, the alarm fired below 256, and
/// the pass logged "251GB free >= 232GB floor — no build-cache reclaim"
/// while the alarm stood three readings running — and when it would
/// act, Longhorn was already refusing. Two numbers in two languages
/// under no mechanism, drifted into the order that makes the reclaim
/// answer only after the alarm (CLAUDE.md §9a). Pinned, not collapsed:
/// the comparison is compiled into the dispatcher image, which carries
/// no infra/ tree to read.
///
/// This pin's first version read only the Longhorn pair, and so stayed
/// green over a 30% floor that sat UNDER the 35% term on every disk
/// from 100 to 669 GB (500 GB: 150 against 175), and over a raise of
/// the 200 GB ceiling to 280 that put w-1's alarm above its 278 (review
/// 1eee8160). So it now computes the alarm from all five constants, as
/// `disk_tight_finding` does, and the reclaim from the file's whole rule,
/// as `scratch_floor_gb` does. Checked at every disk size from 100 GB —
/// under the dev pod's own 150Gi ephemeral-storage request, so no smaller
/// disk holds /scratch — to 10 TB, with each side's own rounding (the
/// reclaim floors, the comparison rounds up), because a pin on the
/// percentages alone would pass a floor the roundings put level on some
/// disk.
#[test]
fn the_scratch_reclaim_floor_sits_above_the_estates_disk_tight_floor() {
    let reclaim = ReclaimFloor::read();
    let estate = EstateFloor::read();
    assert!(
        reclaim.pct <= 100 && reclaim.headroom_pct <= 100,
        "SCRATCH_RECLAIM_FLOOR_PCT and SCRATCH_RECLAIM_HEADROOM_PCT are shares of the disk"
    );
    for disk_gb in 100..=10_000_i64 {
        let reclaim_gb = reclaim.gb(disk_gb);
        let alarm_gb = estate.cluster_node_gb(disk_gb);
        assert!(
            reclaim_gb > alarm_gb,
            "on a {disk_gb} GB disk the scratch reclaim acts below {reclaim_gb} GB \
             (SCRATCH_RECLAIM_FLOOR_PCT={}, SCRATCH_RECLAIM_HEADROOM_PCT={} up to \
             SCRATCH_RECLAIM_HEADROOM_CEILING_GB={} in infra/build-floor.env) but the \
             estate's disk_tight fires below {alarm_gb} GB (DISK_TIGHT_FLOOR_PCT={} \
             between DISK_TIGHT_FLOOR_GB={} and DISK_TIGHT_HEADROOM_CEILING_GB={}, or \
             LONGHORN_MINIMAL_AVAILABLE_PCT={} + LONGHORN_FLOOR_MARGIN_PCT={}%, in \
             estate_compare.rs): the reclaim must act BEFORE the alarm, so raise the \
             reclaim term under the estate term that rules on this disk",
            reclaim.pct,
            reclaim.headroom_pct,
            reclaim.headroom_ceiling_gb,
            estate.floor_pct,
            estate.floor_gb,
            estate.ceiling_gb,
            estate.longhorn_pct,
            estate.margin_pct,
        );
    }
}

/// Driven through the real script: with no `BOSS_GATE_MIN_FREE_GB` in
/// the environment, the gate refuses one GB under the file's floor and
/// names that floor. A builder's shell that lowered the knob by hand is
/// exactly what this must not inherit, so it is removed explicitly.
#[test]
fn the_gate_refuses_below_the_floor_the_file_defines() {
    let floor = build_floor("GATE_MIN_FREE_GB");
    let dir = boss_testing::scratch_dir("boss-build-floor-gate");
    let fake = dir.join("df");
    let kb = (floor - 1) * 1024 * 1024;
    boss_testing::write_exec(
        &fake,
        &format!(
            "#!/usr/bin/env bash\n\
             echo 'Filesystem 1024-blocks Used Available Capacity Mounted on'\n\
             for _ in \"$@\"; do echo '/dev/fake 1 1 {kb} 99% /'; done\n"
        ),
    );
    // Refused at `require_headroom "to start"`, before any git call —
    // the ordering gate_sh.rs `the_gate_refuses_to_run_without_headroom`
    // relies on for the same invocation.
    let out = Command::new("bash")
        .arg(repo_root().join("infra/gate.sh"))
        .arg("--auto")
        .env_remove("BOSS_GATE_MIN_FREE_GB")
        .env("BOSS_GATE_DF_CMD", fake.to_str().expect("utf8"))
        .current_dir(repo_root())
        .output()
        .expect("run gate.sh");
    let _ = std::fs::remove_dir_all(&dir);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(2),
        "a floor refusal is exit 2, not a failed check\nstderr: {stderr}"
    );
    assert!(
        stderr.contains(&format!(
            "{}GB free, need {floor}GB. Refusing to start",
            floor - 1
        )),
        "the gate must apply the floor infra/build-floor.env defines ({floor}GB)\nstderr: {stderr}"
    );
}

/// No script spells a default for any floor of its own. A second
/// literal is how the two drifted apart: `${BOSS_GATE_MIN_FREE_GB:-12}`
/// and `${BOSS_WORK_FLOOR_GB:-6}` were each correct when written, and
/// `${BOSS_SCRATCH_FLOOR_PCT:-25}` was correct until the estate's alarm
/// rose above it (4cc39798).
#[test]
fn neither_script_carries_a_floor_literal_of_its_own() {
    for path in ["infra/gate.sh", "infra/cluster/dev-scratch-reclaim.sh"] {
        let text = std::fs::read_to_string(repo_root().join(path)).expect(path);
        for knob in [
            "BOSS_GATE_MIN_FREE_GB:-",
            "BOSS_WORK_FLOOR_GB:-",
            "BOSS_SCRATCH_FLOOR_PCT:-",
        ] {
            let literal = text.match_indices(knob).any(|(at, _)| {
                text[at + knob.len()..]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_digit())
            });
            assert!(
                !literal,
                "{path} spells its own default for {knob}<number>; read the floor \
                 from infra/build-floor.env"
            );
        }
        assert!(
            text.contains("build-floor.env"),
            "{path} must read the floor from infra/build-floor.env"
        );
    }
}
