//! EVERY CLUSTER CHORE AND ROOT-TREE UNIT THAT OPENS ITS PACKET THROUGH
//! boss-maintenance-wrap.sh LETS IT TRY MORE THAN ONCE (backlog a2ae9f39),
//! AND EVERY JOB THAT RUNS ONE GIVES THOSE TRIES ROOM BEFORE ITS ACT.
//!
//! Measured 2026-10-28: the nightly backup RAN on 10-26 and 10-27 (the
//! 10-27 off-site archive verified, 5.28 GB, gzip ok) and filed no
//! maintenance-backup packet on either night, so the cadence-silence
//! sweep raised it as silent for 2.6 days. Its CronJob opens the packet
//! through the wrap with no BOSS_WRAP_OPEN_RETRY, and the wrap reads an
//! unset bound as ONE attempt (`OPEN_ATTEMPTS`), so a single 502 from a
//! stalling jobs API (d9b39685) cost the night's record and the
//! filesystem sample that rides on it. Every boss-gcp root-tree unit
//! already set 3 (40b8dea1), pinned unit by unit in
//! gcp_root_adoption_sh.rs; the cluster CronJobs that open through
//! boss-chore.sh, which runs the wrap, set nothing either. Nothing
//! checked the cluster side, so the fix to one tree left the other at
//! one attempt. This is the check, over both trees, by caller.
//!
//! THE TRIES COME OUT OF THE ACT'S DEADLINE (review 0aced427). boss-chore
//! starts the act only after the wrap returns, so every second the open
//! spends is a second the act no longer has inside the Job's
//! activeDeadlineSeconds. At three attempts the longest open the wrap can
//! make is three list reads, three POSTs and a read-back after each POST,
//! every one at the wrap's --max-time, with the retry wait between tries:
//! the reviewer's stall server measured exactly that shape (9 requests,
//! 4 waits, about 278 s at production values). machine-token-push ran
//! under 240 s, so a stall burst ended the Job before the push began —
//! the visibility retries taking the act's deadline, which the wrap's
//! header forbids as THE EXECUTOR NEVER WAITS ON ITS VISIBILITY. So a
//! caller whose Job sets activeDeadlineSeconds DECLARES what it still
//! runs after the open, as BOSS_ACT_BOUND_S beside its other env, and
//! the deadline must be at least the worst-case open at that caller's
//! own attempt count plus that bound. The open's cost is READ from the
//! wrap — its --max-time default, its retry wait, and the requests and
//! waits in its read loop, its POST and its read-back loop — never typed
//! here; the act bound is read from the caller. A bound below a deadline
//! the caller's own script declares (`…_DEADLINE_S:-N`) is not a bound,
//! and is refused too: the playground crawl's record waits 3450 s, so a
//! bound under that is a wish (review 0aced427, N4).
//!
//! NOT COUNTED in the open, deliberately and visibly: the transport's
//! re-send of a REFUSED connect (boss-api-curl.sh, held to at most 30 s
//! per request by the wrap when it retries), which answers in
//! milliseconds while the API is down and only stacks with a stall on a
//! request that is first refused and then never answered; and the
//! settle-a-previous-finished-packet path, which runs only when the last
//! run's packet is completed but not yet closed and polls for its close.
//! Neither is the shape the reviewer measured or d9b39685 recorded.
//!
//! WHAT COUNTS AS A CALLER. In a manifest, a CONTAINER with a
//! non-comment line that runs boss-maintenance-wrap.sh, or
//! boss-chore.sh (which runs the wrap; this file reads that before it
//! trusts it): either with an argument after the script on the same line
//! (shell form), or as an exec-form argv element of its own — a flow
//! array item or a block list item (review 0aced427, N2). An exec item
//! is a mention when the container's command is `cp`: the recovery
//! sheet's `tools` init container lists the wrap in a `cp` and runs
//! nothing. In a unit template, an `Exec…=` line of the shell shape. The
//! bound a caller runs with is its own setting or, unset, the wrap's
//! default, both read from the files rather than retyped; it must be at
//! least two AND a value the wrap accepts, because the wrap refuses any
//! other bound and then opens nothing at all.
//!
//! NOT READ, deliberately: the systemd units elsewhere under infra/ (the
//! forge's converge, watchdog and deploy runner among them, whose
//! pre-start runs ahead of the act itself, so lengthening it is a choice
//! of its own and not this measured failure), the root-tree units'
//! start timeouts (c232c11c), and the dev pod's reclaim pass, which
//! calls the wrap through a variable and files again on its next hourly
//! pass.
//!
//! Hand-scanned rather than YAML-parsed, as
//! every_job_bounds_its_finished_history.rs is: the workspace carries no
//! YAML parser.
//!
//! tree-wide pin — it scans a tree no changed-file map can attribute
//! to this crate, so every scoped gate runs it whatever its scope
//! (`tree_wide_pins` in infra/gate.sh; backlog c87ad472).

use boss_testing::repo_root;
use regex::Regex;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

/// The wrap's own name for the bound.
const VAR: &str = "BOSS_WRAP_OPEN_RETRY";
/// The caller's declaration of what it still runs after the open.
const ACT: &str = "BOSS_ACT_BOUND_S";
/// One attempt is the failure this pin exists for.
const MIN_ATTEMPTS: u32 = 2;

/// A line that RUNS one of the two scripts: the name, then an argument.
static CALL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"boss-(maintenance-wrap|chore)\.sh[ \t]+\S").unwrap());
/// An exec-form argv element that IS one of the two scripts: a block
/// list item (`- /usr/local/bin/boss-chore.sh`) or a flow array item
/// (`["/usr/local/bin/boss-chore.sh", "kind", …]`), quoted or not.
static EXEC_ITEM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?:^\s*-\s+|[\[,]\s*)["']?[^\s"',\[\]]*boss-(maintenance-wrap|chore)\.sh["']?\s*(?:[,\]]|$)"#,
    )
    .unwrap()
});
/// The bound as a container sets it: a block or flow env entry, or a
/// shell assignment in its script.
static MANIFEST_BOUND: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"BOSS_WRAP_OPEN_RETRY(?:=|,?\s*value:\s*)["']?([0-9]+)"#).unwrap()
});
/// The act bound, in the same two env shapes.
static ACT_BOUND: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"BOSS_ACT_BOUND_S(?:=|,?\s*value:\s*)["']?([0-9]+)"#).unwrap());
/// A deadline the caller's own script declares, by its default.
static SCRIPT_DEADLINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"_DEADLINE_S:-([0-9]+)\}").unwrap());
/// The Job's deadline, on its own line.
static JOB_DEADLINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*activeDeadlineSeconds:\s*([0-9]+)\s*$").unwrap());
/// The bound as a unit sets it.
static UNIT_BOUND: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^Environment=.*?"?BOSS_WRAP_OPEN_RETRY=([0-9]+)"#).unwrap());

#[derive(Debug, PartialEq)]
struct Caller {
    /// Repo-relative file.
    file: String,
    /// The container's name, or the unit's file name.
    who: String,
    /// 1-based line of the call.
    line: usize,
    /// What the caller sets, if anything.
    bound: Option<u32>,
    /// The tightest activeDeadlineSeconds in the caller's YAML document.
    deadline: Option<u32>,
    /// The caller's BOSS_ACT_BOUND_S.
    act: Option<u32>,
    /// Every `…_DEADLINE_S:-N` default the caller's own lines declare.
    script_deadlines: Vec<u32>,
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

fn indent(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

fn is_comment(line: &str) -> bool {
    line.trim_start().starts_with('#')
}

fn skipped(line: &str) -> bool {
    line.trim().is_empty() || is_comment(line)
}

fn unquote(s: &str) -> &str {
    s.trim().trim_matches(|c| c == '"' || c == '\'')
}

/// A list item that starts a container (or any named item).
fn item_name(line: &str) -> Option<String> {
    let t = line.trim_start();
    let rest = t
        .strip_prefix("- name:")
        .or_else(|| t.strip_prefix("- {name:"))?;
    Some(
        rest.split(',')
            .next()
            .unwrap_or("")
            .trim()
            .trim_end_matches('}')
            .trim()
            .to_string(),
    )
}

/// Whether a container's `command` is `cp`, in flow or block form: then
/// an exec item naming the wrap is a file being copied, not run.
fn runs_cp(body: &[&str]) -> bool {
    let Some(at) = body
        .iter()
        .position(|l| l.trim_start().starts_with("command:"))
    else {
        return false;
    };
    let rest = body[at].trim_start()["command:".len()..].trim();
    let first = match rest.strip_prefix('[') {
        Some(flow) => flow.split([',', ']']).next().unwrap_or(""),
        None if rest.is_empty() => body[at + 1..]
            .iter()
            .find(|l| !skipped(l))
            .and_then(|l| l.trim_start().strip_prefix('-'))
            .unwrap_or(""),
        None => rest,
    };
    unquote(first) == "cp"
}

/// The wrap's default bound and the bounds it accepts, from its own lines.
fn wrap_bounds() -> (u32, Vec<u32>) {
    let wrap = read(&repo_root().join("infra/boss-maintenance-wrap.sh"));
    let default = Regex::new(r#"OPEN_ATTEMPTS="\$\{BOSS_WRAP_OPEN_RETRY:-([0-9]+)\}""#)
        .unwrap()
        .captures(&wrap)
        .unwrap_or_else(|| panic!("the wrap no longer reads its default bound from {VAR}"))[1]
        .parse()
        .unwrap();
    let accepted = Regex::new(r#"case "\$OPEN_ATTEMPTS" in ([0-9|]+)\)"#)
        .unwrap()
        .captures(&wrap)
        .unwrap_or_else(|| panic!("the wrap no longer states which bounds it accepts"))[1]
        .split('|')
        .map(|n| n.parse().unwrap())
        .collect();
    (default, accepted)
}

/// What the wrap's open can cost, from its own lines.
#[derive(Debug)]
struct OpenCost {
    /// `--max-time` on every request of the open, its default.
    max_time: u32,
    /// The wait between tries, its default.
    wait: u32,
    /// Requests and waits per try of the open-packet read.
    list_requests: u32,
    list_waits: u32,
    /// Requests per POST of the packet.
    post_requests: u32,
    /// Per failed POST: read-back requests, re-POSTs and waits.
    readback_requests: u32,
    reposts: u32,
    readback_waits: u32,
}

impl OpenCost {
    /// The longest open at `attempts`: (requests, waits, seconds).
    fn worst(&self, attempts: u32) -> (u32, u32, u32) {
        let retries = attempts.saturating_sub(1);
        let posts = 1 + retries * self.reposts;
        let requests = attempts * self.list_requests
            + posts * self.post_requests
            + posts * self.readback_requests;
        let waits = retries * (self.list_waits + self.readback_waits);
        (
            requests,
            waits,
            requests * self.max_time + waits * self.wait,
        )
    }
}

/// The lines of the shell block whose first line starts with `opener` (a
/// `while …; do` or a `name() {`), to the first `done` or `}` at its
/// indent.
fn block<'a>(lines: &[&'a str], opener: &str, closer: &str) -> Vec<&'a str> {
    let at = lines
        .iter()
        .position(|l| !is_comment(l) && l.trim().starts_with(opener))
        .unwrap_or_else(|| panic!("the wrap no longer opens `{opener}`, so its cost is unread"));
    let depth = indent(lines[at]);
    let end = (at + 1..lines.len())
        .find(|&k| indent(lines[k]) == depth && lines[k].trim() == closer)
        .unwrap_or_else(|| panic!("the wrap's `{opener}` has no `{closer}`"));
    lines[at..=end].to_vec()
}

/// Requests in a block, each of which must carry the wrap's deadline:
/// a request without it is one this count cannot bound.
fn requests(block: &[&str], what: &str) -> u32 {
    let calls: Vec<&&str> = block
        .iter()
        .filter(|l| !is_comment(l) && l.contains(r#""$API_CURL""#))
        .collect();
    for call in &calls {
        assert!(
            call.contains(r#"--max-time "$MAX_TIME""#),
            "a request in the wrap's {what} has no --max-time \"$MAX_TIME\", so this pin cannot bound it: {call}"
        );
    }
    calls.len() as u32
}

fn count(block: &[&str], needle: &str) -> u32 {
    block
        .iter()
        .filter(|l| !is_comment(l) && l.contains(needle))
        .count() as u32
}

fn open_cost() -> OpenCost {
    let wrap = read(&repo_root().join("infra/boss-maintenance-wrap.sh"));
    let default = |var: &str, name: &str| -> u32 {
        Regex::new(&format!(r#"{name}="\$\{{{var}:-([0-9]+)\}}""#))
            .unwrap()
            .captures(&wrap)
            .unwrap_or_else(|| panic!("the wrap no longer reads {name}'s default from {var}"))[1]
            .parse()
            .unwrap()
    };
    let lines: Vec<&str> = wrap.lines().collect();
    let list = block(
        &lines,
        r#"while [ "$attempt" -lt "$OPEN_ATTEMPTS" ]; do"#,
        "done",
    );
    let post = block(&lines, "post_packet() {", "}");
    let readback = block(&lines, r#"while [ "$rc" -ne 0 ]; do"#, "done");
    assert!(
        readback
            .iter()
            .any(|l| l.trim() == r#"[ "$post_attempt" -lt "$OPEN_ATTEMPTS" ] || break"#),
        "the wrap's read-back loop no longer stops re-POSTing at the attempt bound, so the open's worst case is not the one counted here"
    );
    OpenCost {
        max_time: default("BOSS_WRAP_MAX_TIME", "MAX_TIME"),
        wait: default("BOSS_WRAP_OPEN_RETRY_WAIT", "OPEN_WAIT"),
        list_requests: requests(&list, "open-packet read"),
        list_waits: count(&list, r#"sleep "$OPEN_WAIT""#),
        post_requests: requests(&post, "POST"),
        readback_requests: requests(&readback, "read-back"),
        reposts: count(&readback, "post_packet "),
        readback_waits: count(&readback, r#"sleep "$OPEN_WAIT""#),
    }
}

/// Every caller in one manifest's text.
fn manifest_callers(file: &str, text: &str) -> Vec<Caller> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    // One container may call twice; it is one caller with one bound.
    let mut containers = std::collections::BTreeSet::new();
    for (i, line) in lines.iter().enumerate() {
        let shell = CALL.is_match(line);
        if is_comment(line) || !(shell || EXEC_ITEM.is_match(line)) {
            continue;
        }
        // The container: the nearest named item above whose every line
        // between is deeper than it (an env item above `args:` is not).
        let mut floor = indent(line);
        let mut start = None;
        for j in (0..i).rev() {
            let above = lines[j];
            if skipped(above) || indent(above) >= floor {
                continue;
            }
            if item_name(above).is_some() {
                start = Some(j);
                break;
            }
            floor = indent(above);
        }
        let start =
            start.unwrap_or_else(|| panic!("{file}:{}: a call outside any named container", i + 1));
        if containers.contains(&start) {
            continue;
        }
        let depth = indent(lines[start]);
        let end = (start + 1..lines.len())
            .find(|&k| !skipped(lines[k]) && indent(lines[k]) <= depth)
            .unwrap_or(lines.len());
        let body: Vec<&str> = lines[start..end]
            .iter()
            .copied()
            .filter(|l| !is_comment(l))
            .collect();
        if !shell && runs_cp(&body) {
            continue;
        }
        containers.insert(start);
        let joined = body.join("\n");
        let last = |re: &Regex| {
            re.captures_iter(&joined)
                .last()
                .map(|c| c[1].parse().unwrap())
        };
        // The Job: the YAML document the call sits in.
        let doc_start = (0..i)
            .rev()
            .find(|&k| lines[k].trim_end() == "---")
            .unwrap_or(0);
        let doc_end = (i..lines.len())
            .find(|&k| lines[k].trim_end() == "---")
            .unwrap_or(lines.len());
        let deadline = lines[doc_start..doc_end]
            .iter()
            .filter(|l| !is_comment(l))
            .filter_map(|l| JOB_DEADLINE.captures(l).map(|c| c[1].parse().unwrap()))
            .min();
        out.push(Caller {
            file: file.to_string(),
            who: item_name(lines[start]).unwrap_or_default(),
            line: i + 1,
            bound: last(&MANIFEST_BOUND),
            deadline,
            act: last(&ACT_BOUND),
            script_deadlines: SCRIPT_DEADLINE
                .captures_iter(&joined)
                .map(|c| c[1].parse().unwrap())
                .collect(),
        });
    }
    out
}

/// The caller in one unit template, if it is one.
fn unit_caller(file: &str, text: &str) -> Option<Caller> {
    let line = text
        .lines()
        .position(|l| l.starts_with("Exec") && CALL.is_match(l))?;
    let bound = text
        .lines()
        .filter_map(|l| UNIT_BOUND.captures(l))
        .next_back()
        .map(|c| c[1].parse().unwrap());
    Some(Caller {
        file: file.to_string(),
        who: file.rsplit('/').next().unwrap_or(file).to_string(),
        line: line + 1,
        bound,
        deadline: None,
        act: None,
        script_deadlines: Vec::new(),
    })
}

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in
            std::fs::read_dir(&d).unwrap_or_else(|e| panic!("reading {}: {e}", d.display()))
        {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

fn relative(path: &Path) -> String {
    path.strip_prefix(repo_root())
        .unwrap_or(path)
        .display()
        .to_string()
}

#[test]
fn the_scanner_reads_a_call_not_a_mention() {
    let text = "\
spec:
  initContainers:
    - name: tools
      command:
        - cp
        - /usr/local/bin/boss-maintenance-wrap.sh
    - name: flow-tools
      command: [\"cp\", \"/usr/local/bin/boss-chore.sh\", \"/tools/\"]
  containers:
    - name: env-first
      env:
        - name: BOSS_WRAP_OPEN_RETRY
          value: \"3\"
      args:
        - |
          # boss-chore.sh runs here, said in a comment
          /usr/local/bin/boss-chore.sh maintenance-x \"X\" -- true
    - name: unset
      args:
        - |
          /usr/local/bin/boss-maintenance-wrap.sh maintenance-y \"Y\"
      env:
        - {name: BOSS_JOBS_URL, value: http://x}
    - name: flow
      env:
        - {name: BOSS_WRAP_OPEN_RETRY, value: \"2\"}
      command: [\"/bin/sh\", \"-c\", \"boss-chore.sh maintenance-z Z -- true\"]
    - name: exec-flow
      command: [\"/usr/local/bin/boss-chore.sh\", \"maintenance-v\", \"V\", \"--\", \"/usr/local/bin/v\"]
    - name: exec-block
      command:
        - /bin/bash
      args:
        - \"/usr/local/bin/boss-maintenance-wrap.sh\"
        - maintenance-w
";
    let found: Vec<(String, Option<u32>)> = manifest_callers("t.yaml", text)
        .into_iter()
        .map(|c| (c.who, c.bound))
        .collect();
    assert_eq!(
        found,
        [
            ("env-first".to_string(), Some(3)),
            ("unset".to_string(), None),
            ("flow".to_string(), Some(2)),
            ("exec-flow".to_string(), None),
            ("exec-block".to_string(), None),
        ],
        "a cp list (block or flow) and a comment are mentions; an exec argv \
         element is a call; each call is bound by its own container"
    );
    let unit = "[Service]\nEnvironment=\"BOSS_WRAP_OPEN_RETRY=3\"\n\
ExecStartPre=-/t/root-tree.sh run infra/boss-maintenance-wrap.sh maintenance-u \"U\"\n";
    assert_eq!(unit_caller("u.service.in", unit).unwrap().bound, Some(3));
    assert_eq!(
        unit_caller("v.service.in", "ExecStart=/bin/true\n"),
        None,
        "a unit that never calls the wrap is not a caller"
    );
}

#[test]
fn the_scanner_reads_each_callers_job_deadline_and_act_bound() {
    let text = "\
kind: ConfigMap
data:
  activeDeadlineSeconds: 5
---
kind: CronJob
spec:
  jobTemplate:
    spec:
      activeDeadlineSeconds: 420
      template:
        spec:
          containers:
            - name: declared
              command: [\"/bin/bash\", \"-c\"]
              args:
                - |
                  export RECORD_DEADLINE_S=\"${BOSS_X_RECORD_DEADLINE_S:-90}\"
                  # BOSS_ACT_BOUND_S=1 in a comment is no declaration
                  /usr/local/bin/boss-chore.sh maintenance-a \"A\" -- true
              env:
                - name: BOSS_ACT_BOUND_S
                  value: \"95\"
---
kind: CronJob
spec:
  jobTemplate:
    spec:
      template:
        spec:
          containers:
            - name: no-deadline
              args: [\"/usr/local/bin/boss-chore.sh\", \"maintenance-c\", \"C\", \"--\", \"true\"]
";
    let found: Vec<(String, Option<u32>, Option<u32>, Vec<u32>)> = manifest_callers("t.yaml", text)
        .into_iter()
        .map(|c| (c.who, c.deadline, c.act, c.script_deadlines))
        .collect();
    assert_eq!(
        found,
        [
            ("declared".to_string(), Some(420), Some(95), vec![90]),
            ("no-deadline".to_string(), None, None, vec![]),
        ],
        "a deadline is the caller's own document's, never a neighbour's"
    );
}

#[test]
fn every_maintenance_wrap_caller_retries_its_open() {
    // boss-chore.sh counts as a caller only because it runs the wrap.
    let chore = read(&repo_root().join("infra/boss-chore.sh"));
    assert!(
        chore.contains(r#"WRAP="$here/boss-maintenance-wrap.sh""#)
            && chore.lines().any(|l| l.starts_with(r#""$WRAP" "$KIND""#)),
        "infra/boss-chore.sh no longer runs the wrap, so this pin's caller list is wrong"
    );
    let (default, accepted) = wrap_bounds();
    let cost = open_cost();
    // The model is the measured one: review 0aced427's stall server saw
    // the wrap make 9 requests and 4 waits at three attempts. A model
    // that reads fewer (a loop it no longer finds, a request it lost)
    // would let every deadline pass.
    let (reqs, waits, _) = cost.worst(3);
    assert_eq!(
        (reqs, waits),
        (9, 4),
        "the wrap's open at three attempts no longer counts the 9 requests and 4 waits review 0aced427 measured: {cost:?}"
    );

    let mut callers = Vec::new();
    for path in files_under(&repo_root().join("infra/cluster/manifests")) {
        if matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("yaml" | "yml")
        ) {
            callers.extend(manifest_callers(&relative(&path), &read(&path)));
        }
    }
    let manifests = callers.len();
    for path in files_under(&repo_root().join("infra/gcp/root-tree-units")) {
        callers.extend(unit_caller(&relative(&path), &read(&path)));
    }
    let units = callers.len() - manifests;

    // No callers found is not a pass: the scanner must see the two the
    // packet is about, and must not mistake the sheet's cp for a call;
    // and it must see the deadline of the Job review 0aced427 measured.
    let seen = |file: &str, who: &str| {
        callers
            .iter()
            .any(|c| c.file.ends_with(file) && c.who == who)
    };
    assert!(
        seen("manifests/boss-backup.yaml", "open-packet")
            && seen("manifests/boss-recovery-sheet.yaml", "check")
            && !seen("manifests/boss-recovery-sheet.yaml", "tools"),
        "the scanner lost the callers it exists for: {callers:#?}"
    );
    assert!(units > 0, "no root-tree unit calls the wrap: {callers:#?}");
    assert!(
        callers.iter().any(
            |c| c.file.ends_with("manifests/boss-machine-token-push.yaml") && c.deadline.is_some()
        ),
        "the scanner lost machine-token-push's activeDeadlineSeconds: {callers:#?}"
    );

    let offenders: Vec<String> = callers
        .iter()
        .filter_map(|c| {
            let runs = c.bound.unwrap_or(default);
            (runs < MIN_ATTEMPTS || !accepted.contains(&runs)).then(|| match c.bound {
                None => format!(
                    "{}:{} ({}) opens its packet through the wrap with {VAR} unset, so it runs the wrap's default of {default} attempt(s)",
                    c.file, c.line, c.who
                ),
                Some(n) => format!(
                    "{}:{} ({}) sets {VAR}={n}; it must be at least {MIN_ATTEMPTS} and one the wrap accepts ({accepted:?})",
                    c.file, c.line, c.who
                ),
            })
        })
        .collect();
    assert!(
        offenders.is_empty(),
        "{} of {} wrap callers ({manifests} containers, {units} units) can lose a night's packet to one failed read — set {VAR}=3 beside the caller, as the root-tree units do (backlog a2ae9f39):\n  {}",
        offenders.len(),
        callers.len(),
        offenders.join("\n  ")
    );

    let bounded: Vec<&Caller> = callers.iter().filter(|c| c.deadline.is_some()).collect();
    let late: Vec<String> = bounded
        .iter()
        .filter_map(|c| {
            let deadline = c.deadline?;
            let runs = c.bound.unwrap_or(default);
            let (reqs, waits, open) = cost.worst(runs);
            let Some(act) = c.act else {
                return Some(format!(
                    "{}:{} ({}) runs under activeDeadlineSeconds: {deadline} and declares no {ACT}, so nothing says its act fits after an open that can take {open} s",
                    c.file, c.line, c.who
                ));
            };
            if let Some(&own) = c.script_deadlines.iter().max().filter(|&&own| own > act) {
                return Some(format!(
                    "{}:{} ({}) declares {ACT}={act}, below the {own} s deadline its own lines declare",
                    c.file, c.line, c.who
                ));
            }
            (deadline < open + act).then(|| format!(
                "{}:{} ({}) runs under activeDeadlineSeconds: {deadline}, below the wrap's worst-case open at {runs} attempt(s) ({open} s: {reqs} requests at --max-time {} s and {waits} waits of {} s) plus its {ACT}={act}: {} s, so a stall burst ends the Job before its act does",
                c.file, c.line, c.who, cost.max_time, cost.wait, open + act
            ))
        })
        .collect();
    assert!(
        late.is_empty(),
        "{} of {} wrap callers under a Job deadline give the open's retries the act's time (review 0aced427) — raise activeDeadlineSeconds, or declare the act's true bound:\n  {}",
        late.len(),
        bounded.len(),
        late.join("\n  ")
    );
}
