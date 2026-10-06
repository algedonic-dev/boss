//! The ops-runner EXECUTES a passkey-approved verb, and refuses every
//! approval it cannot verify (backlog fd7090cc, design 17835005).
//!
//! THE GAP THIS CLOSES, measured on origin/main 87c2e366 (2026-09-23).
//! The design had two halves on main: a destructive verb renders a plan
//! first (`plan-a-pod-reap`, `plan-a-tenant-merge`, each printing the
//! plan on stdout and `plan-sha256:` on stderr, the write re-rendering
//! and comparing), and ops-request v2 carries an `approve` sign-off step
//! with `assurance_required = "presence"`, so a passkey stamp binds
//! `step_shape_hash(title, metadata)` — the plan bytes on that step. The
//! third half was missing: the runner refused EVERY `requires_approval`
//! verb ("nothing issues one yet"), nothing wrote a plan onto the approve
//! step, so David's passkey approved nothing.
//!
//! WHAT THE RUNNER DOES NOW, and what these cases pin:
//!
//! - an approve step that is ready with no plan gets the verb's declared
//!   `plan_verb` rendered on the host and written onto it through the step
//!   metadata door; a plan verb that refuses closes the request refused,
//!   carrying its words;
//! - `execute` runs only when the approve step is COMPLETED and carries,
//!   for every required role, a PRESENCE stamp bound to the step's
//!   current shape hash — recomputed here, pinned equal to
//!   `boss_core::job::step_shape_hash` by the happy path below (§9a);
//! - the approval is single-use (execute is claimed `active` before the
//!   argv runs, and an `active` execute is never run again) and
//!   time-boxed (`APPROVAL_TTL_S`, ten minutes);
//! - the write gets sha256 of the SIGNED plan as its `plan_sha256`, so a
//!   state that drifted since the signature re-renders to different bytes
//!   and the write's own script refuses it.
//!
//! q2, as David ruled it: the runner trusts the system of record, and a
//! writable field is never an approval. Every refusal names what it
//! refused, on the request itself.
//!
//! Run against a stubbed system of record (a `curl` on PATH that serves
//! one packet on GET and logs every write, in order), so each verdict is
//! one the runner actually made.

use boss_testing::{feed_stdin, repo_root};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::Command;

fn has(tool: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {tool} >/dev/null 2>&1")])
        .status()
        .is_ok_and(|s| s.success())
}

/// THESE CASES NEVER PASS BY NOT RUNNING (security review of this car,
/// 2026-09-24). This macro used to `return` with a SKIPPED line when jq
/// or sha256sum was missing — and a test that returns early is reported
/// as passed, so every refusal below could have gone green on a box
/// where none of them ran. The gate image carries both tools, so a box
/// without them is a box these tests cannot vouch for, and that is a
/// failure naming the tool, not a pass.
macro_rules! needs_tools {
    () => {
        for tool in ["jq", "sha256sum"] {
            assert!(
                has(tool),
                "ops_runner_approval_sh: no {tool} on this box — the gate image has it, and a \
                 security test that cannot run must fail, never pass by returning early"
            );
        }
    };
}

/// The plan the fixture plan verb renders: every byte class a real plan
/// carries (quotes, a backslash, a tab, non-ASCII, a trailing newline),
/// because the runner's shape hash must agree with the server's over
/// exactly these, and a disagreement would refuse every real approval.
const PLAN: &str = "PLAN wipe target-a\n  observed: \"quoted\" and a back\\slash\n\tindented — é\nargv: wipe target-a <plan-sha256>\n";

const APPROVE_TITLE: &str = "Approve the plan: wipe on forge";

/// The approve step's procedure, as ops-request v2 materialises it: an
/// em dash and newlines, so the canonical form is exercised on prose.
const PROCEDURE: &str = "READ THE PLAN ON THIS STEP AND NOTHING ELSE.\n\nYOUR PASSKEY SIGNS THESE EXACT BYTES — nothing else.";

/// sha256 of `bytes`, by the same `sha256sum` the runner uses. Fed on
/// stdin rather than through a file: every case hashes, the cases run
/// on parallel threads of one process, and a shared scratch file would
/// be cleared under one case by another.
fn sha256_hex(bytes: &[u8]) -> String {
    let mut child = Command::new("sha256sum")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("sha256sum runs");
    // Its exit status is the verdict, not the write (backlog fec29a02).
    feed_stdin(&mut child, bytes);
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "sha256sum: {:?}", out.status);
    String::from_utf8_lossy(&out.stdout)[..64].to_string()
}

/// The stubbed system of record. A GET of the open-request LIST serves
/// `jobs.json`; a GET of ONE job (`/api/jobs/<id>`, the re-read the
/// runner makes immediately before it claims) serves `reread.json` when
/// a case wrote one, else that same job — so a case can move the record
/// between the runner's first read and its claim. Every write is logged
/// in order with its method, url, the `x-boss-user` it was signed as and
/// its body (`null` for a bodyless POST, which is what the claim door
/// is). `STUB_REFUSE_STATUS` answers 409 to a write whose body sets that
/// status; `STUB_REFUSE_CLAIM` answers 409 to the claim door
/// unconditionally — a race lost at the server after the runner's
/// re-read, which a static record cannot otherwise show.
/// `STUB_RACE_ONCE` answers the FIRST step PUT of a run 409 with that
/// value as its `code` — the step race lost once (car 88123ae0).
/// `STUB_REFUSE_URL` answers 403 to every write whose url ends with it —
/// the `written_by` refusal (backlog aa816dd4).
///
/// THE CLAIM DOOR JUDGES THE HOLDER THE WAY THE SERVER DOES (re-review of
/// 2026-09-25). This stub used to answer every claim 200, so the happy
/// path went green while the live door would have refused every runner
/// claim: the dispatcher had nominated each execute to `agent-claude`,
/// and the compare-and-set (`claim_step_at`, both adapters) admits a
/// READY step only when it is unheld or held by the claimant, and an
/// ACTIVE one only to its holder. The stub now reads the step from the
/// record the runner would re-read and applies exactly that rule,
/// answering 409 with the server's body — so a fixture that models the
/// dispatcher wrongly fails here, not in production.
const STUB_CURL: &str = r#"#!/bin/sh
m=GET; prev=; o=; w=; url=; body=; user=
for a in "$@"; do
    [ "$prev" = -X ] && m="$a"
    [ "$prev" = -o ] && o="$a"
    [ "$prev" = -w ] && w=1
    if [ "$prev" = -H ]; then
        case "$a" in "x-boss-user: "*) user="${a#x-boss-user: }" ;; esac
    fi
    case "$a" in @*) body="${a#@}" ;; http*) url="$a" ;; esac
    prev="$a"
done
if [ "$m" = GET ]; then
    case "$url" in
        */api/jobs/*)
            if [ -f "$STUB_DIR/reread.json" ]; then cat "$STUB_DIR/reread.json"
            else jq -c '.data[0]' "$STUB_DIR/jobs.json"; fi ;;
        *) cat "$STUB_DIR/jobs.json" ;;
    esac
    exit 0
fi
if [ -n "$body" ]; then b=$(cat "$body"); else b=null; fi
printf '%s' "$b" | jq -c --arg m "$m" --arg u "$url" --arg who "$user" \
    '{method: $m, url: $u, user: (($who | fromjson?) // $who), body: .}' >> "$STUB_DIR/writes.jsonl"
code=200; said='{"error":"stub refused"}'
if [ -n "${STUB_REFUSE_STATUS:-}" ] && [ "$(printf '%s' "$b" | jq -r '.status? // ""')" = "$STUB_REFUSE_STATUS" ]; then code=409; fi
case "$url" in *"${STUB_REFUSE_URL:-//never//}") code=403; said='{"error":"this step'"'"'s record is written by its declared writer"}' ;; esac
if [ -n "${STUB_RACE_ONCE:-}" ] && [ "$m" = PUT ] && [ ! -e "$STUB_DIR/raced" ]; then
    : > "$STUB_DIR/raced"; code=409
    said=$(jq -cn --arg c "$STUB_RACE_ONCE" '{error: "the stub lost a step race", code: $c}')
fi
case "$url" in
    */claim)
        if [ -n "${STUB_REFUSE_CLAIM:-}" ]; then code=409
        else
            sid=${url%/claim}; sid=${sid##*/}
            if [ -f "$STUB_DIR/reread.json" ]; then rec=$(cat "$STUB_DIR/reread.json")
            else rec=$(jq -c '.data[0]' "$STUB_DIR/jobs.json"); fi
            me=$(jq -rn --arg w "$user" '(($w | fromjson?) // {id: $w}) | .id // ""')
            verdict=$(printf '%s' "$rec" | jq -c --arg sid "$sid" --arg me "$me" '
                ((.steps // []) | map(select(.id == $sid)) | .[0]) as $s
                | ($s.assignee_id // null) as $h
                | if $s == null then {code: 404, body: "step not found"}
                  elif ($s.status == "ready" and ($h == null or $h == $me))
                       or ($s.status == "active" and $h == $me)
                  then {code: 200, body: ($s + {status: "active", assignee_id: $me})}
                  else {code: 409, body: {error: "step already claimed or not claimable",
                                          holder: $h, status: $s.status}} end')
            code=$(printf '%s' "$verdict" | jq -r '.code')
            said=$(printf '%s' "$verdict" | jq -c '.body')
        fi ;;
esac
if [ -n "$o" ]; then printf '%s' "$said" > "$o"; fi
if [ -n "$w" ]; then printf '%s' "$code"; exit 0; fi
[ "$code" = 200 ] || exit 22
exit 0
"#;

/// A fixture: the stubbed SoR, a plan verb and a write verb, and the
/// scripts behind them. `plan-wipe` prints [`PLAN`] and its hash (or
/// refuses when `PLAN_REFUSE` is set, or renders `PLAN_OVERRIDE`'s
/// bytes instead — the state having moved); `wipe` records the argv it
/// was given and how many writes the runner had made before it ran.
struct Fixture {
    root: PathBuf,
    verbs: PathBuf,
}

impl Fixture {
    fn new(case: &str) -> Self {
        let root = boss_testing::scratch_dir(&format!("ops-runner-approval-{case}"));
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        boss_testing::write_exec(&bin.join("curl"), STUB_CURL);
        std::fs::write(root.join("plan.txt"), PLAN).unwrap();
        let plan_sh = root.join("plan.sh");
        boss_testing::write_exec(
            &plan_sh,
            &format!(
                "#!/bin/sh\n\
                 if [ -n \"${{PLAN_REFUSE:-}}\" ]; then echo \"plan-wipe: REFUSED — $PLAN_REFUSE\" >&2; exit 78; fi\n\
                 p=\"{plan}\"\n\
                 if [ -n \"${{PLAN_OVERRIDE:-}}\" ]; then p=\"$PLAN_OVERRIDE\"; fi\n\
                 cat \"$p\"\n\
                 echo \"plan-sha256: $(sha256sum \"$p\" | cut -d' ' -f1)\" >&2\n",
                plan = root.join("plan.txt").display()
            ),
        );
        let apply_sh = root.join("apply.sh");
        boss_testing::write_exec(
            &apply_sh,
            &format!(
                "#!/bin/sh\n\
                 printf '%s\\n' \"$@\" > \"{root}/applied.args\"\n\
                 wc -l < \"{root}/writes.jsonl\" > \"{root}/writes-before-apply\"\n\
                 echo applied\n",
                root = root.display()
            ),
        );
        let verbs = root.join("verbs");
        std::fs::create_dir_all(&verbs).unwrap();
        std::fs::write(
            verbs.join("wipe.json"),
            json!({
                "about": "MUTATING — test fixture.",
                "hosts": ["forge"],
                "requires_approval": true,
                "plan_verb": "plan-wipe",
                "approvers": [APPROVER],
                "argv": [apply_sh.display().to_string(), "{1}", "{2}"],
                "params": [
                    {"name": "target", "pattern": "^[a-z-]{1,20}$"},
                    {"name": "plan_sha256", "pattern": "^[0-9a-f]{64}$"}
                ]
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            verbs.join("plan-wipe.json"),
            json!({
                "about": "READ-ONLY: renders what wipe would do.",
                "hosts": ["forge"],
                "argv": [plan_sh.display().to_string(), "{1}"],
                "params": [{"name": "target", "pattern": "^[a-z-]{1,20}$"}]
            })
            .to_string(),
        )
        .unwrap();
        Fixture { root, verbs }
    }

    fn verb_file(&self, name: &str, spec: Value) {
        std::fs::write(self.verbs.join(format!("{name}.json")), spec.to_string()).unwrap();
    }

    fn packet(&self, job: Value) {
        std::fs::write(
            self.root.join("jobs.json"),
            json!({"data": [job], "total": 1}).to_string(),
        )
        .unwrap();
    }

    /// What the ONE-job GET answers from here on: the record as it stands
    /// when the runner re-reads it immediately before claiming.
    fn reread(&self, job: Value) {
        std::fs::write(self.root.join("reread.json"), job.to_string()).unwrap();
    }

    /// Run the runner once. Returns its output and every write it made,
    /// in order.
    fn run(&self, env: &[(&str, &str)]) -> (String, Vec<Value>) {
        let _ = std::fs::remove_file(self.root.join("writes.jsonl"));
        let _ = std::fs::remove_file(self.root.join("raced"));
        std::fs::write(self.root.join("writes.jsonl"), "").unwrap();
        let _ = std::fs::remove_file(self.root.join("applied.args"));
        let path = format!(
            "{}:{}",
            self.root.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut cmd = Command::new("sh");
        cmd.arg(repo_root().join("infra/ops/ops-runner.sh"))
            .env_clear()
            .env("PATH", path)
            .env("HOST_ID", "forge")
            .env("BOSS_JOBS_URL", "http://sor.invalid")
            .env("OPS_VERBS_DIR", &self.verbs)
            .env("STUB_DIR", &self.root);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("ops-runner.sh runs");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let writes = std::fs::read_to_string(self.root.join("writes.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).expect("a logged write is JSON"))
            .collect();
        (text, writes)
    }

    /// The argv the write verb ran with, or None when it never ran.
    fn applied(&self) -> Option<Vec<String>> {
        std::fs::read_to_string(self.root.join("applied.args"))
            .ok()
            .map(|s| s.lines().map(str::to_string).collect())
    }
}

/// The one employee the fixture's write verb names as its approver.
const APPROVER: &str = "emp-david";

/// The approve step's metadata as v2 materialises it, plus — once the
/// runner has rendered — everything the runner writes there for the
/// passkey to sign: the plan, the verb, host and args it was rendered
/// for, and the hash of the bytes the runner itself rendered. And the
/// approver's `decision`, which both surfaces (sign-off.js and
/// ApprovalSurface) save BEFORE the stamp, so it sits inside the signed
/// shape: `approved` here, because a Reject completes the same step
/// through the same ceremony (see `a_rejected_approval_runs_nothing`).
fn approve_meta(plan: Option<&str>) -> Value {
    let mut m = json!({"authority_role": "platform-admin", "procedure": PROCEDURE});
    if let Some(p) = plan {
        m["plan"] = json!(p);
        m["verb"] = json!("wipe");
        m["host"] = json!("forge");
        m["args"] = json!(["target-a"]);
        m["rendered_plan_sha256"] = json!(sha256_hex(p.as_bytes()));
        m["decision"] = json!("approved");
    }
    m
}

/// The server's shape hash of an approve step carrying `meta` — the
/// value a presence stamp is bound to.
fn shape_of(meta: &Value) -> String {
    boss_core::job::step_shape_hash(APPROVE_TITLE, meta)
}

/// `secs` ago (negative: in the future), in the shape a live stamp's
/// `stamped_at` carries: RFC 3339, microseconds, `Z`.
fn iso_ago(secs: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let out = Command::new("date")
        .args([
            "-u",
            "-d",
            &format!("@{}", now - secs),
            "+%Y-%m-%dT%H:%M:%S.603230Z",
        ])
        .output()
        .expect("date runs");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// A sign-off stamp in the shape `SignOffStamp` serialises to.
fn stamp(role: &str, assurance: &str, shape: &str, age_s: i64) -> Value {
    let mut s = json!({
        "authority_id": APPROVER,
        "role": role,
        "stamped_at": iso_ago(age_s),
        "shape_hash": shape,
        "assurance": assurance,
    });
    if assurance == "presence" {
        s["presence_nonce"] = json!("nonce-1");
    }
    s
}

/// The agent executor the dispatcher nominates executable
/// `platform-admin` work to on the live deployment — the holder 299 of
/// 300 ops-request executes carried on 2026-09-25.
const NOMINEE: &str = "agent-claude";

/// The `execute` step as the system of record hands it to the runner:
/// its metadata materialised from the TREE's `ops-request.toml` (the
/// step's `authority_role`, and `claimable` when it declares one — the
/// keys `merge_metadata` writes), and its holder what the dispatcher
/// makes it. The dispatcher leaves a step unheld only when the protocol
/// declares a role queue (`claimable`, or `human_only`:
/// `left_for_role_queue` in boss-dispatcher); anything else it nominates.
/// So a protocol edit that drops the declaration puts `agent-claude` on
/// every fixture's execute, the stub's claim door refuses the runner as
/// the live one would, and the happy path goes red here.
fn execute_as_dispatched() -> (Value, Value) {
    let path = repo_root().join("infra/platform/workflows/ops-request.toml");
    let doc: toml::Table =
        toml::from_str(&std::fs::read_to_string(&path).expect("ops-request.toml is in the tree"))
            .expect("ops-request.toml parses");
    let step = doc["workflow"][0]["step"]
        .as_array()
        .and_then(|s| {
            s.iter()
                .find(|s| s.get("title").and_then(|t| t.as_str()) == Some("execute"))
        })
        .expect("ops-request declares an execute step")
        .clone();
    let mut md = json!({});
    if let Some(role) = step.get("authority_role").and_then(|v| v.as_str()) {
        md["authority_role"] = json!(role);
    }
    let claimable = step.get("claimable").and_then(|v| v.as_bool());
    if let Some(c) = claimable {
        md["claimable"] = json!(c);
    }
    let human_only = step.get("human_only").and_then(|v| v.as_bool()) == Some(true);
    let holder = if claimable == Some(true) || human_only {
        Value::Null
    } else {
        json!(NOMINEE)
    };
    (md, holder)
}

/// An ops-request packet for `wipe target-a` on the forge.
fn job(approve_status: &str, meta: Value, sign_offs: Value, exec_status: &str) -> Value {
    let (exec_md, exec_holder) = execute_as_dispatched();
    json!({
        "id": "aaaaaaaa-0000-4000-8000-000000000000",
        "status": "open",
        "metadata": {"host": "forge", "verb": "wipe", "args": ["target-a"], "requires_approval": true},
        "steps": [
            {"id": "s-filed", "spec_slug": "filed", "title": "Host read requested: wipe on forge",
             "status": "completed", "metadata": {}},
            {"id": "s-approve", "spec_slug": "approve", "title": APPROVE_TITLE,
             "status": approve_status, "assurance_required": "presence",
             "sign_offs_required": ["platform-admin"], "sign_offs": sign_offs, "metadata": meta},
            {"id": "s-execute", "spec_slug": "execute", "title": "Run wipe on forge",
             "status": exec_status, "assignee_id": exec_holder, "metadata": exec_md},
            {"id": "s-answered", "spec_slug": "answered", "title": "Answered",
             "status": "pending", "metadata": {"outcome_kind": "completed"}},
            {"id": "s-refused", "spec_slug": "refused", "title": "Refused — outside the allowlist",
             "status": "pending", "metadata": {"outcome_kind": "aborted"}}
        ]
    })
}

/// A packet whose approval is exactly right: the plan on a completed
/// approve step, one presence stamp bound to its shape, a minute old.
fn approved_job() -> Value {
    let meta = approve_meta(Some(PLAN));
    let shape = shape_of(&meta);
    job(
        "completed",
        meta,
        json!([stamp("platform-admin", "presence", &shape, 60)]),
        "ready",
    )
}

fn writes_to<'a>(writes: &'a [Value], step: &str) -> Vec<&'a Value> {
    writes
        .iter()
        .filter(|w| {
            w["url"]
                .as_str()
                .is_some_and(|u| u.contains(&format!("/steps/{step}")))
        })
        .collect()
}

/// The execute step's completion, which every execute-stage verdict
/// ends in.
fn execute_completion(writes: &[Value], out: &str) -> Value {
    step_completion(writes, "s-execute", out)
}

/// A step's completion as the runner makes it (backlog 2aa2b19e, the
/// gate runner's shape since e39a9d2a): the keys through the step merge
/// door, THEN a PUT of the status alone. Returns the keys, having held
/// both writes and their order — a PUT carrying metadata re-sends what
/// the runner read, and a status before its keys meets the
/// required-at-done check without them.
fn step_completion(writes: &[Value], step: &str, out: &str) -> Value {
    let to = writes_to(writes, step);
    let put = to
        .iter()
        .position(|w| w["method"] == "PUT" && w["body"]["status"] == "completed")
        .unwrap_or_else(|| panic!("{step} was never completed: {writes:?}\n{out}"));
    assert_eq!(
        to[put]["body"],
        json!({"status": "completed"}),
        "the completion PUT carries the status alone: {writes:?}"
    );
    to[..put]
        .iter()
        .rev()
        .find(|w| {
            w["method"] == "PATCH"
                && w["url"]
                    .as_str()
                    .is_some_and(|u| u.ends_with(&format!("/steps/{step}/metadata")))
        })
        .unwrap_or_else(|| {
            panic!("{step}'s keys did not go through the merge door before its status: {writes:?}\n{out}")
        })["body"]
        .clone()
}

/// Every execute-stage refusal: completed `refused`, the reason on the
/// step naming what failed, and the write verb never run.
fn assert_refused(f: &Fixture, out: &str, writes: &[Value], names: &[&str]) {
    assert!(
        f.applied().is_none(),
        "the write verb RAN on a refused approval: {:?}\n{out}",
        f.applied()
    );
    let md = execute_completion(writes, out);
    assert_eq!(md["disposition"], "refused", "{md}\n{out}");
    let reason = md["reason"].as_str().unwrap_or_default();
    for n in names {
        assert!(
            reason.contains(n),
            "the refusal names {n:?}: {reason}\n{out}"
        );
    }
    assert!(
        !writes_to(writes, "s-execute")
            .iter()
            .any(|w| w["body"]["status"] == "active"),
        "a refused approval claims nothing: {writes:?}"
    );
}

// ---------------------------------------------------------------- plan

/// A READY APPROVE STEP GETS ITS PLAN. The plan verb runs on the host
/// with the request's own args, and its stdout — the exact bytes whose
/// hash it printed — lands on the approve step's `plan` field through
/// the step metadata door, where the passkey will sign it. Nothing runs
/// the write, and execute is not touched.
#[test]
fn a_ready_approve_step_gets_the_plan_verbs_bytes() {
    needs_tools!();
    let f = Fixture::new("plan-rendered");
    f.packet(job("ready", approve_meta(None), json!([]), "pending"));
    let (out, writes) = f.run(&[]);
    assert!(
        f.applied().is_none(),
        "rendering a plan runs no write: {out}"
    );
    let patch = writes_to(&writes, "s-approve")
        .into_iter()
        .find(|w| w["method"] == "PATCH")
        .unwrap_or_else(|| panic!("no plan was written onto the approve step: {writes:?}\n{out}"));
    assert!(
        patch["url"]
            .as_str()
            .unwrap()
            .ends_with("/api/jobs/aaaaaaaa-0000-4000-8000-000000000000/steps/s-approve/metadata"),
        "the plan goes through the step metadata door: {patch}"
    );
    assert_eq!(
        patch["body"]["plan"], PLAN,
        "the plan is the plan verb's stdout, byte for byte: {patch}"
    );
    // WHAT THE PASSKEY SIGNS IS THE WHOLE REQUEST, not the plan alone
    // (security review, 2026-09-24): the verb, the host and the args the
    // plan was rendered for ride the same write, so they sit inside the
    // step's shape hash — an args edit on the request after the signature
    // cannot borrow it — and the hash of the bytes THIS runner rendered
    // rides beside them, so a plan written by anyone else is detectable.
    assert_eq!(patch["body"]["verb"], "wipe", "{patch}");
    assert_eq!(patch["body"]["host"], "forge", "{patch}");
    assert_eq!(patch["body"]["args"], json!(["target-a"]), "{patch}");
    assert_eq!(
        patch["body"]["rendered_plan_sha256"],
        sha256_hex(PLAN.as_bytes()),
        "{patch}"
    );
    assert!(
        writes_to(&writes, "s-execute").is_empty() && writes_to(&writes, "s-refused").is_empty(),
        "rendering a plan completes nothing: {writes:?}"
    );
    assert!(
        out.contains(&sha256_hex(PLAN.as_bytes())),
        "the journal names the hash the passkey will be asked to sign: {out}"
    );
}

/// A plan already on the approve step is the one waiting to be signed —
/// re-rendering it would move the bytes under a reviewer.
#[test]
fn a_plan_waiting_for_its_signature_is_not_rendered_again() {
    needs_tools!();
    let f = Fixture::new("plan-waits");
    f.packet(job("ready", approve_meta(Some(PLAN)), json!([]), "pending"));
    let (out, writes) = f.run(&[]);
    assert!(
        writes.is_empty(),
        "a waiting plan is left alone: {writes:?}\n{out}"
    );
    assert!(f.applied().is_none(), "{out}");
    assert!(out.contains("waits for a passkey approval"), "{out}");
}

/// A PLAN VERB THAT REFUSES CLOSES THE REQUEST REFUSED, carrying its
/// words: a plan for something that cannot run is not a plan, and a
/// request left open on it would wait for a signature nothing can use.
/// It closes through the `refused` terminal, which completes from any
/// open state (fd0f92ae) — execute is still pending behind the approve
/// step, so it cannot be the door.
#[test]
fn a_refused_plan_closes_the_request_refused_with_the_plan_verbs_words() {
    needs_tools!();
    let f = Fixture::new("plan-refused");
    f.packet(job("ready", approve_meta(None), json!([]), "pending"));
    let (out, writes) = f.run(&[("PLAN_REFUSE", "the target is mounted")]);
    assert!(f.applied().is_none(), "{out}");
    assert!(
        writes_to(&writes, "s-approve").is_empty(),
        "a refused plan writes no plan: {writes:?}"
    );
    let md = step_completion(&writes, "s-refused", &out);
    let reason = md["reason"].as_str().unwrap_or_default();
    assert!(
        reason.contains("plan-wipe") && reason.contains("the target is mounted"),
        "the refusal carries the plan verb's own words: {reason}"
    );
    // The close sends what it records and nothing it READ (backlog
    // 2aa2b19e): the stored keys — `outcome_kind` among them, which the
    // merge door guards as the protocol's — stay on the row untouched,
    // rather than riding a whole-metadata PUT back over it.
    let mut sent: Vec<&str> = md
        .as_object()
        .map(|m| m.keys().map(String::as_str).collect())
        .unwrap_or_default();
    sent.sort_unstable();
    assert_eq!(sent, ["reason", "runner_host"], "{md}");
}

/// A CLOSE THAT LOST THE STEP RACE IS SENT ONCE MORE (backlog 2aa2b19e
/// part 3). The refused terminal's close was a whole-body PUT with no
/// resend, so the one 409 that says nothing was written — another write
/// moved the step between the server's read and its write (car
/// 88123ae0) — left the request open over a refusal the runner had
/// already decided. Its status is now sent alone, after its keys went
/// through the merge door, so the same body goes once more exactly as
/// the execute completion's does.
#[test]
fn a_refused_close_that_lost_the_step_race_is_sent_once_more() {
    needs_tools!();
    let f = Fixture::new("close-race");
    f.packet(job("ready", approve_meta(None), json!([]), "pending"));
    let (out, writes) = f.run(&[
        ("PLAN_REFUSE", "the target is mounted"),
        (
            "STUB_RACE_ONCE",
            boss_jobs::step_metadata_write::STEP_CHANGED_CODE,
        ),
    ]);
    let puts: Vec<&Value> = writes_to(&writes, "s-refused")
        .into_iter()
        .filter(|w| w["method"] == "PUT")
        .collect();
    assert_eq!(
        puts.len(),
        2,
        "the lost close and exactly one resend: {writes:?}\n{out}"
    );
    assert!(
        puts.iter()
            .all(|w| w["body"] == json!({"status": "completed"})),
        "each is the status alone: {puts:?}"
    );
    assert!(
        out.contains("refused aaaaaaaa"),
        "the resent close landed and the request reads refused: {out}"
    );
}

// --------------------------------------------------------- nothing to do

/// The plan `plan-wipe` renders when there is nothing to wipe, and the
/// `nothing_to_do` it declares: the WHOLE plan, anchored at BOTH ends
/// (`\A` … `\z`), so a line copied into a plan with work in it cannot
/// match. It was anchored at its end alone until backlog aa816dd4 — the
/// fixture taught the shape the README forbids.
const NOTHING_PLAN: &str = "PLAN wipe target-a\nnothing to wipe\n";
const NOTHING_TO_DO: &str = "\\APLAN wipe target-a\\nnothing to wipe\\n\\z";

/// The job metadata key the runner sets and `ops-request.toml`'s
/// `nothing-to-do` terminal waits on — read out of the runner, so the
/// pin below holds the two copies equal (CLAUDE.md §9a).
fn nothing_marker() -> String {
    let runner = std::fs::read_to_string(repo_root().join("infra/ops/ops-runner.sh")).unwrap();
    runner
        .lines()
        .find_map(|l| l.strip_prefix("NOTHING_TO_DO_MARKER="))
        .map(|v| v.trim_matches('\'').to_string())
        .expect("ops-runner.sh defines NOTHING_TO_DO_MARKER")
}

impl Fixture {
    /// `plan-wipe` declares `nothing_to_do = re`; the plan it renders is
    /// [`NOTHING_PLAN`] (written beside it) when the case asks for it
    /// through `PLAN_OVERRIDE`.
    fn declare_nothing_to_do(&self, re: &str) {
        let path = self.verbs.join("plan-wipe.json");
        let mut spec: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        spec["nothing_to_do"] = json!(re);
        std::fs::write(&path, spec.to_string()).unwrap();
        std::fs::write(self.root.join("nothing.txt"), NOTHING_PLAN).unwrap();
    }

    fn nothing_plan(&self) -> String {
        self.root.join("nothing.txt").display().to_string()
    }
}

/// A request on the ops-request version that carries the `nothing-to-do`
/// terminal.
fn with_nothing_terminal(mut j: Value) -> Value {
    j["steps"].as_array_mut().unwrap().push(json!(
        {"id": "s-nothing", "spec_slug": "nothing-to-do",
         "title": "Nothing to do — the plan names no change",
         "status": "pending", "metadata": {"outcome_kind": "skipped"}}
    ));
    j
}

/// A PLAN THAT NAMES NOTHING TO DO CLOSES ITS REQUEST AND ASKS NOBODY
/// (backlog b2f78bb9, car 3 of 3df309bf). The machine files a remedy
/// when a finding persists; a remedy that already ran leaves a plan with
/// nothing in it, and that plan used to land on the approve step as a
/// passkey prompt in front of David for a no-op. Now the plan verb says
/// in its own file what its empty plan looks like, and a plan matching
/// it closes the request through its declared `nothing-to-do` terminal,
/// carrying the plan and everything it was rendered for — the record of
/// why nobody was asked. The approve step is never written, and nothing
/// runs.
#[test]
fn a_plan_that_names_nothing_to_do_closes_its_request_and_asks_nobody() {
    needs_tools!();
    let f = Fixture::new("nothing-to-do");
    f.declare_nothing_to_do(NOTHING_TO_DO);
    f.packet(with_nothing_terminal(job(
        "ready",
        approve_meta(None),
        json!([]),
        "pending",
    )));
    let (out, writes) = f.run(&[("PLAN_OVERRIDE", &f.nothing_plan())]);
    assert!(f.applied().is_none(), "nothing runs: {out}");
    assert!(
        writes_to(&writes, "s-approve").is_empty(),
        "no plan reaches the approve step, so no passkey is asked for: {writes:?}\n{out}"
    );
    let md = step_completion(&writes, "s-nothing", &out);
    assert_eq!(
        md["plan"], NOTHING_PLAN,
        "the plan rides on the record: {md}"
    );
    assert_eq!(md["verb"], "wipe", "{md}");
    assert_eq!(md["host"], "forge", "{md}");
    assert_eq!(md["args"], json!(["target-a"]), "{md}");
    assert_eq!(md["plan_verb"], "plan-wipe", "{md}");
    assert_eq!(md["nothing_to_do"], NOTHING_TO_DO, "{md}");
    assert_eq!(
        md["rendered_plan_sha256"],
        sha256_hex(NOTHING_PLAN.as_bytes()),
        "{md}"
    );
    // The terminal waits on the job marker, which is written AFTER the
    // step's keys (so the step is never ready without its record) and
    // BEFORE its status (so the completion meets a ready step).
    let marker = writes
        .iter()
        .position(|w| {
            w["method"] == "PATCH"
                && w["url"].as_str().is_some_and(|u| {
                    u.ends_with("/api/jobs/aaaaaaaa-0000-4000-8000-000000000000/metadata")
                })
        })
        .unwrap_or_else(|| panic!("the job marker was never written: {writes:?}\n{out}"));
    assert_eq!(
        writes[marker]["body"],
        json!({ nothing_marker(): true }),
        "{writes:?}"
    );
    let keys = writes
        .iter()
        .position(|w| {
            w["method"] == "PATCH"
                && w["url"]
                    .as_str()
                    .is_some_and(|u| u.ends_with("/steps/s-nothing/metadata"))
        })
        .unwrap();
    let put = writes
        .iter()
        .position(|w| {
            w["method"] == "PUT"
                && w["url"]
                    .as_str()
                    .is_some_and(|u| u.ends_with("/steps/s-nothing"))
        })
        .unwrap();
    assert!(keys < marker && marker < put, "{writes:?}");
    assert!(out.contains("nothing to do"), "the journal says so: {out}");
}

/// A plan with work in it is rendered for the passkey exactly as before,
/// whatever the plan verb declares.
#[test]
fn a_plan_with_work_in_it_still_waits_for_a_passkey() {
    needs_tools!();
    let f = Fixture::new("nothing-but-work");
    f.declare_nothing_to_do(NOTHING_TO_DO);
    f.packet(with_nothing_terminal(job(
        "ready",
        approve_meta(None),
        json!([]),
        "pending",
    )));
    let (out, writes) = f.run(&[]);
    let patch = writes_to(&writes, "s-approve")
        .into_iter()
        .find(|w| w["method"] == "PATCH")
        .unwrap_or_else(|| panic!("the plan was not rendered for approval: {writes:?}\n{out}"));
    assert_eq!(patch["body"]["plan"], PLAN, "{patch}");
    assert!(writes_to(&writes, "s-nothing").is_empty(), "{writes:?}");
    assert!(
        !writes.iter().any(|w| w["url"]
            .as_str()
            .is_some_and(|u| u.ends_with("-000000000000/metadata"))),
        "no marker on a plan with work: {writes:?}"
    );
}

/// A REQUEST FILED BEFORE THE TERMINAL EXISTED is pinned to the version
/// it was admitted under, which has nowhere to record "nothing to do":
/// its plan is rendered for the passkey as before, and the journal says
/// why. Asking is the safe side — a no-op signed is a no-op run.
#[test]
fn a_request_pinned_before_the_terminal_is_rendered_for_a_passkey_as_before() {
    needs_tools!();
    let f = Fixture::new("nothing-pinned");
    f.declare_nothing_to_do(NOTHING_TO_DO);
    f.packet(job("ready", approve_meta(None), json!([]), "pending"));
    let (out, writes) = f.run(&[("PLAN_OVERRIDE", &f.nothing_plan())]);
    let patch = writes_to(&writes, "s-approve")
        .into_iter()
        .find(|w| w["method"] == "PATCH")
        .unwrap_or_else(|| panic!("the plan was not rendered for approval: {writes:?}\n{out}"));
    assert_eq!(patch["body"]["plan"], NOTHING_PLAN, "{patch}");
    assert!(
        out.contains("no nothing-to-do terminal"),
        "the journal names why it asked: {out}"
    );
}

/// A declaration that cannot be judged — a regex jq will not compile —
/// asks: a guess that there is nothing to do would close a request
/// nobody looked at.
#[test]
fn a_nothing_to_do_that_cannot_be_judged_asks_for_a_passkey() {
    needs_tools!();
    let f = Fixture::new("nothing-unjudged");
    f.declare_nothing_to_do("(unclosed");
    f.packet(with_nothing_terminal(job(
        "ready",
        approve_meta(None),
        json!([]),
        "pending",
    )));
    let (out, writes) = f.run(&[("PLAN_OVERRIDE", &f.nothing_plan())]);
    assert!(
        writes_to(&writes, "s-approve")
            .iter()
            .any(|w| w["method"] == "PATCH"),
        "the plan was rendered for approval: {writes:?}\n{out}"
    );
    assert!(writes_to(&writes, "s-nothing").is_empty(), "{writes:?}");
    assert!(out.contains("could not be judged"), "{out}");
}

/// THE PROTOCOL HALF, held to the runner (§9a): `ops-request.toml`
/// declares the `nothing-to-do` terminal, gated on exactly the job key
/// the runner writes, false on a fresh request (absent reads false), its
/// record required at done — so a filer setting the marker by hand
/// cannot close a request with no plan on it — and `skipped`, the
/// outcome vocabulary's word for "not applicable here".
#[test]
fn the_nothing_to_do_terminal_waits_on_the_marker_the_runner_writes() {
    let path = repo_root().join("infra/platform/workflows/ops-request.toml");
    let doc: toml::Table = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let step = doc["workflow"][0]["step"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s.get("title").and_then(|t| t.as_str()) == Some("nothing-to-do"))
        .expect("ops-request declares a nothing-to-do terminal")
        .clone();
    assert_eq!(step["kind"].as_str(), Some("outcome"));
    assert_eq!(step["terminal"]["outcome"].as_str(), Some("nothing-to-do"));
    assert_eq!(
        step["metadata_defaults"]["outcome_kind"].as_str(),
        Some("skipped")
    );
    let ready = step["ready_when"].as_str().unwrap();
    assert!(
        ready.contains(&format!("job.metadata.{}", nothing_marker())),
        "the terminal waits on the runner's marker: {ready}"
    );
    let required: Vec<&str> = step["fields"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["required"].as_bool() == Some(true))
        .filter_map(|f| f["name"].as_str())
        .collect();
    for k in [
        "plan",
        "verb",
        "host",
        "args",
        "rendered_plan_sha256",
        "plan_verb",
        "nothing_to_do",
    ] {
        assert!(
            required.contains(&k),
            "{k} is required at done: {required:?}"
        );
    }
}

/// ONLY THE RUNNER WRITES THE RECORD (backlog aa816dd4, LOW-1 of review
/// ea2ecfd4), held to the runner (§9a): the `nothing-to-do` terminal
/// declares `written_by` as exactly the account ops-runner.sh signs as
/// when no unit overrides it, so the jobs API refuses the record from
/// every other automation or agent session and admits the runner's.
#[test]
fn the_nothing_to_do_record_is_written_by_the_runners_account() {
    let runner = std::fs::read_to_string(repo_root().join("infra/ops/ops-runner.sh")).unwrap();
    let actor = runner
        .lines()
        .find_map(|l| l.strip_prefix("ACTOR=\"${BOSS_OPS_ACTOR:-"))
        .and_then(|rest| rest.strip_suffix("}\""))
        .expect("ops-runner.sh defines ACTOR=\"${BOSS_OPS_ACTOR:-<account>}\"");
    let path = repo_root().join("infra/platform/workflows/ops-request.toml");
    let doc: toml::Table = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let step = doc["workflow"][0]["step"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s.get("title").and_then(|t| t.as_str()) == Some("nothing-to-do"))
        .expect("ops-request declares a nothing-to-do terminal")
        .clone();
    assert_eq!(
        step["metadata_defaults"]["written_by"].as_str(),
        Some(actor),
        "the nothing-to-do record's declared writer is the runner's account"
    );
}

/// A record write the jobs API refuses — a runner signing as another
/// account than the step declares — is a doubt, and a doubt asks: the
/// plan is rendered onto the approve step for a passkey, rather than
/// the request waiting forever on a close it can never make.
#[test]
fn a_nothing_to_do_record_the_server_refuses_asks_for_a_passkey() {
    needs_tools!();
    let f = Fixture::new("nothing-refused");
    f.declare_nothing_to_do(NOTHING_TO_DO);
    f.packet(with_nothing_terminal(job(
        "ready",
        approve_meta(None),
        json!([]),
        "pending",
    )));
    let (out, writes) = f.run(&[
        ("PLAN_OVERRIDE", &f.nothing_plan()),
        ("STUB_REFUSE_URL", "/steps/s-nothing/metadata"),
    ]);
    let patch = writes_to(&writes, "s-approve")
        .into_iter()
        .find(|w| w["method"] == "PATCH")
        .unwrap_or_else(|| panic!("the plan was not rendered for approval: {writes:?}\n{out}"));
    assert_eq!(patch["body"]["plan"], NOTHING_PLAN, "{patch}");
    assert!(
        out.contains("could not record nothing to do"),
        "the journal names the refusal: {out}"
    );
}

/// The plans each shipped `nothing_to_do` declaration is held to: plans
/// its verb renders that name NO change (must match) and plans that name
/// work (must not). A verb that declares `nothing_to_do` and has no row
/// here is a red gate — the README says every declaration is held to its
/// verb's plans, and until backlog aa816dd4 only the one verb hard-coded
/// below the loop was.
///
/// `plan-a-gcp-root-reclaim` names nothing to do when its plan is the
/// journal line alone: no backup, capture or checkout to remove, and a
/// journal the converge already caps at 256M (infra/gcp/journald-cap.conf),
/// so the fixed vacuum to 1G changes nothing. The plan David signed on
/// 2026-10-01 (ops-request f302d620, which removed /opt/boss-dev-bak) is
/// work, and so is a plan that keeps a checkout: it is shown to a person.
fn nothing_to_do_plans(verb: &str) -> Option<(Vec<String>, Vec<String>)> {
    match verb {
        "plan-a-gcp-root-reclaim" => {
            let journal = "would vacuum the journal to 1G (journalctl --vacuum-size=1G)\n";
            Some((
                vec![journal.to_string()],
                vec![
                    format!(
                        "would remove /opt/boss-dev-bak (536 MiB), holding:\n  .git\n  Cargo.toml\n{journal}"
                    ),
                    format!(
                        "would keep /opt/boss-binbak-x — it holds a git checkout, and this verb removes only /opt/boss-dev-bak's (when its plan proves it holds nothing unpushed). Its unpushed state, against its own remote-tracking refs as of its last fetch:\n  checkout /opt/boss-binbak-x:\n{journal}"
                    ),
                    format!("{journal}{journal}"),
                ],
            ))
        }
        _ => None,
    }
}

/// THE SHIPPED DECLARATIONS, EVERY ONE (backlog aa816dd4, LOW-3 of
/// review ea2ecfd4). Each `nothing_to_do` in the tree:
/// - is a regex jq compiles;
/// - sits on a verb some approval verb names as its plan verb —
///   anywhere else nothing would read it;
/// - is anchored to the WHOLE plan, `\A` first and `\z` last, as the
///   README requires: a plan can quote text it read off the host, and a
///   pattern free at either end lets that text close a request. Judged
///   on the text AND on the behaviour, because `\Aa|b\z` starts and ends
///   right and still matches a `b` anywhere: no nothing-plan matches
///   with a byte added before it or after it;
/// - is held to its verb's plans ([`nothing_to_do_plans`]): every
///   nothing-plan matches and every plan with work in it does not.
#[test]
fn the_shipped_nothing_to_do_declarations_hold() {
    needs_tools!();
    let judge = |re: &str, plan: &str| -> String {
        let out = Command::new("jq")
            .args(["-rn", "--arg", "p", plan, "--arg", "re", re])
            .arg("$p | test($re)")
            .output()
            .expect("jq runs");
        assert!(
            out.status.success(),
            "jq could not judge {re:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    let dir = repo_root().join("infra/ops/verbs");
    let mut specs = std::collections::BTreeMap::new();
    for e in std::fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "json") {
            let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
            specs.insert(p.file_stem().unwrap().to_string_lossy().into_owned(), v);
        }
    }
    let plan_verbs: Vec<&str> = specs
        .values()
        .filter(|v| v["requires_approval"] == json!(true))
        .filter_map(|v| v["plan_verb"].as_str())
        .collect();
    let mut declared = 0;
    for (name, v) in &specs {
        let Some(re) = v.get("nothing_to_do") else {
            continue;
        };
        declared += 1;
        let re = re
            .as_str()
            .unwrap_or_else(|| panic!("{name}: nothing_to_do is a regex string"));
        assert!(
            plan_verbs.contains(&name.as_str()),
            "{name} declares nothing_to_do but no approval verb renders its plan with it"
        );
        judge(re, "");
        assert!(
            re.starts_with("\\A") && re.ends_with("\\z"),
            "{name}'s nothing_to_do {re:?} is not anchored to the whole plan: it must open \
             with \\A and close with \\z (infra/ops/verbs/README.md)"
        );
        let (nothing, work) = nothing_to_do_plans(name).unwrap_or_else(|| {
            panic!(
                "{name} declares nothing_to_do but nothing_to_do_plans holds no plans for it: \
                 add the plans its verb renders with nothing in them and with work in them"
            )
        });
        assert!(
            !nothing.is_empty() && !work.is_empty(),
            "{name}: hold it to at least one plan of each"
        );
        for p in &nothing {
            assert_eq!(
                judge(re, p),
                "true",
                "{name}: a plan with nothing in it: {p:?}"
            );
            for forged in [format!("x{p}"), format!("{p}x")] {
                assert_eq!(
                    judge(re, &forged),
                    "false",
                    "{name}: {re:?} matches with a byte outside the plan, so it is not \
                     anchored to the whole plan: {forged:?}"
                );
            }
        }
        for p in &work {
            assert_eq!(
                judge(re, p),
                "false",
                "{name}: a plan with work in it: {p:?}"
            );
        }
    }
    assert!(declared > 0, "no plan verb declares nothing_to_do");
}

/// The anchoring check above is not vacuous: a pattern free at its
/// start — the shape this file's own fixture had until aa816dd4 — is
/// caught by the forged-prefix leg, and one free at its end by the
/// forged-suffix leg.
#[test]
fn an_unanchored_nothing_to_do_matches_a_forged_plan() {
    needs_tools!();
    let judge = |re: &str, plan: &str| -> bool {
        let out = Command::new("jq")
            .args(["-rn", "--arg", "p", plan, "--arg", "re", re])
            .arg("$p | test($re)")
            .output()
            .expect("jq runs");
        String::from_utf8_lossy(&out.stdout).trim() == "true"
    };
    assert!(judge("\\nnothing to wipe\\n\\z", "x\nnothing to wipe\n"));
    assert!(judge("\\Anothing to wipe\\n", "nothing to wipe\nx"));
    assert!(!judge(NOTHING_TO_DO, &format!("x{NOTHING_PLAN}")));
    assert!(!judge(NOTHING_TO_DO, &format!("{NOTHING_PLAN}x")));
}

/// An approval verb whose write cannot re-render and compare is
/// refused before any plan exists: its last param must be a required
/// `plan_sha256`, or an approval could outlive the state it approved.
/// `commission-a-disk` is the shipped instance, and stays inert.
#[test]
fn an_approval_verb_that_cannot_void_a_drifted_plan_is_refused() {
    needs_tools!();
    let f = Fixture::new("contract");
    f.verb_file(
        "wipe",
        json!({"about": "MUTATING — test fixture.", "hosts": ["forge"],
               "requires_approval": true, "plan_verb": "plan-wipe",
               "argv": ["true", "{1}"],
               "params": [{"name": "target", "pattern": "^[a-z-]{1,20}$"}]}),
    );
    f.packet(job("ready", approve_meta(None), json!([]), "pending"));
    let (out, writes) = f.run(&[]);
    let close = step_completion(&writes, "s-refused", &out);
    let reason = close["reason"].as_str().unwrap_or_default();
    assert!(reason.contains("plan_sha256"), "{reason}");
    assert!(
        writes_to(&writes, "s-approve").is_empty(),
        "no plan is rendered for a verb that cannot honour one: {writes:?}"
    );

    // No `plan_verb` at all: nothing to sign.
    f.verb_file(
        "wipe",
        json!({"about": "MUTATING — test fixture.", "hosts": ["forge"],
               "requires_approval": true, "argv": ["true", "{1}", "{2}"],
               "params": [{"name": "target", "pattern": "^[a-z-]{1,20}$"},
                          {"name": "plan_sha256", "pattern": "^[0-9a-f]{64}$"}]}),
    );
    let (out, writes) = f.run(&[]);
    let close = step_completion(&writes, "s-refused", &out);
    let reason = close["reason"].as_str().unwrap_or_default();
    assert!(reason.contains("plan_verb"), "{reason}");
}

/// The shipped approval verbs, through the real runner and the real
/// allowlist: every one's script re-renders and compares, so each
/// passes the contract (its plan verb is named, read-only, on the same
/// host, taking the write's params minus the hash).
///
/// `commission-a-disk` was the inert one until backlog b2d5b546
/// (2026-09-26): its script took no hash, and the runner closed it
/// refused before any plan ran. Now it holds the contract, so the runner
/// goes on to RENDER its plan — the real script, from the real
/// checkout — which refuses a device that does not exist in its own
/// words. That refusal is the proof the contract passed: the runner
/// reaches the plan verb only after approval_contract holds.
#[test]
fn the_shipped_approval_verbs_hold_the_contract_and_the_disk_verb_reaches_its_plan() {
    needs_tools!();
    let root = repo_root();
    let read = |n: &str| -> Value {
        serde_json::from_str(
            &std::fs::read_to_string(root.join(format!("infra/ops/verbs/{n}.json"))).unwrap(),
        )
        .unwrap()
    };
    let names = |v: &Value| -> Vec<String> {
        v["params"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["name"].as_str().unwrap().to_string())
            .collect()
    };
    for (write, plan) in [
        ("set-longhorn-drain-policy", "plan-a-longhorn-drain-policy"),
        ("reap-terminated-pods", "plan-a-pod-reap"),
        ("merge-tenant-main", "plan-a-tenant-merge"),
        ("reclaim-gcp-root", "plan-a-gcp-root-reclaim"),
        ("move-volume-replica", "plan-a-volume-replica-move"),
        ("commission-a-disk", "plan-a-disk-commission"),
        ("shutdown-node", "plan-a-node-shutdown"),
        ("node-converge", "plan-a-node-converge"),
        ("set-volume-replicas", "plan-a-volume-replica-change"),
        ("retire-volume-replica", "plan-a-volume-replica-retirement"),
        (
            "expand-instance-volume",
            "plan-an-instance-volume-expansion",
        ),
        ("retire-ops-runner", "plan-retire-ops-runner"),
    ] {
        let w = read(write);
        let p = read(plan);
        assert_eq!(w["plan_verb"], plan, "{write} names its plan verb");
        assert_eq!(
            w["approvers"],
            json!([APPROVER]),
            "{write} names who may approve it, by employee id (design 03451237 q2)"
        );
        assert_ne!(p["requires_approval"], true, "{plan} is read-only");
        assert_eq!(w["hosts"], p["hosts"], "{write} and {plan} serve one host");
        let mut wn = names(&w);
        assert_eq!(wn.pop().as_deref(), Some("plan_sha256"), "{write}");
        assert_eq!(wn, names(&p), "{plan} renders from exactly {write}'s args");
    }

    // The disk verb, through the runner with the shipped allowlist.
    let f = Fixture::new("disk-plan");
    std::fs::remove_dir_all(&f.verbs).unwrap();
    std::fs::create_dir_all(&f.verbs).unwrap();
    for e in std::fs::read_dir(root.join("infra/ops/verbs")).unwrap() {
        let p = e.unwrap().path();
        std::fs::copy(&p, f.verbs.join(p.file_name().unwrap())).unwrap();
    }
    let mut j = job("ready", approve_meta(None), json!([]), "pending");
    j["metadata"]["verb"] = json!("commission-a-disk");
    j["metadata"]["args"] = json!(["/dev/disk/by-id/nvme-TEST-0000", "/srv/data"]);
    f.packet(j);
    let (out, writes) = f.run(&[]);
    let close = step_completion(&writes, "s-refused", &out);
    let reason = close["reason"].as_str().unwrap_or_default();
    assert!(
        !reason.contains("does not take a required plan_sha256"),
        "the disk verb's script takes the hash now; the contract must pass: {reason}"
    );
    assert!(
        reason.contains("plan-a-disk-commission") && reason.contains("no such device"),
        "the runner rendered the disk verb's plan, and the real script refused a device \
         that does not exist, in its own words: {reason}"
    );
    assert!(
        writes_to(&writes, "s-approve").is_empty(),
        "a refused plan writes no plan: {writes:?}"
    );
    assert!(f.applied().is_none(), "{out}");
}

// ------------------------------------------------------------- execute

/// THE WHOLE POINT: a completed approve step carrying a fresh presence
/// stamp bound to the plan's shape runs the write — claimed `active`
/// BEFORE the argv runs, given sha256 of the signed plan as its last
/// arg, and completed `answered` with the hash it ran under on the step.
///
/// This is also the equality pin on the shape hash (§9a): the stamp is
/// bound with `boss_core::job::step_shape_hash` over a plan and a
/// procedure carrying quotes, a backslash, a tab and non-ASCII, and the
/// runner recomputes it in jq. If the two ever disagree, every real
/// approval is refused, and this case goes red first.
#[test]
fn a_fresh_presence_approval_bound_to_the_plan_runs_the_write_once() {
    needs_tools!();
    let f = Fixture::new("approved");
    f.packet(approved_job());
    let (out, writes) = f.run(&[]);
    let plan_sha = sha256_hex(PLAN.as_bytes());
    assert_eq!(
        f.applied(),
        Some(vec!["target-a".to_string(), plan_sha.clone()]),
        "the write runs with the request's args and the SIGNED plan's hash: {out}"
    );
    // THE CLAIM IS A COMPARE-AND-SET (security review, 2026-09-24). It
    // was a PUT of `{"status":"active"}`, which the server overlays on
    // whatever the step is — so two passes that both read `ready` both
    // "claimed". It goes through the claim door now, whose WHERE clause
    // admits exactly one ready→active, and it is signed as a claimant
    // unique to this pass: the door is idempotent for its holder, so a
    // second pass signing as the same account would be let through.
    let exec = writes_to(&writes, "s-execute");
    let claim = exec
        .first()
        .unwrap_or_else(|| panic!("execute was never claimed: {writes:?}\n{out}"));
    assert_eq!(claim["method"], "POST", "{claim}");
    assert!(
        claim["url"]
            .as_str()
            .unwrap()
            .ends_with("/api/jobs/aaaaaaaa-0000-4000-8000-000000000000/steps/s-execute/claim"),
        "execute is claimed through the claim door before anything else is written to it: {claim}"
    );
    let claimant = claim["user"]["id"].as_str().unwrap_or_default().to_string();
    assert!(
        claimant.starts_with("automation:ops-runner:forge:") && claimant.len() > 28,
        "the claimant names the runner, its host and this pass: {claim}"
    );
    assert!(
        !exec
            .iter()
            .any(|w| w["body"] == json!({"status": "active"})),
        "no PUT claims by overlay: {writes:?}"
    );
    // A second pass is a second claimant, so the door's holder-idempotence
    // cannot hand it the same claim.
    let (_, writes2) = f.run(&[]);
    let claimant2 = writes_to(&writes2, "s-execute")
        .first()
        .map(|w| w["user"]["id"].as_str().unwrap_or_default().to_string());
    assert_ne!(claimant2.as_deref(), Some(claimant.as_str()), "{writes2:?}");
    let before: usize = std::fs::read_to_string(f.root.join("writes-before-apply"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(
        before, 1,
        "the claim is the one write made BEFORE the argv runs: {writes:?}"
    );
    let md = execute_completion(&writes, &out);
    assert_eq!(md["disposition"], "answered", "{md}\n{out}");
    assert_eq!(md["exit_code"], "0", "{md}");
    assert_eq!(
        md["approved_plan_sha256"], plan_sha,
        "the step says which signed plan it ran under: {md}"
    );
}

/// THE MACHINE-FILED SHAPE (backlog 3df309bf). The dispatcher rule
/// `file-the-remedy-a-verb-declares-for-an-estate-finding` (handler
/// `ops.file_remedies`) files a request for a verb whose ONLY param is
/// `plan_sha256` — the only shape it will file — so it carries `args: []`,
/// an empty list, beside the keys every dispatcher filing stamps
/// (`spawned_by_rule`, `triggered_by_*`) and the `remedies` it answers. Every other case here files
/// `wipe target-a`, so the arg-count check (`args` must be an array of
/// exactly params - 1) was never run at zero, and none carried the
/// spawn's keys. Both halves, through the real runner: a ready approve
/// step gets the plan rendered for `[]`, and the approved request runs
/// the write with the signed plan's hash as its one and only arg.
#[test]
fn a_machine_filed_request_with_no_args_renders_its_plan_and_runs_on_approval() {
    needs_tools!();
    let f = Fixture::new("machine-filed-no-args");
    let plan_sh = f.root.join("plan.sh");
    let apply_sh = f.root.join("apply.sh");
    f.verb_file(
        "reclaim",
        json!({"about": "MUTATING — test fixture.", "hosts": ["forge"],
               "requires_approval": true, "plan_verb": "plan-reclaim",
               "approvers": [APPROVER],
               "argv": [apply_sh.display().to_string(), "{1}"],
               "params": [{"name": "plan_sha256", "pattern": "^[0-9a-f]{64}$"}]}),
    );
    f.verb_file(
        "plan-reclaim",
        json!({"about": "READ-ONLY: renders what reclaim would do.", "hosts": ["forge"],
               "argv": [plan_sh.display().to_string()], "params": []}),
    );
    let title = "Approve the plan: reclaim on forge";
    let machine_filed = |approve_status: &str, meta: Value, sign_offs: Value, exec: &str| {
        let mut j = job(approve_status, meta, sign_offs, exec);
        j["metadata"] = json!({
            "host": "forge", "verb": "reclaim", "args": [], "requires_approval": true,
            "remedies": "disk_tight:forge",
            "spawned_by_rule": "file-the-remedy-a-verb-declares-for-an-estate-finding",
            "triggered_by_event_id": "b7c2d05a-b23b-4a91-b537-03fc38160347",
            "triggered_by_topic": "jobs.estate.compared"
        });
        j["steps"][1]["title"] = json!(title);
        j
    };

    // 1. The approve step is ready: the plan is rendered for `[]`.
    f.packet(machine_filed(
        "ready",
        approve_meta(None),
        json!([]),
        "pending",
    ));
    let (out, writes) = f.run(&[]);
    assert!(
        f.applied().is_none(),
        "rendering a plan runs no write: {out}"
    );
    let patch = writes_to(&writes, "s-approve")
        .into_iter()
        .find(|w| w["method"] == "PATCH")
        .unwrap_or_else(|| panic!("no plan was written onto the approve step: {writes:?}\n{out}"));
    assert_eq!(patch["body"]["plan"], PLAN, "{patch}");
    assert_eq!(patch["body"]["verb"], "reclaim", "{patch}");
    assert_eq!(
        patch["body"]["args"],
        json!([]),
        "the plan is signed with the request's own empty args: {patch}"
    );
    assert!(
        writes_to(&writes, "s-refused").is_empty(),
        "an empty args list is not refused at the plan: {writes:?}\n{out}"
    );

    // 2. Approved: the write runs with the signed plan's hash alone.
    let mut meta = approve_meta(Some(PLAN));
    meta["verb"] = json!("reclaim");
    meta["args"] = json!([]);
    let shape = boss_core::job::step_shape_hash(title, &meta);
    f.packet(machine_filed(
        "completed",
        meta,
        json!([stamp("platform-admin", "presence", &shape, 60)]),
        "ready",
    ));
    let (out, writes) = f.run(&[]);
    let plan_sha = sha256_hex(PLAN.as_bytes());
    assert_eq!(
        f.applied(),
        Some(vec![plan_sha.clone()]),
        "args [] passes the arg-count check, and the write gets the SIGNED plan's hash as its only arg: {out}"
    );
    let md = execute_completion(&writes, &out);
    assert_eq!(md["disposition"], "answered", "{md}\n{out}");
    assert_eq!(md["approved_plan_sha256"], plan_sha, "{md}");

    // The control: the check is live at zero. The same approved request
    // with `args` ABSENT — what a rule whose list arg resolved to nothing
    // would file — is refused by name and runs nothing.
    let mut meta = approve_meta(Some(PLAN));
    meta["verb"] = json!("reclaim");
    meta["args"] = json!([]);
    let shape = boss_core::job::step_shape_hash(title, &meta);
    let mut absent = machine_filed(
        "completed",
        meta,
        json!([stamp("platform-admin", "presence", &shape, 60)]),
        "ready",
    );
    absent["metadata"].as_object_mut().unwrap().remove("args");
    f.packet(absent);
    let (out, writes) = f.run(&[]);
    // Named as missing, never as a count: "carries 0 arg(s) where it
    // takes 0" was the old text, a refusal that named no defect.
    assert_refused(
        &f,
        &out,
        &writes,
        &[
            "reclaim",
            "carries no args list (args is null)",
            "an empty list",
        ],
    );
    assert!(
        !out.contains("where it takes 0"),
        "a missing args list is not reported as a count: {out}"
    );

    // And a value that is there but not a list is named by its type.
    let mut meta = approve_meta(Some(PLAN));
    meta["verb"] = json!("reclaim");
    meta["args"] = json!([]);
    let shape = boss_core::job::step_shape_hash(title, &meta);
    let mut stringly = machine_filed(
        "completed",
        meta,
        json!([stamp("platform-admin", "presence", &shape, 60)]),
        "ready",
    );
    stringly["metadata"]["args"] = json!("");
    f.packet(stringly);
    let (out, writes) = f.run(&[]);
    assert_refused(
        &f,
        &out,
        &writes,
        &["carries no args list (args is string)"],
    );
}

/// THE d5efbb3c SHAPE (2026-09-22): the first presence-assured step in
/// the system completed with `sign_offs: []` and no ceremony, and the
/// host then ran the verb. A completed step is not an approval.
#[test]
fn a_completed_approve_step_with_no_stamp_is_refused() {
    needs_tools!();
    let f = Fixture::new("forged-no-stamp");
    f.packet(job(
        "completed",
        approve_meta(Some(PLAN)),
        json!([]),
        "ready",
    ));
    let (out, writes) = f.run(&[]);
    assert_refused(&f, &out, &writes, &["wipe", "no platform-admin sign-off"]);

    // And the packet exactly as it was: the step required no sign-off at
    // all, so there was nothing a stamp could have been checked against.
    let f = Fixture::new("forged-d5efbb3c");
    let mut j = job("completed", approve_meta(Some(PLAN)), json!([]), "ready");
    j["steps"][1]["sign_offs_required"] = json!([]);
    f.packet(j);
    let (out, writes) = f.run(&[]);
    assert_refused(&f, &out, &writes, &["requires no sign-off"]);
}

/// A REJECTION IS NOT AN APPROVAL (adversarial re-review, 2026-09-25).
/// Reject, on either surface, runs the SAME ceremony as Approve — the
/// decision saved, a presence stamp bound to the shape that carries it,
/// the step completed — so a rejected plan arrives as a completed approve
/// step with a valid, fresh, bound, named-approver presence stamp, which
/// is every check the runner made. Reproduced on the car's own runner:
/// the write ran. The decision is inside the signed shape, so reading it
/// is reading what the approver signed; only the exact string `approved`
/// is one, and anything else — another decision, none, or a value that
/// is not a string — runs nothing and is refused naming the decision.
#[test]
fn a_rejected_approval_runs_nothing() {
    needs_tools!();
    let decisions = [
        ("rejected", Some(json!("rejected"))),
        ("changes-requested", Some(json!("changes-requested"))),
        ("pending", Some(json!("pending"))),
        ("approved-with-space", Some(json!("approved "))),
        ("absent", None),
        ("bool", Some(json!(true))),
        ("list", Some(json!(["approved"]))),
        ("object", Some(json!({"decision": "approved"}))),
        ("null", Some(Value::Null)),
    ];
    for (name, decision) in decisions {
        let f = Fixture::new(&format!("decision-{name}"));
        let mut meta = approve_meta(Some(PLAN));
        match &decision {
            Some(d) => meta["decision"] = d.clone(),
            None => {
                meta.as_object_mut().unwrap().remove("decision");
            }
        }
        // The stamp is bound to the step AS IT STANDS, decision and all:
        // every other check passes, so only the decision can refuse it.
        let shape = shape_of(&meta);
        f.packet(job(
            "completed",
            meta,
            json!([stamp("platform-admin", "presence", &shape, 60)]),
            "ready",
        ));
        let (out, writes) = f.run(&[]);
        let shown = match &decision {
            Some(d) => d.to_string(),
            None => "none".to_string(),
        };
        assert_refused(&f, &out, &writes, &["wipe", "decision", &shown]);
    }
}

/// A WITHDRAWN APPROVAL DOES NOT COME BACK (backlog c085256d, design
/// 87329a13, option C). The approver signs X (S1), withdraws — Reject
/// saves `rejected` and stamps it (S2 on Y); Request changes saves its
/// decision and stamps nothing — and anyone who can write step metadata
/// PATCHes X back through the merge door. Byte for byte, S1 matches
/// the step again, and before this line the write RAN on it, inside
/// S1's ten-minute window, once a passkey holder completed the step.
///
/// The runner judges the record itself, independently of the server
/// (design 17835005), on two rules. A stamp the server VOIDED — the
/// edit that took its shape off the step marks it `voided_at` — is not
/// an approval, even on the shape it signed; that alone covers Request
/// changes, which leaves no second stamp to compare with. And a named
/// approver's NEWEST stamp for the role must be on the shape being run,
/// which covers a record written before the server voided anything.
#[test]
fn an_approval_withdrawn_and_restored_runs_nothing() {
    needs_tools!();
    let meta = approve_meta(Some(PLAN));
    let x = shape_of(&meta);
    let mut rejected = approve_meta(Some(PLAN));
    rejected["decision"] = json!("rejected");
    let y = shape_of(&rejected);
    let voided = |mut s: Value, secs: i64| {
        s["voided_at"] = json!(iso_ago(secs));
        s["voided_by_event"] = json!("0e1d2c3b-0000-4000-8000-00000000c085");
        s
    };

    // Request changes, as the server records it now: S1 on X, voided by
    // the edit that took X off the step, and X restored after.
    let f = Fixture::new("withdrawn-voided");
    f.packet(job(
        "completed",
        meta.clone(),
        json!([voided(stamp("platform-admin", "presence", &x, 60), 40)]),
        "ready",
    ));
    let (out, writes) = f.run(&[]);
    assert_refused(&f, &out, &writes, &["platform-admin", "voided"]);

    // Reject, on a record that carries no void (written before the
    // server voided anything): S1 on X, then the signed rejection S2 on
    // Y, and X restored. The approver's newest word is the rejection.
    let f = Fixture::new("withdrawn-by-a-newer-stamp");
    f.packet(job(
        "completed",
        meta.clone(),
        json!([
            stamp("platform-admin", "presence", &x, 90),
            stamp("platform-admin", "presence", &y, 30)
        ]),
        "ready",
    ));
    let (out, writes) = f.run(&[]);
    assert_refused(&f, &out, &writes, &[APPROVER, "newest", &y]);

    // The control: after the restore the approver signs X AGAIN, so the
    // newest stamp is a live one on the shape being run — and it runs.
    let f = Fixture::new("withdrawn-then-signed-again");
    f.packet(job(
        "completed",
        meta,
        json!([
            voided(stamp("platform-admin", "presence", &x, 90), 50),
            voided(stamp("platform-admin", "presence", &y, 50), 30),
            stamp("platform-admin", "presence", &x, 20)
        ]),
        "ready",
    ));
    let (out, _) = f.run(&[]);
    assert_eq!(
        f.applied(),
        Some(vec!["target-a".to_string(), sha256_hex(PLAN.as_bytes())]),
        "a fresh signature on the restored plan is an approval: {out}"
    );
}

/// A session-assured stamp is someone logged in; it is not a passkey.
#[test]
fn a_session_assured_stamp_is_refused() {
    needs_tools!();
    let f = Fixture::new("forged-session");
    let meta = approve_meta(Some(PLAN));
    let shape = shape_of(&meta);
    f.packet(job(
        "completed",
        meta,
        json!([stamp("platform-admin", "session", &shape, 60)]),
        "ready",
    ));
    let (out, writes) = f.run(&[]);
    assert_refused(&f, &out, &writes, &["session", "presence"]);

    // The assurance is judged on its own: a session stamp that somehow
    // carries a nonce is still a session stamp.
    let f = Fixture::new("forged-session-nonce");
    let meta = approve_meta(Some(PLAN));
    let shape = shape_of(&meta);
    let mut s = stamp("platform-admin", "session", &shape, 60);
    s["presence_nonce"] = json!("nonce-1");
    f.packet(job("completed", meta, json!([s]), "ready"));
    let (out, writes) = f.run(&[]);
    assert_refused(&f, &out, &writes, &["session", "presence"]);
}

/// A declaration the runner cannot read as a plain `false` is read as
/// requiring an approval: a typo in a reviewed file (`"true"`, a
/// string) must not quietly turn the gate off.
#[test]
fn a_requires_approval_that_is_not_a_boolean_still_requires_one() {
    needs_tools!();
    let f = Fixture::new("non-boolean");
    let apply: Value =
        serde_json::from_str(&std::fs::read_to_string(f.verbs.join("wipe.json")).unwrap()).unwrap();
    let mut spec = apply.clone();
    spec["requires_approval"] = json!("true");
    f.verb_file("wipe", spec);
    let mut j = job("pending", approve_meta(None), json!([]), "ready");
    j["metadata"] = json!({"host": "forge", "verb": "wipe", "args": ["target-a"]});
    f.packet(j);
    let (out, writes) = f.run(&[]);
    assert_refused(&f, &out, &writes, &["requires a passkey approval"]);
}

/// A presence stamp for another role does not stand in for the role the
/// step requires.
#[test]
fn a_presence_stamp_for_another_role_is_refused() {
    needs_tools!();
    let f = Fixture::new("forged-role");
    let meta = approve_meta(Some(PLAN));
    let shape = shape_of(&meta);
    f.packet(job(
        "completed",
        meta,
        json!([stamp("finance-admin", "presence", &shape, 60)]),
        "ready",
    ));
    let (out, writes) = f.run(&[]);
    assert_refused(&f, &out, &writes, &["platform-admin"]);
}

/// MISMATCHED HASH: the plan on the step is not the plan that was
/// signed. The stamp is bound to the shape of a DIFFERENT plan, so the
/// passkey never saw these bytes.
#[test]
fn a_stamp_bound_to_a_different_plan_is_refused() {
    needs_tools!();
    let f = Fixture::new("mismatched-shape");
    let signed = approve_meta(Some("PLAN wipe target-b\n"));
    let shape = shape_of(&signed);
    f.packet(job(
        "completed",
        approve_meta(Some(PLAN)),
        json!([stamp("platform-admin", "presence", &shape, 60)]),
        "ready",
    ));
    let (out, writes) = f.run(&[]);
    assert_refused(&f, &out, &writes, &["not the plan that was signed", &shape]);
}

/// TIME-BOXED (q4): an approval older than the window is refused, and
/// says when it was signed and what the window is.
#[test]
fn an_approval_older_than_its_window_is_refused() {
    needs_tools!();
    let f = Fixture::new("expired");
    let meta = approve_meta(Some(PLAN));
    let shape = shape_of(&meta);
    f.packet(job(
        "completed",
        meta,
        json!([stamp("platform-admin", "presence", &shape, 11 * 60)]),
        "ready",
    ));
    let (out, writes) = f.run(&[]);
    assert_refused(&f, &out, &writes, &["expires after 600s"]);
}

/// A stamp from this host's future is a clock nobody can reconcile, and
/// an approval that cannot be dated cannot be time-boxed.
#[test]
fn an_approval_stamped_in_the_future_is_refused() {
    needs_tools!();
    let f = Fixture::new("future");
    let meta = approve_meta(Some(PLAN));
    let shape = shape_of(&meta);
    f.packet(job(
        "completed",
        meta,
        json!([stamp("platform-admin", "presence", &shape, -30 * 60)]),
        "ready",
    ));
    let (out, writes) = f.run(&[]);
    assert_refused(&f, &out, &writes, &["future"]);
}

/// CLAIMED, THEN SILENCE: an execute step already `active` was claimed by
/// an earlier pass that never answered — it died, or its host did.
/// Single-use means that claim was the one use, so this pass never runs
/// the write again. And it does not know whether the write ran, so it
/// must not SAY it did not: closing the request `refused` told a reader
/// "nothing ran" about a write that may have (security review,
/// 2026-09-24). The request is held open, active, looking troubled, with
/// "claimed, outcome unknown" on it — once, not every minute.
#[test]
fn an_execute_claimed_by_a_pass_that_never_answered_records_claimed_outcome_unknown() {
    needs_tools!();
    let f = Fixture::new("claimed-unknown");
    let mut j = approved_job();
    j["steps"][2]["status"] = json!("active");
    j["steps"][2]["assignee_id"] = json!("automation:ops-runner:forge:20260924T101500Z-4242");
    f.packet(j.clone());
    let (out, writes) = f.run(&[]);
    assert!(
        f.applied().is_none(),
        "a claimed execute is never run again: {out}"
    );
    assert!(
        !writes_to(&writes, "s-execute")
            .iter()
            .any(|w| w["body"]["status"] == "completed"),
        "the step is not answered — nobody knows the answer: {writes:?}\n{out}"
    );
    assert!(
        writes_to(&writes, "s-refused").is_empty(),
        "and the request is NOT closed refused, which would say nothing ran: {writes:?}"
    );
    let note = writes
        .iter()
        .find(|w| {
            w["method"] == "PATCH"
                && w["url"].as_str().is_some_and(|u| {
                    u.ends_with("/api/jobs/aaaaaaaa-0000-4000-8000-000000000000/metadata")
                })
        })
        .unwrap_or_else(|| panic!("nothing was recorded on the request: {writes:?}\n{out}"));
    let rec = &note["body"]["execute_outcome_unknown"];
    assert_eq!(rec["state"], "claimed, outcome unknown", "{note}");
    assert_eq!(
        rec["claimed_by"], "automation:ops-runner:forge:20260924T101500Z-4242",
        "it names the pass that claimed: {note}"
    );
    assert!(
        rec["reason"].as_str().unwrap_or_default().contains("wipe"),
        "{note}"
    );
    assert!(out.contains("claimed, outcome unknown"), "{out}");
    assert!(out.contains("held=1"), "{out}");

    // Recorded once: a second pass over the same record writes nothing.
    j["metadata"]["execute_outcome_unknown"] = rec.clone();
    f.packet(j);
    let (out, writes) = f.run(&[]);
    assert!(
        writes.is_empty(),
        "the note is written once: {writes:?}\n{out}"
    );
    assert!(f.applied().is_none(), "{out}");
    assert!(out.contains("held=1"), "{out}");
}

/// CLAIMED, ANSWERED, NOT YET COMPLETED (the adversarial review of car
/// 24eb9471). The runner completes execute in two writes — its keys
/// through the merge door, then the status alone (backlog 2aa2b19e) — so
/// a pass that died, or whose status PUT never arrived, between the two
/// leaves execute ACTIVE under its own claim carrying its own answer.
/// That is not "claimed, outcome unknown": the answer is on the step,
/// signed by the same pass (`claimed_as` = the holder, `runner_host` =
/// this host). The next pass finishes it with the status alone — never
/// runs the write again, never records "no answer was ever recorded"
/// beside an answer. An answer another claimant signed is still unknown.
#[test]
fn an_execute_answered_by_its_own_claim_but_left_active_is_finished_with_its_status() {
    needs_tools!();
    let f = Fixture::new("claimed-answered");
    let holder = "automation:ops-runner:forge:20260924T101500Z-4242";
    let mut j = approved_job();
    j["steps"][2]["status"] = json!("active");
    j["steps"][2]["assignee_id"] = json!(holder);
    j["steps"][2]["metadata"]["disposition"] = json!("answered");
    j["steps"][2]["metadata"]["exit_code"] = json!("0");
    j["steps"][2]["metadata"]["output"] = json!("applied\n");
    j["steps"][2]["metadata"]["runner_host"] = json!("forge");
    j["steps"][2]["metadata"]["claimed_as"] = json!(holder);
    f.packet(j.clone());
    let (out, writes) = f.run(&[]);
    assert!(f.applied().is_none(), "the write is never run again: {out}");
    let exec = writes_to(&writes, "s-execute");
    assert_eq!(
        exec.iter().map(|w| &w["body"]).collect::<Vec<_>>(),
        vec![&json!({"status": "completed"})],
        "exactly one write to execute, the status alone: {writes:?}\n{out}"
    );
    assert!(
        !writes
            .iter()
            .any(|w| w["body"].get("execute_outcome_unknown").is_some()),
        "no 'outcome unknown' beside an answer: {writes:?}"
    );
    assert!(writes_to(&writes, "s-refused").is_empty(), "{writes:?}");
    assert!(
        out.contains("answered=1") && out.contains("failed=0"),
        "{out}"
    );

    // An answer signed by ANOTHER claimant is not this pass's to finish.
    j["steps"][2]["metadata"]["claimed_as"] =
        json!("automation:ops-runner:forge:20260924T090000Z-1111");
    f.packet(j);
    let (out, writes) = f.run(&[]);
    assert!(
        !writes_to(&writes, "s-execute")
            .iter()
            .any(|w| w["body"]["status"] == "completed"),
        "{writes:?}\n{out}"
    );
    assert!(out.contains("claimed, outcome unknown"), "{out}");
}

/// A METADATA FLAG IS NEVER AN APPROVAL (q2). A request filed without
/// `requires_approval` never makes its approve step ready, so execute is
/// ready at once — and a job-level `approved: true` beside it proves
/// nothing. The runner reads the approval requirement off the VERB and
/// the approval off the SoR's sign-off record, never off the packet.
#[test]
fn a_request_carrying_a_flag_instead_of_a_signature_is_refused() {
    needs_tools!();
    let f = Fixture::new("flag");
    let mut j = job("pending", approve_meta(None), json!([]), "ready");
    j["metadata"] =
        json!({"host": "forge", "verb": "wipe", "args": ["target-a"], "approved": true});
    f.packet(j);
    let (out, writes) = f.run(&[]);
    assert_refused(&f, &out, &writes, &["pending", "requires_approval"]);
}

/// A filer cannot supply the hash: the runner appends the SIGNED plan's
/// hash itself, and an arg in that position is refused rather than
/// shifted or trusted.
#[test]
fn a_request_that_names_its_own_plan_hash_is_refused() {
    needs_tools!();
    let f = Fixture::new("filer-hash");
    let mut j = approved_job();
    j["metadata"]["args"] = json!(["target-a", "0".repeat(64)]);
    f.packet(j);
    let (out, writes) = f.run(&[]);
    assert_refused(&f, &out, &writes, &["plan_sha256"]);
}

/// A claim the server refuses runs nothing: the claim is what makes the
/// approval single-use, so without it there is no run.
#[test]
fn a_refused_claim_runs_nothing() {
    needs_tools!();
    let f = Fixture::new("claim-refused");
    f.packet(approved_job());
    let (out, writes) = f.run(&[("STUB_REFUSE_CLAIM", "1")]);
    assert!(
        f.applied().is_none(),
        "the write ran without its claim: {out}"
    );
    assert!(
        !writes_to(&writes, "s-execute")
            .iter()
            .any(|w| w["body"]["status"] == "completed"),
        "an unclaimed approval is left for the next pass, not answered: {writes:?}"
    );
    assert!(out.contains("failed=1"), "and the unit goes red: {out}");
}

// ------------------------------------------- security review, 2026-09-24

/// WHO MAY APPROVE IS A NAMED LIST OF EMPLOYEES, not a role (design
/// 03451237 q2, David 2026-09-22: a role is registry data, so a role
/// gate makes the approval depend on whoever can write a policy row).
/// A presence stamp for the right role, bound to the right shape, fresh —
/// by someone the verb file does not name — approves nothing.
#[test]
fn a_presence_stamp_by_someone_not_named_as_an_approver_is_refused() {
    needs_tools!();
    let f = Fixture::new("not-an-approver");
    let meta = approve_meta(Some(PLAN));
    let shape = shape_of(&meta);
    let mut s = stamp("platform-admin", "presence", &shape, 60);
    s["authority_id"] = json!("emp-mallory");
    f.packet(job("completed", meta, json!([s]), "ready"));
    let (out, writes) = f.run(&[]);
    assert_refused(&f, &out, &writes, &["emp-mallory", "approvers", APPROVER]);
}

/// A verb that names no approver has nobody whose passkey could carry
/// it, so it is refused before any plan is rendered — the same place a
/// verb with no plan verb is.
#[test]
fn an_approval_verb_that_names_no_approver_is_refused_before_any_plan() {
    needs_tools!();
    let f = Fixture::new("no-approvers");
    let mut spec: Value =
        serde_json::from_str(&std::fs::read_to_string(f.verbs.join("wipe.json")).unwrap()).unwrap();
    for bad in [json!(null), json!([]), json!("emp-david"), json!([""])] {
        if bad.is_null() {
            spec.as_object_mut().unwrap().remove("approvers");
        } else {
            spec["approvers"] = bad.clone();
        }
        f.verb_file("wipe", spec.clone());
        f.packet(job("ready", approve_meta(None), json!([]), "pending"));
        let (out, writes) = f.run(&[]);
        let close = step_completion(&writes, "s-refused", &out);
        let reason = close["reason"].as_str().unwrap_or_default();
        assert!(reason.contains("approvers"), "{bad}: {reason}");
        assert!(
            writes_to(&writes, "s-approve").is_empty(),
            "{bad}: no plan is rendered for a verb nobody may approve: {writes:?}"
        );
    }
}

/// THE SHAPE HASH ENCODES ITS KEYS (security review, 2026-09-24). Keys
/// were written raw — `key:value,` — so `{"zz": 1, "zzz": 2}` and
/// `{"zz:1,zzz": 2}` both canonicalised to `{…zz:1,zzz:2,}`: a stamp over
/// one step shape was a stamp over another. Both definitions JSON-encode
/// a key now, and this case holds them to it end to end: a stamp the
/// SERVER bound to one of the pair must not verify on a step carrying
/// the other.
#[test]
fn a_stamp_does_not_carry_over_to_a_step_whose_keys_collide_with_its_shape() {
    needs_tools!();
    let f = Fixture::new("key-collision");
    let mut signed = approve_meta(Some(PLAN));
    signed["zz"] = json!(1);
    signed["zzz"] = json!(2);
    let mut carried = approve_meta(Some(PLAN));
    carried["zz:1,zzz"] = json!(2);
    let shape = shape_of(&signed);
    f.packet(job(
        "completed",
        carried,
        json!([stamp("platform-admin", "presence", &shape, 60)]),
        "ready",
    ));
    let (out, writes) = f.run(&[]);
    assert_refused(&f, &out, &writes, &["not the plan that was signed"]);
}

/// The other half of the §9a pin: over keys that NEED encoding — a
/// quote, a tab, a colon, non-ASCII — the runner's jq and the server's
/// Rust still agree, so an honest approval is not refused.
#[test]
fn the_runners_shape_hash_agrees_with_the_servers_over_keys_that_need_encoding() {
    needs_tools!();
    let f = Fixture::new("key-encoding");
    let mut meta = approve_meta(Some(PLAN));
    meta["a \"quoted\"\tkey: é,"] = json!({"inner \\ key": [1, "two"]});
    let shape = shape_of(&meta);
    f.packet(job(
        "completed",
        meta,
        json!([stamp("platform-admin", "presence", &shape, 60)]),
        "ready",
    ));
    let (out, _) = f.run(&[]);
    assert_eq!(
        f.applied(),
        Some(vec!["target-a".to_string(), sha256_hex(PLAN.as_bytes())]),
        "an honest approval over encoded keys runs: {out}"
    );
}

/// THE REQUEST IS SIGNED, NOT ONLY ITS PLAN. The runner wrote the verb,
/// host and args onto the approve step when it rendered, so they are in
/// the signed shape; the request's own metadata stays writable. A
/// request whose args (or verb) moved after the signature is refused
/// before any argv is built — naming both.
#[test]
fn a_request_that_moved_after_its_signature_is_refused() {
    needs_tools!();
    let f = Fixture::new("moved-args");
    let mut j = approved_job();
    j["metadata"]["args"] = json!(["target-b"]);
    f.packet(j);
    let (out, writes) = f.run(&[]);
    assert_refused(&f, &out, &writes, &["target-b", "target-a", "signed"]);

    // A plan rendered before this car carries no verb/host/args at all:
    // there is nothing signed to compare the request against.
    let f = Fixture::new("unsigned-request");
    let mut meta = approve_meta(Some(PLAN));
    for k in ["verb", "host", "args"] {
        meta.as_object_mut().unwrap().remove(k);
    }
    let shape = shape_of(&meta);
    f.packet(job(
        "completed",
        meta,
        json!([stamp("platform-admin", "presence", &shape, 60)]),
        "ready",
    ));
    let (out, writes) = f.run(&[]);
    assert_refused(&f, &out, &writes, &["signed"]);
}

/// A PLAN THE RUNNER DID NOT WRITE IS REFUSED. Two checks. The step must
/// carry the hash of the bytes the runner rendered, and the plan on it
/// must hash to that. And — the one a forger cannot satisfy by writing
/// both fields — the runner renders the plan AGAIN on the host, from the
/// signed args, and the signed plan must be those bytes exactly. A plan
/// typed onto the step by anyone else, however consistent, is not.
#[test]
fn a_plan_the_runner_did_not_render_is_refused() {
    needs_tools!();
    // The rendered hash is missing, or names other bytes.
    for rendered in [json!(null), json!("0".repeat(64))] {
        let f = Fixture::new("not-rendered-hash");
        let mut meta = approve_meta(Some(PLAN));
        if rendered.is_null() {
            meta.as_object_mut().unwrap().remove("rendered_plan_sha256");
        } else {
            meta["rendered_plan_sha256"] = rendered.clone();
        }
        let shape = shape_of(&meta);
        f.packet(job(
            "completed",
            meta,
            json!([stamp("platform-admin", "presence", &shape, 60)]),
            "ready",
        ));
        let (out, writes) = f.run(&[]);
        assert_refused(&f, &out, &writes, &["rendered_plan_sha256"]);
    }

    // A forged plan, internally consistent and properly signed: the
    // runner's own re-render says otherwise.
    let f = Fixture::new("forged-plan");
    let forged = "PLAN wipe nothing at all\n";
    let meta = approve_meta(Some(forged));
    let shape = shape_of(&meta);
    f.packet(job(
        "completed",
        meta,
        json!([stamp("platform-admin", "presence", &shape, 60)]),
        "ready",
    ));
    let (out, writes) = f.run(&[]);
    assert_refused(
        &f,
        &out,
        &writes,
        &[
            "plan-wipe",
            &sha256_hex(forged.as_bytes()),
            &sha256_hex(PLAN.as_bytes()),
        ],
    );
}

/// RE-READ IMMEDIATELY BEFORE THE CLAIM. The runner judged the approval
/// on the list it read at the top of the pass; by the time it reaches
/// this request another pass may have claimed it, or the approval may
/// be gone. It reads the one job again and judges it again, and a record
/// that moved is left for the next pass — no claim, no write, nothing
/// run, and nothing written that a stale read decided.
#[test]
fn an_approval_that_moved_before_the_claim_is_not_claimed() {
    needs_tools!();
    // Another pass claimed execute in between.
    let f = Fixture::new("reread-claimed");
    f.packet(approved_job());
    let mut moved = approved_job();
    moved["steps"][2]["status"] = json!("active");
    f.reread(moved);
    let (out, writes) = f.run(&[]);
    assert!(f.applied().is_none(), "{out}");
    assert!(
        writes.is_empty(),
        "a moved record is written nothing: {writes:?}\n{out}"
    );
    assert!(out.contains("moved"), "{out}");

    // The stamp was withdrawn (the step reopened) in between.
    let f = Fixture::new("reread-unsigned");
    f.packet(approved_job());
    let mut moved = approved_job();
    moved["steps"][1]["status"] = json!("ready");
    moved["steps"][1]["sign_offs"] = json!([]);
    f.reread(moved);
    let (out, writes) = f.run(&[]);
    assert!(f.applied().is_none(), "{out}");
    assert!(writes.is_empty(), "{writes:?}\n{out}");

    // Someone was nominated to execute in between: the claim door would
    // refuse this pass, so it is not attempted on a stale read.
    let f = Fixture::new("reread-nominated");
    f.packet(approved_job());
    let mut moved = approved_job();
    moved["steps"][2]["assignee_id"] = json!(NOMINEE);
    f.reread(moved);
    let (out, writes) = f.run(&[]);
    assert!(f.applied().is_none(), "{out}");
    assert!(writes.is_empty(), "{writes:?}\n{out}");
    assert!(out.contains(NOMINEE), "the move names the holder: {out}");
}

// --------------------------------------------- security re-review, 2026-09-25

/// THE BLOCKER, AS THE LIVE SYSTEM HAD IT: every ops-request execute was
/// nominated to the agent executor ~50ms after it went ready, and the
/// claim door admits a ready step only to an unheld row or its holder.
/// The fixture's execute is now dispatched by the tree's own protocol
/// (unheld, because it declares `claimable`), and this is the other
/// arm — a request whose execute someone holds, as every v2 request's
/// did. Its claim could never be taken, so the runner does not try: it
/// refuses the request by name, once, instead of failing a claim every
/// minute until the approval expires. Nothing runs.
#[test]
fn an_approved_execute_someone_else_was_assigned_is_refused_by_name() {
    needs_tools!();
    let f = Fixture::new("nominated");
    let mut j = approved_job();
    j["steps"][2]["assignee_id"] = json!(NOMINEE);
    f.packet(j);
    let (out, writes) = f.run(&[]);
    assert_refused(&f, &out, &writes, &[NOMINEE, "claim", "unheld"]);
    assert!(
        !writes
            .iter()
            .any(|w| w["url"].as_str().is_some_and(|u| u.ends_with("/claim"))),
        "a claim the door must refuse is not attempted: {writes:?}"
    );
}

/// THE STUB'S CLAIM DOOR IS THE SERVER'S RULE. Called directly, the way
/// the runner calls it: a ready step is admitted to an unheld row or its
/// holder, an active one only to its holder, and anything else is 409
/// naming the holder — `claim_step_at`'s WHERE clause. The always-200
/// door this replaced is why the blocker passed green.
#[test]
fn the_stubbed_claim_door_admits_exactly_what_the_servers_does() {
    needs_tools!();
    let f = Fixture::new("claim-door");
    let claim = |status: &str, holder: Value, claimant: &str| -> (String, String) {
        let mut j = approved_job();
        j["steps"][2]["status"] = json!(status);
        j["steps"][2]["assignee_id"] = holder;
        f.packet(j);
        std::fs::write(f.root.join("writes.jsonl"), "").unwrap();
        let body = f.root.join("claim-body");
        let out = Command::new(f.root.join("bin/curl"))
            .args([
                "-sS",
                "-o",
                body.to_str().unwrap(),
                "-w",
                "%{http_code}",
                "-X",
                "POST",
                "-H",
                &format!("x-boss-user: {}", json!({"id": claimant})),
                "http://sor.invalid/api/jobs/aaaaaaaa-0000-4000-8000-000000000000/steps/s-execute/claim",
            ])
            .env("STUB_DIR", &f.root)
            .output()
            .expect("the stub runs");
        (
            String::from_utf8_lossy(&out.stdout).to_string(),
            std::fs::read_to_string(&body).unwrap_or_default(),
        )
    };
    let me = "automation:ops-runner:forge:20260925T010203Z-77";
    assert_eq!(claim("ready", Value::Null, me).0, "200");
    assert_eq!(claim("ready", json!(me), me).0, "200");
    assert_eq!(claim("active", json!(me), me).0, "200");
    let (code, body) = claim("ready", json!(NOMINEE), me);
    assert_eq!(code, "409", "{body}");
    assert!(
        body.contains(NOMINEE),
        "the refusal names the holder: {body}"
    );
    assert_eq!(
        claim("active", json!("automation:ops-runner:forge:other"), me).0,
        "409"
    );
    assert_eq!(claim("active", Value::Null, me).0, "409");
    assert_eq!(claim("completed", Value::Null, me).0, "409");
}

/// CLAIMED, OUTCOME UNKNOWN IS A RUNNER PASS'S RECORD ONLY (re-review of
/// 2026-09-25). An `active` execute held by a pass of this runner may
/// have run its write, so it is recorded as unknown and never run again.
/// One held by anyone else was not taken by this runner's claim — the
/// only way this runner runs a write — so nothing ran here, and calling
/// it "outcome unknown" would say a write might have happened that could
/// not have. It is refused by name.
#[test]
fn an_active_execute_held_by_anyone_but_a_runner_pass_is_refused_by_name() {
    needs_tools!();
    for holder in [
        json!(NOMINEE),
        json!("emp-david"),
        // Runner-shaped for ANOTHER host, or not a pass id at all.
        json!("automation:ops-runner:boss-gcp:20260924T101500Z-4242"),
        json!("automation:ops-runner:forge:whenever"),
        json!(null),
    ] {
        let f = Fixture::new("active-other");
        let mut j = approved_job();
        j["steps"][2]["status"] = json!("active");
        j["steps"][2]["assignee_id"] = holder.clone();
        f.packet(j);
        let (out, writes) = f.run(&[]);
        let name = holder.as_str().unwrap_or("no recorded claimant");
        assert_refused(
            &f,
            &out,
            &writes,
            &[name, "not a pass of this runner", "unknown"],
        );
        // AN UNRECOGNISED HOLDER IS NOT PROOF NOTHING RAN (adversarial
        // re-review, 2026-09-25): a pass of this runner signed under an
        // earlier BOSS_OPS_ACTOR, or a runner on another host, is exactly
        // such a holder. The refusal says what it cannot know.
        let reason = execute_completion(&writes, &out)["reason"]
            .as_str()
            .unwrap_or_default()
            .to_lowercase();
        assert!(
            !reason.contains("nothing ran"),
            "{holder}: the runner cannot know nothing ran under a claim it does not recognise: {reason}"
        );
        assert!(
            !writes
                .iter()
                .any(|w| !w["body"]["execute_outcome_unknown"].is_null()),
            "{holder}: outcome unknown is recorded only for a runner pass: {writes:?}"
        );
    }
}
