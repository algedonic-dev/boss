//! A gate that launches during a seed refresh waits for the new seed
//! instead of building cold.
//!
//! THE RACE (backlog ae39a328, found triaging 646c0ade). `refresh_seed`
//! takes the EXCLUSIVE flock on the seed, deletes `$SEED/target`, copies
//! the new target into `target.partial` and renames it into place. Until
//! this car `seed_target` tested `[ -d "$SEED/target" ]` BEFORE it took
//! the shared lock, so a reader whose test landed inside that window saw
//! no seed at all, printed "no warm seed", and built cold. Triage run
//! c221e11e read 236 gate pods' logs (2026-09-27 11:40Z to 2026-09-28
//! 11:51Z): 234 seeded, and the 2 that went cold were exactly the two
//! whose clone landed inside a refresh. One branch against itself, the
//! cold run cost +3.6 min (11.3 against 7.7) — so the runner's own
//! "~20+ min extra" line overstated what it announced, too.
//!
//! The fix is one reorder: take the shared lock, then test for the seed
//! inside it. A reader blocked behind a refresh then copies the seed the
//! refresh just installed. These tests run `seed_target` lifted out of
//! run.sh as it ships, against a real `flock` on a scratch lock file.

use boss_testing::{repo_root, scratch_dir};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

const RUN_SH: &str = "infra/gate-runner/run.sh";

struct Rig {
    dir: PathBuf,
    seed: PathBuf,
    lock: PathBuf,
    dest: PathBuf,
}

fn rig(tag: &str) -> Rig {
    let dir = scratch_dir(&format!("seed-inside-lock-{tag}"));
    let seed = dir.join("gate-seed");
    let dest = dir.join("gate-target/target");
    fs::create_dir_all(&seed).unwrap();
    fs::create_dir_all(&dest).unwrap();
    Rig {
        lock: seed.join(".seed.lock"),
        dir,
        seed,
        dest,
    }
}

/// `seed_target` as run.sh ships it, run in its own bash against the rig.
fn reader(r: &Rig) -> Child {
    let script = format!(
        "set -euo pipefail; eval \"$(sed -n '/^seed_target()/,/^}}/p' {})\"; seed_target \"$DEST\"",
        repo_root().join(RUN_SH).display(),
    );
    Command::new("bash")
        .arg("-c")
        .arg(script)
        .env("SEED", &r.seed)
        .env("SEED_LOCK", &r.lock)
        .env("DEST", &r.dest)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn wait_for(path: &Path, what: &str) {
    let start = Instant::now();
    while !path.exists() {
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "timed out waiting for {what}"
        );
        sleep(Duration::from_millis(20));
    }
}

/// THE PIN. A refresher holds the exclusive lock with the seed deleted;
/// a gate starts seeding; the refresher installs the new seed and lets
/// go. The gate must copy that seed, not announce a cold build.
#[test]
fn a_reader_blocked_behind_a_refresh_copies_the_new_seed() {
    let r = rig("refresh");
    // refresh_seed's shape: exclusive lock, target gone, staged copy
    // renamed into place, head written last — then the lock drops.
    let mut holder = Command::new("bash")
        .arg("-c")
        .arg(
            r#"set -euo pipefail
exec 9>>"$SEED_LOCK"
flock -x 9
: > "$DIR/held"
n=0
while [ ! -e "$DIR/go" ]; do sleep 0.05; n=$((n + 1)); [ "$n" -lt 600 ] || exit 9; done
mkdir -p "$SEED/target.partial/debug/deps"
echo rlib > "$SEED/target.partial/debug/deps/libx-0123456789abcdef.rlib"
mv "$SEED/target.partial" "$SEED/target"
echo cafef00d > "$SEED/.seed-head"
"#,
        )
        .env("SEED", &r.seed)
        .env("SEED_LOCK", &r.lock)
        .env("DIR", &r.dir)
        .spawn()
        .unwrap();
    wait_for(
        &r.dir.join("held"),
        "the refresher to take the exclusive lock",
    );
    assert!(
        !r.seed.join("target").exists(),
        "the rig starts inside the refresh window: no seed on disk"
    );

    let mut gate = reader(&r);
    sleep(Duration::from_millis(1000));
    let answered_early = gate.try_wait().unwrap().is_some();

    fs::write(r.dir.join("go"), b"").unwrap();
    assert!(holder.wait().unwrap().success(), "the refresher failed");
    let out = gate.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();

    assert!(out.status.success(), "seed_target failed: {stdout}{stderr}");
    assert!(
        !stdout.contains("no warm seed"),
        "a gate that started during a refresh built cold — it tested for the seed OUTSIDE the \
         lock the refresh holds (ae39a328): {stdout}"
    );
    assert!(
        !answered_early,
        "seed_target answered while the refresh held the exclusive lock: it must wait on the \
         shared lock before it looks for the seed: {stdout}"
    );
    assert!(
        stdout.contains("target seeded from head cafef00d"),
        "the gate must report the head it copied, read inside the lock: {stdout}"
    );
    assert!(
        r.dest
            .join("debug/deps/libx-0123456789abcdef.rlib")
            .is_file(),
        "the new seed must be copied into the gate's target: {stdout}"
    );
    let _ = fs::remove_dir_all(&r.dir);
}

/// No seed and nobody refreshing: a cold build, announced at its
/// measured cost rather than the "~20+ min" the line used to claim.
#[test]
fn no_seed_at_all_is_a_cold_build_at_its_measured_cost() {
    let r = rig("absent");
    let out = reader(&r).wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success(),
        "a missing seed is a cold build, never a failed gate: {stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("no warm seed"), "{stdout}");
    assert!(
        !stdout.contains("20+ min"),
        "the cold cost was measured at +3.6 min on 2026-09-28 (ae39a328), not 20+: {stdout}"
    );
    assert!(
        r.dest.is_dir() && fs::read_dir(&r.dest).unwrap().next().is_none(),
        "a cold build starts from an empty target"
    );
    let _ = fs::remove_dir_all(&r.dir);
}
