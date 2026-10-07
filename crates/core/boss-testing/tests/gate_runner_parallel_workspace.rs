//! Parallel gates are safe because the workspace is per-run — pin it.
//!
//! THE INCIDENT CLASS (packet 28de3845). Gates were strictly serial:
//! the runner Job mounted one shared RWO PVC as /gate-target, run.sh
//! wiped it per run, and `boss gate` refused a second launch. Nothing
//! but that refusal (and scheduling luck) stood between two concurrent
//! gates and 2026-08-24's crossed receipts — a receipt naming one
//! branch's head reported under another, all three results discarded.
//! On 2026-09-02 nine cars serialized through the one runner at ~14
//! minutes each; gating was the pipeline's bottleneck.
//!
//! The shape that ends it: /gate-target becomes a per-run emptyDir
//! (born with the pod, dies with it — isolation is structural, not
//! guarded), and the PVC survives only as the WARM SEED: a snapshot of
//! a target/ built at main's tip, copied into each run's workspace,
//! plus the crate cache. Every property that makes that safe and fast
//! is pinned here, because each one decays into a real, named incident
//! if it drifts:
//!
//! - workspace on a PVC again: crossed receipts (2026-08-24)
//! - seed mount gone: every gate cold, ~74G / 20+ min of rebuild
//!   (measured, boss-dev.yaml)
//! - crate cache per-run: verdicts bet on static.crates.io; a green
//!   branch was called red by the network on 2026-08-27
//! - unbounded workspace disk: "No space left on device" turned into
//!   fake code failures (2026-08-23)
//! - unlocked seed read/refresh: a torn seed — half-copied rlibs under
//!   fresh fingerprints, reds that are nobody's code
//!
//! Since backlog 52ea56ac (2026-09-30) the workspace is no longer an
//! emptyDir: the seed and every workspace moved to w-1's second NVMe as
//! ONE claim, because a reflink cannot cross two filesystems and the
//! kubelet puts every emptyDir on the install disk. Each pod mounts its
//! own `runs/<pod name>` directory of it, so the isolation is the same;
//! what an emptyDir did by dying with its pod, run.sh now does with a
//! liveness lock, an empty-on-exit and a sweep — pinned below, the
//! sweep by running it.

use boss_testing::repo_root;

fn read(rel: &str) -> String {
    let path = repo_root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

fn manifest() -> String {
    read("infra/gate-runner/gate-runner.yaml")
}

fn run_sh() -> String {
    read("infra/gate-runner/run.sh")
}

/// run.sh with comment lines dropped — the pins below are about what
/// the script DOES, and a comment explaining the old world must stay
/// legal prose (same rule as run_sh_verdict.rs).
fn printed_run_sh() -> String {
    run_sh()
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The Job document out of the multi-doc manifest.
fn job_doc() -> String {
    manifest()
        .split("\n---")
        .find(|doc| doc.contains("kind: Job"))
        .expect("gate-runner.yaml carries a Job document")
        .to_string()
}

/// The volume name mounted at `mount_path` in the gate container,
/// read from the inline mount style the manifest uses
/// (`- {name: x, mountPath: /y}`).
fn volume_mounted_at(job: &str, mount_path: &str) -> Option<String> {
    job.lines()
        .filter(|l| l.contains("mountPath:"))
        .find(|l| {
            l.split("mountPath:")
                .nth(1)
                .map(|rest| {
                    rest.trim_start()
                        .trim_end_matches('}')
                        .split([',', ' '])
                        .next()
                        == Some(mount_path)
                })
                .unwrap_or(false)
        })
        .and_then(|l| {
            let after = l.split("name:").nth(1)?;
            Some(
                after
                    .trim_start()
                    .split([',', '}'])
                    .next()?
                    .trim()
                    .to_string(),
            )
        })
}

/// What backs a named volume in the Job's `volumes:` list.
fn volume_backing(job: &str, name: &str) -> String {
    let mut in_entry = false;
    for line in job.lines() {
        let t = line.trim_start();
        if t.starts_with("- name:") {
            in_entry = t.split(':').nth(1).map(str::trim) == Some(name);
            continue;
        }
        if in_entry {
            if t.starts_with("emptyDir") {
                return "emptyDir".into();
            }
            if t.starts_with("persistentVolumeClaim") {
                return "persistentVolumeClaim".into();
            }
            if t.starts_with("configMap") || t.starts_with("secret") {
                return "other".into();
            }
        }
    }
    format!("volume `{name}` not found in the Job's volumes list")
}

/// The whole inline mount entry for `mount_path` in the gate container.
fn mount_line(job: &str, mount_path: &str) -> Option<String> {
    let name = volume_mounted_at(job, mount_path)?;
    job.lines()
        .map(str::trim)
        .find(|l| {
            l.contains(&format!("name: {name},"))
                && l.contains(&format!("mountPath: {mount_path},"))
        })
        .map(str::to_string)
}

/// THE ISOLATION PIN. Two gates that can see each other's workspace
/// cross their receipts; two that cannot are safe by construction.
///
/// Since backlog 52ea56ac the workspace is no longer an emptyDir: it
/// must sit on the SAME filesystem as the seed (w-1's second NVMe, the
/// `gate` claim) or the seeding copy stops being a reflink — and the
/// kubelet puts every emptyDir on the install disk's EPHEMERAL. So the
/// workspace is the claim again, but each pod mounts its OWN directory
/// of it at /gate-target, named by the pod's own name through the
/// downward API. Two gates write two directories.
///
/// ONE KNOWN EXCEPTION (review 0b9c02f1, F2): the gate container also
/// mounts the claim's `runs/` directory at /gate-runs, read-write, for
/// the sweep and the proof marker, so code running in a gate CAN reach
/// concurrent runs' workspaces. The seed was already shared read-write,
/// so this widens an existing exposure rather than opening a new class.
/// The structural fix is backlog 5e77f216. This pin names the exception
/// by listing EVERY mount of the claim, so a second shared mount cannot
/// arrive silently.
#[test]
fn the_workspace_is_per_run_and_shares_the_seeds_filesystem() {
    let job = job_doc();

    let workspace = volume_mounted_at(&job, "/gate-target")
        .expect("the gate container must mount a workspace at /gate-target");
    let seed = volume_mounted_at(&job, "/gate-seed")
        .expect("the gate container must mount the warm seed at /gate-seed");
    assert_eq!(
        workspace, seed,
        "the workspace and the seed must be ONE volume — a reflink cannot cross two \
         filesystems, and without it every gate rewrites the whole seed (5b3dabb5)"
    );
    assert_eq!(
        volume_backing(&job, &seed),
        "persistentVolumeClaim",
        "the seed must OUTLIVE the pod — an emptyDir seed is empty by definition, \
         and every gate goes cold: ~74G of target rebuilt, 20+ minutes each."
    );
    let ws = mount_line(&job, "/gate-target").expect("inline workspace mount");
    assert!(
        ws.contains("subPathExpr: runs/$(POD_NAME)"),
        "/gate-target must be the pod's OWN directory of the claim. Shared, two \
         concurrent gates yank the tree from under each other and write one receipt \
         path — the 2026-08-24 crossed-receipts incident, made possible again: {ws}"
    );
    assert!(
        job.contains("- {name: POD_NAME, valueFrom: {fieldRef: {fieldPath: metadata.name}}}"),
        "POD_NAME must be the pod's own name (downward API) — a name two pods can \
         share is a shared workspace"
    );
    let sd = mount_line(&job, "/gate-seed").expect("inline seed mount");
    assert!(sd.contains("subPath: seed"), "{sd}");
    let claim_mounts: Vec<&str> = job
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with(&format!("- {{name: {seed},")) && l.contains("mountPath:"))
        .collect();
    assert_eq!(
        claim_mounts,
        vec![
            "- {name: gate-disk, mountPath: /gate-seed, subPath: seed}",
            "- {name: gate-disk, mountPath: /gate-target, subPathExpr: runs/$(POD_NAME)}",
            "- {name: gate-disk, mountPath: /gate-runs, subPath: runs}",
        ],
        "the gate claim is mounted exactly three ways: the shared seed, this pod's own \
         workspace, and /gate-runs — the ONE known exception to per-pod isolation \
         (review 0b9c02f1 F2, structural fix 5e77f216). A new shared mount must be \
         argued here, not added silently."
    );
}

/// THE FLOOR READS THE WORKSPACE DISK. It is NOT a per-run bound
/// (renamed from the_workspace_disk_is_bounded, review 0b9c02f1 F4):
/// that bound was the emptyDir's sizeLimit, and a local volume has no
/// quota, so a single run can now fill the gate disk (C4, 8d5b997c).
/// What this pins is the start-time floor. gate.sh's disk floor reads
/// the filesystem the clone sits on, so a gate on a short gate disk is
/// REFUSED before any check runs rather than reading ENOSPC as a red
/// (2026-08-23: shared-disk exhaustion read as code failures). The
/// install disk is out of a runaway gate's reach; the gate disk is not.
#[test]
fn the_floor_reads_the_workspace_disk() {
    let sh = printed_run_sh();
    let gate_sh = read("infra/gate.sh");
    assert!(
        gate_sh.contains("df -Pk ."),
        "gate.sh's disk floor reads the filesystem it runs in"
    );
    let cd = sh
        .find("cd /gate-target/repo")
        .expect("run.sh runs the gate from the clone in /gate-target");
    let gate = sh
        .find("./infra/gate.sh")
        .expect("run.sh runs infra/gate.sh");
    assert!(
        cd < gate,
        "the floor must read the workspace's disk, so the gate runs from inside it"
    );
    assert!(
        job_doc().contains("ephemeral-storage"),
        "the gate container must still request ephemeral-storage for what it keeps \
         on the install disk (its writable layer, /tmp, the pgdata emptyDir)"
    );
}

/// w-1's allocatable ephemeral-storage in GiB, MEASURED rather than
/// estimated (backlog e6dc7331, 2026-09-30). The dev session cannot get
/// nodes, so it is read off the kubelet's own words: every w-1 eviction
/// the estate observer recorded names "Threshold quantity: 149611037292"
/// — the 15% nodefs line, 139.3 GiB — so nodefs is 997,406,915,280 bytes
/// (928.9 GiB, the estate registry's 929), and capacity less that line
/// is 789.6 GiB; 789 leaves the kubelet's system reservation its room.
const W1_ALLOCATABLE_EPHEMERAL_GIB: f64 = 789.0;

/// A `requests:`/`limits:` quantity in GiB (`150Gi`, `64Mi`) — the two
/// units these manifests use; any other is a red naming it, never a 0.
fn gib(quantity: &str) -> f64 {
    let q = quantity.trim().trim_end_matches('}').trim();
    if let Some(n) = q.strip_suffix("Gi") {
        n.parse().unwrap_or_else(|e| panic!("quantity {q}: {e}"))
    } else if let Some(n) = q.strip_suffix("Mi") {
        n.parse::<f64>()
            .unwrap_or_else(|e| panic!("quantity {q}: {e}"))
            / 1024.0
    } else {
        panic!("an ephemeral-storage quantity in a unit this pin does not read: {q}")
    }
}

/// Every `ephemeral-storage:` REQUEST in a manifest, in GiB, one per
/// requesting container.
fn ephemeral_requests(doc: &str) -> Vec<f64> {
    doc.lines()
        .map(str::trim_start)
        .filter(|l| l.starts_with("requests:") && l.contains("ephemeral-storage"))
        .map(|l| {
            let q = l
                .split("ephemeral-storage:")
                .nth(1)
                .expect("the filter guarantees the key");
            gib(q.split(',').next().unwrap_or(q))
        })
        .collect()
}

/// THE REQUEST IS WHAT A NEW-LAYOUT GATE USES, AND THE POLICY'S BAYS
/// FIT BESIDE THE DEV POD (backlog e6dc7331, delivery-policy v5).
///
/// History: pinned equal to the workspace emptyDir's 160Gi sizeLimit
/// after w-1 evicted a running gate on 2026-09-29 22:30:29Z (backlog
/// 461159e7: 5 x 90 + 154 admitted, worst case 954 of ~790), then HELD
/// at 160 once the workspace left EPHEMERAL for the gate disk (52ea56ac),
/// on purpose, to keep v4's three bays until it was measured.
///
/// Measured 2026-09-30 on w-1's nodefs (the estate observer's 15-minute
/// talos-nodefs series) across the first five new-layout gates, 21:03Z
/// to 22:37Z: free read 494 and 491 GB with no gate running and 492,
/// 501, 489 and 486 with one or two running, 4 to 18 minutes in — the
/// largest drop, two gates against the no-gate reading, is 8 GB. The
/// old layout swung 506 -> 391 the same afternoon. The car set 20Gi
/// from that; review be559bda raised it to 40Gi: the 8 GB is inside
/// the samples' noise (one gate-running sample read more free than
/// both idle ones, and no full train gate's peak is among them), and
/// the pgdata emptyDir and /tmp both sit unmeasured under this one
/// request. 40Gi costs nothing against 789 GiB and keeps an ordinary
/// gate out of the "exceeds request" eviction class until 943f1065
/// records a measured peak to set it from.
///
/// The arithmetic is pinned, not narrated: the policy's bays PLUS the
/// train gate, which launches outside the bay bound (0f4224bc), at this
/// request, beside every ephemeral request the dev pod makes, must fit
/// the measured allocatable. A future raise of either number fails here
/// naming the sum, instead of a gate sitting Pending on w-1.
#[test]
fn a_gate_requests_what_the_new_layout_uses_and_the_bays_fit() {
    let job = job_doc();
    let gate = ephemeral_requests(&job);
    assert_eq!(
        gate.len(),
        1,
        "exactly one container (gate) should request ephemeral-storage: {gate:?}"
    );
    assert!(
        (gate[0] - 40.0).abs() < f64::EPSILON,
        "the gate's ephemeral request is 40Gi (backlog e6dc7331, review be559bda): \
         the ~8 GB new-layout reading is inside its noise and pgdata and /tmp are \
         unmeasured; read {} GiB. Move it only with a measured peak (943f1065), \
         and the bays with it",
        gate[0]
    );
    assert!(
        !job.contains("name: gate-workspace"),
        "the emptyDir workspace is gone; a request pinned to it would pin nothing"
    );

    let policy = read("infra/platform/delivery-policy/train-conductor.toml");
    let bays: f64 = policy
        .lines()
        .find_map(|l| l.trim().strip_prefix("gate_max_concurrent ="))
        .expect("train-conductor.toml declares gate_max_concurrent")
        .trim()
        .parse()
        .expect("gate_max_concurrent is a number");
    let dev: f64 = ephemeral_requests(&read("infra/cluster/manifests/boss-dev.yaml"))
        .iter()
        .sum();
    assert!(
        dev > 0.0,
        "the dev pod's ephemeral requests were read (boss-dev.yaml)"
    );
    let admitted = (bays + 1.0) * gate[0] + dev;
    assert!(
        admitted <= W1_ALLOCATABLE_EPHEMERAL_GIB,
        "{bays} bays + the train gate at {} GiB each, beside the dev pod's {dev:.1} GiB, \
         is {admitted:.1} GiB of w-1's {W1_ALLOCATABLE_EPHEMERAL_GIB} allocatable: the \
         last gate would sit Pending and read as a running slot",
        gate[0]
    );
}

/// The sweep function out of run.sh, between its markers, so it is RUN
/// rather than read.
fn sweep_fn() -> String {
    let sh = run_sh();
    let start = sh
        .find("# --- workspace-sweep (begin) ---")
        .expect("run.sh marks its workspace sweep");
    let end = sh
        .find("# --- workspace-sweep (end) ---")
        .expect("run.sh marks the end of its workspace sweep");
    sh[start..end].to_string()
}

/// A DEAD GATE'S WORKSPACE IS SWEPT, A LIVE ONE NEVER IS. An emptyDir
/// died with its pod; a directory of the gate claim does not, so a gate
/// killed mid-run (activeDeadlineSeconds, an eviction, w-1 resetting)
/// would leave ~100G behind for ever. Every gate therefore sweeps the
/// runs directory at start, and the rule is a LOCK, not an age guess: a
/// live run holds a shared flock on its own `.alive` for its whole life,
/// so an exclusive, non-blocking flock succeeds only on a dead one. A
/// directory written to in the last hour is spared too, which covers
/// the few seconds between the kubelet creating a new pod's directory
/// and its run.sh taking the lock, and a run under a script older than
/// the lock (the runner car's own gate runs the previous ConfigMap).
#[test]
fn the_sweep_removes_dead_workspaces_and_spares_live_ones() {
    use std::process::{Command, Stdio};
    let dir = boss_testing::scratch_dir("gate-workspace-sweep");
    let runs = dir.join("runs");
    for d in [
        "dead",
        "live",
        "fresh",
        "own",
        "unscripted-busy",
        "unscripted-dead",
    ] {
        std::fs::create_dir_all(runs.join(d).join("target")).unwrap();
        std::fs::write(runs.join(d).join("target/blob"), b"x").unwrap();
    }
    for d in ["dead", "live", "fresh", "own"] {
        std::fs::write(runs.join(d).join(".alive"), b"").unwrap();
    }
    std::fs::write(runs.join("unscripted-busy").join("gate.log"), b"started\n").unwrap();
    // Everything two hours old except `fresh` — every entry at the depth
    // the sweep reads, so nothing is fresh by accident.
    for d in ["dead", "live", "own", "unscripted-busy", "unscripted-dead"] {
        let entries: Vec<_> = std::fs::read_dir(runs.join(d))
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        let status = Command::new("touch")
            .args(["-d", "2 hours ago"])
            .arg(runs.join(d))
            .args(&entries)
            .status()
            .unwrap();
        assert!(status.success());
    }
    // A run under the previous script holds no lock, but a running gate
    // APPENDS to its log — which moves the log's mtime and not the
    // directory's.
    {
        use std::io::Write;
        let mut log = std::fs::OpenOptions::new()
            .append(true)
            .open(runs.join("unscripted-busy").join("gate.log"))
            .unwrap();
        log.write_all(b"still going\n").unwrap();
    }
    // `live` holds its lock the way run.sh does, for the test's life.
    let mut holder = Command::new("flock")
        .arg("-s")
        .arg(runs.join("live").join(".alive"))
        .args(["sleep", "30"])
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(300));

    let script = format!(
        "set -euo pipefail\n{}\nsweep_dead_workspaces \"$1\" own\n",
        sweep_fn()
    );
    let out = Command::new("bash")
        .args(["-c", &script, "sweep"])
        .arg(&runs)
        .output()
        .unwrap();
    let _ = holder.kill();
    let _ = holder.wait();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{text}");
    assert!(!runs.join("dead").exists(), "a dead run is swept:\n{text}");
    assert!(
        !runs.join("unscripted-dead").exists(),
        "a dead run under the previous script, which never took the lock, is swept:\n{text}"
    );
    for kept in ["live", "fresh", "own", "unscripted-busy"] {
        assert!(
            runs.join(kept).join("target/blob").exists(),
            "{kept} must survive the sweep:\n{text}"
        );
    }
    assert!(
        text.contains("swept 2 dead gate workspace(s)"),
        "the sweep says what it removed:\n{text}"
    );
}

/// The gate-disk guard out of run.sh, between its markers.
fn guard_fn() -> String {
    let sh = run_sh();
    let start = sh
        .find("# --- gate-disk guard (begin) ---")
        .expect("run.sh marks its gate-disk guard");
    let end = sh
        .find("# --- gate-disk guard (end) ---")
        .expect("run.sh marks the end of its gate-disk guard");
    sh[start..end].to_string()
}

/// Run the guard with a GATE_DISK value, four paths and a POD_NAME;
/// (exit status, output).
fn guard(
    mode: &str,
    runs: &std::path::Path,
    seed: &str,
    target: &str,
    ephemeral: &str,
    pod: &str,
) -> (i32, String) {
    let script = format!(
        "set -euo pipefail\n{}\ngate_disk_guard \"$1\" \"$2\" \"$3\" \"$4\" \"$5\" \"$6\"\n",
        guard_fn()
    );
    let out = std::process::Command::new("bash")
        .args(["-c", &script, "guard", mode])
        .arg(runs)
        .args([seed, target, ephemeral, pod])
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// A GATE ON THE INSTALL DISK REFUSES; IT DOES NOT RUN (review a79746c6,
/// finding 1). Once the gate volume has ever mounted, its mountpoint
/// directory stays on EPHEMERAL, so a boot without the disk leaves a
/// path the local PV mounts happily, and every gate would write its seed
/// and workspace to the install disk — xfs, reflinks, green — with
/// nothing evicting, because writes through a PV are not the pod's
/// ephemeral storage. So the runner compares devices before it writes
/// anything: /gate-target on the same device as the pod's kubelet-made
/// /etc/hosts (EPHEMERAL) refuses, and so does a seed and workspace on
/// two devices (no reflink).
///
/// The guard is keyed DELIBERATELY (review 635317c5, C1 and C3): the
/// manifest sets GATE_DISK=required beside the claim mount, and the gate
/// disk must carry the positive marker /gate-runs/.gate-volume that the
/// proof Job writes once — the empty EPHEMERAL fallback directory never
/// carries it. It used to switch itself off whenever /gate-runs was not
/// mounted, so a later car dropping that mount would have silently
/// disarmed it. Now: GATE_DISK=required with no runs directory, or no
/// marker, refuses; and the gate layout mounted WITHOUT the key refuses
/// too. Only a pod with neither — the OLD manifest, on EPHEMERAL by
/// design — passes untouched.
///
/// And the workspace must be THIS POD'S OWN directory inside the marked
/// runs directory (review 0b9c02f1, F3): POD_NAME set, and /gate-target
/// the same inode on the same device as /gate-runs/$POD_NAME. That ties
/// the marker to the workspace — a /gate-runs from the gate disk beside
/// a workspace from some other volume refuses — and catches a dropped
/// POD_NAME, which the kubelet would leave as a literal `$(POD_NAME)`
/// directory every pod shares.
#[test]
fn a_gate_whose_disk_is_the_install_disk_refuses() {
    let dir = boss_testing::scratch_dir("gate-disk-guard");
    let runs = dir.join("runs");
    let seed = dir.join("seed");
    let target = runs.join("gate-pod-a");
    let foreign = dir.join("some-other-volume");
    for d in [&runs, &seed, &target, &foreign] {
        std::fs::create_dir_all(d).unwrap();
    }
    let unmarked = dir.join("runs-unmarked");
    std::fs::create_dir_all(&unmarked).unwrap();
    std::fs::write(runs.join(".gate-volume"), b"proven\n").unwrap();
    let on_scratch = dir.join("hosts");
    std::fs::write(&on_scratch, b"127.0.0.1 localhost\n").unwrap();
    let (seed, target, foreign, same_fs) = (
        seed.to_str().unwrap(),
        target.to_str().unwrap(),
        foreign.to_str().unwrap(),
        on_scratch.to_str().unwrap(),
    );
    let pod = "gate-pod-a";
    // /proc is a filesystem of its own: an "EPHEMERAL" the gate is not on.
    let elsewhere = "/proc/version";

    let (rc, out) = guard("required", &runs, seed, target, elsewhere, pod);
    assert_eq!(
        rc, 0,
        "seed and this pod's own workspace together, off EPHEMERAL, marked:\n{out}"
    );

    let (rc, out) = guard("required", &runs, seed, target, same_fs, pod);
    assert_eq!(rc, 1, "{out}");
    assert!(
        out.contains("REFUSED") && out.contains("EPHEMERAL") && out.contains("install disk"),
        "the refusal says the gate is on the install disk:\n{out}"
    );

    let (rc, out) = guard("required", &runs, seed, "/proc", elsewhere, pod);
    assert_eq!(rc, 1, "{out}");
    assert!(
        out.contains("REFUSED") && out.contains("different filesystems"),
        "{out}"
    );

    let (rc, out) = guard("required", &unmarked, seed, target, elsewhere, pod);
    assert_eq!(rc, 1, "{out}");
    assert!(
        out.contains("REFUSED") && out.contains(".gate-volume"),
        "a gate disk nobody proved carries no marker:\n{out}"
    );

    let (rc, out) = guard("required", &runs, seed, target, elsewhere, "");
    assert_eq!(rc, 1, "no POD_NAME:\n{out}");
    assert!(out.contains("REFUSED") && out.contains("POD_NAME"), "{out}");

    let (rc, out) = guard("required", &runs, seed, foreign, elsewhere, pod);
    assert_eq!(
        rc, 1,
        "a workspace that is not /gate-runs/$POD_NAME:\n{out}"
    );
    assert!(
        out.contains("REFUSED") && out.contains("own directory"),
        "{out}"
    );

    let missing = dir.join("no-runs-mount");
    let (rc, out) = guard("required", &missing, seed, target, elsewhere, pod);
    assert_eq!(rc, 1, "GATE_DISK=required without /gate-runs:\n{out}");
    assert!(out.contains("REFUSED"), "{out}");

    let (rc, out) = guard("", &runs, seed, target, elsewhere, pod);
    assert_eq!(rc, 1, "the gate layout mounted without the key:\n{out}");
    assert!(
        out.contains("REFUSED") && out.contains("GATE_DISK"),
        "{out}"
    );

    let (rc, out) = guard("", &missing, seed, target, same_fs, "");
    assert_eq!(
        rc, 0,
        "the old manifest sets no GATE_DISK, mounts no /gate-runs, and is on EPHEMERAL by design:\n{out}"
    );
}

/// C1's manifest half: a runner that mounts claim `gate` must set
/// GATE_DISK=required and mount /gate-runs, so dropping either is a red
/// here rather than a disarmed guard in production.
#[test]
fn a_runner_mounting_the_gate_claim_arms_the_guard() {
    let job = job_doc();
    assert!(
        job.contains("claimName: gate}"),
        "the shipped runner mounts the gate claim"
    );
    assert!(
        job.contains("- {name: GATE_DISK, value: required}"),
        "a runner mounting claim `gate` sets GATE_DISK=required"
    );
    let runs =
        mount_line(&job, "/gate-runs").expect("a runner mounting claim `gate` mounts /gate-runs");
    assert!(runs.contains("subPath: runs"), "{runs}");
}

/// The guard's refusal is a REFUSAL — the verdict gate.sh's disk floor
/// uses, which judges nothing about the branch — reported before the
/// run writes anything, not a log line nobody reads.
#[test]
fn the_gate_disk_guard_reports_refused_before_any_write() {
    let sh = printed_run_sh();
    let call = sh
        .find("gate_disk_guard \"${GATE_DISK:-}\" /gate-runs /gate-seed /gate-target /etc/hosts \"${POD_NAME:-}\"")
        .expect("run.sh runs the guard on its own mounts and its own pod name");
    let refused = sh[call..]
        .find("report refused")
        .map(|i| i + call)
        .expect("a refusal is reported as `refused`");
    // THE RECEIPT IS THE REFUSAL SHAPE, not prose (review 0b9c02f1, F1):
    // {"verdict":"refused","refused_because":...}, the shape gate.sh's
    // disk floor and gate.rs's launch refusals write, so
    // train_gate::standing and `boss gate --wait` carry the reason
    // instead of "no reason recorded". A verdict must name what failed.
    let line = sh[refused..].lines().next().unwrap();
    assert!(
        line.contains("refused_because") && line.contains("jq -nc"),
        "the guard's refusal receipt must be the structured refusal shape: {line}"
    );
    let out = std::process::Command::new("bash")
        .args([
            "-c",
            "GATE_DISK_REFUSAL='REFUSED: example'; jq -nc --arg w \"gate disk: $GATE_DISK_REFUSAL\" '{verdict:\"refused\",refused_because:$w}'",
        ])
        .output()
        .unwrap();
    let receipt = String::from_utf8_lossy(&out.stdout);
    assert!(
        receipt.trim()
            == r#"{"verdict":"refused","refused_because":"gate disk: REFUSED: example"}"#,
        "the receipt parses as the refusal shape: {receipt}"
    );
    assert!(
        line.contains(r#"jq -nc --arg w "gate disk: $GATE_DISK_REFUSAL" '{verdict:"refused",refused_because:$w}'"#),
        "run.sh builds exactly that receipt: {line}"
    );
    let seed = sh.find("seed_target /gate-target/target").expect("seeding");
    let sweep = sh
        .find("sweep_dead_workspaces /gate-runs")
        .expect("the sweep");
    assert!(
        refused < seed && call < sweep,
        "the guard runs before the sweep and before the seed copy"
    );
}

/// A run holds its liveness lock from the start and empties its own
/// workspace on the way out — the ~100G a finished gate leaves is freed
/// at once rather than at the next gate's sweep.
#[test]
fn a_run_holds_its_liveness_lock_and_empties_its_workspace_at_exit() {
    let sh = printed_run_sh();
    let lock = sh
        .find("flock -s 8")
        .expect("run.sh holds a shared lock on /gate-target/.alive for its life");
    let clone = sh.find("git clone").expect("run.sh clones");
    assert!(lock < clone, "the lock is taken before any work");
    assert!(
        sh.contains("exec 8>>/gate-target/.alive"),
        "the lock lives on the workspace's own .alive file"
    );
    // ONLY on the gate-volume layout (review 635317c5, C2): under the
    // pre-seed manifest /gate-target is the shared PVC whose `cargo/`
    // crate cache the skew guard deliberately keeps, and an unkeyed EXIT
    // trap emptying /gate-target would delete it on every run.
    let exit_traps: Vec<_> = sh
        .lines()
        .filter(|line| line.trim_start().starts_with("trap ") && line.ends_with(" EXIT"))
        .collect();
    assert_eq!(
        exit_traps.len(),
        1,
        "the workspace is emptied at exit, from one place"
    );
    let exit_trap = exit_traps[0].trim();
    assert_eq!(
        exit_trap, "trap 'retain_runtime_raw; empty_workspace' EXIT",
        "the single EXIT trap retains the raw journal before emptying the workspace"
    );
    let execution = std::process::Command::new("bash")
        .args([
            "-c",
            &format!(
                "retain_runtime_raw() {{ printf 'retain\\n'; }}\nempty_workspace() {{ printf 'cleanup\\n'; }}\n{exit_trap}\n"
            ),
        ])
        .output()
        .unwrap();
    assert!(execution.status.success());
    assert_eq!(execution.stdout, b"retain\ncleanup\n");
    let key = sh
        .find("if [ \"${GATE_DISK:-}\" = required ]; then")
        .expect("the gate-volume layout is keyed on GATE_DISK=required");
    let trap = sh.find(exit_trap).unwrap();
    let block_end = sh[key..].find("\nfi").map(|i| i + key).unwrap();
    assert!(
        key < trap && trap < block_end,
        "the EXIT trap is installed only inside the GATE_DISK=required block"
    );
    let sweep = sh
        .find("sweep_dead_workspaces /gate-runs")
        .expect("run.sh sweeps the runs directory");
    assert!(
        sweep < clone,
        "the sweep frees dead workspaces before this run fills its own"
    );
}

/// Concurrent Jobs must be tellable apart in `kubectl get jobs` — a
/// refusal that names `gate-8kx2p` three times names nothing.
#[test]
fn concurrent_jobs_carry_the_branch_in_name_and_label() {
    let job = job_doc();
    assert!(
        job.contains("generateName: gate-$GATE_NAME_HINT-"),
        "the Job name must carry the branch hint (gate-<branch>-<rand>); \
         `boss gate` fills $GATE_NAME_HINT when it renders the manifest"
    );
    assert!(
        job.contains("boss.dev/branch: $GATE_NAME_HINT"),
        "a branch label makes `kubectl get jobs -l boss.dev/branch=...` answer \
         which gate is whose without parsing generated names"
    );
}

/// THE CRATE CACHE SURVIVES THE RUN — on the seed volume, not the
/// per-run workspace. This is a correctness fix before a speed one:
/// with a per-run CARGO_HOME every gate re-downloads every dependency,
/// and on 2026-08-27 a green branch was recorded as a clippy FAILURE
/// because static.crates.io dropped one fetch of crc32fast.
#[test]
fn the_crate_cache_lives_on_the_seed_volume() {
    let sh = printed_run_sh();
    assert!(
        sh.contains("/gate-seed/cargo")
            || sh.contains("$SEED/cargo")
            || sh.contains("${SEED}/cargo"),
        "CARGO_HOME must point at the seed volume so the registry cache outlives \
         the pod — a per-run cache bets every verdict on several hundred \
         consecutive crates.io fetches"
    );
}

/// A stale checkout renders the OLD manifest (no /gate-seed mount)
/// against the NEW ConfigMap script. That skew must cost speed, not
/// gates: run.sh degrades to a cold, per-run cache and says so.
#[test]
fn a_missing_seed_mount_degrades_to_cold_not_dead() {
    let sh = printed_run_sh();
    assert!(
        sh.contains(r#"[ -d "$SEED" ]"#),
        "run.sh must probe whether /gate-seed is mounted before using it"
    );
    assert!(
        sh.contains("/gate-target/cargo"),
        "the no-seed fallback must keep a usable CARGO_HOME on the workspace — \
         cold and loud beats dead"
    );
}

/// SEED READS AND SEED WRITES ARE LOCK-DISCIPLINED. Readers copy under
/// a shared flock; the refresher rewrites under an exclusive,
/// non-blocking one. Without this, a refresh racing a seeding copy
/// hands the new gate half-copied rlibs under fresh-looking
/// fingerprints — reds that are nobody's code, the exact class the
/// gate exists to never produce.
#[test]
fn seed_copy_and_refresh_take_opposite_locks() {
    let sh = printed_run_sh();
    assert!(
        sh.contains("flock -s"),
        "the seed copy must hold a SHARED lock — concurrent readers are fine, \
         a reader racing the refresher is not"
    );
    assert!(
        sh.contains("flock -x -n"),
        "the refresh must hold an EXCLUSIVE lock and skip when busy (-n): \
         refreshing is best-effort housekeeping, blocking a verdict is not its job"
    );
}

/// The refresh stages into target.partial and renames. A pod that dies
/// mid-refresh (w-1 has reset mid-gate before) must leave a MISSING
/// seed — the next gate runs cold, slow and correct — never a torn one.
#[test]
fn the_refresh_stages_then_renames_so_death_leaves_cold_not_torn() {
    let sh = printed_run_sh();
    assert!(
        sh.contains("target.partial"),
        "the refresh must copy into a staging dir, not into the live seed path"
    );
    let stage = sh.find("target.partial").expect("staging dir present");
    let swap = sh
        .rfind("mv \"$SEED/target.partial\" \"$SEED/target\"")
        .expect("the staged copy must be renamed into place as the last step");
    assert!(
        stage < swap,
        "staging must happen before the rename that publishes it"
    );
}

/// The seed refreshes only from a GREEN run at/near main's tip.
/// Refresh from a red run and the seed inherits a broken tree's
/// artifacts; refresh from a stale-based branch and every later gate
/// rebuilds the distance to main anyway.
#[test]
fn the_seed_refreshes_only_on_a_green_near_tip_run() {
    let sh = printed_run_sh();
    assert!(
        sh.contains(r#"[ "$VERDICT" = "green" ]"#),
        "the refresh must be gated on the verdict being green"
    );
    assert!(
        sh.contains("rev-list --count HEAD..origin/main"),
        "the refresh must measure distance to origin/main — 'near the tip' is a \
         count, not a feeling"
    );
}

/// THE SEED COPY IS A REFLINK (backlog 5b3dabb5). Every gate copied the
/// warm target from a Longhorn volume with a plain `cp -a` — 184 s and
/// tens of GB written per gate, the single largest writer on w-1's SSD
/// (~3.6 TB/day). Both copies — seeding the workspace and refreshing
/// the seed — must ask for a reflink, and the seed must live where a
/// reflink can happen: a `local` PV on the build node's own filesystem,
/// bound by name so a typo binds nothing rather than the wrong disk.
/// Three files state one arrangement; this pins them to each other.
#[test]
fn the_seed_is_copied_by_reflink_from_a_local_volume_on_the_build_node() {
    let sh = run_sh();
    let seeding = sh
        .lines()
        .filter(|l| l.contains("cp -a") && l.contains("$SEED/target/."))
        .count();
    let refreshing = sh
        .lines()
        .filter(|l| l.contains("cp -a") && l.contains("$SEED/target.partial"))
        .count();
    assert_eq!(seeding, 1, "one seeding copy in run.sh");
    assert_eq!(refreshing, 1, "one refresh copy in run.sh");
    for l in sh
        .lines()
        .filter(|l| l.contains("cp -a") && l.contains("$SEED/target"))
    {
        assert!(
            l.contains("--reflink=auto"),
            "a seed copy without --reflink=auto rewrites the whole target: {l}"
        );
    }

    let job = job_doc();
    assert_eq!(volume_backing(&job, "gate-disk"), "persistentVolumeClaim");
    assert!(
        job.contains("persistentVolumeClaim: {claimName: gate}"),
        "the seed and the workspaces must be the gate-volume claim `gate` (w-1's \
         second NVMe, 52ea56ac) — not the EPHEMERAL seed, not the Longhorn one"
    );
    assert!(
        !job.contains("claimName: gate-runner-disk") && !job.contains("claimName: gate-seed}"),
        "the Job must no longer mount the Longhorn claim or the EPHEMERAL seed"
    );

    // The claim is CONVERGED with its PV (infra/cluster/manifests), not
    // declared beside the Job: nothing applies gate-runner.yaml's
    // claims — `boss gate` creates only the Job.
    let local = read("infra/cluster/manifests/gate-seed-local.yaml");
    let pvc = local
        .split(
            "
---
",
        )
        .find(|d| {
            d.contains("kind: PersistentVolumeClaim")
                && d.contains(
                    "name: gate
",
                )
        })
        .expect("the gate claim is declared beside its PV");
    assert!(pvc.contains("storageClassName: gate-seed-local"), "{pvc}");
    assert!(pvc.contains("volumeName: gate-w-1"), "{pvc}");
    assert!(pvc.contains("namespace: boss-dev"), "{pvc}");
    assert!(
        local.contains("name: gate-seed-local")
            && local.contains("provisioner: kubernetes.io/no-provisioner")
    );
    assert!(
        local.contains("name: gate-w-1") && local.contains("storageClassName: gate-seed-local")
    );
    assert!(
        local.contains("path: /var/mnt/gate\n"),
        "the PV's path: the gate user volume's mountpoint"
    );
    // The directory is the NODE's declaration (Talos machine.files on
    // w-1), not a CronJob's hostPath mount: boss-dev enforces the Talos
    // default `baseline`, which refuses hostPath — measured on
    // 2026-09-12 (backlog d42d4967) when the prepare Job created no pod
    // in ten minutes while its emptyDir twin completed in twenty
    // seconds. A manifest that reintroduces the mount is refused by
    // infra/lint/no-manifest-mounts-a-hostpath.sh; this pins the
    // manifest's own account of where the directory comes from.
    assert!(
        !local.contains("kind: CronJob") && !local.contains("hostPath:"),
        "nothing in the cluster may create the seed directory — baseline refuses hostPath"
    );
    assert!(
        local.contains("machine.files") && local.contains("talosctl"),
        "the manifest names the Talos declaration that creates the PV's path"
    );
    // The directory alone is not enough: the kubelet runs in its own
    // mount namespace and does not see /var/local unless the machine
    // config binds it in. With only machine.files applied, a pod
    // mounting the PVC sat nine hours at ContainerCreating while the
    // kubelet logged `path "/var/local/gate-seed" does not exist` 296
    // times, on a node where `talosctl ls` showed it (2026-09-12).
    assert!(
        local.contains("machine.kubelet.extraMounts"),
        "the manifest names the kubelet mount that lets the PV's path be seen"
    );
    assert!(
        !repo_root()
            .join("infra/platform/workflows/maintenance-gate-seed.toml")
            .exists(),
        "the prepare CronJob's workflow went with it"
    );
    assert!(
        local.contains(r#"operator: In, values: ["w-1"]"#),
        "the PV binds only on the build node"
    );
}
