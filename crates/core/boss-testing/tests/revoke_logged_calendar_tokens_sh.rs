//! `revoke-logged-calendar-tokens.sh` — the logged calendar tokens on one
//! declared instance are counted and revoked through a rendered plan, from
//! inside that instance, never through a door the dev pod holds
//! (backlog 77dc1579; design d33dfbe5, decided by David 2026-09-26).
//!
//! WHY. Car 4aaff4dc (train #697) made a calendar token rest as its hash
//! and left one act owed on every instance: revoke the feeds whose token
//! the audit log still holds in the clear. The system of record measured
//! 0. The playground (namespace boss-playground) is where anonymous
//! guests read the audit log, so it is the instance the leak was readable
//! on, and the dev pod's doors reach only the system of record. The
//! design chose an ops verb PAIR over an `--instance` on boss-api: one
//! named act on a public instance, not a standing write path to it.
//!
//! THE FIXTURE. A stub `kubectl` on `BOSS_KUBECTL` models ONE call, an
//! exec into `deploy/boss -c boss` of the instance's namespace, and runs
//! the program the script hands it locally — with the tree's own
//! `infra/lib/secret-header.sh` standing for the image's copy
//! (`BOSS_LOGGED_RAW_POD_LIB`), a fixture token mount standing for the
//! container's `/etc/boss/machine-token`, and a stub `curl` standing for
//! the jobs API on loopback. So what is tested is the real in-pod
//! program: which route it calls, as whom, and that the machine token
//! rides in a header FILE and in no argv. The instance bound is read off
//! the REAL infra/cluster/instances.toml. Nothing here touches a
//! cluster or any instance.

use boss_testing::{repo_root, scratch_dir, write_exec};
use std::path::{Path, PathBuf};
use std::process::Command;

const TOKEN: &str = "fixture-machine-token-7f3a9c";

fn script() -> PathBuf {
    repo_root().join("infra/forge/revoke-logged-calendar-tokens.sh")
}

/// The one kubectl call the script may make: `-n <ns> exec deploy/boss
/// -c boss -- <program…>`. Anything else is refused, so a second kind of
/// call (a get, a port-forward, a secret read) fails here.
const KUBECTL: &str = r##"#!/usr/bin/env bash
S="${KSTATE:?}"
printf '%s\n' "$*" >> "$S/calls"
ns=""; target=""; container=""; sub=""
while [ $# -gt 0 ]; do
    case "$1" in
        -n) ns="$2"; shift 2 ;;
        -c) container="$2"; shift 2 ;;
        exec) sub=exec; target="$2"; shift 2 ;;
        --) shift; break ;;
        *) echo "stub: unmodelled word $1" >&2; exit 1 ;;
    esac
done
[ "$sub" = exec ] || { echo "stub: only an exec is modelled" >&2; exit 1; }
if [ "$target" != deploy/boss ] || [ "$container" != boss ]; then
    echo "stub: exec into $target -c $container, not deploy/boss -c boss" >&2
    exit 1
fi
printf '%s\n' "$ns" >> "$S/namespaces"
if [ -e "$S/no-pod" ]; then
    echo 'Error from server (NotFound): deployments.apps "boss" not found' >&2
    exit 1
fi
exec env -u http_proxy -u HTTP_PROXY -u https_proxy -u HTTPS_PROXY \
    -u all_proxy -u ALL_PROXY -u BOSS_MACHINE_TOKEN_HOSTS \
    PATH="$S/bin:$PATH" BOSS_MACHINE_TOKEN_DIR="$S/token" "$@"
"##;

/// The jobs API on loopback, as `curl` sees it: the count route and the
/// revoke route, the revoke refused 403 unless the caller asserts
/// platform-admin (as `revoke_logged_raw` refuses). `stuck` makes the
/// revoke answer without clearing the count; `code` forces an answer.
const CURL: &str = r##"#!/usr/bin/env bash
S="${KSTATE:?}"
printf '%s\n' "$*" >> "$S/curl-argv"
out=""; method=GET; url=""; hdrs=()
while [ $# -gt 0 ]; do
    case "$1" in
        -o) out="$2"; shift 2 ;;
        -w|-m|--max-time) shift 2 ;;
        -X) method="$2"; shift 2 ;;
        -H)
            case "$2" in
                @*) f="${2#@}"; printf '%s\n' "$f" >> "$S/header-files"; hdrs+=("$(cat "$f")") ;;
                *) hdrs+=("$2") ;;
            esac
            shift 2 ;;
        -*) shift ;;
        *) url="$1"; shift ;;
    esac
done
{ printf '%s %s\n' "$method" "$url"; for h in "${hdrs[@]}"; do printf '  %s\n' "$h"; done; } >> "$S/requests"
admin=no
for h in "${hdrs[@]}"; do
    case "$h" in 'x-boss-user: '*'"role":"platform-admin"'*) admin=yes ;; esac
done
count=$(cat "$S/count")
code=200
case "$method $url" in
    "GET http://127.0.0.1:7900/api/scheduling/calendar-tokens/logged-raw")
        body="{\"logged_raw\":$count}" ;;
    "POST http://127.0.0.1:7900/api/scheduling/calendar-tokens/logged-raw/revoke")
        if [ "$admin" = yes ]; then
            body="{\"revoked\":$count}"
            [ -e "$S/stuck" ] || echo 0 > "$S/count"
        else
            code=403; body="revoking the logged calendar tokens is a platform-admin's act"
        fi ;;
    *) code=404; body="no route" ;;
esac
[ -e "$S/code" ] && code=$(cat "$S/code")
printf '%s' "$body" > "$out"
printf '%s' "$code"
"##;

struct Fixture {
    state: PathBuf,
    kubectl: PathBuf,
}

fn fixture(case: &str, count: u32) -> Fixture {
    let root = scratch_dir(&format!("revoke-logged-calendar-tokens-{case}"));
    let state = root.join("state");
    std::fs::create_dir_all(state.join("bin")).unwrap();
    std::fs::create_dir_all(state.join("token")).unwrap();
    std::fs::write(state.join("token/current"), format!("{TOKEN}\n")).unwrap();
    std::fs::write(state.join("count"), format!("{count}\n")).unwrap();
    write_exec(&state.join("bin/curl"), CURL);
    let kubectl = root.join("kubectl-stub");
    write_exec(&kubectl, KUBECTL);
    Fixture { state, kubectl }
}

fn read(f: &Fixture, name: &str) -> String {
    std::fs::read_to_string(f.state.join(name)).unwrap_or_default()
}

/// Run the script; (exit code, stdout, stderr).
fn run(f: &Fixture, args: &[&str]) -> (i32, String, String) {
    let out = Command::new("bash")
        .arg(script())
        .args(args)
        .env("BOSS_KUBECTL", &f.kubectl)
        .env("KSTATE", &f.state)
        .env("BOSS_LOGGED_RAW_POD_LIB", repo_root().join("infra/lib"))
        .env("OPS_REQUEST_ID", "0f0e0d0c-0000-4000-8000-000000000077")
        .env_remove("BOSS_CLUSTER_TREE")
        .output()
        .expect("the script runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn hash_on(stderr: &str) -> String {
    stderr
        .lines()
        .find_map(|l| l.strip_prefix("plan-sha256: "))
        .unwrap_or_else(|| panic!("no plan-sha256 on stderr:\n{stderr}"))
        .to_string()
}

fn sha256(bytes: &str) -> String {
    let p = scratch_dir("revoke-logged-calendar-tokens-hash").join("plan");
    std::fs::write(&p, bytes).unwrap();
    let out = Command::new("sha256sum").arg(&p).output().unwrap();
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .unwrap()
        .to_string()
}

fn verb(name: &str) -> serde_json::Value {
    let p = repo_root().join(format!("infra/ops/verbs/{name}.json"));
    serde_json::from_str(&std::fs::read_to_string(&p).unwrap_or_else(|e| {
        panic!("read {}: {e}", p.display());
    }))
    .unwrap()
}

/// jq's engine, the runner's: does `re` match `text`?
fn jq_test(re: &str, text: &str) -> bool {
    let out = Command::new("jq")
        .args(["-rn", "--arg", "p", text, "--arg", "re", re])
        .arg("$p | test($re)")
        .output()
        .expect("jq runs");
    assert!(
        out.status.success(),
        "jq could not judge {re:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim() == "true"
}

fn effect_lines(stdout: &str) -> Vec<String> {
    let re = verb("revoke-logged-calendar-tokens")["effect"]
        .as_str()
        .expect("the act declares its effect")
        .to_string();
    stdout
        .lines()
        .filter(|l| jq_test(&re, l))
        .map(str::to_string)
        .collect()
}

fn posts(f: &Fixture) -> usize {
    read(f, "requests")
        .lines()
        .filter(|l| l.starts_with("POST "))
        .count()
}

/// The token is presented in a header FILE that is gone when the script
/// exits, on every request, and in no argv anywhere — not kubectl's, not
/// curl's — and in nothing the script printed.
fn the_token_rode_only_in_a_file(f: &Fixture, stdout: &str, stderr: &str) {
    let requests = read(f, "requests");
    let sent = requests.lines().filter(|l| !l.starts_with(' ')).count();
    let stamped = requests
        .lines()
        .filter(|l| *l == format!("  x-boss-machine-token: {TOKEN}"))
        .count();
    assert!(sent > 0, "no request reached the stub API");
    assert_eq!(
        stamped, sent,
        "every request presents the container's machine token:\n{requests}"
    );
    for (what, text) in [
        ("kubectl argv", read(f, "calls")),
        ("curl argv", read(f, "curl-argv")),
        ("stdout", stdout.to_string()),
        ("stderr", stderr.to_string()),
    ] {
        assert!(
            !text.contains(TOKEN),
            "the machine token appears in {what}:\n{text}"
        );
    }
    for file in read(f, "header-files").lines() {
        assert!(
            !Path::new(file).exists(),
            "the header file {file} outlived the in-pod program"
        );
    }
}

#[test]
fn the_plan_counts_the_logged_tokens_inside_the_instance_and_writes_nothing() {
    let f = fixture("plan", 3);
    let (rc, out, err) = run(&f, &["--plan", "boss-playground"]);
    assert_eq!(rc, 0, "stdout:\n{out}\nstderr:\n{err}");
    assert_eq!(
        out,
        "plan: revoke-logged-calendar-tokens\n\
         instance: playground\n\
         namespace: boss-playground\n\
         read: GET /api/scheduling/calendar-tokens/logged-raw on the instance's jobs port 7900, from inside deploy/boss container boss through kubectl exec, as automation:ops-runner at audit-readonly\n\
         logged_raw: 3\n\
         \n\
         == the act this plan authorises ==\n\
         POST /api/scheduling/calendar-tokens/logged-raw/revoke on the instance's jobs port 7900, from inside deploy/boss container boss through kubectl exec, ONCE, as automation:ops-runner at platform-admin: it revokes every calendar feed whose token the instance's audit log holds in the clear (3 now), one rotated event each. Then logged_raw is read back; only 0 is the verdict.\n"
    );
    assert_eq!(hash_on(&err), sha256(&out), "the hash is of the plan bytes");
    assert_eq!(posts(&f), 0, "a plan writes nothing");
    assert_eq!(read(&f, "namespaces"), "boss-playground\n");
    the_token_rode_only_in_a_file(&f, &out, &err);
    assert!(
        read(&f, "requests")
            .contains(r#"x-boss-user: {"id":"automation:ops-runner","role":"audit-readonly","#),
        "the count is read at audit-readonly, never as platform-admin:\n{}",
        read(&f, "requests")
    );
    // Deterministic: a second render of the same state is the same bytes.
    let (_, again, err2) = run(&f, &["--plan", "boss-playground"]);
    assert_eq!(again, out);
    assert_eq!(hash_on(&err2), hash_on(&err));
}

#[test]
fn the_namespace_must_be_a_declared_instance_and_never_boss_dev() {
    for (ns, says) in [
        ("boss-dev", "boss-dev"),
        ("boss-nowhere", "not an instance"),
        ("Boss", "DNS label"),
    ] {
        let f = fixture(&format!("bound-{ns}"), 3);
        let (rc, out, err) = run(&f, &["--plan", ns]);
        assert_eq!(rc, 78, "{ns}: a refusal\nstdout:\n{out}\nstderr:\n{err}");
        assert!(err.contains(says), "{ns}: the refusal says why:\n{err}");
        assert!(out.is_empty(), "{ns}: a refusal renders no plan:\n{out}");
        assert_eq!(read(&f, "calls"), "", "{ns}: nothing was exec'd");
    }
    let f = fixture("bound-prod", 0);
    let (rc, out, err) = run(&f, &["--plan", "boss"]);
    assert_eq!(rc, 0, "prod is a declared instance:\n{err}");
    assert!(out.contains("instance: prod\nnamespace: boss\n"), "{out}");
}

#[test]
fn the_act_takes_only_a_namespace_and_a_full_plan_hash() {
    let f = fixture("usage", 3);
    for args in [
        vec!["boss-playground"],
        vec!["boss-playground", "abc123"],
        vec!["--plan", "boss-playground", "extra"],
    ] {
        let (rc, _, err) = run(&f, &args);
        assert_eq!(rc, 78, "{args:?} is refused:\n{err}");
    }
    assert_eq!(read(&f, "calls"), "", "a malformed call execs nothing");
}

#[test]
fn an_approved_plan_revokes_once_and_reads_the_count_back_to_zero() {
    let f = fixture("act", 3);
    let (_, plan, err) = run(&f, &["--plan", "boss-playground"]);
    let hash = hash_on(&err);
    let (rc, out, err) = run(&f, &["boss-playground", &hash]);
    assert_eq!(rc, 0, "stdout:\n{out}\nstderr:\n{err}");
    assert!(
        out.starts_with(&plan),
        "the approved plan prints first:\n{out}"
    );
    assert_eq!(posts(&f), 1, "exactly one revoke");
    assert_eq!(
        effect_lines(&out),
        vec![
            "revoke-logged-calendar-tokens: boss-playground revoked 3 — read back: logged_raw 0 (was 3)"
                .to_string()
        ]
    );
    let requests = read(&f, "requests");
    let post = requests.split("\nPOST ").nth(1).expect("a POST was sent");
    assert!(
        post.contains(r#"x-boss-user: {"id":"automation:ops-runner","role":"platform-admin","#),
        "the revoke asserts platform-admin as automation:ops-runner, never a person:\n{post}"
    );
    assert!(
        err.contains("0f0e0d0c-0000-4000-8000-000000000077"),
        "the run names its ops-request:\n{err}"
    );
    the_token_rode_only_in_a_file(&f, &out, &err);

    // At most once: the applied plan no longer renders, so a second run
    // refuses and sends nothing.
    let (rc, out2, err2) = run(&f, &["boss-playground", &hash]);
    assert_eq!(rc, 78, "a second run of an applied plan:\n{out2}\n{err2}");
    assert_eq!(posts(&f), 1, "no second revoke");
    assert!(effect_lines(&out2).is_empty());
}

#[test]
fn a_count_that_moved_since_the_approval_revokes_nothing() {
    let f = fixture("moved", 3);
    let (_, _, err) = run(&f, &["--plan", "boss-playground"]);
    let hash = hash_on(&err);
    std::fs::write(f.state.join("count"), "4\n").unwrap();
    let (rc, out, err) = run(&f, &["boss-playground", &hash]);
    assert_eq!(rc, 78, "stdout:\n{out}\nstderr:\n{err}");
    assert!(
        err.contains(&hash),
        "the refusal names the approved hash:\n{err}"
    );
    assert_eq!(posts(&f), 0);
    assert!(effect_lines(&out).is_empty());
}

#[test]
fn an_approved_empty_plan_sends_no_revoke() {
    let f = fixture("empty", 0);
    let (_, plan, err) = run(&f, &["--plan", "boss"]);
    assert!(
        plan.ends_with(
            "logged_raw: 0\n\n== the act this plan authorises ==\nnone — nothing to revoke\n"
        ),
        "{plan}"
    );
    let hash = hash_on(&err);
    let (rc, out, err) = run(&f, &["boss", &hash]);
    assert_eq!(rc, 0, "stdout:\n{out}\nstderr:\n{err}");
    assert_eq!(posts(&f), 0, "nothing to revoke sends no revoke");
    assert_eq!(
        effect_lines(&out),
        vec![format!(
            "revoke-logged-calendar-tokens: plan {hash} still holds and names no token — nothing to revoke in boss"
        )]
    );
}

#[test]
fn a_revoke_whose_count_does_not_read_back_zero_is_not_proven() {
    let f = fixture("stuck", 2);
    let (_, _, err) = run(&f, &["--plan", "boss-playground"]);
    let hash = hash_on(&err);
    std::fs::write(f.state.join("stuck"), "").unwrap();
    let (rc, out, err) = run(&f, &["boss-playground", &hash]);
    assert_eq!(rc, 1, "stdout:\n{out}\nstderr:\n{err}");
    assert!(
        err.contains("logged_raw 2"),
        "names what it read back:\n{err}"
    );
    assert!(effect_lines(&out).is_empty(), "no effect line:\n{out}");
}

#[test]
fn a_refused_revoke_is_a_failure_with_its_answer() {
    let f = fixture("forbidden", 2);
    let (_, _, err) = run(&f, &["--plan", "boss-playground"]);
    let hash = hash_on(&err);
    std::fs::write(f.state.join("code"), "403").unwrap();
    let (rc, out, err) = run(&f, &["boss-playground", &hash]);
    assert_eq!(rc, 1, "stdout:\n{out}\nstderr:\n{err}");
    assert!(err.contains("403"), "the answer is named:\n{err}");
    assert!(effect_lines(&out).is_empty());
}

#[test]
fn an_instance_whose_pod_cannot_be_reached_has_no_plan() {
    let f = fixture("no-pod", 3);
    std::fs::write(f.state.join("no-pod"), "").unwrap();
    let (rc, out, err) = run(&f, &["--plan", "boss-playground"]);
    assert_eq!(rc, 1, "cannot answer, never an empty plan:\n{out}\n{err}");
    assert!(out.is_empty(), "{out}");
    assert!(
        err.contains("NotFound"),
        "kubectl's own words are kept:\n{err}"
    );
}

#[test]
fn the_pair_is_a_read_only_plan_verb_and_a_passkey_gated_act() {
    let plan = verb("plan-a-logged-raw-revoke");
    let act = verb("revoke-logged-calendar-tokens");
    let about = plan["about"].as_str().unwrap();
    assert!(about.starts_with("READ-ONLY"), "{about}");
    assert!(!about.contains("MUTATING"));
    assert!(plan.get("requires_approval").is_none());
    assert_eq!(
        plan["argv"],
        serde_json::json!([
            "infra/forge/revoke-logged-calendar-tokens.sh",
            "--plan",
            "{1}"
        ])
    );
    assert_eq!(act["requires_approval"], serde_json::json!(true));
    assert_eq!(act["plan_verb"], "plan-a-logged-raw-revoke");
    assert_eq!(act["approvers"], serde_json::json!(["emp-david"]));
    assert_eq!(
        act["argv"],
        serde_json::json!(["infra/forge/revoke-logged-calendar-tokens.sh", "{1}", "{2}"])
    );
    assert_eq!(plan["hosts"], serde_json::json!(["forge"]));
    assert_eq!(act["hosts"], serde_json::json!(["forge"]));
    let names = |v: &serde_json::Value| -> Vec<String> {
        v["params"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["name"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(names(&plan), vec!["namespace"]);
    assert_eq!(names(&act), vec!["namespace", "plan_sha256"]);
    assert_eq!(plan["params"][0], act["params"][0], "one namespace bound");
}

#[test]
fn the_nothing_to_do_declaration_matches_only_the_empty_plan() {
    let re = verb("plan-a-logged-raw-revoke")["nothing_to_do"]
        .as_str()
        .expect("the plan verb declares nothing_to_do")
        .to_string();
    let f = fixture("ntd-empty", 0);
    let (_, empty, _) = run(&f, &["--plan", "boss"]);
    assert!(
        jq_test(&re, &empty),
        "{re:?} matches the empty plan:\n{empty}"
    );
    let f = fixture("ntd-work", 1);
    let (_, work, _) = run(&f, &["--plan", "boss-playground"]);
    assert!(
        !jq_test(&re, &work),
        "{re:?} matches a plan with work:\n{work}"
    );
}
