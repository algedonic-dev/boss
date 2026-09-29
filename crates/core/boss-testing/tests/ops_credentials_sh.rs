//! `infra/estate/ops-credentials.sh` — the ONE check of a host's declared
//! root material, read by the converge (install-cluster-operator.sh) and
//! carried on the host observation (observe-host.sh) so the estate
//! compare can raise its absence (backlog 714bc71f).
//!
//! WHY IT IS A FILE. The forge declared cluster-operator and held neither
//! /etc/boss-ops/kubeconfig nor talosconfig from 2026-09-25 22:04Z on.
//! The converge recorded `ops_credentials not ready` on every packet and
//! nothing read it; car 78a88f65 then made the same absence fatal
//! through a second reader, and every forge converge closed failed for
//! twelve hours with no alarm. The fix makes the converge record, not
//! fail, and sends the reading to the estate compare — so two readers
//! need the check, and a second copy of it is the pair CLAUDE.md §9a
//! says to collapse.
//!
//! HOW THIS IS MEASURED. The function runs under `sh` (dash on the
//! hosts — observe-host.sh is `#!/bin/sh`), and observe-host.sh runs for
//! real against an unreachable jobs API, so the observation it would
//! have posted lands in its spool, where this reads it back.

use boss_testing::{repo_root, scratch_dir, scratch_path};
use std::process::Command;

const LIB: &str = "infra/estate/ops-credentials.sh";

/// What the caller that died printed after it had its answer. Only a
/// caller that got PAST the reading prints it.
const CONTINUED: &str = "caller continued past the reading";

/// The check run the way observe-host.sh runs it: `sh` (dash on the
/// hosts), `set -eu`, the lib sourced, then `ops_state=$(…)`. Plain
/// `sh -c '. lib; ops_credentials_state'` has no -e, and so passed a
/// door whose nonzero status killed the real caller with nothing
/// printed — the forge's whole host reading, disk and all, lost on the
/// first day a credential went missing (review of 058b1ef1, H1).
///
/// Returns the exit status and the reading; a caller that did not get
/// past the reading returns everything it printed instead, so the
/// failure names itself.
fn observe_shaped(
    ops_dir: &std::path::Path,
    bin: Option<&std::path::Path>,
    timeout_s: Option<&str>,
) -> (i32, String) {
    // The forge, which declares both root credentials; the per-host set
    // is a_host_declares_the_ops_credentials_it_holds.rs (f371c749).
    observe_shaped_as("forge", ops_dir, bin, timeout_s)
}

/// [`observe_shaped`] for a named host, whose declared set — its
/// `[ops_credentials.<host>]` table in the tree's estate.toml — is the
/// one judged, named as observe-host.sh names its HOST_ID.
fn observe_shaped_as(
    host: &str,
    ops_dir: &std::path::Path,
    bin: Option<&std::path::Path>,
    timeout_s: Option<&str>,
) -> (i32, String) {
    let lib = repo_root().join(LIB);
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(format!(
            "set -eu\n. '{}'\nops_state=$(ops_credentials_state {host})\nprintf '%s\\n' \"$ops_state\"\necho '{CONTINUED}'\n",
            lib.display()
        ))
        .env(
            "BOSS_ESTATE_SOURCE",
            repo_root().join("infra/estate/estate.toml"),
        )
        .env("BOSS_OPS_DIR", ops_dir);
    if let Some(bin) = bin {
        cmd.env(
            "PATH",
            format!(
                "{}:{}",
                bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        );
    }
    if let Some(t) = timeout_s {
        cmd.env("BOSS_OPS_DOOR_TIMEOUT_S", t);
    }
    let out = cmd.output().expect("sh runs");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let rc = out.status.code().unwrap_or(-1);
    match stdout.trim_end().strip_suffix(CONTINUED) {
        Some(reading) => (rc, reading.trim().to_string()),
        None => (
            rc,
            format!(
                "THE CALLER DIED before it got past the reading: stdout={stdout:?} stderr={:?}",
                String::from_utf8_lossy(&out.stderr)
            ),
        ),
    }
}

fn state(ops_dir: &str) -> (i32, String) {
    observe_shaped(std::path::Path::new(ops_dir), None, None)
}

#[test]
fn an_absent_directory_is_both_credentials_absent() {
    let dir = scratch_path("ops-creds-absent");
    let (rc, out) = state(dir.to_str().expect("utf8"));
    assert_eq!(rc, 0, "{out}");
    assert_eq!(out, "not ready: talosconfig:absent kubeconfig:absent");
    assert!(!dir.exists(), "the check never creates the directory");
}

#[test]
fn a_file_a_test_can_make_is_named_with_what_it_actually_is() {
    // A test process cannot make a root:root 0600 file — whatever it
    // makes is wrong, and the check must say what it found, not pass.
    let dir = scratch_dir("ops-creds-mode");
    std::fs::write(dir.join("kubeconfig"), "placeholder").expect("write");
    let (rc, out) = state(dir.to_str().expect("utf8"));
    assert_eq!(rc, 0, "{out}");
    assert!(
        out.starts_with("not ready: talosconfig:absent kubeconfig:"),
        "{out}"
    );
    assert!(!out.contains("kubeconfig:absent"), "it is there: {out}");
}

// ── THE UNSEARCHABLE DIRECTORY (backlog 6296dce1, 2026-09-29) ─────────
//
// The forge's observer runs as david and /etc/boss-ops is 0700 root, so
// every host reading from the forge said `unmeasured` for a week and the
// `ops_credentials_absent` class was BLIND there — its alarm could not
// fire. The same unit already reaches that directory through
// `sudo docker run --mount …,readonly` (ops_kubectl, ops_talosctl), so
// the check now stats the two files through that door: owner, group and
// mode, never a byte of either file, and never a change to the directory.
// `unmeasured` stays, for the one case it is true of: the door refused.
//
// No test here runs the real sudo or docker. A stand-in pair on PATH
// records every argument it was handed, and the stand-in docker answers
// what its test tells it the daemon would have seen.

/// A directory this process cannot search, or `None` where the fixture
/// cannot seal one. MEASURED, not inferred from the uid: this pod's
/// root holds no CAP_DAC_READ_SEARCH (CapEff 0x200400c0), so a 000
/// directory is as closed to it as to the gate's uid 65534, while a
/// root that does hold it searches anything and the branch is out of
/// reach — and then there is nothing to assert.
fn sealed(name: &str) -> Option<std::path::PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch_dir(name);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    let searchable = Command::new("sh")
        .arg("-c")
        .arg("[ -x \"$1\" ]")
        .arg("sh")
        .arg(&dir)
        .status()
        .expect("sh runs")
        .success();
    if searchable {
        unseal(&dir);
        return None;
    }
    Some(dir)
}

fn unseal(dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    // mode-bits-ok: a directory given its search bit back; nothing execs it
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).expect("chmod");
}

/// The mode of `dir` as `stat` prints it, read by the test process as
/// its owner — so the check provably left the directory as it found it.
fn mode_of(dir: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(dir).expect("stat").permissions().mode() & 0o7777
}

/// The stand-in `sudo` and `docker`, in a bin dir to put first on PATH.
/// `sudo` refuses when `refuse` is set (the words sudo -n prints), and
/// otherwise drops its `-n` and runs the rest. `docker` runs `body`
/// after recording. Both append their argv, one argument per line, to
/// `argv.log`.
struct Door {
    bin: std::path::PathBuf,
    log: std::path::PathBuf,
}

fn stand_in(name: &str, refuse: bool, body: &str) -> Door {
    let root = scratch_dir(name);
    let bin = root.join("bin");
    boss_testing::create_dir(&bin);
    let log = root.join("argv.log");
    let record = format!(
        "for a in \"$0\" \"$@\"; do printf '%s\\n' \"$a\" >> '{}'; done\n",
        log.display()
    );
    let sudo = if refuse {
        format!("#!/bin/sh\n{record}echo 'sudo: a password is required' >&2\nexit 1\n")
    } else {
        format!("#!/bin/sh\n{record}[ \"$1\" = -n ] && shift\nexec \"$@\"\n")
    };
    boss_testing::write_exec(&bin.join("sudo"), &sudo);
    boss_testing::write_exec(&bin.join("docker"), &format!("#!/bin/sh\n{record}{body}"));
    Door { bin, log }
}

/// A `docker` that prints the canned stdout and stderr it is handed and
/// exits with the given code — what the daemon would have answered.
fn door(name: &str, refuse: bool, stdout: &str, stderr: &str, rc: i32) -> Door {
    let root = scratch_dir(&format!("{name}-canned"));
    let out = root.join("docker.out");
    let err = root.join("docker.err");
    boss_testing::write_file(&out, stdout);
    boss_testing::write_file(&err, stderr);
    stand_in(
        name,
        refuse,
        &format!(
            "cat '{}'\ncat '{}' >&2\nexit {rc}\n",
            out.display(),
            err.display()
        ),
    )
}

fn state_through(ops_dir: &std::path::Path, door: &Door) -> (i32, String) {
    observe_shaped(ops_dir, Some(&door.bin), None)
}

fn argv(door: &Door) -> Vec<String> {
    std::fs::read_to_string(&door.log)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

/// What `stat -c '%n %U:%G %a %u:%g'` prints inside the container for
/// the directory and both credentials as David placed them.
const BOTH_PLACED: &str = "/ops/. root:root 700 0:0\n\
                           /ops/talosconfig root:root 600 0:0\n\
                           /ops/kubeconfig root:root 600 0:0\n";

#[test]
fn a_directory_the_reader_cannot_search_is_measured_through_the_privileged_door() {
    let Some(dir) = sealed("ops-creds-door") else {
        return;
    };
    let d = door("ops-creds-door-bin", false, BOTH_PLACED, "", 0);
    let (rc, out) = state_through(&dir, &d);
    let mode = mode_of(&dir);
    unseal(&dir);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(out, "present", "argv: {:?}", argv(&d));
    assert_eq!(mode, 0, "the check never changes the directory's mode");
}

#[test]
fn the_door_reads_metadata_only_through_a_readonly_mount() {
    // The trust boundary of this change (backlog 6296dce1): the door is
    // root, so what it is ASKED to do is the whole guarantee. It is
    // asked for owner, group and mode through a read-only mount with no
    // network, and for nothing that opens either file.
    let Some(dir) = sealed("ops-creds-argv") else {
        return;
    };
    let d = door("ops-creds-argv-bin", false, BOTH_PLACED, "", 0);
    let _ = state_through(&dir, &d);
    unseal(&dir);
    let args = argv(&d);
    let sudo_at = args
        .iter()
        .position(|a| a.ends_with("/sudo"))
        .unwrap_or_else(|| panic!("sudo was the door: {args:?}"));
    assert_eq!(
        args.get(sudo_at + 1).map(String::as_str),
        Some("-n"),
        "a reader on a timer never waits at a password prompt: {args:?}"
    );
    // What docker itself was handed — the stand-in sudo logs the same
    // words first, so read from docker's own entry on.
    let docker_at = args
        .iter()
        .position(|a| a.ends_with("/docker"))
        .unwrap_or_else(|| panic!("docker ran behind sudo: {args:?}"));
    let docker: Vec<&str> = args[docker_at + 1..].iter().map(String::as_str).collect();
    let mount = format!("type=bind,src={},dst=/ops,readonly", dir.display());
    assert!(
        docker.windows(2).any(|w| w == ["--mount", mount.as_str()]),
        "a read-only bind of the directory: {docker:?}"
    );
    assert!(
        docker.windows(2).any(|w| w == ["--network", "none"]),
        "no network: {docker:?}"
    );
    let image_at = docker
        .iter()
        .position(|a| *a == "alpine/k8s:1.33.3")
        .unwrap_or_else(|| panic!("the image ops_kubectl pulls, not a second one: {docker:?}"));
    // `timeout 10` INSIDE the container: killing the host-side client
    // leaves a container running (review of 058b1ef1, M1).
    assert_eq!(
        &docker[image_at + 1..],
        [
            "timeout",
            "10",
            "stat",
            "-c",
            "%n %U:%G %a %u:%g",
            "/ops/.",
            "/ops/talosconfig",
            "/ops/kubeconfig"
        ],
        "the container runs a bounded stat on the two paths and nothing else"
    );
}

#[test]
fn a_credential_the_door_cannot_find_is_absent_and_a_wrong_mode_is_named() {
    // stat exits 1 when a credential is missing — the case this reading
    // exists to report — and the caller runs under set -eu. The reading
    // must still print and the caller must go on (review of 058b1ef1,
    // H1: it used to die here with nothing printed).
    let Some(dir) = sealed("ops-creds-door-partial") else {
        return;
    };
    let d = door(
        "ops-creds-door-partial-bin",
        false,
        "/ops/. root:root 700 0:0\n/ops/kubeconfig root:root 644 0:0\n",
        "stat: can't stat '/ops/talosconfig': No such file or directory\n",
        1,
    );
    let (rc, out) = state_through(&dir, &d);
    unseal(&dir);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(
        out,
        "not ready: talosconfig:absent kubeconfig:root:root 644 0:0"
    );
}

#[test]
fn the_door_judges_only_the_set_the_host_declares() {
    // The two cars assembled (6296dce1's door, f371c749's declared set):
    // the door stats both paths whatever the host, but boss-gcp declares
    // the scoped kubeconfig alone, so a talosconfig the door cannot find
    // is no word of its reading — the alarm must never ask for one there.
    let Some(dir) = sealed("ops-creds-door-edge") else {
        return;
    };
    let d = door(
        "ops-creds-door-edge-bin",
        false,
        "/ops/. root:root 700 0:0\n/ops/kubeconfig root:root 644 0:0\n",
        "stat: can't stat '/ops/talosconfig': No such file or directory\n",
        1,
    );
    let (rc, out) = observe_shaped_as("boss-gcp", &dir, Some(&d.bin), None);
    unseal(&dir);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(out, "not ready: kubeconfig:root:root 644 0:0");
}

#[test]
fn an_owner_is_judged_by_its_number_not_the_containers_name_for_it() {
    // The names come from the IMAGE's /etc/passwd. A host account the
    // image does not know reads UNKNOWN there, and one it does know may
    // read as someone else; the uid:gid beside them is the host's own
    // fact, and it is what decides (review of 058b1ef1, L2).
    let Some(dir) = sealed("ops-creds-door-uid") else {
        return;
    };
    let d = door(
        "ops-creds-door-uid-bin",
        false,
        "/ops/. root:root 700 0:0\n\
         /ops/talosconfig UNKNOWN:UNKNOWN 600 1000:1000\n\
         /ops/kubeconfig root:root 600 0:0\n",
        "",
        0,
    );
    let (rc, out) = state_through(&dir, &d);
    unseal(&dir);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(out, "not ready: talosconfig:UNKNOWN:UNKNOWN 600 1000:1000");
}

#[test]
fn a_door_that_refuses_is_unmeasured_and_names_the_refusal() {
    // The one case `unmeasured` is still true of. "absent" would be a
    // guess, and so would "present"; the reading says which door
    // refused and in whose words — under the caller's set -eu, which a
    // refusing sudo's exit 1 used to turn into a dead caller (H1).
    let Some(dir) = sealed("ops-creds-refused") else {
        return;
    };
    let d = door("ops-creds-refused-bin", true, "", "", 0);
    let (rc, out) = state_through(&dir, &d);
    unseal(&dir);
    assert_eq!(rc, 0, "{out}");
    assert!(out.starts_with("unmeasured: "), "{out}");
    assert!(
        out.contains("not searchable by") && out.contains("sudo: a password is required"),
        "{out}"
    );
}

#[test]
fn a_door_that_answers_without_the_directory_is_unmeasured_not_absent() {
    // Docker's refusal (a mount it could not make, a daemon that is
    // down) prints no stat line at all. Only a line for the directory
    // itself proves the container saw inside it; without one, two
    // missing credential lines are silence, not absence.
    let Some(dir) = sealed("ops-creds-docker-refused") else {
        return;
    };
    let d = door(
        "ops-creds-docker-refused-bin",
        false,
        "",
        "docker: Error response from daemon: invalid mount config\n",
        125,
    );
    let (rc, out) = state_through(&dir, &d);
    unseal(&dir);
    assert_eq!(rc, 0, "{out}");
    assert!(out.starts_with("unmeasured: "), "{out}");
    assert!(out.contains("invalid mount config"), "{out}");
}

#[test]
fn a_door_that_ignores_term_is_killed_and_the_reading_goes_on() {
    // This door runs inside the forge's OWN host reading, the one the
    // boarding check reads, so a wedged docker must cost it seconds and
    // not the unit's ten-minute start timeout. `docker run` proxies
    // TERM, and a client stuck on its daemon can outlive it: the
    // stand-in here ignores TERM outright, so only the host-side
    // `timeout -k` KILL ends it (review of 058b1ef1, M1). Without the
    // bound, or without its -k, this takes the stand-in's full 30s.
    let Some(dir) = sealed("ops-creds-hang") else {
        return;
    };
    let d = stand_in("ops-creds-hang-bin", false, "trap '' TERM\nsleep 30\n");
    let started = std::time::Instant::now();
    let (rc, out) = observe_shaped(&dir, Some(&d.bin), Some("1"));
    let took = started.elapsed();
    unseal(&dir);
    assert_eq!(rc, 0, "{out}");
    assert!(out.starts_with("unmeasured: "), "{out}");
    assert!(
        took < std::time::Duration::from_secs(20),
        "the door held the reading {took:?}: {out}"
    );
}

#[test]
fn both_readers_source_the_one_check() {
    for reader in [
        "infra/estate/install-cluster-operator.sh",
        "infra/estate/observe-host.sh",
    ] {
        let text = std::fs::read_to_string(repo_root().join(reader))
            .unwrap_or_else(|e| panic!("{reader} is readable: {e}"));
        assert!(
            text.contains("ops-credentials.sh") && text.contains("ops_credentials_state"),
            "{reader} must source and call the one check, not carry its own copy"
        );
    }
}

#[test]
fn the_host_observation_carries_the_credential_reading() {
    let root = scratch_dir("ops-creds-observe");
    let spool = root.join("spool");
    let ops = root.join("boss-ops");
    let out = Command::new("sh")
        .arg(repo_root().join("infra/estate/observe-host.sh"))
        .env("HOST_ID", "forge")
        // Nothing listens on port 1: the POST fails and the observation
        // is retained, which is where this reads it.
        .env("JOBS_API", "http://127.0.0.1:1")
        .env("ADDRESS", "127.0.0.1")
        .env("SPOOL_DIR", &spool)
        .env("BOSS_OPS_DIR", &ops)
        .output()
        .expect("observe-host runs");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let spooled: Vec<_> = std::fs::read_dir(&spool)
        .unwrap_or_else(|e| panic!("the observation was spooled ({e}): {text}"))
        .filter_map(Result::ok)
        .collect();
    assert_eq!(spooled.len(), 1, "{text}");
    let body = std::fs::read_to_string(spooled[0].path()).expect("read");
    let v: serde_json::Value = serde_json::from_str(&body).expect("json");
    let creds = &v["nodes"][0]["ops_credentials"];
    assert_eq!(creds["dir"], ops.display().to_string(), "{body}");
    assert_eq!(
        creds["state"], "not ready: talosconfig:absent kubeconfig:absent",
        "{body}"
    );
}
