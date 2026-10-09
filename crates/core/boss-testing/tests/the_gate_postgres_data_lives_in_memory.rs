//! A gate's Postgres data directory is memory, sized from a measurement,
//! and the memory it can take is in the numbers the scheduler and the
//! kernel judge the pod by (backlog 9cd74fd5, design 3a8a44c1 step 2).
//!
//! WHY. Design 8457c07b moved a gate's disk I/O to the gate volume (w-1's
//! second NVMe) and left one writer behind: each gate's Postgres kept its
//! data directory in an emptyDir, and the kubelet puts every emptyDir on
//! the install disk's EPHEMERAL, beside the dev pod's builders. Measured on
//! backlog 17f6170c: a gate's test check (30 of its 37 minutes alone)
//! barely writes the gate drive and writes the Samsung, with write await
//! spikes of 40 to 143 ms there, and two false reds on loopback timeouts on
//! 2026-10-07 came with three gates plus five builders. The database
//! already runs with fsync, synchronous_commit and full_page_writes off:
//! its data is disposable by design, so it belongs in memory.
//!
//! WHAT A MEMORY VOLUME COSTS, AND WHY EACH NUMBER IS PINNED. A tmpfs
//! page is charged to the cgroup of the process that wrote it (the
//! postgres container) and is not reclaimable: there is no swap. So:
//!
//!   * the volume has a `sizeLimit` — without one a tmpfs is as large as
//!     the pod's memory limit, and a leak of scratch databases takes the
//!     node's memory instead of filling a volume;
//!   * the postgres container's memory LIMIT covers the whole volume, the
//!     /dev/shm volume beside it and the server's own processes — else the
//!     kernel OOM-kills the database BELOW the volume's size, and a gate
//!     reads that as its tests failing;
//!   * its memory REQUEST covers the measured peak, so the scheduler
//!     reserves what a gate really takes and an ordinary gate is not in
//!     the kubelet's "usage exceeds request" class under memory pressure
//!     — whose other face is that the dev pod, over its own smaller
//!     request, is then the likely first eviction (the manifest says so);
//!   * the policy's bays plus the train gate, at those requests, fit w-1
//!     beside the dev pod, and every one of them FILLING its volume still
//!     leaves the node room.
//!
//! THE MEASUREMENT (2026-10-07 23:18Z to 2026-10-08 00:42Z, three `--auto`
//! gates on w-1: two car gates and the 23:36 train gate). Read from each
//! running gate's own Postgres every 20 to 25 s — the sum of
//! `pg_database_size` over every database plus the size of `pg_wal` —
//! because the kubelet's stats are not readable by the dev session. The
//! shape repeated on all three: about ten minutes in, 110 to 120 scratch
//! databases appear within 90 seconds (16 MB each, copies of the schema
//! template) and stay for most of the test check; the data directory
//! peaks at 1.78, 1.85 and 1.93 GiB, with pg_wal never above 0.08 GiB, and
//! the train gate ended at 0.19 GiB once they were dropped — which is why
//! the receipt carries the PEAK and a watcher reads between boundaries.
//! The largest reading is [`MEASURED_PEAK_GIB`].
//! NOT measured: a full-scope gate (`--mode full`, every crate's suite),
//! the first ten minutes of two of the three, and anything between two
//! reads. That is what the factor of three is for, and why the
//! receipt now carries the number (`runtime_evidence.pgdata`), so the
//! limit is checked on every gate rather than argued once.
//!
//! tree-wide pin — it reads every manifest under infra/ for a pod that
//! runs a throwaway Postgres, which no changed-file map attributes to this
//! crate, so every scoped gate runs it whatever its scope
//! (`tree_wide_pins` in infra/gate.sh).

use boss_testing::rbac::{self, Node, Value};
use boss_testing::repo_root;

const RUNNER: &str = "infra/gate-runner/gate-runner.yaml";
const DEV: &str = "infra/cluster/manifests/boss-dev.yaml";
const CONSIST: &str = "infra/consist-worker/job.json";
const POLICY: &str = "infra/platform/delivery-policy/train-conductor.toml";

/// The largest gate Postgres data directory measured (see the header):
/// 2,072,238,410 bytes, 126 databases, on gate-run for
/// `fix/in-cluster-holders-…` at 2026-10-07 23:41:35Z.
const MEASURED_PEAK_GIB: f64 = 1.93;

/// The volume is at least this many times the measured peak. Three,
/// because the measurement is of scoped gates only.
const SAFETY_FACTOR: f64 = 3.0;

/// What the Postgres server's own processes may take beside its two
/// memory volumes: 128 MB of shared buffers and up to 100 backends. The
/// container's whole limit was 4Gi while its data was on disk, most of it
/// reclaimable page cache; nothing on a tmpfs is cached twice.
const SERVER_ROOM_GIB: f64 = 2.0;

/// w-1's memory, read from /proc/meminfo in the dev pod, which runs on
/// w-1 (MemTotal 65,735,920 kB, 2026-10-07). The dev session cannot get
/// nodes, so the kubelet's allocatable is not read; the pins below leave
/// a tenth of this for the kubelet's reservation and the node's own pods.
/// That tenth is an ASSUMPTION, not a reading, and the pin counts the
/// pods of this namespace only (review bc011e92, F8).
const W1_MEMORY_GIB: f64 = 62.69;

/// MemAvailable on w-1 with FIVE gates running, 44 GB (backlog 17f6170c,
/// the measurement the design quotes), in GiB.
const W1_AVAILABLE_UNDER_FIVE_GATES_GIB: f64 = 40.98;

fn read(rel: &str) -> String {
    let path = repo_root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// A memory or storage quantity in GiB. `Gi` and `Mi` are the units these
/// manifests use; any other is a red naming it, never a zero.
fn gib(quantity: &str) -> f64 {
    let q = quantity.trim();
    if let Some(n) = q.strip_suffix("Gi") {
        n.parse().unwrap_or_else(|e| panic!("quantity {q}: {e}"))
    } else if let Some(n) = q.strip_suffix("Mi") {
        n.parse::<f64>()
            .unwrap_or_else(|e| panic!("quantity {q}: {e}"))
            / 1024.0
    } else {
        panic!("a quantity in a unit this pin does not read: {q}")
    }
}

fn items(node: Option<&Node>) -> Vec<&Node> {
    match node.map(|n| &n.value) {
        Some(Value::Seq(items)) => items.iter().collect(),
        _ => Vec::new(),
    }
}

fn text<'a>(node: &'a Node, key: &str) -> Option<&'a str> {
    node.get(key).and_then(Node::str)
}

/// Every container of a pod spec, init containers (where a native sidecar
/// lives) included.
fn containers(pod: &Node) -> Vec<&Node> {
    items(pod.get("initContainers"))
        .into_iter()
        .chain(items(pod.get("containers")))
        .collect()
}

fn env<'a>(container: &'a Node, name: &str) -> Option<&'a str> {
    items(container.get("env"))
        .into_iter()
        .find(|e| text(e, "name") == Some(name))
        .and_then(|e| text(e, "value"))
}

/// A container that runs a Postgres SERVER whose data nobody will read
/// again: it names a data directory and turns durability off.
fn is_throwaway_postgres(container: &Node) -> bool {
    env(container, "PGDATA").is_some()
        && items(container.get("args"))
            .iter()
            .any(|a| a.str() == Some("fsync=off"))
}

/// The pod volume a container's `PGDATA` sits on: the mount whose path is
/// the longest prefix of the data directory.
fn pgdata_volume<'a>(pod: &'a Node, container: &Node) -> &'a Node {
    let data = env(container, "PGDATA").expect("the caller checked PGDATA");
    let mount = items(container.get("volumeMounts"))
        .into_iter()
        .filter(|m| {
            text(m, "mountPath").is_some_and(|p| {
                data == p || data.starts_with(&format!("{}/", p.trim_end_matches('/')))
            })
        })
        .max_by_key(|m| text(m, "mountPath").map_or(0, str::len))
        .unwrap_or_else(|| {
            panic!(
                "PGDATA {data} is under no volume mount: it would be the container's writable \
                 layer, which is the install disk"
            )
        });
    let name = text(mount, "name").expect("a mount names its volume");
    items(pod.get("volumes"))
        .into_iter()
        .find(|v| text(v, "name") == Some(name))
        .unwrap_or_else(|| panic!("volume {name} is mounted and not declared"))
}

fn memory(container: &Node, which: &str) -> f64 {
    let q = container
        .path(&["resources", which, "memory"])
        .and_then(Node::str)
        .unwrap_or_else(|| {
            panic!(
                "container {:?} declares no memory {which}",
                text(container, "name")
            )
        });
    gib(q)
}

fn size_limit(volume: &Node) -> f64 {
    gib(volume
        .path(&["emptyDir", "sizeLimit"])
        .and_then(Node::str)
        .unwrap_or_else(|| {
            panic!(
                "volume {:?} has no sizeLimit: a tmpfs without one is as large as the pod's \
                 memory limit, so leaked scratch databases take the node's memory",
                text(volume, "name")
            )
        }))
}

/// The gate Job's pod spec, read with the estate's YAML reader — which
/// REFUSES a shape it does not read rather than reading it as empty.
fn runner_pod() -> Node {
    let docs = rbac::parse_stream(RUNNER, &read(RUNNER)).unwrap_or_else(|e| panic!("{e}"));
    docs.into_iter()
        .find(|d| text(d, "kind") == Some("Job"))
        .and_then(|job| job.path(&["spec", "template", "spec"]).cloned())
        .unwrap_or_else(|| panic!("{RUNNER} carries no Job with a pod template"))
}

fn named<'a>(pod: &'a Node, name: &str) -> &'a Node {
    containers(pod)
        .into_iter()
        .find(|c| text(c, "name") == Some(name))
        .unwrap_or_else(|| panic!("{RUNNER} has no container named {name}"))
}

/// THE DATA DIRECTORY IS NOT ON THE INSTALL DISK.
#[test]
fn the_gate_postgres_data_directory_is_a_bounded_memory_volume() {
    let pod = runner_pod();
    let pg = named(&pod, "postgres");
    assert!(
        is_throwaway_postgres(pg),
        "{RUNNER}: the postgres sidecar no longer reads as a server with durability off — \
         this pin's subject moved, and so did the reason its data may live in memory"
    );
    let volume = pgdata_volume(&pod, pg);
    assert_eq!(
        volume.path(&["emptyDir", "medium"]).and_then(Node::str),
        Some("Memory"),
        "{RUNNER}: the gate's PGDATA volume `{}` must be `emptyDir: {{medium: Memory, …}}`. \
         A plain emptyDir is a directory on the node's EPHEMERAL — the install disk, beside \
         the dev pod's builders — and a gate's 30-minute test check wrote there with 40 to \
         143 ms write await while builders compiled (backlog 17f6170c). The fallback, if the \
         receipts show memory pressure, is the gate volume (a `runs/<pod>` subPath of the \
         `gate` claim), never EPHEMERAL again",
        text(volume, "name").unwrap_or("?")
    );
    let limit = size_limit(volume);
    assert!(
        limit >= SAFETY_FACTOR * MEASURED_PEAK_GIB,
        "the PGDATA volume's sizeLimit is {limit} GiB, under {SAFETY_FACTOR} x the measured \
         {MEASURED_PEAK_GIB} GiB peak. At the limit Postgres gets `No space left on device` \
         and every DB-backed test after it fails, and that run is recorded `failed` (the \
         receipt marks the volume full; nothing rewrites the verdict): a limit near the peak \
         turns an ordinary gate into a red on a correct branch"
    );
}

/// THE MEMORY IT CAN TAKE IS IN THE CONTAINER'S NUMBERS.
#[test]
fn the_postgres_container_is_sized_for_the_memory_its_volumes_take() {
    let pod = runner_pod();
    let pg = named(&pod, "postgres");
    // Every memory-backed volume this container mounts is charged to it.
    let mounted: Vec<&str> = items(pg.get("volumeMounts"))
        .into_iter()
        .filter_map(|m| text(m, "name"))
        .collect();
    let tmpfs: f64 = items(pod.get("volumes"))
        .into_iter()
        .filter(|v| text(v, "name").is_some_and(|n| mounted.contains(&n)))
        .filter(|v| v.path(&["emptyDir", "medium"]).and_then(Node::str) == Some("Memory"))
        .map(size_limit)
        .sum();
    let data = size_limit(pgdata_volume(&pod, pg));
    assert!(
        tmpfs >= data && data > 0.0,
        "the memory volumes read ({tmpfs} GiB) do not include PGDATA ({data} GiB)"
    );
    let limit = memory(pg, "limits");
    assert!(
        limit >= tmpfs + SERVER_ROOM_GIB,
        "the postgres container's memory limit is {limit} GiB; its memory volumes may hold \
         {tmpfs} GiB and the server needs {SERVER_ROOM_GIB} GiB beside them. Below that sum \
         the kernel OOM-kills the database BEFORE the volume is full, the gate container \
         cannot see why, and the run reads as tests failing on a correct branch"
    );
    let request = memory(pg, "requests");
    assert!(
        request >= MEASURED_PEAK_GIB + 0.5,
        "the postgres container requests {request} GiB of memory; a gate's data directory \
         was measured at {MEASURED_PEAK_GIB} GiB and the server asked 512Mi before its data \
         moved to memory. A request under real use puts every ordinary gate in the kubelet's \
         'usage exceeds request' class, first to be evicted under memory pressure"
    );
    assert!(
        request <= limit,
        "request {request} GiB above limit {limit} GiB"
    );
}

/// THE BAYS FIT w-1's MEMORY, BY REQUEST AND AT THE WORST CASE. The same
/// arithmetic `a_gate_requests_what_the_new_layout_uses_and_the_bays_fit`
/// pins for disk: the policy's bays plus the train gate, which launches
/// outside the bay bound (0f4224bc).
#[test]
fn the_bays_and_the_train_gate_fit_the_build_nodes_memory() {
    let pod = runner_pod();
    let gate_pod: f64 = containers(&pod)
        .into_iter()
        .map(|c| memory(c, "requests"))
        .sum();
    let bays: f64 = read(POLICY)
        .lines()
        .find_map(|l| l.trim().strip_prefix("gate_max_concurrent ="))
        .expect("train-conductor.toml declares gate_max_concurrent")
        .trim()
        .parse()
        .expect("gate_max_concurrent is a number");
    let dev_docs = rbac::parse_stream(DEV, &read(DEV)).unwrap_or_else(|e| panic!("{e}"));
    let dev: f64 = dev_docs
        .iter()
        .filter(|d| text(d, "kind") == Some("Deployment"))
        .filter_map(|d| d.path(&["spec", "template", "spec"]))
        .flat_map(containers)
        .map(|c| memory(c, "requests"))
        .sum();
    assert!(dev > 0.0, "the dev pod's memory requests were read ({DEV})");

    let gates = bays + 1.0;
    let admitted = gates * gate_pod + dev;
    assert!(
        admitted <= 0.9 * W1_MEMORY_GIB,
        "{bays} bays + the train gate at {gate_pod} GiB of memory requests each, beside the \
         dev pod's {dev:.2} GiB, is {admitted:.1} GiB of w-1's {W1_MEMORY_GIB}: the last \
         gate would sit Pending and read as a running slot. (The bound is nine tenths of \
         MemTotal, standing in for an allocatable the dev session cannot read, and only \
         this namespace's pods are counted.)"
    );
    // WHAT THIS PIN DOES NOT HOLD, said where it is computed: one more
    // gate than the policy's bays plus the train gate. A dock re-gate
    // beside all four is a fifth, and the manifest's comment says it
    // would sit Pending. That sentence is held to the numbers here, so it
    // cannot outlive a resize in either direction.
    let fifth = (gates + 1.0) * gate_pod + dev;
    let says_pending = read(RUNNER).contains("a FIFTH concurrent gate");
    assert_eq!(
        says_pending,
        fifth > 0.9 * W1_MEMORY_GIB,
        "{RUNNER} {} that a fifth concurrent gate would sit Pending, and by request five \
         gates beside the dev pod ask {fifth:.1} GiB of w-1's {W1_MEMORY_GIB} (bound: nine \
         tenths). Make the comment beside the `pgdata` volume say what the numbers say",
        if says_pending { "says" } else { "does not say" }
    );
    let worst = gates * size_limit(pgdata_volume(&pod, named(&pod, "postgres")));
    assert!(
        W1_AVAILABLE_UNDER_FIVE_GATES_GIB - worst >= 0.25 * W1_MEMORY_GIB,
        "{gates} gates each FILLING their PGDATA volume take {worst} GiB that cannot be \
         reclaimed; w-1 had {W1_AVAILABLE_UNDER_FIVE_GATES_GIB} GiB available under five \
         gates (backlog 17f6170c), and less than a quarter of the node would be left. Build \
         the fallback instead: PGDATA on the gate volume"
    );
}

/// EVERY POD THAT RUNS A THROWAWAY POSTGRES IS NAMED HERE. A second gate
/// template (the consist worker, a new runner) that brings its own
/// database on a plain emptyDir would put the writes this car removed
/// straight back on the install disk, with every pin above still green.
#[test]
fn every_job_that_runs_a_throwaway_postgres_keeps_its_data_in_memory() {
    let objects = rbac::estate().objects().unwrap_or_else(|e| panic!("{e}"));
    let mut seen = Vec::new();
    for object in &objects {
        let Some(pod) = object.pod_spec() else {
            continue;
        };
        for container in containers(pod)
            .into_iter()
            .filter(|c| is_throwaway_postgres(c))
        {
            let medium = pgdata_volume(pod, container)
                .path(&["emptyDir", "medium"])
                .and_then(Node::str)
                .unwrap_or("disk");
            seen.push(format!("{} {}: {medium}", object.kind, object.source));
        }
    }
    seen.sort();
    seen.dedup();
    // The dev pod is the one exception, and it is a Deployment, not a
    // gate: its sidecar is the builders' harness database, its scratch
    // databases accumulate across days (4,603 measured), and its pod
    // carries the operator's session — moving it is a decision about the
    // dev pod's memory, not this car's.
    let dev = seen
        .iter()
        .filter(|s| s.starts_with("Deployment ") && s.contains("boss-dev.yaml"))
        .count();
    let gates: Vec<&String> = seen.iter().filter(|s| s.starts_with("Job ")).collect();
    assert!(
        dev >= 1 && gates.len() == 1 && seen.len() == dev + 1,
        "the pods under infra/ that run a Postgres with durability off are expected to be \
         the gate Job and the dev pod; read {seen:?}. A new one is argued here."
    );
    assert_eq!(
        gates[0],
        &format!("Job {RUNNER}: Memory"),
        "the gate Job's Postgres data must be memory-backed"
    );
    // The consist worker is a JSON template the conductor fills, outside
    // the YAML estate: it runs no database today.
    let consist: serde_json::Value =
        serde_json::from_str(&read(CONSIST)).unwrap_or_else(|e| panic!("{CONSIST}: {e}"));
    let images: Vec<String> = ["initContainers", "containers"]
        .iter()
        .flat_map(|k| {
            consist["spec"]["template"]["spec"][k]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .map(|c| c.to_string())
        .collect();
    assert!(
        !images.is_empty() && images.iter().all(|c| !c.contains("PGDATA")),
        "{CONSIST} now runs a Postgres: its data directory must be a bounded memory volume, \
         pinned the way the gate Job's is"
    );
}

/// THE NUMBER REACHES THE RECEIPT. The gate container mounts the data
/// volume read-only at the path it tells the collector about, so every
/// receipt says how full the volume got; without the mount the limit
/// above is an argument again.
#[test]
fn the_gate_container_can_read_how_full_the_data_volume_is() {
    let pod = runner_pod();
    let gate = named(&pod, "gate");
    let pg = named(&pod, "postgres");
    let volume = text(pgdata_volume(&pod, pg), "name").expect("named");
    let path = env(gate, "BOSS_GATE_PGDATA").unwrap_or_else(|| {
        panic!(
            "{RUNNER}: the gate container must set BOSS_GATE_PGDATA to where it mounts the \
             Postgres data volume — runtime-evidence.py reads the volume's use there"
        )
    });
    let mount = items(gate.get("volumeMounts"))
        .into_iter()
        .find(|m| text(m, "name") == Some(volume))
        .unwrap_or_else(|| panic!("the gate container does not mount `{volume}`"));
    assert_eq!(text(mount, "mountPath"), Some(path));
    assert_eq!(
        mount.get("readOnly").and_then(rbac::yaml_bool),
        Some(true),
        "the gate container reads the volume's size; code a gate runs must not be able to \
         write the database's files"
    );
    let collector = read("infra/gate-runner/runtime-evidence.py");
    assert!(
        collector.contains("BOSS_GATE_PGDATA"),
        "runtime-evidence.py no longer reads BOSS_GATE_PGDATA"
    );
    let run_sh: String = read("infra/gate-runner/run.sh")
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    let watcher: Vec<&str> = run_sh
        .lines()
        .filter(|l| l.contains("runtime-evidence.py watch \"$BOSS_GATE_RUNTIME_EVIDENCE\""))
        .collect();
    assert_eq!(
        watcher.len(),
        1,
        "run.sh must start the collector's watcher, once: a check boundary is all the \
         collector otherwise sees, and the test check is one 30-minute boundary"
    );
    // HEARD, NOT SILENCED (review bc011e92, F5). The collector moves its
    // own output onto the file the receipt reads back; a redirect here
    // would discard what a watcher that cannot start has to say, and a
    // dead watcher would read as one never started.
    assert!(
        !watcher[0].contains("/dev/null") && !watcher[0].contains('>'),
        "run.sh starts the watcher with its output redirected: `{}`. Leave it inherited — \
         runtime-evidence.py writes its start line and its first failed reading to \
         <journal>.watch itself, and the receipt carries them as \
         runtime_evidence.pgdata.watcher",
        watcher[0].trim()
    );
    assert!(
        collector.contains("def watcher_report(") && collector.contains("os.dup2("),
        "runtime-evidence.py no longer records what the watcher says about itself"
    );
    // THE COLLECTOR WRITES NO VERDICT (review bc011e92, F1). Its merge adds
    // runtime_evidence and nothing else; gate_pgdata_evidence.rs pins the
    // behaviour, this pins that no line of it assigns the receipt's word.
    let assigns: Vec<&str> = collector
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .filter(|l| l.contains("body[\"verdict\"]") || l.contains("refused_because"))
        .collect();
    assert!(
        assigns.is_empty(),
        "runtime-evidence.py touches the receipt's verdict: {assigns:?}. A full volume or an \
         OOM kill beside a red is co-occurrence, not cause, and a `refused` is relaunched \
         without bound: the evidence goes on the receipt, the verdict stays gate.sh's"
    );
}
