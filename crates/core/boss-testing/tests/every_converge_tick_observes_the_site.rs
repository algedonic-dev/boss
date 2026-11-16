//! EVERY converge tick observes the tenant SITE — what the instance's
//! gateway serves for `/` under the site hostname — and records it on
//! the run step beside the connector: `site` as an OBJECT (`host`,
//! `namespace`, `http_status`, `bytes`, `hash`, `observed_at`) and
//! `site_line` as prose. The lib function is RUN against a stub
//! `kubectl` (the gateway Service's address) and a stub `curl` (the
//! answer), the way every_converge_tick_observes_the_connector runs
//! `observe_connector`, so every field below is one it recorded and
//! every call below is one it made.
//!
//! WHY (backlog e114238a; design b64c4377). Site ops by protocol — the
//! tenant workflow publish-the-landing-page in david/algedonic-llc —
//! closes its loop on the converge's RECORD of what it served, never on
//! a belief: the publish step records `site_hash` = sha256 of
//! site/index.html at the merge, and the dispatcher rule
//! (jobs.complete_step_matching, event_path `steps.run.site.hash`)
//! completes the `live` step when a closed converge's run step carries
//! the same hash. So the converge must hash the BODY the gateway served
//! for `/`, exactly as received, and record it where the rule reads —
//! `site.hash` on the run step, a nested object. Unreachable is recorded
//! as such, in curl's own words, and never fails the converge: the site
//! is not what a train delivers.
//!
//! What each case pins: a 200 records status, byte count, the sha256 of
//! the body and the line; the request carries `Host: <site>` to the
//! address the gateway Service reports (read off the cluster, not
//! assumed); a refused connection records `unreachable (<curl's
//! words>)` with no hash and exits 0; a Service with no address records
//! unreachable without calling curl; no declared site records nothing;
//! several declared sites ride `sites` keyed by host with `site` the
//! source instance's; and both ticks — unchanged and deploying — call
//! the one function.
//!
//! EVERY STAGED PAGE, not only `/` (backlog 96e171bf; design f89a241f
//! D3). The tenant workflow publish-a-blog-post closes its `live` step
//! on `pages["/blog-<slug>.html"]` equal to its own `publish.site_hash`,
//! so `site.pages` maps `/<file>` to the sha256 of the body served for
//! it, for every `.html` key of the `boss-site` ConfigMap — the pages
//! the converge delivered, read off the cluster rather than assumed.
//! A page that answers anything but 200, or no answer at all, records
//! WHY in place of a hash; a ConfigMap that cannot be read records
//! `pages_unread` in kubectl's words; and none of it fails the tick.
//!
//! NOR HOLDS IT (review b843ec40 R1). Every cluster read the observation
//! makes is bounded — a ConfigMap or Service read that does not answer
//! within the bound is recorded as such and the observation goes on to
//! write its summary — and the page phase as a whole stops at a
//! deadline, recording `pages_unread` for what it did not reach.
//!
//! NO ANSWER RACES A CLOCK HERE (backlog dac8cf01). These cases used to
//! let the stubs SLEEP against the lib's real bounds and read the wall
//! clock after, and train gate 3082de78 lost two of them to a 280 s
//! io.full stall on the gate drive: an answered read was slower than
//! the 1 s bound the test had set, so the deadline the test meant for
//! the page phase fired before the ConfigMap was read. On that drive a
//! wall-clock budget of any size is the wrong shape, so the fixture
//! decides instead. The lib reads its clock through `date +%s` and
//! bounds each cluster read with `timeout`, both found on PATH, so the
//! stub `date` answers from a clock file only the stub curl advances
//! (one STUB_PAGE_TICK per page read), and the stub `timeout` runs an
//! ANSWERED read unbounded. The one read the fixture never answers
//! (STUB_HANG) goes through the REAL `timeout` with the lib's own
//! bound, and the stub hangs only when that bound is around it — a
//! read the lib forgot to bound fails at once, in words, rather than
//! hanging the test. Every answered call then waits STUB_STALL, longer
//! than any bound these cases set, and still nothing changes. The
//! wiring, the clock and that decision are `boss_testing::path_stub`'s,
//! shared with a_step_close_has_a_deadline.rs (backlog 59f4b65d).

use boss_testing::path_stub::{self, PathStub};
use boss_testing::{repo_root, scratch_dir, write_file};
use std::path::Path;
use std::process::Command;

const LIB: &str = "infra/forge/cluster-deploy-lib.sh";
const RUNNER: &str = "infra/forge/cluster-deploy-runner.sh";
const RUN_SUMMARY: &str = "infra/run-summary.sh";
const SITE: &str = "www.algedonic.dev";
/// The body the stub gateway answers: a trailing newline and a
/// non-ASCII byte, so a hash of anything but the bytes as received
/// would differ.
const BODY: &str = "<!doctype html>\n<title>Algedonic — BOSS</title>\n";

/// Instance rows as `render-instance.sh --instances` prints them:
/// nine tab-separated columns, the ninth the site.
fn rows(instances: &[(&str, &str, &str)]) -> String {
    instances
        .iter()
        .map(|(name, ns, site)| {
            format!("{name}\t{ns}\ttenant\tfalse\t{name}.example\t\t\t\t{site}\n")
        })
        .collect()
}

/// How late every ANSWERED stub call arrives: longer than every bound
/// and deadline these cases set (1 s), so an assertion that raced an
/// answer against a clock would lose here, as two did on gate 3082de78.
const ANSWER_STALL: &str = "2";

/// The stubs, built through `boss_testing::path_stub` (which owns the
/// PATH wiring, the call log, the fixture clock and the decision of
/// which call is never answered). The stub kubectl: logs every call;
/// answers `get svc boss-gateway` with the address in STUB_ADDR (empty =
/// no LoadBalancer address yet); NEVER answers the call whose argv
/// contains STUB_HANG, and hangs on it only inside the bound the stub
/// `timeout` reports — an unbounded read of it fails at once, saying so
/// (`stub_hang`). The stub curl: logs its argv, writes BODY to the `-o`
/// file, prints STUB_CODE — or, when STUB_CURL_FAIL is set, prints
/// curl's own words to stderr and exits 7 with nothing written; each
/// page read advances the fixture clock by STUB_PAGE_TICK seconds. The
/// stub `date` answers `+%s` from that clock, which reads 1000 until a
/// page read advances it, and passes every other form to the real one.
/// The stub `timeout` runs an answered call unbounded, and the STUB_HANG
/// call under the REAL `timeout` with the lib's own arguments.
fn stubs(dir: &Path) -> PathStub {
    let stubs = PathStub::new(dir).with_clock(1000);
    stubs.stub(
        "kubectl",
        r#"stub_log "$@"
if stub_never_answered "$@"; then stub_hang; fi
if [ -n "${STUB_STALL:-}" ]; then sleep "$STUB_STALL"; fi
case "$*" in
  *"get svc boss-gateway -n "*)
    printf '%s' "${STUB_ADDR-10.0.0.1:80}" ;;
  *"get configmap boss-site -n "*)
    if [ -n "${STUB_CM_FAIL:-}" ]; then
      echo 'Error from server (Forbidden): configmaps "boss-site" is forbidden' >&2
      exit 1
    fi
    for f in ${STUB_PAGES:-}; do printf '%s\n' "$f"; done ;;
esac
exit 0
"#,
    );
    stubs.stub(
        "curl",
        r#"stub_log "$@"
if [ -n "${STUB_STALL:-}" ]; then sleep "$STUB_STALL"; fi
out="" url=""
for a in "$@"; do case "$a" in http://*) url="$a" ;; esac; done
path="/${url#http://*/}"
if [ -n "${STUB_CURL_FAIL:-}" ] || [ "$path" = "${STUB_PAGE_FAIL:-}" ]; then
  echo "curl: (7) Failed to connect to 10.0.0.1 port 80 after 3 ms: Connection refused" >&2
  exit 7
fi
while [ $# -gt 0 ]; do
  case "$1" in -o) out="$2"; shift ;; esac
  shift
done
if [ "$path" = / ]; then
  printf '%s' "$STUB_BODY" > "$out"
  printf '%s' "${STUB_CODE-200}"
else
  if [ -n "${STUB_PAGE_TICK:-}" ]; then stub_clock_tick "$STUB_PAGE_TICK"; fi
  printf 'page %s\n' "$path" > "$out"
  if [ "$path" = "${STUB_PAGE_404:-}" ]; then printf 404; else printf 200; fi
fi
"#,
    );
    stubs.stub("timeout", path_stub::TIMEOUT_UNBOUNDED_WHEN_ANSWERED);
    stubs
}

struct Run {
    rc: i32,
    out: String,
    err: String,
    recorded: serde_json::Value,
    calls: String,
}

/// Runs `observe_sites` from the lib with the stub kubectl as `$K`
/// and the stub curl first on PATH, recording through the real
/// run-summary.sh.
fn observe(
    name: &str,
    instances: &str,
    source_ns: &str,
    skipped: &str,
    env: &[(&str, &str)],
) -> Run {
    let dir = scratch_dir(&format!("tick-observes-site-{name}"));
    let stubs = stubs(&dir);
    let script = format!(
        "set -euo pipefail\n. '{summary}'\n. '{lib}'\n\
         observe_sites '{k}' \"$STUB_INSTANCES\" '{source_ns}' \"$STUB_SKIPPED\"\n",
        summary = repo_root().join(RUN_SUMMARY).display(),
        lib = repo_root().join(LIB).display(),
        k = stubs.bin().join("kubectl").display(),
    );
    let summary = dir.join("summary.json");
    let mut cmd = Command::new("bash");
    stubs.wire(&mut cmd);
    cmd.arg("-c")
        .arg(script)
        .env("BOSS_RUN_SUMMARY_FILE", &summary)
        .env("STUB_INSTANCES", instances)
        .env("STUB_SKIPPED", skipped)
        .env("STUB_BODY", BODY)
        .current_dir(&dir);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    Run {
        rc: out.status.code().unwrap_or(-1),
        out: String::from_utf8_lossy(&out.stdout).to_string(),
        err: String::from_utf8_lossy(&out.stderr).to_string(),
        recorded: std::fs::read_to_string(&summary)
            .map(|s| serde_json::from_str(&s).unwrap())
            .unwrap_or(serde_json::Value::Null),
        calls: stubs.calls(),
    }
}

/// sha256 of BODY as `sha256sum` prints it — the same tool the lib
/// hashes with, run here on the same bytes.
fn body_sha256() -> String {
    sha256_of(BODY)
}

/// sha256 of `body` as `sha256sum` prints it.
fn sha256_of(body: &str) -> String {
    let dir = scratch_dir("tick-observes-site-sha");
    let f = dir.join("body");
    write_file(&f, body);
    let out = Command::new("sha256sum").arg(&f).output().unwrap();
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .unwrap()
        .to_string()
}

#[test]
fn a_served_site_records_status_bytes_and_the_hash_of_the_body_as_received() {
    let r = observe("served", &rows(&[("prod", "boss", SITE)]), "boss", "", &[]);
    assert_eq!(r.rc, 0, "{}\n{}", r.out, r.err);
    let site = &r.recorded["site"];
    assert!(
        site.is_object(),
        "`site` is an object the rule's event_path steps.run.site.hash can walk: {}",
        r.recorded
    );
    assert_eq!(site["host"], SITE, "{}", r.recorded);
    assert_eq!(site["namespace"], "boss", "{}", r.recorded);
    assert_eq!(site["http_status"], 200, "{}", r.recorded);
    assert_eq!(site["bytes"], BODY.len(), "{}", r.recorded);
    let sha = body_sha256();
    assert_eq!(
        site["hash"], sha,
        "the sha256 of the body bytes exactly as received: {}",
        r.recorded
    );
    assert!(
        site["observed_at"]
            .as_str()
            .is_some_and(|t| t.ends_with('Z') && t.len() == 20),
        "observed_at is a UTC instant: {}",
        r.recorded
    );
    assert!(site.get("unreachable").is_none(), "{}", r.recorded);
    assert_eq!(
        r.recorded["site_line"],
        format!(
            "{SITE} → boss: HTTP 200, {} bytes, sha256 {sha}",
            BODY.len()
        ),
        "{}",
        r.recorded
    );
    assert!(
        r.recorded.get("sites").is_none(),
        "one declared site rides `site` alone: {}",
        r.recorded
    );
    // The request: the address the Service reports, the site as Host,
    // a bounded wait, and the body to a file (never a digest of a
    // string a shell has touched).
    assert!(
        r.calls.contains("kubectl get svc boss-gateway -n boss"),
        "the reach is READ off the gateway Service: {}",
        r.calls
    );
    let curl = r
        .calls
        .lines()
        .find(|l| l.starts_with("curl "))
        .expect("curl was called");
    for want in [
        "-H Host: www.algedonic.dev",
        "http://10.0.0.1:80/",
        "--max-time 10",
        "-sS",
        "-o ",
    ] {
        assert!(curl.contains(want), "the request carries `{want}`: {curl}");
    }
    assert!(
        r.out.contains(&format!(
            "cluster-deploy-runner: site: {SITE} → boss: HTTP 200"
        )),
        "{}",
        r.out
    );
}

#[test]
fn an_answer_that_is_not_200_is_still_the_answer_recorded() {
    // A tenant with no site/ yet answers 404 (a_tenant_site_per_instance):
    // the converge records what it got, hash included, and never fakes
    // a 200.
    let r = observe(
        "404",
        &rows(&[("prod", "boss", SITE)]),
        "boss",
        "",
        &[("STUB_CODE", "404"), ("STUB_BODY", "not found\n")],
    );
    assert_eq!(r.rc, 0, "{}\n{}", r.out, r.err);
    assert_eq!(r.recorded["site"]["http_status"], 404, "{}", r.recorded);
    assert_eq!(r.recorded["site"]["bytes"], 10, "{}", r.recorded);
    assert!(
        r.recorded["site"]["hash"]
            .as_str()
            .is_some_and(|h| h.len() == 64),
        "{}",
        r.recorded
    );
}

#[test]
fn an_unreachable_site_is_recorded_in_curls_words_and_never_fails_the_converge() {
    let r = observe(
        "refused",
        &rows(&[("prod", "boss", SITE)]),
        "boss",
        "",
        &[("STUB_CURL_FAIL", "1")],
    );
    assert_eq!(
        r.rc, 0,
        "the site is not what a train delivers — the observation never fails the tick: {}\n{}",
        r.out, r.err
    );
    let site = &r.recorded["site"];
    assert_eq!(site["host"], SITE, "{}", r.recorded);
    assert_eq!(
        site["unreachable"],
        "curl: (7) Failed to connect to 10.0.0.1 port 80 after 3 ms: Connection refused",
        "curl's own words, verbatim: {}",
        r.recorded
    );
    for faked in ["hash", "http_status", "bytes"] {
        assert!(
            site.get(faked).is_none(),
            "nothing is faked for an answer that never came: {}",
            r.recorded
        );
    }
    assert_eq!(
        r.recorded["site_line"],
        format!(
            "{SITE} → boss: unreachable (curl: (7) Failed to connect to 10.0.0.1 port 80 after 3 ms: Connection refused)"
        ),
        "{}",
        r.recorded
    );

    // A Service with no LoadBalancer address yet: unreachable, named,
    // and curl is never asked to guess an address.
    let r = observe(
        "no-address",
        &rows(&[("prod", "boss", SITE)]),
        "boss",
        "",
        &[("STUB_ADDR", "")],
    );
    assert_eq!(r.rc, 0, "{}\n{}", r.out, r.err);
    assert_eq!(
        r.recorded["site"]["unreachable"],
        "no LoadBalancer address on Service boss-gateway in boss",
        "{}",
        r.recorded
    );
    assert!(
        !r.calls.contains("curl "),
        "no address, no request: {}",
        r.calls
    );

    // An instance the secret gate skipped has no gateway running: its
    // site is unreachable for the reason the packet already names.
    let r = observe(
        "skipped",
        &rows(&[
            ("prod", "boss", ""),
            ("stage", "boss-stage", "stage.example"),
        ]),
        "boss",
        "boss-stage (secrets absent: boss-secrets)",
        &[],
    );
    assert_eq!(r.rc, 0, "{}\n{}", r.out, r.err);
    assert_eq!(
        r.recorded["site"]["unreachable"], "boss-stage skipped (secrets absent)",
        "{}",
        r.recorded
    );
    assert!(!r.calls.contains("curl "), "{}", r.calls);
}

#[test]
fn no_declared_site_records_nothing() {
    let r = observe(
        "none",
        &rows(&[("prod", "boss", ""), ("playground", "boss-playground", "")]),
        "boss",
        "",
        &[],
    );
    assert_eq!(r.rc, 0, "{}\n{}", r.out, r.err);
    for field in ["site", "sites", "site_line"] {
        assert!(
            r.recorded.get(field).is_none(),
            "nothing is claimed about a site nobody declared: {}",
            r.recorded
        );
    }
    assert!(!r.calls.contains("curl "), "{}", r.calls);
    assert!(
        r.out.contains("no instance declares a site"),
        "the journal says why the packet is silent: {}",
        r.out
    );
}

#[test]
fn several_declared_sites_ride_sites_by_host_and_site_is_the_sources() {
    let r = observe(
        "several",
        &rows(&[
            ("stage", "boss-stage", "stage.example"),
            ("prod", "boss", SITE),
        ]),
        "boss",
        "",
        &[],
    );
    assert_eq!(r.rc, 0, "{}\n{}", r.out, r.err);
    assert_eq!(
        r.recorded["site"]["host"], SITE,
        "`site` is the source instance's whatever the row order: {}",
        r.recorded
    );
    assert_eq!(r.recorded["site"]["namespace"], "boss", "{}", r.recorded);
    let sites = r.recorded["sites"]
        .as_object()
        .expect("`sites` keyed by host");
    assert_eq!(sites.len(), 2, "{}", r.recorded);
    assert_eq!(
        sites["stage.example"]["namespace"], "boss-stage",
        "{}",
        r.recorded
    );
    assert_eq!(sites[SITE]["http_status"], 200, "{}", r.recorded);
    let line = r.recorded["site_line"].as_str().unwrap();
    assert!(
        line.starts_with("stage.example → boss-stage: HTTP 200")
            && line.contains(&format!("; {SITE} → boss: HTTP 200")),
        "one line per instance, in row order: {line}"
    );
    assert!(
        r.calls.contains("get svc boss-gateway -n boss-stage")
            && r.calls.contains("get svc boss-gateway -n boss"),
        "{}",
        r.calls
    );
}

#[test]
fn both_ticks_observe_the_site_through_one_definition() {
    let lib = std::fs::read_to_string(repo_root().join(LIB)).unwrap();
    let f = lib
        .find("observe_sites() {")
        .expect("the lib defines observe_sites");
    let body = &lib[f..f + lib[f..].find("\n}\n").expect("a body")];
    for field in ["site", "site_line"] {
        assert!(
            body.contains(&format!("run_summary_json {field} "))
                || body.contains(&format!("run_summary_field {field} ")),
            "observe_sites records `{field}`: {body}"
        );
    }
    let runner = std::fs::read_to_string(repo_root().join(RUNNER)).unwrap();
    let unchanged_start = runner
        .find("if [ \"$HEAD\" = \"$LAST\" ]; then")
        .expect("the unchanged block");
    let unchanged_end = unchanged_start
        + runner[unchanged_start..]
            .find("_stage_started=$(date +%s)")
            .unwrap();
    let unchanged = &runner[unchanged_start..unchanged_end];
    assert!(
        unchanged.contains("observe_deployed_surfaces")
            && lib.contains("observe_sites \"$K\" \"$instances\""),
        "the unchanged tick observes the site after the connector: {unchanged}"
    );
    let stamped = runner
        .find("OUTCOME=\"converged=$HEAD\"")
        .expect("the runner marks the roll real");
    let deploying = &runner[stamped..];
    let connector = deploying
        .find("observe_connector \"$K\"")
        .expect("the deploying tick observes the connector");
    assert!(
        deploying[connector..].contains("observe_sites \"$K\""),
        "the deploying tick observes the site after the connector, its own stage: {}",
        &deploying[connector..connector + 400]
    );
    assert_eq!(
        runner.matches("observe_sites \"$K\"").count(),
        1,
        "the deployment calls directly; unchanged and data ticks share the read-only stage"
    );
}

#[test]
fn every_staged_html_page_is_read_and_recorded_by_path_with_its_hash() {
    let r = observe(
        "pages",
        &rows(&[("prod", "boss", SITE)]),
        "boss",
        "",
        &[(
            "STUB_PAGES",
            "blog-hello.html engineering-evidence.json index.html style.css",
        )],
    );
    assert_eq!(r.rc, 0, "{}\n{}", r.out, r.err);
    let site = &r.recorded["site"];
    assert_eq!(
        site["hash"],
        body_sha256(),
        "`site.hash` stays the hash of `/`, so publish-the-landing-page is unchanged: {}",
        r.recorded
    );
    let pages = site["pages"]
        .as_object()
        .unwrap_or_else(|| panic!("`site.pages` is an object keyed by path: {}", r.recorded));
    let mut keys: Vec<&str> = pages.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["/blog-hello.html", "/index.html"],
        "every staged .html page and nothing else: {}",
        r.recorded
    );
    for path in ["/blog-hello.html", "/index.html"] {
        assert_eq!(
            pages[path],
            sha256_of(&format!("page {path}\n")),
            "the sha256 of the body served for {path}, as received: {}",
            r.recorded
        );
    }
    assert!(
        r.calls.contains("kubectl get configmap boss-site -n boss"),
        "the staged pages are READ off the delivered ConfigMap: {}",
        r.calls
    );
    let blog = r
        .calls
        .lines()
        .find(|l| l.starts_with("curl ") && l.contains("http://10.0.0.1:80/blog-hello.html"))
        .unwrap_or_else(|| panic!("the post was requested at its own path: {}", r.calls));
    for want in ["-H Host: www.algedonic.dev", "--max-time 10", "-sS", "-o "] {
        assert!(
            blog.contains(want),
            "the page request carries `{want}`: {blog}"
        );
    }
    assert!(
        !r.calls.contains("/style.css") && !r.calls.contains("/engineering-evidence.json"),
        "only pages are read: {}",
        r.calls
    );
}

#[test]
fn a_page_that_cannot_be_read_records_why_and_never_a_hash() {
    let r = observe(
        "page-fails",
        &rows(&[("prod", "boss", SITE)]),
        "boss",
        "",
        &[
            ("STUB_PAGES", "blog-gone.html blog-hello.html index.html"),
            ("STUB_PAGE_FAIL", "/blog-hello.html"),
            ("STUB_PAGE_404", "/blog-gone.html"),
        ],
    );
    assert_eq!(
        r.rc, 0,
        "a page read never fails the tick: {}\n{}",
        r.out, r.err
    );
    let pages = &r.recorded["site"]["pages"];
    assert_eq!(
        pages["/blog-hello.html"],
        serde_json::json!({"unreachable": "curl: (7) Failed to connect to 10.0.0.1 port 80 after 3 ms: Connection refused"}),
        "curl's own words in place of a hash: {}",
        r.recorded
    );
    assert_eq!(
        pages["/blog-gone.html"],
        serde_json::json!({"http_status": 404}),
        "a miss is not a page: its status, never the hash of the miss: {}",
        r.recorded
    );
    assert_eq!(
        pages["/index.html"],
        sha256_of("page /index.html\n"),
        "one failed page does not stop the others: {}",
        r.recorded
    );

    // The ConfigMap itself unreadable: the pages are not knowable, and
    // the record says why; `/` is still observed.
    let r = observe(
        "cm-unread",
        &rows(&[("prod", "boss", SITE)]),
        "boss",
        "",
        &[("STUB_PAGES", "index.html"), ("STUB_CM_FAIL", "1")],
    );
    assert_eq!(r.rc, 0, "{}\n{}", r.out, r.err);
    let site = &r.recorded["site"];
    assert!(site.get("pages").is_none(), "{}", r.recorded);
    assert!(
        site["pages_unread"]
            .as_str()
            .is_some_and(|w| w.contains("boss-site") && w.contains("Forbidden")),
        "kubectl's words, naming the ConfigMap: {}",
        r.recorded
    );
    assert_eq!(site["hash"], body_sha256(), "{}", r.recorded);

    // `/` unreachable: the gateway did not answer, so no page is asked
    // and the tick spends no ten-second bound per page on it.
    let r = observe(
        "root-down",
        &rows(&[("prod", "boss", SITE)]),
        "boss",
        "",
        &[
            ("STUB_PAGES", "blog-hello.html index.html"),
            ("STUB_CURL_FAIL", "1"),
        ],
    );
    assert_eq!(r.rc, 0, "{}\n{}", r.out, r.err);
    assert_eq!(
        r.calls.lines().filter(|l| l.starts_with("curl ")).count(),
        1,
        "one request, for `/`: {}",
        r.calls
    );
    assert!(r.recorded["site"].get("pages").is_none(), "{}", r.recorded);
}

/// The calls a hung read leaves in the log: the stub `timeout` was asked
/// to bound the read by `bound` seconds, and the stub kubectl hung inside
/// that bound — so the lib's own bound, not the stub, ended it.
fn assert_the_bound_ended_the_hang(r: &Run, read: &str, bound: &str) {
    assert!(
        r.calls
            .lines()
            .any(|l| l.starts_with(&format!("timeout -k 5 {bound} ")) && l.contains(read)),
        "the lib bounds `{read}` by {bound} s: {}",
        r.calls
    );
    assert!(
        r.calls.contains("kubectl hangs; the bound ends it"),
        "the read hung inside that bound, and the bound ended it: {}",
        r.calls
    );
}

#[test]
fn a_stalled_configmap_read_is_bounded_and_the_observation_goes_on() {
    let r = observe(
        "cm-stalls",
        &rows(&[("prod", "boss", SITE)]),
        "boss",
        "",
        &[
            ("STUB_PAGES", "index.html"),
            ("STUB_HANG", "get configmap boss-site"),
            ("BOSS_SITE_READ_TIMEOUT", "1"),
            ("STUB_STALL", ANSWER_STALL),
        ],
    );
    assert_eq!(r.rc, 0, "{}\n{}", r.out, r.err);
    assert_the_bound_ended_the_hang(&r, "get configmap boss-site", "1");
    let site = &r.recorded["site"];
    assert_eq!(
        site["hash"],
        body_sha256(),
        "`/` is still recorded, answered later than the bound: {}",
        r.recorded
    );
    assert!(site.get("pages").is_none(), "{}", r.recorded);
    assert!(
        site["pages_unread"]
            .as_str()
            .is_some_and(|w| w.contains("boss-site") && w.contains("did not answer within 1 s")),
        "the record names the stall and its bound: {}",
        r.recorded
    );
    assert!(
        r.recorded["site_line"].is_string(),
        "the summary is written after the stall: {}",
        r.recorded
    );
    assert!(
        r.calls.contains("--request-timeout=1s"),
        "kubectl is asked to bound its own request too: {}",
        r.calls
    );
}

#[test]
fn a_stalled_service_read_is_bounded_and_named() {
    let r = observe(
        "svc-stalls",
        &rows(&[("prod", "boss", SITE)]),
        "boss",
        "",
        &[
            ("STUB_HANG", "get svc boss-gateway"),
            ("BOSS_SITE_READ_TIMEOUT", "1"),
            ("STUB_STALL", ANSWER_STALL),
        ],
    );
    assert_eq!(r.rc, 0, "{}\n{}", r.out, r.err);
    assert_the_bound_ended_the_hang(&r, "get svc boss-gateway", "1");
    assert_eq!(
        r.recorded["site"]["unreachable"],
        "the read of Service boss-gateway in boss did not answer within 1 s",
        "a stall is not reported as a missing address: {}",
        r.recorded
    );
    assert!(!r.calls.contains("curl "), "{}", r.calls);
}

/// The deadline is judged on the fixture's clock: it reads 1000 when the
/// observation starts (so the deadline is 1001) and each page read moves
/// it one second, so exactly one page is read before the deadline is
/// reached — however long each answered call took on the wall.
#[test]
fn the_page_phase_stops_at_the_observation_deadline_and_says_so() {
    let r = observe(
        "deadline",
        &rows(&[("prod", "boss", SITE)]),
        "boss",
        "",
        &[
            ("STUB_PAGES", "a.html b.html c.html d.html"),
            ("STUB_PAGE_TICK", "1"),
            ("BOSS_SITE_OBSERVE_SECONDS", "1"),
            ("STUB_STALL", ANSWER_STALL),
        ],
    );
    assert_eq!(r.rc, 0, "{}\n{}", r.out, r.err);
    let site = &r.recorded["site"];
    assert_eq!(
        site["pages"],
        serde_json::json!({"/a.html": sha256_of("page /a.html\n")}),
        "the deadline stopped the page reads after the one read before it: {}",
        r.recorded
    );
    assert_eq!(
        site["pages_unread"], "the site observation's 1 s deadline passed with 1 of 4 pages read",
        "the record names the deadline and how far it got: {}",
        r.recorded
    );
    assert_eq!(
        r.calls.lines().filter(|l| l.starts_with("curl ")).count(),
        2,
        "`/` and the one page, and nothing past the deadline: {}",
        r.calls
    );
    assert!(r.recorded["site_line"].is_string(), "{}", r.recorded);
}
