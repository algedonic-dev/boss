//! `infra/cluster/dev-memory-watch.sh` — the dev pod's memory ring and
//! the alert a container restart raises from it.
//!
//! Measured 2026-10-08 (backlog 9ecd12ea): container `dev` was
//! OOMKilled at its 32Gi limit at 10:31:52Z, restarted four seconds
//! later, and nothing recorded either that it happened or what had
//! filled it — the cgroup's counters were recreated with the container
//! and no per-process reading from before the kill existed anywhere.
//! These pins drive the real script against a fixture process table, a
//! fixture pod status and a stub `curl`, and hold the three things the
//! next kill needs: a sample that names the largest processes by
//! container with nothing secret-shaped in it, ONE alert per restart
//! carrying the last sample from BEFORE the kill, and a loop that only
//! starts where it will outlive the container it watches.

use boss_testing::{repo_root, without_proxy};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Removes its directory on drop, so a panicking test leaves nothing.
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Kills the loop a test started, whatever the test's outcome.
struct Loop(u32);
impl Drop for Loop {
    fn drop(&mut self) {
        let _ = Command::new("kill").arg(self.0.to_string()).status();
    }
}

const DEV_ID: &str = "73eff21d23f725719f549df150b81af9d93d05a4042c7aa5b462a129bb29dd41";
const PG_ID: &str = "62725fc1bbfbef74a1c4f08dafbd84dce778a0158615caacecd1da9b3581003b";

/// One process in the fixture table, shaped as the script reads it:
/// `statm` (pages: size, resident, shared), `cgroup` (as another
/// container's process shows from inside a private cgroup namespace —
/// `0::/../<container id>`, or the bare root for the reader's own),
/// `comm`, and a NUL-separated `cmdline`.
fn process(proc: &Path, pid: u32, container: &str, resident: u64, shared: u64, argv: &[&str]) {
    let d = proc.join(pid.to_string());
    boss_testing::create_dir(&d);
    boss_testing::write_file(
        &d.join("statm"),
        &format!("{} {resident} {shared} 1 0 1 0\n", resident * 2),
    );
    let cgroup = if container.is_empty() {
        "0::/\n".to_string()
    } else {
        format!("0::/../{container}\n")
    };
    boss_testing::write_file(&d.join("cgroup"), &cgroup);
    let comm = argv[0].rsplit('/').next().unwrap_or("?");
    boss_testing::write_file(&d.join("comm"), &format!("{comm}\n"));
    boss_testing::write_file(&d.join("cmdline"), &format!("{}\0", argv.join("\0")));
}

/// The pod's status as the cluster API serves it, cut to what the
/// script reads. `killed` is the kubelet's record of the dev
/// container's last termination: (reason, exit code, finishedAt).
fn pod(dev_restarts: u32, killed: Option<(&str, i32, &str)>) -> String {
    let last = match killed {
        Some((reason, code, finished)) => serde_json::json!({"terminated": {
            "reason": reason, "exitCode": code, "finishedAt": finished}}),
        None => serde_json::json!({}),
    };
    serde_json::json!({
        "metadata": {"name": "boss-dev-test", "uid": "pod-uid-1"},
        "spec": {"containers": [
            {"name": "dev", "resources": {"limits": {"cpu": "16", "memory": "32Gi"}}},
            {"name": "postgres", "resources": {"limits": {"memory": "4Gi"}}},
        ]},
        "status": {"containerStatuses": [
            {"name": "dev", "restartCount": dev_restarts,
             "containerID": format!("containerd://{DEV_ID}"), "lastState": last},
            {"name": "postgres", "restartCount": 0,
             "containerID": format!("containerd://{PG_ID}"), "lastState": {}},
        ]},
    })
    .to_string()
}

/// A stub `curl`: the pod read is answered from `$STUB_POD` into the
/// `-o` file (absent, nothing answers: exit 7). A POST is the alert,
/// and `$STUB_SOR` says what the system of record does with it: `down`
/// answers nothing (exit 7, curl's own `000`); `refuse` answers 422 to
/// every body and `refuse:<needle>` only to a body containing the
/// needle; `answer:<code>` answers that code to every body; anything
/// else appends the body to `$STUB_POSTED` as one line
/// and answers 201. Any other read (the owner lookup) is an empty
/// roster.
fn stub_curl(root: &Path) -> PathBuf {
    let bin = root.join("bin");
    boss_testing::create_dir(&bin);
    boss_testing::write_exec(
        &bin.join("curl"),
        concat!(
            "#!/usr/bin/env bash\n",
            "out=/dev/null; url=; post=; data=\n",
            "while [ $# -gt 0 ]; do\n",
            "    case \"$1\" in\n",
            "        -o) out=$2; shift ;;\n",
            "        -X) [ \"$2\" = POST ] && post=1; shift ;;\n",
            "        --data-binary) data=${2#@}; shift ;;\n",
            // How the machine token's header reached this request: `file`
            // (a header FILE holding the line), `argv` (the line itself
            // on the command line), or not at all.
            "        -H)\n",
            "            case \"$2\" in\n",
            "                @*) grep -q '^x-boss-machine-token: ' \"${2#@}\" 2>/dev/null && mt=file ;;\n",
            "                [Xx]-[Bb]oss-[Mm]achine-[Tt]oken*) mt=argv ;;\n",
            "            esac\n",
            "            shift ;;\n",
            "        --max-time | --cacert | -w) shift ;;\n",
            "        http*) url=$1 ;;\n",
            "    esac\n",
            "    shift\n",
            "done\n",
            "echo \"${mt:-none} ${post:+POST }$url\" >> \"$STUB_POSTED.headers\"\n",
            "if [ -n \"$post\" ]; then\n",
            // How many samples the ring held when the alert was SENT:
            // the order a tick samples and sends in (delta review, N5).
            "    ls \"$STUB_RING\" 2>/dev/null | grep -c tsv >> \"$STUB_POSTED.ring\"\n",
            "    body=$(jq -c . \"$data\")\n",
            "    sor=$(cat \"$STUB_SOR\" 2>/dev/null)\n",
            "    case \"$sor\" in\n",
            "        down) printf 000; exit 7 ;;\n",
            "        refuse) refused=1 ;;\n",
            "        refuse:*) case \"$body\" in *\"${sor#refuse:}\"*) refused=1 ;; esac ;;\n",
            // An answer that judges the CALLER or the API, not the body.
            "        answer:*) echo '{\"error\":\"not this body\"}' > \"$out\"; printf '%s' \"${sor#answer:}\"; exit 0 ;;\n",
            "    esac\n",
            "    if [ -n \"${refused:-}\" ]; then\n",
            "        echo '{\"error\":\"unprocessable\"}' > \"$out\"; printf 422; exit 0\n",
            "    fi\n",
            "    printf '%s\\n' \"$body\" >> \"$STUB_POSTED\"\n",
            // The jobs API answers a create with the new packet's id; the
            // stub numbers them by the bodies it has taken.
            "    printf '{\"id\":\"item-%s\"}' \"$(grep -c . \"$STUB_POSTED\")\" > \"$out\"\n",
            "    printf 201\n",
            "    exit 0\n",
            "fi\n",
            "case \"$url\" in\n",
            "    */pods/*) [ -f \"$STUB_POD\" ] || exit 7; cat \"$STUB_POD\" > \"$out\" ;;\n",
            // A packet read by id: `$STUB_JOBS/<id>` is the packet, absent
            // it is a 404.
            "    */api/jobs/*)\n",
            "        id=${url##*/}\n",
            "        if [ -f \"$STUB_JOBS/$id\" ]; then cat \"$STUB_JOBS/$id\" > \"$out\"; printf 200; else printf 404; fi ;;\n",
            "    *) echo '[]' ;;\n",
            "esac\n",
        ),
    );
    bin
}

struct Fixture {
    root: PathBuf,
    proc: PathBuf,
    dir: PathBuf,
}

impl Fixture {
    fn new(root: &Path) -> Self {
        let proc = root.join("proc");
        let sa = root.join("sa");
        boss_testing::create_dir(&proc);
        boss_testing::create_dir(&sa);
        boss_testing::write_file(&sa.join("token"), "sa-token-for-the-fixture");
        boss_testing::write_file(&sa.join("namespace"), "boss-dev\n");
        boss_testing::write_file(&sa.join("ca.crt"), "");
        stub_curl(root);
        Fixture {
            root: root.to_path_buf(),
            proc,
            dir: root.join("work").join("telemetry").join("dev-memory"),
        }
    }

    fn set_pod(&self, doc: &str) {
        boss_testing::write_file(&self.root.join("pod.json"), doc);
    }

    fn run(&self, mode: &str, extra: &[(&str, &str)]) -> Output {
        let mut cmd = Command::new("bash");
        without_proxy(&mut cmd);
        cmd.arg(repo_root().join("infra/cluster/dev-memory-watch.sh"))
            .arg(mode)
            .env("WORK_MOUNT", self.root.join("work"))
            .env("PROC_ROOT", &self.proc)
            .env("BOSS_K8S_SA_DIR", self.root.join("sa"))
            .env("KUBERNETES_SERVICE_HOST", "k8s.test")
            .env("KUBERNETES_SERVICE_PORT", "443")
            .env("BOSS_POD_NAME", "boss-dev-test")
            .env("BOSS_JOBS_URL", "http://sor.test:7900")
            // No token and no rendered address in reach: the alert door
            // must work with neither, as it does in the sidecar.
            .env("BOSS_MACHINE_TOKEN_DIR", "/nonexistent/no-machine-token")
            .env("BOSS_SOR_ENV", "/nonexistent/no-sor.env")
            .env("BOSS_PLATFORM_OWNER", "")
            .env("STUB_POD", self.root.join("pod.json"))
            .env("STUB_POSTED", self.root.join("posted.txt"))
            .env("STUB_SOR", self.root.join("sor-state"))
            .env("STUB_RING", self.dir.join("ring"))
            .env("STUB_JOBS", self.root.join("jobs"))
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.root.join("bin").display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            );
        for (k, v) in extra {
            cmd.env(k, v);
        }
        cmd.output().expect("run dev-memory-watch.sh")
    }

    fn ring(&self) -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = std::fs::read_dir(self.dir.join("ring"))
            .map(|d| d.filter_map(|e| e.ok().map(|e| e.path())).collect())
            .unwrap_or_default();
        files.retain(|p| p.extension().is_some_and(|x| x == "tsv"));
        files.sort();
        files
    }

    /// The alerts the stub system of record took, one JSON body each.
    fn posted(&self) -> Vec<serde_json::Value> {
        std::fs::read_to_string(self.root.join("posted.txt"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).expect("an alert body is one JSON document"))
            .collect()
    }

    /// Alert bodies in a directory under the watch's own (`alert-spool`
    /// waiting to be sent, `alert-refused` moved aside).
    fn alerts_in(&self, dir: &str) -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = std::fs::read_dir(self.dir.join(dir))
            .map(|d| d.filter_map(|e| e.ok().map(|e| e.path())).collect())
            .unwrap_or_default();
        files.retain(|p| p.extension().is_some_and(|x| x == "json"));
        files.sort();
        files
    }

    fn spooled(&self) -> usize {
        self.alerts_in("alert-spool").len()
    }

    /// `--state`, as the reclaim pass reads it: key=value lines.
    fn state(&self) -> String {
        let out = self.run("--state", &[]);
        assert!(out.status.success(), "{}", say(&out));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// The one line of state the watch keeps for the dev container.
    fn seen(&self) -> PathBuf {
        self.dir.join("seen").join("pod-uid-1.dev")
    }
}

fn say(out: &Output) -> String {
    format!(
        "status {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after 1970")
        .as_secs()
}

/// An epoch as the kubelet writes a time.
fn rfc3339(epoch: u64) -> String {
    let out = Command::new("date")
        .args(["-u", "-d", &format!("@{epoch}"), "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .expect("date");
    String::from_utf8(out.stdout)
        .expect("utf8")
        .trim()
        .to_string()
}

/// The pod as it stood before the kill: a 7.6 GiB rustc and a client
/// carrying credentials in the dev container, one postgres backend, and
/// the sidecar's own shell.
fn busy_pod(fx: &Fixture) {
    process(
        &fx.proc,
        100,
        DEV_ID,
        2_000_000,
        100_000,
        &[
            "/usr/bin/rustc",
            "--crate-name",
            "boss_jobs",
            "--edition=2024",
        ],
    );
    // Assembled here, so the tree holds no URL with a password in it for
    // the no-secrets lint to find: the fixture is the shape, not a line.
    let url_with_password = format!("http://david:{}@forge.test/x", "hunter2");
    process(
        &fx.proc,
        101,
        DEV_ID,
        50_000,
        1_000,
        &[
            "curl",
            "-H",
            "Authorization: Bearer abc.def.ghi",
            "--token",
            "s3cr3tvalue",
            &url_with_password,
            "0123456789abcdef0123456789abcdef01234567",
            "the_machine_door_carries_every_read_surface",
        ],
    );
    process(&fx.proc, 200, PG_ID, 30_000, 20_000, &["postgres"]);
    process(&fx.proc, 300, "", 700, 600, &["bash", "watch"]);
}

fn running_sampler_pod(extra_missing: bool) -> String {
    let mut p: serde_json::Value = serde_json::from_str(&pod(0, None)).expect("fixture pod");
    for status in p["status"]["containerStatuses"]
        .as_array_mut()
        .expect("statuses")
    {
        status["state"] = serde_json::json!({"running": {"startedAt": "2026-10-12T00:00:00Z"}});
    }
    for (name, id) in [
        (
            "reclaim",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ),
        (
            "unobserved",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        ),
    ] {
        if name == "unobserved" && !extra_missing {
            continue;
        }
        p["spec"]["containers"]
            .as_array_mut()
            .expect("spec")
            .push(serde_json::json!({"name": name}));
        p["status"]["containerStatuses"]
            .as_array_mut()
            .expect("statuses")
            .push(serde_json::json!({
                "name": name, "containerID": format!("containerd://{id}"), "restartCount": 0,
                "state": {"running": {"startedAt": "2026-10-12T00:00:00Z"}}
            }));
    }
    p.to_string()
}

#[test]
fn sandbox_attestation_refuses_unrelated_malformed_and_ambiguous_groups() {
    let sandbox = "4191072d995a45c2998d8b7d994892ddce53c55fbf9309b9836d322fbc2a53b0";
    let valid = format!(
        "42 1 0:12 /lib/containerd/io.containerd.grpc.v1.cri/sandboxes/{sandbox}/hostname /etc/hostname rw - xfs fixture rw\n"
    );
    let cases = [
        ("missing", String::new(), false),
        ("wrong-id", valid.replace(sandbox, &"c".repeat(64)), false),
        ("short-id", valid.replace(sandbox, "4191072d995a"), false),
        (
            "uppercase",
            valid.replace(sandbox, &sandbox.to_uppercase()),
            false,
        ),
        (
            "wrong-point",
            valid.replace(" /etc/hostname ", " /etc/other "),
            false,
        ),
        (
            "wrong-root",
            valid.replace("/lib/containerd/", "/unrelated/containerd/"),
            false,
        ),
        (
            "runtime-lookalike",
            valid.replace("io.containerd.grpc.v1.cri", "ioXcontainerdXgrpcXv1Xcri"),
            false,
        ),
        (
            "root-suffix",
            valid.replace("/hostname /etc", "/hostname/extra /etc"),
            false,
        ),
        (
            "root-prefix",
            valid.replace("/lib/containerd/", "/extra/lib/containerd/"),
            false,
        ),
        ("duplicate", format!("{valid}{valid}"), false),
        ("truncated", valid.replace(" - xfs fixture rw", ""), false),
        ("regular-collision", valid.replace(sandbox, DEV_ID), false),
        ("extra-group", valid.clone(), true),
    ];
    for (name, mountinfo, extra) in cases {
        let root = boss_testing::scratch_dir(&format!("boss-dmw-sandbox-{name}"));
        let _guard = Scratch(root.clone());
        let fx = Fixture::new(&root);
        busy_pod(&fx);
        fx.set_pod(&running_sampler_pod(false));
        process(&fx.proc, 400, sandbox, 56, 46, &["pause"]);
        if extra {
            process(&fx.proc, 401, &"d".repeat(64), 1, 0, &["unknown"]);
        }
        boss_testing::create_dir(&fx.proc.join("self"));
        boss_testing::write_file(&fx.proc.join("self/mountinfo"), &mountinfo);
        let out = fx.run("--tick", &[]);
        assert!(out.status.success(), "{name}: {}", say(&out));
        let sample = std::fs::read_to_string(fx.ring().last().expect("sample")).expect("text");
        assert!(
            sample.contains("own-binding\tself\tunknown"),
            "{name}: {sample}"
        );
        assert!(
            sample.contains("container\t4191072d995a\tprocs=1\trss_kib=224"),
            "{name}: {sample}"
        );
        assert!(
            sample.contains("container\tself\tprocs=1\trss_kib=2800"),
            "{name}: {sample}"
        );
    }
}

#[test]
fn sandbox_attestation_accepts_containerd_state_root_with_optional_mount_fields() {
    let root = boss_testing::scratch_dir("boss-dmw-sandbox-var-lib");
    let _guard = Scratch(root.clone());
    let fx = Fixture::new(&root);
    busy_pod(&fx);
    fx.set_pod(&running_sampler_pod(false));
    let sandbox = "4191072d995a45c2998d8b7d994892ddce53c55fbf9309b9836d322fbc2a53b0";
    process(&fx.proc, 400, sandbox, 56, 46, &["pause"]);
    boss_testing::create_dir(&fx.proc.join("self"));
    boss_testing::write_file(
        &fx.proc.join("self/mountinfo"),
        &format!(
            "41 1 0:1 / /sys/fs/cgroup rw - cgroup2 cgroup rw\n42 1 0:12 /var/lib/containerd/io.containerd.grpc.v1.cri/sandboxes/{sandbox}/hostname /etc/hostname rw shared:12 - xfs fixture rw\n"
        ),
    );
    let out = fx.run("--tick", &[]);
    assert!(out.status.success(), "{}", say(&out));
    let sample = std::fs::read_to_string(fx.ring().last().expect("sample")).expect("text");
    assert!(
        sample.contains("own-binding\treclaim\tmethod=unique-running-minus-external"),
        "{sample}"
    );
    assert!(sample.contains("pod_uid=pod-uid-1"), "{sample}");
    assert!(
        sample.contains("container\t4191072d995a\tprocs=1\trss_kib=224"),
        "{sample}"
    );
}

#[test]
fn sandbox_attestation_cannot_exempt_a_regular_container_id() {
    let root = boss_testing::scratch_dir("boss-dmw-sandbox-regular-id");
    let _guard = Scratch(root.clone());
    let fx = Fixture::new(&root);
    busy_pod(&fx);
    fx.set_pod(&running_sampler_pod(false));
    boss_testing::create_dir(&fx.proc.join("self"));
    boss_testing::write_file(
        &fx.proc.join("self/mountinfo"),
        &format!(
            "42 1 0:12 /lib/containerd/io.containerd.grpc.v1.cri/sandboxes/{DEV_ID}/hostname /etc/hostname rw - xfs fixture rw\n"
        ),
    );
    let out = fx.run("--tick", &[]);
    assert!(out.status.success(), "{}", say(&out));
    let sample = std::fs::read_to_string(fx.ring().last().expect("sample")).expect("text");
    assert!(sample.contains("own-binding\tself\tunknown"), "{sample}");
    assert!(sample.contains("container\tdev\tprocs=2"), "{sample}");
}

#[test]
fn independently_attested_sandbox_preserves_sampler_binding_and_population() {
    let root = boss_testing::scratch_dir("boss-dmw-attested-sandbox");
    let _guard = Scratch(root.clone());
    let fx = Fixture::new(&root);
    busy_pod(&fx);
    fx.set_pod(&running_sampler_pod(false));
    let sandbox = "4191072d995a45c2998d8b7d994892ddce53c55fbf9309b9836d322fbc2a53b0";
    process(&fx.proc, 400, sandbox, 56, 46, &["pause"]);
    boss_testing::create_dir(&fx.proc.join("self"));
    boss_testing::write_file(
        &fx.proc.join("self/mountinfo"),
        &format!(
            "42 1 0:12 /lib/containerd/io.containerd.grpc.v1.cri/sandboxes/{sandbox}/hostname /etc/hostname rw - xfs fixture rw\n"
        ),
    );
    let out = fx.run("--tick", &[]);
    assert!(out.status.success(), "{}", say(&out));
    let sample = std::fs::read_to_string(fx.ring().last().expect("sample")).expect("sample text");
    assert!(
        sample.contains("own-binding\treclaim\tmethod=unique-running-minus-external"),
        "{sample}"
    );
    assert!(
        sample.contains(&format!("sandbox_id={sandbox}")),
        "{sample}"
    );
    assert!(
        sample.contains("sandbox_source=own-hostname-mount"),
        "{sample}"
    );
    let mut regular_external = [DEV_ID, PG_ID];
    regular_external.sort();
    assert!(
        sample.contains(&format!(
            "external_ids={}\tsandbox_id={sandbox}",
            regular_external.join(",")
        )),
        "sandbox provenance must be separate from regular IDs: {sample}"
    );
    assert!(
        sample.contains("container\t4191072d995a\tprocs=1\trss_kib=224"),
        "{sample}"
    );
}

#[test]
fn sampler_identity_requires_unique_native_running_container_and_keeps_numeric_categories() {
    let root = boss_testing::scratch_dir("boss-dmw-own-binding");
    let _guard = Scratch(root.clone());
    let fx = Fixture::new(&root);
    busy_pod(&fx);
    let own = fx.proc.join("300/root/sys/fs/cgroup");
    boss_testing::create_dir(&own);
    boss_testing::write_file(&own.join("memory.current"), "268210176\n");
    boss_testing::write_file(
        &own.join("memory.stat"),
        "anon 169148416\nfile 90000000\nshmem 12000000\nkernel 8000000\nslab 4000000\nunknown_field PRIVATE_MARKER\n",
    );
    fx.set_pod(&running_sampler_pod(false));
    let out = fx.run("--tick", &[]);
    assert!(out.status.success(), "{}", say(&out));
    let sample = std::fs::read_to_string(fx.ring().last().expect("sample")).expect("sample text");
    for expected in [
        "container\treclaim\tprocs=1",
        "cgroup\treclaim\tcurrent=268210176",
        "own-binding\treclaim\tmethod=unique-running-minus-external",
        "memory-stat\treclaim\tanon=169148416",
        "file=90000000",
        "shmem=12000000",
        "kernel=8000000",
        "slab=4000000",
    ] {
        assert!(sample.contains(expected), "missing {expected}:\n{sample}");
    }
    assert!(
        !sample.contains("PRIVATE_MARKER"),
        "non-numeric stat data is not telemetry"
    );
}

#[test]
fn ambiguous_sampler_identity_keeps_unassigned_own_numeric_evidence() {
    let root = boss_testing::scratch_dir("boss-dmw-own-ambiguous");
    let _guard = Scratch(root.clone());
    let fx = Fixture::new(&root);
    busy_pod(&fx);
    fx.set_pod(&running_sampler_pod(true));
    let out = fx.run("--tick", &[]);
    assert!(out.status.success(), "{}", say(&out));
    let sample = std::fs::read_to_string(fx.ring().last().expect("sample")).expect("sample text");
    assert!(
        sample.contains("container\tself\tprocs=1"),
        "ambiguous evidence stays unassigned: {sample}"
    );
    assert!(
        sample.contains("own-binding\tself\tunknown"),
        "ambiguity is explicit: {sample}"
    );
    assert!(!sample.contains("container\treclaim\t"));
}

#[test]
fn historical_unbound_sampler_is_not_reassigned_or_reported_as_no_processes() {
    let root = boss_testing::scratch_dir("boss-dmw-historical-own");
    let _guard = Scratch(root.clone());
    let fx = Fixture::new(&root);
    busy_pod(&fx);
    let own = fx.proc.join("300/root/sys/fs/cgroup");
    boss_testing::create_dir(&own);
    boss_testing::write_file(&own.join("memory.current"), "268210176\n");
    boss_testing::write_file(
        &own.join("memory.stat"),
        "anon 169148416\nfile 90000000\nshmem 12000000\n",
    );
    fx.set_pod(&pod(0, None));
    let out = fx.run("--tick", &[]);
    assert!(out.status.success(), "{}", say(&out));
    // A v1 sample had no own-binding record. Its original self label
    // still must not acquire today's native identity on replay.
    let old_sample = fx.ring().last().expect("sample").clone();
    let text = std::fs::read_to_string(&old_sample).expect("fixture sample");
    let v1: String = text
        .lines()
        .filter(|line| !line.starts_with("own-binding\t"))
        .map(|line| format!("{line}\n"))
        .collect();
    boss_testing::write_file(&old_sample, &v1);
    let (_, finished) = date_the_sample_before_a_kill(&fx, 10);
    let mut p: serde_json::Value = serde_json::from_str(&running_sampler_pod(false)).expect("pod");
    p["status"]["containerStatuses"][2]["restartCount"] = serde_json::json!(1);
    p["status"]["containerStatuses"][2]["lastState"] = serde_json::json!({"terminated": {
        "reason": "OOMKilled", "exitCode": 137, "finishedAt": finished}});
    fx.set_pod(&p.to_string());
    let out = fx.run("--tick", &[]);
    assert!(out.status.success(), "{}", say(&out));
    let posted = fx.posted();
    assert_eq!(posted.len(), 1);
    let detail = posted[0]["metadata"]["detail"].as_str().expect("detail");
    assert!(
        detail.contains("binding unknown")
            && detail.contains("unassigned sampler")
            && detail.contains("current=268210176"),
        "unknown evidence remains visible: {detail}"
    );
    assert!(
        !detail.contains("none of its processes"),
        "absence of binding is not absence: {detail}"
    );
    assert!(
        !detail.contains("bash[300]"),
        "unassigned processes must not become reclaim processes: {detail}"
    );
    assert!(
        detail.contains("unassigned memory.stat") && detail.contains("shmem=12000000"),
        "safe unknown-binding categories stay visible: {detail}"
    );
}

#[test]
fn incomplete_native_status_cannot_manufacture_unique_sampler_identity() {
    let root = boss_testing::scratch_dir("boss-dmw-incomplete-native");
    let _guard = Scratch(root.clone());
    let fx = Fixture::new(&root);
    busy_pod(&fx);
    let mut p: serde_json::Value = serde_json::from_str(&running_sampler_pod(true)).expect("pod");
    p["status"]["containerStatuses"]
        .as_array_mut()
        .expect("statuses")
        .pop();
    // Its spec still names the fourth container. Dropping its status
    // must not turn two possible owners into one.
    fx.set_pod(&p.to_string());
    let out = fx.run("--tick", &[]);
    assert!(out.status.success(), "{}", say(&out));
    let sample = std::fs::read_to_string(fx.ring().last().expect("sample")).expect("text");
    assert!(
        sample.contains("own-binding\tself\tunknown"),
        "incomplete native status is not a unique binding: {sample}"
    );
    assert!(!sample.contains("container\treclaim\t"));
}

#[test]
fn duplicate_or_malformed_memory_stat_values_are_unknown() {
    let root = boss_testing::scratch_dir("boss-dmw-stat-unknown");
    let _guard = Scratch(root.clone());
    let fx = Fixture::new(&root);
    busy_pod(&fx);
    let own = fx.proc.join("300/root/sys/fs/cgroup");
    boss_testing::create_dir(&own);
    boss_testing::write_file(
        &own.join("memory.stat"),
        "anon 42\nanon MALFORMED EXTRA\nfile nonnumeric\nshmem 17\n",
    );
    fx.set_pod(&running_sampler_pod(false));
    let out = fx.run("--tick", &[]);
    assert!(out.status.success(), "{}", say(&out));
    let sample = std::fs::read_to_string(fx.ring().last().expect("sample")).expect("text");
    let stats = sample
        .lines()
        .find(|line| line.starts_with("memory-stat\treclaim\t"))
        .expect("reclaim categories");
    for expected in [
        "\tanon=unknown\t",
        "\tfile=unknown\t",
        "\tshmem=17\t",
        "\tkernel=unknown\t",
    ] {
        assert!(stats.contains(expected), "missing {expected}: {sample}");
    }
    assert!(!sample.contains("MALFORMED") && !sample.contains("nonnumeric"));
}

#[test]
fn sampler_binding_refuses_dark_wrong_pod_unread_and_duplicate_native_ids() {
    for mode in [
        "dark",
        "empty-document",
        "wrong-pod",
        "unread-cgroup",
        "duplicate-id",
        "malformed-id",
    ] {
        let root = boss_testing::scratch_dir(&format!("boss-dmw-own-{mode}"));
        let _guard = Scratch(root.clone());
        let fx = Fixture::new(&root);
        busy_pod(&fx);
        let mut p: serde_json::Value =
            serde_json::from_str(&running_sampler_pod(false)).expect("pod");
        match mode {
            "wrong-pod" => p["metadata"]["name"] = serde_json::json!("another-pod"),
            "duplicate-id" => {
                p["status"]["containerStatuses"][2]["containerID"] =
                    serde_json::json!(format!("containerd://{PG_ID}"))
            }
            "malformed-id" => {
                p["status"]["containerStatuses"][2]["containerID"] =
                    serde_json::json!("containerd://unknown")
            }
            "unread-cgroup" => {
                std::fs::remove_file(fx.proc.join("200/cgroup")).expect("remove fixture cgroup");
            }
            _ => {}
        }
        if mode == "empty-document" {
            fx.set_pod("");
        } else if mode != "dark" {
            fx.set_pod(&p.to_string());
        }
        let out = fx.run("--tick", &[]);
        assert!(out.status.success(), "{mode}: {}", say(&out));
        let sample = std::fs::read_to_string(fx.ring().last().expect("sample")).expect("text");
        assert!(
            sample.contains("own-binding\tself\tunknown")
                && sample.contains("container\tself\tprocs=1"),
            "{mode} must retain unassigned evidence: {sample}"
        );
        assert!(!sample.contains("container\treclaim\t"), "{mode}: {sample}");
        let stats = sample
            .lines()
            .find(|line| line.starts_with("memory-stat\tself\t"))
            .expect("own unknown stats");
        assert!(
            stats.contains("\tanon=unknown\t"),
            "missing counters are unknown: {stats}"
        );
    }
}

#[test]
fn zero_resident_external_process_still_establishes_its_native_cgroup_identity() {
    let root = boss_testing::scratch_dir("boss-dmw-zero-resident-external");
    let _guard = Scratch(root.clone());
    let fx = Fixture::new(&root);
    busy_pod(&fx);
    process(&fx.proc, 200, PG_ID, 0, 0, &["postgres"]);
    fx.set_pod(&running_sampler_pod(false));
    let out = fx.run("--tick", &[]);
    assert!(out.status.success(), "{}", say(&out));
    let sample = std::fs::read_to_string(fx.ring().last().expect("sample")).expect("text");
    let binding = sample
        .lines()
        .find(|line| line.starts_with("own-binding\treclaim\t"))
        .expect("unique own binding");
    for want in [
        "method=unique-running-minus-external",
        "pod_uid=pod-uid-1",
        DEV_ID,
        PG_ID,
        "id=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    ] {
        assert!(binding.contains(want), "binding lacks {want}: {binding}");
    }
    assert!(sample.contains("container\treclaim\tprocs=1"));
    assert!(
        !sample.contains("container\tpostgres\t"),
        "zero-resident process contributes identity, not RSS: {sample}"
    );
}

#[test]
fn sampler_binding_requires_native_running_state_objects() {
    for (tag, running) in [
        ("false", serde_json::json!(false)),
        ("null", serde_json::Value::Null),
        ("string", serde_json::json!("running")),
        ("number", serde_json::json!(1)),
        ("array", serde_json::json!([])),
        ("empty-object", serde_json::json!({})),
    ] {
        let root = boss_testing::scratch_dir(&format!("boss-dmw-running-{tag}"));
        let _guard = Scratch(root.clone());
        let fx = Fixture::new(&root);
        busy_pod(&fx);
        let mut p: serde_json::Value =
            serde_json::from_str(&running_sampler_pod(false)).expect("pod");
        p["status"]["containerStatuses"][2]["state"]["running"] = running;
        fx.set_pod(&p.to_string());
        let out = fx.run("--tick", &[]);
        assert!(out.status.success(), "{tag}: {}", say(&out));
        let sample = std::fs::read_to_string(fx.ring().last().expect("sample")).expect("text");
        assert!(
            sample.contains("own-binding\tself\tunknown"),
            "{tag} is not native running-state evidence: {sample}"
        );
        assert!(
            !sample.contains("container\treclaim\t"),
            "{tag} must not manufacture a native owner: {sample}"
        );
    }
}

#[test]
fn a_tick_keeps_the_largest_processes_by_container_and_nothing_secret_shaped() {
    let root = boss_testing::scratch_dir("boss-dmw-sample");
    let _guard = Scratch(root.clone());
    let fx = Fixture::new(&root);
    busy_pod(&fx);
    // The reader's own cgroup IS readable to it; another container's is not.
    let own = fx.proc.join("300/root/sys/fs/cgroup");
    boss_testing::create_dir(&own);
    boss_testing::write_file(&own.join("memory.current"), "1104134144\n");
    boss_testing::write_file(&own.join("memory.peak"), "12305707008\n");
    boss_testing::write_file(&own.join("memory.max"), "34359738368\n");
    boss_testing::write_file(&own.join("memory.events"), "low 0\noom 0\noom_kill 3\n");
    fx.set_pod(&pod(0, None));
    // Three samples already in a ring bounded at two.
    boss_testing::create_dir(&fx.dir.join("ring"));
    for old in ["1000000001", "1000000002", "1000000003"] {
        boss_testing::write_file(&fx.dir.join("ring").join(format!("{old}.tsv")), "at\told\n");
    }

    let out = fx.run("--tick", &[("BOSS_DEV_MEMORY_RING", "2")]);
    assert!(out.status.success(), "{}", say(&out));

    let ring = fx.ring();
    assert_eq!(ring.len(), 2, "the ring keeps its newest two: {ring:?}");
    assert!(
        ring[0].ends_with("1000000003.tsv"),
        "the oldest samples are the ones dropped: {ring:?}"
    );
    let sample = std::fs::read_to_string(&ring[1]).expect("the new sample");
    let page_kib = 4;
    assert!(
        sample.contains(&format!(
            "container\tdev\tprocs=2\trss_kib={}\tanon_kib={}\trestarts=0\tlimit=32Gi",
            2_050_000 * page_kib,
            1_949_000 * page_kib
        )),
        "the dev container's processes are summed under its NAME, with its limit:\n{sample}"
    );
    assert!(
        sample.contains("container\tpostgres\tprocs=1\t")
            && sample.contains("container\tself\tprocs=1\t"),
        "every container of the pod is counted:\n{sample}"
    );
    assert!(
        sample.contains(&format!(
            "proc\t{}\t{}\tdev\t100\trustc\t/usr/bin/rustc --crate-name boss_jobs",
            2_000_000 * page_kib,
            1_900_000 * page_kib
        )),
        "the largest process is named with its container, pid and command line:\n{sample}"
    );
    assert!(
        sample.contains("cgroup\tdev\tunreadable"),
        "another container's cgroup is reported unreadable, never as a number:\n{sample}"
    );
    assert!(
        sample.contains(
            "cgroup\tself\tcurrent=1104134144\tpeak=12305707008\tmax=34359738368\toom_kill=3"
        ),
        "the reader's own cgroup counters ride the sample:\n{sample}"
    );
    for secret in [
        "abc.def.ghi",
        "s3cr3tvalue",
        "hunter2",
        "0123456789abcdef0123456789abcdef01234567",
        "sa-token-for-the-fixture",
    ] {
        assert!(
            !sample.contains(secret),
            "{secret} reached the sample:\n{sample}"
        );
    }
    assert!(
        sample.contains("<redacted>")
            && sample.contains("the_machine_door_carries_every_read_surface"),
        "secrets are replaced, and a test's name — what a reader of a kill needs — is kept:\n{sample}"
    );
    assert!(
        fx.posted().is_empty() && fx.spooled() == 0,
        "a pod with no restart raises nothing"
    );
}

/// A process's `/proc/<pid>/stat`, as far as the script reads it: the
/// pid, the name in parentheses, the state, and the parent's pid.
fn parent_is(proc: &Path, pid: u32, comm: &str, ppid: u32) {
    boss_testing::write_file(
        &proc.join(pid.to_string()).join("stat"),
        &format!("{pid} ({comm}) S {ppid} {pid} {pid} 0 -1 4194560 0 0 0 0\n"),
    );
}

/// Backlog 893ce2b0 (from builder run 3f42218d): the ring kept only the
/// largest BOSS_DEV_MEMORY_TOP processes, so on 2026-10-10 07:48:04Z a
/// 32-wide workspace clippy showed at most 15 drivers and the count of
/// concurrent compilers was a lower bound. Every process is now counted
/// by its name, per container, with its summed size and the parents it
/// ran under, and each parent of a crowd is named with its command line
/// — so a sample says how many compilers ran and from which cargo,
/// whatever TOP keeps.
#[test]
fn a_sample_counts_every_process_by_name_and_names_the_cargo_it_ran_under() {
    let root = boss_testing::scratch_dir("boss-dmw-by-name");
    let _guard = Scratch(root.clone());
    let fx = Fixture::new(&root);
    process(
        &fx.proc,
        500,
        DEV_ID,
        20_000,
        1_000,
        &["cargo", "clippy", "--workspace", "--all-targets"],
    );
    parent_is(&fx.proc, 500, "cargo", 1);
    process(
        &fx.proc,
        600,
        DEV_ID,
        15_000,
        1_000,
        &["cargo", "test", "-p", "boss-jobs"],
    );
    parent_is(&fx.proc, 600, "cargo", 1);
    for pid in 501..=512 {
        process(
            &fx.proc,
            pid,
            DEV_ID,
            90_000,
            10_000,
            &["clippy-driver", "rustc", "--crate-name", "boss_jobs"],
        );
        parent_is(&fx.proc, pid, "clippy-driver", 500);
    }
    for pid in 601..=603 {
        process(
            &fx.proc,
            pid,
            DEV_ID,
            60_000,
            5_000,
            &["rustc", "--crate-name", "boss_core"],
        );
        parent_is(&fx.proc, pid, "rustc", 600);
    }
    // Six shells under six parents the table does not hold: the parents
    // list keeps four and counts the rest.
    for (i, pid) in (701..=706).enumerate() {
        process(&fx.proc, pid, DEV_ID, 1_000, 500, &["sh", "-c", "true"]);
        parent_is(&fx.proc, pid, "sh", 801 + i as u32);
    }
    // A process names itself: a `) S <n> (` inside the name must not
    // become its parent. The kernel's own fields follow the LAST `) `.
    process(&fx.proc, 900, DEV_ID, 2_000, 1_000, &["odd"]);
    boss_testing::write_file(&fx.proc.join("900/comm"), "x) S 999 (y\n");
    parent_is(&fx.proc, 900, "x) S 999 (y", 600);
    process(&fx.proc, 200, PG_ID, 30_000, 20_000, &["postgres"]);
    fx.set_pod(&pod(0, None));

    let out = fx.run("--tick", &[("BOSS_DEV_MEMORY_TOP", "5")]);
    assert!(out.status.success(), "{}", say(&out));
    let ring = fx.ring();
    let sample = std::fs::read_to_string(ring.last().expect("a sample")).expect("sample");
    let kib = 4;
    assert_eq!(
        sample.lines().filter(|l| l.starts_with("proc\t")).count(),
        5,
        "TOP still bounds the per-process lines:\n{sample}"
    );
    for want in [
        format!(
            "command\tdev\tclippy-driver\tprocs=12\trss_kib={}\tanon_kib={}\tparents=cargo[500]x12\n",
            12 * 90_000 * kib,
            12 * 80_000 * kib
        ),
        format!(
            "command\tdev\trustc\tprocs=3\trss_kib={}\tanon_kib={}\tparents=cargo[600]x3\n",
            3 * 60_000 * kib,
            3 * 55_000 * kib
        ),
        format!(
            "command\tdev\tcargo\tprocs=2\trss_kib={}\tanon_kib={}\tparents=?[1]x2\n",
            35_000 * kib,
            33_000 * kib
        ),
        format!(
            "command\tdev\tsh\tprocs=6\trss_kib={}\tanon_kib={}\tparents=?[801]x1,?[802]x1,?[803]x1,?[804]x1,+2\n",
            6_000 * kib,
            3_000 * kib
        ),
        format!(
            "command\tdev\tx??S?999??y\tprocs=1\trss_kib={}\tanon_kib={}\tparents=cargo[600]x1\n",
            2_000 * kib,
            1_000 * kib
        ),
        format!(
            "command\tpostgres\tpostgres\tprocs=1\trss_kib={}\tanon_kib={}\tparents=-x1\n",
            30_000 * kib,
            10_000 * kib
        ),
        "parent\tdev\t500\tcargo\tcargo clippy --workspace --all-targets".to_string(),
        "parent\tdev\t600\tcargo\tcargo test -p boss-jobs".to_string(),
    ] {
        assert!(
            sample.contains(&want),
            "the sample lacks `{}`:\n{sample}",
            want.trim_end()
        );
    }
    assert!(
        !sample.contains("parent\tdev\t1\t") && !sample.contains("\t801\t"),
        "a parent the table does not hold has no command line to name:\n{sample}"
    );

    // The alert names the crowd by name and count, never by command line.
    let (_, finished) = date_the_sample_before_a_kill(&fx, 10);
    fx.set_pod(&pod(1, Some(("OOMKilled", 137, &finished))));
    let out = fx.run("--tick", &[("BOSS_DEV_MEMORY_TOP", "5")]);
    assert!(out.status.success(), "{}", say(&out));
    let posted = fx.posted();
    assert_eq!(posted.len(), 1, "{}", say(&out));
    let detail = posted[0]["metadata"]["detail"].as_str().expect("detail");
    assert!(
        detail.contains(
            "processes by name, largest first: 12 clippy-driver 4.1 GiB under cargo[500]x12 | 3 rustc 0.7 GiB under cargo[600]x3"
        ),
        "the alert carries the counts by name:\n{detail}"
    );
    assert!(
        !detail.contains("--workspace") && !detail.contains("boss-jobs"),
        "a parent's command line stays in the ring:\n{detail}"
    );
}

#[test]
fn a_restart_raises_one_alert_carrying_the_last_sample_before_the_kill() {
    let root = boss_testing::scratch_dir("boss-dmw-restart");
    let _guard = Scratch(root.clone());
    let fx = Fixture::new(&root);
    busy_pod(&fx);
    fx.set_pod(&pod(0, None));
    let out = fx.run("--tick", &[]);
    assert!(out.status.success(), "{}", say(&out));
    assert!(fx.posted().is_empty(), "no restart yet, no alert");

    // Date the sample 100 s ago and the kill 10 s after it, then put a
    // LATER sample in the ring: the restarted container, nearly empty.
    let ring = fx.ring();
    assert_eq!(ring.len(), 1, "{ring:?}");
    let before = unix_now() - 100;
    let killed_at = before + 10;
    let busy = std::fs::read_to_string(&ring[0]).expect("sample");
    std::fs::remove_file(&ring[0]).expect("remove");
    boss_testing::write_file(&fx.dir.join("ring").join(format!("{before}.tsv")), &busy);
    boss_testing::write_file(
        &fx.dir.join("ring").join(format!("{}.tsv", killed_at + 5)),
        "at\tafter\ncontainer\tdev\tprocs=1\trss_kib=8\tanon_kib=4\trestarts=1\tlimit=32Gi\nproc\t8\t4\tdev\t7\tsleep\tsleep infinity\n",
    );
    let finished = rfc3339(killed_at);
    fx.set_pod(&pod(1, Some(("OOMKilled", 137, &finished))));

    let out = fx.run("--tick", &[]);
    assert!(out.status.success(), "{}", say(&out));
    let posted = fx.posted();
    assert_eq!(posted.len(), 1, "one restart, one alert: {}", say(&out));
    let alert = &posted[0];
    assert_eq!(alert["kind"], "backlog-item");
    assert_eq!(alert["priority"], "urgent");
    assert_eq!(
        alert["metadata"]["filed_by"],
        "automation:dev-scratch-reclaim"
    );
    let title = alert["title"].as_str().expect("title");
    assert!(
        title.contains("container dev restarted")
            && title.contains("OOMKilled (exit 137)")
            && title.contains(&finished),
        "the title names the container, the kubelet's reason and when: {title}"
    );
    let detail = alert["metadata"]["detail"].as_str().expect("detail");
    for want in [
        "restart count 0 -> 1",
        "memory limit 32Gi",
        "Last sample, 10 s before the kill",
        "2 processes, resident 7.8 GiB",
        "MiB rustc[100]",
        "cgroup unreadable",
    ] {
        assert!(detail.contains(want), "the alert lacks `{want}`:\n{detail}");
    }
    assert!(
        !detail.contains("sleep infinity"),
        "a sample taken AFTER the kill is the restarted container, not evidence:\n{detail}"
    );
    // The whole ring is frozen where the alert says, so the reduction
    // above is never the only copy.
    let frozen = fx
        .dir
        .join("kills")
        .join(format!("dev-{}", finished.replace(':', "")));
    assert!(
        detail.contains(&frozen.display().to_string())
            && frozen.join(format!("{before}.tsv")).is_file(),
        "the ring from before the kill is kept at {}:\n{detail}",
        frozen.display()
    );

    let out = fx.run("--tick", &[]);
    assert!(out.status.success(), "{}", say(&out));
    assert_eq!(
        fx.posted().len(),
        1,
        "the same restart is raised once: {}",
        say(&out)
    );
}

/// The pod status of the pod that REPLACED the fixture's: another uid,
/// so nothing the watch kept for the old pod's containers applies.
fn replaced_pod(dev_restarts: u32, killed: Option<(&str, i32, &str)>) -> String {
    pod(dev_restarts, killed).replace("pod-uid-1", "pod-uid-2")
}

/// Backlog 36b1aa00 (B), from delta review d617cf4e. The ring is on the
/// PVC and survives a pod roll, and the watch in a new pod starts only
/// with the sidecar's first pass, minutes in. A kill of the NEW pod's
/// dev container before that first sample was filed urgent with `Last
/// sample, N s before the kill` describing the PREVIOUS pod's
/// container: a sample was never asked which pod it was taken in. It
/// stays urgent — it is a new kill nobody recorded — and it says that
/// no sample of this pod precedes it, naming the pod the ring's samples
/// came from.
#[test]
fn a_new_pods_first_kill_is_never_described_with_the_old_pods_sample() {
    let root = boss_testing::scratch_dir("boss-dmw-roll");
    let _guard = Scratch(root.clone());
    let fx = Fixture::new(&root);
    busy_pod(&fx);
    fx.set_pod(&pod(0, None));
    // The old pod's loop samples its busy dev container.
    let out = fx.run("--tick", &[]);
    assert!(out.status.success(), "{}", say(&out));
    let (before, finished) = date_the_sample_before_a_kill(&fx, 10);

    // The pod rolls. The new pod's dev container is killed before the
    // watch has sampled it, and the watch's first tick there finds it.
    fx.set_pod(&replaced_pod(1, Some(("OOMKilled", 137, &finished))));
    let new_pod = [("BOSS_POD_NAME", "boss-dev-rolled")];
    let out = fx.run("--tick", &new_pod);
    assert!(out.status.success(), "{}", say(&out));
    let posted = fx.posted();
    assert_eq!(posted.len(), 1, "one restart, one alert: {}", say(&out));
    let alert = &posted[0];
    assert_eq!(
        alert["priority"], "urgent",
        "a kill in a pod the watch had not sampled yet is still a new kill"
    );
    let title = alert["title"].as_str().expect("title");
    assert!(
        !title.contains("predates"),
        "the ring proves the watch ran before this pod, so the kill does not predate it: {title}"
    );
    let detail = alert["metadata"]["detail"].as_str().expect("detail");
    for stale in ["Last sample", "rustc[100]", "resident 7.8 GiB"] {
        assert!(
            !detail.contains(stale),
            "`{stale}` is the PREVIOUS pod's dev container, not the one killed:\n{detail}"
        );
    }
    for want in [
        "Container dev of pod boss-dev-rolled restarted",
        "No sample of this pod precedes this kill",
        "1 sample(s)",
        "pod boss-dev-test",
    ] {
        assert!(detail.contains(want), "the alert lacks `{want}`:\n{detail}");
    }
    // The other pod's samples are still kept, whole, where the alert says.
    let frozen = fx
        .dir
        .join("kills")
        .join(format!("dev-{}", finished.replace(':', "")));
    assert!(
        detail.contains(&frozen.display().to_string())
            && frozen.join(format!("{before}.tsv")).is_file(),
        "the ring as it stood is kept at {}:\n{detail}",
        frozen.display()
    );
}

/// The same question asked of a ring holding BOTH pods' samples: the
/// last sample and the largest are chosen among this pod's alone, and
/// the alert says how many of the ring's samples it left out.
#[test]
fn the_last_and_the_largest_sample_are_chosen_among_this_pods_alone() {
    let root = boss_testing::scratch_dir("boss-dmw-roll-mixed");
    let _guard = Scratch(root.clone());
    let fx = Fixture::new(&root);
    busy_pod(&fx);
    fx.set_pod(&replaced_pod(0, None));
    let new_pod = [("BOSS_POD_NAME", "boss-dev-rolled")];
    let out = fx.run("--tick", &new_pod);
    assert!(out.status.success(), "{}", say(&out));
    let (before, finished) = date_the_sample_before_a_kill(&fx, 10);
    // An OLDER sample from the previous pod, whose dev container was
    // larger than this one's ever was.
    let ours = std::fs::read_to_string(fx.dir.join("ring").join(format!("{before}.tsv")))
        .expect("this pod's sample");
    let page_kib = 4;
    let theirs = ours
        .replace("\tboss-dev-rolled\t", "\tboss-dev-test\t")
        .replace(
            &format!("rss_kib={}", 2_050_000 * page_kib),
            &format!("rss_kib={}", 7_000_000 * page_kib),
        )
        .replace("\trustc\t", "\toldrustc\t");
    assert_ne!(ours, theirs, "the fixture did not rewrite the sample");
    assert!(
        theirs.contains("\tboss-dev-test\t") && theirs.contains("\toldrustc\t"),
        "the previous pod's sample is not the shape this test means:\n{theirs}"
    );
    boss_testing::write_file(
        &fx.dir.join("ring").join(format!("{}.tsv", before - 300)),
        &theirs,
    );

    fx.set_pod(&replaced_pod(1, Some(("OOMKilled", 137, &finished))));
    let out = fx.run("--tick", &new_pod);
    assert!(out.status.success(), "{}", say(&out));
    let posted = fx.posted();
    assert_eq!(posted.len(), 1, "one restart, one alert: {}", say(&out));
    let detail = posted[0]["metadata"]["detail"].as_str().expect("detail");
    for want in [
        "Last sample, 10 s before the kill",
        "resident 7.8 GiB",
        "MiB rustc[100]",
        "(1 samples, one every 15 s)",
        "1 more sample(s) in that ring were taken in another pod",
    ] {
        assert!(detail.contains(want), "the alert lacks `{want}`:\n{detail}");
    }
    for stale in ["oldrustc", "Largest sample in the ring", "26.7 GiB"] {
        assert!(
            !detail.contains(stale),
            "`{stale}` is the PREVIOUS pod's dev container:\n{detail}"
        );
    }
}

/// Short on purpose: a fixture, and no-secrets.sh reads 32+ characters
/// after such a name as a credential.
const FIXTURE_TOKEN: &str = "fixture-not-a-credential";

/// `(how the machine token rode, "[POST ]URL")` per request the stub saw.
fn token_on_each_request(fx: &Fixture) -> Vec<(String, String)> {
    std::fs::read_to_string(fx.root.join("posted.txt.headers"))
        .unwrap_or_default()
        .lines()
        .map(|l| {
            let (how, rest) = l.split_once(' ').expect("how, then the request");
            (how.to_string(), rest.to_string())
        })
        .collect()
}

/// One restart, driven to its alert under `extra`; the tick's output.
fn a_restart_alerted(fx: &Fixture, extra: &[(&str, &str)]) -> String {
    busy_pod(fx);
    fx.set_pod(&pod(0, None));
    let first = fx.run("--tick", extra);
    assert!(first.status.success(), "{}", say(&first));
    let (_, finished) = date_the_sample_before_a_kill(fx, 10);
    fx.set_pod(&pod(1, Some(("OOMKilled", 137, &finished))));
    let out = fx.run("--tick", extra);
    assert!(out.status.success(), "{}", say(&out));
    assert_eq!(
        fx.posted().len(),
        1,
        "one restart, one alert: {}",
        say(&out)
    );
    format!("{}{}", say(&first), say(&out))
}

/// The re-rail of car 099f8a9f over this watch (backlog 37742794): that
/// car mounts Secret boss-machine-token in the `reclaim` sidecar, and
/// the watch's loop is that container's, so from the day it lands the
/// mount is the watch's too. Its two requests to the system of record —
/// the alert and the owner read — take the token from the one shell
/// reader, as a 0600 header FILE and for the estate's hosts only. Its
/// read of the cluster API carries the pod's service-account token and
/// NEVER this one: not when the list names the cluster API's own host,
/// which is the misconfiguration that would otherwise hand the estate
/// token to a host no machine gate fronts.
#[test]
fn with_the_token_mounted_the_alert_carries_it_and_the_cluster_api_read_never_does() {
    for (hosts, record_stamped) in [("sor.test", true), ("k8s.test", false)] {
        let root = boss_testing::scratch_dir("boss-dmw-token");
        let _guard = Scratch(root.clone());
        let fx = Fixture::new(&root);
        let tok = root.join("mt/tok");
        let hdr = root.join("mt/hdr");
        boss_testing::create_dir(&tok);
        boss_testing::create_dir(&hdr);
        boss_testing::write_file(&tok.join("current"), &format!("{FIXTURE_TOKEN}\n"));
        let tok = tok.display().to_string();
        let hdr_dir = hdr.display().to_string();
        let text = a_restart_alerted(
            &fx,
            &[
                ("BOSS_MACHINE_TOKEN_DIR", &tok),
                ("BOSS_MACHINE_TOKEN_HOSTS", hosts),
                ("RUNTIME_DIRECTORY", &hdr_dir),
            ],
        );
        let sent = token_on_each_request(&fx);
        let pod_reads: Vec<&(String, String)> =
            sent.iter().filter(|(_, r)| r.contains("/pods/")).collect();
        assert!(
            pod_reads.len() >= 2 && pod_reads.iter().all(|(_, r)| r.contains("k8s.test")),
            "each tick reads the pod from the cluster API: {sent:#?}"
        );
        let carried: Vec<&&(String, String)> =
            pod_reads.iter().filter(|(how, _)| how != "none").collect();
        assert!(
            carried.is_empty(),
            "the estate machine token rode to the cluster API (hosts list `{hosts}`): {carried:#?}"
        );
        let to_record: Vec<&(String, String)> = sent
            .iter()
            .filter(|(_, r)| r.contains("sor.test"))
            .collect();
        assert!(
            to_record
                .iter()
                .any(|(_, r)| r == "POST http://sor.test:7900/api/jobs")
                && to_record
                    .iter()
                    .any(|(_, r)| r.contains(":7500/api/people")),
            "the alert and the owner read both went to the record: {sent:#?}"
        );
        let want = if record_stamped { "file" } else { "none" };
        let wrong: Vec<&&(String, String)> =
            to_record.iter().filter(|(how, _)| how != want).collect();
        assert!(
            wrong.is_empty(),
            "with the hosts list `{hosts}` every request to the record carries the token as \
             `{want}`: {wrong:#?}\n{text}"
        );
        assert!(
            !text.contains(FIXTURE_TOKEN),
            "the token is in the tick's output\n{text}"
        );
        let left: Vec<PathBuf> = std::fs::read_dir(&hdr)
            .expect("the header directory")
            .filter_map(Result::ok)
            .map(|e| e.path())
            .collect();
        assert!(left.is_empty(), "a header file outlived the tick: {left:?}");
    }
}

/// The newest sample in the ring, re-dated `secs_before` the kill time
/// this returns, so the next tick finds it BEFORE the restart it reads.
fn date_the_sample_before_a_kill(fx: &Fixture, secs_before: u64) -> (u64, String) {
    let ring = fx.ring();
    let newest = ring.last().expect("a sample in the ring");
    let before = unix_now() - 100;
    let body = std::fs::read_to_string(newest).expect("sample");
    for f in &ring {
        std::fs::remove_file(f).expect("remove");
    }
    boss_testing::write_file(&fx.dir.join("ring").join(format!("{before}.tsv")), &body);
    (before, rfc3339(before + secs_before))
}

/// Delta review of car 7849553a, N1 (blocking): the whole proc line
/// went through the scrub, whose `bearer` rule eats the whitespace
/// after the word — and when a process is NAMED bearer, that whitespace
/// is the tab before its command line, so the command line joined the
/// name field the alert prints. A process sets its own name to any
/// bytes it likes, so the test also names one with a tab and a space.
/// Nothing a process calls itself may move a field or reach the packet
/// as more than a short, safe name.
#[test]
fn a_process_name_cannot_carry_its_command_line_into_the_alert() {
    let root = boss_testing::scratch_dir("boss-dmw-comm");
    let _guard = Scratch(root.clone());
    let fx = Fixture::new(&root);
    busy_pod(&fx);
    let basic = format!("david:{}", "FAKEPASSu9");
    let named = [
        (201, "bearer", 900_000),
        (202, "Token-Bearer", 800_000),
        (203, "odd\tname here", 700_000),
    ];
    for (pid, comm, resident) in named {
        process(
            &fx.proc,
            pid,
            DEV_ID,
            resident,
            1_000,
            &[
                "tool",
                "FIRSTWORD1",
                "curl",
                "-u",
                &basic,
                "http://h.test/x",
            ],
        );
        boss_testing::write_file(
            &fx.proc.join(pid.to_string()).join("comm"),
            &format!("{comm}\n"),
        );
    }
    fx.set_pod(&pod(0, None));
    let out = fx.run("--tick", &[]);
    assert!(out.status.success(), "{}", say(&out));
    let (_, finished) = date_the_sample_before_a_kill(&fx, 10);
    fx.set_pod(&pod(1, Some(("OOMKilled", 137, &finished))));

    let out = fx.run("--tick", &[]);
    assert!(out.status.success(), "{}", say(&out));
    let posted = fx.posted();
    assert_eq!(posted.len(), 1, "{}", say(&out));
    let body = posted[0].to_string();
    for leaked in ["FAKEPASSu9", "FIRSTWORD1", "david:", "h.test", "<redacted>"] {
        assert!(
            !body.contains(leaked),
            "`{leaked}` is command-line text and reached the packet:\n{body}"
        );
    }
    let detail = posted[0]["metadata"]["detail"].as_str().expect("detail");
    for want in [
        "MiB bearer[201]",
        "MiB Token-Bearer[202]",
        "MiB odd?name?here[203]",
    ] {
        assert!(
            detail.contains(want),
            "each process is named by a short safe name and its pid, `{want}`:\n{detail}"
        );
    }
}

/// Review of car 7849553a, finding 1 (blocking): the scrub is a
/// denylist, and 23 of 32 credential shapes the reviewer wrote passed
/// it — `curl -u user:pass` among them. A packet is an immutable record
/// every reader of the queue sees, so it names a process by its name,
/// pid, container and size and carries NO command line; the ring on the
/// PVC, whose only audience is the pod that can already read its own
/// /proc, keeps them.
#[test]
fn the_alert_names_processes_and_carries_no_command_line() {
    let root = boss_testing::scratch_dir("boss-dmw-no-cmdline");
    let _guard = Scratch(root.clone());
    let fx = Fixture::new(&root);
    busy_pod(&fx);
    // A shape the scrub does not take, in a process large enough to be
    // among the container's largest.
    let basic = format!("david:{}", "plainsecret");
    process(
        &fx.proc,
        102,
        DEV_ID,
        900_000,
        1_000,
        &["curl", "-u", &basic, "http://forge.test/"],
    );
    fx.set_pod(&pod(0, None));
    let out = fx.run("--tick", &[]);
    assert!(out.status.success(), "{}", say(&out));
    let (_, finished) = date_the_sample_before_a_kill(&fx, 10);
    fx.set_pod(&pod(1, Some(("OOMKilled", 137, &finished))));

    let out = fx.run("--tick", &[]);
    assert!(out.status.success(), "{}", say(&out));
    let posted = fx.posted();
    assert_eq!(posted.len(), 1, "{}", say(&out));
    let body = posted[0].to_string();
    for leaked in [
        "plainsecret",
        "david:",
        "--crate-name",
        "forge.test",
        "/usr/bin/rustc",
    ] {
        assert!(
            !body.contains(leaked),
            "`{leaked}` is command-line text and reached the packet:\n{body}"
        );
    }
    let detail = posted[0]["metadata"]["detail"].as_str().expect("detail");
    assert!(
        detail.contains("MiB rustc[100]") && detail.contains("MiB curl[102]"),
        "each large process is still named by its name and pid:\n{detail}"
    );
    assert!(
        detail.contains("command lines") && detail.contains("/kills/"),
        "and the alert says where the command lines are kept:\n{detail}"
    );
}

#[test]
fn a_system_of_record_that_does_not_answer_keeps_the_alert_for_the_next_tick() {
    let root = boss_testing::scratch_dir("boss-dmw-dark");
    let _guard = Scratch(root.clone());
    let fx = Fixture::new(&root);
    busy_pod(&fx);
    fx.set_pod(&pod(1, Some(("OOMKilled", 137, "2026-10-08T10:31:52Z"))));
    boss_testing::write_file(&root.join("sor-state"), "down");

    let out = fx.run("--tick", &[]);
    assert!(out.status.success(), "{}", say(&out));
    assert!(fx.posted().is_empty(), "nothing was taken: {}", say(&out));
    assert_eq!(
        fx.spooled(),
        1,
        "the alert is kept on the PVC: {}",
        say(&out)
    );

    boss_testing::write_file(&root.join("sor-state"), "up");
    let out = fx.run("--tick", &[]);
    assert!(out.status.success(), "{}", say(&out));
    let posted = fx.posted();
    assert_eq!(
        posted.len(),
        1,
        "the kept alert is filed, once: {}",
        say(&out)
    );
    assert_eq!(fx.spooled(), 0, "{}", say(&out));
    // Review finding 5: this restart was already on the pod when the
    // watch first ran — on the live pod, the 10:31:52Z kill the item
    // itself records. It is filed, but it must not read as a new kill.
    let title = posted[0]["title"].as_str().expect("title");
    assert!(
        title.contains("restarted before the memory watch ran")
            && title.contains("restart predates this watch, no samples")
            && title.contains("OOMKilled (exit 137) at 2026-10-08T10:31:52Z"),
        "a restart found on the watch's first tick says so in its title: {title}"
    );
    assert_eq!(
        posted[0]["priority"], "standard",
        "and is not urgent: nothing new happened"
    );
    let detail = posted[0]["metadata"]["detail"].as_str().expect("detail");
    assert!(
        detail.starts_with("NOT A NEW KILL") && detail.contains("no sample precedes it"),
        "its body opens by saying what it is:\n{detail}"
    );
}

/// The dev container's state line with its item dated `secs` ago, so
/// the next tick stands outside (or inside) the one-item window.
fn date_the_last_item(fx: &Fixture, secs: u64) {
    let line = std::fs::read_to_string(fx.seen()).expect("the state line");
    let f: Vec<&str> = line.split_whitespace().collect();
    assert_eq!(f.len(), 3, "count, item time, frozen count: {line}");
    boss_testing::write_file(
        &fx.seen(),
        &format!("{} {} {}\n", f[0], unix_now() - secs, f[2]),
    );
}

/// Review finding 4: a container in CrashLoopBackOff restarts about
/// every five minutes, and one urgent item each buries the one that
/// matters. One item per container per window; the restarts in between
/// are counted into the next.
#[test]
fn a_restart_loop_files_one_item_a_window_and_counts_the_rest_into_the_next() {
    let root = boss_testing::scratch_dir("boss-dmw-window");
    let _guard = Scratch(root.clone());
    let fx = Fixture::new(&root);
    busy_pod(&fx);
    fx.set_pod(&pod(0, None));
    assert!(fx.run("--tick", &[]).status.success());
    let (_, finished) = date_the_sample_before_a_kill(&fx, 10);
    fx.set_pod(&pod(1, Some(("Error", 1, &finished))));
    let out = fx.run("--tick", &[]);
    assert_eq!(
        fx.posted().len(),
        1,
        "the first restart is an item: {}",
        say(&out)
    );

    // Two more restarts inside the hour: frozen, not filed.
    let later = rfc3339(unix_now());
    fx.set_pod(&pod(3, Some(("Error", 1, &later))));
    for _ in 0..2 {
        let out = fx.run("--tick", &[]);
        assert!(out.status.success(), "{}", say(&out));
    }
    assert_eq!(
        fx.posted().len() + fx.spooled(),
        1,
        "restarts inside the window raise no item of their own"
    );
    assert!(
        fx.dir
            .join("kills")
            .join(format!("dev-{}", later.replace(':', "")))
            .is_dir(),
        "but each one's ring is still frozen"
    );

    // The window passes: ONE item, covering both.
    date_the_last_item(&fx, 4000);
    let out = fx.run("--tick", &[]);
    assert!(out.status.success(), "{}", say(&out));
    let posted = fx.posted();
    assert_eq!(posted.len(), 2, "{}", say(&out));
    let title = posted[1]["title"].as_str().expect("title");
    let detail = posted[1]["metadata"]["detail"].as_str().expect("detail");
    assert!(
        title.contains("2 restarts since the last item") && detail.contains("restart count 1 -> 3"),
        "the next item counts the restarts it stands for: {title}\n{detail}"
    );
    assert_eq!(posted[1]["metadata"]["restarts_in_this_item"], "2");
}

/// The frozen rings under kills/, by name.
fn kills(fx: &Fixture) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(fx.dir.join("kills"))
        .map(|d| {
            d.filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn touch_at(path: &Path, epoch: u64) {
    let out = Command::new("touch")
        .arg("-d")
        .arg(format!("@{epoch}"))
        .arg(path)
        .output()
        .expect("touch");
    assert!(out.status.success(), "{}", say(&out));
}

/// Backlog 0143612a, measured 2026-10-14 on the alert item 89675645: its
/// `frozen_ring` — the whole ring from before an Oct 12 kill, the copy
/// the item says it was reduced from — was gone from the volume while
/// the item was still open. `freeze_ring` kept the newest five kill
/// directories and removed the rest unconditionally, and a container in
/// a restart loop freezes a ring for EVERY restart (the window above
/// files one item for many), so five restarts nobody itemised rotated
/// away the one ring an open item names. A ring an alert names is that
/// incident's: the rotation of the rest passes it by, and it joins the
/// rotation only when its item is read closed.
#[test]
fn a_ring_an_open_incident_names_outlives_the_rotation_and_leaves_when_it_closes() {
    let root = boss_testing::scratch_dir("boss-dmw-held-ring");
    let _guard = Scratch(root.clone());
    let fx = Fixture::new(&root);
    busy_pod(&fx);
    fx.set_pod(&pod(0, None));
    assert!(fx.run("--tick", &[]).status.success());
    let (_, finished) = date_the_sample_before_a_kill(&fx, 10);
    fx.set_pod(&pod(1, Some(("OOMKilled", 137, &finished))));
    let out = fx.run("--tick", &[]);
    assert!(out.status.success(), "{}", say(&out));
    assert_eq!(fx.posted().len(), 1, "{}", say(&out));
    let held = format!("dev-{}", finished.replace(':', ""));
    let incident = fx.dir.join("kills").join(format!("{held}.incident"));
    let line = std::fs::read_to_string(&incident).unwrap_or_default();
    assert!(
        line.starts_with("item item-1 "),
        "the ring the filed item names records that item: `{line}`\n{}",
        say(&out)
    );

    // Six more restarts inside the window: each ring is frozen, none is
    // filed, and the rotation keeps the newest five of THEM.
    let now = unix_now();
    for i in 2..=7u32 {
        let at = rfc3339(now + u64::from(i));
        fx.set_pod(&pod(i, Some(("Error", 1, &at))));
        let out = fx.run("--tick", &[]);
        assert!(out.status.success(), "{}", say(&out));
    }
    assert_eq!(fx.posted().len() + fx.spooled(), 1, "no item of their own");
    let names = kills(&fx);
    assert!(
        names.contains(&held),
        "the ring an open item names was rotated away by restarts nobody itemised: {names:?}"
    );
    assert_eq!(
        names.len(),
        6,
        "the held ring plus the newest five of the rest: {names:?}"
    );
    assert_eq!(state_field(&fx.state(), "memory_watch_rings_held"), "1");

    // The item is closed. Read at most once an hour, so the last read is
    // dated past that, and the next tick finds it closed.
    boss_testing::create_dir(&root.join("jobs"));
    boss_testing::write_file(
        &root.join("jobs").join("item-1"),
        r#"{"id":"item-1","status":"closed"}"#,
    );
    boss_testing::write_file(&incident, &format!("item item-1 {}\n", now - 4000));
    let out = fx.run("--tick", &[]);
    assert!(out.status.success(), "{}", say(&out));
    assert!(
        !incident.exists(),
        "a closed item holds nothing: {}",
        say(&out)
    );
    let released = std::fs::read_to_string(fx.dir.join("kills").join(format!("{held}.released")))
        .unwrap_or_default();
    assert!(
        released.starts_with("item item-1 closed "),
        "and the ring says what released it: `{released}`\n{}",
        say(&out)
    );
    assert_eq!(state_field(&fx.state(), "memory_watch_rings_held"), "0");

    // Released, it is the oldest of the rotation, and the next freeze
    // takes it.
    fx.set_pod(&pod(8, Some(("Error", 1, &rfc3339(now + 8)))));
    let out = fx.run("--tick", &[]);
    assert!(out.status.success(), "{}", say(&out));
    let names = kills(&fx);
    assert!(!names.contains(&held), "{names:?}");
    assert_eq!(names.len(), 5, "{names:?}");
    assert_eq!(
        state_field(&fx.state(), "memory_watch_rings_retired"),
        "0",
        "a ring released by its closed item is not a loss"
    );
}

/// The bound on what open items hold, and the record of what it costs.
/// Held rings cannot grow without end on a 40 GB volume, so past
/// KILLS_HELD_MAX (twenty, about 20 MB) the oldest is RETIRED — removed,
/// with a line naming it and its item in kills-retired.log and a count
/// `--state` carries, which the reclaim pass raises as a problem until a
/// person acknowledges it. Evidence an open incident names is never
/// removed in silence.
#[test]
fn past_its_bound_the_oldest_held_ring_is_retired_and_the_loss_is_on_the_record() {
    let root = boss_testing::scratch_dir("boss-dmw-held-bound");
    let _guard = Scratch(root.clone());
    let fx = Fixture::new(&root);
    let now = unix_now();
    for i in 0..20u64 {
        let d = fx.dir.join("kills").join(format!("dev-old-{i:02}"));
        boss_testing::create_dir(&d);
        boss_testing::write_file(&d.join("1.tsv"), "at\told\n");
        touch_at(&d, now - 10_000 + i);
        boss_testing::write_file(
            &fx.dir
                .join("kills")
                .join(format!("dev-old-{i:02}.incident")),
            &format!("item old-{i:02} {now}\n"),
        );
    }
    busy_pod(&fx);
    fx.set_pod(&pod(0, None));
    assert!(fx.run("--tick", &[]).status.success());
    let (_, finished) = date_the_sample_before_a_kill(&fx, 10);
    fx.set_pod(&pod(1, Some(("OOMKilled", 137, &finished))));
    let out = fx.run("--tick", &[]);
    assert!(out.status.success(), "{}", say(&out));
    assert_eq!(fx.posted().len(), 1, "{}", say(&out));

    let names = kills(&fx);
    assert!(
        !names.contains(&"dev-old-00".to_string())
            && names.contains(&"dev-old-01".to_string())
            && names.contains(&format!("dev-{}", finished.replace(':', ""))),
        "the oldest held ring is the one retired: {names:?}"
    );
    let log = std::fs::read_to_string(fx.dir.join("kills-retired.log")).unwrap_or_default();
    assert!(
        log.contains("dev-old-00") && log.contains("item old-00"),
        "the retirement names the ring and the item that held it: `{log}`\n{}",
        say(&out)
    );
    let state = fx.state();
    assert_eq!(
        state_field(&state, "memory_watch_rings_held"),
        "20",
        "{state}"
    );
    assert_eq!(
        state_field(&state, "memory_watch_rings_retired"),
        "1",
        "{state}"
    );

    let out = fx.run("--acknowledge", &[("BOSS_ACTOR", "operator:test")]);
    assert!(out.status.success(), "{}", say(&out));
    let state = fx.state();
    assert_eq!(
        state_field(&state, "memory_watch_rings_retired"),
        "0",
        "{state}\n{}",
        say(&out)
    );
    let said = state_field(&state, "memory_watch_acknowledged");
    assert!(
        said.contains("1 frozen ring(s) retired"),
        "the acknowledgement says what it cleared: {said}"
    );
    let stamp = state_field(&state, "memory_watch_acknowledgements");
    let kept = fx.dir.join("acknowledged").join(&stamp);
    assert!(
        std::fs::read_to_string(kept.join("kills-retired.log"))
            .unwrap_or_default()
            .contains("dev-old-00"),
        "the names of what was retired are kept with the acknowledgement: {}",
        kept.display()
    );
}

/// Review finding 3: alert-lib's replay stops at the first body the API
/// does not take and retries it every run — right for a dark API, wrong
/// for one that REFUSES, where one bad body held every later alert
/// behind it for ever, retried every 15 seconds.
#[test]
fn a_refused_alert_backs_off_lets_the_next_pass_and_is_moved_aside() {
    let root = boss_testing::scratch_dir("boss-dmw-refused");
    let _guard = Scratch(root.clone());
    let fx = Fixture::new(&root);
    busy_pod(&fx);
    fx.set_pod(&pod(1, Some(("OOMKilled", 137, "2026-10-08T10:31:52Z"))));
    boss_testing::write_file(&root.join("sor-state"), "refuse:OOMKilled");
    let out = fx.run("--tick", &[]);
    assert!(out.status.success(), "{}", say(&out));
    let kept = fx.alerts_in("alert-spool");
    assert_eq!(
        kept.len(),
        1,
        "a refused alert is still kept: {}",
        say(&out)
    );
    let tries = PathBuf::from(format!("{}.tries", kept[0].display()));
    let line = std::fs::read_to_string(&tries).expect("a refusal is counted beside the body");
    let f: Vec<u64> = line
        .split_whitespace()
        .map(|n| n.parse().expect("number"))
        .collect();
    assert_eq!(f[0], 1, "one refusal: {line}");
    assert!(
        f[1] >= unix_now() + 100,
        "and the next try is minutes away, not the next tick: {line}"
    );

    // A second alert behind it, of a shape the API takes.
    boss_testing::write_file(
        &fx.dir.join("alert-spool").join("9999999999-1-1.json"),
        r#"{"kind":"backlog-item","title":"the alert behind","owner_id":""}"#,
    );
    let out = fx.run("--tick", &[]);
    assert!(out.status.success(), "{}", say(&out));
    let posted = fx.posted();
    assert_eq!(posted.len(), 1, "{}", say(&out));
    assert_eq!(
        posted[0]["title"], "the alert behind",
        "the refused body does not hold the one behind it"
    );
    assert_eq!(
        std::fs::read_to_string(&tries).expect("tries"),
        line,
        "and inside its back-off the refused body is not sent again"
    );

    // Its fifth refusal moves it aside, kept, with the answer it got.
    boss_testing::write_file(&tries, "4 0\n");
    let out = fx.run("--tick", &[]);
    assert!(out.status.success(), "{}", say(&out));
    assert_eq!(fx.spooled(), 0, "{}", say(&out));
    let aside = fx.alerts_in("alert-refused");
    assert_eq!(aside.len(), 1, "{}", say(&out));
    let why = std::fs::read_to_string(format!("{}.why", aside[0].display())).expect("why");
    assert!(
        why.contains("refused 5 times, last answer 422") && why.contains("unprocessable"),
        "the refusal's own answer is kept beside the body: {why}"
    );
    let state = fx.state();
    assert!(
        state.contains("memory_watch_alerts_refused=1\n")
            && state.contains("memory_watch_alerts_kept=0\n"),
        "and it is counted where the reclaim's packet reads it:\n{state}"
    );
}

/// Residue of backlog 9ecd12ea, named in the excludes of car 97edf615:
/// the watch counted EVERY answer but 201 and no answer against the body,
/// so a 5xx, the machine gate refusing a stale or revoked token (401,
/// 403), a proxy or a timeout (407, 408) or a rate limit (429) moved a
/// kill alert aside after its fifth, about half an hour in, and it was
/// never filed though nothing was wrong with it. Those answers judge the
/// API or the caller, and every alert here is the same signer and kind,
/// so the one behind would get the same: the replay stops, counts
/// nothing, and the next tick tries again — alert-lib.sh's own predicate,
/// `alert_answer_judges_the_body`, decides which answers those are.
#[test]
fn an_answer_that_judges_the_caller_stops_the_send_and_counts_nothing_against_the_alert() {
    let root = boss_testing::scratch_dir("boss-dmw-caller");
    let _guard = Scratch(root.clone());
    let fx = Fixture::new(&root);
    busy_pod(&fx);
    fx.set_pod(&pod(1, Some(("OOMKilled", 137, "2026-10-08T10:31:52Z"))));
    boss_testing::write_file(&root.join("sor-state"), "down");
    assert!(fx.run("--tick", &[]).status.success());
    let kept = fx.alerts_in("alert-spool");
    assert_eq!(kept.len(), 1, "the kill alert is kept");
    // Its fourth refusal is already counted and due, so one more counted
    // answer would move it aside: the strongest place to be wrong.
    let tries = PathBuf::from(format!("{}.tries", kept[0].display()));
    boss_testing::write_file(&tries, "4 0\n");
    // A second alert behind it: the replay stops at the first.
    boss_testing::write_file(
        &fx.dir.join("alert-spool").join("9999999999-1-1.json"),
        r#"{"kind":"backlog-item","title":"the alert behind","owner_id":""}"#,
    );
    let posts = || {
        std::fs::read_to_string(root.join("posted.txt.headers"))
            .unwrap_or_default()
            .lines()
            .filter(|l| l.contains(" POST "))
            .count()
    };

    for code in ["401", "403", "407", "408", "429", "500", "503"] {
        boss_testing::write_file(&root.join("sor-state"), &format!("answer:{code}"));
        let before = posts();
        let out = fx.run("--tick", &[]);
        assert!(out.status.success(), "{code}: {}", say(&out));
        assert_eq!(
            fx.spooled(),
            2,
            "answer {code} judges the caller or the API: both alerts stay kept: {}",
            say(&out)
        );
        assert!(
            fx.alerts_in("alert-refused").is_empty(),
            "answer {code} moved nothing aside: {}",
            say(&out)
        );
        assert_eq!(
            std::fs::read_to_string(&tries).expect("tries"),
            "4 0\n",
            "answer {code} counted nothing against the body: {}",
            say(&out)
        );
        assert_eq!(
            posts() - before,
            1,
            "answer {code} stops the send at the first alert: {}",
            say(&out)
        );
        let said = say(&out);
        assert!(
            said.contains(&format!("answer {code}")) && said.contains("not this alert"),
            "and says why it stopped: {said}"
        );
    }

    // The first tick the API takes them, both are filed.
    boss_testing::write_file(&root.join("sor-state"), "up");
    let out = fx.run("--tick", &[]);
    assert!(out.status.success(), "{}", say(&out));
    assert_eq!(fx.posted().len(), 2, "{}", say(&out));
    assert_eq!(fx.spooled(), 0, "{}", say(&out));
    assert!(fx.alerts_in("alert-refused").is_empty());
}

/// One `key=value` line of `--state`, as the reclaim pass's
/// `watch_field` reads it: absent and empty are the same answer.
fn state_field(state: &str, key: &str) -> String {
    state
        .lines()
        .find_map(|l| l.strip_prefix(&format!("{key}=")))
        .unwrap_or_default()
        .to_string()
}

/// Review fb97dd5d of car 1fe37401, finding F1: counting nothing
/// against a body the API did not judge also means nothing ever ENDS the
/// hold — a persistent 403 to the watch's signer (while the reclaim's own
/// kind still files) or a 5xx one body causes kept the kill alert in the
/// spool with refused at 0, so no problem was raised and only the
/// spool's bound of 48 ever ended the silence. The hold is now measured
/// instead of counted: `--state` names the oldest kept alert's age and
/// the bound past which no refusal of its body explains it (twice the
/// back-offs a refused body can wait out before it is moved aside), and
/// the reclaim pass raises it — pinned in dev_scratch_reclaim_sh.rs.
#[test]
fn an_alert_held_by_answers_that_count_nothing_says_how_long_on_state() {
    let root = boss_testing::scratch_dir("boss-dmw-held");
    let _guard = Scratch(root.clone());
    let fx = Fixture::new(&root);
    busy_pod(&fx);
    fx.set_pod(&pod(0, None));

    let state = fx.state();
    assert_eq!(
        state_field(&state, "memory_watch_alerts_oldest_kept_age_s"),
        "",
        "nothing kept, no age: {state}"
    );
    // The bound is the watch's own fact, derived from its back-off: a
    // body refused four times waits 2 + 4 + 8 + 16 minutes, then goes
    // aside at the fifth; twice that is an hour.
    assert_eq!(
        state_field(&state, "memory_watch_alerts_held_after_s"),
        "3600",
        "{state}"
    );

    // A kill alert kept two hours ago, and the API answering 403 to it
    // on every tick since.
    let kept_at = unix_now() - 7200;
    boss_testing::create_dir(&fx.dir.join("alert-spool"));
    boss_testing::write_file(
        &fx.dir
            .join("alert-spool")
            .join(format!("{kept_at}-1-1.json")),
        r#"{"kind":"backlog-item","title":"the held kill alert","owner_id":""}"#,
    );
    boss_testing::write_file(&root.join("sor-state"), "answer:403");
    for _ in 0..3 {
        let out = fx.run("--tick", &[]);
        assert!(out.status.success(), "{}", say(&out));
        assert_eq!(fx.spooled(), 1, "a 403 keeps it: {}", say(&out));
        assert!(fx.alerts_in("alert-refused").is_empty(), "{}", say(&out));
    }
    let state = fx.state();
    assert!(
        state.contains("memory_watch_alerts_refused=0\n")
            && state.contains("memory_watch_alerts_kept=1\n"),
        "still nothing is counted against the body: {state}"
    );
    let age: u64 = state_field(&state, "memory_watch_alerts_oldest_kept_age_s")
        .parse()
        .unwrap_or_else(|_| panic!("the oldest kept alert's age is a whole number: {state}"));
    assert!(
        (7200..7200 + 120).contains(&age),
        "the age is read from when the alert was kept, not from its last try: {age}\n{state}"
    );

    // The tick the API takes it, the hold is over and the age is gone.
    boss_testing::write_file(&root.join("sor-state"), "up");
    let out = fx.run("--tick", &[]);
    assert!(out.status.success(), "{}", say(&out));
    assert_eq!(fx.posted().len(), 1, "{}", say(&out));
    let state = fx.state();
    assert_eq!(
        state_field(&state, "memory_watch_alerts_oldest_kept_age_s"),
        "",
        "{state}"
    );
}

/// The operator call recorded on backlog 9ecd12ea when car 7849553a was
/// released: refused and dropped counts do not clear themselves, so once
/// above zero every hourly pass files a failed packet until someone
/// empties `alert-refused/` and removes `alerts-dropped` BY HAND — an
/// act nothing records, which also throws away the refused bodies. The
/// acknowledgement is a verb: it keeps the bodies, zeroes the count,
/// says who and when, and stays named on the state the reclaim's packet
/// reads until that packet has carried it. The count is never aged out.
#[test]
fn refused_and_dropped_alerts_are_acknowledged_by_a_verb_that_keeps_the_bodies() {
    let root = boss_testing::scratch_dir("boss-dmw-ack");
    let _guard = Scratch(root.clone());
    let fx = Fixture::new(&root);
    let aside = fx.dir.join("alert-refused");
    boss_testing::create_dir(&aside);
    boss_testing::write_file(&aside.join("1-1-1.json"), r#"{"title":"a refused kill"}"#);
    boss_testing::write_file(
        &aside.join("1-1-1.json.why"),
        "refused 5 times, last answer 422:\n",
    );
    boss_testing::write_file(&fx.dir.join("alerts-dropped"), "3\n");

    let state = fx.state();
    assert!(
        state.contains("memory_watch_alerts_refused=1\n")
            && state.contains("memory_watch_alerts_dropped=3\n"),
        "{state}"
    );
    assert!(
        state.contains(&format!("memory_watch_dir={}\n", fx.dir.display())),
        "the packet names where the watch keeps what it holds:\n{state}"
    );

    let out = fx.run("--acknowledge", &[("BOSS_ACTOR", "operator:test")]);
    assert!(out.status.success(), "{}", say(&out));
    let state = fx.state();
    assert!(
        state.contains("memory_watch_alerts_refused=0\n")
            && state.contains("memory_watch_alerts_dropped=0\n"),
        "the counts are zeroed by the act, not by time:\n{state}\n{}",
        say(&out)
    );
    let stamp = state
        .lines()
        .find_map(|l| l.strip_prefix("memory_watch_acknowledgements="))
        .unwrap_or_default()
        .to_string();
    assert!(
        !stamp.is_empty() && !stamp.contains(' '),
        "one acknowledgement waits for a packet:\n{state}"
    );
    let said = state
        .lines()
        .find_map(|l| l.strip_prefix("memory_watch_acknowledged="))
        .unwrap_or_default();
    assert!(
        said.contains("operator:test")
            && said.contains("1 refused")
            && said.contains("3 dropped")
            && said.contains(&stamp),
        "and says who acknowledged what:\n{state}"
    );
    let kept = fx.dir.join("acknowledged").join(&stamp);
    assert!(
        kept.join("1-1-1.json").is_file() && kept.join("1-1-1.json.why").is_file(),
        "the refused body and its answer are kept, not deleted: {}",
        kept.display()
    );

    // Once the reclaim's packet has carried it, the pass says so and the
    // acknowledgement stops riding every later packet.
    let out = fx.run("--recorded", &[]);
    assert!(
        !out.status.success(),
        "--recorded with no acknowledgement named is refused: {}",
        say(&out)
    );
    // A name that is not an acknowledgement's (a path out of the
    // directory) is skipped, never followed.
    let out = Command::new("bash")
        .arg(repo_root().join("infra/cluster/dev-memory-watch.sh"))
        .args(["--recorded", &stamp, "../../etc"])
        .env("WORK_MOUNT", root.join("work"))
        .output()
        .expect("run --recorded");
    assert!(out.status.success(), "{}", say(&out));
    let state = fx.state();
    assert!(
        state.contains("memory_watch_acknowledgements=\n")
            && state.contains("memory_watch_acknowledged=\n"),
        "{state}"
    );
    assert!(
        kept.join("1-1-1.json").is_file(),
        "recording keeps the body"
    );

    // Nothing to acknowledge is said, and makes no record.
    let out = fx.run("--acknowledge", &[("BOSS_ACTOR", "operator:test")]);
    assert!(out.status.success(), "{}", say(&out));
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("nothing to acknowledge"),
        "{}",
        say(&out)
    );
    let acks = std::fs::read_dir(fx.dir.join("acknowledged"))
        .expect("acknowledged/")
        .count();
    assert_eq!(acks, 1, "an empty acknowledgement records nothing");
}

/// Review 1cd6b7a1 of car 9f9bba8d, finding 1 (backlog 8aff9d6b): a
/// dropped count that is not a number is printed raw by `--state`, and
/// the reclaim pass flags any count that is not all zeroes, so it files a
/// failed packet every hour; `--acknowledge` read the same file as 0 and
/// said there was nothing to acknowledge — the verb the problem names
/// could not clear it. Now anything `--state` would print as other than
/// zero is acknowledged, and the file is kept as it was, not retyped.
#[test]
fn a_dropped_count_that_is_not_a_number_is_acknowledged_and_kept_as_it_was() {
    let root = boss_testing::scratch_dir("boss-dmw-ack-garbled");
    let _guard = Scratch(root.clone());
    let fx = Fixture::new(&root);
    boss_testing::create_dir(&fx.dir);
    boss_testing::write_file(&fx.dir.join("alerts-dropped"), "garbled\n");
    let state = fx.state();
    assert_eq!(
        state_field(&state, "memory_watch_alerts_dropped"),
        "garbled",
        "the reclaim pass reads this as a problem: {state}"
    );

    let out = fx.run("--acknowledge", &[("BOSS_ACTOR", "operator:test")]);
    assert!(out.status.success(), "{}", say(&out));
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("nothing to acknowledge"),
        "the count the problem names is something to acknowledge:\n{}",
        say(&out)
    );
    let state = fx.state();
    assert_eq!(
        state_field(&state, "memory_watch_alerts_dropped"),
        "0",
        "{state}"
    );
    let stamp = state_field(&state, "memory_watch_acknowledgements");
    assert!(!stamp.is_empty(), "it waits for a packet: {state}");
    let said = state_field(&state, "memory_watch_acknowledged");
    assert!(
        said.contains("operator:test") && said.contains("not a number"),
        "the record says the count could not be read, not that it was 0: {said}"
    );
    assert_eq!(
        std::fs::read_to_string(fx.dir.join("acknowledged").join(&stamp).join("dropped"))
            .unwrap_or_default(),
        "garbled\n",
        "the file is moved as it was — the one copy of what it said"
    );
}

/// Review 1cd6b7a1, finding 2: the bodies were moved BEFORE the record
/// and the `unrecorded` mark were written, so a move that failed part-way
/// left an acknowledged/ folder holding refused bodies and neither file —
/// invisible on `--state`, and deleted by the ACK_KEEP bound on a later
/// acknowledgement, the one copy of each kill alert with it. The mark is
/// written first, and a part-way stop says so in the record.
#[test]
fn an_acknowledgement_that_stops_part_way_is_on_the_state_and_outlives_the_bound() {
    let root = boss_testing::scratch_dir("boss-dmw-ack-part");
    let _guard = Scratch(root.clone());
    let fx = Fixture::new(&root);
    let aside = fx.dir.join("alert-refused");
    boss_testing::create_dir(&aside);
    boss_testing::write_file(&aside.join("1-1-1.json"), r#"{"title":"moved"}"#);
    boss_testing::write_file(&aside.join("1-1-1.json.why"), "refused 5 times\n");
    boss_testing::write_file(&aside.join("2-stuck-1.json"), r#"{"title":"stuck"}"#);
    boss_testing::write_file(&fx.dir.join("alerts-dropped"), "3\n");
    // An `mv` that refuses the second body, as a full or failing volume
    // would; every other move is the real one.
    let stub = root.join("stub-mv");
    boss_testing::create_dir(&stub);
    boss_testing::write_exec(
        &stub.join("mv"),
        concat!(
            "#!/usr/bin/env bash\n",
            "case \"$*\" in *stuck*) echo 'mv: refused by the fixture' >&2; exit 1 ;; esac\n",
            "PATH=${PATH#*:} exec mv \"$@\"\n",
        ),
    );
    let path = format!(
        "{}:{}:{}",
        stub.display(),
        root.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = fx.run(
        "--acknowledge",
        &[("BOSS_ACTOR", "operator:test"), ("PATH", &path)],
    );
    assert!(
        !out.status.success(),
        "a part-way acknowledgement is not a success: {}",
        say(&out)
    );
    let state = fx.state();
    let stamp = state_field(&state, "memory_watch_acknowledgements");
    assert!(
        !stamp.is_empty() && !stamp.contains(' '),
        "the folder holding the moved body is named on the state:\n{state}"
    );
    let kept = fx.dir.join("acknowledged").join(&stamp);
    assert!(kept.join("1-1-1.json").is_file(), "{}", kept.display());
    let said = state_field(&state, "memory_watch_acknowledged");
    assert!(
        said.contains("part-way") && said.contains("1 of 2"),
        "the record says what was moved and what was not: {said}"
    );
    assert!(
        state.contains("memory_watch_alerts_refused=1\n")
            && state.contains("memory_watch_alerts_dropped=3\n"),
        "what was not moved is still counted, so the problem stays:\n{state}"
    );

    // Twenty later acknowledgements no packet still waits on: the bound
    // keeps the newest ACK_KEEP and never one with its mark.
    for n in 0..20 {
        let later = fx
            .dir
            .join("acknowledged")
            .join(format!("29990101T0000{n:02}Z"));
        boss_testing::create_dir(&later);
        boss_testing::write_file(&later.join("record"), "acknowledged long ago\n");
    }
    let out = fx.run("--acknowledge", &[("BOSS_ACTOR", "operator:test")]);
    assert!(out.status.success(), "{}", say(&out));
    assert!(
        kept.join("1-1-1.json").is_file(),
        "the bound does not delete a body no packet has carried: {}",
        kept.display()
    );
    assert!(fx.alerts_in("alert-refused").is_empty(), "{}", say(&out));
}

/// Review 1cd6b7a1, finding 3: `--state` printed every acknowledged/
/// folder's name unfiltered, so a hand-made name holding a newline added
/// a line of its own to the packet's key=value block. Only a name
/// `--recorded` would take back is listed.
#[test]
fn an_acknowledged_folder_whose_name_is_not_one_adds_no_line_to_the_state() {
    let root = boss_testing::scratch_dir("boss-dmw-ack-name");
    let _guard = Scratch(root.clone());
    let fx = Fixture::new(&root);
    let odd = fx
        .dir
        .join("acknowledged")
        .join("odd\nmemory_watch=sampling");
    boss_testing::create_dir(&odd);
    boss_testing::write_file(&odd.join("record"), "made by hand\n");
    boss_testing::write_file(&odd.join("unrecorded"), "");
    let state = fx.state();
    assert!(
        state.lines().all(|l| l.starts_with("memory_watch")
            && l.contains('=')
            && !l.starts_with("memory_watch=sampling")),
        "every line is one of the watch's own keys:\n{state}"
    );
    assert_eq!(
        state_field(&state, "memory_watch_acknowledgements"),
        "",
        "{state}"
    );
}

/// Delta review, N5: a send waits on the system of record — the owner
/// read up to 5 s, each POST up to 15 — and a tick that sent before it
/// sampled left a healthy loop with no sample for that long, which the
/// pass that had just started it could record as "not sampling".
#[test]
fn a_tick_samples_before_it_sends() {
    let root = boss_testing::scratch_dir("boss-dmw-order");
    let _guard = Scratch(root.clone());
    let fx = Fixture::new(&root);
    busy_pod(&fx);
    fx.set_pod(&pod(1, Some(("OOMKilled", 137, "2026-10-08T10:31:52Z"))));
    let out = fx.run("--tick", &[]);
    assert!(out.status.success(), "{}", say(&out));
    assert_eq!(fx.posted().len(), 1, "{}", say(&out));
    let at_send = std::fs::read_to_string(root.join("posted.txt.ring")).expect("the stub's count");
    assert_eq!(
        at_send.trim(),
        "1",
        "this tick's sample is in the ring by the time its alert is sent: {}",
        say(&out)
    );
}

#[test]
fn the_loops_own_log_is_trimmed_on_its_size_every_tick() {
    let root = boss_testing::scratch_dir("boss-dmw-log");
    let _guard = Scratch(root.clone());
    let fx = Fixture::new(&root);
    busy_pod(&fx);
    fx.set_pod(&pod(0, None));
    boss_testing::create_dir(&fx.dir);
    let log = fx.dir.join("watch.log");
    boss_testing::write_file(
        &log,
        &format!("{}THE-NEWEST-LINE\n", "old line\n".repeat(500)),
    );
    let out = fx.run("--tick", &[("BOSS_DEV_MEMORY_LOG_MAX_BYTES", "1000")]);
    assert!(out.status.success(), "{}", say(&out));
    let kept = std::fs::read_to_string(&log).expect("log");
    assert!(
        kept.len() <= 1000 && kept.ends_with("THE-NEWEST-LINE\n"),
        "a log over its bound keeps its tail: {} bytes",
        kept.len()
    );
}

fn watch_pid(fx: &Fixture) -> Option<u32> {
    std::fs::read_to_string(fx.dir.join("watch.pid"))
        .ok()?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// Is `pid` a running loop? Its command line, not its /proc entry: a
/// killed loop nobody reaps keeps the entry with an empty cmdline.
fn alive(pid: u32) -> bool {
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .map(|c| {
            let c = String::from_utf8_lossy(&c);
            c.contains("dev-memory-watch.sh") && c.contains("--loop")
        })
        .unwrap_or(false)
}

#[test]
fn the_loop_starts_once_and_only_outside_the_container_it_watches() {
    let root = boss_testing::scratch_dir("boss-dmw-ensure");
    let _guard = Scratch(root.clone());
    let fx = Fixture::new(&root);
    fx.set_pod(&pod(0, None));
    let slow = [("BOSS_DEV_MEMORY_INTERVAL_S", "1")];

    // Inside the dev container its own processes show the cgroup root:
    // none carries the container's id.
    process(&fx.proc, 100, "", 1_000, 10, &["rustc"]);
    process(&fx.proc, 200, PG_ID, 1_000, 10, &["postgres"]);
    let out = fx.run("--ensure", &slow);
    assert!(out.status.success(), "{}", say(&out));
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("not started here")
            && watch_pid(&fx).is_none(),
        "a sampler inside the watched container dies with the evidence, so none starts: {}",
        say(&out)
    );
    assert!(
        fx.state()
            .contains("memory_watch=not sampling: no loop is alive\n"),
        "and with no loop the state says so:\n{}",
        fx.state()
    );

    // From the sidecar, the dev container's processes carry its id.
    process(&fx.proc, 101, DEV_ID, 1_000, 10, &["rustc"]);
    let out = fx.run("--ensure", &slow);
    assert!(out.status.success(), "{}", say(&out));
    let first =
        watch_pid(&fx).unwrap_or_else(|| panic!("a started loop leaves its pid: {}", say(&out)));
    let _first = Loop(first);
    // Polled: `--ensure` returns as soon as it has forked, and until the
    // child has exec'd its command line is not yet the loop's.
    let running = (0..50).any(|_| {
        std::thread::sleep(std::time::Duration::from_millis(100));
        alive(first)
    });
    assert!(running, "the loop outlives the --ensure that started it");
    // Review finding 2: started is a claim, a sample is the evidence —
    // and both are readable by something that is not standing in the
    // sidecar. This is what the reclaim's packet carries.
    let state = fx.state();
    assert!(
        state.contains("memory_watch=sampling\n")
            && state.contains(&format!("memory_watch_pid={first}\n"))
            && state.contains("memory_watch_generation=1\n")
            && state.contains("memory_watch_ensure=started (pid "),
        "a started loop reads as sampling, with its pid and what --ensure decided:\n{state}"
    );

    let out = fx.run("--ensure", &slow);
    assert!(out.status.success(), "{}", say(&out));
    assert_eq!(
        watch_pid(&fx),
        Some(first),
        "a second --ensure finds the first loop and starts none: {}",
        say(&out)
    );

    // A loop of another generation is replaced, which is how a change
    // to the loop itself reaches a pod nobody restarts.
    boss_testing::write_file(&fx.dir.join("watch.pid"), &format!("{first} 0\n"));
    let out = fx.run("--ensure", &slow);
    assert!(out.status.success(), "{}", say(&out));
    let second = watch_pid(&fx).expect("the replacement's pid");
    let _second = Loop(second);
    assert_ne!(second, first, "{}", say(&out));
    let gone = (0..50).any(|_| {
        std::thread::sleep(std::time::Duration::from_millis(100));
        !alive(first)
    });
    assert!(gone, "the loop of the old generation is stopped");
    assert!(
        !fx.ring().is_empty() || {
            std::thread::sleep(std::time::Duration::from_secs(2));
            !fx.ring().is_empty()
        },
        "the running loop writes samples"
    );
}
