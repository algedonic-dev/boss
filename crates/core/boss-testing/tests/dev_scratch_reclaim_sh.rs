//! `infra/cluster/dev-scratch-reclaim.sh` — the dev pod's reclaim
//! sidecar — reclaims the per-builder target dirs that actually
//! accumulate, not only the one path it was told about.
//!
//! Measured 2026-09-11 01:40Z (backlog 0efbd69e): /scratch held 90 GB,
//! of which /scratch/target — the ONLY dir the reclaim knew — was 28 GB
//! and twenty sibling dirs (target-item-at-open, target-ae887a1b,
//! tgt-rerail-sweep, …) held 62 GB, nearly all for branches that had
//! landed the day before. Every coding agent is briefed to use its own
//! CARGO_TARGET_DIR under /scratch, so the caches that pile up are
//! SIBLINGS of the path the reclaim scanned, and the reclaim could not
//! see one byte of them. Its floor (50 GB free) would not have fired
//! anyway with 304 GB free — a dead cache is not a headroom problem, it
//! is an AGE problem, so the pass below runs on age regardless of free
//! space. Driven as a real script against a fixture scratch mount.

use boss_testing::{repo_root, without_proxy};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[test]
fn a_terminal_run_retires_its_worker_and_publishes_the_exact_receipt() {
    use std::os::unix::fs::symlink;
    let root = boss_testing::scratch_dir("boss-dsr-terminal-worker");
    let _guard = Scratch(root.clone());
    let _yard = Yard::new(&root);
    let store = root.join("work/tools/dev-control");
    boss_testing::create_dir(&store);
    let generation = "a".repeat(40);
    symlink(
        store.join("generations").join(&generation),
        store.join("enabled"),
    )
    .unwrap();
    let calls = root.join("worker-calls");
    boss_testing::write_exec(
        &store.join("dev-build"),
        &format!(
            r#"#!/bin/bash
printf '%s\n' "$*" >> '{}'
case "$1" in
owned-runs) [ "${{STUB_WORKER_EMPTY_INVENTORY:-0}}" != 1 ] || exit 0; echo '{{"runs":[{{"run":"11111111-1111-4111-8111-111111111111"}},{{"run":"22222222-2222-4222-8222-222222222222"}}],"next":""}}' ;;
stop-run) [ "${{STUB_WORKER_EMPTY_STOP:-0}}" != 1 ] || exit 0; echo '{{"run":"11111111-1111-4111-8111-111111111111","session":"dev-build-1234567890abcdef","retirement":{{"native":"unaltered-receipt","job_delete_acknowledged":true,"job_deletion_complete":false}}}}' ;;
*) exit 75 ;;
esac
"#,
            calls.display()
        ),
    );
    // The API transport is real shell I/O; only its response is a fixture.
    let bin = stub_curl(&root);
    std::fs::rename(bin.join("curl"), bin.join("fallback-curl")).unwrap();
    boss_testing::write_exec(
        &bin.join("curl"),
        &format!(
            r#"#!/bin/bash
url=; method=GET; body=
for ((i=1;i<=$#;i++)); do
  a=${{!i}}
  case "$a" in -X) j=$((i+1)); method=${{!j}} ;; --data-binary) j=$((i+1)); body=${{!j}} ;; http*) url=$a ;; esac
done
case "$url" in
*/11111111-1111-4111-8111-111111111111/metadata)
  [ "${{STUB_WORKER_PATCH_REFUSED:-0}}" != 1 ] || exit 22
  printf '%s\n' PATCH >> '{patches}'
  case "$body" in @*) cat "${{body#@}}" ;; *) printf '%s' "$body" ;; esac > '{saved}'; echo '{{}}' ;;
*/11111111-1111-4111-8111-111111111111)
  [ "${{STUB_WORKER_EMPTY_READ:-0}}" != 1 ] || exit 0
  [ "${{STUB_WORKER_READ_REFUSED:-0}}" != 1 ] || exit 22
  [ "${{STUB_WORKER_MALFORMED:-0}}" != 1 ] || {{ echo '{{broken'; exit 0; }}
  jq -nc --arg pad "${{STUB_WORKER_PADDING:-0}}" --arg id "${{STUB_WORKER_ID:-11111111-1111-4111-8111-111111111111}}" --arg stamp "${{STUB_WORKER_STAMP-2026-10-23T00:00:00Z}}" --arg status "${{STUB_WORKER_STATUS:-closed}}" --arg kind "${{STUB_WORKER_KIND:-agent-run}}" --slurpfile m '{saved}' '{{id:$id,kind:$kind,status:$status,metadata:({{outcome:"died",closed_at:$stamp}} + ($m[0] // {{}}))}} | if $pad == "1" then .metadata.padding=("x" * 200000) else . end' ;;
*/22222222-2222-4222-8222-222222222222) echo '{{"id":"22222222-2222-4222-8222-222222222222","kind":"agent-run","status":"open","metadata":{{}}}}' ;;
*) exec '{fallback}' "$@" ;;
esac
"#,
            saved = root.join("retirement.json").display(),
            patches = root.join("patches").display(),
            fallback = bin.join("fallback-curl").display()
        ),
    );
    boss_testing::write_file(&root.join("retirement.json"), "{}");
    // command_with installs its normal transport; retain our dedicated wrapper afterwards.
    let wrapper = std::fs::read(bin.join("curl")).unwrap();
    let mut command = command_with(&root, &[], &[]);
    std::fs::write(bin.join("curl"), &wrapper).unwrap();
    let out = command.output().unwrap();
    let text = say(&out);
    assert!(out.status.success(), "{text}");
    let calls = std::fs::read_to_string(calls).unwrap_or_default();
    assert!(
        calls.contains("stop-run 11111111-1111-4111-8111-111111111111"),
        "closed run worker was not retired: {calls}\n{text}"
    );
    assert!(
        !calls.contains("stop-run 22222222"),
        "an active run was stopped"
    );
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("retirement.json")).unwrap()).unwrap();
    assert_eq!(
        saved["worker_retirement"]["retirement"]["native"],
        "unaltered-receipt"
    );
    let again = |extra: &[(&str, &str)]| {
        let mut command = command_with(&root, &[], extra);
        std::fs::write(bin.join("curl"), &wrapper).unwrap();
        command.output().unwrap()
    };
    for variable in [
        "STUB_WORKER_EMPTY_INVENTORY",
        "STUB_WORKER_EMPTY_READ",
        "STUB_WORKER_EMPTY_STOP",
    ] {
        let out = again(&[(variable, "1")]);
        assert!(
            !out.status.success(),
            "empty successful transport {variable} must refuse\n{}",
            say(&out)
        );
    }
    let out = again(&[]);
    assert!(out.status.success(), "{}", say(&out));
    assert_eq!(
        std::fs::read_to_string(root.join("patches"))
            .unwrap()
            .lines()
            .count(),
        1,
        "published receipt is not written twice"
    );
    let calls_before = std::fs::read_to_string(root.join("worker-calls")).unwrap();
    for extra in [
        vec![("STUB_WORKER_READ_REFUSED", "1")],
        vec![("STUB_WORKER_STATUS", "open")],
        vec![("STUB_WORKER_KIND", "backlog-item")],
        vec![("STUB_WORKER_ID", "99999999-9999-4999-8999-999999999999")],
        vec![("STUB_WORKER_STAMP", "")],
        vec![("STUB_WORKER_MALFORMED", "1")],
    ] {
        let _out = again(&extra);
        let after = std::fs::read_to_string(root.join("worker-calls")).unwrap();
        assert_eq!(
            after.matches("stop-run").count(),
            calls_before.matches("stop-run").count(),
            "unread/open/wrong-kind evidence cannot stop a worker"
        );
    }
    boss_testing::write_file(&root.join("retirement.json"), "{}");
    let out = again(&[("STUB_WORKER_PATCH_REFUSED", "1")]);
    assert!(
        !out.status.success(),
        "publication failure must remain loud\n{}",
        say(&out)
    );
    assert!(say(&out).contains("publication/readback unpaid"));
    let out = again(&[]);
    assert!(out.status.success(), "{}", say(&out));
    let recovered: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("retirement.json")).unwrap()).unwrap();
    assert_eq!(
        recovered["worker_retirement"], saved["worker_retirement"],
        "retry publishes the original whole receipt"
    );
    boss_testing::write_file(
        &root.join("retirement.json"),
        "{\"worker_retirement\":{\"session\":\"another-worker\"}}",
    );
    let out = again(&[]);
    assert!(
        !out.status.success(),
        "a conflicting historical receipt must not be replaced\n{}",
        say(&out)
    );
    let conflict: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("retirement.json")).unwrap()).unwrap();
    assert_eq!(conflict["worker_retirement"]["session"], "another-worker");
    boss_testing::write_file(&root.join("retirement.json"), "{}");
    let out = again(&[("STUB_WORKER_PADDING", "1")]);
    assert!(
        out.status.success(),
        "a run over the single-argument bound still publishes its exact receipt\n{}",
        say(&out)
    );
    let large: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("retirement.json")).unwrap()).unwrap();
    assert_eq!(large["worker_retirement"], saved["worker_retirement"]);
}

/// Removes its directory on drop, so a panicking test leaves nothing.
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn touch_at(path: &Path, hours_ago: u64) {
    let when = format!("-{hours_ago} hours");
    let ok = Command::new("touch")
        .args(["-d", &when])
        .arg(path)
        .status()
        .expect("touch")
        .success();
    assert!(ok, "touch -d {when} {}", path.display());
}

/// A dir shaped like a cargo target: `CACHEDIR.TAG` plus `debug/`, with
/// every mtime set `hours_ago`.
fn target_dir(root: &Path, name: &str, hours_ago: u64) -> PathBuf {
    let d = root.join(name);
    boss_testing::create_dir(&d.join("debug").join("deps"));
    boss_testing::write_file(
        &d.join("CACHEDIR.TAG"),
        "Signature: 8a477f597d28d172789f06886806bc55\n",
    );
    boss_testing::write_file(&d.join("debug").join("deps").join("libfoo.rlib"), "x");
    for p in [
        d.join("debug").join("deps").join("libfoo.rlib"),
        d.join("debug").join("deps"),
        d.join("debug"),
        d.join("CACHEDIR.TAG"),
        d.clone(),
    ] {
        touch_at(&p, hours_ago);
    }
    d
}

/// A stub `curl` on PATH, so a pass that files its packet reaches this
/// and never a network: every call is appended to `curl-log.txt` as
/// `METHOD URL BODY`, and the answers walk the wrap + boss-step
/// sequence the real system of record would — the first open-packet
/// GET finds none, the POST opens `job-1`, later GETs find it with a
/// ready `run` step, a PUT is acknowledged. The maintenance helpers
/// parse the reply shape they always did; only the transport is a
/// fake.
fn stub_curl(root: &Path) -> PathBuf {
    let bin = root.join("bin");
    boss_testing::create_dir(&bin);
    boss_testing::write_exec(
        &bin.join("curl"),
        concat!(
            "#!/usr/bin/env bash\n",
            "method=GET; url=; body=; stamp=bare\n",
            "while [ $# -gt 0 ]; do\n",
            "    case \"$1\" in\n",
            "        -X) method=$2; shift ;;\n",
            "        -d) body=$(printf '%s' \"$2\" | jq -c . 2>/dev/null || printf '%s' \"$2\"); shift ;;\n",
            // The wrap sends its packet from a file (backlog 9fd7f51e).
            "        --data-binary) body=$(jq -c . \"${2#@}\" 2>/dev/null || cat \"${2#@}\"); shift ;;\n",
            // A header FILE is the machine token's (backlog 37742794):
            // `stamped` when it is the one line STUB_EXPECT_HDR holds,
            // mode 600. Logged beside the request in `$STUB_LOG.stamps`,
            // so the main log keeps the shape every other test reads.
            "        -H) case \"$2\" in @*)\n",
            "                if [ -n \"${STUB_EXPECT_HDR:-}\" ] && cmp -s \"${2#@}\" \"$STUB_EXPECT_HDR\" \\\n",
            "                    && [ \"$(stat -c %a \"${2#@}\")\" = 600 ]; then stamp=stamped; else stamp=wrong-header-file; fi ;;\n",
            "            esac; shift ;;\n",
            "        http*) url=$1 ;;\n",
            "    esac\n",
            "    shift\n",
            "done\n",
            "printf '%s %s %s\\n' \"$method\" \"$url\" \"$body\" >> \"$STUB_LOG\"\n",
            "printf '%s %s %s\\n' \"$stamp\" \"$method\" \"$url\" >> \"$STUB_LOG.stamps\"\n",
            // The fast-forward pass asked the system of record whether a
            // gate was LAUNCHING until backlog af27db95 (2026-10-01)
            // deleted the question; the answers stay, so a pin can hand
            // the pass the two readings that once held the checkout and
            // see that nothing holds it now: one gate opened THIS SECOND
            // with STUB_OPEN_GATE, and an API that ANSWERS an error
            // (curl's 22 under -f) with STUB_GATE_RC.
            "case \"$url\" in\n",
            // The worktree pass's RECORD reads (backlog 9a044141): the
            // gate-runs launched from a worktree, the cars on a branch
            // (both narrowed by `metadata=`), and an agent-run by id.
            // Answered from the JSON document at STUB_RECORD —
            // `{gate_runs: [...], cars: [...], runs: {id: status},
            // closed_at: {id: rfc3339}}`,
            // empty when unset — filtered by the same containment the
            // real list applies; STUB_RECORD_RC fails every one of
            // them (7: nothing answered; 22: an error answered).
            // `metadata_has=<key>` answers the gate-runs carrying that
            // top-level key, as `metadata ? $n` does (backlog 7c9715b6);
            // STUB_SUBMITTED_RC fails that read alone, saying on stderr
            // what curl -fsS says of an error that answered.
            "    *kind=gate-run*metadata=* | *kind=gate-run*metadata_has=* | *kind=ship-a-change*metadata=* | */api/jobs/run*)\n",
            "        if [ -n \"${STUB_RECORD_RC:-}\" ]; then exit \"$STUB_RECORD_RC\"; fi\n",
            "        rec=$(cat \"${STUB_RECORD:-/dev/null}\" 2>/dev/null); [ -n \"$rec\" ] || rec='{}'\n",
            "        case \"$url\" in\n",
            "            */api/jobs/run*)\n",
            "                id=${url##*/api/jobs/}\n",
            "                jq -c --arg id \"$id\" '(.runs[$id] // \"open\") as $s\n",
            "                    | {id: $id, kind: \"agent-run\", status: $s,\n",
            // `closed_at` from the record's own `closed_at: {id: time}`
            // map, null when absent (backlog 3a84cbf5).
            "                       metadata: {outcome: (if $s == \"closed\" then \"landed\" else null end),\n",
            "                                  closed_at: (.closed_at[$id] // null)}}' <<<\"$rec\" ;;\n",
            "            *metadata_has=*)\n",
            "                if [ -n \"${STUB_SUBMITTED_RC:-}\" ]; then\n",
            "                    echo 'curl: (22) The requested URL returned error: 503' >&2; exit \"$STUB_SUBMITTED_RC\"; fi\n",
            "                k=${url##*metadata_has=}; k=${k%%&*}\n",
            "                jq -c --arg k \"$k\" '\n",
            "                    [(.gate_runs // [])[] | select(has($k))]\n",
            "                    | {total: length, data: [.[] | {id: (.id // \"packet\"), status: (.status // \"open\"),\n",
            "                                                    metadata: del(.id, .status)}]}' <<<\"$rec\" ;;\n",
            "            *)\n",
            "                m=${url##*metadata=}; m=${m%%&*}; m=$(printf '%b' \"${m//%/\\\\x}\")\n",
            // The server binds a FLAT object of string values and answers
            // anything else 400 (boss_jobs::metadata_containment). A stub
            // that matched a nested document by jq's `contains` answered
            // the query the real API refused on every pass for six days
            // (backlog 7c9715b6), so it refuses the same way.
            "                if ! jq -e 'type == \"object\" and all(.[]; type == \"string\")' <<<\"$m\" >/dev/null 2>&1; then\n",
            "                    echo 'curl: (22) The requested URL returned error: 400' >&2; exit 22; fi\n",
            "                case \"$url\" in *kind=gate-run*) c=gate_runs ;; *) c=cars ;; esac\n",
            "                jq -c --argjson m \"$m\" --arg c \"$c\" '\n",
            "                    [(.[$c] // [])[] | select(contains($m))]\n",
            "                    | {total: length, data: [.[] | {id: (.id // \"packet\"), status: (.status // \"open\"),\n",
            "                                                    metadata: del(.id, .status)}]}' <<<\"$rec\" ;;\n",
            "        esac\n",
            "        exit 0 ;;\n",
            "    *kind=gate-run*)\n",
            "        if [ -n \"${STUB_GATE_RC:-}\" ]; then exit \"$STUB_GATE_RC\"; fi\n",
            "        if [ -n \"${STUB_OPEN_GATE:-}\" ]; then\n",
            "            at=$(date -u +%Y-%m-%dT%H:%M:%S.000000000+00:00)\n",
            "            jq -nc --arg at \"$at\" \\\n",
            "               '{total: 1, data: [{id: \"gate-1\", status: \"open\", metadata: {opened_at: $at}}]}'\n",
            "        else echo '{\"total\":0,\"data\":[]}'; fi\n",
            "        exit 0 ;;\n",
            "esac\n",
            "case \"$method\" in\n",
            "    POST) [ -z \"${STUB_WRAP_RC:-}\" ] || exit \"$STUB_WRAP_RC\"; touch \"$STUB_LOG.opened\"; echo '{\"id\":\"job-1\"}' ;;\n",
            // The step PUT carries the status alone (backlog e39a9d2a):
            // a metadata body is refused, as the server's end state
            // refuses it; the keys ride the merge door's PATCH.
            "    PUT) case \"$body\" in *'\"metadata\"'*) exit 22 ;; esac; echo '{}' ;;\n",
            "    PATCH) [ -z \"${STUB_METADATA_RC:-}\" ] || exit \"$STUB_METADATA_RC\"; echo '{}' ;;\n",
            "    GET) if [ -e \"$STUB_LOG.opened\" ]; then\n",
            "             echo '{\"data\":[{\"id\":\"job-1\",\"status\":\"open\",\"steps\":[",
            "{\"id\":\"step-run\",\"spec_slug\":\"run\",\"status\":\"ready\",\"metadata\":{}}]}]}'\n",
            "         else echo '{\"data\":[]}'; fi ;;\n",
            "esac\n",
        ),
    );
    bin
}

/// A `git` on PATH ahead of the real one that REFUSES `ls-remote` the
/// way the sidecar's does (backlog b50a65ef, 2026-09-18: the sidecar
/// mounts only /work and /scratch — no HOME, no credential helper — and
/// the forge answers an anonymous info/refs with 401, so `git ls-remote
/// --heads origin` there is `fatal: could not read Username`, exit 128).
/// Every other git command goes through to the real binary. A call that
/// reached ls-remote leaves `git-ls-remote.txt` behind for the test to
/// find.
fn stub_git(root: &Path) -> PathBuf {
    let bin = root.join("bin");
    boss_testing::create_dir(&bin);
    let real = String::from_utf8(
        without_proxy(&mut Command::new("bash"))
            .args(["-c", "command -v git"])
            .output()
            .expect("locate git")
            .stdout,
    )
    .expect("utf8");
    let real = real.trim();
    assert!(!real.is_empty(), "a real git on PATH");
    boss_testing::write_exec(
        &bin.join("git"),
        &format!(
            concat!(
                "#!/usr/bin/env bash\n",
                "if [ -n \"${{STUB_GIT_TRACE:-}}\" ]; then printf '%s\\t' \"$@\" >> \"$STUB_GIT_TRACE\"; printf '\\n' >> \"$STUB_GIT_TRACE\"; fi\n",
                "for a in \"$@\"; do\n",
                "    if [ \"$a\" = ls-remote ]; then\n",
                "        printf '%s\\n' \"$*\" >> \"$STUB_LS_REMOTE\"\n",
                "        echo \"fatal: could not read Username for 'http://forge.test': terminal prompts disabled\" >&2\n",
                "        exit 128\n",
                "    fi\n",
                "done\n",
                "exec {real} \"$@\"\n",
            ),
            real = real
        ),
    );
    bin
}

/// What the stub git saw of ls-remote: empty when the pass never asked.
fn ls_remote_calls(scratch: &Path) -> String {
    std::fs::read_to_string(scratch.join("git-ls-remote.txt")).unwrap_or_default()
}

fn run(scratch: &Path, extra: &[(&str, &str)]) -> Output {
    run_with(scratch, &[], extra)
}

/// `run`, with arguments after the script (a mode such as `--cli`).
fn run_with(scratch: &Path, args: &[&str], extra: &[(&str, &str)]) -> Output {
    command_with(scratch, args, extra)
        .output()
        .expect("run dev-scratch-reclaim.sh")
}

/// The command `run_with` runs, unrun — for a test that must REMOVE a
/// knob the fixture sets, such as the floor share it pins to 0.
fn command_with(scratch: &Path, args: &[&str], extra: &[(&str, &str)]) -> Command {
    let bin = stub_curl(scratch);
    stub_git(scratch);
    stub_df(&bin);
    // The fixture's two directories model mounts, just as stub_df does.
    // Production requires the real kernel mount boundary before holding
    // descendants; tests must not chmod the worker's own shared parents.
    boss_testing::write_exec(
        &bin.join("mountpoint"),
        "#!/bin/bash\nlast=${!#}\n[ \"$last\" != \"${STUB_REFUSED_MOUNT:-}\" ] || exit 1\n[ \"$last\" != \"${STUB_NESTED_MOUNT:-}\" ] || exit 0\ncase \"$last\" in \"$WORK_MOUNT\"|\"$SCRATCH_MOUNT\") exit 0;; *) exit 1;; esac\n",
    );
    let installer = stub_installer(scratch);
    let mut cmd = Command::new("bash");
    without_proxy(&mut cmd);
    cmd.arg(repo_root().join("infra/cluster/dev-scratch-reclaim.sh"))
        .args(args)
        // The CLI leg goes to the stub installer below, never to the
        // registry; the stub's log is what the CLI tests read.
        .env("BOSS_CLI_INSTALLER", &installer)
        .env("STUB_INSTALL_LOG", scratch.join("install-log.txt"))
        .env("SCRATCH_MOUNT", scratch)
        .env("CARGO_TARGET_DIR", scratch.join("target"))
        .env("WORK_MOUNT", scratch.join("work"))
        .env("REPO_DIR", scratch.join("work").join("repo"))
        .env("WORKTREES_DIR", scratch.join("work").join("wt"))
        // Not the script's default (3h since backlog b4112350): the
        // floor tests date targets 1-5h old to order an LRU, and the age
        // pass must not be what takes them. The default has its own pin,
        // `with_no_knob_set_the_grace_and_the_stale_target_age_are_three_hours`.
        .env("BOSS_STALE_TARGET_H", "12")
        // The floor is a share of the REAL filesystem the fixture sits
        // on, so a nearly full test host would fire it and reclaim the
        // fixtures other tests expect kept. Off unless a test asks.
        .env("BOSS_SCRATCH_FLOOR_PCT", "0")
        // The packet goes to the stub above, never to a real system of
        // record, and a transport failure gives up at once.
        .env("BOSS_JOBS_URL", "http://sor.test:7900")
        .env("BOSS_API_RETRY_DEADLINE", "0")
        // No machine token unless a test plants one: the pass reads the
        // mount at the reader's default directory, and a gate pod's or
        // a host's own environment is handed to every process in it.
        .env("BOSS_MACHINE_TOKEN_DIR", scratch.join("no-machine-token"))
        .env("BOSS_SOR_ENV", scratch.join("no-sor.env"))
        .env_remove("BOSS_MACHINE_TOKEN_HOSTS")
        .env_remove("RUNTIME_DIRECTORY")
        .env_remove("STUB_EXPECT_HDR")
        .env("STUB_LOG", scratch.join("curl-log.txt"))
        .env("STUB_LS_REMOTE", scratch.join("git-ls-remote.txt"))
        // What the system of record holds about worktrees, cars and
        // runs: absent, it holds nothing (see `write_record`).
        .env("STUB_RECORD", scratch.join("record.json"))
        // The memory watch the hourly pass ensures (backlog 9ecd12ea)
        // has no pod to stand outside of in a fixture, and a watch that
        // is not sampling is a problem the pass files — every test's
        // problem, here. Off unless a test asks; and with no
        // service-account token in reach it asks the cluster nothing,
        // whichever pod these tests run in.
        .env("BOSS_DEV_MEMORY_WATCH", "off")
        .env("BOSS_K8S_SA_DIR", scratch.join("no-service-account"))
        // The artifact inventory (backlog 25022367) is a dry run of the
        // fixture's own work mount; off unless a test asks, so a test of
        // another leg reads only that leg's output.
        .env("BOSS_ARTIFACT_INVENTORY", "off")
        .env_remove("BOSS_ARTIFACT_INVENTORY_DIR")
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        );
    for (k, v) in extra {
        cmd.env(k, v);
    }
    cmd
}

/// What the stub curl saw, one `METHOD URL BODY` line per call.
/// The run step's keys, as boss-step.sh writes them: through the step
/// merge door, followed by a PUT carrying the status alone (backlog
/// e39a9d2a, Stage 2). The stub refuses a step PUT with a metadata body.
fn is_run_step_merge(line: &str) -> bool {
    line.starts_with("PATCH ") && line.contains("/steps/") && line.contains("/metadata ")
}

fn curl_log(scratch: &Path) -> String {
    std::fs::read_to_string(scratch.join("curl-log.txt")).unwrap_or_default()
}

fn say(out: &Output) -> String {
    format!(
        "status {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn a_stale_sibling_target_is_reclaimed_and_the_fresh_ones_and_the_primary_are_kept() {
    let root = boss_testing::scratch_dir("boss-dsr-siblings");
    let _guard = Scratch(root.clone());
    boss_testing::create_dir(&root.join("work").join("wt"));
    boss_testing::create_dir(&root.join("work").join("repo"));
    let primary = target_dir(&root, "target", 72);
    let stale = target_dir(&root, "target-landed-yesterday", 30);
    let fresh = target_dir(&root, "target-live-builder", 1);
    // Not a target at all — old, but not ours to touch.
    let notes = root.join("notes");
    boss_testing::create_dir(&notes);
    boss_testing::write_file(&notes.join("a.txt"), "keep");
    touch_at(&notes.join("a.txt"), 200);
    touch_at(&notes, 200);

    let out = run(&root, &[]);
    let text = say(&out);
    assert!(
        !stale.exists(),
        "the 30h-old sibling target must be reclaimed\n{text}"
    );
    assert!(
        fresh.exists(),
        "a sibling touched 1h ago belongs to a live builder\n{text}"
    );
    assert!(
        primary.exists(),
        "the primary CARGO_TARGET_DIR is never reclaimed by age\n{text}"
    );
    assert!(
        notes.join("a.txt").exists(),
        "a dir that is not a cargo target is not touched\n{text}"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("target-landed-yesterday") && stdout.contains("stale target"),
        "the reclaim must NAME the dir it removed and why\n{text}"
    );
}

#[test]
fn nothing_stale_reclaims_nothing_and_says_so() {
    let root = boss_testing::scratch_dir("boss-dsr-quiet");
    let _guard = Scratch(root.clone());
    boss_testing::create_dir(&root.join("work").join("wt"));
    boss_testing::create_dir(&root.join("work").join("repo"));
    let fresh = target_dir(&root, "target-live", 2);
    let out = run(&root, &[]);
    let text = say(&out);
    assert!(fresh.exists(), "{text}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("0 stale target"),
        "a quiet pass still reports its count\n{text}"
    );
    // A pass that reclaimed nothing is a reading, not work: it files no
    // packet (David, 2026-09-16 — telemetry lives outside the audit
    // log; a chore turns a reading into a packet when it matters).
    assert!(
        curl_log(&root).is_empty(),
        "a quiet pass files nothing\n{}\n{text}",
        curl_log(&root)
    );
}

#[test]
fn the_age_threshold_is_validated_like_the_floors() {
    let root = boss_testing::scratch_dir("boss-dsr-badage");
    let _guard = Scratch(root.clone());
    let out = run(&root, &[("BOSS_STALE_TARGET_H", "soon")]);
    assert_eq!(out.status.code(), Some(64), "{}", say(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("STALE_TARGET_H"),
        "{}",
        say(&out)
    );
}

// ---------------------------------------------------------------------
// The worktree pass (backlog 1933db9e, audit H11).
//
// Measured 2026-09-18 on the pod: 203 worktrees under /work/boss (20 on
// a detached HEAD), 182 distinct branches of which 175 no longer exist
// on the forge, 36 per-worktree target dirs on /scratch — 364 GB on a
// 929 GB disk at 77%. The stale-target pass above retires a target only
// by age and nothing ever retired a worktree, so a landed branch's
// checkout and its target both outlived it indefinitely. The pass under
// test removes a worktree whose branch the forge no longer has, when its
// tree is clean and it has had no git activity for a grace period, and
// takes the target wt-cargo named for it; a detached or main worktree
// is judged by idleness alone; anything dirty, live, recent or locked
// is kept and named.
//
// Driven against a REAL forge stand-in: a bare repository that `origin`
// points at, so a push leaves the checkout the `refs/remotes/origin/<b>`
// ref the pass reads. The pass reads ONLY those refs, never the forge
// (backlog b50a65ef, 2026-09-18): the sidecar has no forge credential,
// so the `git ls-remote --heads origin` the pass first shipped with
// (H11) answered `fatal: could not read Username` every hour and the
// three newest packets on the system of record all said
// `worktree_pass=skipped` with no reason. `run()` puts a git on PATH
// that refuses ls-remote, so a pass that reached for the network fails
// here the way it failed there.
// ---------------------------------------------------------------------

/// Run git in `dir`, with the author/committer fixed so no test reads the
/// pod's global config, and `hours_ago` applied to both dates so a
/// commit's age is what the test says it is.
fn git(dir: &Path, hours_ago: u64, args: &[&str]) -> String {
    let when = format!("{} +0000", unix_now() - hours_ago * 3600);
    let out = Command::new("git")
        .current_dir(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@test",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .env("GIT_AUTHOR_DATE", &when)
        .env("GIT_COMMITTER_DATE", &when)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?} in {}:\n{}{}",
        dir.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs()
}

/// The pod's checkout in miniature: a bare `forge.git` the main
/// checkout's `origin` points at, the checkout itself at `work/repo`,
/// worktrees under `work/wt/<name>`.
struct Yard {
    root: PathBuf,
    repo: PathBuf,
}

impl Yard {
    fn new(root: &Path) -> Self {
        let forge = root.join("forge.git");
        boss_testing::create_dir(&forge);
        git(&forge, 0, &["init", "--bare", "-q"]);
        let repo = root.join("work").join("repo");
        boss_testing::create_dir(&repo);
        boss_testing::create_dir(&root.join("work").join("wt"));
        git(&repo, 100, &["init", "-q"]);
        boss_testing::write_file(&repo.join("README"), "seed\n");
        git(&repo, 100, &["add", "."]);
        git(&repo, 100, &["commit", "-q", "-m", "seed"]);
        git(
            &repo,
            100,
            &["remote", "add", "origin", forge.to_str().expect("utf8")],
        );
        git(&repo, 100, &["push", "-q", "origin", "main"]);
        Self {
            root: root.to_path_buf(),
            repo,
        }
    }

    /// A worktree at `work/wt/<name>` on a new branch (or detached when
    /// `branch` is None) with one commit of its own, every activity
    /// signal — the commit, the worktree's HEAD, index and reflog, the
    /// directory itself — dated `hours_ago`.
    fn worktree(&self, name: &str, branch: Option<&str>, hours_ago: u64) -> PathBuf {
        self.worktree_writing(name, branch, hours_ago, "work.txt")
    }

    /// `worktree`, with its one commit writing `file` — so two trees
    /// whose work lands on one main do not both add the same path.
    fn worktree_writing(
        &self,
        name: &str,
        branch: Option<&str>,
        hours_ago: u64,
        file: &str,
    ) -> PathBuf {
        let path = self.root.join("work").join("wt").join(name);
        let p = path.to_str().expect("utf8");
        match branch {
            Some(b) => git(
                &self.repo,
                hours_ago,
                &["worktree", "add", "-q", "-b", b, p, "main"],
            ),
            None => git(
                &self.repo,
                hours_ago,
                &["worktree", "add", "-q", "--detach", p, "main"],
            ),
        };
        boss_testing::write_file(&path.join(file), name);
        git(&path, hours_ago, &["add", "."]);
        git(&path, hours_ago, &["commit", "-q", "-m", name]);
        let gitdir = PathBuf::from(git(&path, hours_ago, &["rev-parse", "--absolute-git-dir"]));
        for f in ["HEAD", "index", "logs/HEAD"] {
            let f = gitdir.join(f);
            if f.exists() {
                touch_at(&f, hours_ago);
            }
        }
        touch_at(&gitdir, hours_ago);
        touch_at(&path.join(file), hours_ago);
        touch_at(&path, hours_ago);
        path
    }

    /// Publish a branch to the forge stand-in — which leaves the
    /// checkout `refs/remotes/origin/<branch>`, the ref the pass reads.
    fn publish(&self, branch: &str) {
        git(&self.repo, 0, &["push", "-q", "origin", branch]);
    }

    /// Land a branch: its commit becomes the forge's `main`, so the
    /// checkout's `refs/remotes/origin/main` moves to it while NO
    /// `refs/remotes/origin/<branch>` is ever made — a car that merged
    /// and whose branch the forge swept.
    fn land(&self, branch: &str) {
        git(
            &self.repo,
            0,
            &["push", "-q", "origin", &format!("{branch}:main")],
        );
    }

    fn origin_main(&self) -> String {
        git(&self.repo, 0, &["rev-parse", "refs/remotes/origin/main"])
    }

    /// Land `branch` the way the pipeline does (backlog 4a738ca5): an
    /// earlier train moves the forge's main, `boss gate --rebase`
    /// replays the branch's commit onto it, and the train SQUASHES the
    /// car into one commit on main — with a sibling car's change when
    /// `sibling`, so the commit's patch-id is the union of two cars' and
    /// equals neither. `later_edit` then lands one more train editing
    /// every file the car's commit touched, so main no longer holds the
    /// car's bytes and only the patch-id can say it landed. The forge
    /// never has a branch of this name and the checkout's head is never
    /// on origin/main — the shape of 56 of 58 worktree heads measured on
    /// the pod. The checkout then fetches, as the dev container does.
    fn train_land(&self, branch: &str, sibling: bool, later_edit: bool) {
        let tag = branch.replace('/', "-");
        let clone = self.root.join(format!("train-{tag}"));
        let forge = self.root.join("forge.git");
        git(
            &self.root,
            0,
            &[
                "clone",
                "-q",
                forge.to_str().expect("utf8"),
                clone.to_str().expect("utf8"),
            ],
        );
        boss_testing::write_file(&clone.join(format!("earlier-{tag}.txt")), "earlier\n");
        git(&clone, 0, &["add", "."]);
        git(&clone, 0, &["commit", "-q", "-m", "an earlier train"]);
        git(
            &clone,
            0,
            &[
                "fetch",
                "-q",
                self.repo.to_str().expect("utf8"),
                &format!("refs/heads/{branch}:refs/heads/car"),
            ],
        );
        // `boss gate --rebase`: the car's commit, replayed onto a main
        // that has moved — a new sha.
        git(&clone, 0, &["cherry-pick", "car"]);
        let touched = git(&clone, 0, &["diff", "--name-only", "HEAD~1", "HEAD"]);
        if sibling {
            // The train: the replayed car and a sibling, ONE commit.
            git(&clone, 0, &["reset", "-q", "--soft", "HEAD~1"]);
            boss_testing::write_file(&clone.join(format!("sibling-{tag}.txt")), "sibling\n");
            git(&clone, 0, &["add", "."]);
            git(&clone, 0, &["commit", "-q", "-m", "train: (2 changes)"]);
        }
        if later_edit {
            for f in touched.lines().filter(|l| !l.is_empty()) {
                boss_testing::write_file(&clone.join(f), "a later car edited this\n");
            }
            git(&clone, 0, &["commit", "-q", "-am", "a later train"]);
        }
        git(&clone, 0, &["push", "-q", "origin", "main"]);
        git(&self.repo, 0, &["fetch", "-q", "origin"]);
    }

    /// Is `sha` an ancestor of the checkout's origin/main?
    fn on_origin_main(&self, sha: &str) -> bool {
        Command::new("git")
            .current_dir(&self.repo)
            .args([
                "merge-base",
                "--is-ancestor",
                sha,
                "refs/remotes/origin/main",
            ])
            .status()
            .expect("run git")
            .success()
    }
}

#[test]
fn a_clean_worktree_whose_branch_is_gone_from_the_forge_is_removed_with_its_target() {
    let root = boss_testing::scratch_dir("boss-dsr-worktrees");
    let _guard = Scratch(root.clone());
    let yard = Yard::new(&root);

    // The bulk case: LANDED — its head is on origin/main, the forge
    // swept the branch so the checkout has no origin/ ref for it — and
    // idle past the grace window. Its target is FRESH so the age pass
    // cannot be what takes it.
    let gone = yard.worktree("agent-gone", Some("feat/gone"), 30);
    yard.land("feat/gone");
    let gone_target = target_dir(&root, "target-agent-gone", 1);
    // Still on the forge — the checkout has its origin/ ref — a car in
    // flight or parked. Kept whatever its age.
    let live = yard.worktree("agent-live", Some("feat/live"), 200);
    yard.publish("feat/live");
    let live_target = target_dir(&root, "target-agent-live", 1);
    // ABANDONED: never pushed (no origin/ ref, head not on origin/main)
    // and idle past WORKTREE_MAX_AGE_H — the harness's own
    // `worktree-agent-*` branches never reach the forge at all.
    let abandoned = yard.worktree("agent-abandoned", Some("feat/abandoned"), 60);
    // Never pushed but inside WORKTREE_MAX_AGE_H: a builder's branch is
    // absent from the forge until its push, so an unpushed tree gets
    // the longer window, not the grace one.
    let unpushed = yard.worktree("agent-unpushed", Some("feat/unpushed"), 30);
    // Abandoned, idle, but carrying uncommitted work: kept and named.
    // The edits are as old as the checkout — a tree edited minutes ago
    // is kept as RECENT before its dirtiness is even read.
    let dirty = yard.worktree("agent-dirty", Some("feat/dirty"), 60);
    boss_testing::write_file(&dirty.join("work.txt"), "edited, not committed");
    boss_testing::write_file(&dirty.join("notes.txt"), "untracked");
    for p in [
        dirty.join("work.txt"),
        dirty.join("notes.txt"),
        dirty.clone(),
    ] {
        touch_at(&p, 60);
    }
    // Landed but touched within the grace window.
    let recent = yard.worktree("agent-recent", Some("feat/recent"), 1);
    // No branch at all: judged by idleness, and 7 days is the bar.
    let detached_old = yard.worktree("agent-detached-old", None, 24 * 10);
    // ...and whose head a ref still holds, so removing the checkout
    // loses no commit (the orphan case is its own test below).
    let detached_old_head = git(&detached_old, 0, &["rev-parse", "HEAD"]);
    git(
        &yard.repo,
        0,
        &["update-ref", "refs/pulls/1", &detached_old_head],
    );
    let detached_new = yard.worktree("agent-detached-new", None, 24 * 2);
    // Its directory is already gone: only the admin entry remains.
    let vanished = yard.worktree("agent-vanished", Some("feat/vanished"), 30);
    std::fs::remove_dir_all(&vanished).expect("rm vanished worktree");

    let out = run(&root, &[]);
    let text = say(&out);
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(!gone.exists(), "gone branch, clean, idle: removed\n{text}");
    assert!(
        !gone_target.exists(),
        "and its wt-cargo target with it\n{text}"
    );
    assert!(
        live.exists(),
        "a branch the checkout still has an origin/ ref for is kept\n{text}"
    );
    assert!(live_target.exists(), "and so is its target\n{text}");
    assert!(
        !abandoned.exists(),
        "no origin/ ref, not on origin/main, idle past MAX_AGE_H: abandoned, removed\n{text}"
    );
    assert!(
        unpushed.exists(),
        "no origin/ ref, not on origin/main, inside MAX_AGE_H: a builder not yet pushed, kept\n{text}"
    );
    assert!(dirty.exists(), "a dirty tree is never removed\n{text}");
    assert!(
        stdout.contains("agent-dirty") && stdout.contains("2 dirty"),
        "the kept dirty tree is NAMED with its count\n{text}"
    );
    assert!(
        recent.exists(),
        "activity inside the grace window means in use\n{text}"
    );
    assert!(
        !detached_old.exists(),
        "a detached HEAD idle 10 days is removed\n{text}"
    );
    assert!(
        detached_new.exists(),
        "a detached HEAD idle 2 days is not\n{text}"
    );
    assert!(
        stdout.contains("worktree pass: removed 3"),
        "the pass reports its totals on one line\n{text}"
    );
    assert!(
        ls_remote_calls(&root).is_empty(),
        "the pass never reads the forge — the sidecar cannot\n{}\n{text}",
        ls_remote_calls(&root)
    );
    let main = yard.origin_main();
    assert!(
        stdout.contains(&main[..8]) && stdout.contains("last fetch"),
        "the pass names the origin/main it read and that it is only as fresh as the last fetch\n{text}"
    );
    let listed = git(&yard.repo, 0, &["worktree", "list", "--porcelain"]);
    assert!(
        !listed.contains("agent-vanished"),
        "an entry whose directory is gone is pruned\n{listed}\n{text}"
    );

    // The record: one packet opened, its run step completed with the
    // totals, so the reclaim is readable from the system of record and
    // not only from a sidecar log nobody tails.
    let log = curl_log(&root);
    assert!(
        log.contains("POST http://sor.test:7900/api/jobs ")
            && log.contains("\"kind\":\"maintenance-dev-scratch-reclaim\""),
        "a pass that acted opens its packet\n{log}\n{text}"
    );
    let put = log
        .lines()
        .find(|l| is_run_step_merge(l))
        .unwrap_or_else(|| panic!("the run step is completed\n{log}\n{text}"));
    let main_ts = git(
        &yard.repo,
        0,
        &["log", "-1", "--format=%ct", "refs/remotes/origin/main"],
    );
    for field in [
        "\"result\":\"ok\"".to_string(),
        "\"worktree_pass\":\"ran\"".to_string(),
        "\"worktrees_removed\":\"3\"".to_string(),
        "\"worktrees_kept_live\":\"1\"".to_string(),
        "\"worktrees_kept_dirty\":\"1\"".to_string(),
        "\"targets_removed\":\"1\"".to_string(),
        // The ref the judgement was made against, and its commit time,
        // so a pass on a stale fetch is visible on the packet.
        format!("\"origin_main_sha\":\"{main}\""),
        format!("\"origin_main_ref_ts\":\"{main_ts}\""),
    ] {
        assert!(
            put.contains(&field),
            "the run step carries {field}\n{put}\n{text}"
        );
    }
    assert!(
        put.contains("agent-dirty"),
        "the kept dirty tree is named on the packet too\n{put}\n{text}"
    );
}

/// A checkout with no origin/main cannot say what landed. The pass
/// removes nothing, and — unlike the H11 pass, which returned in
/// silence — RECORDS the skip and its reason on its packet: a skipped
/// pass is a finding, not silence (backlog b50a65ef).
#[test]
fn a_checkout_without_origin_main_skips_the_pass_and_records_why() {
    let root = boss_testing::scratch_dir("boss-dsr-noref");
    let _guard = Scratch(root.clone());
    let yard = Yard::new(&root);
    let gone = yard.worktree("agent-gone", Some("feat/gone"), 200);
    git(
        &yard.repo,
        0,
        &["update-ref", "-d", "refs/remotes/origin/main"],
    );

    let out = run(&root, &[]);
    let text = say(&out);
    assert!(
        gone.exists(),
        "a pass that cannot answer removes nothing\n{text}"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("worktree pass skipped"),
        "the skip is said out loud, on stderr\n{text}"
    );
    assert!(
        ls_remote_calls(&root).is_empty(),
        "and never by asking the forge\n{}\n{text}",
        ls_remote_calls(&root)
    );
    let log = curl_log(&root);
    assert!(
        log.contains("POST http://sor.test:7900/api/jobs "),
        "a pass that could not answer files its packet\n{log}\n{text}"
    );
    let put = log
        .lines()
        .find(|l| is_run_step_merge(l))
        .unwrap_or_else(|| panic!("the run step is completed\n{log}\n{text}"));
    assert!(
        put.contains("\"worktree_pass\":\"skipped\"")
            && put.contains("\"worktree_pass_reason\":\"")
            && put.contains("origin/main")
            && put.contains("\"result\":\"incomplete\""),
        "the packet carries the skip AND why, as an incomplete pass\n{put}\n{text}"
    );
}

/// The mtime of a worktree's reflog, as epoch seconds.
fn reflog_mtime(worktree: &Path) -> u64 {
    let gitdir = PathBuf::from(git(worktree, 0, &["rev-parse", "--absolute-git-dir"]));
    std::fs::metadata(gitdir.join("logs/HEAD"))
        .and_then(|m| m.modified())
        .expect("reflog mtime")
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs()
}

/// ONE repo-wide git operation must not read as activity in every
/// worktree at once. Measured 2026-09-21 (backlog adce5171): the gone-
/// branch pass removed nothing and kept 373 of 397 worktrees "with git
/// activity inside the window", because the idle clock took the MAX of
/// four signals and one of them — the mtime of `$gitdir/logs/HEAD` —
/// read EXACTLY 32h on three unrelated worktrees whose own HEAD, index
/// and directory were 73h, 80h and 101h quiet. Re-measured 2026-09-23:
/// 154 worktrees' reflogs rewritten inside four seconds at 2026-09-22
/// 05:43:30Z, beside a write of info/refs and objects/info — a `git gc`,
/// whose `reflog expire --all` rewrites every worktree's reflog whether
/// or not an entry expires. The fixture does exactly that, for real,
/// and the three trees idle past their windows must still go.
///
/// What a reflog SAYS is kept as a signal: each entry carries the time
/// git wrote it, and an expire copies entries without redating them.
/// So a tree whose only recent act is a reflog entry (a reset that
/// moved no file this pass reads) is kept.
#[test]
fn a_repo_wide_reflog_rewrite_is_not_activity_in_every_worktree() {
    let root = boss_testing::scratch_dir("boss-dsr-gc-touch");
    let _guard = Scratch(root.clone());
    let yard = Yard::new(&root);

    let landed = yard.worktree("agent-landed", Some("feat/landed"), 30);
    yard.land("feat/landed");
    let abandoned = yard.worktree("agent-abandoned", Some("feat/abandoned"), 60);
    let detached = yard.worktree("agent-detached", None, 24 * 10);
    // A detached head the pass may remove is one some ref still holds —
    // here a forge PR ref, the shape `refs/pulls/<n>` takes on the pod.
    let detached_head = git(&detached, 0, &["rev-parse", "HEAD"]);
    git(
        &yard.repo,
        0,
        &["update-ref", "refs/pulls/1", &detached_head],
    );
    // Quiet by every file signal for 60h, but its reflog's newest ENTRY
    // is an hour old: someone is working here.
    let working = yard.worktree("agent-working", Some("feat/working"), 60);
    git(&working, 1, &["reset", "-q", "--soft", "HEAD"]);
    let gitdir = PathBuf::from(git(&working, 0, &["rev-parse", "--absolute-git-dir"]));
    for p in [gitdir.join("HEAD"), gitdir.join("index"), working.clone()] {
        touch_at(&p, 60);
    }

    // The repo-wide operation: what `git gc` runs first.
    git(&yard.repo, 0, &["reflog", "expire", "--all"]);
    let now = unix_now();
    for wt in [&landed, &abandoned, &detached, &working] {
        assert!(
            now.saturating_sub(reflog_mtime(wt)) < 600,
            "precondition: the expire rewrote {}'s reflog, as a gc does",
            wt.display()
        );
    }

    let out = run(&root, &[]);
    let text = say(&out);
    assert!(
        !landed.exists(),
        "landed, idle 30h against a 3h grace: a gc's reflog rewrite is not activity\n{text}"
    );
    assert!(
        !abandoned.exists(),
        "unpushed, idle 60h against 48h: removed despite the gc\n{text}"
    );
    assert!(
        !detached.exists(),
        "detached, idle 10 days, its head on a ref: removed despite the gc\n{text}"
    );
    assert!(
        working.exists(),
        "a reflog ENTRY an hour old is activity, whatever the other files say\n{text}"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("worktree pass: removed 3"),
        "the totals say so\n{text}"
    );
}

/// A detached HEAD whose commit no ref holds is UNPUSHED WORK: the
/// worktree's own HEAD and reflog are the only things naming it, and
/// `git worktree remove` deletes both, leaving the commit to the next
/// gc. Measured 2026-09-23 on the pod: of the 16 detached worktrees the
/// corrected idle clock makes due, 13 sit on a ref (a forge PR ref, a
/// branch) and 3 — preflight-2026-09-10b, preflight-3way, preflight-f73
/// — on none. Those are kept and NAMED, on the log and on the packet,
/// the way a dirty tree is, so an operator can decide.
#[test]
fn a_detached_worktree_whose_head_no_ref_holds_is_kept_and_named() {
    let root = boss_testing::scratch_dir("boss-dsr-orphan-head");
    let _guard = Scratch(root.clone());
    let yard = Yard::new(&root);
    let orphan = yard.worktree("agent-orphan", None, 24 * 10);
    // One tree the pass does remove, so it acts and files its packet.
    let landed = yard.worktree("agent-landed", Some("feat/landed"), 30);
    yard.land("feat/landed");

    let out = run(&root, &[]);
    let text = say(&out);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        orphan.exists(),
        "a commit only this worktree names is never thrown to the gc\n{text}"
    );
    assert!(
        stdout.contains("agent-orphan") && stdout.contains("no ref holds"),
        "the kept tree is named with why\n{text}"
    );
    assert!(
        stdout.contains("1 with a head no ref holds"),
        "and counted on the totals line\n{text}"
    );
    assert!(!landed.exists(), "the landed tree beside it goes\n{text}");
    let log = curl_log(&root);
    let put = log
        .lines()
        .find(|l| is_run_step_merge(l))
        .unwrap_or_else(|| panic!("the run step is completed\n{log}\n{text}"));
    assert!(
        put.contains("\"worktrees_kept_unreferenced\":\"1\"")
            && put.contains("\"worktrees_kept_unreferenced_names\":\"agent-orphan"),
        "the packet names the kept tree too\n{put}\n{text}"
    );
}

/// The OLDER pass, `reclaim_work`, fires only under the /work floor and
/// removed every clean, unlocked worktree whose DIRECTORY was 48h old —
/// with no reading of what the checkout holds. Clean is not safe for a
/// detached HEAD: its commits have no ref but the checkout, so the
/// guard the gone-worktree pass got in adce5171 is owed here too
/// (backlog 5da0428a, from the 99ce8744 builder, 2026-09-24). Three
/// days idle puts both trees past this pass's 48h and inside the gone
/// pass's 168h idle window, so only the floor pass judges them — and
/// the tree a ref holds going is what shows the floor pass ran.
#[test]
fn under_the_work_floor_a_detached_worktree_whose_head_no_ref_holds_is_kept_and_named() {
    let root = boss_testing::scratch_dir("boss-dsr-floor-orphan");
    let _guard = Scratch(root.clone());
    let yard = Yard::new(&root);
    let orphan = yard.worktree("agent-floor-orphan", None, 24 * 3);
    let held = yard.worktree("agent-floor-held", None, 24 * 3);
    let held_head = git(&held, 0, &["rev-parse", "HEAD"]);
    git(&yard.repo, 0, &["update-ref", "refs/pulls/7", &held_head]);

    // Under the /work floor both ways it can be spelled: today's
    // BOSS_WORK_FLOOR_GB, and the stub df's STUB_DF_WORK_GB that the
    // held car 99ce8744 reads once it retires that knob — so this test
    // holds on either side of that car landing.
    let out = run(
        &root,
        &[("BOSS_WORK_FLOOR_GB", "1000000"), ("STUB_DF_WORK_GB", "1")],
    );
    let text = say(&out);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !held.exists(),
        "a detached tree some ref holds is still the floor pass's to take\n{text}"
    );
    assert!(
        orphan.exists(),
        "a commit only this worktree names is never thrown to the gc, floor or not\n{text}"
    );
    assert!(
        stdout.contains("agent-floor-orphan") && stdout.contains("no ref holds"),
        "the kept tree is named with why\n{text}"
    );
    let log = curl_log(&root);
    let put = log
        .lines()
        .find(|l| is_run_step_merge(l))
        .unwrap_or_else(|| panic!("the run step is completed\n{log}\n{text}"));
    assert!(
        put.contains("\"floor_worktrees_removed\":\"1\"")
            && put.contains("\"floor_worktrees_kept_unreferenced_names\":\"agent-floor-orphan"),
        "the packet names the kept tree too\n{put}\n{text}"
    );
}

/// A fake process table under `root/proc`, read by the pass through
/// `PROC_ROOT`: `pid1` is what `/proc/1/comm` says (the pod's `pause`
/// when the sidecar shares the pod's process namespace), and each
/// `(pid, comm, start)` gets a `stat` line in the kernel's shape, its
/// start time in field 22. Returns the root to hand the pass.
fn fake_proc(root: &Path, pid1: &str, procs: &[(u32, &str, u64)]) -> PathBuf {
    let proc_root = root.join("proc");
    boss_testing::create_dir(&proc_root.join("1"));
    boss_testing::write_file(&proc_root.join("1").join("comm"), &format!("{pid1}\n"));
    for (pid, comm, start) in procs {
        boss_testing::create_dir(&proc_root.join(pid.to_string()));
        boss_testing::write_file(
            &proc_root.join(pid.to_string()).join("stat"),
            &format!("{pid} ({comm}) S 1 1 1 0 -1 4194560 0 0 0 0 0 0 0 0 20 0 1 0 {start} 0 0\n"),
        );
    }
    proc_root
}

/// Lock a worktree the way the Claude harness does.
fn harness_lock(yard: &Yard, wt: &Path, reason: &str) {
    git(
        &yard.repo,
        0,
        &[
            "worktree",
            "lock",
            "--reason",
            reason,
            wt.to_str().expect("utf8"),
        ],
    );
}

/// A lock is a claim by a PROCESS, and a claim outlives its claimant
/// unless something checks. The Claude harness locks each agent
/// worktree `claude agent agent-<id> (pid N start T)` for its session,
/// and until 2026-09-23 the pass kept every locked tree without asking
/// whether N still lived. Measured that day (backlog e14a741c): two
/// lock files named pid 355 start 94472290 while pid 355 had started at
/// 95092646 — the harness had restarted, the old sessions were gone,
/// and their trees were kept forever. /proc/<pid>/stat is
/// world-readable, so the pid AND its start time are compared: gone, or
/// started at another time, is a stale lock, and the tree is judged
/// like any other — still behind the window, dirty and unreferenced-
/// head guards — and NAMED. A live lock (the pid alive with the start
/// it recorded) is never judged stale, and neither is a lock whose
/// reason is not the harness's shape: that one is a human's.
#[test]
fn a_lock_whose_process_is_gone_or_restarted_is_stale_and_a_live_lock_is_kept() {
    let root = boss_testing::scratch_dir("boss-dsr-stale-lock");
    let _guard = Scratch(root.clone());
    let yard = Yard::new(&root);
    // Every tree UNPUSHED and idle 60h against a 48h window: without
    // its lock, each would go. The locks alone decide.
    let names = [
        "agent-live",
        "agent-restarted",
        "agent-gone",
        "agent-human",
        "agent-stale-dirty",
    ];
    let trees: Vec<PathBuf> = names
        .iter()
        .map(|n| {
            let b = format!("feat/{n}");
            yard.worktree(n, Some(&b), 60)
        })
        .collect();
    let [live, restarted, gone, human, stale_dirty] = [0, 1, 2, 3, 4].map(|i| trees[i].clone());
    harness_lock(
        &yard,
        &live,
        "claude agent agent-live (pid 355 start 95092646)",
    );
    harness_lock(
        &yard,
        &restarted,
        "claude agent agent-restarted (pid 355 start 94472290)",
    );
    harness_lock(&yard, &gone, "claude agent agent-gone (pid 4242 start 100)");
    harness_lock(&yard, &human, "operator: bisecting, leave it");
    harness_lock(
        &yard,
        &stale_dirty,
        "claude agent agent-stale-dirty (pid 4243 start 100)",
    );
    boss_testing::write_file(&stale_dirty.join("notes.txt"), "untracked");
    for p in [stale_dirty.join("notes.txt"), stale_dirty.clone()] {
        touch_at(&p, 60);
    }
    // The live process's comm carries `) ` itself — a comm is any 15
    // bytes — so a parser that splits at the FIRST close-paren reads the
    // wrong field and would call a live lock stale.
    let proc_root = fake_proc(&root, "pause", &[(355, "x) S 1 2 3", 95092646)]);

    let out = run(&root, &[("PROC_ROOT", proc_root.to_str().expect("utf8"))]);
    let text = say(&out);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        live.exists(),
        "a lock whose pid lives with the start it recorded is never stale\n{text}"
    );
    assert!(
        human.exists(),
        "a lock that is not the harness's shape is a human's, and kept\n{text}"
    );
    assert!(
        !restarted.exists(),
        "pid 355 now started at another time: the locker is gone, the tree is judged and goes\n{text}"
    );
    assert!(
        !gone.exists(),
        "no such pid: the locker is gone, the tree is judged and goes\n{text}"
    );
    assert!(
        stale_dirty.exists(),
        "a stale lock only opens the judgement: a dirty tree is still kept\n{text}"
    );
    for name in ["agent-restarted", "agent-gone", "agent-stale-dirty"] {
        assert!(
            stdout
                .lines()
                .any(|l| l.contains(name) && l.contains("stale lock")),
            "{name}'s stale lock is named in the log\n{text}"
        );
    }
    assert!(
        stdout.contains("2 locked") && stdout.contains("3 with a stale lock"),
        "the totals count live and stale locks apart\n{text}"
    );
    let listed = git(&yard.repo, 0, &["worktree", "list", "--porcelain"]);
    assert!(
        listed.contains("pid 4243 start 100"),
        "a stale-locked tree the pass KEEPS keeps its lock too — nothing is unlocked but to remove\n{listed}\n{text}"
    );
    let log = curl_log(&root);
    let put = log
        .lines()
        .find(|l| is_run_step_merge(l))
        .unwrap_or_else(|| panic!("the run step is completed\n{log}\n{text}"));
    assert!(
        put.contains("\"worktrees_kept_locked\":\"2\"")
            && put.contains("\"worktrees_stale_locks\":\"3\"")
            && put.contains("\"worktrees_stale_lock_names\":\"agent-")
            && put.contains("agent-restarted")
            && put.contains("agent-gone")
            && put.contains("agent-stale-dirty"),
        "the packet names every stale lock\n{put}\n{text}"
    );
}

/// The judgement above is only as good as the process table it reads.
/// The sidecar sees the dev container's processes because the pod sets
/// `shareProcessNamespace` (infra/cluster/manifests/boss-dev.yaml), and
/// then pid 1 is the pod's `pause`. In a namespace of its own, pid 1 is
/// the sidecar's own entrypoint, every harness pid reads as gone, and
/// every LIVE agent's lock would read stale — so a view that is not
/// pod-wide judges no lock at all, and says so.
#[test]
fn a_process_table_that_is_not_the_pods_judges_no_lock() {
    let root = boss_testing::scratch_dir("boss-dsr-unshared-proc");
    let _guard = Scratch(root.clone());
    let yard = Yard::new(&root);
    let locked = yard.worktree("agent-locked", Some("feat/locked"), 30);
    yard.land("feat/locked");
    harness_lock(
        &yard,
        &locked,
        "claude agent agent-locked (pid 4242 start 100)",
    );
    let proc_root = fake_proc(&root, "sh", &[]);

    let out = run(&root, &[("PROC_ROOT", proc_root.to_str().expect("utf8"))]);
    let text = say(&out);
    assert!(
        locked.exists(),
        "an unshared process table cannot say a harness pid is gone\n{text}"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("locks not judged"),
        "and the pass says why it did not judge\n{text}"
    );
}

/// A LOCK THAT NAMES NO OWNER (backlog f66227aa). `git worktree add
/// --lock` with no `--reason` writes git's own sentence, `added with
/// --lock`, and nothing else: no process, no person, no run. Measured
/// 2026-10-09 20:14Z on the pod: 139 of 315 worktrees carried exactly
/// that lock — 75 under /work/wt, every one idle 98h to 185h, 73 clean —
/// while /work sat at 11 GB free against its 18 GB floor and the pass
/// kept every one of them as "a human's" lock, so it removed nothing and
/// the gate's pre-flight refused every builder. A lock with no owner
/// cannot be judged by an owner, so it is judged by the tree: every guard
/// an unlocked tree passes — an origin/ ref keeps it, a dirty tree keeps
/// it, `git worktree remove` without --force — and NEVER sooner than the
/// 48h unpushed window, whatever the branch or the floor says, because
/// the live window under the floor is 30 minutes and a live owner may sit
/// that long in a clean tree. It reads no process table, so a view that
/// is not the pod's changes nothing. A lock with ANY other text,
/// including an empty one (`git worktree lock` with no reason, an act
/// someone took after the add), is still somebody's, and kept.
#[test]
fn a_lock_that_names_no_owner_is_judged_by_the_tree_behind_the_unpushed_window() {
    let root = boss_testing::scratch_dir("boss-dsr-ownerless-lock");
    let _guard = Scratch(root.clone());
    let yard = Yard::new(&root);

    // git's own text, read from git rather than typed: a tree added with
    // --lock and no reason, minutes old — which also stands as the FRESH
    // case, inside every window.
    let fresh = root.join("work").join("wt").join("agent-ownerless-fresh");
    git(
        &yard.repo,
        0,
        &[
            "worktree",
            "add",
            "-q",
            "--lock",
            "--detach",
            fresh.to_str().expect("utf8"),
            "main",
        ],
    );
    let listed = git(&yard.repo, 0, &["worktree", "list", "--porcelain"]);
    let git_text = listed
        .split("\n\n")
        .find(|b| b.contains(fresh.to_str().expect("utf8")))
        .and_then(|b| b.lines().find_map(|l| l.strip_prefix("locked ")))
        .unwrap_or_else(|| panic!("git names a reason for an add --lock\n{listed}"))
        .to_string();

    // Unpushed and idle 60h against the 48h window: with no lock it goes,
    // and its lock names nobody, so it goes.
    let old = yard.worktree("agent-ownerless-old", Some("feat/ownerless-old"), 60);
    harness_lock(&yard, &old, &git_text);
    // LANDED and idle 2h, under the floor: unlocked, the 30-minute live
    // window would take it (under_the_work_floor_a_landed_worktree_waits_
    // only_the_live_window); a lock with no owner waits the 48h.
    let landed = yard.worktree("agent-ownerless-landed", Some("feat/ownerless-landed"), 2);
    yard.land("feat/ownerless-landed");
    harness_lock(&yard, &landed, &git_text);
    // Due, but dirty: kept, and keeps its lock.
    let dirty_tree = yard.worktree("agent-ownerless-dirty", Some("feat/ownerless-dirty"), 60);
    dirty(&dirty_tree, 60);
    harness_lock(&yard, &dirty_tree, &git_text);
    // An EMPTY reason is a lock someone placed after the add: kept.
    let quiet = yard.worktree("agent-empty-reason", Some("feat/empty-reason"), 60);
    git(
        &yard.repo,
        0,
        &["worktree", "lock", quiet.to_str().expect("utf8")],
    );
    // Not the pod's process table: pid locks go unjudged, and this
    // judgement never asked it.
    let proc_root = fake_proc(&root, "sh", &[]);

    let out = run(
        &root,
        &[
            ("STUB_DF_WORK_GB", &under_the_work_floor()),
            ("PROC_ROOT", proc_root.to_str().expect("utf8")),
        ],
    );
    let text = say(&out);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !old.exists(),
        "a lock reading only {git_text:?} names no owner: the clean, unpushed tree idle 60h is judged and goes\n{text}"
    );
    assert!(
        stdout
            .lines()
            .any(|l| l.contains("agent-ownerless-old") && l.contains("names no owner")),
        "the removal says it was a lock with no owner that was judged\n{text}"
    );
    assert!(
        landed.exists(),
        "a lock with no owner waits the 48h window even landed and under the floor\n{text}"
    );
    assert!(
        dirty_tree.exists(),
        "a lock with no owner only opens the judgement: a dirty tree is still kept\n{text}"
    );
    assert!(
        quiet.exists(),
        "an empty reason is not git's add --lock sentence: somebody's lock, kept\n{text}"
    );
    assert!(fresh.exists(), "a tree added minutes ago is kept\n{text}");
    let after = git(&yard.repo, 0, &["worktree", "list", "--porcelain"]);
    for kept in [&landed, &dirty_tree, &fresh] {
        let block = after
            .split("\n\n")
            .find(|b| b.contains(kept.to_str().expect("utf8")))
            .unwrap_or_default();
        assert!(
            block.contains(&format!("locked {git_text}")),
            "a tree the pass keeps keeps its lock, word for word\n{block}\n{text}"
        );
    }
    assert!(
        stdout.contains("4 with a lock naming no owner"),
        "the totals count the ownerless locks it judged\n{text}"
    );
    let put = run_step_put(&root, &text);
    assert!(
        put.contains("\"worktrees_ownerless_locks\":\"4\"")
            && put.contains("\"worktrees_ownerless_lock_names\":\"")
            && put.contains("agent-ownerless-old"),
        "the packet counts and names every ownerless lock it judged\n{put}\n{text}"
    );
}

/// The index is not an activity signal. Measured 2026-09-23 (backlog
/// e14a741c): five worktrees' index files were written 2026-09-21
/// 17:11, days after their last commit — a `git status` refreshes the
/// index's stat cache on a CLEAN tree, and the harness snapshots every
/// session's status as it starts. Anything the index can hold that
/// HEAD does not is a staged change, which the dirty guard keeps at any
/// age; a commit, checkout or reset that writes it also writes a
/// reflog entry, which is read for what it says. So a clean, landed
/// tree whose only recent write is its index goes.
#[test]
fn an_index_refresh_on_a_clean_tree_is_not_activity() {
    let root = boss_testing::scratch_dir("boss-dsr-index-refresh");
    let _guard = Scratch(root.clone());
    let yard = Yard::new(&root);
    let landed = yard.worktree("agent-landed", Some("feat/landed"), 30);
    yard.land("feat/landed");
    let gitdir = PathBuf::from(git(&landed, 0, &["rev-parse", "--absolute-git-dir"]));
    touch_at(&gitdir.join("index"), 1);

    let out = run(&root, &[]);
    let text = say(&out);
    assert!(
        !landed.exists(),
        "landed, clean, 30h since any commit or reflog entry: an index refreshed an hour ago keeps nothing\n{text}"
    );
}

// ---------------------------------------------------------------------
// A WORKTREE'S TOP AND ITS .git FILE ARE HELD, AND PAIRED (backlog
// 0cc54611, review finding F1 of run d6747820, 2026-10-22). A linked
// worktree's `.git` is a FILE naming the admin entry git reads for that
// tree, and `git -C <tree>` follows it wherever it points. Measured on
// the pod by run 73c1e68a (stat only): 18 of 221 such files root:1500
// at 0664, in 18 tree directories at 2775, so a group member could
// rewrite one, or rename its own over it. The fixtures below are mode
// and pairing states only: a 2775 top and a 0664 .git file, a .git file
// naming a path that does not exist, and one naming ANOTHER tree's
// admin entry — nothing is planted for git to run.
// ---------------------------------------------------------------------

/// The admin entry git keeps for `tree` under the checkout's .git.
fn admin_entry(tree: &Path) -> PathBuf {
    PathBuf::from(git(tree, 0, &["rev-parse", "--absolute-git-dir"]))
}

#[test]
fn reclaim_status_never_reads_a_staged_gitlinks_git_file() {
    let root = boss_testing::scratch_dir("boss-dsr-gitlink-hook");
    let _guard = Scratch(root.clone());
    let yard = Yard::new(&root);
    let tree = yard.worktree("agent-gitlink", Some("feat/gitlink"), 60);
    let sub = tree.join("sub");
    boss_testing::create_dir(&sub);
    git(&sub, 60, &["init", "-q"]);
    boss_testing::write_file(&sub.join("seed"), "submodule\n");
    git(&sub, 60, &["add", "."]);
    git(&sub, 60, &["commit", "-q", "-m", "submodule"]);
    let sub_head = git(&sub, 60, &["rev-parse", "HEAD"]);
    git(
        &tree,
        60,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            "160000",
            &sub_head,
            "sub",
        ],
    );
    let planted = root.join("planted-gitdir");
    std::fs::rename(sub.join(".git"), &planted).expect("move submodule gitdir outside tree");
    boss_testing::write_file(
        &sub.join(".git"),
        &format!("gitdir: {}\n", planted.display()),
    );
    let marker = root.join("hook-ran");
    let hook = root.join("marker-hook");
    boss_testing::write_file(&hook, &format!("#!/bin/sh\ntouch '{}'\n", marker.display()));
    chmod_checkout(&hook, 0o755);
    git(
        &root,
        0,
        &[
            "--git-dir",
            planted.to_str().expect("utf8"),
            "config",
            "core.fsmonitor",
            hook.to_str().expect("utf8"),
        ],
    );
    let admin = admin_entry(&tree);
    // Control: even the admin-directed form follows a gitlink's nested
    // .git file without the flag. The marker is the only hook effect.
    git(
        &root,
        0,
        &[
            "--git-dir",
            admin.to_str().expect("utf8"),
            "--work-tree",
            tree.to_str().expect("utf8"),
            "status",
            "--porcelain",
        ],
    );
    assert!(
        marker.exists(),
        "control must execute the nested gitdir's hook"
    );
    std::fs::remove_file(&marker).expect("clear control marker");
    for entry in ["HEAD", "index", "logs/HEAD"] {
        touch_at(&admin.join(entry), 60);
    }
    touch_at(&tree, 60);
    let out = run(&root, &[]);
    let text = say(&out);
    assert!(
        !marker.exists(),
        "reclaim must not run a nested gitdir's hook\n{text}"
    );
    assert!(
        tree.exists(),
        "a staged gitlink is uncommitted work, kept\n{text}"
    );
}

#[test]
fn every_pass_holds_each_worktrees_top_and_git_file_by_owner_and_still_judges_it() {
    let root = boss_testing::scratch_dir("boss-dsr-wt-held");
    let _guard = Scratch(root.clone());
    let yard = Yard::new(&root);
    // Landed, clean, idle: the pass removes it once it is held and paired.
    let gone = yard.worktree("agent-gone", Some("feat/gone"), 30);
    yard.land("feat/gone");
    // In use: kept, and held.
    let live = yard.worktree("agent-live", Some("feat/live"), 1);
    boss_testing::create_dir(&live.join("dist"));
    for tree in [&gone, &live] {
        chmod_checkout(tree, 0o2775);
        chmod_checkout(&tree.join(".git"), 0o664);
    }
    chmod_checkout(&live.join("dist"), 0o2775);
    chmod_checkout(&live.join("work.txt"), 0o664);
    touch_at(&gone, 30);

    let out = run(&root, &[]);
    let text = say(&out);
    assert!(out.status.success(), "{text}");
    assert_eq!(
        checkout_mode(&live),
        0o2755,
        "the tree's top loses group and other write and keeps setgid\n{text}"
    );
    assert_eq!(
        checkout_mode(&live.join(".git")),
        0o644,
        "its .git file is held by its owner\n{text}"
    );
    for (p, m) in [(live.join("dist"), 0o2775), (live.join("work.txt"), 0o664)] {
        assert_eq!(
            checkout_mode(&p),
            m,
            "a tree's contents are a builder's to write: {} left as it was\n{text}",
            p.display()
        );
    }
    assert!(
        !gone.exists(),
        "a held tree paired with its admin entry is still judged: landed, clean, idle, removed\n{text}"
    );
    assert!(live.exists(), "a tree in use is kept\n{text}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let top = format!("took group and other write off {}", live.display());
    let file = format!("{}/.git (was 664)", live.display());
    assert!(
        stdout.contains(&top) && stdout.contains(&file),
        "each entry it held is named with the mode it had\n{text}"
    );
}

#[test]
fn a_worktree_whose_git_file_names_no_admin_entry_naming_it_back_is_red_and_skipped() {
    let root = boss_testing::scratch_dir("boss-dsr-wt-unpaired");
    let _guard = Scratch(root.clone());
    let yard = Yard::new(&root);
    // Three abandoned trees the pass would remove (never pushed, idle
    // past the unpushed window) and one in use whose entry is borrowed.
    let missing = yard.worktree("agent-missing", Some("feat/missing"), 60);
    let crossed = yard.worktree("agent-crossed", Some("feat/crossed"), 60);
    let paired = yard.worktree("agent-paired", Some("feat/paired"), 60);
    let other = yard.worktree("agent-other", Some("feat/other"), 1);
    let nowhere = root.join("nowhere-at-all");
    std::fs::write(
        missing.join(".git"),
        format!("gitdir: {}\n", nowhere.display()),
    )
    .expect("point .git at a path that does not exist");
    std::fs::write(
        crossed.join(".git"),
        format!("gitdir: {}\n", admin_entry(&other).display()),
    )
    .expect("point .git at another tree's admin entry");
    for tree in [&missing, &crossed] {
        touch_at(tree, 60);
    }

    let out = run(&root, &[]);
    let text = say(&out);
    assert_eq!(
        out.status.code(),
        Some(1),
        "an unpaired .git file reds the pass\n{text}"
    );
    assert!(
        missing.exists() && crossed.exists(),
        "a tree whose .git file is not paired is skipped, never judged\n{text}"
    );
    assert!(
        !paired.exists(),
        "control: the same tree, paired, is removed — the fixture is strong enough\n{text}"
    );
    assert!(
        other.exists(),
        "the tree whose entry was named is untouched\n{text}"
    );

    let log = curl_log(&root);
    let put = log
        .lines()
        .rfind(|l| is_run_step_merge(l))
        .unwrap_or_else(|| panic!("the pass records itself\n{log}\n{text}"));
    let output = recorded_output(put);
    for tree in [&missing, &crossed] {
        assert!(
            output.lines().any(|l| l
                .starts_with("RED dev-scratch-reclaim/worktree-git-unpaired problem: ")
                && l.contains(&format!("{}/.git", tree.display()))),
            "the packet names {} as unpaired\n{output}",
            tree.display()
        );
    }
    assert!(
        !output.contains("nowhere-at-all") && !text.contains("nowhere-at-all"),
        "what the .git file says is never printed, only its path\n{output}\n{text}"
    );
}

#[test]
fn absent_and_unseen_registrations_are_counted_without_git_or_permanent_red() {
    let root = boss_testing::scratch_dir("boss-dsr-unseen-hold");
    let _guard = Scratch(root.clone());
    let yard = Yard::new(&root);
    let inside = yard.worktree("agent-inside", Some("feat/inside"), 1);
    chmod_checkout(&inside, 0o775);
    let gone = yard.worktree("agent-gone", Some("feat/gone"), 1);
    git(&yard.repo, 1, &["worktree", "lock", gone.to_str().unwrap()]);
    std::fs::remove_dir_all(&gone).unwrap();
    let source = yard.worktree("agent-unseen", Some("feat/unseen"), 1);
    // The fixture's whole scratch root is SCRATCH_MOUNT, so use a
    // second root to reproduce the sidecar's view of /tmp honestly.
    let overlay = boss_testing::scratch_dir("boss-dsr-unseen-hold-overlay");
    let _overlay_guard = Scratch(overlay.clone());
    let unseen = overlay.join("outside-visible-mounts");
    git(
        &yard.repo,
        1,
        &[
            "worktree",
            "move",
            source.to_str().unwrap(),
            unseen.to_str().unwrap(),
        ],
    );
    git(
        &yard.repo,
        1,
        &["worktree", "lock", unseen.to_str().unwrap()],
    );
    let trace = root.join("per-tree-git.txt");
    let out = command_with(&root, &[], &[])
        .env("STUB_GIT_TRACE", &trace)
        .output()
        .unwrap();
    let text = say(&out);
    assert!(
        out.status.success(),
        "absent/unseen never become permanent RED: {text}"
    );
    let calls = std::fs::read_to_string(trace).unwrap();
    assert!(!calls.contains(gone.to_str().unwrap()), "{calls}");
    assert!(!calls.contains(unseen.to_str().unwrap()), "{calls}");
    let log = curl_log(&root);
    let put = log.lines().rfind(|l| is_run_step_merge(l)).unwrap();
    let metadata: serde_json::Value =
        serde_json::from_str(put.splitn(3, ' ').nth(2).unwrap()).unwrap();
    assert_eq!(metadata["worktree_hold_complete"], true);
    for (path, state) in [(&gone, "absent"), (&unseen, "unseen"), (&inside, "held")] {
        assert!(
            metadata["worktree_hold_inventory"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["path"] == path.to_str().unwrap()
                    && r["state"] == state
                    && r["git_admitted"] == (state == "held")),
            "{metadata}"
        );
    }
}

#[test]
fn registered_worktrees_outside_the_legacy_root_are_held_with_their_rename_boundary() {
    let root = boss_testing::scratch_dir("boss-dsr-registry-hold");
    let _guard = Scratch(root.clone());
    let yard = Yard::new(&root);
    let tree = yard.worktree("agent-registry", Some("feat/registry"), 1);
    let parent = root.join("work").join("registered");
    boss_testing::create_dir(&parent);
    chmod_checkout(&parent, 0o2775);
    let moved = parent.join("agent-registry");
    git(
        &yard.repo,
        1,
        &[
            "worktree",
            "move",
            tree.to_str().unwrap(),
            moved.to_str().unwrap(),
        ],
    );
    chmod_checkout(&moved, 0o2775);
    chmod_checkout(&moved.join(".git"), 0o664);
    chmod_checkout(&moved.join("work.txt"), 0o664);

    let out = run(&root, &[]);
    let text = say(&out);
    assert!(out.status.success(), "{text}\n{}", curl_log(&root));
    assert_eq!(
        checkout_mode(&moved.join(".git")),
        0o644,
        "the trusted registry, not the legacy root glob, defines coverage\n{text}"
    );
    assert_eq!(checkout_mode(&moved), 0o2755, "{text}");
    assert_eq!(
        checkout_mode(&parent),
        0o3775,
        "the owner adds sticky without removing shared creation or setgid\n{text}"
    );
    assert_eq!(
        checkout_mode(&moved.join("work.txt")),
        0o664,
        "source contents stay writable\n{text}"
    );
    let log = curl_log(&root);
    let put = log
        .lines()
        .rfind(|l| is_run_step_merge(l))
        .expect("coverage is recorded even when no tree is removed");
    let metadata: serde_json::Value =
        serde_json::from_str(put.splitn(3, ' ').nth(2).unwrap()).unwrap();
    let coverage = &metadata["worktree_hold_inventory"];
    assert!(
        coverage
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["path"] == moved.to_str().unwrap() && r["state"] == "held"),
        "{coverage}"
    );
}

#[test]
fn a_chmod_success_without_a_sticky_parent_does_not_admit_the_tree() {
    let root = boss_testing::scratch_dir("boss-dsr-sticky-refusal");
    let _guard = Scratch(root.clone());
    let yard = Yard::new(&root);
    let tree = yard.worktree("agent-no-sticky", Some("feat/no-sticky"), 60);
    yard.land("feat/no-sticky");
    let parent = root.join("work/no-sticky");
    boss_testing::create_dir(&parent);
    let moved = parent.join("agent-no-sticky");
    git(
        &yard.repo,
        60,
        &[
            "worktree",
            "move",
            tree.to_str().unwrap(),
            moved.to_str().unwrap(),
        ],
    );
    chmod_checkout(&parent, 0o2775);
    let mut cmd = command_with(&root, &[], &[]);
    boss_testing::write_exec(
        &root.join("bin/chmod"),
        "#!/bin/bash\nlast=${!#}\nif [ \"$last\" = \"$STUB_STICKY_PARENT\" ]; then exit 0; fi\nexec /usr/bin/chmod \"$@\"\n",
    );
    let trace = root.join("per-tree-git.txt");
    cmd.env("STUB_STICKY_PARENT", &parent)
        .env("STUB_GIT_TRACE", &trace);
    let out = cmd.output().unwrap();
    let text = say(&out);
    assert!(
        moved.exists(),
        "chmod's exit alone is no receipt that the rename boundary changed\n{text}"
    );
    assert_eq!(
        out.status.code(),
        Some(1),
        "the unchanged boundary is named as a problem\n{text}"
    );
    let calls = std::fs::read_to_string(trace).unwrap();
    assert!(
        !calls.contains(moved.to_str().unwrap()),
        "no Git in an unheld tree\n{calls}\n{text}"
    );
}

#[test]
fn every_registry_entry_is_accounted_even_when_absent_or_unreadable() {
    let root = boss_testing::scratch_dir("boss-dsr-registry-inventory");
    let _guard = Scratch(root.clone());
    let yard = Yard::new(&root);
    let live = yard.worktree("agent-present", Some("feat/present"), 1);
    let missing = yard.worktree("agent-absent", Some("feat/absent"), 60);
    std::fs::remove_dir_all(&missing).unwrap();
    let invalid = yard.repo.join(".git/worktrees/invalid/gitdir");
    boss_testing::create_dir(invalid.parent().unwrap());
    boss_testing::write_file(&invalid, "a planted target that must never be printed\n");
    let out = run(&root, &[]);
    let text = say(&out);
    assert_eq!(
        out.status.code(),
        Some(1),
        "an invalid registration is an explicit unknown\n{text}"
    );
    let keys = run_step_keys(&root, &text);
    let encoded = serde_json::to_string(&keys["worktree_hold_inventory"]).unwrap();
    let rows = &keys["worktree_hold_inventory"];
    let rows = rows.as_array().unwrap();
    assert_eq!(
        rows.len(),
        3,
        "every trusted admin entry has one record: {rows:?}"
    );
    assert_eq!(keys["worktree_hold_total"], 3);
    assert_eq!(keys["worktree_hold_held"], 1);
    assert!(
        rows.iter()
            .any(|r| r["path"] == live.to_str().unwrap() && r["state"] == "held")
    );
    assert!(
        rows.iter()
            .any(|r| r["path"] == missing.to_str().unwrap() && r["state"] == "absent")
    );
    assert!(rows.iter().any(
        |r| r["admin"] == invalid.parent().unwrap().to_str().unwrap()
            && r["state"] == "invalid-registration"
    ));
    let inventory = root.join("recorded-inventory.json");
    std::fs::write(&inventory, encoded).unwrap();
    let hash = Command::new("sha256sum").arg(&inventory).output().unwrap();
    let hash = String::from_utf8(hash.stdout).unwrap();
    assert_eq!(
        keys["worktree_hold_sha256"],
        hash.split_whitespace().next().unwrap()
    );
    assert!(!curl_log(&root).contains("a planted target that must never be printed"));
}

#[test]
fn unknown_or_nested_mounts_never_admit_registered_worktrees() {
    for nested in [false, true] {
        let root = boss_testing::scratch_dir("boss-dsr-mount-refusal");
        let _guard = Scratch(root.clone());
        let yard = Yard::new(&root);
        let tree = yard.worktree("agent-mount", Some("feat/mount"), 60);
        yard.land("feat/mount");
        let trace = root.join("per-tree-git.txt");
        let mut cmd = command_with(&root, &[], &[]);
        cmd.env("STUB_GIT_TRACE", &trace);
        if nested {
            cmd.env("STUB_NESTED_MOUNT", root.join("work/wt"));
        } else {
            cmd.env("STUB_REFUSED_MOUNT", root.join("work"));
        }
        let out = cmd.output().unwrap();
        let text = say(&out);
        assert_eq!(out.status.code(), Some(1), "{text}");
        assert!(
            tree.exists(),
            "unproven mount boundary retains the tree\n{text}"
        );
        let calls = std::fs::read_to_string(trace).unwrap();
        assert!(!calls.contains(tree.to_str().unwrap()), "{calls}\n{text}");
        assert!(
            recorded_output(&run_step_put(&root, &text)).contains(if nested {
                "undeclared nested mount"
            } else {
                "verified declared"
            }),
            "{text}"
        );
    }
}

#[test]
fn a_linked_admin_registry_is_not_a_trusted_membership_source() {
    let root = boss_testing::scratch_dir("boss-dsr-admin-link");
    let _guard = Scratch(root.clone());
    let yard = Yard::new(&root);
    let tree = yard.worktree("agent-linked-admin", Some("feat/linked-admin"), 1);
    let registry = yard.repo.join(".git/worktrees");
    let outside = root.join("unheld-admin-registry");
    std::fs::rename(&registry, &outside).unwrap();
    chmod_checkout(&outside, 0o2777);
    std::os::unix::fs::symlink(&outside, &registry).unwrap();
    let trace = root.join("per-tree-git.txt");
    let out = command_with(&root, &[], &[])
        .env("STUB_GIT_TRACE", &trace)
        .output()
        .unwrap();
    let text = say(&out);
    let calls = std::fs::read_to_string(trace).unwrap();
    assert!(
        !calls.contains(tree.to_str().unwrap()),
        "a linked registry cannot authorize per-tree Git\n{calls}\n{text}"
    );
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(tree.exists(), "{text}");
    assert!(
        recorded_output(&run_step_put(&root, &text)).contains("registry"),
        "{text}"
    );
}

#[test]
fn a_truncated_registry_enumeration_cannot_claim_complete_coverage() {
    assert_registry_enumeration_is_incomplete("truncate");
}

#[test]
fn a_failed_registry_enumeration_with_full_output_cannot_claim_complete_coverage() {
    assert_registry_enumeration_is_incomplete("fail-full");
}

fn assert_registry_enumeration_is_incomplete(mode: &str) {
    let root = boss_testing::scratch_dir("boss-dsr-partial-registry");
    let _guard = Scratch(root.clone());
    let yard = Yard::new(&root);
    for name in ["agent-first", "agent-second"] {
        let tree = yard.worktree(name, Some(&format!("feat/{name}")), 1);
        chmod_checkout(&tree, 0o775);
    }
    let mut cmd = command_with(&root, &[], &[]);
    boss_testing::write_exec(
        &root.join("bin/sort"),
        r#"#!/bin/bash
set -eu
if [ "$*" != '-z' ]; then exec /usr/bin/sort "$@"; fi
if [ "$STUB_REGISTRY_SORT" = truncate ]; then
    IFS= read -r -d '' first
    printf '%s\0' "$first"
    cat >/dev/null
else
    /usr/bin/sort "$@"
    printf '%s\n' 'fixture original registry sort error' >&2
    exit 17
fi
"#,
    );
    let out = cmd.env("STUB_REGISTRY_SORT", mode).output().unwrap();
    let text = say(&out);
    let log = curl_log(&root);
    let put = log.lines().rfind(|l| is_run_step_merge(l)).unwrap();
    let metadata: serde_json::Value =
        serde_json::from_str(put.splitn(3, ' ').nth(2).unwrap()).unwrap();
    assert_eq!(
        metadata["worktree_hold_complete"], false,
        "a failed or partial enumeration must not claim full inventory: {metadata}\n{text}"
    );
    assert_eq!(
        metadata["worktree_hold_total"],
        if mode == "truncate" { 1 } else { 2 }
    );
    assert_eq!(
        metadata["worktree_hold_held"],
        metadata["worktree_hold_total"]
    );
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(
        metadata["output"]
            .as_str()
            .unwrap()
            .contains("worktree-inventory-incomplete"),
        "{metadata}"
    );
    if mode == "fail-full" {
        assert!(
            text.contains("fixture original registry sort error"),
            "{text}"
        );
    }
}

#[test]
fn failed_publication_keeps_full_named_refusals_in_the_journal_without_temp_leaks() {
    let root = boss_testing::scratch_dir("boss-dsr-refusal-journal");
    let _guard = Scratch(root.clone());
    let yard = Yard::new(&root);
    let tree = yard.worktree("agent-refused", Some("feat/refused"), 1);
    std::fs::write(tree.join(".git"), "not a paired gitdir\n").unwrap();
    let temporary = root.join("record-temporaries");
    boss_testing::create_dir(&temporary);
    let out = command_with(&root, &[], &[("STUB_METADATA_RC", "22")])
        .env("TMPDIR", &temporary)
        .output()
        .unwrap();
    let text = say(&out);
    assert!(!out.status.success());
    assert!(
        text.contains("RED dev-scratch-reclaim/worktree-git-unpaired problem:"),
        "the failed sender's only full problem copy must survive in its journal: {text}"
    );
    assert!(text.contains(tree.to_str().unwrap()), "{text}");
    assert_eq!(std::fs::read_dir(temporary).unwrap().count(), 0);
    assert!(
        yard.repo
            .join(".git/boss-worktree-hold-pending.json")
            .exists()
    );
}

#[test]
fn an_unpublished_whole_inventory_is_retained_and_retried_on_an_idle_pass() {
    for refusal in ["STUB_METADATA_RC", "STUB_WRAP_RC"] {
        let root = boss_testing::scratch_dir("boss-dsr-hold-pending");
        let _guard = Scratch(root.clone());
        let yard = Yard::new(&root);
        let tree = yard.worktree("agent-pending", Some("feat/pending"), 1);
        chmod_checkout(&tree.join(".git"), 0o664);
        let temporary = root.join("record-temporaries");
        boss_testing::create_dir(&temporary);
        let first = command_with(&root, &[], &[(refusal, "22")])
            .env("TMPDIR", &temporary)
            .output()
            .unwrap();
        let pending = yard.repo.join(".git/boss-worktree-hold-pending.json");
        assert!(
            pending.is_file(),
            "the metadata refusal retains the full observation\n{}",
            say(&first)
        );
        let original: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&pending).unwrap()).unwrap();
        assert_eq!(original["worktree_hold_state"], "observed");
        assert_eq!(original["worktree_hold_held"], 1);
        assert_eq!(
            std::fs::read_dir(&temporary).unwrap().count(),
            0,
            "failed publication retains only the durable inventory, not unbounded temp files"
        );
        let first_log = curl_log(&root);
        assert!(
            !first_log
                .lines()
                .any(|l| l.starts_with("PUT ") && l.contains("/steps/")),
            "a refused metadata merge never completes the step\n{first_log}"
        );
        std::fs::remove_file(root.join("curl-log.txt")).unwrap();
        let second = run(&root, &[]);
        let text = say(&second);
        assert!(second.status.success(), "{text}");
        assert!(
            !pending.exists(),
            "only successful publication clears the retained inventory\n{text}"
        );
        let recorded = run_step_keys(&root, &text);
        assert_eq!(
            recorded["worktree_hold_inventory"],
            original["worktree_hold_inventory"]
        );
        assert_eq!(
            recorded["worktree_hold_sha256"],
            original["worktree_hold_sha256"]
        );
    }
}

/// Registry membership is necessary, not sufficient: a linked ancestor
/// keeps a tree outside the positive admission set in both passes.
#[test]
fn both_reclaim_passes_run_no_git_in_registered_trees_the_hold_did_not_pair() {
    for floor in [false, true] {
        let root = boss_testing::scratch_dir("boss-dsr-positive-hold");
        let _guard = Scratch(root.clone());
        let yard = Yard::new(&root);
        let held = yard.worktree("agent-held", Some("feat/held"), 60);
        yard.land("feat/held");
        git(&yard.repo, 60, &["merge", "-q", "--ff-only", "feat/held"]);
        let mut outside = Vec::new();
        for (name, parent) in [
            ("agent-work", root.join("work/unsafe-work")),
            ("agent-scratch", root.join("unsafe-scratch")),
        ] {
            boss_testing::create_dir(&parent);
            let tree = yard.worktree(name, Some(&format!("feat/{name}")), 60);
            yard.land(&format!("feat/{name}"));
            git(
                &yard.repo,
                60,
                &["merge", "-q", "--ff-only", &format!("feat/{name}")],
            );
            let moved = parent.join(name);
            git(
                &yard.repo,
                60,
                &[
                    "worktree",
                    "move",
                    tree.to_str().unwrap(),
                    moved.to_str().unwrap(),
                ],
            );
            chmod_checkout(&moved, 0o2775);
            chmod_checkout(&moved.join(".git"), 0o664);
            touch_at(&moved, 60);
            let aside = parent.with_extension("aside");
            std::fs::rename(&parent, &aside).unwrap();
            std::os::unix::fs::symlink(&aside, &parent).unwrap();
            outside.push(moved);
        }
        let trace = root.join("per-tree-git.txt");
        let mut cmd = command_with(&root, &[], &[]);
        cmd.env("STUB_GIT_TRACE", &trace);
        if floor {
            // Keep the hourly pass inside its grace, leaving removal to
            // the floor pass. Both still have to refuse unheld trees.
            cmd.env("BOSS_LIVE_TARGET_MIN", "100000")
                .env("STUB_DF_WORK_GB", under_the_work_floor());
        }
        let out = cmd.output().expect("run reclaim");
        let text = say(&out);
        if floor {
            assert_eq!(
                out.status.code(),
                Some(1),
                "the fixed low-space fixture remains below its floor\n{text}"
            );
            assert!(text.contains("WORK FLOOR UNMET"), "{text}");
        } else {
            assert_eq!(
                out.status.code(),
                Some(1),
                "unsafe ancestors are named problems\n{text}"
            );
        }
        assert!(
            !held.exists(),
            "held, paired control is reclaimed, floor={floor}\n{text}"
        );
        let calls = std::fs::read_to_string(&trace).expect("git trace");
        for tree in outside {
            assert!(tree.exists(), "unheld tree survives, floor={floor}\n{text}");
            assert!(
                !calls.contains(tree.to_str().unwrap()),
                "no git judges or removes unheld tree {}\n{calls}\n{text}",
                tree.display()
            );
            assert!(
                text.contains(tree.to_str().unwrap()) && text.contains("not held and paired"),
                "skip reason names the unheld tree\n{text}"
            );
        }
    }
}

#[test]
fn an_absent_legacy_hold_root_does_not_hide_the_trusted_registry() {
    let root = boss_testing::scratch_dir("boss-dsr-no-hold");
    let _guard = Scratch(root.clone());
    let yard = Yard::new(&root);
    let tree = yard.worktree("agent-unheld", Some("feat/unheld"), 60);
    yard.land("feat/unheld");
    let trace = root.join("per-tree-git.txt");
    let out = command_with(&root, &[], &[])
        .env("WORKTREES_DIR", root.join("missing-hold-root"))
        .env("STUB_GIT_TRACE", &trace)
        .env("STUB_DF_WORK_GB", under_the_work_floor())
        .output()
        .expect("run without a hold root");
    let text = say(&out);
    assert_eq!(
        out.status.code(),
        Some(1),
        "the fixed low-space fixture remains below its floor\n{text}"
    );
    assert!(text.contains("WORK FLOOR UNMET"), "{text}");
    assert!(
        !tree.exists(),
        "the trusted registry still admits and reclaims a safe tree\n{text}"
    );
    let calls = std::fs::read_to_string(trace).expect("git trace");
    assert!(
        calls.contains(tree.to_str().unwrap()),
        "the positively held registered tree is still judged\n{calls}\n{text}"
    );
}

// ---------------------------------------------------------------------
// THE CLI LEG (backlog c35eda6c, retro 27fad542, 2026-09-18): the pod's
// `boss` is the tree's by construction. Each pass takes the CLI at
// origin/main out of the cluster image with the estate's installer
// into a store under /work the shim (infra/dev/boss) reads. The retro
// counted the CLI rebuilt by hand three times in one window because a
// landed car changed what it validates locally; this pass is what
// deletes the rebuild. The installer is stubbed here — its own pin
// (install_cli_from_image_sh.rs) drives the registry protocol — and
// what is pinned is the HANDOFF: the sha, the store, the link, and the
// registry host rendered from infra/estate/estate.toml, never a literal.
// ---------------------------------------------------------------------

/// A stub installer on disk: records its argument and the environment
/// the pass handed it, and exits with `STUB_INSTALLER_RC`.
fn stub_installer(root: &Path) -> PathBuf {
    let path = root.join("bin").join("install-cli");
    boss_testing::create_dir(&root.join("bin"));
    boss_testing::write_exec(
        &path,
        concat!(
            "#!/usr/bin/env bash\n",
            "printf 'sha=%s store=%s link=%s sor_env=%s dev_control=%s\\n' \"$1\" \"${BOSS_CLI_STORE:-}\" \"${BOSS_CLI_LINK:-}\" \"${BOSS_SOR_ENV:-}\" \"${BOSS_DEV_CONTROL_STORE:-}\" >> \"$STUB_INSTALL_LOG\"\n",
            "case \"${STUB_INSTALLER_RC:-0}\" in\n",
            "    0) echo \"install-cli-from-image: CONFIRMED — stub at $1\" ;;\n",
            "    75) echo \"install-cli-from-image: NOT YET — no image for $1 (stub)\" >&2 ;;\n",
            "    *) echo \"install-cli-from-image: REFUSED — stub refusal for $1\" >&2 ;;\n",
            "esac\n",
            "exit \"${STUB_INSTALLER_RC:-0}\"\n",
        ),
    );
    path
}

fn install_log(root: &Path) -> String {
    std::fs::read_to_string(root.join("install-log.txt")).unwrap_or_default()
}

/// What infra/estate/estate.toml spells as the registry — read, not typed.
fn tree_registry() -> String {
    let toml = std::fs::read_to_string(repo_root().join("infra/estate/estate.toml"))
        .expect("infra/estate/estate.toml");
    toml.lines()
        .find_map(|l| l.strip_prefix("forge_registry = \""))
        .and_then(|v| v.strip_suffix('"'))
        .expect("estate.toml spells forge_registry")
        .to_string()
}

#[test]
fn each_pass_installs_the_trees_cli_from_the_image_through_the_estate_installer() {
    let root = boss_testing::scratch_dir("boss-dsr-cli");
    let _guard = Scratch(root.clone());
    let yard = Yard::new(&root);
    let main = git(&yard.repo, 0, &["rev-parse", "refs/remotes/origin/main"]);
    let store = root.join("work").join("tools").join("image-cli");
    let link = root
        .join("work")
        .join("tools")
        .join("bin")
        .join("boss-image");

    // Confirmed: the sha is the checkout's origin/main — the ref the
    // shim compares against, and the only one the sidecar can read (it
    // has no forge credential) — the store and link are under the work
    // mount, and the registry reaches the installer as a rendered
    // sor.env (BOSS_SOR_ENV) whose host is estate.toml's.
    let out = run(&root, &[]);
    let text = say(&out);
    assert!(out.status.success(), "{text}");
    let log = install_log(&root);
    let want = format!(
        "sha={main} store={} link={} sor_env={} dev_control={}",
        store.display(),
        link.display(),
        store.join("sor.env").display(),
        root.join("work/tools/dev-control").display()
    );
    assert_eq!(log.trim(), want, "the handoff to the installer\n{text}");
    let rendered = std::fs::read_to_string(store.join("sor.env")).expect("the rendered sor.env");
    assert!(
        rendered.contains(&format!("BOSS_FORGE_REGISTRY_HOST={}\n", tree_registry())),
        "the registry host is rendered from estate.toml, never a literal:\n{rendered}\n{text}"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("cli: install-cli-from-image: CONFIRMED") && stdout.contains(&main[..8]),
        "the installer's output rides the log, prefixed, and the sha is named\n{text}"
    );
    assert!(
        !curl_log(&root).contains("POST"),
        "a confirmed install alone files no packet\n{}\n{text}",
        curl_log(&root)
    );

    // Not yet: the image is not built — a wait, logged, not a problem.
    let out = run(&root, &[("STUB_INSTALLER_RC", "75")]);
    let text = say(&out);
    assert!(out.status.success(), "not yet is not a failure\n{text}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("not yet") && text.contains(&main[..8]),
        "the wait is said, with the sha\n{text}"
    );
    assert!(
        !curl_log(&root).contains("POST"),
        "and files nothing\n{text}"
    );

    // Refused: a real fault (a dark registry, a digest mismatch) is a
    // problem — loud, exit 1, and on a packet with the exit named.
    let out = run(&root, &[("STUB_INSTALLER_RC", "1")]);
    let text = say(&out);
    assert_eq!(
        out.status.code(),
        Some(1),
        "a refusal reds the pass\n{text}"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("CLI install FAILED"),
        "the failure is loud, on stderr\n{text}"
    );
    let log = curl_log(&root);
    let put = log
        .lines()
        .find(|l| is_run_step_merge(l))
        .unwrap_or_else(|| panic!("the run step is completed\n{log}\n{text}"));
    assert!(
        put.contains(&format!("\"cli_sha\":\"{main}\""))
            && put.contains("\"cli_result\":\"failed: exit 1\""),
        "the packet names the sha and the verdict\n{put}\n{text}"
    );

    // `--cli` runs the CLI leg alone — what the shim's refusal names as
    // the ten-second fix — and none of the reclaim passes.
    std::fs::remove_file(root.join("install-log.txt")).expect("reset the install log");
    let out = without_proxy(&mut Command::new("bash"))
        .arg(repo_root().join("infra/cluster/dev-scratch-reclaim.sh"))
        .arg("--cli")
        .env("BOSS_CLI_INSTALLER", stub_installer(&root))
        .env("STUB_INSTALL_LOG", root.join("install-log.txt"))
        .env("SCRATCH_MOUNT", &root)
        .env("WORK_MOUNT", root.join("work"))
        .env("REPO_DIR", &yard.repo)
        .env("BOSS_JOBS_URL", "http://sor.test:7900")
        .output()
        .expect("run --cli");
    let text = say(&out);
    assert!(out.status.success(), "{text}");
    assert!(
        install_log(&root).contains(&format!("sha={main} ")),
        "{text}"
    );
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("worktree pass"),
        "--cli runs no reclaim pass\n{text}"
    );

    // `--cli` is a STATUS the shim reads, not prose (backlog 49d9e99d,
    // 2026-09-18): the shim runs this leg itself before a write and
    // must tell "installed, proceed" from "not built yet, wait" without
    // parsing the log. Not yet exits 75 — the installer's own code —
    // while the hourly pass above still counts it as a wait, exit 0.
    let out = without_proxy(&mut Command::new("bash"))
        .arg(repo_root().join("infra/cluster/dev-scratch-reclaim.sh"))
        .arg("--cli")
        .env("BOSS_CLI_INSTALLER", stub_installer(&root))
        .env("STUB_INSTALLER_RC", "75")
        .env("STUB_INSTALL_LOG", root.join("install-log.txt"))
        .env("SCRATCH_MOUNT", &root)
        .env("WORK_MOUNT", root.join("work"))
        .env("REPO_DIR", &yard.repo)
        .env("BOSS_JOBS_URL", "http://sor.test:7900")
        .output()
        .expect("run --cli");
    let text = say(&out);
    assert_eq!(
        out.status.code(),
        Some(75),
        "--cli answers not yet with the installer's 75, so the shim can wait on it\n{text}"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("not yet"),
        "and still says so\n{text}"
    );
}

/// DEV ISOLATION IS NEVER SILENT (design a68fc42a D1, David 2026-10-10;
/// backlog b3071bf4). Once the store exists — isolation was activated —
/// every hourly pass reads its selector: refused, missing, or naming a
/// generation the installed CLI has moved past is a problem on the
/// pass's packet. A pod that was never activated (no store) is inert,
/// and the held state files nothing.
#[test]
fn a_refused_missing_or_behind_dev_isolation_selector_is_a_problem_every_pass() {
    use std::os::unix::fs::symlink;
    let root = boss_testing::scratch_dir("boss-dsr-dev-control");
    let _guard = Scratch(root.clone());
    let _yard = Yard::new(&root);
    let store = root.join("work/tools/dev-control");
    let cli = root.join("work/tools/image-cli");
    let (x, y) = ("a".repeat(40), "b".repeat(40));

    let out = run(&root, &[]);
    let text = say(&out);
    assert!(
        out.status.success(),
        "never activated is not a problem\n{text}"
    );
    assert!(text.contains("dev isolation: inert"), "{text}");
    assert!(!curl_log(&root).contains("dev-control"), "{text}");

    boss_testing::create_dir(&store);
    boss_testing::create_dir(&cli);
    boss_testing::write_exec(
        &store.join("dev-build"),
        "#!/bin/sh\nprintf '%s\\n' '{\"runs\":[],\"next\":\"\"}'\n",
    );
    symlink(&x, cli.join("current")).unwrap();
    symlink(store.join("generations").join(&x), store.join("enabled")).unwrap();
    let out = run(&root, &[]);
    let text = say(&out);
    assert!(
        out.status.success(),
        "a current selector is healthy\n{text}"
    );
    assert!(
        text.contains(&format!("dev isolation: on at {}", &x[..8])),
        "{text}"
    );

    let cases: [(&str, Box<dyn Fn()>); 3] = [
        (
            "dev-control-refused",
            Box::new(|| {
                std::fs::remove_file(store.join("enabled")).unwrap();
                symlink("refused", store.join("enabled")).unwrap();
            }),
        ),
        (
            "dev-control-selector-missing",
            Box::new(|| std::fs::remove_file(store.join("enabled")).unwrap()),
        ),
        (
            "dev-control-behind",
            Box::new(|| {
                symlink(store.join("generations").join(&x), store.join("enabled")).unwrap();
                std::fs::remove_file(cli.join("current")).unwrap();
                symlink(&y, cli.join("current")).unwrap();
            }),
        ),
    ];
    for (slug, plant) in cases {
        plant();
        let _ = std::fs::remove_file(root.join("curl-log.txt"));
        let out = run(&root, &[]);
        let text = say(&out);
        assert_eq!(out.status.code(), Some(1), "{slug} reds the pass\n{text}");
        let log = curl_log(&root);
        let put = log
            .lines()
            .find(|l| is_run_step_merge(l))
            .unwrap_or_else(|| panic!("{slug}: the run step is completed\n{log}\n{text}"));
        assert!(
            put.contains(&format!("dev-scratch-reclaim/{slug} problem")),
            "{slug} rides the packet by name\n{put}"
        );
        assert!(
            put.contains("activation.log"),
            "{slug} names the log\n{put}"
        );
    }
    std::fs::remove_file(store.join("enabled")).unwrap();
    std::fs::write(store.join("off.json"), "{\"state\":\"off\"}").unwrap();
    // mode-bits-ok: a controller record, never exec'd
    std::fs::set_permissions(
        store.join("off.json"),
        std::os::unix::fs::PermissionsExt::from_mode(0o600),
    )
    .unwrap();
    let out = run(&root, &[]);
    let text = say(&out);
    assert!(
        out.status.success(),
        "an explicit supported rollback is not a missing-selector alarm\n{text}"
    );
    assert!(text.contains("dev isolation: off"), "{text}");
}

#[test]
fn silence_is_not_a_supported_dev_control_rollback() {
    let root = boss_testing::scratch_dir("boss-dsr-dev-off-document");
    let _guard = Scratch(root.clone());
    let _yard = Yard::new(&root);
    let store = root.join("work/tools/dev-control");
    boss_testing::create_dir(&store);
    for body in [
        "",
        " \n\t\n",
        "not json",
        "null",
        "{}",
        "{\"state\":\"on\"}",
        "{\"state\":\"off\",\"extra\":true}",
        "{\"state\":\"on\"} {\"state\":\"off\"}",
    ] {
        std::fs::write(store.join("off.json"), body).unwrap();
        // mode-bits-ok: a controller record, never exec'd
        std::fs::set_permissions(
            store.join("off.json"),
            std::os::unix::fs::PermissionsExt::from_mode(0o600),
        )
        .unwrap();
        let _ = std::fs::remove_file(root.join("curl-log.txt"));
        let out = run(&root, &[]);
        let text = say(&out);
        assert_eq!(
            out.status.code(),
            Some(1),
            "{body:?} is no rollback\n{text}"
        );
        assert!(
            text.contains("selector"),
            "the missing selector is named\n{text}"
        );
        assert!(
            !text.contains("off — supported rollback"),
            "silence cannot suppress the alarm\n{text}"
        );
        assert!(
            curl_log(&root).contains("dev-control-selector-missing"),
            "the refusal reaches the record\n{text}"
        );
    }
    std::fs::write(store.join("off.json"), "{\"state\":\"off\"}").unwrap();
    // mode-bits-ok: deliberately unsafe fixture record, never exec'd
    std::fs::set_permissions(
        store.join("off.json"),
        std::os::unix::fs::PermissionsExt::from_mode(0o644),
    )
    .unwrap();
    let out = run(&root, &[]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "a public off record cannot silence the alarm\n{}",
        say(&out)
    );
    // mode-bits-ok: a controller record, never exec'd
    std::fs::set_permissions(
        store.join("off.json"),
        std::os::unix::fs::PermissionsExt::from_mode(0o600),
    )
    .unwrap();
    let out = run(&root, &[]);
    assert!(
        out.status.success(),
        "a real rollback document is accepted\n{}",
        say(&out)
    );
    assert!(say(&out).contains("off — supported rollback"));
}

/// A checkout with no origin/main — never fetched — names no sha to
/// install, and the pass says so rather than guess one.
#[test]
fn a_checkout_without_origin_main_installs_no_cli() {
    let root = boss_testing::scratch_dir("boss-dsr-cli-noref");
    let _guard = Scratch(root.clone());
    let yard = Yard::new(&root);
    git(
        &yard.repo,
        0,
        &["update-ref", "-d", "refs/remotes/origin/main"],
    );
    let out = run(&root, &[]);
    let text = say(&out);
    assert!(
        install_log(&root).is_empty(),
        "no origin/main, no install\n{text}"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("CLI install skipped"),
        "the skip is said out loud, on stderr\n{text}"
    );
}

// ---------------------------------------------------------------------
// THE FAST-FORWARD PASS (backlog 033d1fd3, 2026-09-19).
//
// Car fix/a-door-refuses-to-answer-from-a-stale-checkout made every pod
// door judge its own copy: a read WARNS and a boss-api write is REFUSED
// when /work/boss is behind origin/main. That is the correctness half.
// The ergonomics half was still a human act — somebody typing `git -C
// /work/boss merge --ff-only origin/main` — and the operator session
// did exactly that FOUR TIMES on 2026-09-19 alone (session start, 17:05
// after the 404, 17:40, 18:00) while trains land roughly every 50
// minutes. A warning everybody expects is a warning nobody reads, which
// is how the class comes back; mechanical operations belong to the
// machine (CLAUDE.md §Diagnosis).
//
// The pass takes NO network: it fast-forwards to the
// refs/remotes/origin/main the checkout already has, which is the same
// ref door-freshness.sh compares against — so the door can only call a
// copy stale in the window where this pass can repair it, and the
// sidecar's missing forge credential (b50a65ef) never comes into it.
//
// NO GATE HOLDS IT (backlog af27db95, 2026-10-01). The pass deferred
// while a `gate-run` packet was open — narrowed to one opened in the
// last 120 s by backlog 475fbd10 — on the theory that `boss gate`
// renders its runner from the tree as it launches. It guarded nothing:
// `boss gate` takes everything it will ever take from a tree in ONE
// `read_to_string` of the runner manifest, the first statement of
// `gate::run`, BEFORE its packet is filed, so the window the deferral
// watched opened only after the read it named; and the runner Job
// clones from the forge and mounts no /work. Measured on the live
// record from 2026-09-23: 16 deferrals, every one for a gate that had
// already read its manifest, from a builder's own worktree, or from the
// in-cluster conductor — and three passes red past a deadline that
// existed only to bound that deferral. Both are gone; the fast-forward
// asks the system of record nothing and owes nothing to it.
// ---------------------------------------------------------------------

/// The checkout's own HEAD — what the doors run from.
fn head_sha(repo: &Path) -> String {
    git(repo, 0, &["rev-parse", "HEAD"])
}

/// A fixture whose checkout is one train behind origin/main, the shape
/// the pod is in a few minutes after every merge. The worktree that
/// carried the commit is one hour old, so the worktree pass keeps it and
/// the fast-forward is the only thing the run can act on.
fn behind_by_one(root: &Path) -> Yard {
    let yard = Yard::new(root);
    yard.worktree("agent-ahead", Some("feat/ahead"), 1);
    yard.land("feat/ahead");
    yard
}

/// Did the pass ask the system of record about OPEN gate-runs — the
/// question the deleted deferral asked? The worktree pass's own gate-run
/// read is narrowed by `metadata=` to one tree and is a different
/// question, so it does not count.
fn asked_about_open_gates(log: &str) -> bool {
    log.lines()
        .any(|l| l.contains("kind=gate-run") && !l.contains("metadata="))
}

#[test]
fn a_checkout_behind_origin_main_is_fast_forwarded_and_asks_the_record_nothing() {
    let root = boss_testing::scratch_dir("boss-dsr-ff");
    let _guard = Scratch(root.clone());
    let yard = behind_by_one(&root);
    let main = yard.origin_main();
    assert_ne!(head_sha(&yard.repo), main, "the fixture starts behind");

    let out = run(&root, &[]);
    let text = say(&out);
    assert_eq!(
        head_sha(&yard.repo),
        main,
        "the checkout the doors run from is taken to origin/main\n{text}"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("fast-forward") && stdout.contains(&main[..8]),
        "the pass says what it moved and to which sha\n{text}"
    );
    let log = curl_log(&root);
    assert!(
        !asked_about_open_gates(&log),
        "no gate reads /work/boss after its packet exists, so the fast-forward asks \
         the system of record nothing about gates (backlog af27db95)\n{log}\n{text}"
    );
    // A routine fast-forward is maintenance that worked, like the CLI
    // install beside it: it rides the log, not a packet (David
    // 2026-09-16 — telemetry lives outside the audit log).
    assert!(
        !log.contains("POST"),
        "a fast-forward that worked files no packet\n{log}\n{text}"
    );
}

/// The reading that USED to hold the checkout: a gate-run opened this
/// second. That gate read its runner manifest before it filed the
/// packet, and its runner clones from the forge, so moving /work/boss
/// now cannot reach it (backlog af27db95).
#[test]
fn a_gate_run_opened_this_second_does_not_hold_the_fast_forward() {
    let root = boss_testing::scratch_dir("boss-dsr-ff-gate");
    let _guard = Scratch(root.clone());
    let yard = behind_by_one(&root);
    let main = yard.origin_main();

    let out = run(&root, &[("STUB_OPEN_GATE", "1")]);
    let text = say(&out);
    assert_eq!(
        head_sha(&yard.repo),
        main,
        "a launching gate has already taken everything it will take from a tree\n{text}"
    );
    assert!(
        out.status.success(),
        "and moving the checkout beside it is the pass working\n{text}"
    );
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("deferred")
            && !String::from_utf8_lossy(&out.stderr).contains("fast-forward deferred"),
        "nothing defers the fast-forward any more\n{text}"
    );
}

/// An arm that acts owes nothing to what it watches (CLAUDE.md
/// §Diagnosis): with the deferral gone, a dark system of record no
/// longer leaves every door here answering from an old tree.
#[test]
fn a_system_of_record_that_cannot_be_read_does_not_hold_the_fast_forward() {
    let root = boss_testing::scratch_dir("boss-dsr-ff-dark");
    let _guard = Scratch(root.clone());
    let yard = behind_by_one(&root);
    let main = yard.origin_main();

    // curl exit 22 on every read: the API ANSWERED an error.
    let out = run(&root, &[("STUB_GATE_RC", "22"), ("STUB_RECORD_RC", "22")]);
    let text = say(&out);
    assert_eq!(
        head_sha(&yard.repo),
        main,
        "the fast-forward needs nothing from the system of record\n{text}"
    );
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("fast-forward deferred"),
        "and a dark record defers nothing\n{text}"
    );
}

/// A DIVERGED checkout is left as it is and NAMED (review e5607585,
/// finding 2b). A commit of its own on main is never this pass's to
/// rewind — but it is not a checkout that forgot to pull either, and
/// door-freshness.sh declines to judge one, so until this a diverged
/// /work/boss was a log line every hour while every door ran a tree
/// origin/main does not hold. The review reproduced the route: a
/// builder call whose cd the harness dropped, writing a file and running
/// `git commit -am` from a script, leaves exactly this. The packet names
/// the head, origin/main, and what the checkout carries that main does
/// not, like the off-main problems beside it.
#[test]
fn a_checkout_that_is_not_plainly_behind_on_main_is_left_alone() {
    let root = boss_testing::scratch_dir("boss-dsr-ff-diverged");
    let _guard = Scratch(root.clone());
    let yard = behind_by_one(&root);
    boss_testing::write_file(&yard.repo.join("local.txt"), "mine\n");
    git(&yard.repo, 0, &["add", "."]);
    git(
        &yard.repo,
        0,
        &["commit", "-q", "-m", "a stray local commit"],
    );
    let before = head_sha(&yard.repo);

    let out = run(&root, &[]);
    let text = say(&out);
    assert_eq!(
        head_sha(&yard.repo),
        before,
        "a diverged checkout is never rewound or merged\n{text}"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("fast-forward skipped")
            && String::from_utf8_lossy(&out.stdout).contains("diverged"),
        "the pass names what it found\n{text}"
    );
    assert_eq!(out.status.code(), Some(1), "and reds the pass\n{text}");
    let keys = run_step_keys(&root, &text);
    assert_eq!(keys["ff_result"], "diverged", "{keys}\n{text}");
    let output = keys["output"].as_str().unwrap_or("");
    assert!(
        output.lines().any(|l| l
            .starts_with("RED dev-scratch-reclaim/checkout-diverged problem: ")
            && l.contains(&before[..8])
            && l.contains("a stray local commit")),
        "the packet names the problem, the head and the commit it carries: {keys}\n{text}"
    );
}

/// The run step's keys as the pass wrote them, from the merge door's body.
fn run_step_keys(root: &Path, text: &str) -> serde_json::Value {
    let log = curl_log(root);
    let put = log
        .lines()
        .find(|l| is_run_step_merge(l))
        .unwrap_or_else(|| panic!("the run step is completed\n{log}\n{text}"));
    serde_json::from_str(put.split_once("/metadata ").map(|(_, b)| b).unwrap_or("{}"))
        .unwrap_or_else(|e| panic!("the run step body is JSON ({e}): {put}"))
}

/// THE CHECKOUT GOES BACK TO MAIN (backlog 88553fea). Measured from the
/// /work/boss reflog on 2026-10-20: three reviewer runs moved the shared
/// checkout off main — detached at 8a9fdecb, detached at dcb010de, and
/// onto branch `worktree-agent-adc4e55861d5bfd66` for thirteen minutes —
/// each through a bare git whose `cd <worktree> &&` the harness dropped,
/// and every pod door ran that tree meanwhile. This pass used to say "a
/// checkout somebody moved is theirs to move back" and skip it; nobody
/// whose it was knew they had moved it, and the operator put it back by
/// hand. Now a CLEAN checkout off main goes back to main (and is then
/// fast-forwarded like any other), and the packet records the reflog
/// entry that moved it, so the next reader sees which run did it without
/// reading the reflog by hand.
#[test]
fn a_clean_checkout_moved_off_main_is_returned_and_the_packet_names_the_move() {
    for (shape, moved) in [
        ("branch", "to worktree-agent-adc4e55861d5bfd66"),
        ("detached", "checkout: moving from main to"),
    ] {
        let root = boss_testing::scratch_dir(&format!("boss-dsr-off-main-{shape}"));
        let _guard = Scratch(root.clone());
        let yard = behind_by_one(&root);
        let main = yard.origin_main();
        if shape == "branch" {
            git(
                &yard.repo,
                0,
                &["checkout", "-q", "-b", "worktree-agent-adc4e55861d5bfd66"],
            );
        } else {
            git(&yard.repo, 0, &["checkout", "-q", "--detach", "HEAD"]);
        }

        let out = run(&root, &[]);
        let text = say(&out);
        assert_eq!(
            git(&yard.repo, 0, &["symbolic-ref", "--short", "HEAD"]),
            "main",
            "a clean checkout {shape} off main is returned to main\n{text}"
        );
        assert_eq!(
            head_sha(&yard.repo),
            main,
            "and then fast-forwarded like any checkout on main\n{text}"
        );
        assert!(out.status.success(), "a return is the pass working\n{text}");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("returned") && stdout.contains(moved),
            "the pass says it returned the checkout and from what\n{text}"
        );
        // A return is an ACT, unlike a routine fast-forward: somebody moved
        // the tree every door runs from, and the record is where that is
        // read. The packet carries the reflog entry it found.
        let keys = run_step_keys(&root, &text);
        let from = keys["ff_returned_from"].as_str().unwrap_or("");
        assert!(
            from.contains("HEAD@{") && from.contains(moved),
            "the packet names the reflog entry that moved the checkout ({moved}): {keys}\n{text}"
        );
        assert_eq!(keys["ff_result"], "ok", "{keys}\n{text}");
    }
}

/// A DIRTY checkout off main is somebody's work in progress — or a stray
/// edit nobody owns — and switching under it could carry the edit onto
/// main or refuse halfway. So it is left exactly as it is, and NAMED: a
/// problem on the packet, with the branch and the files, because every
/// door still runs that tree until someone looks.
#[test]
fn a_dirty_checkout_off_main_is_left_alone_and_named() {
    let root = boss_testing::scratch_dir("boss-dsr-off-main-dirty");
    let _guard = Scratch(root.clone());
    let yard = behind_by_one(&root);
    git(
        &yard.repo,
        0,
        &["checkout", "-q", "-b", "worktree-agent-dirty"],
    );
    let before = head_sha(&yard.repo);
    boss_testing::write_file(&yard.repo.join("README"), "a stray edit\n");

    let out = run(&root, &[]);
    let text = say(&out);
    assert_eq!(
        git(&yard.repo, 0, &["symbolic-ref", "--short", "HEAD"]),
        "worktree-agent-dirty",
        "a dirty checkout is never switched\n{text}"
    );
    assert_eq!(head_sha(&yard.repo), before, "{text}");
    assert_eq!(
        std::fs::read_to_string(yard.repo.join("README")).expect("read README"),
        "a stray edit\n",
        "and its edit is untouched\n{text}"
    );
    assert_eq!(out.status.code(), Some(1), "and reds the pass\n{text}");
    let keys = run_step_keys(&root, &text);
    let output = keys["output"].as_str().unwrap_or("");
    assert!(
        output.contains("RED dev-scratch-reclaim/checkout-off-main problem: ")
            && output.contains("worktree-agent-dirty")
            && output.contains("README"),
        "the packet names the problem, the branch and the dirty file: {keys}\n{text}"
    );
}

/// A detached commit no branch holds is a commit of its own — the same
/// line the diverged rule draws — and switching away would leave it only
/// in the reflog. Clean or not, it is left as it is and named.
#[test]
fn a_detached_commit_no_branch_holds_is_left_alone_and_named() {
    let root = boss_testing::scratch_dir("boss-dsr-off-main-orphan");
    let _guard = Scratch(root.clone());
    let yard = behind_by_one(&root);
    git(&yard.repo, 0, &["checkout", "-q", "--detach", "HEAD"]);
    boss_testing::write_file(&yard.repo.join("orphan.txt"), "mine\n");
    git(&yard.repo, 0, &["add", "."]);
    git(
        &yard.repo,
        0,
        &["commit", "-q", "-m", "a commit on a detached head"],
    );
    let before = head_sha(&yard.repo);

    let out = run(&root, &[]);
    let text = say(&out);
    assert_eq!(
        head_sha(&yard.repo),
        before,
        "a detached commit is never left behind\n{text}"
    );
    assert_eq!(out.status.code(), Some(1), "and reds the pass\n{text}");
    let keys = run_step_keys(&root, &text);
    assert!(
        keys["output"]
            .as_str()
            .unwrap_or("")
            .contains("RED dev-scratch-reclaim/checkout-off-main problem: "),
        "the packet names it: {keys}\n{text}"
    );
}

/// git's own refusal is the last lock, and it is a FINDING: the doors
/// keep warning until somebody looks, so the pass says so on stderr,
/// reds the run, and puts the verdict on its packet — never a silent
/// checkout that quietly stopped catching up.
#[test]
fn a_fast_forward_git_refuses_is_loud_and_lands_on_the_packet() {
    let root = boss_testing::scratch_dir("boss-dsr-ff-refused");
    let _guard = Scratch(root.clone());
    let yard = behind_by_one(&root);
    let before = head_sha(&yard.repo);
    // A local file the incoming commit also adds: git refuses rather
    // than overwrite it. (Measured 2026-09-20: /work/boss carries a
    // modified, tracked .claude/settings.json, so this is the live
    // shape — and the reason the pass leans on git's narrow refusal
    // instead of demanding a pristine tree, which would never run.)
    boss_testing::write_file(&yard.repo.join("work.txt"), "mine, uncommitted\n");

    let out = run(&root, &[]);
    let text = say(&out);
    assert_eq!(
        head_sha(&yard.repo),
        before,
        "a refused fast-forward moves nothing\n{text}"
    );
    assert_eq!(
        out.status.code(),
        Some(1),
        "and reds the pass, like every other problem here\n{text}"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("fast-forward FAILED")
            && String::from_utf8_lossy(&out.stderr).contains("work.txt"),
        "git's complete complaint rides the log — a digest would throw away the only copy\n{text}"
    );
    let log = curl_log(&root);
    let put = log
        .lines()
        .find(|l| is_run_step_merge(l))
        .unwrap_or_else(|| panic!("the run step is completed\n{log}\n{text}"));
    assert!(
        put.contains("\"ff_result\":\"failed: exit")
            && put.contains(&format!("\"ff_to\":\"{}\"", yard.origin_main()))
            && put.contains("\"result\":\"incomplete\""),
        "the packet names the verdict and the sha it could not reach\n{put}\n{text}"
    );
    // AND WHAT GIT ACTUALLY SAID (backlog 6db0b658; David, 2026-09-22 —
    // accept the jam, make the refusal loud). `failed: exit 1` is a
    // verdict a reader must go and re-derive from a journal they are not
    // reading: CLAUDE.md §Diagnosis, "a verdict must name what failed".
    // The offending path is the one fact that turns the packet into an
    // action, and it was in the log all along.
    assert!(
        put.contains("\"ff_detail\":\""),
        "the packet carries git's own complaint, not only an exit code\n{put}\n{text}"
    );
    assert!(
        put.contains("work.txt"),
        "and it NAMES THE FILE — without that the reader has an exit code and a sha, and \
         must go to the host's journal to learn which path refused\n{put}\n{text}"
    );
}

// ---------------------------------------------------------------------
// THE SHARED CHECKOUT IS HELD BY ITS OWNER (backlog c19ee02b; measured
// on the pod 2026-10-21 05:00Z by run bf97aae8). /work/boss and every
// directory in it outside the worktrees were drwxrwsr-x root:1500 with
// no sticky bit, and 6,762 files were group- or other-writable, among
// them infra/dev/sor-url (where the doors and this pass send the machine
// token) and .git/config. Every builder under as-gate-uid is group 1500,
// so it could rename a door aside and plant its own, rewrite git's
// config or drop a hook beside it, and root — this pass, every door,
// every session's git — would run what it left. The hourly pass is the
// versioned owner of these modes: it takes group and other write off the
// checkout and everything in it except the worktrees' own contents,
// which builders write in, and names what it cannot hold.
// ---------------------------------------------------------------------

fn checkout_mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::symlink_metadata(path)
        .unwrap_or_else(|e| panic!("stat {}: {e}", path.display()))
        .permissions()
        .mode()
        & 0o7777
}

fn chmod_checkout(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    // mode-bits-ok: the fixture checkout, given the modes the pod's fsGroup left it
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .unwrap_or_else(|e| panic!("chmod {mode:o} {}: {e}", path.display()));
}

/// The pod's checkout as it stood on 2026-10-21, one train behind main:
/// the work mount sticky (the doors' tree holds that, backlog 3db83d0d),
/// and below it the paths root runs or reads group-writable — a door,
/// the token's address, git's config and hooks, a node_modules file
/// other-writable, the harness's worktrees directory — plus one worktree
/// under it whose own contents a builder writes in. Returns the yard,
/// the paths the pass must hold, and the worktree contents it must not
/// touch with the mode each must keep.
fn shared_checkout(root: &Path) -> (Yard, Vec<PathBuf>, Vec<(PathBuf, u32)>) {
    let yard = behind_by_one(root);
    let repo = yard.repo.clone();
    let wt = repo.join(".claude/worktrees/agent-builder");
    git(
        &repo,
        1,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            wt.to_str().unwrap(),
            "HEAD",
        ],
    );
    boss_testing::create_dir(&repo.join("infra/dev"));
    boss_testing::create_dir(&repo.join("node_modules/pkg"));
    boss_testing::create_dir(&wt.join("apps/web/dist"));
    boss_testing::write_exec(&repo.join("infra/dev/boss"), "#!/bin/sh\n");
    boss_testing::write_file(&repo.join("infra/dev/sor-url"), "http://sor.test:7900\n");
    boss_testing::write_file(&repo.join("node_modules/pkg/index.js"), "x\n");
    boss_testing::write_file(&wt.join("apps/web/dist/index.html"), "x\n");
    boss_testing::create_dir(&repo.join(".git/hooks"));
    std::os::unix::fs::symlink("boss", repo.join("infra/dev/boss-link")).expect("door link");
    // The fixture's own infra/ — named through its child, so the pin
    // that hunts a walk of the real top-level tree does not mistake it
    // for one.
    let infra = repo
        .join("infra/dev")
        .parent()
        .expect("infra")
        .to_path_buf();
    let held: Vec<(PathBuf, u32)> = vec![
        (repo.clone(), 0o2775),
        (infra, 0o2775),
        (repo.join("infra/dev"), 0o2775),
        (repo.join("infra/dev/boss"), 0o775),
        (repo.join("infra/dev/sor-url"), 0o664),
        (repo.join(".git"), 0o2775),
        (repo.join(".git/config"), 0o664),
        (repo.join(".git/hooks"), 0o2775),
        (repo.join("node_modules/pkg/index.js"), 0o666),
        (repo.join(".claude"), 0o2775),
        (repo.join(".claude/worktrees"), 0o2775),
        // A tree's TOP is held too, so no group member renames its own
        // file over the tree's .git (backlog 0cc54611).
        (wt.clone(), 0o2775),
    ];
    let kept: Vec<(PathBuf, u32)> = vec![
        (wt.join("apps/web/dist"), 0o2775),
        (wt.join("apps/web/dist/index.html"), 0o664),
    ];
    for (p, m) in held.iter().chain(kept.iter()) {
        chmod_checkout(p, *m);
    }
    chmod_checkout(&root.join("work"), 0o3775);
    (yard, held.into_iter().map(|(p, _)| p).collect(), kept)
}

/// The pass with the worktrees where the pod keeps them, inside the
/// checkout, rather than the fixture's default beside it.
fn run_in_shared_checkout(root: &Path) -> Output {
    let wts = root.join("work/repo/.claude/worktrees");
    run(root, &[("WORKTREES_DIR", wts.to_str().expect("utf8"))])
}

#[test]
fn every_pass_takes_group_and_other_write_off_the_shared_checkout_but_not_its_worktrees() {
    let root = boss_testing::scratch_dir("boss-dsr-checkout-held");
    let _guard = Scratch(root.clone());
    let (yard, held, kept) = shared_checkout(&root);
    let before: Vec<u32> = held.iter().map(|p| checkout_mode(p)).collect();
    let main = yard.origin_main();

    let out = run_in_shared_checkout(&root);
    let text = say(&out);
    assert!(out.status.success(), "{text}");
    for (p, was) in held.iter().zip(before) {
        let now = checkout_mode(p);
        assert_eq!(
            now & 0o022,
            0,
            "{} is {now:o}: a group member could still replace what root runs\n{text}",
            p.display()
        );
        assert_eq!(
            now,
            was & !0o022,
            "{} keeps setgid, its owner's bits and everyone's read ({was:o} -> {now:o})\n{text}",
            p.display()
        );
    }
    for (p, m) in &kept {
        assert_eq!(
            checkout_mode(p),
            *m,
            "a worktree's own contents are a builder's to write: {} left as it was\n{text}",
            p.display()
        );
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("shared checkout: held, re-held"),
        "the pass says it held the checkout and how many entries it re-held\n{text}"
    );
    assert!(
        stdout.contains("took group and other write off")
            && stdout.contains("infra/dev/sor-url (was 664)"),
        "each entry it held is named with the mode it had\n{text}"
    );

    // git's own writes still land in the held tree: the pass's own
    // fast-forward above, and the owner's commit and new worktree below.
    assert_eq!(
        head_sha(&yard.repo),
        main,
        "the fast-forward ran in the held checkout\n{text}"
    );
    boss_testing::write_file(&yard.repo.join("after.txt"), "after\n");
    git(&yard.repo, 0, &["add", "after.txt"]);
    git(&yard.repo, 0, &["commit", "-q", "-m", "after the hold"]);
    // Landed, so the second pass below finds the checkout current: a
    // commit origin/main does not hold is a diverged checkout, which is
    // now a problem of its own (review e5607585, finding 2b).
    yard.land("main");
    let fresh = yard.repo.join(".claude/worktrees/agent-fresh");
    git(
        &yard.repo,
        0,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feat/fresh",
            fresh.to_str().expect("utf8"),
            "main",
        ],
    );
    assert!(
        fresh.join("README").exists(),
        "a worktree is still creatable in the held checkout"
    );

    // Nothing to hold the second time, and the pass says so.
    let out = run_in_shared_checkout(&root);
    let text = say(&out);
    assert!(out.status.success(), "{text}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("shared checkout: held ("),
        "a held checkout is held, with nothing re-held\n{text}"
    );
}

/// What a group member leaves after moving the real checkout aside
/// through a work mount that is not yet sticky: a link where the
/// checkout was. The pass follows no link to hold what it points at
/// (that tree could be anyone's) and names it instead.
#[test]
fn a_checkout_replaced_by_a_link_is_a_problem_the_packet_names() {
    let root = boss_testing::scratch_dir("boss-dsr-checkout-link");
    let _guard = Scratch(root.clone());
    let (yard, _held, _kept) = shared_checkout(&root);
    let real = root.join("work/repo-real");
    std::fs::rename(&yard.repo, &real).expect("move the checkout aside");
    std::os::unix::fs::symlink(&real, &yard.repo).expect("leave a link in its place");

    let out = run_in_shared_checkout(&root);
    let text = say(&out);
    assert_eq!(
        out.status.code(),
        Some(1),
        "a checkout that is a link reds the pass\n{text}"
    );
    assert_eq!(
        checkout_mode(&real),
        0o2775,
        "the pass holds nothing through the link\n{text}"
    );
    let log = curl_log(&root);
    let put = log
        .lines()
        .find(|l| is_run_step_merge(l))
        .unwrap_or_else(|| panic!("the pass records itself\n{log}\n{text}"));
    let output = recorded_output(put);
    assert!(
        output.lines().any(
            |l| l.starts_with("RED dev-scratch-reclaim/checkout-open problem: ")
                && l.contains("work/repo is not a directory")
        ),
        "the packet names the checkout and why it cannot be held\n{output}"
    );
    assert!(
        put.contains("\"checkout\":\"OPEN: "),
        "and the checkout's state says it is not held\n{put}"
    );
}

/// NO CLIENT CONFIG IN THE DEV HOME (backlog 2b1bf9b4, review 4ceac0c8).
/// /work/home is root's HOME on the pod and group 1500 may create files
/// in it; curl reads a `.curlrc` there, wget a `.wgetrc`, and curl, wget
/// and git a `.netrc`, none with an ownership check. The doors pass `-q`
/// now, so the planted file is no longer read by them; nothing in the
/// estate writes any of the three, so ONE existing is the finding itself,
/// named by path every pass — and a home without them names nothing.
#[test]
fn a_curlrc_netrc_or_wgetrc_in_the_dev_home_is_a_red_problem() {
    let root = boss_testing::scratch_dir("boss-dsr-home-client-config");
    let _guard = Scratch(root.clone());
    let _yard = Yard::new(&root);
    let home = root.join("work/home");
    boss_testing::create_dir(&home);
    for name in [".curlrc", ".netrc", ".wgetrc"] {
        boss_testing::write_file(&home.join(name), "# a fixture, planted\n");
    }

    let out = run(&root, &[]);
    let text = say(&out);
    let put = run_step_put(&root, &text);
    let output = recorded_output(&put);
    let red: Vec<&str> = output
        .lines()
        .filter(|l| l.starts_with("RED dev-scratch-reclaim/home-client-config problem: "))
        .collect();
    assert_eq!(
        red.len(),
        1,
        "one RED line names the home's client config\n{output}\n{text}"
    );
    for name in [".curlrc", ".netrc", ".wgetrc"] {
        assert!(
            red[0].contains(&format!("{name} (uid ")),
            "{name} is named with its owner\n{output}"
        );
    }

    // A home that holds none of them names nothing.
    for name in [".curlrc", ".netrc", ".wgetrc"] {
        std::fs::remove_file(home.join(name)).expect("remove the fixture");
    }
    let _ = std::fs::remove_file(root.join("curl-log.txt"));
    let out = run(&root, &[]);
    let text = say(&out);
    assert!(
        !curl_log(&root).contains("home-client-config"),
        "a home without a client config files no problem for it\n{text}"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("dev home's client config: none"),
        "and the pass says it looked\n{text}"
    );
}

/// THE DEV HOME'S GIT CONFIG IS HELD BY ITS OWNER (backlog aa1f7d7c).
/// /work/home/.gitconfig was root:1500 0664 on the pod, so a group member
/// could set a credential helper, core.fsmonitor, core.sshCommand or an
/// include, and root's git — HOME=/work/home — would run it. The pass
/// takes group and other write off it every hour, as its owner; one it
/// cannot hold — a link, another uid's — is the RED home-client-config
/// line. Its contents are never read.
#[test]
fn the_dev_homes_gitconfig_is_held_by_its_owner_and_named_when_it_cannot_be() {
    use std::os::unix::fs::PermissionsExt;
    let mode_of = |p: &Path| {
        std::fs::symlink_metadata(p)
            .expect("stat the fixture")
            .permissions()
            .mode()
            & 0o7777
    };
    let root = boss_testing::scratch_dir("boss-dsr-home-gitconfig");
    let _guard = Scratch(root.clone());
    let _yard = Yard::new(&root);
    let home = root.join("work/home");
    boss_testing::create_dir(&home);
    let config = home.join(".gitconfig");
    boss_testing::write_file(&config, "# a fixture git config\n");
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o664))
        .expect("open the fixture to the group");

    let out = run(&root, &[]);
    let text = say(&out);
    assert_eq!(
        mode_of(&config),
        0o644,
        "group and other write come off the dev home's git config\n{text}"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains(&format!(
            "root's git config: took group and other write off {}",
            config.display()
        )),
        "the pass says what it held\n{text}"
    );
    assert!(
        !curl_log(&root).contains("home-client-config"),
        "a config the pass could hold is held, not a problem\n{text}"
    );

    // Held already: nothing to hold, and the pass says so.
    let out = run(&root, &[]);
    let text = say(&out);
    assert!(
        String::from_utf8_lossy(&out.stdout)
            .contains(&format!("root's git config: held ({}", config.display())),
        "{text}"
    );

    // A link where the config belongs — what a member leaves after
    // moving the real one aside — is named, and nothing is held through it.
    let aside = root.join("work/planted-gitconfig");
    std::fs::rename(&config, &aside).expect("move the config aside");
    std::fs::set_permissions(&aside, std::fs::Permissions::from_mode(0o664))
        .expect("open the moved config");
    std::os::unix::fs::symlink(&aside, &config).expect("leave a link in its place");
    let _ = std::fs::remove_file(root.join("curl-log.txt"));
    let out = run(&root, &[]);
    let text = say(&out);
    assert_eq!(
        mode_of(&aside),
        0o664,
        "the pass holds nothing through a link\n{text}"
    );
    let put = run_step_put(&root, &text);
    let output = recorded_output(&put);
    assert!(
        output.lines().any(|l| l
            .starts_with("RED dev-scratch-reclaim/home-client-config problem: ")
            && l.contains(".gitconfig (")
            && l.contains("symbolic link")),
        "the link is named on the packet\n{output}\n{text}"
    );
}

/// The one global git config the dev container's git reads, read from
/// the manifest that sets it: `GIT_CONFIG_GLOBAL` in the dev container's
/// env (infra/cluster/manifests/boss-dev.yaml), returned as the path
/// under /work, the mount the reclaim sidecar shares.
fn container_git_config_global_under_work() -> String {
    let text = std::fs::read_to_string(repo_root().join("infra/cluster/manifests/boss-dev.yaml"))
        .expect("read boss-dev.yaml");
    let values: Vec<String> = text
        .lines()
        .filter(|l| l.contains("name: GIT_CONFIG_GLOBAL,"))
        .map(|l| {
            l.split_once("value:")
                .map(|(_, v)| v.trim().trim_end_matches('}').trim().trim_matches('"'))
                .unwrap_or("")
                .to_string()
        })
        .collect();
    assert!(
        !values.is_empty() && values.iter().all(|v| v == &values[0]),
        "boss-dev.yaml sets GIT_CONFIG_GLOBAL to one value: {values:?}"
    );
    let rest = values[0].strip_prefix("/work/").unwrap_or_else(|| {
        panic!(
            "GIT_CONFIG_GLOBAL={} is not under /work, the mount the reclaim \
             sidecar shares with the dev container, so the pass cannot hold \
             it: move the hold with it (backlog aa1f7d7c)",
            values[0]
        )
    });
    rest.to_string()
}

/// ROOT'S GIT READS /work/.gitconfig, NOT THE HOME'S (backlog aa1f7d7c;
/// review ef57ab2e of car 335ace12, 2026-10-22). The dev container sets
/// GIT_CONFIG_GLOBAL, and with it set git ignores $HOME/.gitconfig: the
/// file every kubectl exec and every process in the container's tree
/// reads was root:1500 0664 and held by nothing. The fixture is planted
/// where the MANIFEST says the variable points — not where the script
/// says — so a manifest that moves it fails here rather than leaving the
/// pass holding the old file (CLAUDE.md 9a). $HOME/.config/git/config,
/// git's other global, is held the same way. A link at either is named
/// on the RED line by its full path, and nothing is held through it.
/// Red before: e9802b62 held $DEV_HOME/.gitconfig alone, so both
/// fixtures stayed 0664.
#[test]
fn the_container_git_config_global_is_held_by_its_owner() {
    use std::os::unix::fs::PermissionsExt;
    let mode_of = |p: &Path| {
        std::fs::symlink_metadata(p)
            .expect("stat the fixture")
            .permissions()
            .mode()
            & 0o7777
    };
    let root = boss_testing::scratch_dir("boss-dsr-container-gitconfig");
    let _guard = Scratch(root.clone());
    let _yard = Yard::new(&root);
    let global = root
        .join("work")
        .join(container_git_config_global_under_work());
    let xdg = root.join("work/home/.config/git/config");
    for p in [&global, &xdg] {
        boss_testing::create_dir(p.parent().expect("a parent"));
        boss_testing::write_file(p, "# a fixture git config\n");
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o664))
            .expect("open the fixture to the group");
    }

    let out = run(&root, &[]);
    let text = say(&out);
    for p in [&global, &xdg] {
        assert_eq!(
            mode_of(p),
            0o644,
            "group and other write come off {}\n{text}",
            p.display()
        );
        assert!(
            String::from_utf8_lossy(&out.stdout).contains(&format!(
                "root's git config: took group and other write off {}",
                p.display()
            )),
            "the pass names {} as held\n{text}",
            p.display()
        );
    }
    assert!(
        !curl_log(&root).contains("home-client-config"),
        "configs the pass could hold are held, not a problem\n{text}"
    );

    // A link where the container's global config belongs is named by its
    // full path, and nothing is held through it.
    let aside = root.join("work/planted-global-gitconfig");
    std::fs::rename(&global, &aside).expect("move the config aside");
    std::fs::set_permissions(&aside, std::fs::Permissions::from_mode(0o664))
        .expect("open the moved config");
    std::os::unix::fs::symlink(&aside, &global).expect("leave a link in its place");
    let _ = std::fs::remove_file(root.join("curl-log.txt"));
    let out = run(&root, &[]);
    let text = say(&out);
    assert_eq!(
        mode_of(&aside),
        0o664,
        "the pass holds nothing through a link\n{text}"
    );
    let put = run_step_put(&root, &text);
    let output = recorded_output(&put);
    assert!(
        output.lines().any(|l| l
            .starts_with("RED dev-scratch-reclaim/home-client-config problem: ")
            && l.contains(&format!("{} (", global.display()))
            && l.contains("symbolic link")),
        "the link is named on the packet by its full path\n{output}\n{text}"
    );
}

/// `argv` as uid 65534 — the gate's uid, read from
/// infra/gate-runner/gate-runner.yaml by infra/dev/as-gate-uid.sh, which
/// drops the same way — in group `gid` and no other: a member of the
/// checkout's group that does not own it, which is what every builder is
/// to the pod's root:1500 checkout.
fn as_a_member_of_the_checkouts_group(gid: u32, argv: &[&str]) -> Output {
    Command::new("setpriv")
        .args(["--reuid=65534", &format!("--regid={gid}"), "--clear-groups"])
        .arg("--")
        .args(argv)
        .output()
        .expect("setpriv")
}

/// The finding itself, reproduced and then refused: a uid in the
/// checkout's group that does not own it replaces a door, rewrites the
/// token's address and git's config and drops a hook, until the pass has
/// held the checkout; then it can do none of that, a builder's worktree
/// stays writable to it, and the hook it planted is a problem by name.
///
/// The pod has no CAP_CHOWN, so the fixture cannot be staged in group
/// 1500 as the pod's is; it keeps the group root created it with, and
/// the uid drops into THAT group. The kernel's rule is the same for any
/// group. Root is needed to drop to another uid at all, which the gate
/// (uid 65534) is not: there the test says NOT RUN on stderr and the
/// mode pins above are the gate's evidence.
#[test]
fn a_group_member_cannot_replace_what_root_runs_from_the_checkout_once_the_pass_holds_it() {
    use std::os::unix::fs::MetadataExt;
    let uid = Command::new("id").arg("-u").output().expect("id -u");
    if String::from_utf8_lossy(&uid.stdout).trim() != "0" {
        eprintln!(
            "NOT RUN: dropping to another uid needs root; the mode pins above cover the gate"
        );
        return;
    }
    let root = boss_testing::scratch_dir("boss-dsr-checkout-member");
    let _guard = Scratch(root.clone());
    let (yard, _held, kept) = shared_checkout(&root);
    chmod_checkout(&root, 0o755);
    let repo = yard.repo.clone();
    let gid = std::fs::metadata(&repo).expect("stat repo").gid();
    let s = |p: &Path| p.to_str().expect("utf8").to_string();
    let door = s(&repo.join("infra/dev/boss"));
    let aside = s(&repo.join("infra/dev/boss.aside"));
    let sor_url = s(&repo.join("infra/dev/sor-url"));
    let config = s(&repo.join(".git/config"));
    let hook = s(&repo.join(".git/hooks/post-merge"));
    let in_worktree = s(&kept[0].0.join("bundle.js"));
    let as_member = |what: &str, argv: &[&str]| {
        (
            what.to_string(),
            as_a_member_of_the_checkouts_group(gid, argv),
        )
    };
    let append = "printf x >> \"$1\"";

    // The finding, reproduced on the unheld checkout.
    for (what, out) in [
        as_member(
            "rename a door aside",
            &["mv", "-T", door.as_str(), aside.as_str()],
        ),
        as_member("and back", &["mv", "-T", aside.as_str(), door.as_str()]),
        as_member(
            "rewrite the token's address",
            &["sh", "-c", append, "sh", sor_url.as_str()],
        ),
        as_member(
            "rewrite git's config",
            &["sh", "-c", append, "sh", config.as_str()],
        ),
        as_member("drop a hook root's git runs", &["touch", hook.as_str()]),
    ] {
        assert!(
            out.status.success(),
            "control: a group member can {what} before the pass\n{}",
            say(&out)
        );
    }

    // The pass holds every mode it owns, and cannot hold the hook it
    // does not (the sidecar has no CAP_CHOWN): a problem, by name.
    let out = run_in_shared_checkout(&root);
    let text = say(&out);
    assert_eq!(
        out.status.code(),
        Some(1),
        "a foreign entry in the checkout reds the pass\n{text}"
    );
    let log = curl_log(&root);
    let put = log
        .lines()
        .rfind(|l| is_run_step_merge(l))
        .unwrap_or_else(|| panic!("the pass records itself\n{log}\n{text}"));
    let output = recorded_output(put);
    assert!(
        output.lines().any(
            |l| l.starts_with("RED dev-scratch-reclaim/checkout-open problem: ")
                && l.contains(".git/hooks/post-merge is owned by uid 65534")
        ),
        "the packet names the planted hook and its owner\n{output}"
    );
    std::fs::remove_file(&hook).expect("the owner of .git/hooks removes the planted hook");

    for (what, out) in [
        as_member(
            "rename a door aside",
            &["mv", "-T", door.as_str(), aside.as_str()],
        ),
        as_member(
            "rewrite the token's address",
            &["sh", "-c", append, "sh", sor_url.as_str()],
        ),
        as_member(
            "rewrite git's config",
            &["sh", "-c", append, "sh", config.as_str()],
        ),
        as_member("drop a hook root's git runs", &["touch", hook.as_str()]),
    ] {
        assert!(
            !out.status.success(),
            "a group member could {what} after the pass held the checkout\n{}",
            say(&out)
        );
    }
    let (what, out) = as_member("build in a worktree", &["touch", in_worktree.as_str()]);
    assert!(
        out.status.success(),
        "a group member can still {what}: the worktrees' contents are left as they were\n{}",
        say(&out)
    );
}

// ---------------------------------------------------------------------
// The floor sits ABOVE the kubelet's eviction line, and under it the
// idle builders' targets go first, least recently used.
//
// Measured 2026-09-23 from the `free_gb` every gate receipt records for
// w-1: the node drained from ~600 GB free to ~140 GB over 8–12 hours,
// the kubelet evicted the dev pod at its 15% line (~139 GiB), and free
// space jumped straight back to ~600 GB — six evictions in eight days.
// What refilled it was the builders' per-worktree targets on /scratch.
// This pass never acted: its floor was 50 GB, ninety below the line the
// kubelet enforces, so the eviction always came first; and even under
// the floor it trimmed only the primary target's incremental cache,
// never the siblings that held the bulk. The floor is now a SHARE of
// the filesystem, as the kubelet's own threshold is, and under it the
// sibling targets are reclaimed oldest-touched first until it is met,
// sparing any touched within the live window — the mtime is the
// liveness check per dir, as in the stale-target pass.
// ---------------------------------------------------------------------

/// A cargo-shaped target holding `mib` MiB of real (non-sparse) bytes.
fn sized_target(root: &Path, name: &str, hours_ago: u64, mib: usize) -> PathBuf {
    let d = target_dir(root, name, hours_ago);
    let blob = d.join("debug").join("deps").join("libbig.rlib");
    std::fs::write(&blob, vec![7u8; mib * 1024 * 1024]).expect("write blob");
    for p in [
        blob,
        d.join("debug").join("deps"),
        d.join("debug"),
        d.clone(),
    ] {
        touch_at(&p, hours_ago);
    }
    d
}

fn du_kb(path: &Path) -> u64 {
    let out = Command::new("du")
        .arg("-sk")
        .arg(path)
        .output()
        .expect("du");
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .and_then(|n| n.parse().ok())
        .expect("du -sk prints a number")
}

/// A `df` that reports the scratch fixture as a filesystem of
/// `STUB_DF_SIZE_GB` whose free space is `STUB_DF_CAP_KB` minus what
/// the fixture holds now, with each fixture MiB read as one GB — so a
/// reclaim of a 3 MiB target shows as 3 GB freed. `STUB_DF_WORK_GB`
/// sets the /work fixture's free space in whole GB. Any other path is
/// roomy, so neither floor fires unless a test asks — `run_with`
/// installs this for every test, because the /work floor now sits at
/// the gate's floor plus a margin (infra/build-floor.env), and the
/// real volume a test host's fixtures land on may well be under it.
fn stub_df(bin: &Path) {
    boss_testing::write_exec(
        &bin.join("df"),
        concat!(
            "#!/usr/bin/env bash\n",
            "m=\"${@: -1}\"\n",
            "if [ \"$m\" = \"${STUB_DF_SCRATCH:-}\" ]; then\n",
            "    used=$(du -sk \"$m\" | cut -f1)\n",
            "    free=$(( (STUB_DF_CAP_KB - used) * 1024 ))\n",
            "    size=$(( STUB_DF_SIZE_GB * 1024 * 1024 ))\n",
            "elif [ -n \"${STUB_DF_WORK_GB:-}\" ] && [ \"$m\" = \"$WORK_MOUNT\" ]; then\n",
            "    free=$(( STUB_DF_WORK_GB * 1024 * 1024 )); size=$(( 40 * 1024 * 1024 ))\n",
            "else\n",
            "    free=$(( 1 << 40 )); size=$(( 1 << 41 ))\n",
            "fi\n",
            "echo 'Filesystem 1024-blocks Used Available Capacity Mounted on'\n",
            "echo \"stub $size 0 $free 0% $m\"\n",
        ),
    );
}

#[test]
fn under_the_floor_idle_sibling_targets_go_oldest_first_until_it_is_met() {
    let root = boss_testing::scratch_dir("boss-dsr-floor-lru");
    let _guard = Scratch(root.clone());
    boss_testing::create_dir(&root.join("work").join("wt"));
    boss_testing::create_dir(&root.join("work").join("repo"));
    let bin = stub_curl(&root);
    stub_df(&bin);
    let primary = target_dir(&root, "target", 0);
    let oldest = sized_target(&root, "target-idle-5h", 5, 3);
    let older = sized_target(&root, "target-idle-3h", 3, 3);
    let newer = sized_target(&root, "target-idle-1h", 1, 3);
    let live = sized_target(&root, "target-building-now", 0, 3);

    // A 20 GB filesystem with a 50% floor = 10 GB; 5.5 GB free now.
    // Removing the 5h target frees 3 (8.5, still under); the 3h one
    // frees 3 more (11.5, met) — and there the pass must stop.
    let cap = du_kb(&root) + 5 * 1024 + 512;
    let out = run(
        &root,
        &[
            ("BOSS_SCRATCH_FLOOR_PCT", "50"),
            ("STUB_DF_SCRATCH", root.to_str().expect("utf-8 path")),
            ("STUB_DF_CAP_KB", &cap.to_string()),
            ("STUB_DF_SIZE_GB", "20"),
        ],
    );
    let text = say(&out);
    assert!(
        !oldest.exists(),
        "the least recently used idle target goes first\n{text}"
    );
    assert!(
        !older.exists(),
        "the next oldest goes while still under the floor\n{text}"
    );
    assert!(
        newer.exists(),
        "the pass stops the moment the floor is met\n{text}"
    );
    assert!(
        live.exists(),
        "a target touched inside the live window belongs to a build\n{text}"
    );
    assert!(
        primary.exists(),
        "the primary target is the incremental trim's, never removed whole\n{text}"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("target-idle-5h") && stdout.contains("target-idle-3h"),
        "each reclaimed target is named\n{text}"
    );
    let log = curl_log(&root);
    let put = log
        .lines()
        .find(|l| is_run_step_merge(l))
        .unwrap_or_else(|| panic!("an acting pass records itself\n{log}\n{text}"));
    assert!(
        put.contains("\"floor_targets_reclaimed\":\"2\""),
        "the packet counts what the floor pass took\n{put}\n{text}"
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "a met floor is a clean pass\n{text}"
    );
}

#[test]
fn a_floor_share_that_is_not_a_percentage_is_refused() {
    let root = boss_testing::scratch_dir("boss-dsr-badpct");
    let _guard = Scratch(root.clone());
    for bad in ["lots", "101"] {
        let out = run(&root, &[("BOSS_SCRATCH_FLOOR_PCT", bad)]);
        assert_eq!(out.status.code(), Some(64), "{bad}: {}", say(&out));
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("SCRATCH_FLOOR_PCT"),
            "{}",
            say(&out)
        );
    }
}

/// UNSET, THE SCRATCH FLOOR IS THE RULE infra/build-floor.env DEFINES
/// (backlog 4cc39798, 2026-10-20). The script spelled its own 25% —
/// Longhorn's refusal line exactly, under the estate's 27.5% disk_tight
/// — so on w-1 the pass logged "251GB free >= 232GB floor" with the
/// alarm three readings old. The order of the two floors is pinned in
/// the_build_floor_lives_once.rs; this drives the script with the knob
/// REMOVED, in the band that order is about — 2.5 GB under the file's
/// floor, above the old 25% — on two disks, one for each term of the
/// rule: on 100 GB the capped headroom share is the higher (the estate's
/// 35% rules there), on 1000 GB the plain share is (Longhorn's 27.5%
/// rules there, as on w-1).
#[test]
fn unset_the_scratch_floor_is_the_rule_the_build_floor_file_defines() {
    let pct = build_floor("SCRATCH_RECLAIM_FLOOR_PCT");
    let headroom_pct = build_floor("SCRATCH_RECLAIM_HEADROOM_PCT");
    let ceiling = build_floor("SCRATCH_RECLAIM_HEADROOM_CEILING_GB");
    let rule = format!("{pct}%, or {headroom_pct}% up to {ceiling}GB");
    for (disk_gb, headroom_rules) in [(100_u64, true), (1000, false)] {
        let share = disk_gb * pct / 100;
        let headroom = (disk_gb * headroom_pct / 100).min(ceiling);
        assert_eq!(
            headroom > share,
            headroom_rules,
            "on {disk_gb} GB the fixture expects the {} term to rule ({rule})",
            if headroom_rules { "headroom" } else { "share" }
        );
        let floor = share.max(headroom);
        assert!(
            floor >= disk_gb / 4 + 3,
            "the fixture's band needs a floor above the old 25% on {disk_gb} GB"
        );
        let root = boss_testing::scratch_dir("boss-dsr-floor-from-file");
        let _guard = Scratch(root.clone());
        boss_testing::create_dir(&root.join("work").join("wt"));
        boss_testing::create_dir(&root.join("work").join("repo"));
        let bin = stub_curl(&root);
        stub_df(&bin);
        let primary = target_dir(&root, "target", 0);
        let idle = sized_target(&root, "target-idle-2h", 2, 5);
        // Free is 2.5 GB under the floor, and the 5 MiB (= 5 GB, see
        // stub_df) idle target meets it.
        let cap = du_kb(&root) + floor * 1024 - 2 * 1024 - 512;
        let mut cmd = command_with(
            &root,
            &["--scratch-floor"],
            &[
                ("STUB_DF_SCRATCH", root.to_str().expect("utf-8 path")),
                ("STUB_DF_CAP_KB", &cap.to_string()),
                ("STUB_DF_SIZE_GB", &disk_gb.to_string()),
            ],
        );
        cmd.env_remove("BOSS_SCRATCH_FLOOR_PCT");
        let out = cmd.output().expect("run dev-scratch-reclaim.sh");
        let text = say(&out);
        assert_eq!(out.status.code(), Some(0), "{text}");
        assert!(
            String::from_utf8_lossy(&out.stdout).contains(&format!("{floor}GB floor ({rule})")),
            "on {disk_gb} GB the pass applies the infra/build-floor.env rule ({rule}), \
             {floor} GB\n{text}"
        );
        assert!(
            !idle.exists(),
            "under the file's floor the idle sibling is taken\n{text}"
        );
        assert!(primary.exists(), "the primary is never removed\n{text}");
    }
}

// ---------------------------------------------------------------------
// THE FLOOR FOLLOWS THE BUILD (backlog 3f2a08ab, 2026-09-24).
// ---------------------------------------------------------------------
// The dev pod was evicted at 01:27Z with the 25% floor in force. The
// gate receipts' free_gb (GiB free on w-1's ephemeral xfs, read at each
// verdict) date the drain: 487 at 21:47Z, 378 at 00:01, 284 at 00:53,
// 218 at 01:14, 159 at 01:20, 136 at the eviction, then 567 at 01:31 —
// ~430 GiB of it the dev pod's own /scratch. The hourly passes at
// 21:58, 22:59 and 23:59 took no floor target, and the ~01:00 pass
// filed nothing, so it found the floor met: from above 232 GiB to 136
// in under 27 minutes, against a 93 GiB margin and a 60-minute timer.
// So `wt-cargo` — the event that fills /scratch — runs the floor's
// sibling pass itself before each build, and this mode is that pass
// alone: one df above the floor, the idle siblings below it, and none
// of the hourly pass's other legs.

#[test]
fn the_scratch_floor_mode_takes_idle_siblings_and_runs_no_other_pass() {
    let root = boss_testing::scratch_dir("boss-dsr-floor-mode");
    let _guard = Scratch(root.clone());
    boss_testing::create_dir(&root.join("work").join("wt"));
    boss_testing::create_dir(&root.join("work").join("repo"));
    let bin = stub_curl(&root);
    stub_df(&bin);
    let primary = target_dir(&root, "target", 0);
    let oldest = sized_target(&root, "target-idle-5h", 5, 3);
    let older = sized_target(&root, "target-idle-3h", 3, 3);
    let newer = sized_target(&root, "target-idle-1h", 1, 3);
    let live = sized_target(&root, "target-building-now", 0, 3);

    // The same arithmetic as the hourly floor test above: two targets
    // take the fixture from 5.5 GB free to 11.5, over a 10 GB floor.
    let cap = du_kb(&root) + 5 * 1024 + 512;
    let out = run_with(
        &root,
        &["--scratch-floor"],
        &[
            ("BOSS_SCRATCH_FLOOR_PCT", "50"),
            ("STUB_DF_SCRATCH", root.to_str().expect("utf-8 path")),
            ("STUB_DF_CAP_KB", &cap.to_string()),
            ("STUB_DF_SIZE_GB", "20"),
        ],
    );
    let text = say(&out);
    assert!(
        !oldest.exists(),
        "the oldest idle target goes first\n{text}"
    );
    assert!(
        !older.exists(),
        "then the next, while under the floor\n{text}"
    );
    assert!(newer.exists(), "and it stops when the floor is met\n{text}");
    assert!(live.exists(), "a live target is a build's\n{text}");
    assert!(primary.exists(), "the primary is never removed\n{text}");
    assert!(
        install_log(&root).is_empty(),
        "the floor mode must not run the CLI leg — it runs before every build\n{text}"
    );
    assert!(
        ls_remote_calls(&root).is_empty(),
        "the floor mode must not touch git\n{text}"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("worktree pass") && !stdout.contains("stale-target pass"),
        "the floor mode runs the floor's sibling pass and nothing else\n{text}"
    );
    let log = curl_log(&root);
    let put = log
        .lines()
        .find(|l| is_run_step_merge(l))
        .unwrap_or_else(|| panic!("an acting floor pass records itself\n{log}\n{text}"));
    assert!(
        put.contains("\"floor_targets_reclaimed\":\"2\""),
        "the packet counts what the floor pass took\n{put}\n{text}"
    );
    assert!(
        put.contains("scratch-floor"),
        "the packet says which pass this was, so a reader can tell a build-triggered \
         pass from the hourly one\n{put}\n{text}"
    );
    assert_eq!(out.status.code(), Some(0), "a met floor is clean\n{text}");
}

#[test]
fn above_the_floor_the_scratch_floor_mode_is_silent_and_takes_nothing() {
    let root = boss_testing::scratch_dir("boss-dsr-floor-mode-quiet");
    let _guard = Scratch(root.clone());
    boss_testing::create_dir(&root.join("work").join("wt"));
    boss_testing::create_dir(&root.join("work").join("repo"));
    let primary = target_dir(&root, "target", 0);
    // Stale by the fixture's 12h — the hourly pass would take it. The
    // floor mode is not the age pass and must leave it alone.
    let stale = target_dir(&root, "target-landed-yesterday", 30);
    let out = run_with(&root, &["--scratch-floor"], &[]);
    let text = say(&out);
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert!(stale.exists(), "above the floor nothing is taken\n{text}");
    assert!(primary.exists(), "{text}");
    assert!(
        out.stdout.is_empty() && out.stderr.is_empty(),
        "it runs before every build, so above the floor it says nothing\n{text}"
    );
    assert!(
        curl_log(&root).is_empty(),
        "a pass that took nothing files nothing\n{text}"
    );
    assert!(install_log(&root).is_empty(), "{text}");
}

// ---------------------------------------------------------------------
// THE HOURLY PASS KEEPS THE MEMORY WATCH RUNNING (backlog 9ecd12ea).
// ---------------------------------------------------------------------
// The dev container was OOMKilled on 2026-10-08 and nothing had sampled
// it. The sampler (infra/cluster/dev-memory-watch.sh, pinned in
// dev_memory_watch_sh.rs) needs something to start it that is not the
// manifest — an edit there rolls the pod — and the hourly pass is what
// the sidecar already runs from the checkout. The build-triggered floor
// mode must NOT: it runs in the dev container before every build, where
// a sampler would die with the container it watches.

#[test]
fn the_hourly_pass_ensures_the_memory_watch_and_the_floor_mode_does_not() {
    let root = boss_testing::scratch_dir("boss-dsr-memory-watch");
    let _guard = Scratch(root.clone());
    boss_testing::create_dir(&root.join("work").join("wt"));
    boss_testing::create_dir(&root.join("work").join("repo"));
    target_dir(&root, "target", 0);

    let on = [("BOSS_DEV_MEMORY_WATCH", "on")];
    let out = run(&root, &on);
    let text = say(&out);
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("dev-memory-watch: not started"),
        "the hourly pass asks the watch to be running, and the watch says why it is not \
         (no service-account token in this fixture)\n{text}"
    );
    // Review of car 7849553a, finding 2: every way --ensure declines
    // returns 0, so its refusal reached only the sidecar's stdout. A
    // watch that is not sampling is now a PROBLEM, which files the
    // packet, and the packet carries the watch's state and its reason.
    assert_eq!(
        out.status.code(),
        Some(1),
        "a pass that leaves the dev container unsampled is not a clean pass\n{text}"
    );
    let log = curl_log(&root);
    let put = log
        .lines()
        .find(|l| is_run_step_merge(l))
        .unwrap_or_else(|| {
            panic!("a pass with an unsampled dev container records itself\n{log}\n{text}")
        });
    for want in [
        "\"memory_watch\":\"not sampling: no loop is alive\"",
        "\"memory_watch_ensure\":\"not started: no service-account token",
        "\"memory_watch_alerts_kept\":\"0\"",
        "\"memory_watch_alerts_refused\":\"0\"",
        "\"memory_watch_sample_age_s\":\"\"",
        "\"result\":\"incomplete\"",
    ] {
        assert!(put.contains(want), "the packet lacks {want}\n{put}\n{text}");
    }
    assert!(
        put.contains("\"problems\":\"1\""),
        "the unsampled watch is this fixture's one problem\n{put}"
    );

    // Delta review, N2: an alert the API refused until it was moved
    // aside rode the packet as a count but was never itself a reason
    // to file one — on a healthy pod it would have reached nobody.
    let aside = root
        .join("work")
        .join("telemetry")
        .join("dev-memory")
        .join("alert-refused");
    boss_testing::create_dir(&aside);
    boss_testing::write_file(&aside.join("1-1-1.json"), "{}");
    let out = run(&root, &on);
    let text = say(&out);
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("alerts that never reached the system of record"),
        "an alert that did not leave is said aloud\n{text}"
    );
    let log = curl_log(&root);
    let put = log
        .lines()
        .rfind(|l| is_run_step_merge(l))
        .unwrap_or_else(|| panic!("the pass records itself\n{log}\n{text}"));
    assert!(
        put.contains("\"memory_watch_alerts_refused\":\"1\"") && put.contains("\"problems\":\"2\""),
        "and counted as a problem of its own\n{put}\n{text}"
    );
    // The operator call recorded on 9ecd12ea at car 7849553a's release:
    // the problem stays every hour until a person acts, so it names the
    // act — a verb that keeps the bodies — and where the watch keeps them.
    let watch = repo_root().join("infra/cluster/dev-memory-watch.sh");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("dev-memory-watch.sh --acknowledge")
            && put.contains("dev-memory-watch.sh --acknowledge"),
        "the problem names the verb that clears it, aloud and on the packet\n{put}\n{text}"
    );
    assert!(
        put.contains(&format!(
            "\"memory_watch_dir\":\"{}\"",
            aside.parent().expect("the watch dir").display()
        )),
        "and the packet names the watch's directory\n{put}"
    );

    // Acknowledged, the next pass's packet carries who and when, and the
    // problem is gone; the pass after that no longer carries it.
    let ack = Command::new("bash")
        .arg(&watch)
        .arg("--acknowledge")
        .env("WORK_MOUNT", root.join("work"))
        .env("BOSS_ACTOR", "operator:test")
        .output()
        .expect("run --acknowledge");
    assert!(ack.status.success(), "{}", say(&ack));
    let out = run(&root, &on);
    let text = say(&out);
    let log = curl_log(&root);
    let put = log
        .lines()
        .rfind(|l| is_run_step_merge(l))
        .unwrap_or_else(|| panic!("the pass records itself\n{log}\n{text}"));
    assert!(
        put.contains("\"memory_watch_alerts_refused\":\"0\"")
            && put.contains("\"problems\":\"1\"")
            && put.contains("\"memory_watch_acknowledged\":\"")
            && put.contains("by operator:test: 1 refused"),
        "the acknowledgement rides the next packet\n{put}\n{text}"
    );
    let out = run(&root, &on);
    let text = say(&out);
    let log = curl_log(&root);
    let put = log
        .lines()
        .rfind(|l| is_run_step_merge(l))
        .unwrap_or_else(|| panic!("the pass records itself\n{log}\n{text}"));
    assert!(
        put.contains("\"memory_watch_acknowledged\":\"\""),
        "and only that one: a packet that carried it marks it recorded\n{put}\n{text}"
    );

    let out = run_with(&root, &["--scratch-floor"], &on);
    let text = say(&out);
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("dev-memory-watch"),
        "the floor mode runs before every build, inside the dev container\n{text}"
    );
}

/// Review fb97dd5d of car 1fe37401, finding F1. An answer that judges
/// the caller or the API (a 401 or 403 to the watch's signer, a 5xx)
/// counts nothing against a kept alert, so refused stays 0 and the
/// problem above never fires: a kill alert held that way was silent
/// until the spool's bound of 48. The watch now says how long its oldest
/// kept alert has waited and past what no refusal of the body explains
/// it (dev_memory_watch_sh.rs holds a 403 on one and reads both off
/// `--state`); held past that bound, it is a problem of its own here,
/// and inside it, it is not — a body the API refuses is still backing
/// off and will be moved aside and counted by the problem above.
#[test]
fn a_kill_alert_the_watch_holds_past_its_bound_is_a_problem_of_the_hourly_pass() {
    let root = boss_testing::scratch_dir("boss-dsr-memory-held");
    let _guard = Scratch(root.clone());
    boss_testing::create_dir(&root.join("work").join("wt"));
    boss_testing::create_dir(&root.join("work").join("repo"));
    target_dir(&root, "target", 0);
    let on = [("BOSS_DEV_MEMORY_WATCH", "on")];
    let spool = root
        .join("work")
        .join("telemetry")
        .join("dev-memory")
        .join("alert-spool");
    boss_testing::create_dir(&spool);
    let body = r#"{"kind":"backlog-item","title":"a held kill alert","owner_id":""}"#;

    // Kept ten minutes ago: inside any back-off, not yet a problem. The
    // fixture's unsampled watch is its one problem.
    let young = spool.join(format!("{}-1-1.json", unix_now() - 600));
    boss_testing::write_file(&young, body);
    let out = run(&root, &on);
    let text = say(&out);
    let log = curl_log(&root);
    let put = log
        .lines()
        .rfind(|l| is_run_step_merge(l))
        .unwrap_or_else(|| panic!("the pass records itself\n{log}\n{text}"));
    assert!(
        put.contains("\"problems\":\"1\"") && !put.contains("memory-watch-alerts-held"),
        "an alert kept ten minutes is not held past the bound\n{put}\n{text}"
    );
    assert!(
        put.contains("\"memory_watch_alerts_held_after_s\":\"3600\""),
        "the bound rides the packet beside the age\n{put}"
    );

    // Kept two hours ago and still in the spool: held.
    std::fs::remove_file(&young).expect("remove the young alert");
    boss_testing::write_file(&spool.join(format!("{}-1-1.json", unix_now() - 7200)), body);
    let out = run(&root, &on);
    let text = say(&out);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("held a kill alert unsent"),
        "a held alert is said aloud\n{text}"
    );
    let log = curl_log(&root);
    let put = log
        .lines()
        .rfind(|l| is_run_step_merge(l))
        .unwrap_or_else(|| panic!("the pass records itself\n{log}\n{text}"));
    assert!(
        put.contains("\"problems\":\"2\"")
            && put.contains("dev-scratch-reclaim/memory-watch-alerts-held problem")
            && put.contains("\"memory_watch_alerts_refused\":\"0\"")
            && put.contains("\"memory_watch_alerts_oldest_kept_age_s\":\"72"),
        "held past the bound with nothing counted, it is a problem of its own\n{put}\n{text}"
    );
    assert_eq!(out.status.code(), Some(1), "{text}");
}

/// Backlog 0143612a. A frozen ring an open item names is kept past the
/// rotation of the rest, up to a bound; past it the watch retires the
/// oldest and counts it (dev_memory_watch_sh.rs holds that half). A
/// count that rides only a packet nobody files is the silence this
/// replaces, so a retirement is a problem of the hourly pass until a
/// person acknowledges it.
#[test]
fn a_frozen_ring_retired_while_its_item_was_open_is_a_problem_of_the_hourly_pass() {
    let root = boss_testing::scratch_dir("boss-dsr-memory-retired");
    let _guard = Scratch(root.clone());
    boss_testing::create_dir(&root.join("work").join("wt"));
    boss_testing::create_dir(&root.join("work").join("repo"));
    target_dir(&root, "target", 0);
    let on = [("BOSS_DEV_MEMORY_WATCH", "on")];
    let watch_dir = root.join("work").join("telemetry").join("dev-memory");
    boss_testing::create_dir(&watch_dir);
    boss_testing::write_file(&watch_dir.join("kills-retired"), "1\n");
    boss_testing::write_file(
        &watch_dir.join("kills-retired.log"),
        "2026-10-18T00:00:00Z retired dev-x (item i-1 1): more than 20 held\n",
    );
    let out = run(&root, &on);
    let text = say(&out);
    let log = curl_log(&root);
    let put = log
        .lines()
        .rfind(|l| is_run_step_merge(l))
        .unwrap_or_else(|| panic!("the pass records itself\n{log}\n{text}"));
    assert!(
        put.contains("\"problems\":\"2\"")
            && put.contains("dev-scratch-reclaim/memory-watch-rings-retired problem")
            && put.contains("\"memory_watch_rings_retired\":\"1\""),
        "a retired ring is a problem of its own\n{put}\n{text}"
    );
    assert_eq!(out.status.code(), Some(1), "{text}");
}

// ---------------------------------------------------------------------
// THE /work FLOOR IS THE GATE'S FLOOR PLUS A MARGIN (backlog 99ce8744).
// ---------------------------------------------------------------------
// Measured 2026-09-24 ~23:00Z: /work at 12 GB free of 40. The gate
// refused below 12 and this pass acted only below 6, so it never fired
// before a builder's pre-flight was refused, and two builders lowered
// the gate's floor by hand. Both now read infra/build-floor.env, and
// the floor here is DERIVED — the gate's plus a margin — so the
// automatic pass acts first. The gate's half of the pin is in
// the_build_floor_lives_once.rs.

/// The value of `name` in `infra/build-floor.env`, read the way the
/// scripts read it: the last `NAME=<number>` line.
fn build_floor(name: &str) -> u64 {
    let text = std::fs::read_to_string(repo_root().join("infra/build-floor.env"))
        .expect("infra/build-floor.env is the one definition of the build floor");
    text.lines()
        .rev()
        .find_map(|l| l.strip_prefix(&format!("{name}=")))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or_else(|| panic!("infra/build-floor.env defines {name} as a whole number"))
}

fn work_floor() -> u64 {
    build_floor("GATE_MIN_FREE_GB") + build_floor("WORK_RECLAIM_MARGIN_GB")
}

#[test]
fn the_work_floor_is_the_gates_floor_plus_the_margin_and_the_retired_knob_moves_nothing() {
    let floor = work_floor();
    for (free, below) in [(floor - 1, true), (floor, false)] {
        let root = boss_testing::scratch_dir("boss-dsr-work-floor");
        let _guard = Scratch(root.clone());
        let _yard = Yard::new(&root);
        let out = run(
            &root,
            &[
                ("STUB_DF_WORK_GB", &free.to_string()),
                // The live sidecar still sets this (boss-dev.yaml) and a
                // manifest edit rolls the pod, so the script must be the
                // one that stops reading it — else the 6 it names keeps
                // the reclaim below the gate for as long as the pod runs.
                ("BOSS_WORK_FLOOR_GB", "6"),
            ],
        );
        let text = say(&out);
        let stdout = String::from_utf8_lossy(&out.stdout);
        if below {
            assert!(
                stdout.contains(&format!("{free}GB free < {floor}GB floor")),
                "one GB under the gate's floor plus the margin, the /work pass fires\n{text}"
            );
        } else {
            assert!(
                stdout.contains(&format!("{free}GB free >= {floor}GB floor")),
                "at the derived floor the /work pass is met\n{text}"
            );
        }
    }
}

/// Under the /work floor a LANDED worktree waits only the live window
/// the floor's target pass keeps (BOSS_LIVE_TARGET_MIN), not the 3h
/// grace — "the grace yields to the floor, as it already does for
/// targets". A freshly cut tree (its head still on origin/main, so it
/// reads as landed) is inside the live window and is kept.
#[test]
fn under_the_work_floor_a_landed_worktree_waits_only_the_live_window() {
    let floor = work_floor();
    for under in [true, false] {
        let root = boss_testing::scratch_dir("boss-dsr-work-floor-grace");
        let _guard = Scratch(root.clone());
        let yard = Yard::new(&root);
        let landed = yard.worktree("agent-landed-2h", Some("feat/landed"), 2);
        yard.land("feat/landed");
        let fresh = root.join("work").join("wt").join("agent-fresh");
        git(
            &yard.repo,
            0,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "feat/fresh",
                fresh.to_str().expect("utf8"),
                "main",
            ],
        );
        let free = if under { floor - 1 } else { floor + 10 };
        let out = run(&root, &[("STUB_DF_WORK_GB", &free.to_string())]);
        let text = say(&out);
        if under {
            assert!(
                !landed.exists(),
                "under the /work floor a landed tree idle 2h goes — the 3h grace yields\n{text}"
            );
            assert!(
                String::from_utf8_lossy(&out.stdout).contains("under the /work floor"),
                "the removal says it was the floor that shortened the wait\n{text}"
            );
        } else {
            assert!(
                landed.exists(),
                "above the floor a landed tree keeps its 3h grace\n{text}"
            );
        }
        assert!(
            fresh.exists(),
            "a tree cut minutes ago is inside the live window, floor or not\n{text}"
        );
    }
}

/// THE GRACE IS THREE HOURS (backlog b4112350). Measured 2026-10-22
/// 00:05Z on the pod: /scratch, w-1's only Longhorn disk, held 369 GiB in
/// 35 live build targets, each tied to a worktree the 12h grace kept
/// after its run closed, and Longhorn could place 88 GiB there — under
/// one 100 GiB worker volume, so dev isolation admitted one builder.
/// With NEITHER knob set, a landed tree and a sibling target idle 4h are
/// taken and ones idle 2h are kept: this pins the script's default, which
/// every other test here overrides or never reaches.
#[test]
fn with_no_knob_set_the_grace_and_the_stale_target_age_are_three_hours() {
    for (idle_h, due) in [(2, false), (4, true)] {
        let root = boss_testing::scratch_dir("boss-dsr-three-hours");
        let _guard = Scratch(root.clone());
        let yard = Yard::new(&root);
        let landed = yard.worktree("agent-landed", Some("feat/landed"), idle_h);
        yard.land("feat/landed");
        let sibling = target_dir(&root, "target-landed-earlier", idle_h);
        let mut cmd = command_with(&root, &[], &[]);
        cmd.env_remove("BOSS_WORKTREE_GRACE_H")
            .env_remove("BOSS_STALE_TARGET_H");
        let out = cmd.output().expect("run dev-scratch-reclaim.sh");
        let text = say(&out);
        assert_eq!(
            landed.exists(),
            !due,
            "a landed, clean tree idle {idle_h}h against the default 3h grace\n{text}"
        );
        assert_eq!(
            sibling.exists(),
            !due,
            "a sibling target idle {idle_h}h against the default 3h stale age\n{text}"
        );
    }
}

/// LANDED IS JUDGED BY CONTENT, NOT BY SHA (backlog 4a738ca5). Measured
/// 2026-09-25 04:10Z: a pass under the /work floor removed 11 worktrees
/// and kept 286 as recent while /work sat at 12 GB free of 40 — and of
/// 58 sampled worktrees only 2 had a head on any origin ref. Builders
/// commit on local `worktree-agent-*` branches, `boss gate --rebase`
/// replays the commit onto a new sha, and trains squash-merge, so no sha
/// the pass read ever reached origin/main: every landed tree read as
/// UNPUSHED and waited the 48h window instead of the landed one.
///
/// Two landed shapes, one per rule `boss merged` already decides by
/// (crates/orchestrators/boss-cli/src/merged.rs): a car squashed into a
/// two-car train (its patch-id matches no commit on main, and every file
/// it changed is byte-identical there — CONTENT), and a car squashed
/// alone whose file a later car then edited (the bytes differ, the
/// patch-id matches — PATCH-ID). Beside them, a tree whose work did NOT
/// land, idle just as long, is kept.
#[test]
fn a_worktree_whose_work_landed_by_a_rebase_and_a_squash_is_landed_and_unlanded_work_is_kept() {
    let floor = work_floor();
    for under in [true, false] {
        let root = boss_testing::scratch_dir("boss-dsr-landed-by-content");
        let _guard = Scratch(root.clone());
        let yard = Yard::new(&root);
        let trained = yard.worktree_writing(
            "agent-trained",
            Some("worktree-agent-trained"),
            2,
            "trained.txt",
        );
        yard.train_land("worktree-agent-trained", true, false);
        let squashed = yard.worktree_writing(
            "agent-squashed",
            Some("worktree-agent-squashed"),
            2,
            "squashed.txt",
        );
        yard.train_land("worktree-agent-squashed", false, true);
        let unlanded = yard.worktree_writing(
            "agent-unlanded",
            Some("worktree-agent-unlanded"),
            2,
            "unlanded.txt",
        );
        for b in [
            "worktree-agent-trained",
            "worktree-agent-squashed",
            "worktree-agent-unlanded",
        ] {
            let head = git(&yard.repo, 0, &["rev-parse", &format!("refs/heads/{b}")]);
            assert!(
                !yard.on_origin_main(&head),
                "precondition: {b}'s head is on no origin ref, as on the pod"
            );
        }

        let free = if under { floor - 1 } else { floor + 10 };
        let out = run(&root, &[("STUB_DF_WORK_GB", &free.to_string())]);
        let text = say(&out);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            unlanded.exists(),
            "work that is not on origin/main by sha, patch-id or content is unpushed: \
             idle 2h against 48h, kept, floor or not\n{text}"
        );
        if under {
            assert!(
                !trained.exists(),
                "squashed into a two-car train after a rebase: every file it changed is on \
                 origin/main, so it is landed and waits only the live window\n{text}"
            );
            assert!(
                !squashed.exists(),
                "squashed alone, then edited by a later car: its patch is on origin/main, \
                 so it is landed\n{text}"
            );
            assert!(
                stdout.contains("worktree-agent-trained")
                    && stdout.contains("on origin/main by content"),
                "the removal says it was the content that showed the landing\n{text}"
            );
            assert!(
                stdout.contains("worktree-agent-squashed")
                    && stdout.contains("on origin/main by patch-id"),
                "and which rule showed it\n{text}"
            );
            let log = curl_log(&root);
            let put = log
                .lines()
                .find(|l| is_run_step_merge(l))
                .unwrap_or_else(|| panic!("the run step is completed\n{log}\n{text}"));
            assert!(
                put.contains("\"worktrees_removed_by_content\":\"2\""),
                "the packet counts the trees only content could call landed\n{put}\n{text}"
            );
        } else {
            assert!(
                trained.exists() && squashed.exists(),
                "above the floor a tree landed by content keeps the 3h grace, as one \
                 landed by sha does\n{text}"
            );
        }
    }
}

// ---------------------------------------------------------------------
// LANDED ON RECORD (backlog 9a044141). Measured 2026-09-25 16:15-16:40Z:
// /work at 14 GB free against its 18 GB floor, 297 worktrees under
// .claude/worktrees, 253 of them a named branch with no origin/ ref and
// a head on no origin ref — and `landed_by_content` called 0 of the 218
// idle ones landed, because a train squashes N cars into one commit,
// `boss gate --rebase` replays the car, and a later car edits the same
// files. So every finished builder's tree waited the 48h unpushed
// window, even under the floor, where a landed one waits 30 minutes.
// The system of record holds the answer git cannot see: the gate-runs
// launched from a worktree (flat `worktree`, `branch`, `agent_run`), the
// agent-run each names and whether it finished, and the car on a branch
// with the conductor's `merged` stamp. These tests write the refusals
// first — a dirty tree, a tree with no landing on record, and a record
// that cannot be read — because this car changes what gets deleted.
// ---------------------------------------------------------------------

/// What the stub system of record holds (see `stub_curl`).
fn write_record(root: &Path, doc: serde_json::Value) {
    boss_testing::write_file(&root.join("record.json"), &doc.to_string());
}

fn path_str(p: &Path) -> String {
    p.to_str().expect("utf8").to_string()
}

/// One GB under the /work floor: a landed tree waits only the live
/// window, so a tree idle 2h is due the moment anything calls it landed.
fn under_the_work_floor() -> String {
    (work_floor() - 1).to_string()
}

/// A tree past WORKTREE_MAX_AGE_H with nothing on record: the pass takes
/// it without asking anyone, so every test here files its packet and the
/// counts can be read off the run step.
fn abandoned_tree(yard: &Yard) -> PathBuf {
    yard.worktree("agent-abandoned", Some("feat/abandoned"), 60)
}

/// The run step's completion, as the stub saw it.
fn run_step_put(root: &Path, text: &str) -> String {
    let log = curl_log(root);
    log.lines()
        .find(|l| is_run_step_merge(l))
        .unwrap_or_else(|| panic!("the run step is completed\n{log}\n{text}"))
        .to_string()
}

/// Make a tree dirty — an edit and an untracked file — dated like the
/// tree itself, so it is the dirt and not recency that keeps it.
fn dirty(tree: &Path, hours_ago: u64) {
    boss_testing::write_file(&tree.join("work.txt"), "edited, not committed");
    boss_testing::write_file(&tree.join("notes.txt"), "untracked");
    for p in [
        tree.join("work.txt"),
        tree.join("notes.txt"),
        tree.to_path_buf(),
    ] {
        touch_at(&p, hours_ago);
    }
}

/// REFUSAL 1. A landing on record makes a tree DUE; it never makes a
/// dirty one removable. The record only shortens the wait — every guard
/// after it still runs, and `git worktree remove` still has no --force.
#[test]
fn a_dirty_worktree_whose_run_finished_on_record_is_kept_and_named() {
    let root = boss_testing::scratch_dir("boss-dsr-record-dirty");
    let _guard = Scratch(root.clone());
    let yard = Yard::new(&root);
    let tree = yard.worktree("agent-dirty", Some("fix/dirty"), 2);
    dirty(&tree, 2);
    abandoned_tree(&yard);
    write_record(
        &root,
        serde_json::json!({
            "gate_runs": [{"worktree": path_str(&tree), "branch": "fix/dirty", "agent_run": "run0dirt"}],
            "runs": {"run0dirt": "closed"},
        }),
    );

    let out = run(&root, &[("STUB_DF_WORK_GB", &under_the_work_floor())]);
    let text = say(&out);
    assert!(
        tree.exists(),
        "a dirty tree is never removed, whatever the record says\n{text}"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("agent-dirty (2 dirty"),
        "the record made it due, so the dirty guard judged it and NAMED it\n{text}"
    );
    let put = run_step_put(&root, &text);
    assert!(
        put.contains("\"worktrees_kept_dirty\":\"1\"") && put.contains("agent-dirty:2"),
        "the kept dirty tree is on the packet with its count\n{put}\n{text}"
    );
}

/// REFUSAL 2. No landing on record is not a landing. A run still open,
/// a car still parked, and a tree the record has never heard of all keep
/// the 48h unpushed window — and the packet says WHY each stays, rather
/// than folding them into "git activity inside the window".
#[test]
fn an_unpushed_worktree_with_no_landing_on_record_is_kept_and_counted() {
    let root = boss_testing::scratch_dir("boss-dsr-record-none");
    let _guard = Scratch(root.clone());
    let yard = Yard::new(&root);
    let open_run = yard.worktree("agent-open-run", Some("fix/open-run"), 2);
    let unknown = yard.worktree("agent-unknown", Some("worktree-agent-unknown"), 2);
    let gone = abandoned_tree(&yard);
    write_record(
        &root,
        serde_json::json!({
            "gate_runs": [{"worktree": path_str(&open_run), "branch": "fix/open-run", "agent_run": "run0open"}],
            "cars": [{"id": "c0parked", "branch": "fix/open-run", "status": "open"}],
            "runs": {"run0open": "open"},
        }),
    );

    let out = run(&root, &[("STUB_DF_WORK_GB", &under_the_work_floor())]);
    let text = say(&out);
    assert!(
        open_run.exists(),
        "a run still open and a car not merged are no landing\n{text}"
    );
    assert!(
        unknown.exists(),
        "a tree the record has never heard of is no landing either\n{text}"
    );
    assert!(
        !gone.exists(),
        "past the unpushed window the pass still takes an abandoned tree\n{text}"
    );
    let log = curl_log(&root);
    assert!(
        log.contains("kind=gate-run")
            && log.contains("metadata=")
            && log.contains("kind=ship-a-change")
            && log.contains("/api/jobs/run0open"),
        "the record was asked — the gate-runs from the tree, the car on its branch, the run\n{log}\n{text}"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("no landing on record"),
        "each kept tree says why it stays\n{text}"
    );
    let put = run_step_put(&root, &text);
    assert!(
        put.contains("\"worktrees_kept_unpushed_no_landing\":\"2\""),
        "the packet counts the trees kept for want of a landing on record\n{put}\n{text}"
    );
}

/// REFUSAL 3. A record that cannot be read FAILS CLOSED: every unpushed
/// tree keeps its 48h window, exactly as before the record was asked.
/// Two ways it goes dark — nothing answers (curl 7) and an error answers
/// (curl 22) — and both trees here DO have a landing on record, so the
/// dark read is the only thing that keeps them. It is asked once a pass:
/// a hundred trees must not each wait out a dead API.
#[test]
fn a_system_of_record_that_cannot_be_read_keeps_every_unpushed_worktree() {
    for rc in ["7", "22"] {
        let root = boss_testing::scratch_dir("boss-dsr-record-dark");
        let _guard = Scratch(root.clone());
        let yard = Yard::new(&root);
        let first = yard.worktree("agent-first", Some("fix/first"), 2);
        let second = yard.worktree("agent-second", Some("fix/second"), 2);
        let gone = abandoned_tree(&yard);
        write_record(
            &root,
            serde_json::json!({
                "cars": [
                    {"id": "c0first0", "branch": "fix/first", "merged": "true"},
                    {"id": "c0second", "branch": "fix/second", "merged": "true"},
                ],
            }),
        );

        let out = run(
            &root,
            &[
                ("STUB_DF_WORK_GB", &under_the_work_floor()),
                ("STUB_RECORD_RC", rc),
            ],
        );
        let text = say(&out);
        assert!(
            first.exists() && second.exists(),
            "curl {rc}: a record that could not be read is no landing — kept\n{text}"
        );
        assert!(
            !gone.exists(),
            "curl {rc}: and the rest of the pass still runs\n{text}"
        );
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("could not be read"),
            "curl {rc}: the dark read is said out loud\n{text}"
        );
        let log = curl_log(&root);
        let asked = log
            .lines()
            .filter(|l| l.contains("metadata=") || l.contains("/api/jobs/run"))
            .count();
        assert_eq!(
            asked, 1,
            "curl {rc}: a dark record is asked once a pass, not once a tree\n{log}\n{text}"
        );
        let put = run_step_put(&root, &text);
        assert!(
            put.contains("\"worktrees_kept_unpushed_sor_unread\":\"2\"")
                && put.contains(&format!("curl exit {rc}")),
            "curl {rc}: the packet counts what the dark read kept, and why\n{put}\n{text}"
        );
    }
}

/// THE CHANGE. A tree whose builder's run FINISHED, whose branch's car
/// MERGED, or which launched a gate for a car that merged under another
/// branch name, is landed on record, and under the /work floor waits only
/// the live window. A landed tree inside the live window is still kept,
/// and is not even asked about; above the floor the 3h grace holds.
#[test]
fn a_worktree_whose_run_finished_or_whose_car_merged_on_record_is_landed() {
    for under in [true, false] {
        let root = boss_testing::scratch_dir("boss-dsr-record-landed");
        let _guard = Scratch(root.clone());
        let yard = Yard::new(&root);
        let run_done = yard.worktree("agent-run-done", Some("worktree-agent-run-done"), 2);
        let car_merged = yard.worktree("agent-car-merged", Some("fix/car-merged"), 2);
        let via_gate = yard.worktree("agent-via-gate", Some("worktree-agent-via-gate"), 2);
        let fresh = yard.worktree("agent-fresh", Some("fix/fresh"), 0);
        write_record(
            &root,
            serde_json::json!({
                "gate_runs": [
                    {"worktree": path_str(&run_done), "branch": "fix/run-done", "agent_run": "run0done"},
                    {"worktree": path_str(&via_gate), "branch": "fix/via-gate", "agent_run": "run0via1"},
                ],
                "cars": [
                    {"id": "c0merged", "branch": "fix/car-merged", "merged": "true"},
                    {"id": "c0viagat", "branch": "fix/via-gate", "status": "closed", "outcome": "merged"},
                    {"id": "c0fresh0", "branch": "fix/fresh", "merged": "true"},
                ],
                "runs": {"run0done": "closed", "run0via1": "open"},
            }),
        );

        let free = if under {
            under_the_work_floor()
        } else {
            (work_floor() + 10).to_string()
        };
        let out = run(&root, &[("STUB_DF_WORK_GB", &free)]);
        let text = say(&out);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            fresh.exists(),
            "a tree inside the live window is kept, landed or not\n{text}"
        );
        if under {
            assert!(
                !run_done.exists(),
                "its run finished: landed on record, removed after the live window\n{text}"
            );
            assert!(
                !car_merged.exists(),
                "its branch's car merged: landed on record\n{text}"
            );
            assert!(
                !via_gate.exists(),
                "it launched a gate for a branch whose car merged: landed on record\n{text}"
            );
            assert!(
                stdout.contains("agent-run run0done") && stdout.contains("is finished"),
                "the removal names the run the record answered with\n{text}"
            );
            assert!(
                stdout.contains("car c0merged") && stdout.contains("car c0viagat"),
                "and the car\n{text}"
            );
            let put = run_step_put(&root, &text);
            assert!(
                put.contains("\"worktrees_removed_by_record\":\"3\""),
                "the packet counts the trees only the record could call landed\n{put}\n{text}"
            );
        } else {
            assert!(
                run_done.exists() && car_merged.exists() && via_gate.exists(),
                "above the floor a tree landed on record keeps the 3h grace\n{text}"
            );
            assert!(
                !curl_log(&root).contains("metadata="),
                "inside the landed window nothing is asked of the record\n{}\n{text}",
                curl_log(&root)
            );
        }
    }
}

/// An RFC 3339 instant `hours_ago`, in the shape the system of record
/// stamps `metadata.closed_at` (nanoseconds and an offset).
fn instant_hours_ago(hours_ago: u64) -> String {
    let at = format!("@{}", unix_now() - hours_ago * 3600);
    let out = Command::new("date")
        .args(["-u", "-d", &at, "+%Y-%m-%dT%H:%M:%S.123456789+00:00"])
        .output()
        .expect("run date");
    assert!(out.status.success(), "date -d {at}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A HARNESS LOCK OUTLIVES ITS AGENT (backlog 3a84cbf5). The Claude
/// harness locks every agent worktree `claude agent agent-<id> (pid N
/// start T)` with the pid of the OPERATOR'S session, not the agent's, so
/// the lock reads live for as long as that session lives, however long
/// ago the agent handed back. Measured 2026-10-21 16:15Z on the pod: 116
/// such locks, every one naming one of two live sessions; 97 of those
/// trees had gated, and every agent-run that gated from each was closed
/// (9.1 GB), while /work sat at its 18 GB floor and the pass reported
/// `kept 536`. The record is the owner the lock cannot name: a harness
/// lock is RELEASED when every agent-run whose gate-run launched from
/// the tree is closed, each at least WORKTREE_GRACE_H ago, and git has
/// been quiet in the tree as long — the grace is NOT shortened under the
/// floor, because the session the lock names is alive and could resume
/// the agent. Released, the tree is judged like any other: dirty keeps
/// it, and its lock stays on unless it is removed. Every refusal is here:
/// an open run, one open run among closed ones, a close inside the grace,
/// no gate-run at all, a closed run with no close time, a tree with
/// recent git activity, and a lock that is not the harness's shape.
#[test]
fn a_harness_lock_whose_every_gated_run_closed_past_the_grace_is_released() {
    let root = boss_testing::scratch_dir("boss-dsr-harness-released");
    let _guard = Scratch(root.clone());
    let yard = Yard::new(&root);
    let lock = |name: &str| format!("claude agent {name} (pid 355 start 95092646)");
    // Unpushed, idle 20h: past the 3h grace, inside the 48h unpushed
    // window, so only the record can make any of them due. Every id is
    // alphanumeric after `agent-`, as the harness writes it and as
    // HARNESS_LOCK_SHAPE requires: a hyphenated id is refused on its
    // shape before any guard runs, and three refusals here once passed
    // for that reason alone — deleting the closed_at grace or releasing
    // a never-gated tree left this test green (review 48211da3).
    let names = [
        "agent-done",
        "agent-dirty",
        "agent-open",
        "agent-mixed",
        "agent-recentclose",
        "agent-nogate",
        "agent-noclosedat",
        "agent-human",
    ];
    let trees: Vec<PathBuf> = names
        .iter()
        .map(|n| yard.worktree(n, Some(&format!("worktree-{n}")), 20))
        .collect();
    // Its run closed long ago, but git moved in it 2h ago.
    let busy = yard.worktree("agent-busy", Some("worktree-agent-busy"), 2);
    for (name, tree) in names.iter().zip(&trees) {
        if *name == "agent-human" {
            harness_lock(&yard, tree, "operator: bisecting, leave it");
        } else {
            harness_lock(&yard, tree, &lock(name));
        }
    }
    harness_lock(&yard, &busy, &lock("agent-busy"));
    let [
        done,
        done_dirty,
        open,
        mixed,
        recent,
        no_gate,
        no_closed_at,
        human,
    ] = [0, 1, 2, 3, 4, 5, 6, 7].map(|i| trees[i].clone());
    dirty(&done_dirty, 20);
    let gate = |tree: &Path, run: &str| serde_json::json!({"id": format!("g-{run}"), "worktree": path_str(tree), "branch": "fix/x", "agent_run": run});
    // Closed 4h ago: past the 3h grace (backlog b4112350; 15h while the
    // grace was 12h), so this test fails if the default grows back. The
    // close inside the grace stays at 2h.
    let long_ago = instant_hours_ago(4);
    write_record(
        &root,
        serde_json::json!({
            "gate_runs": [
                gate(&done, "run0done"),
                gate(&done_dirty, "run0drty"),
                gate(&open, "run0open"),
                gate(&mixed, "run0mix1"),
                gate(&mixed, "run0mix2"),
                gate(&recent, "run0rcnt"),
                gate(&no_closed_at, "run0noat"),
                gate(&human, "run0humn"),
                gate(&busy, "run0busy"),
            ],
            "runs": {
                "run0done": "closed", "run0drty": "closed", "run0open": "open",
                "run0mix1": "closed", "run0mix2": "open", "run0rcnt": "closed",
                "run0noat": "closed", "run0humn": "closed", "run0busy": "closed",
            },
            "closed_at": {
                "run0done": long_ago, "run0drty": long_ago, "run0mix1": long_ago,
                "run0rcnt": instant_hours_ago(2), "run0humn": long_ago,
                "run0busy": long_ago,
            },
        }),
    );
    // pid 355 is alive with the start every lock recorded: by the
    // process table alone, every lock here is live.
    let proc_root = fake_proc(&root, "pause", &[(355, "claude", 95092646)]);

    // Under the /work floor, where a landed tree waits only 30 minutes:
    // a released lock still waits the full grace.
    let out = run(
        &root,
        &[
            ("PROC_ROOT", proc_root.to_str().expect("utf8")),
            ("STUB_DF_WORK_GB", &under_the_work_floor()),
        ],
    );
    let text = say(&out);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !done.exists(),
        "every run that gated from it closed past the grace, quiet, clean: released and removed\n{text}"
    );
    for (tree, why) in [
        (&open, "its run is open"),
        (&mixed, "one of its runs is open"),
        (&recent, "its run closed inside the grace"),
        (
            &no_gate,
            "no gate-run names it, so the record names no owner",
        ),
        (
            &no_closed_at,
            "a closed run with no close time cannot say how long ago",
        ),
        (
            &human,
            "a lock that is not the harness's shape is never judged",
        ),
        (&busy, "git moved in it inside the grace"),
    ] {
        assert!(tree.exists(), "{} is kept: {why}\n{text}", tree.display());
    }
    assert!(
        done_dirty.exists() && stdout.contains("agent-dirty (2 dirty"),
        "a released lock only opens the judgement: a dirty tree is kept and named\n{text}"
    );
    let listed = git(&yard.repo, 0, &["worktree", "list", "--porcelain"]);
    assert!(
        listed.contains(&lock("agent-dirty")),
        "a released tree the pass KEEPS keeps its lock\n{listed}\n{text}"
    );
    assert!(
        stdout.lines().any(|l| l.contains("agent-done:")
            && l.contains("every agent-run that gated from this worktree")),
        "the release names the record that made it\n{text}"
    );
    let put = run_step_put(&root, &text);
    assert!(
        put.contains("\"worktrees_released_locks\":\"2\"")
            && put.contains("\"worktrees_released_lock_names\":\"agent-dirty, agent-done\"")
            && put.contains("\"worktrees_kept_locked\":\"7\""),
        "the packet counts and names the released locks apart from the live ones\n{put}\n{text}"
    );
}

/// The release FAILS CLOSED: a record that cannot be read releases no
/// harness lock, says so, and is asked once a pass. The tree surviving
/// is not the proof — it is unpushed, so the landed judgement's own
/// latch keeps it even when the lock is wrongly released — so the
/// packet's counters are: nothing released, the one lock kept (review
/// 48211da3, B2).
#[test]
fn a_record_that_cannot_be_read_releases_no_harness_lock() {
    let root = boss_testing::scratch_dir("boss-dsr-harness-dark");
    let _guard = Scratch(root.clone());
    let yard = Yard::new(&root);
    let tree = yard.worktree("agent-done", Some("worktree-agent-done"), 20);
    harness_lock(
        &yard,
        &tree,
        "claude agent agent-done (pid 355 start 95092646)",
    );
    let gone = abandoned_tree(&yard);
    write_record(
        &root,
        serde_json::json!({
            "gate_runs": [{"worktree": path_str(&tree), "branch": "fix/x", "agent_run": "run0done"}],
            "runs": {"run0done": "closed"},
            "closed_at": {"run0done": instant_hours_ago(15)},
        }),
    );
    let proc_root = fake_proc(&root, "pause", &[(355, "claude", 95092646)]);
    let out = run(
        &root,
        &[
            ("PROC_ROOT", proc_root.to_str().expect("utf8")),
            ("STUB_RECORD_RC", "7"),
        ],
    );
    let text = say(&out);
    assert!(tree.exists(), "an unread record releases nothing\n{text}");
    assert!(
        !gone.exists(),
        "and the rest of the pass still runs\n{text}"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("could not be read"),
        "the dark read is said out loud\n{text}"
    );
    let put = run_step_put(&root, &text);
    assert!(
        put.contains("\"worktrees_released_locks\":\"0\"")
            && put.contains("\"worktrees_kept_locked\":\"1\""),
        "the lock itself is what keeps it, not the unpushed branch\n{put}\n{text}"
    );
}

/// A worktree this pass cannot SEE is not a worktree that is gone
/// (backlog 52fefc45). The sidecar mounts only /work and /scratch
/// (boss-dev.yaml); a reviewer's scratch worktree under the session
/// scratchpad in /tmp is the dev container's own overlay, invisible to
/// it, and a bare `git worktree prune` read every such entry as
/// "gitdir file points to non-existent location" and dropped it —
/// measured 2026-09-28 by run ff5ff31c: `pruned: Removing worktrees/rv`,
/// `wt`, `rr`, `wt` at 16:58, 17:59 and 18:59Z, inside the window in
/// which three live reviewers each lost their tree's metadata.
///
/// SHAPE: the outside tree is a real `git worktree add` in a directory
/// that is not under WORKTREES_DIR, WORK_MOUNT or SCRATCH_MOUNT, then
/// deleted — which is exactly what the sidecar's view of /tmp is. The
/// control is the same shape under WORKTREES_DIR, and it must be pruned,
/// or the fixture proves nothing. Run above the /work floor (the gone-
/// worktree pass's prune) and under it (the floor pass's two), so each
/// of the three prune sites meets the entry.
#[test]
fn an_entry_outside_the_mounts_this_pass_can_see_is_kept_and_a_gone_one_inside_is_pruned() {
    for floor in [false, true] {
        let root = boss_testing::scratch_dir("boss-dsr-unseen");
        let _guard = Scratch(root.clone());
        let yard = Yard::new(&root);
        // The session scratchpad: a root of its own, under none of the
        // mounts the run below names.
        let overlay = boss_testing::scratch_dir("boss-dsr-unseen-overlay");
        let _overlay_guard = Scratch(overlay.clone());
        let unseen = overlay.join("rv");
        git(
            &yard.repo,
            0,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                unseen.to_str().expect("utf8"),
                "main",
            ],
        );
        std::fs::remove_dir_all(&unseen).expect("rm the unseen worktree");

        // The control: gone, under WORKTREES_DIR — an operator's rm -rf.
        let control = yard.worktree("agent-control", Some("feat/control"), 30);
        std::fs::remove_dir_all(&control).expect("rm the control worktree");
        // Gone, under WORKTREES_DIR, but LOCKED: git's own prune honours
        // the lock, and so does this pass.
        let locked = yard.worktree("agent-locked", Some("feat/locked"), 30);
        git(
            &yard.repo,
            0,
            &["worktree", "lock", path_str(&locked).as_str()],
        );
        std::fs::remove_dir_all(&locked).expect("rm the locked worktree");

        let floor_gb = under_the_work_floor();
        let extra: Vec<(&str, &str)> = if floor {
            vec![("STUB_DF_WORK_GB", floor_gb.as_str())]
        } else {
            vec![]
        };
        let out = run(&root, &extra);
        let text = format!("under the floor: {floor}\n{}", say(&out));
        let stdout = String::from_utf8_lossy(&out.stdout);

        let admin = yard.repo.join(".git").join("worktrees");
        assert!(
            admin.join("rv").is_dir(),
            "an entry whose worktree lies outside every mount this pass can see is KEPT\n{text}"
        );
        assert!(
            !admin.join("agent-control").exists(),
            "the control — gone, under WORKTREES_DIR — is pruned, so the fixture is strong enough\n{text}"
        );
        assert!(
            admin.join("agent-locked").is_dir(),
            "a locked entry is untouched even with its directory gone\n{text}"
        );
        assert!(
            stdout.contains("kept worktrees/rv") && stdout.contains(&path_str(&unseen)),
            "the kept entry is named with the path the pass could not see\n{text}"
        );
        let put = run_step_put(&root, &text);
        for field in [
            "\"worktrees_pruned\":\"1\"",
            "\"worktrees_kept_unseen\":\"1\"",
            "\"worktrees_kept_unseen_names\":\"rv\"",
        ] {
            assert!(
                put.contains(field),
                "the run step carries {field}\n{put}\n{text}"
            );
        }
    }
}

// ---------------------------------------------------------------------
// The sidecar presents the estate machine token (backlog 37742794;
// David's answer to design-doc c8502e17, `reclaim-sidecar`, 2026-10-07).
// The pass sends two kinds of request: its own reads of the record (the
// gate-runs from a worktree, the car on its branch, a run by id) and
// the wrap + step pair that files its packet. With the Secret mounted in
// the sidecar every one of them carries the token as a 0600 header file;
// with nothing mounted — the dev container, where wt-cargo and the shim
// run this same script — the pass sends exactly what it sent before.
// ---------------------------------------------------------------------

/// Short on purpose: a fixture, and no-secrets.sh reads 32+ characters
/// after such a name as a credential.
const FIXTURE_TOKEN: &str = "fixture-not-a-credential";

/// `(stamp, "METHOD URL")` per request the stub curl saw.
fn stamps(root: &Path) -> Vec<(String, String)> {
    std::fs::read_to_string(root.join("curl-log.txt.stamps"))
        .unwrap_or_default()
        .lines()
        .map(|l| {
            let (stamp, rest) = l.split_once(' ').expect("stamp, then the request");
            (stamp.to_string(), rest.to_string())
        })
        .collect()
}

/// The record-reading scenario of REFUSAL 2 under `root`, run with
/// `extra` on top: three reads of the record, then the packet.
fn a_pass_that_reads_the_record_and_files(root: &Path, extra: &[(&str, &str)]) -> Output {
    let yard = Yard::new(root);
    let open_run = yard.worktree("agent-open-run", Some("fix/open-run"), 2);
    yard.worktree("agent-unknown", Some("worktree-agent-unknown"), 2);
    abandoned_tree(&yard);
    write_record(
        root,
        serde_json::json!({
            "gate_runs": [{"worktree": path_str(&open_run), "branch": "fix/open-run", "agent_run": "run0open"}],
            "cars": [{"id": "c0parked", "branch": "fix/open-run", "status": "open"}],
            "runs": {"run0open": "open"},
        }),
    );
    let floor = under_the_work_floor();
    let mut env: Vec<(&str, &str)> = vec![("STUB_DF_WORK_GB", &floor)];
    env.extend_from_slice(extra);
    run(root, &env)
}

/// A machine-token mount holding `slot` as its `current`, a header
/// directory, and the one line a stamped request's file must hold.
fn token_mount(root: &Path, slot: &str) -> (String, String, String) {
    boss_testing::create_dir(&root.join("mt/tok"));
    boss_testing::create_dir(&root.join("mt/hdr"));
    boss_testing::write_file(&root.join("mt/tok/current"), slot);
    boss_testing::write_file(
        &root.join("mt/expect-header"),
        &format!("x-boss-machine-token: {FIXTURE_TOKEN}\n"),
    );
    (
        path_str(&root.join("mt/tok")),
        path_str(&root.join("mt/hdr")),
        path_str(&root.join("mt/expect-header")),
    )
}

#[test]
fn with_the_token_mounted_every_request_of_a_pass_carries_it_as_a_header_file() {
    let root = boss_testing::scratch_dir("boss-dsr-token-held");
    let _guard = Scratch(root.clone());
    let (tok, hdr, expect) = token_mount(&root, &format!("{FIXTURE_TOKEN}\n"));
    let out = a_pass_that_reads_the_record_and_files(
        &root,
        &[
            ("BOSS_MACHINE_TOKEN_DIR", &tok),
            ("BOSS_MACHINE_TOKEN_HOSTS", "sor.test"),
            ("RUNTIME_DIRECTORY", &hdr),
            ("STUB_EXPECT_HDR", &expect),
        ],
    );
    let text = say(&out);
    let sent = stamps(&root);
    for needle in [
        "kind=gate-run",
        "kind=ship-a-change",
        "/api/jobs/run0open",
        "POST ",
        "PATCH ",
        "PUT ",
    ] {
        assert!(
            sent.iter().any(|(_, r)| r.contains(needle)),
            "the pass did not send `{needle}` — a route this test never drives is one it never judges\n{sent:#?}\n{text}"
        );
    }
    let bare: Vec<&(String, String)> = sent.iter().filter(|(s, _)| s != "stamped").collect();
    assert!(
        bare.is_empty(),
        "these requests went out without the machine token as a 0600 file holding the one \
         header line — the machine door refuses each the day it enforces:\n{bare:#?}\n{text}"
    );
    let everything = format!("{text}{}", curl_log(&root));
    assert!(
        !everything.contains(FIXTURE_TOKEN),
        "the token is in the pass's output or in a curl argv\n{text}"
    );
    let left: Vec<PathBuf> = std::fs::read_dir(root.join("mt/hdr"))
        .expect("the header directory")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .collect();
    assert!(left.is_empty(), "a header file outlived the pass: {left:?}");
}

#[test]
fn without_a_token_or_for_another_host_the_pass_sends_the_same_requests_bare() {
    let base_root = boss_testing::scratch_dir("boss-dsr-token-none");
    let _g0 = Scratch(base_root.clone());
    let base = a_pass_that_reads_the_record_and_files(&base_root, &[]);
    // The requests, with the one thing that differs by fixture — the
    // scratch root inside a url-encoded `metadata=` worktree path — cut.
    let requests = |root: &Path| -> Vec<String> {
        stamps(root)
            .into_iter()
            .map(|(_, r)| match r.split_once("metadata=") {
                Some((head, _)) => format!("{head}metadata=…"),
                None => r,
            })
            .collect()
    };
    assert!(
        stamps(&base_root).iter().all(|(s, _)| s == "bare"),
        "with no token directory nothing is stamped\n{}",
        say(&base)
    );
    assert!(
        requests(&base_root).len() >= 6,
        "the reference pass read the record and filed its packet\n{:#?}\n{}",
        requests(&base_root),
        say(&base)
    );
    for (name, hosts, slot) in [
        (
            "boss-dsr-token-off-host",
            "elsewhere.test",
            format!("{FIXTURE_TOKEN}\n"),
        ),
        ("boss-dsr-token-blank", "sor.test", "\n  \n".to_string()),
    ] {
        let root = boss_testing::scratch_dir(name);
        let _g = Scratch(root.clone());
        let (tok, hdr, expect) = token_mount(&root, &slot);
        let out = a_pass_that_reads_the_record_and_files(
            &root,
            &[
                ("BOSS_MACHINE_TOKEN_DIR", &tok),
                ("BOSS_MACHINE_TOKEN_HOSTS", hosts),
                ("RUNTIME_DIRECTORY", &hdr),
                ("STUB_EXPECT_HDR", &expect),
            ],
        );
        assert_eq!(
            out.status.code(),
            base.status.code(),
            "{name}: the pass must never end differently because of the token\n{}",
            say(&out)
        );
        assert!(
            stamps(&root).iter().all(|(s, _)| s == "bare"),
            "{name}: nothing is sent for a token that is not one, or to a host that is not the estate's\n{:#?}",
            stamps(&root)
        );
        assert_eq!(
            requests(&root),
            requests(&base_root),
            "{name}: the token is a header, not a behaviour — the same requests go out"
        );
    }
}

// ---------------------------------------------------------------------
// A failed pass names what failed (backlog 5e5d4be8).
// ---------------------------------------------------------------------

/// The `output` key of a run-step merge: the pass's `RED` lines.
fn recorded_output(put: &str) -> String {
    let body = put
        .split_once("/metadata ")
        .map(|(_, b)| b)
        .unwrap_or_default();
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("output").and_then(|o| o.as_str()).map(str::to_string))
        .unwrap_or_else(|| panic!("the run step carries an `output` string\n{put}"))
}

// ---------------------------------------------------------------------
// THE DEV HOME'S KUBE CONFIG IS HELD BY ITS OWNER (backlog 5ebc7a6d;
// measured by run c38afb80 on the pod 2026-10-19, and again 2026-10-21).
// /work/home/.kube/config was 0664 root:1500 under /work/home/.kube and
// /work/home, both 2775 root:1500 with no sticky bit. Every builder
// under as-gate-uid is group 1500, so it could rewrite the file — or,
// as the review of 6d7b418b noted, rename .kube aside and plant its own
// — and kubectl would send the dev session's ServiceAccount token to
// whatever server the planted file names. The hourly pass holds
// /work/home sticky, .kube and its config private to the uid that owns
// them, and names every other credential-adjacent path under the home a
// group or other may write.
// ---------------------------------------------------------------------

fn home_mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::symlink_metadata(path)
        .unwrap_or_else(|e| panic!("stat {}: {e}", path.display()))
        .permissions()
        .mode()
        & 0o7777
}

fn chmod_home(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    // mode-bits-ok: the fixture home, given the modes the pod's fsGroup and umask left it
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .unwrap_or_else(|e| panic!("chmod {mode:o} {}: {e}", path.display()));
}

/// The dev home as the pod held it on 2026-10-19: /work/home and
/// /work/home/.kube 2775, the config 0664, kubectl's cache 2770.
fn dev_home(root: &Path) -> PathBuf {
    let home = root.join("work").join("home");
    boss_testing::create_dir(&home.join(".kube/cache/discovery"));
    boss_testing::write_file(&home.join(".kube/config"), "apiVersion: v1\nkind: Config\n");
    // The work mount above it as the doors' tree's pass leaves it
    // (backlog 3db83d0d): sticky, so the home cannot be moved aside.
    for (p, m) in [
        (root.join("work"), 0o3775),
        (home.clone(), 0o2775),
        (home.join(".kube"), 0o2775),
        (home.join(".kube/config"), 0o664),
        (home.join(".kube/cache"), 0o2770),
    ] {
        chmod_home(&p, m);
    }
    home
}

#[test]
fn every_pass_holds_the_dev_homes_kube_config_private_to_its_owner() {
    let root = boss_testing::scratch_dir("boss-dsr-home-kube");
    let _guard = Scratch(root.clone());
    let _yard = Yard::new(&root);
    let home = dev_home(&root);

    let out = run(&root, &[]);
    let text = say(&out);
    assert!(out.status.success(), "{text}");
    assert_eq!(
        home_mode(&home.join(".kube")) & 0o077,
        0,
        "no group or other may enter, list or rename inside .kube\n{text}"
    );
    assert_eq!(
        home_mode(&home.join(".kube/config")) & 0o077,
        0,
        "no group or other may read or rewrite the kube config\n{text}"
    );
    assert_eq!(
        home_mode(&home.join(".kube/config")) & 0o600,
        0o600,
        "its owner still reads and writes it\n{text}"
    );
    let h = home_mode(&home);
    assert_ne!(
        h & 0o1000,
        0,
        "the home is sticky, so .kube cannot be renamed aside ({h:o})\n{text}"
    );
    assert_eq!(
        h & 0o2070,
        0o2070,
        "the home keeps group rwx and setgid: its own mode is a separate decision ({h:o})\n{text}"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("dev home's kube config: held"),
        "the pass says what it holds\n{text}"
    );
    assert!(
        !curl_log(&root).contains("POST "),
        "holding modes is not reclaiming: a pass that only held files no packet\n{}",
        curl_log(&root)
    );

    // A link in place of the config is not a file this pass can hold: it
    // names it, every pass, and the packet carries the state.
    std::fs::remove_file(home.join(".kube/config")).expect("drop the config");
    std::os::unix::fs::symlink("/elsewhere/config", home.join(".kube/config"))
        .expect("plant a link");
    chmod_home(&home.join(".kube"), 0o2775);
    let out = run(&root, &[]);
    let text = say(&out);
    assert_eq!(
        out.status.code(),
        Some(1),
        "a kube config that is a link reds the pass\n{text}"
    );
    assert_eq!(
        home_mode(&home.join(".kube")) & 0o077,
        0,
        ".kube held again\n{text}"
    );
    let log = curl_log(&root);
    let put = log
        .lines()
        .rfind(|l| is_run_step_merge(l))
        .unwrap_or_else(|| panic!("the pass records itself\n{log}\n{text}"));
    let output = recorded_output(put);
    assert!(
        output.lines().any(
            |l| l.starts_with("RED dev-scratch-reclaim/home-kube-open problem: ")
                && l.contains(".kube/config is a link")
        ),
        "the packet names the link\n{output}"
    );
    assert!(
        put.contains("\"home_kube\":\"OPEN"),
        "and the kube config's state says it is not held\n{put}"
    );
}

#[test]
fn a_credential_adjacent_path_a_group_may_write_is_named_on_the_packet() {
    let root = boss_testing::scratch_dir("boss-dsr-home-creds");
    let _guard = Scratch(root.clone());
    let _yard = Yard::new(&root);
    let home = dev_home(&root);
    // Open, as the pod's were on 2026-10-21: the actor every verb signs
    // as, in a group-writable directory; an agent's session credential,
    // 0600 itself but in a directory a group member may rename it out of.
    for d in [".config/boss", ".claude", ".ssh", ".codex"] {
        boss_testing::create_dir(&home.join(d));
    }
    boss_testing::write_file(&home.join(".config/boss/actor"), "agent-x\n");
    boss_testing::write_file(&home.join(".claude/.credentials.json"), "{}");
    // Private: a key directory, and a token beside no writable parent.
    boss_testing::write_file(&home.join(".ssh/id_ed25519"), "k");
    boss_testing::write_file(&home.join(".codex/auth.json"), "{}");
    for (p, m) in [
        // Start open; main's worktree guard holds this ancestor before
        // the credential audit, so it must not remain a reported risk.
        (root.join("work"), 0o2775),
        (home.join(".config"), 0o2770),
        (home.join(".config/boss"), 0o2775),
        (home.join(".config/boss/actor"), 0o664),
        (home.join(".claude"), 0o2775),
        (home.join(".claude/.credentials.json"), 0o600),
        (home.join(".ssh"), 0o700),
        (home.join(".ssh/id_ed25519"), 0o600),
        (home.join(".codex"), 0o2755),
        (home.join(".codex/auth.json"), 0o600),
    ] {
        chmod_home(&p, m);
    }

    let out = run(&root, &[]);
    let text = say(&out);
    assert_eq!(
        out.status.code(),
        Some(1),
        "an open credential-adjacent path reds the pass\n{text}"
    );
    let log = curl_log(&root);
    let put = log
        .lines()
        .rfind(|l| is_run_step_merge(l))
        .unwrap_or_else(|| panic!("the pass records itself\n{log}\n{text}"));
    let output = recorded_output(put);
    let red = output
        .lines()
        .find(|l| l.starts_with("RED dev-scratch-reclaim/home-credentials-open problem: "))
        .unwrap_or_else(|| panic!("the packet names the open paths\n{output}"));
    for open in [
        ".config/boss/actor (664)",
        ".config/boss (2775)",
        ".config (2770)",
        ".claude (2775)",
    ] {
        assert!(red.contains(open), "{open} is named\n{red}");
    }
    for private in [".ssh", ".codex", ".kube"] {
        assert!(!red.contains(private), "{private} is not named\n{red}");
    }
    assert_ne!(
        home_mode(&root.join("work")) & 0o1000,
        0,
        "main's worktree guard holds the writable ancestor before the audit\n{text}"
    );
    assert!(
        !red.contains("not sticky"),
        "an ancestor held by this pass must not be reported open\n{red}"
    );
    assert_eq!(
        home_mode(&home.join(".config/boss/actor")),
        0o664,
        "the audit names; it changes no mode it was not asked to hold\n{text}"
    );
    assert!(
        put.contains("\"home_kube\":\"held, re-held 3 this pass\""),
        "the kube config's hold rides the same packet\n{put}"
    );
    assert!(
        put.contains("\"home_credentials\":\"OPEN: 4 path(s)\""),
        "and the audit's count\n{put}"
    );
}

fn running_as_uid_0() -> bool {
    String::from_utf8_lossy(&Command::new("id").arg("-u").output().expect("id -u").stdout).trim()
        == "0"
}

/// `argv` as uid 65534 — the gate's uid, read from
/// infra/gate-runner/gate-runner.yaml by infra/dev/as-gate-uid.sh — in
/// group `gid` and no other: what every builder is to the pod's
/// root:1500 home.
fn as_group_member_of(gid: u32, argv: &[&str]) -> Output {
    Command::new("setpriv")
        .args(["--reuid=65534", &format!("--regid={gid}"), "--clear-groups"])
        .arg("--")
        .args(argv)
        .output()
        .expect("setpriv")
}

/// The finding itself, reproduced and then refused: a uid in the home's
/// group that does not own it rewrites the kube config, renames .kube
/// aside and plants one of its own, until the pass has held them; then
/// it can do none of that, cannot read the config, and cannot plant a
/// kuberc (kubectl 1.34 reads $HOME/.kube/kuberc by default).
///
/// The pod has no CAP_CHOWN, so the fixture cannot be staged in group
/// 1500; it keeps the group root created it with and the uid drops into
/// THAT group — the kernel's rule is the same for any group. Root is
/// needed to drop to another uid at all, which the gate (uid 65534) is
/// not: there the test says NOT RUN and the mode pin above is the gate's
/// evidence.
#[test]
fn a_group_member_cannot_redirect_the_dev_homes_kubectl_once_the_pass_holds_it() {
    use std::os::unix::fs::MetadataExt;
    if !running_as_uid_0() {
        eprintln!(
            "NOT RUN: dropping to another uid needs root; the mode pin above covers the gate"
        );
        return;
    }
    let root = boss_testing::scratch_dir("boss-dsr-home-rename");
    let _guard = Scratch(root.clone());
    let _yard = Yard::new(&root);
    let home = dev_home(&root);
    chmod_home(&root, 0o755);
    chmod_home(&root.join("work"), 0o755);
    let gid = std::fs::metadata(&home).expect("stat home").gid();
    let s = |p: &Path| p.to_str().expect("utf8").to_string();
    let kube = s(&home.join(".kube"));
    let aside = s(&home.join(".kube.aside"));
    let config = s(&home.join(".kube/config"));
    let kuberc = s(&home.join(".kube/kuberc"));
    let rewrite = "printf 'server: https://attacker.invalid\\n' >> \"$1\"";

    for (what, argv) in [
        (
            "rewrite the kube config",
            vec!["sh", "-c", rewrite, "sh", config.as_str()],
        ),
        (
            "rename .kube aside",
            vec!["mv", "-T", kube.as_str(), aside.as_str()],
        ),
        ("and back", vec!["mv", "-T", aside.as_str(), kube.as_str()]),
    ] {
        let out = as_group_member_of(gid, &argv);
        assert!(
            out.status.success(),
            "control: a group member can {what} before the pass\n{}",
            say(&out)
        );
    }

    let out = run(&root, &[]);
    assert!(out.status.success(), "{}", say(&out));

    for (what, argv) in [
        (
            "rewrite the kube config",
            vec!["sh", "-c", rewrite, "sh", config.as_str()],
        ),
        ("read the kube config", vec!["cat", config.as_str()]),
        (
            "rename .kube aside",
            vec!["mv", "-T", kube.as_str(), aside.as_str()],
        ),
        ("plant a kuberc", vec!["touch", kuberc.as_str()]),
    ] {
        let out = as_group_member_of(gid, &argv);
        assert!(
            !out.status.success(),
            "a group member could {what} after the pass held the home\n{}",
            say(&out)
        );
    }
    assert!(home.join(".kube/config").exists(), "the config stayed put");
}

#[test]
fn an_acting_pass_records_value_free_git_observer_state_without_changing_cleanup() {
    let root = boss_testing::scratch_dir("boss-dsr-git-observer");
    let _guard = Scratch(root.clone());
    let yard = Yard::new(&root);
    let kept = yard.worktree("agent-unanswered", Some("feat/unanswered"), 200);
    git(
        &yard.repo,
        0,
        &["update-ref", "-d", "refs/remotes/origin/main"],
    );
    let out = run(&root, &[]);
    assert!(
        kept.exists(),
        "unknown landing still conserves the tree: {}",
        say(&out)
    );
    let calls = curl_log(&root);
    let merge = calls
        .lines()
        .find(|line| line.contains("/steps/step-run/metadata "))
        .unwrap();
    let body: serde_json::Value =
        serde_json::from_str(merge.split_once("/metadata ").unwrap().1).unwrap();
    let state: serde_json::Value = serde_json::from_str(
        body["git_phase_ledger"]
            .as_str()
            .expect("phase state is recorded"),
    )
    .unwrap();
    assert_eq!(state["state"], "observed");
    assert!(state["records"].as_u64().unwrap() > 0);
    assert!(!ls_remote_calls(&root).contains("ls-remote"));
}

/// An `rm` that refuses ONE tree the way the pod's root does a tree the
/// gate's uid made — fifty `Permission denied` lines, one per file — and
/// a `setpriv` that stands in for becoming its owner: it logs its
/// arguments and runs the rest with the refusal lifted, or refuses
/// itself under `STUB_SETPRIV_REFUSES` (a container with no CAP_SETUID).
/// Every other `rm` goes to the real one.
fn stub_owner_only_tree(root: &Path) {
    let bin = root.join("bin");
    boss_testing::create_dir(&bin);
    let real = String::from_utf8(
        without_proxy(&mut Command::new("bash"))
            .args(["-c", "command -v rm"])
            .output()
            .expect("locate rm")
            .stdout,
    )
    .expect("utf8");
    let real = real.trim();
    assert!(!real.is_empty(), "a real rm on PATH");
    boss_testing::write_exec(
        &bin.join("rm"),
        &format!(
            concat!(
                "#!/usr/bin/env bash\n",
                "last=\"${{@: -1}}\"\n",
                "if [ \"$last\" = \"${{STUB_RM_REFUSES:-}}\" ] && [ -z \"${{STUB_AS_OWNER:-}}\" ]; then\n",
                "    for i in $(seq 1 50); do\n",
                "        echo \"rm: cannot remove '$last/debug/deps/f$i': Permission denied\" >&2\n",
                "    done\n",
                "    exit 1\n",
                "fi\n",
                "exec {real} \"$@\"\n",
            ),
            real = real
        ),
    );
    boss_testing::write_exec(
        &bin.join("setpriv"),
        concat!(
            "#!/usr/bin/env bash\n",
            "printf '%s\\n' \"$*\" >> \"$STUB_SETPRIV_LOG\"\n",
            "if [ -n \"${STUB_SETPRIV_REFUSES:-}\" ]; then\n",
            "    echo 'setpriv: setresuid failed: Operation not permitted' >&2\n",
            "    exit 1\n",
            "fi\n",
            "while [ $# -gt 0 ]; do case \"$1\" in --*) shift ;; *) break ;; esac; done\n",
            "STUB_AS_OWNER=1 exec \"$@\"\n",
        ),
    );
}

/// The uid that owns a path, as `stat` prints it.
fn owner_uid(path: &Path) -> String {
    let out = Command::new("stat")
        .args(["-c", "%u"])
        .arg(path)
        .output()
        .expect("stat");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Measured 2026-10-07 and 2026-10-08 (backlog cb1cdc61, 5e5d4be8):
/// /scratch/target-repo, made by the gate's uid 65534, mode 2755. The
/// pod's root holds CAP_SETUID and no CAP_DAC_OVERRIDE, so its `rm -rf`
/// was refused on every file — about 11,025 stderr lines per build from
/// wt-cargo's floor pass, nothing reclaimed, and a packet that said
/// `problems=1` and not which. The pass removes such a tree AS ITS
/// OWNER, the way infra/dev/as-gate-uid.sh removes its own workspace.
#[test]
fn a_tree_another_uid_owns_is_removed_as_its_owner() {
    let root = boss_testing::scratch_dir("boss-dsr-owner-removes");
    let _guard = Scratch(root.clone());
    boss_testing::create_dir(&root.join("work").join("wt"));
    boss_testing::create_dir(&root.join("work").join("repo"));
    stub_owner_only_tree(&root);
    let tree = target_dir(&root, "target-repo", 30);
    let uid = owner_uid(&tree);
    let setpriv_log = root.join("setpriv-log.txt");

    let out = run(
        &root,
        &[
            ("STUB_RM_REFUSES", tree.to_str().expect("utf-8 path")),
            (
                "STUB_SETPRIV_LOG",
                setpriv_log.to_str().expect("utf-8 path"),
            ),
        ],
    );
    let text = say(&out);
    assert!(!tree.exists(), "the owner's rm removes the tree\n{text}");
    let asked = std::fs::read_to_string(&setpriv_log).unwrap_or_default();
    assert!(
        asked.contains(&format!("--reuid={uid}")) && asked.contains("rm -rf"),
        "the retry runs rm as the uid that owns the tree ({uid})\n{asked}\n{text}"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        stderr.matches("Permission denied").count(),
        0,
        "a refusal the retry repaired is not printed per file\n{text}"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("as its owner"),
        "the pass says how the tree went\n{text}"
    );
    let log = curl_log(&root);
    let put = log
        .lines()
        .find(|l| is_run_step_merge(l))
        .unwrap_or_else(|| panic!("an acting pass records itself\n{log}\n{text}"));
    assert!(
        put.contains("\"stale_targets_reclaimed\":\"1\"")
            && put.contains("\"problems\":\"0\"")
            && put.contains("\"result\":\"ok\""),
        "a tree removed as its owner is a reclaim, not a problem\n{put}\n{text}"
    );
    assert_eq!(recorded_output(put), "", "and no RED line\n{put}");
}

/// ...and where nothing can remove it, the refusal is said ONCE — never
/// per file — and the packet NAMES the problem: which tree, whose it is,
/// how many paths refused and the first of them. The three newest
/// packets read 2026-10-08 23:58Z closed `failed` with `problems=2` and
/// no key saying which two (backlog 5e5d4be8; CLAUDE.md §Diagnosis, a
/// verdict must name what failed). The line is the chore judge's shape,
/// `RED <route> <kind>: <error>`, on the key it reads, so the rule
/// file-backlog-items-on-dev-scratch-reclaim-red can file it.
#[test]
fn a_tree_nothing_can_remove_is_said_once_and_named_on_the_packet() {
    let root = boss_testing::scratch_dir("boss-dsr-nobody-removes");
    let _guard = Scratch(root.clone());
    boss_testing::create_dir(&root.join("work").join("wt"));
    boss_testing::create_dir(&root.join("work").join("repo"));
    stub_owner_only_tree(&root);
    let tree = target_dir(&root, "target-repo", 30);
    let uid = owner_uid(&tree);
    let setpriv_log = root.join("setpriv-log.txt");

    let out = run(
        &root,
        &[
            ("STUB_RM_REFUSES", tree.to_str().expect("utf-8 path")),
            ("STUB_SETPRIV_REFUSES", "1"),
            (
                "STUB_SETPRIV_LOG",
                setpriv_log.to_str().expect("utf-8 path"),
            ),
        ],
    );
    let text = say(&out);
    assert!(
        tree.exists(),
        "the fixture's tree cannot be removed\n{text}"
    );
    assert_eq!(
        out.status.code(),
        Some(1),
        "an unremoved tree is loud\n{text}"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        stderr.matches("Permission denied").count(),
        1,
        "fifty refusals are said once, with their count, not fifty times\n{text}"
    );
    assert!(
        stderr.contains("50 path(s)") && stderr.contains(&format!("uid {uid}")),
        "the one line carries the count and the owner\n{text}"
    );
    let log = curl_log(&root);
    let put = log
        .lines()
        .find(|l| is_run_step_merge(l))
        .unwrap_or_else(|| panic!("a pass with a problem records itself\n{log}\n{text}"));
    assert!(
        put.contains("\"problems\":\"1\"") && put.contains("\"result\":\"incomplete\""),
        "the unremoved tree is the pass's one problem\n{put}\n{text}"
    );
    let output = recorded_output(put);
    let lines: Vec<&str> = output.lines().collect();
    assert_eq!(lines.len(), 1, "one problem, one line\n{output}");
    assert!(
        lines[0].starts_with("RED dev-scratch-reclaim/target-not-removed problem: ")
            && lines[0].contains(tree.to_str().expect("utf-8 path"))
            && lines[0].contains(&format!("uid {uid}"))
            && lines[0].contains("50 path(s)")
            && lines[0].contains("Permission denied"),
        "the packet names the tree, its owner, the count and the first refusal\n{output}"
    );
}

/// EVERY problem is named, not only that one: the count and the names
/// are written by one function, so a problem cannot be counted without
/// its line. A skipped worktree pass — the packet's oldest named
/// problem, `worktree_pass_reason` — is the behavioural case; the
/// script's own text is the rest.
#[test]
fn every_problem_the_pass_counts_is_named_on_the_packet() {
    let script = std::fs::read_to_string(repo_root().join("infra/cluster/dev-scratch-reclaim.sh"))
        .expect("read the script");
    let bumps: Vec<&str> = script
        .lines()
        .filter(|l| l.contains("problems=$((problems + 1))"))
        .collect();
    assert_eq!(
        bumps.len(),
        1,
        "the problem count moves in ONE place, `problem`, which also writes the name — a \
         second site is a problem the packet cannot name:\n{bumps:#?}"
    );

    let root = boss_testing::scratch_dir("boss-dsr-named-problems");
    let _guard = Scratch(root.clone());
    let yard = Yard::new(&root);
    yard.worktree("agent-gone", Some("feat/gone"), 200);
    git(
        &yard.repo,
        0,
        &["update-ref", "-d", "refs/remotes/origin/main"],
    );
    let out = run(&root, &[]);
    let text = say(&out);
    let log = curl_log(&root);
    let put = log
        .lines()
        .find(|l| is_run_step_merge(l))
        .unwrap_or_else(|| panic!("the run step is completed\n{log}\n{text}"));
    let output = recorded_output(put);
    let problems = serde_json::from_str::<serde_json::Value>(
        put.split_once("/metadata ").map(|(_, b)| b).unwrap_or("{}"),
    )
    .ok()
    .and_then(|v| {
        v.get("problems")
            .and_then(|p| p.as_str())
            .map(str::to_string)
    })
    .unwrap_or_default();
    assert_eq!(
        output.lines().count().to_string(),
        problems,
        "one RED line per counted problem\n{output}\n{put}"
    );
    assert!(
        output.lines().any(|l| l
            .starts_with("RED dev-scratch-reclaim/worktree-pass-skipped problem: ")
            && l.contains("origin/main")),
        "the skipped worktree pass is named with its reason\n{output}"
    );
    assert!(
        output
            .lines()
            .all(|l| l.starts_with("RED dev-scratch-reclaim/")
                && l.split_whitespace().nth(2) == Some("problem:")),
        "every line is the chore judge's shape, `RED <route> <kind>: <error>`\n{output}"
    );
}

#[test]
fn durable_gate_index_drift_cannot_reclaim_a_live_original_checkout() {
    let root = boss_testing::scratch_dir("boss-dsr-durable-index");
    let _guard = Scratch(root.clone());
    let yard = Yard::new(&root);
    let tree = yard.worktree("agent-original-open", Some("fix/original-open"), 2);
    let source = serde_json::json!({"worktree":path_str(&tree),"branch":"fix/original-open","agent_run":"runoriginal","sha":"head"});
    let envelope = serde_json::json!({"contract_version":1,"source":source});
    write_record(
        &root,
        serde_json::json!({"gate_runs":[{
        "id":"durablegate","worktree":path_str(&tree),"branch":"fix/original-open","agent_run":"runwrong","sha":"head",
        "gate_submission":envelope,"_immutable_metadata":{"gate_submission":envelope}}],
        "runs":{"runoriginal":"open","runwrong":"closed"}}),
    );
    let free = under_the_work_floor();
    let out = run(&root, &[("STUB_DF_WORK_GB", &free)]);
    assert!(
        tree.exists(),
        "an inconsistent raw locator cannot substitute a closed run for the original open owner\n{}",
        say(&out)
    );
}

/// A gate-run as `boss gate` files it now: the flat stamps, and the same
/// source bound immutably in its submission.
fn submitted_gate(
    id: &str,
    flat_worktree: &str,
    source_worktree: &str,
    run: &str,
) -> serde_json::Value {
    let source = serde_json::json!({"worktree":source_worktree,"branch":"fix/x","agent_run":run,"sha":"head"});
    let envelope = serde_json::json!({"contract_version":1,"source":source});
    serde_json::json!({"id":id,"worktree":flat_worktree,"branch":"fix/x","agent_run":run,"sha":"head",
        "gate_submission":envelope,"_immutable_metadata":{"gate_submission":envelope}})
}

/// THE READ THE API ACCEPTS (backlog 7c9715b6). The jobs API's
/// `metadata=` is a flat object of string values; from 2026-10-16 (#1206)
/// to 2026-10-22 this pass asked it for the nested submission source,
/// was refused 400 on every pass, and kept every unpushed tree 48h while
/// /work sat under its floor. Both trees here landed on record — their
/// runs closed — and both carry a submission, so both are taken; every
/// `metadata=` the pass sends is one the server takes, the submitted
/// gate-runs are read ONCE for the pass, and nothing says the record
/// could not be read.
#[test]
fn the_gate_run_reads_are_ones_the_api_accepts_and_the_submitted_read_is_once_a_pass() {
    let root = boss_testing::scratch_dir("boss-dsr-flat-gate-read");
    let _guard = Scratch(root.clone());
    let yard = Yard::new(&root);
    let first = yard.worktree("agent-flat-one", Some("worktree-agent-flat-one"), 2);
    let second = yard.worktree("agent-flat-two", Some("worktree-agent-flat-two"), 2);
    write_record(
        &root,
        serde_json::json!({
            "gate_runs": [
                submitted_gate("gateone", &path_str(&first), &path_str(&first), "run0one"),
                submitted_gate("gatetwo", &path_str(&second), &path_str(&second), "run0two"),
            ],
            "runs": {"run0one": "closed", "run0two": "closed"},
        }),
    );
    let out = run(&root, &[("STUB_DF_WORK_GB", &under_the_work_floor())]);
    let text = say(&out);
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("could not be read"),
        "a read the API accepts is read\n{text}"
    );
    assert!(
        !first.exists() && !second.exists(),
        "both runs closed: both trees landed on record and are taken\n{text}"
    );
    let log = curl_log(&root);
    assert_eq!(
        log.lines()
            .filter(|l| l.contains("kind=gate-run") && l.contains("metadata_has=gate_submission"))
            .count(),
        1,
        "the submitted gate-runs are read once a pass, not once a tree\n{log}"
    );
    assert!(
        !log.contains("gate_submission%22%3A%7B"),
        "no nested containment document is sent\n{log}"
    );
}

/// A submission naming this tree whose FLAT stamp was moved elsewhere is
/// a gate-run the flat read cannot see, and it may be the open run that
/// keeps the tree. The pass fails closed on it rather than judging the
/// tree by the gate-runs it can see — here one whose run closed.
#[test]
fn a_submission_whose_flat_stamp_moved_off_the_tree_fails_closed() {
    let root = boss_testing::scratch_dir("boss-dsr-moved-stamp");
    let _guard = Scratch(root.clone());
    let yard = Yard::new(&root);
    let tree = yard.worktree("agent-moved", Some("worktree-agent-moved"), 2);
    write_record(
        &root,
        serde_json::json!({
            "gate_runs": [
                {"id": "gatedone", "worktree": path_str(&tree), "branch": "fix/x", "agent_run": "run0done"},
                submitted_gate("gatemove", "/work/elsewhere", &path_str(&tree), "run0open"),
            ],
            "runs": {"run0done": "closed", "run0open": "open"},
        }),
    );
    let out = run(&root, &[("STUB_DF_WORK_GB", &under_the_work_floor())]);
    let text = say(&out);
    assert!(
        tree.exists(),
        "a submission the flat read missed keeps the tree\n{text}"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("name it only in their submission"),
        "and the latch names why\n{text}"
    );
}

/// A failed read says WHAT it was answered: the latch carried only "a
/// shape this pass cannot read" while every pass was refused 400, and
/// the 400 sat in an evidence file nobody opened (backlog 7c9715b6).
#[test]
fn a_gate_read_that_fails_names_what_it_was_answered() {
    let root = boss_testing::scratch_dir("boss-dsr-said");
    let _guard = Scratch(root.clone());
    let yard = Yard::new(&root);
    let tree = yard.worktree("agent-said", Some("worktree-agent-said"), 2);
    write_record(
        &root,
        serde_json::json!({
            "gate_runs": [submitted_gate("gatesaid", &path_str(&tree), &path_str(&tree), "run0said")],
            "runs": {"run0said": "closed"},
        }),
    );
    let out = run(
        &root,
        &[
            ("STUB_DF_WORK_GB", &under_the_work_floor()),
            ("STUB_SUBMITTED_RC", "22"),
        ],
    );
    let text = say(&out);
    assert!(tree.exists(), "an unread record is no landing\n{text}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr
            .lines()
            .any(|l| l.contains("could not be read") && l.contains("returned error: 503")),
        "the latch carries what the read was answered\n{text}"
    );
    let put = run_step_put(&root, &text);
    assert!(
        put.contains("returned error: 503"),
        "and so does the packet\n{put}\n{text}"
    );
}

// ---------------------------------------------------------------------
// THE AGENT-ARTIFACT INVENTORY (backlog 25022367, design 195276f0): the
// hourly pass runs the lifecycle's dry run and records its report; the
// dry run changes nothing it reads, and one that did not happen is a
// problem the packet names. dev-artifact-inventory_test.py holds the
// accounting itself (dev_artifact_inventory_py.rs runs it).
// ---------------------------------------------------------------------

/// A fixture /work carrying one artifact per family the tree's policy
/// names, each with its bytes and mtime recorded to compare afterwards.
fn plant_artifacts(root: &Path) -> Vec<(PathBuf, Vec<u8>, std::time::SystemTime)> {
    let work = root.join("work");
    boss_testing::create_dir(&work.join("wt"));
    boss_testing::create_dir(&work.join("repo"));
    [
        "home/.codex/scratch/coordinator-x/run-a/snapshot.json",
        "home/.codex/sessions/2026/10/16/rollout.jsonl",
        "home/.claude/projects/-work-boss/session.jsonl",
    ]
    .iter()
    .map(|rel| {
        let path = work.join(rel);
        boss_testing::create_dir(path.parent().expect("a parent"));
        boss_testing::write_file(&path, &format!("evidence of {rel}\n"));
        touch_at(&path, 72);
        let meta = std::fs::metadata(&path).expect("stat");
        (
            path.clone(),
            std::fs::read(&path).expect("read"),
            meta.modified().expect("mtime"),
        )
    })
    .collect()
}

#[test]
fn the_hourly_pass_records_the_artifact_dry_run_and_changes_nothing_it_read() {
    let root = boss_testing::scratch_dir("boss-dsr-artifact-inventory");
    let _guard = Scratch(root.clone());
    let planted = plant_artifacts(&root);
    let out = run(&root, &[("BOSS_ARTIFACT_INVENTORY", "on")]);
    let text = say(&out);
    let report_path = root
        .join("work")
        .join("telemetry")
        .join("artifact-inventory")
        .join("latest.json");
    let report: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&report_path)
            .unwrap_or_else(|e| panic!("the pass wrote the report: {e}\n{text}")),
    )
    .expect("the report is JSON");
    assert_eq!(report["mutated"], false, "{text}");
    assert_eq!(report["candidates"], serde_json::json!([]), "{text}");
    let families = report["families"].as_array().expect("families");
    assert_eq!(families.len(), 3, "{report}");
    for family in families {
        assert_eq!(family["status"], "read", "{family}");
        assert_eq!(family["files"], 1, "{family}");
    }
    for (path, bytes, mtime) in &planted {
        assert_eq!(
            &std::fs::read(path).expect("still there"),
            bytes,
            "{}",
            path.display()
        );
        assert_eq!(
            &std::fs::metadata(path)
                .expect("stat")
                .modified()
                .expect("mtime"),
            mtime,
            "{}",
            path.display()
        );
    }
    let ring = std::fs::read_to_string(report_path.with_file_name("growth.jsonl")).expect("ring");
    assert_eq!(ring.lines().count(), 1, "one totals line per pass\n{ring}");
    assert!(
        String::from_utf8_lossy(&out.stdout)
            .contains("artifact-inventory: dry run, inventory only"),
        "the pass says what the dry run found\n{text}"
    );
    assert!(
        !curl_log(&root).contains("artifact-inventory-failed"),
        "a dry run that ran is no problem\n{text}"
    );
}

#[test]
fn an_artifact_dry_run_that_did_not_happen_is_a_problem_the_packet_names() {
    let root = boss_testing::scratch_dir("boss-dsr-artifact-inventory-refused");
    let _guard = Scratch(root.clone());
    plant_artifacts(&root);
    // A file where the report's directory should be: the dry run reads
    // the tree and cannot write what it found.
    let blocked = root.join("blocked");
    boss_testing::write_file(&blocked, "not a directory");
    let out = run(
        &root,
        &[
            ("BOSS_ARTIFACT_INVENTORY", "on"),
            (
                "BOSS_ARTIFACT_INVENTORY_DIR",
                blocked.join("inventory").to_str().expect("utf-8 path"),
            ),
        ],
    );
    let text = say(&out);
    assert_eq!(
        out.status.code(),
        Some(1),
        "an unwritten dry run is loud\n{text}"
    );
    let log = curl_log(&root);
    let put = log
        .lines()
        .rfind(|l| is_run_step_merge(l))
        .unwrap_or_else(|| panic!("a pass with a problem records itself\n{log}\n{text}"));
    let output = recorded_output(put);
    assert!(
        output.lines().any(|l| l
            .starts_with("RED dev-scratch-reclaim/artifact-inventory-failed problem: ")
            && l.contains("exited 73")
            && l.contains("latest.json")),
        "the packet names the failed dry run, its exit and the path it could not write\n{output}"
    );
}

// THE COMPLETION JOIN (backlog 25022367, design 195276f0, its second
// car): the lifecycle reads each Codex run directory's agent run from the
// system of record, through the pass's own curl helper, signed as the
// pass and carrying the machine token like every other read it makes.
// An answer it cannot read is a retained owner, never a completed one;
// the tree's policy grants no action, so the pass stays a dry run.
#[test]
fn the_hourly_pass_joins_a_codex_run_to_its_record_with_the_machine_token_and_moves_nothing() {
    let root = boss_testing::scratch_dir("boss-dsr-artifact-join");
    let _guard = Scratch(root.clone());
    let work = root.join("work");
    boss_testing::create_dir(&work.join("wt"));
    boss_testing::create_dir(&work.join("repo"));
    let snap = work.join("home/.codex/scratch/coordinator-x/run-0123abcd/snapshot.json");
    boss_testing::create_dir(snap.parent().expect("a parent"));
    boss_testing::write_file(&snap, &"{\"evidence\": true}\n".repeat(400));
    touch_at(&snap, 72);
    let before = std::fs::read(&snap).expect("read");
    // A whole process table (pid 1 alone, holding nothing), so liveness
    // is known and the join is reached at all.
    let proc_root = fake_proc(&root, "pause", &[]);
    let (tok, hdr, expect) = token_mount(&root, &format!("{FIXTURE_TOKEN}\n"));
    // Two days on: a fixture's ctime is always now, and the settle
    // window reads it, so the pass's clock moves instead.
    let later = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after the epoch")
        .as_secs()
        + 2 * 86_400)
        .to_string();
    let out = run(
        &root,
        &[
            ("BOSS_ARTIFACT_INVENTORY", "on"),
            ("BOSS_ARTIFACT_INVENTORY_NOW", &later),
            ("PROC_ROOT", proc_root.to_str().expect("utf8")),
            ("BOSS_MACHINE_TOKEN_DIR", &tok),
            ("BOSS_MACHINE_TOKEN_HOSTS", "sor.test"),
            ("RUNTIME_DIRECTORY", &hdr),
            ("STUB_EXPECT_HDR", &expect),
        ],
    );
    let text = say(&out);
    let read: Vec<(String, String)> = stamps(&root)
        .into_iter()
        .filter(|(_, r)| r.contains("/api/jobs/0123abcd"))
        .collect();
    assert_eq!(
        read.len(),
        1,
        "the join reads the run its directory names, once\n{read:#?}\n{text}"
    );
    assert_eq!(read[0].0, "stamped", "{read:#?}\n{text}");
    let report: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(work.join("telemetry/artifact-inventory/latest.json"))
            .unwrap_or_else(|e| panic!("the pass wrote the report: {e}\n{text}")),
    )
    .expect("the report is JSON");
    let unit = report["families"]
        .as_array()
        .expect("families")
        .iter()
        .flat_map(|f| f["units"].as_array().cloned().unwrap_or_default())
        .find(|u| u["unit"] == "coordinator-x/run-0123abcd")
        .unwrap_or_else(|| panic!("the unit is reported\n{report}"));
    // The stub answers this read with no status code: unread, so retained.
    assert_eq!(unit["join"]["reason"], "owner-unread", "{unit}");
    assert_eq!(report["mode"], "dry-run", "{report}");
    assert_eq!(report["candidates"], serde_json::json!([]), "{report}");
    assert_eq!(std::fs::read(&snap).expect("still there"), before);
}

/// The launch refs in the fixture checkout, by run.
fn launch_refs(yard: &Yard) -> Vec<String> {
    git(
        &yard.repo,
        0,
        &[
            "for-each-ref",
            "--format=%(refname:lstrip=2)",
            "refs/boss-launch/",
        ],
    )
    .lines()
    .map(str::to_string)
    .collect()
}

/// Backlog af54aae4 (2). `boss dispatch --launch` fetches main into
/// refs/boss-launch/<run> and deletes it once the worktree stands on it;
/// a dispatch killed outright mid-fetch leaves the ref — and, killed
/// mid-update, its `.lock` — and nothing removed either. The pass
/// removes them once the RUN is closed on record (the silence clock
/// closes a run such a launch leaves open) and keeps an open run's. One
/// ref is PACKED, because `git gc --auto` packs refs and a packed ref has
/// no age to read; one lock has no ref beside it, as git leaves when it
/// is killed taking a new one.
#[test]
fn a_launch_ref_whose_run_is_closed_is_removed_and_an_open_runs_is_kept() {
    let root = boss_testing::scratch_dir("boss-dsr-launch-refs");
    let _guard = Scratch(root.clone());
    let yard = Yard::new(&root);
    let head = git(&yard.repo, 0, &["rev-parse", "HEAD"]);
    git(
        &yard.repo,
        0,
        &["update-ref", "refs/boss-launch/run0packed", &head],
    );
    git(&yard.repo, 0, &["pack-refs", "--all"]);
    for run in ["run0loose", "run0open"] {
        git(
            &yard.repo,
            0,
            &["update-ref", &format!("refs/boss-launch/{run}"), &head],
        );
    }
    let dir = yard.repo.join(".git/refs/boss-launch");
    assert!(
        !dir.join("run0packed").exists(),
        "the fixture: run0packed is packed"
    );
    for lock in ["run0loose.lock", "run0lock.lock"] {
        boss_testing::write_file(&dir.join(lock), &format!("{head}\n"));
    }
    write_record(
        &root,
        serde_json::json!({
            "runs": {"run0packed": "closed", "run0loose": "closed", "run0lock": "closed", "run0open": "open"},
        }),
    );

    let out = run(&root, &[]);
    let text = say(&out);
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert_eq!(
        launch_refs(&yard),
        ["run0open"],
        "a closed run's launch ref goes, packed or loose; an open run's stays\n{text}"
    );
    for lock in ["run0loose.lock", "run0lock.lock"] {
        assert!(
            !dir.join(lock).exists(),
            "{lock}: a closed run's lock goes, with or without its ref\n{text}"
        );
    }
    let log = curl_log(&root);
    for run in ["run0packed", "run0loose", "run0lock", "run0open"] {
        assert!(
            log.contains(&format!("/api/jobs/{run} ")),
            "{run}: the record was asked\n{log}"
        );
    }
    let put = run_step_put(&root, &text);
    assert!(
        put.contains("\"launch_refs_removed\":\"3\"")
            && put.contains("\"launch_refs_kept_open\":\"1\"")
            && put.contains("run0lock"),
        "the packet counts and names what went, and counts what stayed\n{put}\n{text}"
    );
    // The deletion is git the observer knows by name: an operation it
    // does not know turns the whole ledger UNKNOWN on every pass that
    // removes a ref.
    let body: serde_json::Value =
        serde_json::from_str(put.split_once("/metadata ").expect("a body").1).expect("json");
    let ledger: serde_json::Value = serde_json::from_str(
        body["git_phase_ledger"]
            .as_str()
            .expect("phase state is recorded"),
    )
    .expect("the ledger is json");
    assert_eq!(ledger["state"], "observed", "{ledger}");
}

/// A record that cannot be read keeps every launch ref — a closed run's
/// included — and is asked once a pass, not once a ref.
#[test]
fn a_record_that_cannot_be_read_keeps_every_launch_ref_and_is_asked_once() {
    let root = boss_testing::scratch_dir("boss-dsr-launch-refs-dark");
    let _guard = Scratch(root.clone());
    let yard = Yard::new(&root);
    let head = git(&yard.repo, 0, &["rev-parse", "HEAD"]);
    for run in ["run0first", "run0second"] {
        git(
            &yard.repo,
            0,
            &["update-ref", &format!("refs/boss-launch/{run}"), &head],
        );
    }
    write_record(
        &root,
        serde_json::json!({"runs": {"run0first": "closed", "run0second": "closed"}}),
    );

    let out = run(&root, &[("STUB_RECORD_RC", "7")]);
    let text = say(&out);
    assert_eq!(
        launch_refs(&yard),
        ["run0first", "run0second"],
        "an unread record removes nothing\n{text}"
    );
    let asked = curl_log(&root)
        .lines()
        .filter(|l| l.contains("/api/jobs/run"))
        .count();
    assert_eq!(asked, 1, "a dark record is asked once a pass\n{text}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("every launch ref is kept this pass"),
        "the dark read is said out loud\n{text}"
    );
}

// ---------------------------------------------------------------------
// THE DOORS' TREE IS HELD BY ITS OWNER (backlog 3db83d0d; reviewer run
// 6d7b418b of car c93c6dfa, measured on the pod 2026-10-19 08:11Z).
// /work/tools was drwxrwsr-x root:1500 with no sticky bit, and so were
// /work/tools/bin and /work/tools/image-cli, with kubectl and the image
// CLI's wrapper 0775 inside them. Every builder under as-gate-uid is
// group 1500, and a group member may rename an entry of a group-writable
// directory that has no sticky bit: it could move /work/tools/dev-control
// aside and plant a store of its own (an older generation, its own
// launcher), or overwrite kubectl outright. /work itself is 2775 too, so
// the same rename moves /work/tools aside whole. The hourly pass is the
// versioned owner of these modes: it runs as the uid that owns the tree,
// takes group and other write off /work/tools, bin and image-cli and off
// each entry directly inside them, and sets the sticky bit on /work, the
// one change there the pod's fsGroupChangePolicy (OnRootMismatch, which
// re-walks the whole volume when the root loses group rwx) leaves alone.
// ---------------------------------------------------------------------

fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::symlink_metadata(path)
        .unwrap_or_else(|e| panic!("stat {}: {e}", path.display()))
        .permissions()
        .mode()
        & 0o7777
}

fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    // mode-bits-ok: the fixture's directories and doors, given the modes the pod's fsGroup left them
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .unwrap_or_else(|e| panic!("chmod {mode:o} {}: {e}", path.display()));
}

/// The doors' tree as the pod held it on 2026-10-19: /work and
/// /work/tools, bin and image-cli group-writable, kubectl and the image
/// CLI's wrapper 0775, a door link in bin, and the dev-control store
/// (2700 on the pod) recording isolation off, so its own check is quiet.
fn doors_tree(root: &Path) -> PathBuf {
    let work = root.join("work");
    let tools = work.join("tools");
    for d in ["bin", "image-cli", "dev-control"] {
        boss_testing::create_dir(&tools.join(d));
    }
    boss_testing::write_exec(&tools.join("bin/kubectl"), "#!/bin/sh\n");
    boss_testing::write_exec(&tools.join("image-cli/boss"), "#!/bin/sh\n");
    std::os::unix::fs::symlink(work.join("repo/infra/dev/boss"), tools.join("bin/boss"))
        .expect("door link");
    let off = tools.join("dev-control/off.json");
    std::fs::write(&off, "{\"state\":\"off\"}").expect("off record");
    set_mode(&off, 0o600);
    for (p, m) in [
        (work.clone(), 0o775),
        (tools.clone(), 0o775),
        (tools.join("bin"), 0o775),
        (tools.join("image-cli"), 0o775),
        (tools.join("dev-control"), 0o700),
        (tools.join("bin/kubectl"), 0o775),
        (tools.join("image-cli/boss"), 0o775),
    ] {
        set_mode(&p, m);
    }
    tools
}

#[test]
fn every_pass_takes_group_and_other_write_off_the_doors_tree() {
    let root = boss_testing::scratch_dir("boss-dsr-tools-held");
    let _guard = Scratch(root.clone());
    let _yard = Yard::new(&root);
    let tools = doors_tree(&root);
    let work = root.join("work");
    let held = [
        tools.clone(),
        tools.join("bin"),
        tools.join("image-cli"),
        tools.join("bin/kubectl"),
        tools.join("image-cli/boss"),
    ];

    let out = run(&root, &[]);
    let text = say(&out);
    assert!(out.status.success(), "{text}");
    for p in &held {
        assert_eq!(
            mode_of(p) & 0o022,
            0,
            "{} is {:o}: a group member could still replace what the doors run\n{text}",
            p.display(),
            mode_of(p)
        );
    }
    assert_eq!(
        mode_of(&tools.join("bin/kubectl")) & 0o755,
        0o755,
        "a door stays runnable\n{text}"
    );
    assert_eq!(
        mode_of(&tools.join("dev-control")),
        0o700,
        "a private store is left as it is\n{text}"
    );
    let w = mode_of(&work);
    assert_ne!(
        w & 0o1000,
        0,
        "the work mount is sticky, so /work/tools cannot be renamed aside ({w:o})\n{text}"
    );
    assert_eq!(
        w & 0o070,
        0o070,
        "the work mount keeps group rwx, or the kubelet's OnRootMismatch re-walks the volume ({w:o})\n{text}"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("doors' tree: held"),
        "the pass says what it holds\n{text}"
    );

    // The kubelet re-walks the volume whenever the root mismatches, and
    // any hand may chmod: the next pass holds the tree again and says so
    // on its packet (forced here by a refused selector, a problem of its
    // own that files the pass).
    set_mode(&tools, 0o775);
    set_mode(&tools.join("bin/kubectl"), 0o775);
    std::fs::remove_file(tools.join("dev-control/off.json")).expect("drop the off record");
    std::os::unix::fs::symlink("refused", tools.join("dev-control/enabled")).expect("refuse");
    let out = run(&root, &[]);
    let text = say(&out);
    assert_eq!(
        out.status.code(),
        Some(1),
        "the refused selector reds the pass\n{text}"
    );
    for p in &held {
        assert_eq!(mode_of(p) & 0o022, 0, "{} held again\n{text}", p.display());
    }
    let log = curl_log(&root);
    let put = log
        .lines()
        .find(|l| is_run_step_merge(l))
        .unwrap_or_else(|| panic!("the run step is completed\n{log}\n{text}"));
    assert!(
        put.contains("\"tools\":\"held, re-held 2 this pass\""),
        "the packet says the tree is held and how many entries this pass re-held\n{put}\n{text}"
    );
}

fn as_root() -> bool {
    String::from_utf8_lossy(&Command::new("id").arg("-u").output().expect("id -u").stdout).trim()
        == "0"
}

/// `argv` as uid 65534 — the gate's uid, read from
/// infra/gate-runner/gate-runner.yaml by infra/dev/as-gate-uid.sh, which
/// drops the same way — in group `gid` and no other: a member of the
/// tree's group that does not own it, which is what every builder is to
/// the pod's root:1500 tree.
fn as_a_group_member(gid: u32, argv: &[&str]) -> Output {
    Command::new("setpriv")
        .args(["--reuid=65534", &format!("--regid={gid}"), "--clear-groups"])
        .arg("--")
        .args(argv)
        .output()
        .expect("setpriv")
}

/// The finding itself, reproduced and then refused: a uid in the tree's
/// group that does not own it renames the dev-control store aside, moves
/// /work/tools aside whole and plants an entry of its own, until the
/// pass has held the tree; then it can do none of that and cannot
/// overwrite a door, and the entry it planted is a problem on the packet.
///
/// The pod has no CAP_CHOWN, so the fixture cannot be staged in group
/// 1500 as the pod's is; it keeps the group root created it with, and
/// the uid drops into THAT group. The kernel's rule is the same for any
/// group. Root is needed to drop to another uid at all, which the gate
/// (uid 65534) is not: there the test says NOT RUN on stderr and the
/// mode pin above is the gate's evidence.
#[test]
fn a_group_member_cannot_rename_the_dev_control_store_once_the_pass_holds_the_tree() {
    use std::os::unix::fs::MetadataExt;
    if !as_root() {
        eprintln!(
            "NOT RUN: dropping to another uid needs root; the mode pin above covers the gate"
        );
        return;
    }
    let root = boss_testing::scratch_dir("boss-dsr-tools-rename");
    let _guard = Scratch(root.clone());
    let _yard = Yard::new(&root);
    let tools = doors_tree(&root);
    let work = root.join("work");
    set_mode(&root, 0o755);
    for p in [&work, &tools, &tools.join("bin"), &tools.join("image-cli")] {
        set_mode(p, 0o2775);
    }
    let gid = std::fs::metadata(&tools).expect("stat tools").gid();
    let s = |p: &Path| p.to_str().expect("utf8").to_string();
    let store = s(&tools.join("dev-control"));
    let aside = s(&tools.join("dev-control.aside"));
    let tools_s = s(&tools);
    let tools_aside = s(&work.join("tools.aside"));
    let planted = s(&tools.join("bin/planted"));

    // The finding, reproduced on the unheld tree.
    for (what, argv) in [
        (
            "rename the store aside",
            vec!["mv", "-T", store.as_str(), aside.as_str()],
        ),
        ("and back", vec!["mv", "-T", aside.as_str(), store.as_str()]),
        (
            "move /work/tools aside whole",
            vec!["mv", "-T", tools_s.as_str(), tools_aside.as_str()],
        ),
        (
            "and back",
            vec!["mv", "-T", tools_aside.as_str(), tools_s.as_str()],
        ),
        (
            "plant an entry beside the doors",
            vec!["touch", planted.as_str()],
        ),
    ] {
        let out = as_a_group_member(gid, &argv);
        assert!(
            out.status.success(),
            "control: a group member can {what} before the pass\n{}",
            say(&out)
        );
    }

    // The pass holds every mode it owns, and cannot hold the entry it
    // does not (the sidecar has no CAP_CHOWN): a problem, by name.
    let out = run(&root, &[]);
    let text = say(&out);
    assert_eq!(
        out.status.code(),
        Some(1),
        "a foreign entry in the doors' tree reds the pass\n{text}"
    );
    let log = curl_log(&root);
    let put = log
        .lines()
        .rfind(|l| is_run_step_merge(l))
        .unwrap_or_else(|| panic!("the pass records itself\n{log}\n{text}"));
    let output = recorded_output(put);
    assert!(
        output.lines().any(
            |l| l.starts_with("RED dev-scratch-reclaim/tools-open problem: ")
                && l.contains("bin/planted")
                && l.contains("uid 65534")
        ),
        "the packet names the entry and its owner\n{output}"
    );
    assert!(
        put.contains("\"tools\":\"OPEN"),
        "and the tree's state says it is not held\n{put}"
    );

    let sibling = s(&tools.join("dc"));
    let kubectl = s(&tools.join("bin/kubectl"));
    let door = s(&tools.join("bin/boss"));
    for (what, argv) in [
        (
            "rename the store aside",
            vec!["mv", "-T", store.as_str(), aside.as_str()],
        ),
        (
            "move /work/tools aside whole",
            vec!["mv", "-T", tools_s.as_str(), tools_aside.as_str()],
        ),
        ("plant a sibling store", vec!["mkdir", sibling.as_str()]),
        (
            "overwrite kubectl",
            vec!["sh", "-c", "printf x > \"$1\"", "sh", kubectl.as_str()],
        ),
        (
            "replace a door link",
            vec!["ln", "-sfn", "/bin/true", door.as_str()],
        ),
    ] {
        let out = as_a_group_member(gid, &argv);
        assert!(
            !out.status.success(),
            "a group member could {what} after the pass held the tree\n{}",
            say(&out)
        );
    }
    assert!(
        tools.join("dev-control/off.json").exists(),
        "the store stayed put"
    );
}

// ---------------------------------------------------------------------
// THE TENANT CHECKOUTS (backlog 6b274be1, 2026-10-19).
//
// The pod keeps a checkout of every tenant repo instances.toml names at
// /work/<the repo's last segment> (brief.rs `tenant_checkout`), and
// nothing kept it current: measured that day, /work/algedonic-llc sat at
// 082d920 (2026-10-07) while the forge's main was 3306bbe (2026-10-12),
// and a triage measured the stale copy and filed a claim the forge had
// fixed three days before — a nomination and a builder run spent on it.
// Its own refs/remotes/origin/main WAS current (a fetch on 10-19 moved
// it); only HEAD lagged, which is exactly the shape the /work/boss pass
// above repairs with no network. These pin the same pass over the
// tenant checkouts, read off the instances file the converge reads.
// ---------------------------------------------------------------------

/// A tenant checkout at `work/<name>` with a bare forge of its own, and
/// `behind` commits on that forge's `main` the checkout has FETCHED but
/// not merged — refs/remotes/origin/main moved, HEAD did not: the state
/// /work/algedonic-llc was measured in on 2026-10-19 (`main...origin/main
/// [behind 5]`).
fn tenant_checkout(root: &Path, name: &str, behind: usize) -> PathBuf {
    let forge = root.join(format!("{name}.git"));
    boss_testing::create_dir(&forge);
    git(&forge, 0, &["init", "--bare", "-q"]);
    let checkout = root.join("work").join(name);
    boss_testing::create_dir(&checkout);
    git(&checkout, 100, &["init", "-q"]);
    boss_testing::write_file(&checkout.join("tenant.toml"), "seed\n");
    git(&checkout, 100, &["add", "."]);
    git(&checkout, 100, &["commit", "-q", "-m", "seed"]);
    git(
        &checkout,
        100,
        &["remote", "add", "origin", forge.to_str().expect("utf8")],
    );
    git(&checkout, 100, &["push", "-q", "origin", "main"]);
    // Somebody else lands on the tenant's main, and a session's fetch
    // brings the ref home without moving the checkout.
    let upstream = root.join(format!("{name}-upstream"));
    git(
        root,
        0,
        &[
            "clone",
            "-q",
            forge.to_str().expect("utf8"),
            upstream.to_str().expect("utf8"),
        ],
    );
    for i in 0..behind {
        boss_testing::write_file(&upstream.join(format!("landed-{i}.txt")), "landed\n");
        git(&upstream, 0, &["add", "."]);
        git(
            &upstream,
            0,
            &["commit", "-q", "-m", &format!("landed {i}")],
        );
    }
    if behind > 0 {
        git(&upstream, 0, &["push", "-q", "origin", "main"]);
    }
    git(&checkout, 0, &["fetch", "-q", "origin"]);
    checkout
}

/// An instances file in the shape infra/cluster/instances.toml has: a
/// tenant_repo instance, a tenant_dir one, and the commented example of
/// the flip that file carries — which is prose, not an instance.
fn tenant_instances(root: &Path, extra: &str) -> PathBuf {
    let path = root.join("instances.toml");
    boss_testing::write_file(
        &path,
        &format!(
            concat!(
                "# THE FLIP, as the real file spells it in a comment:\n",
                "#     tenant_repo = \"acme/commented-out\"\n",
                "#     tenant_ref = \"main\"\n",
                "source = \"prod\"\n\n",
                "[prod]\n",
                "namespace = \"boss\"\n",
                "tenant_repo = \"acme/acme-co\"\n",
                "tenant_ref = \"main\"\n",
                "sim = false\n\n",
                "[playground]\n",
                "namespace = \"boss-playground\"\n",
                "tenant_dir = \"examples/brewery\"\n",
                "{extra}"
            ),
            extra = extra
        ),
    );
    path
}

fn tenant_run(root: &Path, instances: &Path) -> Output {
    run(
        root,
        &[("BOSS_INSTANCES", instances.to_str().expect("utf8"))],
    )
}

#[test]
fn a_tenant_checkout_behind_its_fetched_ref_is_fast_forwarded_with_no_network() {
    let root = boss_testing::scratch_dir("boss-dsr-tenant-ff");
    let _guard = Scratch(root.clone());
    let _yard = Yard::new(&root);
    let tenant = tenant_checkout(&root, "acme-co", 4);
    let target = git(&tenant, 0, &["rev-parse", "refs/remotes/origin/main"]);
    assert_ne!(head_sha(&tenant), target, "the fixture starts behind");
    // A second tenant instance whose checkout this pod does not hold.
    let instances = tenant_instances(
        &root,
        "\n[other]\nnamespace = \"boss-other\"\ntenant_repo = \"acme/not-cloned-here\"\ntenant_ref = \"main\"\n",
    );

    let out = tenant_run(&root, &instances);
    let text = say(&out);
    assert_eq!(
        head_sha(&tenant),
        target,
        "the tenant checkout is taken to the origin/main it already fetched\n{text}"
    );
    assert!(
        out.status.success(),
        "a tenant fast-forward is the pass working, and a tenant this pod holds \
         no checkout of is not a problem\n{text}"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(&tenant.display().to_string()) && stdout.contains(&target[..8]),
        "the pass names the tenant checkout it moved and the sha it moved it to\n{text}"
    );
    assert!(
        ls_remote_calls(&root).is_empty(),
        "no network: the sidecar holds no forge credential (b50a65ef)\n{text}"
    );
    assert!(
        !stdout.contains("commented-out"),
        "a commented line in the instances file names no checkout\n{text}"
    );
    assert!(
        !curl_log(&root).contains("POST"),
        "a routine tenant fast-forward files no packet, like the checkout's own\n{}\n{text}",
        curl_log(&root)
    );
}

#[test]
fn a_tenant_checkout_ahead_of_its_ref_is_left_alone_quietly() {
    let root = boss_testing::scratch_dir("boss-dsr-tenant-ahead");
    let _guard = Scratch(root.clone());
    let _yard = Yard::new(&root);
    let tenant = tenant_checkout(&root, "acme-co", 0);
    // A commit of its own, not yet pushed: somebody's next push, not a
    // copy that forgot to pull.
    boss_testing::write_file(&tenant.join("mine.txt"), "mine\n");
    git(&tenant, 0, &["add", "."]);
    git(&tenant, 0, &["commit", "-q", "-m", "mine"]);
    let before = head_sha(&tenant);

    let out = tenant_run(&root, &tenant_instances(&root, ""));
    let text = say(&out);
    assert_eq!(head_sha(&tenant), before, "never rewound\n{text}");
    assert!(out.status.success(), "and not a problem\n{text}");
    assert!(
        curl_log(&root).is_empty(),
        "and nothing is filed\n{}\n{text}",
        curl_log(&root)
    );
}

/// No door warns on a stale tenant copy the way door-freshness.sh warns
/// on /work/boss, so a tenant checkout this pass cannot bring current is
/// the silence the item measured unless the pass NAMES it: a problem on
/// the packet, with the checkout, where it stands and why.
#[test]
fn a_tenant_checkout_the_pass_cannot_bring_current_is_a_named_problem() {
    for shape in ["off-ref", "diverged", "refused"] {
        let root = boss_testing::scratch_dir(&format!("boss-dsr-tenant-stuck-{shape}"));
        let _guard = Scratch(root.clone());
        let _yard = Yard::new(&root);
        let tenant = tenant_checkout(&root, "acme-co", 1);
        let why = match shape {
            "off-ref" => {
                git(&tenant, 0, &["checkout", "-q", "-b", "site/inquiry-form"]);
                "site/inquiry-form"
            }
            "diverged" => {
                boss_testing::write_file(&tenant.join("mine.txt"), "mine\n");
                git(&tenant, 0, &["add", "."]);
                git(&tenant, 0, &["commit", "-q", "-m", "mine"]);
                "diverged"
            }
            _ => {
                // An untracked file the incoming commit adds: git refuses
                // rather than overwrite it.
                boss_testing::write_file(&tenant.join("landed-0.txt"), "uncommitted\n");
                "landed-0.txt"
            }
        };
        let before = head_sha(&tenant);

        let out = tenant_run(&root, &tenant_instances(&root, ""));
        let text = say(&out);
        assert_eq!(head_sha(&tenant), before, "{shape}: nothing moves\n{text}");
        assert_eq!(
            out.status.code(),
            Some(1),
            "{shape}: and reds the pass\n{text}"
        );
        let keys = run_step_keys(&root, &text);
        let output = keys["output"].as_str().unwrap_or("");
        assert!(
            output.contains("RED dev-scratch-reclaim/tenant-checkout-stuck problem: ")
                && output.contains(&tenant.display().to_string())
                && output.contains(why),
            "{shape}: the packet names the problem, the checkout and why ({why}): {keys}\n{text}"
        );
        assert!(
            keys["tenant_ff"].as_str().unwrap_or("").contains("acme-co"),
            "{shape}: the tenant pass's own verdict rides the packet: {keys}\n{text}"
        );
    }
}
