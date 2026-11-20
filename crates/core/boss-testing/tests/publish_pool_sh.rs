//! `infra/gcp/publish-pool.sh` and its verb file, `publish-pool.json` —
//! the automaton step behind the website approval of a resource pool row
//! (backlog f399458f ask 2; David 2026-11-08: "we should be able to make
//! these registry changes via a different protocol than the code
//! changes").
//!
//! The script is RUN, not read — against a stub `boss` binary and a stub
//! `curl` standing in for the system of record — so every verdict below
//! is one the script reached. What is pinned:
//!
//! - the verb file declares ONE arg, the pool's name, whose pattern
//!   admits every pool the tree ships and nothing that could reach a
//!   path or a flag; it names its approvers and says MUTATING;
//! - a packet its owner signed with a passkey over THESE bytes is
//!   published from the packet's own `row_toml` — never a checkout —
//!   through `boss pool publish <name> <file> --over <signed live>`, and
//!   the confirmed line is the one the answer rule reads;
//! - every way a packet fails to sign this publish refuses 9 before the
//!   CLI runs, judged by infra/lib/signed-publish.jq, the program
//!   publish-workflow.sh runs too;
//! - a host CLI with no `--over` is a host fault (78), not a refusal of
//!   the packet.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use std::path::PathBuf;
use std::process::Command;

const SCRIPT: &str = "infra/gcp/publish-pool.sh";
const VERB: &str = "infra/ops/verbs/publish-pool.json";
const POOL: &str = "robots:opus";
const REQUEST: &str = "f399458f-0000-4000-8000-0000000000b1";
const PACKET: &str = "f399458f-0000-4000-8000-0000000000aa";
const APPROVE_TITLE: &str = "Approve publishing pool row robots:opus over live v2";
const ROW: &str = "[[pool]]\nname = \"robots:opus\"\nversion = 3\nstatus = \"active\"\nunit = \"slot\"\nkind = \"flow\"\nshape = \"capacity\"\ncapacity = { state = \"configured\", amount = 8 }\nmodels = [\"opus-5-5\"]\ncapacity_from = 'a fixture'\n";

fn has(tool: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {tool} >/dev/null 2>&1")])
        .status()
        .is_ok_and(|s| s.success())
}

fn ready() -> bool {
    for tool in ["jq", "sha256sum"] {
        if !has(tool) {
            eprintln!("skipping: publish-pool.sh needs {tool}, and this box has none");
            return false;
        }
    }
    true
}

/// sha256 by the same tool the script hashes with.
fn sha256(text: &str) -> String {
    let file = scratch_dir("publish-pool-sha").join("row.toml");
    write_file(&file, text);
    let out = Command::new("sha256sum")
        .arg(&file)
        .output()
        .expect("sha256sum runs");
    String::from_utf8_lossy(&out.stdout)[..64].to_string()
}

struct Case {
    root: PathBuf,
    bin: PathBuf,
}

impl Case {
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("publish-pool-{name}"));
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        write_exec(
            &bin.join("boss"),
            r#"#!/bin/sh
# stub boss: `pool publish --help` names --over unless STUB_OLD_CLI;
# a publish logs its argv and the bytes it was handed, and confirms.
case "$*" in
  "pool publish --help")
    echo "Usage: boss pool publish <NAME> <PATH> [--dry-run]"
    [ -n "${STUB_OLD_CLI:-}" ] || echo "      --over <VERSION>  refuse unless the lineage's newest is this version"
    exit 0 ;;
  "pool publish "*)
    echo "$*" >> "$STUB_BOSS_LOG"
    cp "$4" "$STUB_PUBLISHED"
    echo "boss pool: $3 v3 lints clean (capacity 6 -> 8)"
    echo "boss pool: published $3 v3 — confirmed by reading the lineage back"
    echo "$3"
    echo "  v3   active   capacity=8"
    exit 0 ;;
  *) echo "stub boss: unexpected $*" >&2; exit 99 ;;
esac
"#,
        );
        write_exec(
            &bin.join("curl"),
            r#"#!/bin/sh
# stub curl: a JOB read is served from STUB_JOBS/<id>.json, 404 when
# there is none; the HTTP code goes out via -w.
out=""; code=0; url=""
while [ $# -gt 0 ]; do
  case "$1" in
    -o) out="$2"; shift ;;
    -w) code=1; shift ;;
    -H|-m) shift ;;
    http*) url="$1" ;;
  esac
  shift
done
src="${STUB_JOBS:-/nonexistent}/${url##*/api/jobs/}.json"; status=200
[ -f "$src" ] || { src=/dev/null; status=404; }
if [ -n "$out" ]; then cp "$src" "$out"; else cat "$src"; fi
[ "$code" = 1 ] && printf '%s' "$status"
exit 0
"#,
        );
        Self { root, bin }
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.root.join("boss.log")).unwrap_or_default()
    }

    fn published(&self) -> Option<String> {
        std::fs::read_to_string(self.root.join("published.toml")).ok()
    }

    fn plant(&self, request: &serde_json::Value, packet: &serde_json::Value) -> PathBuf {
        let dir = self.root.join("jobs");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        write_file(&dir.join(format!("{REQUEST}.json")), &request.to_string());
        write_file(&dir.join(format!("{PACKET}.json")), &packet.to_string());
        dir
    }

    fn run(
        &self,
        args: &[&str],
        request: &serde_json::Value,
        packet: &serde_json::Value,
        extra: &[(&str, &str)],
    ) -> (i32, String) {
        let jobs = self.plant(request, packet);
        let _ = std::fs::remove_file(self.root.join("boss.log"));
        let _ = std::fs::remove_file(self.root.join("published.toml"));
        let mut cmd = Command::new("bash");
        cmd.arg(repo_root().join(SCRIPT))
            .args(args)
            .env_clear()
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("BOSS_JOBS_URL", "http://sor.invalid")
            .env("OPS_REQUEST_ID", REQUEST)
            .env("STUB_JOBS", &jobs)
            .env("STUB_BOSS_LOG", self.root.join("boss.log"))
            .env("STUB_PUBLISHED", self.root.join("published.toml"));
        for (k, v) in extra {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("publish-pool.sh runs");
        (
            out.status.code().unwrap_or(-1),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    }
}

/// Stamp `approve` (step 0) as the gateway would: bound to
/// `step_shape_hash` of its CURRENT title and metadata.
fn stamp(packet: &mut serde_json::Value, who: &str, assurance: &str) {
    let approve = packet["steps"][0].clone();
    let shape = boss_core::job::step_shape_hash(
        approve["title"].as_str().expect("approve has a title"),
        &approve["metadata"],
    );
    packet["steps"][0]["sign_offs"] = serde_json::json!([{
        "authority_id": who, "role": "platform-admin",
        "stamped_at": "2026-11-08T16:00:00.000000Z", "shape_hash": shape,
        "assurance": assurance, "presence_nonce": "fixture-nonce",
    }]);
}

/// A pool packet as the jobs API serves it, signed by David with a
/// passkey over exactly ROW's bytes against live v2.
fn signed_packet() -> serde_json::Value {
    let mut p = serde_json::json!({
        "id": PACKET, "kind": "publish-a-protocol", "status": "open",
        "metadata": {"registry": "pool", "row_name": POOL, "row_toml": ROW},
        "steps": [
            {"id": "00000000-0000-4000-8000-0000000000a1", "spec_slug": "approve",
             "title": APPROVE_TITLE, "status": "completed",
             "assurance_required": "presence", "sign_offs_required": ["platform-admin"],
             "sign_offs": [],
             "metadata": {
                 "human_only": true, "decision": "approved",
                 "registry": "pool", "row_name": POOL,
                 "spec_path": format!("infra/platform/pools/{POOL}.toml"),
                 "spec_sha256": sha256(ROW), "live_version": "2", "tree_sha": "0".repeat(40),
                 "dry_run": "boss pool: robots:opus v3 lints clean (capacity 6 -> 8)",
                 "diff": "capacity.amount: live 6 -> row 8",
             }},
            {"id": "00000000-0000-4000-8000-0000000000a2", "spec_slug": "publish-pool",
             "status": "ready",
             "metadata": {"ops_verb": "publish-pool", "pool": POOL}},
        ],
    });
    stamp(&mut p, "emp-david", "presence");
    p
}

fn request() -> serde_json::Value {
    serde_json::json!({"id": REQUEST, "kind": "ops-request", "status": "open",
        "metadata": {"host": "boss-gcp", "verb": "publish-pool", "args": [POOL],
                     "for_protocol_publish": PACKET}})
}

#[test]
fn the_verb_file_declares_one_pool_arg_its_script_and_its_approvers() {
    let v: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(repo_root().join(VERB)).expect("the publish-pool verb file"),
    )
    .expect("the verb file is JSON");
    let about = v["about"].as_str().unwrap_or_default();
    for needle in [
        "MUTATING",
        "David",
        "f399458f",
        "signed-publish.jq",
        "--over",
    ] {
        assert!(
            about.contains(needle),
            "the about names `{needle}`: {about}"
        );
    }
    assert_eq!(v["hosts"], serde_json::json!(["boss-gcp"]));
    assert_eq!(v["argv"], serde_json::json!([SCRIPT, "{1}"]));
    let params = v["params"].as_array().expect("params");
    assert_eq!(params.len(), 1, "ONE arg: the bytes never ride argv");
    assert_eq!(params[0]["name"], "pool");
    let pat = params[0]["pattern"]
        .as_str()
        .expect("a pattern")
        .to_string();
    let matches = |s: &str| {
        Command::new("sh")
            .args([
                "-c",
                r#"printf '%s' "$2" | grep -Eqx -- "$1""#,
                "sh",
                &pat,
                s,
            ])
            .status()
            .is_ok_and(|st| st.success())
    };
    // Every pool the tree ships is a name the verb admits — derived from
    // the bundle directory, not listed here.
    let shipped: Vec<String> = std::fs::read_dir(repo_root().join("infra/platform/pools"))
        .expect("the pools bundle")
        .filter_map(|e| {
            let name = e.ok()?.file_name().to_string_lossy().to_string();
            name.strip_suffix(".toml").map(str::to_string)
        })
        .collect();
    assert!(shipped.len() > 5, "the bundle read found {shipped:?}");
    for ok in &shipped {
        assert!(
            matches(ok),
            "the pool pattern refuses the shipped pool {ok}"
        );
    }
    for bad in [
        "-x",
        "a b",
        "Robots:opus",
        "robots:opus/../x",
        "../x",
        "robots:",
        ":opus",
        "a:b:c",
        "robots:opus;rm",
    ] {
        assert!(!matches(bad), "the pool pattern admits {bad:?}");
    }
    assert_eq!(v["approvers"], serde_json::json!(["emp-david"]));
    assert!(
        v.get("requires_approval").is_none(),
        "the passkey is on the packet, read by the script: {v}"
    );
    assert!(v["timeout"].as_u64().unwrap_or(0) >= 60);
    // The effect pattern reads the script's confirmed line.
    let effect = regex::Regex::new(v["effect"].as_str().expect("an effect")).expect("a regex");
    assert!(effect.is_match(&confirmed_line()));
}

fn confirmed_line() -> String {
    format!(
        "publish-pool: {POOL} v2 -> v3 live at http://sor.invalid — confirmed by reading the lineage back and comparing it to the signed row (equal); packet {REQUEST}"
    )
}

#[test]
fn a_signed_pool_packet_is_published_from_its_own_bytes_over_the_signed_version() {
    if !ready() {
        return;
    }
    let c = Case::new("signed");
    let (rc, out) = c.run(&[POOL], &request(), &signed_packet(), &[]);
    assert_eq!(rc, 0, "{out}");
    assert!(
        out.contains(&confirmed_line()),
        "the line complete-pool-publish-on-publish-pool-answered reads:\n{out}"
    );
    let log = c.log();
    let calls: Vec<&str> = log.lines().collect();
    assert_eq!(calls.len(), 1, "ONE publish: {calls:?}");
    let argv: Vec<&str> = calls[0].split(' ').collect();
    assert_eq!(&argv[..3], &["pool", "publish", POOL]);
    assert_eq!(
        &argv[4..],
        &["--over", "2"],
        "the publish is held to the signed live version"
    );
    assert_eq!(
        c.published().as_deref(),
        Some(ROW),
        "the bytes published are the packet's signed row_toml, byte for byte"
    );
}

#[test]
fn a_packet_that_does_not_sign_this_publish_is_refused_before_the_cli() {
    if !ready() {
        return;
    }
    let c = Case::new("unsigned");
    type Mutate = fn(&mut serde_json::Value, &mut serde_json::Value);
    let cases: [(&str, Mutate, &str); 8] = [
        (
            "bytes swapped after signing",
            |p, _| p["metadata"]["row_toml"] = serde_json::json!(ROW.replace("8", "80")),
            "these are not the bytes that were signed",
        ),
        (
            "a protocol row's packet",
            |p, _| {
                p["metadata"]["registry"] = serde_json::json!("workflow");
                p["steps"][0]["metadata"]["registry"] = serde_json::json!("workflow");
                stamp(p, "emp-david", "presence");
            },
            "not a pool row",
        ),
        (
            "another pool",
            |p, _| {
                p["metadata"]["row_name"] = serde_json::json!("robots:sol");
            },
            "asks to publish robots:sol, not robots:opus",
        ),
        (
            "a session stamp",
            |p, _| stamp(p, "emp-david", "session"),
            "not presence: no passkey signed it",
        ),
        (
            "edited after signing",
            |p, _| p["steps"][0]["metadata"]["diff"] = serde_json::json!("capacity: 6 -> 80"),
            "what is on the step is not what was signed",
        ),
        (
            "a stranger signed",
            |p, _| stamp(p, "emp-mallory", "presence"),
            "not among the approvers infra/ops/verbs/publish-pool.json names",
        ),
        (
            "already spent",
            |p, _| p["steps"][1]["status"] = serde_json::json!("completed"),
            "its publish-pool step is completed: a signature is spent on one publish",
        ),
        (
            "no link",
            |_, r| {
                r["metadata"]
                    .as_object_mut()
                    .expect("request metadata")
                    .remove("for_protocol_publish");
            },
            "names no publish-a-protocol packet",
        ),
    ];
    for (name, mutate, needle) in cases {
        let mut packet = signed_packet();
        let mut req = request();
        mutate(&mut packet, &mut req);
        let (rc, out) = c.run(&[POOL], &req, &packet, &[]);
        assert_eq!(rc, 9, "{name}: {out}");
        assert!(out.contains("REFUSED"), "{name}: {out}");
        assert!(
            out.contains(needle),
            "{name}: expected `{needle}` in:\n{out}"
        );
        assert!(
            c.log().is_empty(),
            "{name}: a refused packet reached the CLI"
        );
    }
}

#[test]
fn a_host_cli_without_the_door_or_a_name_the_pattern_refuses_never_publishes() {
    if !ready() {
        return;
    }
    let c = Case::new("host");
    let (rc, out) = c.run(
        &[POOL],
        &request(),
        &signed_packet(),
        &[("STUB_OLD_CLI", "1")],
    );
    assert_eq!(rc, 78, "a CLI with no --over is a host fault: {out}");
    assert!(out.contains("--over"), "{out}");
    assert!(c.log().is_empty());
    for bad in ["../x", "robots:opus;rm", ""] {
        let (rc, out) = c.run(&[bad], &request(), &signed_packet(), &[]);
        assert_eq!(rc, 2, "{bad:?}: {out}");
        assert!(c.log().is_empty());
    }
}
