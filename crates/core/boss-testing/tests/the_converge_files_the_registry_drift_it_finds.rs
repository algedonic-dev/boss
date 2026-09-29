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
}

fn plants() -> Plants {
    let k = bundle_kinds();
    Plants {
        tree_ahead: k[0].clone(),
        live_ahead: k[1].clone(),
        retired: k[2].clone(),
        both_moved: k[3].clone(),
    }
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
    let dir = boss_testing::scratch_dir(&format!("registry-drift-repo-{case}"));
    for rel in [
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
    git(&dir, TREE_COMMITTED_AT, &["init", "-q", "-b", "main", "."]);
    git(&dir, TREE_COMMITTED_AT, &["config", "user.email", "t@t"]);
    git(&dir, TREE_COMMITTED_AT, &["config", "user.name", "t"]);
    git(&dir, TREE_COMMITTED_AT, &["add", "-A"]);
    git(&dir, TREE_COMMITTED_AT, &["commit", "-q", "-m", "the tree"]);
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
        let path = repo_root().join(format!("infra/platform/workflows/{kind}.toml"));
        let doc: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
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
        rows.push(row);
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
fn rules_fixture() -> String {
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
    rules.push(serde_json::json!({"name": LIVE_ONLY_RULE, "version": 1, "authored": false, "source": "product"}));
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
        let log = self.log();
        let mut lines = log.lines();
        let mut out = Vec::new();
        while let Some(l) = lines.next() {
            if let Some(path) = l.strip_prefix("POST ") {
                let rec: serde_json::Value = serde_json::from_str(
                    lines.next().expect("a POST line is followed by its body"),
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
    std::fs::write(&rules, rules_fixture()).unwrap();
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
