//! `infra/forge/registry-drift.sh` — the converge's last phase files each
//! disagreement between a live registry and the converged tree as ONE
//! backlog-item, and never fails the converge (design d349e0ba car 2,
//! backlog b79054b2).
//!
//! Car 1 took the two live-registry lints out of the gate, because a
//! gate's verdict must be a function of the sha it vouches for — 14 of 92
//! failed gates in one week were live-state failures no car caused. That
//! left the RULES comparison running nowhere against live, and a car that
//! deletes a platform workflow file while the kind is live caught by
//! nothing. This phase closes both, and these tests hold it to the three
//! promises the design makes:
//!
//!   1. a disagreement files ONE item, naming which side is ahead and
//!      the command that resolves it;
//!   2. a second converge that finds the same disagreement files NONE;
//!   3. an unreachable registry files nothing and exits 0.
//!
//! THE FIXTURE IS A REPO THIS PROCESS OWNS. Which side is ahead is read
//! from git — a deletion in the history, the file's last commit date —
//! and the gate's own checkout is a `--depth 50` clone, so neither is a
//! fact this test could plant there. The fixture copies the files the
//! two lints read into a scratch repo, commits them at a fixed date, and
//! deletes one kind's file in a second commit: the lints and the script
//! are the tree's own, only the history is planted. The stub is the jobs
//! API and the dispatcher as the script sees them, and it keeps what is
//! POSTed OPEN, so the second run meets the first run's items exactly as
//! the next converge would.

use boss_testing::announce::{await_announced_port, with_announce};
use boss_testing::repo_root;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// The fixture repo's commits: the tree as it is, then an edit to two
/// kinds' descriptions, then the retirement. A live row published between
/// the first and the edit is behind the edit; one published after all of
/// them is ahead.
const TREE_COMMITTED_AT: &str = "2025-01-01T00:00:00Z";
const EDITED_AT: &str = "2026-01-01T00:00:00Z";
const RETIRED_AT: &str = "2026-01-02T00:00:00Z";
const LIVE_BEFORE_THE_EDIT: &str = "2025-06-01T00:00:00Z";
const LIVE_AFTER_THE_TREE: &str = "2026-06-01T00:00:00Z";
const TREE_REWRITE: &str = "A sentence the tree rewrote after live was published.";
const LIVE_ONLY_KIND: &str = "zeta-live-only";
const LIVE_ONLY_RULE: &str = "zz-live-only-rule";

const STUB: &str = r#"
import http.server, json, sys, socket, urllib.parse
log, started, registry, rules, mode = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4], sys.argv[5]
sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
sock.bind(("127.0.0.1", 0))
port = sock.getsockname()[1]
# Three unrelated open items, so the dedup has something to read past;
# and the page is capped at 2 rows, as a server may cap it, so the
# script must PAGE to see the whole open list.
OPEN = [{"id": f"0000000{i}-open", "title": f"unrelated {i}", "metadata": {"area": "x"}} for i in range(3)]
# `stale:<kind>,<kind>` (or `stale-403:…`): a drift item already open for
# each kind, filed before any hold, whose remedy names the hand publish —
# backlog-item 7ad62d1c's shape on 2026-10-06. `stale-403` refuses the
# metadata door.
STALE = mode.split(":", 1)[1].split(",") if mode.startswith("stale") else []
for i, k in enumerate(STALE):
    cmd = f"diff first — live v7; then: boss workflow publish {k} infra/platform/workflows/{k}.toml"
    OPEN.append({"id": f"5ta1e00{i}-stale-{k}", "title": f"Registry drift: protocol {k} — tree ahead by an edit",
                 "metadata": {"registry_drift": f"protocol:{k}", "side": "tree ahead by an edit: filed before the hold",
                              "resolve": cmd, "description": f"{k}: 1 field(s) disagree.\n\nResolve with: {cmd}\n",
                              "area": "delivery"}})
PAGE_CAP = 2
class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def _reply(self, code, body=b"{}"):
        self.send_response(code); self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body))); self.end_headers(); self.wfile.write(body)
    def do_GET(self):
        with open(log, "ab") as f:
            f.write(b"GET " + self.path.encode() + b"\n")
        if self.path.startswith("/api/workflows"):
            if mode == "registries-down":
                self._reply(503, b'{"error":"upstream unavailable"}')
            else:
                self._reply(200, open(registry, "rb").read())
        elif self.path.startswith("/api/dispatcher/rules"):
            if mode == "registries-down":
                self._reply(503, b'{"error":"upstream unavailable"}')
            else:
                self._reply(200, open(rules, "rb").read())
        elif self.path.startswith("/api/jobs/"):
            # One packet, read back by id.
            hit = [r for r in OPEN if self.path == f"/api/jobs/{r['id']}"]
            if hit: self._reply(200, json.dumps(hit[0]).encode())
            else: self._reply(404, b'job not found')
        elif self.path.startswith("/api/jobs"):
            q = urllib.parse.parse_qs(urllib.parse.urlparse(self.path).query)
            off = int(q.get("offset", ["0"])[0])
            lim = min(int(q.get("limit", ["500"])[0]), PAGE_CAP)
            has = q.get("metadata_has", [None])[0]
            rows = [r for r in OPEN if has in (r.get("metadata") or {})] if has else OPEN
            if mode == "jobs-503":
                self._reply(503, b'{"error":"policy unavailable"}')
            elif mode == "dark":
                # A denied scope, or a wrong or empty instance: well-formed, and zero.
                self._reply(200, b'{"data":[],"total":0}')
            elif mode == "short-page" and has:
                # The dedup read claims three more rows than it ever hands over.
                self._reply(200, json.dumps({"data": rows[off:off + lim], "total": len(rows) + 3}).encode())
            else:
                self._reply(200, json.dumps({"data": rows[off:off + lim], "total": len(rows)}).encode())
        else:
            self._reply(404, b'{"error":"no such route"}')
    def do_POST(self):
        n = int(self.headers.get("content-length", "0"))
        body = json.loads(self.rfile.read(n))
        who = self.headers.get("x-boss-user", "")
        with open(log, "ab") as f:
            f.write(b"POST " + self.path.encode() + b"\n" + json.dumps({"who": who, "body": body}).encode() + b"\n")
        if self.path == "/api/jobs":
            body["id"] = f"{len(OPEN):08x}-drift-item"
            OPEN.append(body)
            self._reply(201, json.dumps(body).encode())
        else:
            self._reply(404, b'{"error":"no such route"}')
    def do_PATCH(self):
        # The metadata merge door: top-level keys overwrite, null deletes.
        n = int(self.headers.get("content-length", "0"))
        body = json.loads(self.rfile.read(n))
        who = self.headers.get("x-boss-user", "")
        with open(log, "ab") as f:
            f.write(b"PATCH " + self.path.encode() + b"\n" + json.dumps({"who": who, "body": body}).encode() + b"\n")
        hit = [r for r in OPEN if self.path == f"/api/jobs/{r['id']}/metadata"]
        if mode.startswith("stale-403"):
            self._reply(403, b'job is outside your scope')
        elif mode.startswith("stale-noeffect"):
            # A door that answers success and changes nothing: the answer
            # is a claim, the row is the effect.
            self._reply(204, b"")
        elif hit and isinstance(body, dict):
            md = hit[0].setdefault("metadata", {})
            for k, v in body.items():
                if v is None: md.pop(k, None)
                else: md[k] = v
            # What the door answers: 204, no body (boss-jobs http/jobs.rs,
            # patch_job_metadata ends StatusCode::NO_CONTENT). This stub
            # answered 200 with the job until review 6ff3210e found the
            # script counting only that — an answer the door never gives.
            self._reply(204, b"")
        else:
            self._reply(404, b'job not found')
srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), H, bind_and_activate=False)
srv.socket.close(); srv.socket = sock; srv.server_address = sock.getsockname(); srv.server_activate()
announce(started, str(port))
srv.serve_forever()
"#;

fn script() -> PathBuf {
    repo_root().join("infra/forge/registry-drift.sh")
}

fn has_tools() -> bool {
    ["python3", "git", "curl"].iter().all(|t| {
        Command::new(t)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
    })
}

/// Copy `rel` (a file or a directory, recursively) from the tree into `dst`.
fn copy_in(rel: &str, dst: &Path) {
    let from = repo_root().join(rel);
    let to = dst.join(rel);
    if from.is_dir() {
        std::fs::create_dir_all(&to).unwrap();
        for e in std::fs::read_dir(&from).unwrap() {
            let name = e.unwrap().file_name();
            copy_in(&format!("{rel}/{}", name.to_string_lossy()), dst);
        }
    } else {
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::copy(&from, &to).unwrap_or_else(|e| panic!("copy {rel}: {e}"));
    }
}

fn git(dir: &Path, date: &str, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_DATE", date)
        .output()
        .unwrap_or_else(|e| panic!("spawn git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "fixture git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The bundle's kinds, sorted — the three the plants use are the first three.
fn bundle_kinds() -> Vec<String> {
    let mut kinds: Vec<String> = std::fs::read_dir(repo_root().join("infra/platform/workflows"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .map(|p| p.file_stem().unwrap().to_string_lossy().into_owned())
        .collect();
    kinds.sort();
    assert!(kinds.len() >= 4, "the bundle carries fewer than four kinds");
    kinds
}

struct Plants {
    /// Edited in the tree after live was published from the previous file.
    tree_ahead: String,
    /// Edited in the tree after live was published — but live is NOT the
    /// previous file: both sides moved.
    both_moved: String,
    /// Published live after the file last changed.
    live_ahead: String,
    /// Deleted in the tree, still live.
    retired: String,
    /// Authored in the tree with NO live row: the seed never admitted it.
    /// None in the plain fixture, where every bundle kind is live.
    never_admitted: Option<String>,
    /// A kind whose row declares a field `writer`, in the tree AND live:
    /// held BY DEFAULT, with no hold file — the signer row's case.
    default_held: Option<String>,
    /// Live disagrees with the file and its row carries no readable date:
    /// which side is ahead cannot be measured.
    undated: Option<String>,
    /// No plant at all: the tree and both registries agree — the live
    /// shape on 2026-10-07 (0 drift, 61 drift items still open).
    clean: bool,
}

fn plants() -> Plants {
    let k = bundle_kinds();
    Plants {
        tree_ahead: k[0].clone(),
        live_ahead: k[1].clone(),
        retired: k[2].clone(),
        both_moved: k[3].clone(),
        never_admitted: None,
        default_held: None,
        undated: None,
        clean: false,
    }
}

/// Give the row's first field a `writer` — a refusal row by construction
/// (boss-jobs field_writer.rs), which the reader holds with no file.
fn declares_a_writer(doc: &mut toml::Value) {
    let field = doc["workflow"][0]["step"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .filter_map(|s| s.get_mut("fields").and_then(|f| f.as_array_mut()))
        .find_map(|f| f.first_mut())
        .expect("the planted kind has a step with a field");
    field
        .as_table_mut()
        .unwrap()
        .insert("writer".into(), toml::Value::String("signer".into()));
}

/// The same, written into the fixture tree's file for `kind`.
fn declares_a_writer_in(dir: &Path, kind: &str) {
    let path = dir.join(format!("infra/platform/workflows/{kind}.toml"));
    let mut doc: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    declares_a_writer(&mut doc);
    std::fs::write(&path, toml::to_string(&doc).unwrap()).unwrap();
}

const HOLDS_REL: &str = "infra/platform/workflow-holds";
const HOLDS_READER: &str = "infra/gcp/workflow-holds.py";
const HOLD_WHY: &str = "This row turns on a refusal on the approve path and goes live at a deliberate publish with its positive control.";
const HOLD_LIFTS: &str = "backlog 6c9183de: removed by the car that follows the deliberate publish";
/// The one door a hold does not bind, as either voice would spell it.
const HAND_PUBLISH: &str = "boss workflow publish";

/// Declare `kind` held in the fixture tree, before its first commit.
fn hold(dir: &Path, kind: &str) {
    hold_because(dir, kind, HOLD_WHY);
}

/// The same, for a reason of the case's own.
fn hold_because(dir: &Path, kind: &str, why: &str) {
    let holds = dir.join(HOLDS_REL);
    std::fs::create_dir_all(&holds).unwrap();
    std::fs::write(
        holds.join(format!("{kind}.toml")),
        format!("drift_publish = \"held\"\nwhy = '''{why}'''\nlifts = '{HOLD_LIFTS}'\n"),
    )
    .unwrap();
}

/// `kind`'s file with its description replaced — the tree's edit.
fn rewritten(dir: &Path, kind: &str) {
    let path = dir.join(format!("infra/platform/workflows/{kind}.toml"));
    let mut doc: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    doc["workflow"][0].as_table_mut().unwrap().insert(
        "description".into(),
        toml::Value::String(TREE_REWRITE.into()),
    );
    std::fs::write(&path, toml::to_string(&doc).unwrap()).unwrap();
}

/// A scratch repo holding what the two lints read: the tree as it is at
/// TREE_COMMITTED_AT, the `tree_ahead` and `both_moved` descriptions
/// rewritten at EDITED_AT, and the `retired` kind's file deleted at
/// RETIRED_AT.
fn fixture_repo(case: &str, p: &Plants) -> PathBuf {
    fixture_repo_with(case, p, |_| {})
}

/// The same repo, with `plant` run on the tree before its first commit —
/// the holds a case declares, or the damage it does to them.
fn fixture_repo_with(case: &str, p: &Plants, plant: impl Fn(&Path)) -> PathBuf {
    let dir = boss_testing::scratch_dir(&format!("registry-drift-repo-{case}"));
    for rel in [
        // The one reader of the holds: both voices ask it before either
        // names a publish (backlog c6bd9f18).
        HOLDS_READER,
        "infra/lint",
        "infra/lib",
        "infra/platform/workflows",
        "infra/dispatcher/rules",
        "infra/postgres/schema",
        "infra/cluster/instances.toml",
        "crates/core/boss-jobs/src/registry.rs",
    ] {
        copy_in(rel, &dir);
    }
    for e in std::fs::read_dir(repo_root().join("examples")).unwrap() {
        let name = e.unwrap().file_name().to_string_lossy().into_owned();
        let seeds = format!("examples/{name}/seeds/workflows.toml");
        if repo_root().join(&seeds).is_file() {
            copy_in(&seeds, &dir);
        }
    }
    plant(&dir);
    git(&dir, TREE_COMMITTED_AT, &["init", "-q", "-b", "main", "."]);
    git(&dir, TREE_COMMITTED_AT, &["config", "user.email", "t@t"]);
    git(&dir, TREE_COMMITTED_AT, &["config", "user.name", "t"]);
    git(&dir, TREE_COMMITTED_AT, &["add", "-A"]);
    git(&dir, TREE_COMMITTED_AT, &["commit", "-q", "-m", "the tree"]);
    if p.clean {
        return dir;
    }
    rewritten(&dir, &p.tree_ahead);
    rewritten(&dir, &p.both_moved);
    git(
        &dir,
        EDITED_AT,
        &["commit", "-q", "-a", "-m", "rewrite two descriptions"],
    );
    let retired = format!("infra/platform/workflows/{}.toml", p.retired);
    git(&dir, RETIRED_AT, &["rm", "-q", &retired]);
    git(&dir, RETIRED_AT, &["commit", "-q", "-m", "retire a kind"]);
    dir
}

/// A step's `agent` block in the row's shape — the registry hands
/// `budget_usd` back as a float and `null` where the file has none, and
/// the protocols lint compares the block (backlog 1b847556). The same
/// rendering as protocol_drift_sh.rs, whose fixture this one follows.
fn agent_as_the_registry_hands_it_back(block: Option<&toml::Value>) -> serde_json::Value {
    let Some(table) = block.and_then(|b| b.as_table()) else {
        return serde_json::Value::Null;
    };
    serde_json::Value::Object(
        table
            .iter()
            .map(|(k, v)| {
                let v = match v {
                    toml::Value::Integer(n) => serde_json::json!(*n as f64),
                    toml::Value::Float(x) => serde_json::json!(*x),
                    toml::Value::Boolean(b) => serde_json::json!(*b),
                    other => serde_json::json!(other.as_str()),
                };
                (k.clone(), v)
            })
            .collect(),
    )
}

/// The live workflow registry: every kind the TREE's bundle authors
/// (the retired one included — its row is still live), rendered from the
/// files, plus the plants: two description edits dated either side of
/// the tree's commit, and a kind no file has ever authored.
fn registry_fixture(p: &Plants) -> String {
    let mut rows = Vec::new();
    for kind in bundle_kinds() {
        if p.never_admitted.as_deref() == Some(kind.as_str()) {
            continue;
        }
        let path = repo_root().join(format!("infra/platform/workflows/{kind}.toml"));
        let mut doc: toml::Value =
            toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        if p.default_held.as_deref() == Some(kind.as_str()) {
            declares_a_writer(&mut doc);
        }
        let wf = &doc["workflow"][0];
        let steps: Vec<serde_json::Value> = wf
            .get("step")
            .and_then(|s| s.as_array())
            .map(|steps| {
                steps
                    .iter()
                    .map(|s| {
                        let mut step = serde_json::to_value(s).unwrap();
                        step["agent"] = agent_as_the_registry_hands_it_back(s.get("agent"));
                        step
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut row = serde_json::to_value(wf).unwrap();
        let obj = row.as_object_mut().unwrap();
        obj.remove("step");
        obj.insert("steps".into(), serde_json::json!(steps));
        obj.insert("version".into(), serde_json::json!(7));
        obj.insert("status".into(), serde_json::json!("active"));
        obj.insert("owning_team".into(), serde_json::json!("platform"));
        obj.insert(
            "created_at".into(),
            serde_json::json!("2026-01-01T00:00:00.000000Z"),
        );
        if p.clean {
            rows.push(row);
            continue;
        }
        if kind == p.tree_ahead {
            // Published from the file as it stood before the edit.
            row["created_at"] = serde_json::json!(LIVE_BEFORE_THE_EDIT);
            row["authoring_job_id"] = serde_json::json!("0badc0de-live-author");
        }
        if kind == p.both_moved {
            // Published before the edit too — but NOT from the file: an
            // operator's live work that publishing the file would erase.
            row["description"] = serde_json::json!("OPERATOR EDIT: live work the tree never saw.");
            row["created_at"] = serde_json::json!(LIVE_BEFORE_THE_EDIT);
        }
        if kind == p.live_ahead {
            row["description"] =
                serde_json::json!("OPERATOR EDIT: published live after the file last changed.");
            row["created_at"] = serde_json::json!(LIVE_AFTER_THE_TREE);
        }
        if p.undated.as_deref() == Some(kind.as_str()) {
            row["description"] = serde_json::json!("OPERATOR EDIT: a row with no readable date.");
            row["created_at"] = serde_json::json!("unknown");
        }
        rows.push(row);
    }
    if p.clean {
        return serde_json::to_string(&rows).unwrap();
    }
    rows.push(serde_json::json!({
        "kind": LIVE_ONLY_KIND, "version": 1, "status": "active", "label": "Zeta",
        "category": "platform", "owning_team": "platform",
        "created_at": "2026-03-01T00:00:00Z",
        "description": "Published live through POST /api/workflows and never written back."
    }));
    serde_json::to_string(&rows).unwrap()
}

/// The dispatcher's rule registry: every rule the tree authors, plus one
/// its image does not.
fn rules_fixture(p: &Plants) -> String {
    let mut rules: Vec<serde_json::Value> = std::fs::read_dir(
        repo_root().join("infra/dispatcher/rules"),
    )
    .unwrap()
    .map(|e| e.unwrap().path())
    .filter(|p| p.extension().is_some_and(|x| x == "toml"))
    .map(|p| {
        let name = p.file_stem().unwrap().to_string_lossy().into_owned();
        serde_json::json!({"name": name, "version": 1, "authored": true, "source": "product"})
    })
    .collect();
    let authored = rules.len();
    if !p.clean {
        rules.push(serde_json::json!({"name": LIVE_ONLY_RULE, "version": 1, "authored": false, "source": "product"}));
    }
    serde_json::json!({
        "rules": rules,
        "authored_registry": {"dir": "/opt/boss/infra/dispatcher/rules", "rules": authored}
    })
    .to_string()
}

struct Stub {
    child: Child,
    port: u16,
    log: PathBuf,
}
impl Drop for Stub {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Stub {
    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
    /// Every POST the stub took: (path, who, body).
    fn posts(&self) -> Vec<(String, String, serde_json::Value)> {
        self.writes("POST ")
    }
    /// Every PATCH the stub took — the metadata merge door.
    fn patches(&self) -> Vec<(String, String, serde_json::Value)> {
        self.writes("PATCH ")
    }
    fn writes(&self, verb: &str) -> Vec<(String, String, serde_json::Value)> {
        let log = self.log();
        let mut lines = log.lines();
        let mut out = Vec::new();
        while let Some(l) = lines.next() {
            if let Some(path) = l.strip_prefix(verb) {
                let rec: serde_json::Value = serde_json::from_str(
                    lines.next().expect("a write line is followed by its body"),
                )
                .unwrap();
                out.push((
                    path.to_string(),
                    rec["who"].as_str().unwrap_or("").to_string(),
                    rec["body"].clone(),
                ));
            }
        }
        out
    }
}

fn start_stub(case: &str, p: &Plants, mode: &str) -> Stub {
    let dir = boss_testing::scratch_dir(&format!("registry-drift-stub-{case}"));
    let script = dir.join("stub.py");
    let started = dir.join("started");
    let log = dir.join("calls.log");
    let registry = dir.join("workflows.json");
    let rules = dir.join("rules.json");
    std::fs::write(&registry, registry_fixture(p)).unwrap();
    std::fs::write(&rules, rules_fixture(p)).unwrap();
    boss_testing::write_exec(&script, &with_announce(STUB));
    let mut child = Command::new("python3")
        .arg(&script)
        .args([&log, &started, &registry, &rules])
        .arg(mode)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("python3");
    let port = await_announced_port(&mut child, &started, Duration::from_secs(20));
    Stub { child, port, log }
}

/// One converge's last phase: (exit, stdout, stderr).
fn run(repo: &Path, jobs_url: &str, dispatcher_url: Option<&str>) -> (Option<i32>, String, String) {
    let mut cmd = Command::new("bash");
    cmd.arg(script())
        .arg("--repo")
        .arg(repo)
        .env("BOSS_JOBS_URL", jobs_url)
        // No /etc/boss/sor.env of this box's decides the address.
        .env("BOSS_SOR_ENV", repo.join("no-sor.env"))
        // A refused connect is refused now, not after a roll's minute.
        .env("BOSS_SOR_WAIT_SECONDS", "0")
        .env_remove("BOSS_DISPATCHER_URL")
        .env_remove("BOSS_MACHINE_TOKEN")
        .env_remove("BOSS_ESTATE");
    if let Some(url) = dispatcher_url {
        cmd.env("BOSS_DISPATCHER_URL", url);
    }
    let out = cmd.output().expect("bash");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn last_line(out: &str) -> &str {
    out.lines().last().unwrap_or("")
}

fn filed_item<'a>(
    posts: &'a [(String, String, serde_json::Value)],
    key: &str,
) -> &'a serde_json::Value {
    let hits: Vec<&serde_json::Value> = posts
        .iter()
        .map(|(_, _, b)| b)
        .filter(|b| b["metadata"]["registry_drift"] == key)
        .collect();
    assert_eq!(
        hits.len(),
        1,
        "exactly one item for {key}; filed: {posts:#?}"
    );
    hits[0]
}

/// Promise 1: each disagreement — four protocol kinds of three shapes and
/// one rule — is ONE backlog-item, and the item says which side is ahead
/// and the command that resolves it. Promise 2: the next converge, with
/// the same disagreements and those items still open, files none.
#[test]
fn a_disagreement_files_one_item_and_the_next_converge_files_none() {
    if !has_tools() {
        eprintln!("skipping: needs python3, git and curl");
        return;
    }
    let p = plants();
    let repo = fixture_repo("files", &p);
    let stub = start_stub("files", &p, "serve");
    let base = format!("http://127.0.0.1:{}", stub.port);

    let (rc, out, err) = run(&repo, &base, Some(&base));
    assert_eq!(
        rc,
        Some(0),
        "the phase never fails the converge:\n{out}\n{err}"
    );
    let posts = stub.posts();
    assert_eq!(
        posts.len(),
        6,
        "six disagreements, six items — an edit each way, both moved, a retirement, a kind \
         with no file, a rule with no file:\n{out}\n{err}\n{}",
        stub.log()
    );
    assert!(
        stub.log().contains("metadata_has=registry_drift"),
        "the dedup read is narrowed to the items that carry the key:\n{}",
        stub.log()
    );
    for (path, who, body) in &posts {
        assert_eq!(path, "/api/jobs", "items are filed as packets");
        assert!(
            who.contains("automation:cluster-deploy-runner"),
            "signed as the converge that found it, not as a person: {who}"
        );
        assert_eq!(body["kind"], "backlog-item");
        assert_eq!(body["metadata"]["area"], "delivery");
        assert_eq!(body["metadata"]["input_channel"], "telemetry/monitoring");
        for key in ["side", "resolve", "description", "converged_head"] {
            assert!(
                body["metadata"][key]
                    .as_str()
                    .is_some_and(|s| !s.is_empty()),
                "every item carries `{key}`: {body}"
            );
        }
    }

    // Which side is ahead, read from the history and the row's date.
    let edit = filed_item(&posts, &format!("protocol:{}", p.tree_ahead));
    assert!(
        edit["title"]
            .as_str()
            .unwrap()
            .ends_with("tree ahead by an edit"),
        "live published from the file's previous revision, file edited since: {edit}"
    );
    let resolve = edit["metadata"]["resolve"].as_str().unwrap();
    assert!(
        resolve.starts_with("diff first")
            && resolve.contains(&format!("created_at {LIVE_BEFORE_THE_EDIT}"))
            && resolve.contains("0badc0de-live-author")
            && resolve.ends_with(&format!(
                "then: boss workflow publish {k} infra/platform/workflows/{k}.toml",
                k = p.tree_ahead
            )),
        "a publish names whose live version it replaces and says diff first: {resolve}"
    );
    let both = filed_item(&posts, &format!("protocol:{}", p.both_moved));
    assert!(
        both["title"]
            .as_str()
            .unwrap()
            .ends_with("both moved — side not measured"),
        "the file is newer, but live is not its previous revision — never 'tree ahead': {both}"
    );
    let resolve = both["metadata"]["resolve"].as_str().unwrap();
    assert!(
        resolve.starts_with("diff first")
            && resolve.contains("only if the diff shows nothing live to keep"),
        "both-moved never hands over a bare publish: {resolve}"
    );
    let live = filed_item(&posts, &format!("protocol:{}", p.live_ahead));
    assert!(
        live["title"].as_str().unwrap().ends_with("live ahead"),
        "live row published after the file's last commit: {live}"
    );
    assert!(
        live["metadata"]["resolve"]
            .as_str()
            .unwrap()
            .contains(&format!("boss-api GET /api/workflows/{}", p.live_ahead)),
        "a live-ahead edit names the read that exports the row: {live}"
    );
    let retired = filed_item(&posts, &format!("protocol:{}", p.retired));
    assert!(
        retired["title"]
            .as_str()
            .unwrap()
            .ends_with("tree ahead by a retirement"),
        "a kind whose file the history deleted is a retirement: {retired}"
    );
    assert_eq!(
        retired["metadata"]["resolve"],
        format!("boss-api POST /api/workflows/{}/retire", p.retired)
    );
    let never = filed_item(&posts, &format!("protocol:{LIVE_ONLY_KIND}"));
    assert!(
        never["title"].as_str().unwrap().ends_with("live ahead"),
        "a live kind no file ever authored is live ahead, not a retirement: {never}"
    );
    let rule = filed_item(&posts, &format!("rule:{LIVE_ONLY_RULE}"));
    assert!(
        rule["metadata"]["resolve"]
            .as_str()
            .unwrap()
            .contains(&format!("infra/dispatcher/rules/{LIVE_ONLY_RULE}.toml")),
        "a rule the image does not author names the file to write: {rule}"
    );
    assert!(
        last_line(&out).contains("filed 6, already open 0"),
        "the summary the converge records says what happened: {out}"
    );

    // The next converge: the same disagreements, and those six items open.
    let (rc, out, err) = run(&repo, &base, Some(&base));
    assert_eq!(rc, Some(0), "{out}\n{err}");
    assert_eq!(
        stub.posts().len(),
        6,
        "a second converge with the same disagreements files none:\n{out}\n{err}"
    );
    assert!(
        last_line(&out).contains("filed 0, already open 6"),
        "and says each is already open: {out}"
    );
}

/// Everything one run said or filed, as one text: the journal (the lint's
/// report rides stderr), the summary, and every POSTed body.
fn every_voice(out: &str, err: &str, posts: &[(String, String, serde_json::Value)]) -> String {
    let bodies: String = posts.iter().map(|(_, _, b)| format!("{b}\n")).collect();
    format!("{out}\n{err}\n{bodies}")
}

/// A HELD kind is never advised the hand publish (backlog c6bd9f18, review
/// 72485f08 finding F1). The hold car (083d240e) keeps a refusal row out of
/// every unattended publish, so for a held kind the tree is ahead of live
/// BY DESIGN — and both machine voices used to answer that with `boss
/// workflow publish <kind> <file>`, the one door a hold does not bind. An
/// agent draining the queue would have followed it.
///
/// Five held kinds, one of each shape:
///   tree ahead by an edit — the intended disagreement: NOT filed, said
///     every converge on the journal and in the summary, with the hold's
///     source, why and what lifts it;
///   both moved, side not measured — live may hold work the deliberate
///     publish would overwrite, and the summary has no reader, so these
///     ARE filed, with the hold and no publish (review 8d088b41, F5);
///   never admitted — the seed's failure, not the hold's doing: filed, and
///     its remedy names the seed and the hold, never a publish;
///   live ahead — filed as before (its remedy was never a publish), and
///     the item says the kind is held.
#[test]
fn a_held_kind_is_never_advised_the_hand_publish() {
    if !has_tools() {
        eprintln!("skipping: needs python3, git and curl");
        return;
    }
    let mut p = plants();
    p.never_admitted = Some(bundle_kinds()[4].clone());
    p.undated = Some(bundle_kinds()[5].clone());
    let unadmitted = p.never_admitted.clone().unwrap();
    let undated = p.undated.clone().unwrap();
    let held = [
        p.tree_ahead.clone(),
        p.both_moved.clone(),
        p.live_ahead.clone(),
        unadmitted.clone(),
        undated.clone(),
    ];
    let repo = fixture_repo_with("held", &p, |dir| held.iter().for_each(|k| hold(dir, k)));
    let stub = start_stub("held", &p, "serve");
    let base = format!("http://127.0.0.1:{}", stub.port);

    let (rc, out, err) = run(&repo, &base, Some(&base));
    assert_eq!(rc, Some(0), "{out}\n{err}");
    let posts = stub.posts();
    let said = every_voice(&out, &err, &posts);
    assert!(
        !said.contains(HAND_PUBLISH),
        "every drifting kind is held, so nothing — item, journal or the lint's report — names \
         the hand publish:\n{said}"
    );
    assert_eq!(
        posts.len(),
        7,
        "live ahead, both moved, side not measured, never admitted, the retirement, the \
         live-only kind and the rule are filed; the held kind the tree is ahead on by an edit \
         is not:\n{out}\n{err}"
    );
    for (kind, title) in [
        (&p.both_moved, "both moved — side not measured — held"),
        (&undated, "side not measured — held"),
    ] {
        let item = filed_item(&posts, &format!("protocol:{kind}"));
        assert!(item["title"].as_str().unwrap().ends_with(title), "{item}");
        let resolve = item["metadata"]["resolve"].as_str().unwrap();
        assert!(
            resolve.starts_with("no publish")
                && resolve.contains(HOLD_WHY)
                && resolve.contains(HOLD_LIFTS)
                && resolve.contains(&format!("boss-api GET /api/workflows/{kind}")),
            "{kind}: filed with the hold and the read that shows what live carries: {resolve}"
        );
    }
    {
        let kind = &p.tree_ahead;
        assert!(
            !posts
                .iter()
                .any(|(_, _, b)| b["metadata"]["registry_drift"] == format!("protocol:{kind}")),
            "{kind} is held and the tree is ahead by design: no item"
        );
        let line = out
            .lines()
            .find(|l| {
                l.starts_with(&format!(
                    "registry-drift: protocol:{kind} — HELD, not filed"
                ))
            })
            .unwrap_or_else(|| panic!("no held line for {kind} in:\n{out}"));
        for needle in [
            format!("declared in {HOLDS_REL}/{kind}.toml"),
            HOLD_WHY.to_string(),
            HOLD_LIFTS.to_string(),
        ] {
            assert!(
                line.contains(&needle),
                "the held line carries `{needle}`: {line}"
            );
        }
    }
    // The lint's own report, which the converge journals whole.
    for kind in [&p.tree_ahead, &p.both_moved, &p.live_ahead, &undated] {
        assert!(
            err.lines()
                .any(|l| l.contains(&format!("{kind} — declared in {HOLDS_REL}/{kind}.toml"))),
            "the lint names {kind} as held, with the hold's source:\n{err}"
        );
    }
    assert!(
        err.contains(HOLD_WHY) && err.contains(HOLD_LIFTS),
        "the lint says why a kind is held and what lifts it:\n{err}"
    );

    let never = filed_item(&posts, &format!("protocol:{unadmitted}"));
    assert!(
        never["title"]
            .as_str()
            .unwrap()
            .ends_with("tree ahead, never admitted — held"),
        "{never}"
    );
    let resolve = never["metadata"]["resolve"].as_str().unwrap();
    assert!(
        resolve.contains(HOLD_WHY) && resolve.contains(HOLD_LIFTS) && resolve.contains("seed"),
        "a held kind with no live row is the seed's to admit, and the item says the hold: {resolve}"
    );
    let live = filed_item(&posts, &format!("protocol:{}", p.live_ahead));
    assert!(
        live["title"].as_str().unwrap().ends_with("live ahead"),
        "{live}"
    );
    let description = live["metadata"]["description"].as_str().unwrap();
    assert!(
        description.contains("HELD") && description.contains(HOLD_WHY),
        "a live-ahead item on a held kind says it is held — a rollback reads exactly so: {description}"
    );

    let last = last_line(&out);
    assert!(
        last.contains(&format!("1 held and not filed ({})", p.tree_ahead))
            && last.contains("filed 7, already open 0"),
        "the summary the converge records keeps the measurement: {last}"
    );
    assert!(
        stub.patches().is_empty(),
        "no open item named a publish, so the metadata door is not touched: {:?}",
        stub.patches()
    );

    // The next converge: still measured, still said, still not filed.
    let (rc, out, err) = run(&repo, &base, Some(&base));
    assert_eq!(rc, Some(0), "{out}\n{err}");
    assert_eq!(stub.posts().len(), 7, "{out}\n{err}");
    assert!(stub.patches().is_empty(), "{:?}", stub.patches());
    let last = last_line(&out);
    assert!(
        last.contains("1 held and not filed") && last.contains("filed 0, already open 7"),
        "{last}"
    );
    assert!(
        out.contains(&format!(
            "registry-drift: protocol:{} — HELD, not filed",
            p.tree_ahead
        )),
        "a held disagreement is said on every converge's journal: {out}"
    );
}

/// A hold changes what is said about the HELD kind and nothing else: the
/// kind beside it that is not held reads exactly as it did, publish
/// command and all, and the lint's report says which kinds its command is
/// for.
#[test]
fn a_kind_that_is_not_held_reads_as_before_beside_one_that_is() {
    if !has_tools() {
        eprintln!("skipping: needs python3, git and curl");
        return;
    }
    let p = plants();
    let repo = fixture_repo_with("held-beside", &p, |dir| hold(dir, &p.both_moved));
    let stub = start_stub("held-beside", &p, "serve");
    let base = format!("http://127.0.0.1:{}", stub.port);
    let (rc, out, err) = run(&repo, &base, Some(&base));
    assert_eq!(rc, Some(0), "{out}\n{err}");
    let posts = stub.posts();
    assert_eq!(
        posts.len(),
        6,
        "six disagreements, six items, one of them held:\n{out}\n{err}"
    );
    assert!(
        filed_item(&posts, &format!("protocol:{}", p.both_moved))["title"]
            .as_str()
            .unwrap()
            .ends_with("— held"),
        "the held one says so in its title"
    );
    let edit = filed_item(&posts, &format!("protocol:{}", p.tree_ahead));
    assert!(
        edit["metadata"]["resolve"]
            .as_str()
            .unwrap()
            .ends_with(&format!(
                "then: {HAND_PUBLISH} {k} infra/platform/workflows/{k}.toml",
                k = p.tree_ahead
            )),
        "a kind that is not held keeps its remedy: {edit}"
    );
    let said = every_voice(&out, &err, &posts);
    assert!(
        !said.contains(&format!("{HAND_PUBLISH} {}", p.both_moved)),
        "the held kind is never the object of a publish command:\n{said}"
    );
    assert!(
        err.contains(&format!(
            "    {HAND_PUBLISH} <kind> infra/platform/workflows/<kind>.toml"
        )),
        "the lint still names the publish for the kinds that are not held:\n{err}"
    );
    let scope = err
        .lines()
        .find(|l| l.contains("is for the kind(s) that are NOT held"))
        .unwrap_or_else(|| panic!("the lint does not scope its command:\n{err}"));
    assert!(
        scope.contains(&p.tree_ahead)
            && scope.contains(&p.live_ahead)
            && !scope.contains(&p.both_moved),
        "the command is scoped to the kinds that are not held: {scope}"
    );
    assert!(
        !last_line(&out).contains("held and not filed"),
        "nothing went unfiled: {}",
        last_line(&out)
    );
}

/// The signer row's case (review 8d088b41, F7): a row that DECLARES A
/// WRITER is held by default, with no file under workflow-holds — and
/// both voices honour that hold exactly as they do a declared one. The
/// tree is ahead of live by an edit here, so nothing is filed, and the
/// source each voice names is the declaration the reader found.
#[test]
fn a_row_that_declares_a_writer_is_held_with_no_hold_file() {
    if !has_tools() {
        eprintln!("skipping: needs python3, git and curl");
        return;
    }
    let mut p = plants();
    p.default_held = Some(p.tree_ahead.clone());
    let repo = fixture_repo_with("held-by-default", &p, |dir| {
        declares_a_writer_in(dir, &p.tree_ahead)
    });
    assert!(
        !repo.join(HOLDS_REL).exists(),
        "the case declares no hold: the row is the whole of it"
    );
    let stub = start_stub("held-by-default", &p, "serve");
    let base = format!("http://127.0.0.1:{}", stub.port);
    let (rc, out, err) = run(&repo, &base, Some(&base));
    assert_eq!(rc, Some(0), "{out}\n{err}");
    let posts = stub.posts();
    let kind = &p.tree_ahead;
    assert_eq!(
        posts.len(),
        5,
        "six disagreements, the held one unfiled:\n{out}\n{err}"
    );
    let said = every_voice(&out, &err, &posts);
    assert!(
        !said.contains(&format!("{HAND_PUBLISH} {kind}")),
        "a default-held kind is never the object of a publish command:\n{said}"
    );
    let line = out
        .lines()
        .find(|l| {
            l.starts_with(&format!(
                "registry-drift: protocol:{kind} — HELD, not filed"
            ))
        })
        .unwrap_or_else(|| panic!("no held line for {kind} in:\n{out}"));
    assert!(
        line.contains("by default: step ") && line.contains("declares writer signer"),
        "the held line names the declaration that holds it: {line}"
    );
    assert!(
        err.lines()
            .any(|l| l.contains(&format!("{kind} — by default: step "))
                && l.contains("declares writer signer")),
        "the lint names {kind} as held by default:\n{err}"
    );
    let scope = err
        .lines()
        .find(|l| l.contains("is for the kind(s) that are NOT held"))
        .unwrap_or_else(|| panic!("the lint does not scope its command:\n{err}"));
    assert!(!scope.contains(kind.as_str()), "{scope}");
    assert!(
        last_line(&out).contains(&format!("1 held and not filed ({kind})")),
        "{}",
        last_line(&out)
    );
}

/// AN ITEM ALREADY OPEN is the voice a hold must also reach (review
/// 8d088b41, B2). On 2026-10-06 backlog-item 7ad62d1c, "Registry drift:
/// protocol ops-request — tree ahead by an edit", had been open five days
/// with a remedy ending in the hand publish. A held tree-ahead kind is no
/// longer a candidate, so the dedup never looks at such an item again —
/// and its advice would have stood for ever, for the one kind that is
/// held. So for EVERY held kind the converge reads its open drift item
/// and, when the item still names the publish, corrects it through the
/// metadata merge door: the remedy becomes the hold (why, what lifts it,
/// no publish) and the item is marked. Three held kinds: one the tree is
/// ahead on (not a candidate), one both-moved (a candidate — the dedup
/// would have left the old text standing), one that does not drift at
/// all (a drift the daily publish cleared, its item never closed). A
/// kind that is NOT held keeps its item as it was. Once corrected, the
/// next converge touches nothing.
#[test]
fn an_open_item_for_a_held_kind_loses_its_publish_remedy() {
    if !has_tools() {
        eprintln!("skipping: needs python3, git and curl");
        return;
    }
    let p = plants();
    let quiet = bundle_kinds()[5].clone();
    let held = [p.tree_ahead.clone(), p.both_moved.clone(), quiet.clone()];
    let repo = fixture_repo_with("stale-item", &p, |dir| {
        held.iter().for_each(|k| hold(dir, k))
    });
    let mode = format!(
        "stale:{},{},{},{}",
        p.tree_ahead, p.both_moved, quiet, p.live_ahead
    );
    let stub = start_stub("stale-item", &p, &mode);
    let base = format!("http://127.0.0.1:{}", stub.port);

    let (rc, out, err) = run(&repo, &base, Some(&base));
    assert_eq!(rc, Some(0), "{out}\n{err}");
    let patches = stub.patches();
    assert_eq!(
        patches.len(),
        3,
        "one correction per held kind with an open item that names the publish, and none for \
         the kind that is not held:\n{out}\n{err}\n{}",
        stub.log()
    );
    for kind in &held {
        let (path, who, body) = patches
            .iter()
            .find(|(path, _, _)| path.ends_with(&format!("-stale-{kind}/metadata")))
            .unwrap_or_else(|| panic!("no correction for {kind}: {patches:#?}"));
        assert!(
            path.starts_with("/api/jobs/"),
            "the metadata merge door: {path}"
        );
        assert!(who.contains("automation:cluster-deploy-runner"), "{who}");
        assert!(
            !body.to_string().contains(HAND_PUBLISH),
            "{kind}: nothing the correction writes names the publish: {body}"
        );
        let resolve = body["resolve"].as_str().unwrap();
        assert!(
            resolve.starts_with("no publish")
                && resolve.contains(HOLD_WHY)
                && resolve.contains(HOLD_LIFTS),
            "{kind}: the remedy is the hold: {resolve}"
        );
        assert!(
            body["description"]
                .as_str()
                .is_some_and(|d| d.contains(HOLD_WHY) && d.contains("withdrew")),
            "{kind}: the description is rewritten too, and says a remedy was withdrawn: {body}"
        );
        assert_eq!(
            body["publish_withdrawn"]["why"], HOLD_WHY,
            "{kind}: the item is marked with the hold that withdrew its remedy: {body}"
        );
        assert!(
            out.lines().any(|l| l.starts_with(&format!(
                "registry-drift: protocol:{kind} — open item 5ta1e00"
            )) && l.contains("corrected")),
            "{kind}: the journal says the item was corrected:\n{out}"
        );
    }
    assert!(
        !stub.posts().iter().any(
            |(_, _, b)| b["metadata"]["registry_drift"]
                .as_str()
                .is_some_and(|k| k.starts_with("protocol:")
                    && held.iter().any(|h| k == format!("protocol:{h}")))
        ),
        "an item already open is corrected, never filed twice: {:#?}",
        stub.posts()
    );
    let last = last_line(&out);
    assert!(
        last.contains("corrected 3 open item(s) for held kinds"),
        "the summary counts the corrections: {last}"
    );

    // The next converge: every such item already says the hold.
    let (rc, out, err) = run(&repo, &base, Some(&base));
    assert_eq!(rc, Some(0), "{out}\n{err}");
    assert_eq!(
        stub.patches().len(),
        3,
        "a corrected item is not corrected again:\n{out}\n{err}"
    );
    assert!(
        !last_line(&out).contains("corrected"),
        "{}",
        last_line(&out)
    );

    // A door that refuses is said, counted, and never fails the converge.
    let refused = start_stub("stale-item-403", &p, &mode.replace("stale:", "stale-403:"));
    let base = format!("http://127.0.0.1:{}", refused.port);
    let (rc, out, err) = run(&repo, &base, Some(&base));
    assert_eq!(rc, Some(0), "{out}\n{err}");
    assert!(
        last_line(&out).contains("NOT corrected 3"),
        "{}",
        last_line(&out)
    );
    assert!(
        err.contains("NOT corrected") && err.contains("answered HTTP 403"),
        "the refusal is said with its code:\n{err}"
    );

    // A DOOR'S ANSWER IS NOT ITS EFFECT (review 6ff3210e, N1). The door
    // answers 204; the first cut of this loop counted only 200, so a
    // correction that landed was recorded as not made. Neither code is
    // the test: the item is READ BACK, and it is corrected when it
    // carries the mark and the remedy this converge wrote. So a door
    // that answers 204 and changes nothing is NOT a correction.
    let hollow = start_stub(
        "stale-item-hollow",
        &p,
        &mode.replace("stale:", "stale-noeffect:"),
    );
    let base = format!("http://127.0.0.1:{}", hollow.port);
    let (rc, out, err) = run(&repo, &base, Some(&base));
    assert_eq!(rc, Some(0), "{out}\n{err}");
    assert_eq!(hollow.patches().len(), 3, "{out}\n{err}");
    assert!(
        last_line(&out).contains("NOT corrected 3") && !out.contains("corrected through"),
        "a 204 that changed nothing is not counted as a correction:\n{out}"
    );
    assert!(
        err.contains("answered HTTP 204") && err.contains("read back"),
        "and the journal says the answer and what the read-back showed:\n{err}"
    );
}

/// IDEMPOTENCE IS THE MARK, NOT THE PROSE (review 6ff3210e, N2). Whether
/// an item still needs correcting was decided by the words `boss workflow
/// publish` in its text — and the correction writes the hold's own `why`
/// into that text. A hold that names the door it guards ("goes live only
/// when David runs boss workflow publish for it") therefore re-corrected
/// its item on every converge, ~28 writes a day to a person's packet,
/// each reported as a fresh correction. An item carrying
/// `publish_withdrawn` for the hold as it stands IS corrected, whatever
/// its prose says. A hold that CHANGES (its why here) re-corrects once,
/// so the item never quotes a reason the tree no longer gives.
#[test]
fn a_hold_whose_own_words_name_the_door_corrects_its_item_once() {
    if !has_tools() {
        eprintln!("skipping: needs python3, git and curl");
        return;
    }
    let p = plants();
    let names_the_door = "This row turns on a refusal and goes live only when David runs boss workflow publish for it, with a control straight after.";
    let repo = fixture_repo_with("stale-door", &p, |dir| {
        hold_because(dir, &p.tree_ahead, names_the_door)
    });
    let stub = start_stub("stale-door", &p, &format!("stale:{}", p.tree_ahead));
    let base = format!("http://127.0.0.1:{}", stub.port);
    for converge in 1..=3 {
        let (rc, out, err) = run(&repo, &base, Some(&base));
        assert_eq!(rc, Some(0), "{out}\n{err}");
        assert_eq!(
            stub.patches().len(),
            1,
            "converge {converge}: one correction, ever — the mark decides, not the prose:\n{out}\n{err}"
        );
        assert_eq!(
            last_line(&out).contains("corrected 1 open item(s)"),
            converge == 1,
            "converge {converge}: {}",
            last_line(&out)
        );
    }
    assert_eq!(
        stub.patches()[0].2["publish_withdrawn"]["why"],
        names_the_door
    );

    // The hold's reason changes in the tree: corrected once more, then quiet.
    let new_why = "This row turns on the signer refusal; its deliberate publish waits for the positive control to be rehearsed.";
    hold_because(&repo, &p.tree_ahead, new_why);
    for converge in 4..=5 {
        let (rc, out, err) = run(&repo, &base, Some(&base));
        assert_eq!(rc, Some(0), "{out}\n{err}");
        assert_eq!(
            stub.patches().len(),
            2,
            "converge {converge}: a changed hold re-corrects its item once:\n{out}\n{err}"
        );
    }
    assert_eq!(stub.patches()[1].2["publish_withdrawn"]["why"], new_why);
}

/// NO DISAGREEMENT AT ALL is the shape that matters most (review
/// 6ff3210e, N4): on 2026-10-07 the live registries agreed with the tree
/// — 0 drift — while 61 drift items stood open, each naming the publish,
/// because nothing closes a drift item when its drift is published away.
/// A converge with nothing to file still reads the open list when a kind
/// is held, and corrects that kind's item; the old remedy's `side` goes
/// with it, since the registries now agree.
#[test]
fn a_held_kind_with_no_disagreement_still_has_its_open_item_corrected() {
    if !has_tools() {
        eprintln!("skipping: needs python3, git and curl");
        return;
    }
    let mut p = plants();
    p.clean = true;
    let kind = p.tree_ahead.clone();
    let repo = fixture_repo_with("stale-clean", &p, |dir| hold(dir, &kind));
    let stub = start_stub("stale-clean", &p, &format!("stale:{kind}"));
    let base = format!("http://127.0.0.1:{}", stub.port);
    let (rc, out, err) = run(&repo, &base, Some(&base));
    assert_eq!(rc, Some(0), "{out}\n{err}");
    assert!(stub.posts().is_empty(), "nothing to file:\n{out}\n{err}");
    let last = last_line(&out);
    assert!(
        last.starts_with("registry-drift: protocols: agree; rules: agree; nothing to file")
            && last.ends_with("corrected 1 open item(s) for held kinds"),
        "the fixture has no disagreement, and the held kind's item is corrected all the same: {last}"
    );
    let patches = stub.patches();
    assert_eq!(patches.len(), 1, "{out}\n{err}");
    let body = &patches[0].2;
    assert!(
        body["resolve"]
            .as_str()
            .is_some_and(|r| r.starts_with("no publish") && r.contains("no disagreement")),
        "{body}"
    );
    assert!(
        body.as_object().unwrap().contains_key("side") && body["side"].is_null(),
        "the stale `side` is removed (null deletes a key at the merge door): {body}"
    );
}

/// FAIL TOWARD NOT ADVISING. Holds that cannot be read are not absent
/// holds: neither voice falls back to the old text. Every disagreement a
/// publish would resolve is WITHHELD — not filed under a remedy nobody can
/// vouch for — and ONE item says the holds cannot be read, with the
/// reader's own words; the rest (a retirement, a live-ahead row, a rule)
/// is filed as ever. Five ways to be unreadable: a reader that answers 0
/// with a line nobody knows, one that dies saying nothing, the reader not
/// in the tree, the holds path a regular file (review finding F2 — it
/// used to read as no holds at all), and a hold file that does not parse.
#[test]
fn holds_that_cannot_be_read_advise_no_publish() {
    if !has_tools() {
        eprintln!("skipping: needs python3, git and curl");
        return;
    }
    let p = plants();
    type Damage = fn(&Path, &Plants);
    let cases: [(&str, Damage, &str); 5] = [
        // The two shapes no real damage produces alone (review 8d088b41,
        // F6): every problem the reader names comes with exit 1, so a
        // caller that checked only the exit, or only the lines, passed
        // every other case here. A reader that answers 0 with a line that
        // is neither `held` nor `released` (a newline inside a value would
        // do it), and one that dies saying nothing.
        (
            "odd-line",
            // A GOOD held line first, then the odd one: half an answer is
            // no answer, so the kind the good line names is not treated
            // as held either — its open item is left alone (below).
            |dir, p| {
                std::fs::write(
                    dir.join(HOLDS_READER),
                    format!(
                        "print('held\\t{}\\tdeclared in a file\\tA reason long enough to be a reason.\\tbacklog 6c9183de')\nprint('held\\tonly-two-cells')\n",
                        p.tree_ahead
                    ),
                )
                .unwrap()
            },
            "answered a line",
        ),
        (
            "silent-death",
            |dir, _| std::fs::write(dir.join(HOLDS_READER), "import sys\nsys.exit(3)\n").unwrap(),
            "said nothing",
        ),
        (
            "no-reader",
            |dir, _| std::fs::remove_file(dir.join(HOLDS_READER)).unwrap(),
            "does not carry infra/gcp/workflow-holds.py",
        ),
        (
            "path-is-a-file",
            |dir, _| {
                std::fs::create_dir_all(dir.join(HOLDS_REL).parent().unwrap()).unwrap();
                std::fs::write(dir.join(HOLDS_REL), "not a directory\n").unwrap();
            },
            "infra/platform/workflow-holds is not a directory",
        ),
        (
            "bad-hold",
            |dir, p| {
                std::fs::create_dir_all(dir.join(HOLDS_REL)).unwrap();
                std::fs::write(
                    dir.join(HOLDS_REL).join(format!("{}.toml", p.tree_ahead)),
                    "drift_publish = \"held\nwhy = ",
                )
                .unwrap();
            },
            "could not be read as TOML",
        ),
    ];
    for (name, damage, needle) in cases {
        let repo = fixture_repo_with(&format!("holds-{name}"), &p, |dir| damage(dir, &p));
        // An item already open for one of the withheld kinds, naming the
        // publish: with the holds unread, which kinds are held is not
        // known, so NO open item is corrected (review 6ff3210e, N4).
        let stub = start_stub(
            &format!("holds-{name}"),
            &p,
            &format!("stale:{}", p.tree_ahead),
        );
        let base = format!("http://127.0.0.1:{}", stub.port);
        let (rc, out, err) = run(&repo, &base, Some(&base));
        assert_eq!(
            rc,
            Some(0),
            "{name}: never fails the converge:\n{out}\n{err}"
        );
        assert!(
            stub.patches().is_empty(),
            "{name}: holds that cannot be read correct no item: {:?}",
            stub.patches()
        );
        assert!(
            last_line(&out).contains("holds: could not be read"),
            "{name}: the summary says the holds were not read: {}",
            last_line(&out)
        );
        let posts = stub.posts();
        // The stale item is the stub's own plant, not something this run said.
        let said = every_voice(&out, &err, &posts);
        assert!(
            !said.contains(HAND_PUBLISH),
            "{name}: with the holds unread, no voice names the hand publish:\n{said}"
        );
        assert_eq!(
            posts.len(),
            5,
            "{name}: live ahead, the retirement, the live-only kind, the rule, and ONE item for \
             the holds:\n{out}\n{err}"
        );
        let item = filed_item(&posts, "holds-check:unreadable");
        let description = item["metadata"]["description"].as_str().unwrap();
        assert!(
            description.contains(needle)
                && description.contains(&p.tree_ahead)
                && description.contains(&p.both_moved),
            "{name}: the item carries the reader's words and the kinds it withheld: {description}"
        );
        assert!(
            err.contains("NO PUBLISH IS ADVISED") && err.contains(needle),
            "{name}: the lint says so too, with the reason:\n{err}"
        );
        let last = last_line(&out);
        assert!(
            last.contains("2 withheld — the holds could not be read"),
            "{name}: the summary says what was withheld and why: {last}"
        );
    }
}

/// The dedup is the only thing between a disagreement and an item per
/// converge (~28 a day), so a dedup read that cannot be TRUSTED files
/// nothing: the open list refused (503), a page that stops short of its
/// own total, and a dark read — 200 with `total: 0`, a denied scope or a
/// wrong or empty instance — while POST would still work.
#[test]
fn an_untrusted_open_list_files_nothing() {
    if !has_tools() {
        eprintln!("skipping: needs python3, git and curl");
        return;
    }
    let p = plants();
    let repo = fixture_repo("untrusted", &p);
    for (mode, says) in [
        (
            "jobs-503",
            "the known-answer read of the open backlog answered HTTP 503",
        ),
        ("short-page", "the open list stopped at 0 of 3"),
        (
            "dark",
            "the open backlog read back 0 item(s), and it is never empty",
        ),
    ] {
        let stub = start_stub(&format!("untrusted-{mode}"), &p, mode);
        let base = format!("http://127.0.0.1:{}", stub.port);
        let (rc, out, err) = run(&repo, &base, Some(&base));
        assert_eq!(
            rc,
            Some(0),
            "{mode}: never fails the converge:\n{out}\n{err}"
        );
        assert!(
            stub.posts().is_empty(),
            "{mode}: nothing is filed on a dedup read it cannot trust:\n{out}\n{}",
            stub.log()
        );
        let last = last_line(&out);
        assert!(
            last.contains("nothing filed") && last.contains(says),
            "{mode}: the summary says why nothing was filed: {last}"
        );
    }
}

/// A SHALLOW checkout cannot see a retirement older than its depth, so a
/// live kind with no file there is "side not measured" — never "live
/// ahead", which would advise writing a retired kind back into the tree.
#[test]
fn a_shallow_checkout_does_not_advise_resurrecting_a_retired_kind() {
    if !has_tools() {
        eprintln!("skipping: needs python3, git and curl");
        return;
    }
    let p = plants();
    let full = fixture_repo("shallow-src", &p);
    let shallow = boss_testing::scratch_dir("registry-drift-shallow");
    let out = Command::new("git")
        .args(["clone", "-q", "--depth", "1"])
        .arg(format!("file://{}", full.display()))
        .arg(&shallow)
        .output()
        .expect("git clone");
    assert!(
        out.status.success(),
        "shallow clone: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stub = start_stub("shallow", &p, "serve");
    let base = format!("http://127.0.0.1:{}", stub.port);
    let (rc, out, err) = run(&shallow, &base, Some(&base));
    assert_eq!(rc, Some(0), "{out}\n{err}");
    let posts = stub.posts();
    for kind in [p.retired.as_str(), LIVE_ONLY_KIND] {
        let item = filed_item(&posts, &format!("protocol:{kind}"));
        assert!(
            item["title"]
                .as_str()
                .unwrap()
                .ends_with("side not measured"),
            "{kind} on a shallow checkout: {item}"
        );
    }
}

/// The converge runs the phase LAST — after every roll, so every seed has
/// run — records its summary on the packet, and cannot be failed by it.
#[test]
fn the_converge_runs_the_comparison_last_and_cannot_fail_on_it() {
    let src =
        std::fs::read_to_string(repo_root().join("infra/forge/cluster-deploy-runner.sh")).unwrap();
    let last_roll = src
        .rfind("roll_deployment \"$K\"")
        .expect("the runner rolls the deployments");
    let call = src
        .find("\"$REPO/infra/forge/registry-drift.sh\" --repo \"$REPO\"")
        .expect("the runner runs registry-drift.sh on the converged checkout");
    assert!(
        call > last_roll,
        "the comparison runs after the last roll, when the seeds have run"
    );
    // The phase, lifted verbatim from its STAGE line to the end of the
    // runner, run under the runner's own `set -euo pipefail` against a
    // stand-in script: one that says its summary, and one that dies
    // saying nothing with the usage-error status.
    let start = src
        .find("STAGE=\"registry drift\"")
        .expect("the phase names its stage");
    assert!(start < call, "the stage is named before the phase runs");
    let block = &src[start..];
    for (case, stand_in, recorded) in [
        (
            "says",
            "echo 'registry-drift: protocols read at x'\necho 'registry-drift: protocols: agree; rules: agree; nothing to file'\n",
            "protocols: agree; rules: agree; nothing to file",
        ),
        (
            "dies",
            "echo 'half a line' >&2\nexit 2\n",
            "the phase exited 2; last said: nothing",
        ),
    ] {
        let dir = boss_testing::scratch_dir(&format!("registry-drift-phase-{case}"));
        std::fs::create_dir_all(dir.join("infra/forge")).unwrap();
        boss_testing::write_exec(
            &dir.join("infra/forge/registry-drift.sh"),
            &format!("#!/usr/bin/env bash\n{stand_in}"),
        );
        let fields = dir.join("fields");
        let harness = format!(
            "set -euo pipefail\nREPO='{}'\nrun_summary_field() {{ printf '%s=%s\\n' \"$1\" \"$2\" >> '{}'; }}\n{block}\necho PHASE-DONE\n",
            dir.display(),
            fields.display()
        );
        let out = Command::new("bash")
            .arg("-c")
            .arg(&harness)
            .output()
            .expect("bash");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && stdout.contains("PHASE-DONE"),
            "{case}: the phase never fails the converge ({:?}):\n{stdout}\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        let fields = std::fs::read_to_string(&fields).unwrap_or_default();
        assert_eq!(
            fields.trim_end(),
            format!("registry_drift={recorded}"),
            "{case}: the packet carries what the phase said, or that it said nothing"
        );
    }
}

/// Promise 3: a registry that cannot be read is NOT a clean comparison
/// and NOT a failed converge. Both registries answering 503 while the
/// filing path is up files nothing; a system of record that is dark
/// altogether files nothing. Both exit 0 and say what was not compared.
#[test]
fn an_unreachable_registry_files_nothing_and_does_not_fail_the_converge() {
    if !has_tools() {
        eprintln!("skipping: needs python3, git and curl");
        return;
    }
    let p = plants();
    let repo = fixture_repo("down", &p);
    let stub = start_stub("down", &p, "registries-down");
    let base = format!("http://127.0.0.1:{}", stub.port);

    let (rc, out, err) = run(&repo, &base, Some(&base));
    assert_eq!(
        rc,
        Some(0),
        "an unreadable registry never fails the converge:\n{out}\n{err}"
    );
    assert!(
        stub.posts().is_empty(),
        "nothing is filed from no comparison:\n{}",
        stub.log()
    );
    let last = last_line(&out);
    assert!(
        last.starts_with(
            "registry-drift: protocols: not compared (exit 75); rules: not compared (exit 3)"
        ),
        "the summary names both comparisons as not made, with the lints' exits: {last}"
    );
    assert!(last.ends_with("nothing to file"), "{last}");

    // Dark altogether: nothing answers on the jobs port, and the rules
    // are read on the same host at the dispatcher's LAN-door port, which
    // sor-ports.env names — never at a guessed address.
    let (rc, out, err) = run(&repo, "http://127.0.0.1:9", None);
    assert_eq!(
        rc,
        Some(0),
        "a dark system of record never fails the converge:\n{out}\n{err}"
    );
    assert!(
        out.contains("rules at http://127.0.0.1:7950"),
        "the dispatcher is the SoR's host on sor-ports.env's dispatcher port: {out}"
    );
    assert!(
        last_line(&out).contains("protocols: not compared (exit 75)"),
        "{out}\n{err}"
    );
}
