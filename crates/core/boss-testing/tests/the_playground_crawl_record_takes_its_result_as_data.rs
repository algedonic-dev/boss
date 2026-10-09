//! The playground crawl's `record` container takes what the browser's
//! container left as DATA: it records a bounded, cleaned string and an
//! exit status, and nothing the crawl writes can make it do anything
//! else (backlog 37742794; David's answer to design-doc c8502e17,
//! `crawl-and-sheet`, 2026-10-07).
//!
//! WHY THIS IS A TRUST BOUNDARY. The crawl container runs `bun install`
//! from npm and a headless Chromium — third-party code — and decision
//! c395e62c keeps the estate machine token away from such a container.
//! So the packet writes moved into a second container of the same pod
//! that DOES hold the token, and the two share one directory. Whatever
//! the first container can write there is input to a process that can
//! assert any identity to every service port. The obvious attacks are
//! the four the dispatch named: a result too large to carry, bytes that
//! are not text, shell metacharacters, and a path — a symbolic link to
//! the record container's own token mount, which a naive `cat` would
//! follow and post onto a packet anyone with board access can read.
//!
//! WHAT RUNS. The manifest's own text: the `record` container's `args:`
//! block is extracted from infra/cluster/manifests/boss-playground-crawl.yaml
//! and executed (inline shell nothing executes is untested shell), with
//! the tree's real boss-chore.sh between two stubs that record what the
//! wrap and the step were handed. Every path the script reads is one of
//! its own parameters, set here; nothing of the host's is read.
//!
//! tree-wide pin — it runs a script out of a manifest under
//! infra/cluster/manifests and reads the image's Dockerfile, which no
//! changed-file map attributes to this crate, so every scoped gate runs
//! it (`tree_wide_pins` in infra/gate.sh).

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;

const MANIFEST: &str = "infra/cluster/manifests/boss-playground-crawl.yaml";
const DOCKERFILE: &str = "infra/oss-quickstart/Dockerfile";
const GUARD_IN_IMAGE: &str = "/usr/local/bin/wait-out-a-converge.sh";
const CHORE_IN_IMAGE: &str = "/usr/local/bin/boss-chore.sh";
/// Stands in for the machine token the record container holds.
const MARKER: &str = "MARKER-the-record-containers-own-secret";

fn read(rel: &str) -> String {
    std::fs::read_to_string(repo_root().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// The `args:` block scalar of the named container, as the shell the
/// container runs.
fn container_script(yaml: &str, container: &str) -> String {
    let lines: Vec<&str> = yaml.lines().collect();
    let at = lines
        .iter()
        .position(|l| l.trim() == format!("- name: {container}"))
        .unwrap_or_else(|| panic!("{MANIFEST} has no `{container}` container"));
    let open = (at..lines.len())
        .find(|&i| lines[i].trim() == "- |")
        .unwrap_or_else(|| panic!("{MANIFEST}: `{container}` has no block-scalar args"));
    let indent = lines[open].len() - lines[open].trim_start().len();
    let mut out = String::new();
    for line in &lines[open + 1..] {
        if line.trim().is_empty() {
            out.push('\n');
            continue;
        }
        let this = line.len() - line.trim_start().len();
        if this <= indent {
            break;
        }
        out.push_str(&line[indent + 2..]);
        out.push('\n');
    }
    assert!(
        out.contains("maintenance-playground-crawl"),
        "the extracted block is not the record script — the scraper found the wrong block:\n{out}"
    );
    out
}

struct Pod {
    root: PathBuf,
    result: PathBuf,
    go: PathBuf,
    cwd: PathBuf,
    script: PathBuf,
}

/// A scratch pod: the result and go directories, a bin holding the real
/// chore wrapper between a stub wrap and a stub step, a stub guard, and
/// the record script as the manifest has it (or `script`, for a mutant).
fn pod(name: &str, script: Option<&str>) -> Pod {
    let root = scratch_dir(name);
    let mk = |d: &str| {
        let p = root.join(d);
        boss_testing::create_dir(&p);
        p
    };
    let (result, go, cwd, bin) = (mk("result"), mk("go"), mk("cwd"), mk("bin"));
    write_exec(&bin.join("boss-chore.sh"), &read("infra/boss-chore.sh"));
    write_exec(
        &bin.join("boss-maintenance-wrap.sh"),
        "#!/usr/bin/env bash\nprintf 'open:%s\\n' \"$*\" >> \"$STUB_LOG\"\n",
    );
    // One line per call: each argument with its newlines as 0x1e, the
    // arguments joined by 0x1f.
    write_exec(
        &bin.join("boss-step.sh"),
        "#!/usr/bin/env bash\n\
         { printf 'close:'; for a in \"$@\"; do printf '%s' \"$a\" | tr '\\n' '\\036'; printf '\\037'; done; echo; } >> \"$STUB_LOG\"\n",
    );
    // The guard: says what it saw, and whether the crawl had already
    // been told to go when it ran.
    write_exec(
        &bin.join("guard.sh"),
        "#!/usr/bin/env bash\n\
         if [ -e \"$GO_DIR/go\" ]; then echo 'guard: RAN AFTER GO'; else echo 'guard: clear'; fi\n",
    );
    let script_path = root.join("record.sh");
    let text = match script {
        Some(s) => s.to_string(),
        None => container_script(&read(MANIFEST), "record"),
    };
    // THE ONE EDIT made to the manifest's text: the wrapper's path. It
    // is spelled out in the manifest (timers-leave-a-packet.sh reads
    // that line), so here it is pointed at the planted copy of the same
    // file; every other path is a parameter the script itself reads.
    assert_eq!(
        text.matches(CHORE_IN_IMAGE).count(),
        1,
        "the record script calls {CHORE_IN_IMAGE} exactly once"
    );
    let text = text.replace(
        CHORE_IN_IMAGE,
        bin.join("boss-chore.sh").to_str().expect("utf8"),
    );
    write_file(&script_path, &text);
    Pod {
        root,
        result,
        go,
        cwd,
        script: script_path,
    }
}

struct Recorded {
    rc: i32,
    stdout: Vec<u8>,
    stderr: String,
    opens: usize,
    /// The step call's arguments, raw bytes each.
    close: Vec<Vec<u8>>,
    closes: usize,
}

impl Recorded {
    fn pair(&self, key: &str) -> Option<&[u8]> {
        let prefix = format!("{key}=");
        self.close
            .iter()
            .find(|a| a.starts_with(prefix.as_bytes()))
            .map(|a| &a[prefix.len()..])
    }
    fn text(&self, key: &str) -> String {
        String::from_utf8_lossy(self.pair(key).unwrap_or_default()).into_owned()
    }
    fn everything(&self) -> Vec<u8> {
        let mut all = self.stdout.clone();
        all.extend_from_slice(self.stderr.as_bytes());
        for a in &self.close {
            all.extend_from_slice(a);
        }
        all
    }
    fn say(&self) -> String {
        format!(
            "exit {}\nstdout: {}\nstderr: {}",
            self.rc,
            String::from_utf8_lossy(&self.stdout),
            self.stderr
        )
    }
}

fn holds(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|w| w == needle.as_bytes())
}

impl Pod {
    fn record(&self, deadline_s: &str) -> Recorded {
        let log = self.root.join("stub.log");
        let _ = std::fs::remove_file(&log);
        let bin = self.root.join("bin");
        let out = Command::new("bash")
            .arg(&self.script)
            .current_dir(&self.cwd)
            // Its own layout, whole: a gate pod's or a host's
            // environment is handed to every process in it.
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", &self.cwd)
            .env("TMPDIR", self.root.join("bin"))
            .env("STUB_LOG", &log)
            .env("BOSS_JOBS_URL", "http://boss-jobs-internal.test:7900")
            .env("BOSS_CRAWL_RESULT_DIR", &self.result)
            .env("BOSS_CRAWL_GO_DIR", &self.go)
            .env("BOSS_CRAWL_GUARD", bin.join("guard.sh"))
            .env("BOSS_CRAWL_RECORD_DEADLINE_S", deadline_s)
            .env("BOSS_CRAWL_RESULT_POLL_S", "1")
            .output()
            .expect("bash runs the record script");
        let raw = std::fs::read(&log).unwrap_or_default();
        let lines: Vec<&[u8]> = raw
            .split(|b| *b == b'\n')
            .filter(|l| !l.is_empty())
            .collect();
        let closes: Vec<&&[u8]> = lines.iter().filter(|l| l.starts_with(b"close:")).collect();
        let close = closes
            .first()
            .map(|l| {
                l[6..]
                    .split(|b| *b == 0x1f)
                    .filter(|a| !a.is_empty())
                    .map(|a| {
                        a.iter()
                            .map(|b| if *b == 0x1e { b'\n' } else { *b })
                            .collect()
                    })
                    .collect()
            })
            .unwrap_or_default();
        Recorded {
            rc: out.status.code().unwrap_or(-1),
            stdout: out.stdout,
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            opens: lines.iter().filter(|l| l.starts_with(b"open:")).count(),
            closes: closes.len(),
            close,
        }
    }

    /// Nothing anywhere under the pod is named PWNED: no text the crawl
    /// left was run.
    fn nothing_was_executed(&self, r: &Recorded) {
        fn walk(dir: &Path, found: &mut Vec<PathBuf>) {
            for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                let p = e.path();
                if p.file_name()
                    .is_some_and(|n| n.to_string_lossy().contains("PWNED"))
                {
                    found.push(p.clone());
                }
                if p.is_dir() && !p.is_symlink() {
                    walk(&p, found);
                }
            }
        }
        let mut found = Vec::new();
        walk(&self.root, &mut found);
        assert!(
            found.is_empty(),
            "text the crawl container left was EXECUTED by the container that holds the token: {found:?}\n{}",
            r.say()
        );
    }
}

/// What every run owes, whatever the crawl left: one packet opened, one
/// verdict recorded, a record small enough to ride one argv, and text.
fn recorded_once_bounded_and_clean(r: &Recorded) {
    assert_eq!(
        (r.opens, r.closes),
        (1, 1),
        "one open, one close\n{}",
        r.say()
    );
    let output = r.pair("output").unwrap_or_default();
    assert!(
        output.len() < 120 * 1024,
        "the recorded output is {} bytes — it rides one argv, which Linux caps at 128 KiB",
        output.len()
    );
    assert!(
        r.stdout.len() < 2 * 1024 * 1024,
        "the record container printed {} bytes of a result it reads at most 1 MiB of",
        r.stdout.len()
    );
    let text = std::str::from_utf8(output).unwrap_or_else(|e| {
        panic!("the recorded output is not UTF-8 ({e}) — bytes went to the packet as they came")
    });
    let control: Vec<char> = text
        .chars()
        .filter(|c| c.is_control() && !matches!(c, '\n' | '\t'))
        .collect();
    assert!(
        control.is_empty(),
        "control characters reached the packet: {control:?}"
    );
}

#[test]
fn an_honest_result_is_recorded_with_its_status_and_its_red_lines() {
    let p = pod("boss-crawl-record-honest", None);
    write_file(
        &p.result.join("output"),
        "wait: nothing here\ncrawled 3 routes as a guest: 1 red\nRED /it/registry no-shell: the shell never rendered — état\n",
    );
    write_file(&p.result.join("exit"), "1\n");
    let r = p.record("20");
    recorded_once_bounded_and_clean(&r);
    assert_eq!(
        r.rc,
        1,
        "the Job reports the crawl's own status\n{}",
        r.say()
    );
    assert_eq!(r.text("result"), "failed");
    assert_eq!(r.text("exit_status"), "1");
    let output = r.text("output");
    assert!(
        output.contains("RED /it/registry no-shell: the shell never rendered — état")
            && output.contains("crawled 3 routes as a guest: 1 red"),
        "the judge rule reads RED lines off this step, and valid UTF-8 is kept whole:\n{output}"
    );
    assert!(
        output.contains("guard: clear") && !output.contains("RAN AFTER GO"),
        "the converge guard runs BEFORE the crawl is told to go, and its words are on the packet:\n{output}"
    );
    assert!(p.go.join("go").exists(), "the crawl was told to go");

    let p = pod("boss-crawl-record-green", None);
    write_file(
        &p.result.join("output"),
        "crawled 3 routes as a guest: 0 red\n",
    );
    write_file(&p.result.join("exit"), "0\n");
    let r = p.record("20");
    recorded_once_bounded_and_clean(&r);
    assert_eq!((r.rc, r.text("result").as_str()), (0, "ok"), "{}", r.say());
}

#[test]
fn a_crawl_that_leaves_nothing_is_a_recorded_failure_not_a_silent_one() {
    let p = pod("boss-crawl-record-silent", None);
    let r = p.record("2");
    recorded_once_bounded_and_clean(&r);
    assert_eq!(r.rc, 1, "{}", r.say());
    assert_eq!(r.text("result"), "failed");
    assert!(
        r.text("output").contains("left no exit status"),
        "the packet says what happened: {}",
        r.text("output")
    );
    assert!(p.go.join("go").exists(), "it was told to go all the same");
}

#[test]
fn a_huge_result_is_recorded_bounded() {
    let p = pod("boss-crawl-record-huge", None);
    // 24 MiB: a line the wrapper would cut, then enough RED lines to
    // outgrow an argv many times over.
    let mut big = "A".repeat(4 * 1024 * 1024);
    big.push('\n');
    let red = "RED /route no-shell: ".to_string() + &"x".repeat(900) + "\n";
    while big.len() < 24 * 1024 * 1024 {
        big.push_str(&red);
    }
    write_file(&p.result.join("output"), &big);
    write_file(&p.result.join("exit"), &"7".repeat(4096));
    let r = p.record("20");
    recorded_once_bounded_and_clean(&r);
    assert_eq!(
        (r.rc, r.text("result").as_str()),
        (1, "failed"),
        "a status that is not one is a failure, never whatever its first digits say\n{}",
        r.say()
    );
    p.nothing_was_executed(&r);
}

#[test]
fn bytes_that_are_not_text_are_recorded_as_text() {
    let p = pod("boss-crawl-record-binary", None);
    let mut bytes: Vec<u8> = Vec::new();
    for i in 0..200_000u32 {
        // Every byte value, NULs and escapes and lone continuation
        // bytes among them, in an order no encoding would produce.
        bytes.push((i.wrapping_mul(2_654_435_761) >> 13) as u8);
    }
    bytes.extend_from_slice(
        b"\n\x1b]0;title\x07\x1b[2J\xff\xfe\xc3\x28 RED /x no-shell: after the noise\n",
    );
    std::fs::write(p.result.join("output"), &bytes).expect("write");
    write_file(&p.result.join("exit"), "1\n");
    let r = p.record("20");
    recorded_once_bounded_and_clean(&r);
    assert_eq!(r.rc, 1, "{}", r.say());
    assert!(
        std::str::from_utf8(&r.stdout).is_ok_and(|s| !s
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))),
        "what the container prints is what its log keeps: text"
    );
    p.nothing_was_executed(&r);
}

#[test]
fn shell_in_the_result_is_recorded_and_never_run() {
    let p = pod("boss-crawl-record-shell", None);
    let hostile = [
        "$(touch PWNED-1)",
        "`touch PWNED-2`",
        "; touch PWNED-3 ;",
        "' ; touch PWNED-4 ; echo '",
        "\" ; touch PWNED-5 ; echo \"",
        "$(touch \"$RESULT_DIR/../PWNED-6\")",
        "${IFS}touch${IFS}PWNED-7",
        "| touch PWNED-8",
        "&& touch PWNED-9",
        "\n'\nexit 0\ntouch PWNED-10\n",
        "RED $(touch PWNED-11) `touch PWNED-12`: a verdict line is data too",
        "result=ok",
        "exit_status=0",
        "--",
        "-rf /",
    ]
    .join("\n");
    write_file(&p.result.join("output"), &hostile);
    write_file(
        &p.result.join("exit"),
        "1; touch PWNED-13 $(touch PWNED-14)\n",
    );
    let r = p.record("20");
    recorded_once_bounded_and_clean(&r);
    p.nothing_was_executed(&r);
    assert_eq!(
        (r.rc, r.text("result"), r.text("exit_status")),
        (1, "failed".to_string(), "1".to_string()),
        "the status is the digits and nothing after them; a line reading `result=ok` in the \
         OUTPUT is not the verdict\n{}",
        r.say()
    );
    let output = r.text("output");
    for kept in [
        "$(touch PWNED-1)",
        "`touch PWNED-2`",
        "' ; touch PWNED-4 ; echo '",
    ] {
        assert!(
            output.contains(kept),
            "carried as written, not expanded: `{kept}`\n{output}"
        );
    }
    // The verdict pairs come from the wrapper, once each, before the
    // output: nothing the crawl wrote became an argument of its own.
    let keys: Vec<String> = r
        .close
        .iter()
        .map(|a| {
            String::from_utf8_lossy(a)
                .split('=')
                .next()
                .unwrap_or("")
                .to_string()
        })
        .collect();
    assert_eq!(
        &keys[2..],
        ["result", "exit_status", "output"],
        "the step was handed <kind> run result= exit_status= output= and nothing else: {keys:?}"
    );
}

/// The token stand-in, outside the result directory, where the record
/// container's own files are.
fn secret(p: &Pod) -> PathBuf {
    let path = p.root.join("machine-token-current");
    write_file(&path, &format!("{MARKER}\n"));
    path
}

#[test]
fn a_path_in_the_result_is_not_followed() {
    // `output` a link to the record container's own secret.
    let p = pod("boss-crawl-record-link", None);
    symlink(secret(&p), p.result.join("output")).expect("symlink");
    write_file(&p.result.join("exit"), "0\n");
    let r = p.record("20");
    recorded_once_bounded_and_clean(&r);
    assert!(
        !holds(&r.everything(), MARKER),
        "the record container followed a link the crawl planted and carried its own secret \
         onto the packet\n{}",
        r.say()
    );

    // `exit` a link too: a status read through a link is no status.
    let p = pod("boss-crawl-record-link-exit", None);
    write_file(&p.root.join("zero"), "0\n");
    write_file(
        &p.result.join("output"),
        "crawled 1 routes as a guest: 0 red\n",
    );
    symlink(p.root.join("zero"), p.result.join("exit")).expect("symlink");
    let r = p.record("20");
    recorded_once_bounded_and_clean(&r);
    assert_eq!(
        (r.rc, r.text("result").as_str()),
        (1, "failed"),
        "{}",
        r.say()
    );

    // A dangling link, a directory and a FIFO where the files should
    // be: each ends promptly, as a failure, with nothing followed.
    for (name, plant) in [("dangling", 0), ("directory", 1), ("fifo", 2)] {
        let p = pod(&format!("boss-crawl-record-{name}"), None);
        let _ = secret(&p);
        for file in ["output", "exit"] {
            let at = p.result.join(file);
            match plant {
                0 => symlink(p.root.join("nothing-here"), &at).expect("symlink"),
                1 => boss_testing::create_dir(&at),
                _ => {
                    let ok = Command::new("mkfifo").arg(&at).status().expect("mkfifo");
                    assert!(ok.success(), "mkfifo");
                }
            }
        }
        let started = std::time::Instant::now();
        let r = p.record("4");
        recorded_once_bounded_and_clean(&r);
        assert_eq!(
            (r.rc, r.text("result").as_str()),
            (1, "failed"),
            "{name}: {}",
            r.say()
        );
        assert!(
            started.elapsed().as_secs() < 25,
            "{name}: the record container waited {}s on something that is not a file",
            started.elapsed().as_secs()
        );
        assert!(!holds(&r.everything(), MARKER), "{name}");
    }
}

/// The link case has teeth: the same script without the no-follow open
/// DOES carry the secret out. A test that cannot fail proves nothing.
#[test]
fn without_the_no_follow_open_the_link_case_leaks() {
    let script = container_script(&read(MANIFEST), "record");
    assert!(
        script.contains("iflag=nofollow,nonblock"),
        "the record script no longer opens with nofollow — the mutant below would test nothing"
    );
    let mutant = script.replace("iflag=nofollow,nonblock", "iflag=nonblock");
    let p = pod("boss-crawl-record-mutant", Some(&mutant));
    symlink(secret(&p), p.result.join("output")).expect("symlink");
    write_file(&p.result.join("exit"), "0\n");
    let r = p.record("20");
    assert!(
        holds(&r.everything(), MARKER),
        "the mutant followed nothing — this fixture does not exercise the open at all\n{}",
        r.say()
    );
}

#[test]
fn the_image_carries_the_guard_where_the_record_container_runs_it() {
    let script = container_script(&read(MANIFEST), "record");
    assert!(
        script.contains(&format!("{GUARD_IN_IMAGE}}}\"")),
        "the record script's default guard is not {GUARD_IN_IMAGE}"
    );
    let copy = format!("COPY infra/wait-out-a-converge.sh {GUARD_IN_IMAGE}");
    assert!(
        read(DOCKERFILE).lines().any(|l| l.trim() == copy),
        "{DOCKERFILE} does not `{copy}` — the record container would find no guard and the \
         crawl would start inside a converge's roll (backlog c8c6b9a8)"
    );
    // The crawl container, for its part, runs no helper and names no door.
    let crawl = container_script_of_the_crawl();
    for word in [
        "boss-chore",
        "boss-step",
        "boss-maintenance-wrap",
        "boss-api-curl",
        "wait-out-a-converge",
        "curl ",
        "x-boss-user",
        "machine-token",
    ] {
        assert!(
            !crawl.contains(word),
            "the crawl container's script names `{word}` — it sends nothing to the record and holds nothing to send with"
        );
    }
    assert!(
        crawl.contains("bun run test:live") && crawl.contains("/result/exit"),
        "the crawl container runs the live suite and leaves its status"
    );
}

fn container_script_of_the_crawl() -> String {
    let yaml = read(MANIFEST);
    let lines: Vec<&str> = yaml.lines().collect();
    let at = lines
        .iter()
        .position(|l| l.trim() == "- name: crawl")
        .expect("the crawl container");
    let end = lines
        .iter()
        .position(|l| l.trim() == "- name: record")
        .expect("the record container");
    assert!(at < end, "crawl, then record");
    lines[at..end]
        .iter()
        .filter(|l| !l.trim_start().starts_with('#'))
        .copied()
        .collect::<Vec<_>>()
        .join("\n")
}
