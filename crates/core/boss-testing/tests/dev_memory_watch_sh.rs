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

use boss_testing::repo_root;
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
/// needle; anything else appends the body to `$STUB_POSTED` as one line
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
            "    esac\n",
            "    if [ -n \"${refused:-}\" ]; then\n",
            "        echo '{\"error\":\"unprocessable\"}' > \"$out\"; printf 422; exit 0\n",
            "    fi\n",
            "    printf '%s\\n' \"$body\" >> \"$STUB_POSTED\"\n",
            "    printf 201\n",
            "    exit 0\n",
            "fi\n",
            "case \"$url\" in\n",
            "    */pods/*) [ -f \"$STUB_POD\" ] || exit 7; cat \"$STUB_POD\" > \"$out\" ;;\n",
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
