//! A dev pod boot resumes the overnight drain when its switch says so
//! (backlog ea83db08, 2026-09-24).
//!
//! The boot started `claude --remote-control boss-dev` with no task, so
//! every roll ended the drain until a human typed: the pod rolled at
//! ~10:00Z, 18:30Z, 18:52Z and 01:27Z on 2026-09-23/24, and each roll
//! killed the session, its session cron and its builders. David,
//! 2026-09-24: "We still haven't quite stayed productive all night yet."
//!
//! The standing prompt is versioned at `infra/dev/drain.md`, and the
//! boot passes it as the session's first prompt ONLY when the switch
//! file `/work/home/.config/boss/drain-on-boot` exists — it is on the
//! PVC, so David turns the drain on or off without a deploy. Without
//! the switch the boot is exactly what it was.
//!
//! Pinned by RUNNING the script the manifest writes, not by grepping
//! it: the heredoc is lifted out of `boss-dev.yaml`, its `/work/` paths
//! are moved under a scratch root, and a stub `claude` records the argv
//! it was exec'd with. So the quoting is under test too — the prompt
//! carries backticks and quotes, and it must arrive as ONE argument,
//! byte for byte, which only a prompt read at run time delivers.
//!
//! FROM ORIGIN/MAIN, not from the checkout (backlog 7a581397,
//! 2026-09-24). The roll that SHIPPED this feature (train #606) booted
//! idle, silently: the switch was on, but the main checkout was still
//! at #605 — the reclaim sidecar fast-forwards hourly — and drain.md
//! was new in #606. So the boot fetches origin and reads
//! `origin/main:infra/dev/drain.md`, falls back to the checkout's copy,
//! and when the switch is on but NO prompt can be read it says so in
//! postStart.log rather than starting idle without a word. The origin
//! cases below run real git: a checkout cloned BEFORE the commit that
//! carries the prompt, exactly the shape measured at 04:02Z.

use boss_testing::{create_dir, repo_root, scratch_dir, write_exec, write_file};
use std::path::{Path, PathBuf};
use std::process::Command;

const SWITCH: &str = "/work/home/.config/boss/drain-on-boot";
const PROMPT: &str = "/work/boss/infra/dev/drain.md";
const BOOT_LOG: &str = "/work/ssh/postStart.log";
const MISSING_PROMPT_NOTICE: &str = "The drain did not resume: drain-on-boot is on, but neither origin/main:infra/dev/drain.md nor the checkout copy could be read as a non-empty prompt. Report this failure to David. Do not claim that backlog work resumed or infer new permission to repair or activate it.";

fn manifest() -> String {
    std::fs::read_to_string(repo_root().join("infra/cluster/manifests/boss-dev.yaml"))
        .expect("infra/cluster/manifests/boss-dev.yaml")
}

fn drain_prompt() -> String {
    std::fs::read_to_string(repo_root().join("infra/dev/drain.md")).expect("infra/dev/drain.md")
}

/// The body of the `cat > /work/dev-claude.sh <<'DC'` heredoc in the
/// postStart, with the YAML block's indentation taken off — what the
/// pod actually writes to /work/dev-claude.sh.
fn dev_claude_sh() -> String {
    let m = manifest();
    let lines: Vec<&str> = m.lines().collect();
    let opener = lines
        .iter()
        .position(|l| l.trim() == "cat > /work/dev-claude.sh <<'DC'")
        .expect("the postStart writes /work/dev-claude.sh through a quoted DC heredoc");
    let indent = lines[opener].len() - lines[opener].trim_start().len();
    let body: Vec<&str> = lines[opener + 1..]
        .iter()
        .take_while(|l| l.trim() != "DC")
        .map(|l| l.get(indent..).unwrap_or(""))
        .collect();
    assert!(
        lines[opener + 1 + body.len()..]
            .first()
            .is_some_and(|l| l.trim() == "DC"),
        "the dev-claude.sh heredoc has no DC terminator"
    );
    body.join("\n") + "\n"
}

/// What one boot did: the argv `claude` was exec'd with, and what the
/// boot appended to postStart.log.
struct Booted {
    argv: Vec<String>,
    log: String,
}

/// What the boot exec'd `claude` with, given whether the switch file
/// and the checkout's prompt file are present, with no git anywhere.
fn boot(switch_on: bool, prompt: Option<&str>) -> Vec<String> {
    boot_with(switch_on, prompt, None).argv
}

/// Run `git` hermetically: no system or user config, and no discovery
/// above the scratch root, so a scratch dir inside some other checkout
/// can never answer for the one under test.
fn git(root: &Path, dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=boot-test",
            "-c",
            "user.email=boot-test@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .current_dir(dir)
        .env("HOME", root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CEILING_DIRECTORIES", root)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} in {} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The boot, with the checkout's copy of the prompt as `checkout` and,
/// when `origin_ahead_with` is given, /work/boss a real clone whose
/// origin's `main` gained `infra/dev/drain.md` with that text AFTER the
/// clone — the checkout has not pulled it and its `origin/main` does
/// not know it yet, so only a fetch finds it.
fn boot_with(switch_on: bool, checkout: Option<&str>, origin_ahead_with: Option<&str>) -> Booted {
    let root = scratch_dir("drain-on-boot");
    let work = root.join("work");
    let rooted = |p: &str| -> PathBuf { root.join(p.trim_start_matches('/')) };
    let bin = work.join("home/.local/bin");
    create_dir(&bin);
    create_dir(&work.join("ssh"));
    write_exec(
        &bin.join("claude"),
        "#!/usr/bin/env bash\nprintf '%s\\0' \"$@\" > \"$HOME/argv\"\n",
    );
    if switch_on {
        let switch = rooted(SWITCH);
        create_dir(switch.parent().expect("switch has a parent"));
        // An EMPTY file: the switch is `touch`, never a value.
        write_file(&switch, "");
    }
    match origin_ahead_with {
        None => {
            create_dir(&work.join("boss/infra/dev"));
            if let Some(p) = checkout {
                write_file(&rooted(PROMPT), p);
            }
        }
        Some(ahead) => {
            let origin = root.join("origin");
            create_dir(&origin.join("infra/dev"));
            git(&root, &origin, &["init", "-q"]);
            write_file(&origin.join("README"), "base\n");
            if let Some(p) = checkout {
                write_file(&origin.join("infra/dev/drain.md"), p);
            }
            git(&root, &origin, &["add", "-A"]);
            git(&root, &origin, &["commit", "-q", "-m", "base"]);
            let to = work.join("boss");
            git(
                &root,
                &root,
                &[
                    "clone",
                    "-q",
                    &origin.display().to_string(),
                    &to.display().to_string(),
                ],
            );
            write_file(&origin.join("infra/dev/drain.md"), ahead);
            git(&root, &origin, &["add", "-A"]);
            git(
                &root,
                &origin,
                &["commit", "-q", "-m", "the train that ships the prompt"],
            );
        }
    }
    let script = root.join("dev-claude.sh");
    let body = dev_claude_sh().replace("/work/", &format!("{}/", work.display()));
    write_exec(&script, &body);
    let out = Command::new("bash")
        .arg(&script)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CEILING_DIRECTORIES", &root)
        .output()
        .unwrap_or_else(|e| panic!("run {}: {e}", script.display()));
    assert!(
        out.status.success(),
        "dev-claude.sh exited {:?}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    Booted {
        argv: read_argv(&work.join("home/argv")),
        log: std::fs::read_to_string(rooted(BOOT_LOG)).unwrap_or_default(),
    }
}

fn read_argv(path: &Path) -> Vec<String> {
    let raw = std::fs::read(path)
        .unwrap_or_else(|e| panic!("the stub claude never ran ({}): {e}", path.display()));
    raw.split(|b| *b == 0)
        .filter(|a| !a.is_empty())
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect()
}

/// The boot with the switch on, the drain prompt in the checkout, and
/// `/work/drain-seat` holding `drain_seat` (or absent): the decision
/// the postStart made once, before dev-claude.sh ran (review ce8970ce
/// N2).
fn boot_seated(drain_seat: Option<&str>) -> Booted {
    let root = scratch_dir("drain-on-boot-seated");
    let work = root.join("work");
    let rooted = |p: &str| -> PathBuf { root.join(p.trim_start_matches('/')) };
    let bin = work.join("home/.local/bin");
    create_dir(&bin);
    create_dir(&work.join("ssh"));
    write_exec(
        &bin.join("claude"),
        "#!/usr/bin/env bash\nprintf '%s\\0' \"$@\" > \"$HOME/argv\"\n",
    );
    let switch = rooted(SWITCH);
    create_dir(switch.parent().expect("switch has a parent"));
    write_file(&switch, "");
    create_dir(&work.join("boss/infra/dev"));
    write_file(&rooted(PROMPT), &drain_prompt());
    if let Some(seat) = drain_seat {
        write_file(&work.join("drain-seat"), &format!("{seat}\n"));
    }
    let script = root.join("dev-claude.sh");
    let body = dev_claude_sh().replace("/work/", &format!("{}/", work.display()));
    write_exec(&script, &body);
    let out = Command::new("bash")
        .arg(&script)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CEILING_DIRECTORIES", &root)
        .output()
        .unwrap_or_else(|e| panic!("run {}: {e}", script.display()));
    assert!(
        out.status.success(),
        "dev-claude.sh exited {:?}",
        out.status.code()
    );
    Booted {
        argv: read_argv(&work.join("home/argv")),
        log: std::fs::read_to_string(rooted(BOOT_LOG)).unwrap_or_default(),
    }
}

/// The drain moves to its supervised seat (backlog b513514c): when the
/// boot gave the drain to the conductor's seat, `dev-session.sh --boot`
/// runs it in tmux `conductor` under factory-loop.sh, so `dev` starts
/// as David's Remote Control door with no prompt — one drain, never two
/// on one agent — and the log says where the drain went. dev-claude.sh
/// reads the boot's ONE decision, /work/drain-seat, never the checkout
/// a fast-forward may have moved since (review ce8970ce N2).
#[test]
fn with_a_conductor_seat_the_drain_runs_in_its_seat_and_dev_starts_idle() {
    let b = boot_seated(Some("conductor"));
    assert_eq!(
        b.argv,
        ["--remote-control", "boss-dev"],
        "the drain runs in its seat; dev is the idle Remote Control door"
    );
    assert!(
        b.log
            .contains("the drain runs supervised in tmux conductor"),
        "{}",
        b.log
    );
    let unseated = boot_seated(None);
    assert_eq!(
        unseated.argv[..2],
        ["--remote-control", "boss-dev"],
        "{:?}",
        unseated.argv
    );
    assert_eq!(
        unseated.argv.len(),
        3,
        "without the boot's decision for the seat the drain prompt is dev's first prompt, as before"
    );
    assert!(
        !dev_claude_sh().contains("seats.tsv"),
        "dev-claude.sh never reads the checkout's seat list for itself: the boot decided once"
    );
}

/// The postStart lines that decide, once, where the drain runs — from
/// `rm -f /work/drain-seat` to the `fi` that closes the decision — with
/// the YAML block's indentation taken off.
fn drain_seat_decision() -> String {
    let m = manifest();
    let lines: Vec<&str> = m.lines().collect();
    let first = lines
        .iter()
        .position(|l| l.trim() == "rm -f /work/drain-seat")
        .expect("the postStart decides where the drain runs, once, into /work/drain-seat");
    let indent = lines[first].len() - lines[first].trim_start().len();
    let len = lines[first..]
        .iter()
        .position(|l| l.trim() == "fi")
        .expect("the decision closes with fi")
        + 1;
    lines[first..first + len]
        .iter()
        .map(|l| l.get(indent..).unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

/// The decision itself (review ce8970ce N1, N2): the drain goes to the
/// conductor's seat only when the switch is on AND the CHECKOUT's door
/// can start seats AND its seat list holds a conductor — the checkout,
/// because the seat runs the checkout's scripts, so a roll whose
/// checkout predates `--boot` drains in dev exactly as before. Every
/// other case leaves no file, which both boot scripts read as `dev`.
#[test]
fn the_boot_gives_the_drain_to_the_seat_only_when_the_checkout_can_run_it() {
    let seats_with = "conductor claude claude@algedonic.dev infra/dev/drain.md 600 -\n";
    let seats_without = "codex-factory codex agent-codex p 60 codex\n";
    for (label, switch_on, boot_door, seats, want) in [
        ("seated", true, true, seats_with, Some("conductor")),
        ("old checkout", true, false, seats_with, None),
        ("no conductor row", true, true, seats_without, None),
        ("switch off", false, true, seats_with, None),
    ] {
        let root = scratch_dir("drain-seat-decision");
        let work = root.join("work");
        let dev = work.join("boss/infra/dev");
        create_dir(&dev);
        if switch_on {
            let switch = root.join(SWITCH.trim_start_matches('/'));
            create_dir(switch.parent().expect("switch has a parent"));
            write_file(&switch, "");
        }
        let door = if boot_door {
            "if [ \"${1:-}\" = --boot ]; then :; fi\n"
        } else {
            "if [ \"${1:-}\" = --detached ]; then :; fi\n"
        };
        write_file(&dev.join("dev-session.sh"), door);
        write_file(&dev.join("seats.tsv"), seats);
        write_file(&work.join("drain-seat"), "stale\n");
        let script = root.join("decide.sh");
        let body = drain_seat_decision().replace("/work/", &format!("{}/", work.display()));
        write_exec(&script, &body);
        let out = Command::new("bash")
            .arg(&script)
            .output()
            .unwrap_or_else(|e| panic!("run {}: {e}", script.display()));
        assert!(out.status.success(), "{label}: {out:?}");
        let got = std::fs::read_to_string(work.join("drain-seat")).ok();
        assert_eq!(
            got.as_deref().map(str::trim),
            want,
            "{label}: a stale decision never survives a boot"
        );
    }
}

#[test]
fn without_the_switch_the_boot_is_unchanged() {
    assert_eq!(
        boot(false, Some(&drain_prompt())),
        ["--remote-control", "boss-dev"],
        "no switch file means no prompt: the session starts idle, as it always did"
    );
}

#[test]
fn with_the_switch_the_drain_prompt_is_the_first_prompt_as_one_argument() {
    let prompt = drain_prompt();
    // `$(…)` drops trailing newlines and nothing else; every backtick,
    // quote and `$` in the prompt must arrive untouched.
    let expected = prompt.trim_end_matches('\n');
    assert_eq!(
        boot(true, Some(&prompt)),
        ["--remote-control", "boss-dev", expected],
        "the switch passes infra/dev/drain.md verbatim as claude's positional prompt"
    );
}

#[test]
fn the_prompt_survives_shell_metacharacters() {
    let hostile = "Run `date -u` and \"$HOME\" and $(echo no) and 'quotes'.\n";
    assert_eq!(
        boot(true, Some(hostile)),
        [
            "--remote-control",
            "boss-dev",
            hostile.trim_end_matches('\n')
        ],
        "the prompt is read at run time with \"$(cat …)\", never expanded"
    );
}

#[test]
fn a_switch_with_no_prompt_file_starts_with_a_failure_notice_rather_than_idle() {
    assert_eq!(
        boot(true, None),
        ["--remote-control", "boss-dev", MISSING_PROMPT_NOTICE],
        "e5cb7c6e: missing prompt must reach the session, not only its boot log"
    );
}

/// The measured failure (7a581397): the checkout is a train behind and
/// drain.md is new in the train that rolled the pod.
#[test]
fn the_prompt_is_read_from_origin_main_when_the_checkout_has_not_pulled_it() {
    let prompt = drain_prompt();
    let booted = boot_with(true, None, Some(&prompt));
    assert_eq!(
        booted.argv,
        [
            "--remote-control",
            "boss-dev",
            prompt.trim_end_matches('\n')
        ],
        "the boot fetches origin and reads origin/main:infra/dev/drain.md\nlog:\n{}",
        booted.log
    );
    assert!(
        booted.log.contains("dev session: drain-on-boot is ON")
            && booted.log.contains("from origin/main"),
        "postStart.log names where the prompt came from:\n{}",
        booted.log
    );
}

/// A train that EDITS drain.md must not boot the pod on the previous
/// prompt.
#[test]
fn a_newer_prompt_on_origin_main_wins_over_the_checkouts_copy() {
    let booted = boot_with(
        true,
        Some("the previous prompt\n"),
        Some("the new prompt\n"),
    );
    assert_eq!(
        booted.argv,
        ["--remote-control", "boss-dev", "the new prompt"],
        "origin/main's drain.md wins over the checkout's\nlog:\n{}",
        booted.log
    );
}

/// With no origin to read — a fetch that fails, a checkout that is not
/// a clone — the checkout's copy is still the prompt, and the log says
/// the fetch failed and which copy it used.
#[test]
fn without_origin_the_checkouts_copy_is_the_prompt_and_the_log_says_so() {
    let booted = boot_with(true, Some("the checkout's prompt\n"), None);
    assert_eq!(
        booted.argv,
        ["--remote-control", "boss-dev", "the checkout's prompt"]
    );
    assert!(
        booted.log.contains("git fetch origin failed") && booted.log.contains("from the checkout"),
        "postStart.log names the failed fetch and the fallback:\n{}",
        booted.log
    );
}

/// The switch is on and there is nothing to read: the session still
/// starts (a session is worth having), but the boot log says the drain
/// did NOT resume. Silence here is what cost the #606 roll its drain.
#[test]
fn a_switch_with_no_readable_prompt_says_so_loudly_in_the_boot_log() {
    let booted = boot_with(true, None, None);
    assert_eq!(
        booted.argv,
        ["--remote-control", "boss-dev", MISSING_PROMPT_NOTICE]
    );
    assert!(
        booted
            .log
            .contains("dev session: DRAIN-ON-BOOT IS ON BUT NO DRAIN PROMPT COULD BE READ"),
        "an ON switch with no prompt is loud in postStart.log:\n{}",
        booted.log
    );
}

#[test]
fn an_empty_drain_prompt_also_reports_failure_as_one_argument() {
    assert_eq!(
        boot(true, Some("")),
        ["--remote-control", "boss-dev", MISSING_PROMPT_NOTICE],
        "an empty readable file cannot silently resume work or start idle"
    );
}

/// Without the switch the boot neither fetches nor writes a word.
#[test]
fn without_the_switch_the_boot_does_not_touch_origin_or_the_log() {
    let booted = boot_with(false, None, Some("the new prompt\n"));
    assert_eq!(booted.argv, ["--remote-control", "boss-dev"]);
    assert_eq!(booted.log, "", "no switch, no fetch, no log line");
}

#[test]
fn the_post_start_log_says_whether_the_drain_is_on() {
    let m = manifest();
    assert!(
        m.contains("dev session: drain-on-boot is ON"),
        "postStart.log records that the boot passed the drain prompt"
    );
    assert!(
        m.contains("dev session: drain-on-boot is off"),
        "postStart.log records that the switch was absent"
    );
}

/// dev-claude.sh appends its verdict to postStart.log from inside tmux,
/// possibly while the postStart block still holds the file open. Both
/// sides must write O_APPEND, or the block's next write lands at its
/// own offset over the verdict.
#[test]
fn the_post_start_block_appends_to_its_log_so_the_boot_verdict_survives() {
    let m = manifest();
    assert!(
        m.contains("} >> /work/ssh/postStart.log 2>&1 || true"),
        "the postStart block writes its log O_APPEND"
    );
    assert!(
        !m.contains("} > /work/ssh/postStart.log"),
        "a truncating redirect on the block would overwrite dev-claude.sh's verdict"
    );
    // Appending alone would grow the log across every boot, so each
    // boot's log must still start empty — truncated once, before the
    // block opens it.
    assert!(
        m.contains("mkdir -p /work/ssh && : > /work/ssh/postStart.log"),
        "the postStart truncates its log once before the block appends to it"
    );
}

/// The prompt is read by a model that starts cold after a roll, so it
/// names the verbs it must run. A leading `-` would be parsed by the
/// CLI as an option rather than as the prompt.
#[test]
fn the_drain_prompt_names_its_verbs_and_cannot_be_read_as_a_flag() {
    let p = drain_prompt();
    assert!(!p.trim_start().is_empty(), "infra/dev/drain.md is empty");
    assert!(
        !p.starts_with('-'),
        "drain.md must not begin with '-': claude would read it as a flag"
    );
    for needle in [
        "date -u",
        "boss orient",
        "boss dispatch",
        "--report",
        "boss triage",
        "boss gate",
        "boss rerail",
        "/loop",
        "drain-on-boot",
    ] {
        assert!(p.contains(needle), "drain.md no longer names `{needle}`");
    }
}

/// The drain keeps its state on packets, never in agent memory (backlog
/// 38db9ebe, 2026-10-18). Until this car the prompt told a cold session
/// to read MEMORY.md and its READ FIRST overnight entry, to take its
/// owed post-land acts from that entry, and to append a line to it on
/// every wake — the session-state class the decision record deletes
/// outright ("Agent memory and the record", design a5368918), and the
/// opposite of David's 2026-10-09 rule that handoff state lives on the
/// item packets and the work-session packet. A memory file is
/// unversioned, outside the log and mutable in place; the next session
/// inherits it only if it happens to load the same file. So the prompt
/// names no memory at all, and names the packet doors instead: the
/// work-session list it reads the last handoff from, the one `handoff`
/// key, and the metadata merge door it writes it through.
///
/// One key, REPLACED each wake (backlog e18b0b44, design e17c81c9,
/// 2026-11-16): a `handoff_<YYYYMMDD>_<HHMM>Z` key per wake grew session
/// 2d013092 to 738,325 bytes, 97% of its metadata handoffs, until the
/// packet budget refused the very write this prompt requires. Every
/// write's `jobs.job.updated` carries the whole row, so the wakes before
/// are in the log, not lost.
#[test]
fn the_drain_keeps_its_state_on_packets_not_in_memory() {
    let p = drain_prompt();
    let memory: Vec<&str> = p
        .lines()
        .filter(|l| l.to_lowercase().contains("memory"))
        .collect();
    assert!(
        memory.is_empty(),
        "drain.md reads or writes agent memory; session state lives on packets:\n{}",
        memory.join("\n")
    );
    for needle in [
        "kind=work-session",
        "`handoff`",
        "PATCH /api/jobs/",
        "/metadata",
    ] {
        assert!(
            p.contains(needle),
            "drain.md no longer names `{needle}`: the packet door its state goes through"
        );
    }
    assert!(
        !p.contains("handoff_<YYYYMMDD>_<HHMM>Z"),
        "drain.md still writes a new handoff key every wake; the session keeps ONE `handoff`, replaced"
    );
}

/// The reader decides by which keys are present and newer, never by the
/// day a session opened (review 2f225d2e of car a9e8e486, finding 1).
/// Measured 2026-11-16 ~15:25Z: session 48c007cc, opened that morning,
/// held 7 `handoff_*` keys and no `handoff`, so a fallback reserved for
/// sessions "opened before 2026-11-16" left tomorrow's drain reading an
/// empty handoff. A session writes the old keys until it loads the new
/// text — a merge, a fast-forward and a relaunch later — not at midnight.
///
/// And the drain is the coordinator, the conductor of these sessions, so
/// it is told where its own orders go (finding 2): a session-wide order
/// is the session's ONE `order` key, an order about an item a
/// nomination on it. Until this text the only writer told about `order`
/// was roll-the-dev-pod's quiesce step, and the conductor wrote
/// `conductor_order_add_20261116_1509Z` onto the factory's session the
/// same afternoon.
#[test]
fn the_drain_reads_handoffs_by_key_and_writes_one_order() {
    // Whitespace collapsed, so a needle the prose wraps still matches.
    let p = drain_prompt()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        !p.contains("opened before 2026-"),
        "drain.md keys its fallback on a session's opening date; key it on which keys are present and newer"
    );
    for needle in ["newest `handoff_*`", "absent or older"] {
        assert!(
            p.contains(needle),
            "drain.md no longer names `{needle}`: the fallback to the per-wake keys a session still holds"
        );
    }
    for needle in [
        "ONE `order` key",
        "`{by, at, text}`",
        "boss step nominate",
        "never a new `conductor_order_add_*`",
    ] {
        assert!(
            p.contains(needle),
            "drain.md no longer names `{needle}`: where the conductor writes an order"
        );
    }
    // The successor path names its door and the field the next drain
    // finds it by (review 2f225d2e, non-blocking).
    for needle in ["boss job file --kind work-session", "`started_at`"] {
        assert!(
            p.contains(needle),
            "drain.md's successor path no longer names `{needle}`"
        );
    }
}

/// A CAPACITY REFUSAL IS A QUEUE, NOT A STOP (backlog a3bb1743, measured
/// 2026-11-16 03:31Z). Section 7 said "never bypass or retry around a
/// refusal" with no line between the claim door's 409 "resource claim
/// refused" (pool exhausted, the step queued in its line) and a policy or
/// permission refusal, and the factory read the first as the second for
/// hours while slots stood free. The section names the difference and the
/// act: other ready work now, the claim again on the next wake.
///
/// The TEST is the body's `queued` field, never the 409 text (backlog
/// dfc9884e, review of car b8584ecb): every reason the claim door refuses
/// other than capacity-exhausted answers the same "resource claim refused"
/// with no `queued`, and matching the text would retry one every wake and
/// never file it. The old text-keyed sentence and the refuted closed count
/// ("five declaration defects", review b46744de) are refused outright, so
/// the needles are not vocabulary alone. Only a scheduler's
/// Insufficient cpu or memory (dev-build.py's CAPACITY_MISS) is a worker
/// queue; affinity or taints are a placement defect to file. The prompt
/// names that class by CAPACITY_MISS and "cpu or RAM", never by the
/// scheduler's own words: the test above refuses any line of drain.md
/// carrying the word for RAM, and the first gate of this car went red
/// on exactly that (gate-run 64151038).
#[test]
fn the_drain_names_a_capacity_refusal_a_queue_not_a_stop() {
    let raw = drain_prompt();
    let p = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let never = p
        .split_once("## 7. Never")
        .map(|(_, rest)| rest.split("## ").next().unwrap_or(rest))
        .expect("drain.md has its Never section");
    for needle in [
        "resource claim refused",
        "a capacity refusal is a queue",
        "other ready work",
        "next wake",
        "a `queued` field",
        "no `queued` field",
        "every other reason the claim door refuses",
        "CAPACITY_MISS",
        "a refusal to file",
    ] {
        assert!(
            never.contains(needle),
            "section 7 must carry `{needle}`: a capacity refusal is a queue, not a refusal to \
             file and stop on:\n{never}"
        );
    }
    for stale in [
        "When the claim door answers 409 \"resource claim refused\" because",
        "five declaration defects",
    ] {
        assert!(
            !never.contains(stale),
            "section 7 must not carry `{stale}`: the `queued` field is the test, and the \
             refusals without it are not a closed set:\n{never}"
        );
    }
}

/// THE CONDUCTOR RECOVERS WHAT STANDS BELOW IT (backlog de37ea3d, car A;
/// David, 2026-11-18: "I want that seat to be able to recover agents or
/// sessions assigned to lower-level stations"). Until this text section 7
/// said "Never hand-delete, hand-restart, stop, or kill anything" and
/// section 2 said a run that died with the pod is never closed by hand,
/// so every stalled seat and dead run waited on David — measured that
/// day: Claude B's seat not started after the 13:19Z roll, Codex's seat
/// running with BOSS_ACTOR blank since boot, dead run f7b9c25b holding a
/// step for three hours. The grant is three acts, each through its door
/// and each recorded with its reason; a turn that is running is never
/// interrupted (576de803: a seat stopped mid-turn loses the turn), and
/// what policy calls destructive stays out. The turn-boundary verb is
/// car B of the same item; until it lands the two reads of act 2 are
/// the wait.
///
/// Review 9db36fe8 (Claude B, 2026-11-19) held the first text to four
/// more lines, each pinned below. B1: a boot without `--except
/// conductor` starts tmux `conductor`, a SECOND drain beside the one in
/// tmux `dev` (boss-dev.yaml adds the flag at boot; the conductor row
/// has no twin to refuse it). B2: "between turns" is read off the
/// seat's own loop log and its pane, never off the `factory_turn`
/// heartbeat alone, which the loop writes best-effort and an unsigned
/// seat cannot write at all — a stale `end` on a running turn read as a
/// boundary is silence counted as a pass. B3: a run that died with its
/// pod ends `died` with `host_gone`, the observer's terminal for the
/// same fact; `refused` counts it cancelled, not failed, and says no
/// work was done. N1: David's sentence is copied, and the act list is
/// marked as the conductor's reading of it.
#[test]
fn the_conductor_recovers_the_seats_and_runs_below_its_root_station() {
    let raw = drain_prompt();
    let p = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let never = p
        .split_once("## 7. Never")
        .map(|(_, rest)| rest.split("## ").next().unwrap_or(rest))
        .expect("drain.md has its Never section");
    for needle in [
        "THE RECOVERY GRANT",
        "the root station, `algedonic`",
        "de37ea3d",
        // N1: the grant's words are David's, copied, not retyped.
        "\"yes, restart the codex seat, and I don't need that to be restricted to me. I am \
         thinking that we should consider this session to allocated to the whole company \
         system, i.e. sitting in the Algedonic station, and I want that seat to be able to \
         recover agents or sessions assigned to lower-level stations.\"",
        "the conductor's reading of that sentence",
        // N2: the registry row is not written yet, and the text says so.
        "pending",
        // B1: every boot the grant names leaves the drain where it is.
        "infra/dev/dev-session.sh --boot --except conductor",
        "Never start the conductor seat.",
        "seat-off",
        "only between its turns",
        // B2: the boundary is read from the seat's own loop, not the API.
        "factory-loop-<seat>.log",
        "turn <N> ended cleanly",
        "turn <N> FAILED",
        "no `timeout`",
        "is a running turn",
        "never decides",
        "576de803",
        "Never your own seat.",
        // R1 (review a2e4bbce): the reads that justify the record never
        // decide the kill; one command re-reads the pane and kills only
        // if that read holds. Its behaviour is pinned below.
        "a2e4bbce",
        "ONE command",
        "at least 10 seconds of it left",
        "under 20 ms",
        "Only after `killed`",
        // N3: an unsigned seat is restarted only once the registry names it.
        "boss-api GET /api/agents",
        // N-C: by the predicate the boot itself uses, the twin's alias.
        "`codex@algedonic.dev`",
        "`claude-b@algedonic.dev`",
        "`aliases`",
        // N-B: the boot's own restart hint is not the door.
        "never that line",
        // N-D: a seat David turned off is left off, not stopped.
        "the restart would become a stop",
        // N-A: the report verb's advice for a no-terminal run is not this one's.
        "disregard it",
        // N5: tmux's exact-name target.
        "tmux kill-session -t =<seat>",
        // B3 and N4: the observer's terminal, judged the observer's way.
        "boss step complete <run> --step building --field result=died",
        "`host_gone`",
        "estate observer",
        "phase `Failed` or `Succeeded`",
        "open or green",
        "no answer is not \"no gate\"",
        "never pushed",
        // N6: the record comes first.
        "Record each act BEFORE you take it",
        "`recovered_<YYYYMMDD>_<HHMM>Z`",
        "boss dispatch <run> --report",
        "Destructive-by-policy acts stay excluded",
        "starting a second drain",
        "killing or interrupting a turn that is running",
        "bypassing a refusal",
        "credentials",
        "publishing a held protocol",
    ] {
        assert!(
            never.contains(needle),
            "section 7 must carry `{needle}`: the conductor's recovery grant, its doors, \
             its record and its exclusions:\n{never}"
        );
    }
    assert!(
        !never.contains("Never hand-delete, hand-restart"),
        "section 7 still forbids every restart outright, which the grant of de37ea3d \
         replaced:\n{never}"
    );
    // B1: not one boot in the grant may omit the flag — a bare
    // `--boot` anywhere in it is the second drain.
    let boots = never.matches("dev-session.sh --boot").count();
    let guarded = never
        .matches("dev-session.sh --boot --except conductor")
        .count();
    assert!(
        boots > 0 && boots == guarded,
        "section 7 names `dev-session.sh --boot` {boots} time(s) and only {guarded} with \
         `--except conductor`; a boot without it starts tmux `conductor`, a second drain \
         beside the one in tmux `dev` (review 9db36fe8 B1):\n{never}"
    );
    // B3: a pod-dead run is never ended `refused` by this grant.
    assert!(
        !never.contains("result=refused"),
        "section 7 ends a pod-dead run `refused`, which counts it cancelled where the \
         observer's `died` counts it failed (review 9db36fe8 B3):\n{never}"
    );
    // B2: the heartbeat may corroborate, never decide.
    assert!(
        !never.contains("read `factory_turn.phase` as `end` immediately before you act"),
        "section 7 still judges the turn boundary off the best-effort heartbeat alone \
         (review 9db36fe8 B2):\n{never}"
    );
    // N5: tmux's bare target matches by prefix and pattern.
    assert!(
        !never.contains("tmux kill-session -t <seat>"),
        "section 7 names a tmux target without `=`, which matches by prefix \
         (review 9db36fe8 N5):\n{never}"
    );
    // R1: a margin read before the record is spent by the record; the
    // margin lives in the guarded command now, read at the kill.
    assert!(
        !never.contains("at least 20 seconds must remain"),
        "section 7 still sets the turn margin on a read taken before the record, \
         which a record written between the read and the kill can spend \
         (review a2e4bbce R1):\n{never}"
    );
    // N-E: only `refused` maps to cancelled; `handed` has no outcome.
    assert!(
        !never.contains("Ended any other way"),
        "section 7 says any other terminal counts a pod death cancelled; only \
         `refused` does (review a2e4bbce N-E):\n{never}"
    );
    let roll = p
        .split_once("## 2. Close what the roll left open")
        .map(|(_, rest)| rest.split("## ").next().unwrap_or(rest))
        .expect("drain.md has its roll section");
    assert!(
        !roll.contains("Do not close them by hand"),
        "section 2 still forbids ending a run that died with the pod, which section 7 \
         now grants when the estate observer has not:\n{roll}"
    );
    assert!(
        roll.contains("section 7"),
        "section 2 points a dead run the observer missed at section 7's grant:\n{roll}"
    );
}

/// R1 of review a2e4bbce (Claude B, 2026-11-20): act 2 read the seat's
/// log and pane, then wrote its record, then killed. A lower seat waits
/// 60 s between turns and the conductor's tool calls ran 10-22 s apart,
/// so read, record and kill could straddle the next turn's start and
/// kill a turn 8 s old. The deciding read and the kill are now ONE
/// command of drain.md, run after the record, that reads the pane again
/// and kills only if that read holds. These tests run the two commands
/// exactly as drain.md prints them, against a fake `tmux` whose pane is
/// a real process of the shape factory-loop.sh gives a seat: `sleep
/// <wait>` in its wait, `timeout … <cli>` in a turn.
fn act_two_guards() -> (String, String) {
    let raw = drain_prompt();
    let never = raw
        .split_once("## 7. Never")
        .map(|(_, rest)| rest.split("\n## ").next().unwrap_or(rest))
        .expect("drain.md has its Never section");
    let guards: Vec<&str> = never
        .lines()
        .map(str::trim)
        .filter(|l| l.contains("tmux list-panes -t =<seat>") && l.contains("tmux kill-session"))
        .collect();
    assert_eq!(
        guards.len(),
        2,
        "section 7 prints two guarded kills, one for a seat in its wait and one for a \
         stalled seat, each ONE line that reads the pane and kills (review a2e4bbce R1); \
         found {guards:?}"
    );
    let pick = |needle: &str| {
        guards
            .iter()
            .find(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("no guarded kill in section 7 carries `{needle}`"))
            .to_string()
    };
    (pick("sleep <wait>"), pick("turn <N+1>: "))
}

/// A real process standing in for a seat's pane: a bash whose one child
/// is what `script` runs, in its own process group so the whole pane
/// is ended when the test is.
struct Pane(std::process::Child);

impl Pane {
    fn running(script: &str) -> Pane {
        use std::os::unix::process::CommandExt;
        // Owned by the Pane at once, so every path out of here, the
        // panic below included, ends and reaps it through Drop.
        let pane = Pane(
            Command::new("bash")
                .args(["-c", script])
                .process_group(0)
                .spawn()
                .expect("spawn the stand-in pane"),
        );
        let pid = pane.0.id().to_string();
        // Wait until bash has forked the child the guard reads.
        for _ in 0..100 {
            let kids = Command::new("ps")
                .args(["-o", "args=", "--ppid", &pid])
                .output()
                .expect("ps");
            if !kids.stdout.is_empty() {
                return pane;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        panic!("the stand-in pane `{script}` never forked its child");
    }
}

impl Drop for Pane {
    fn drop(&mut self) {
        let _ = Command::new("kill")
            .args(["-KILL", "--", &format!("-{}", self.0.id())])
            .status();
        let _ = self.0.wait();
    }
}

/// What one guarded command did: what it printed, and the
/// `kill-session` the fake tmux received, if any.
struct Guarded {
    out: String,
    killed: Option<String>,
}

fn run_guard(guard: &str, pane: &Pane, wait: &str, log: &str, seat_off: bool) -> Guarded {
    let root = scratch_dir("act-two-guard");
    let bin = root.join("bin");
    create_dir(&bin);
    // Answers only the exact-name target: a guard that drops the `=`
    // reads no pane and so kills nothing.
    write_exec(
        &bin.join("tmux"),
        "#!/bin/bash\ncase $1 in\n  list-panes) [ \"$3\" = =codex-factory ] && echo \"$FAKE_PANE_PID\" ;;\n  kill-session) echo \"$*\" > \"$FAKE_KILLED\" ;;\n  *) exit 1 ;;\nesac\n",
    );
    let home = root.join("home");
    create_dir(&home.join(".config/boss/seat-off"));
    if seat_off {
        write_file(&home.join(".config/boss/seat-off/codex-factory"), "");
    }
    let log_path = root.join("factory-loop.log");
    if log != MISSING {
        write_file(&log_path, log);
    }
    let killed = root.join("killed");
    let cmd = guard
        .replace("<seat>", "codex-factory")
        .replace("<wait>", wait)
        .replace("<N+1>", "381")
        .replace("<N>", "380")
        .replace("<log>", &log_path.display().to_string());
    let out = Command::new("bash")
        .args(["-c", &cmd])
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("HOME", &home)
        .env("FAKE_PANE_PID", pane.0.id().to_string())
        .env("FAKE_KILLED", &killed)
        .output()
        .expect("run the guarded command");
    Guarded {
        out: String::from_utf8_lossy(&out.stdout).into_owned(),
        killed: std::fs::read_to_string(&killed).ok(),
    }
}

/// A `log` argument that leaves the loop log unwritten.
const MISSING: &str = "\0missing";

const ENDED: &str = "factory-loop 2026-11-20T02:41:03Z: turn 380: codex exec -\n\
                     factory-loop 2026-11-20T02:49:12Z: turn 380 ended cleanly; next turn in 60s\n";

#[test]
fn the_act_two_kill_lands_only_in_a_seats_wait_with_time_left() {
    let (wait_guard, _) = act_two_guards();

    let waiting = Pane::running("sleep 60; true");
    let g = run_guard(&wait_guard, &waiting, "60", ENDED, false);
    assert_eq!(
        g.killed.as_deref().map(str::trim),
        Some("kill-session -t =codex-factory"),
        "a seat whose pane's one child is `sleep 60`, seconds into it, is in its wait and \
         is restarted; the guard printed: {}",
        g.out
    );
    assert!(
        g.out.starts_with("killed"),
        "the guard says it killed: {}",
        g.out
    );

    let in_turn = Pane::running("timeout 60 sleep 60; true");
    let g = run_guard(&wait_guard, &in_turn, "60", ENDED, false);
    assert_eq!(
        g.killed, None,
        "a pane running `timeout` is a turn, whatever the log said a minute ago"
    );
    assert!(
        g.out.starts_with("NOT killed"),
        "the guard says it did not: {}",
        g.out
    );

    // A wait with less than 10 s left: the next turn could start between
    // the read and the kill, so the guard refuses.
    let ending = Pane::running("sleep 9; true");
    let g = run_guard(&wait_guard, &ending, "9", ENDED, false);
    assert_eq!(
        g.killed, None,
        "a wait with under 10 s left is too close to the next turn to kill in; the \
         guard printed: {}",
        g.out
    );

    // N-D: a seat David turned off is left off, not stopped.
    let off = Pane::running("sleep 60; true");
    let g = run_guard(&wait_guard, &off, "60", ENDED, true);
    assert_eq!(
        g.killed, None,
        "a seat with a seat-off file is David's to restart; the boot after a kill would \
         skip it and the restart would become a stop (review a2e4bbce N-D)"
    );
}

#[test]
fn the_act_two_kill_of_a_stalled_seat_holds_off_any_turn() {
    let (_, stall_guard) = act_two_guards();

    let stuck = Pane::running("sleep 60; true");
    let g = run_guard(&stall_guard, &stuck, "60", ENDED, false);
    assert_eq!(
        g.killed.as_deref().map(str::trim),
        Some("kill-session -t =codex-factory"),
        "a stalled seat, no turn in its pane and none begun in its log, is restarted; the \
         guard printed: {}",
        g.out
    );

    let in_turn = Pane::running("timeout 60 sleep 60; true");
    let g = run_guard(&stall_guard, &in_turn, "60", ENDED, false);
    assert_eq!(
        g.killed, None,
        "a pane running `timeout` is a turn: {}",
        g.out
    );

    let moved_on = format!("{ENDED}factory-loop 2026-11-20T03:30:00Z: turn 381: codex exec -\n");
    let g = run_guard(&stall_guard, &stuck, "60", &moved_on, false);
    assert_eq!(
        g.killed, None,
        "a seat whose log began turn 381 since the record is no longer stalled: {}",
        g.out
    );

    // An unreadable log is no answer, and no answer is a running turn.
    let g = run_guard(&stall_guard, &stuck, "60", MISSING, false);
    assert_eq!(
        g.killed, None,
        "a log the guard cannot read kills nothing: {}",
        g.out
    );

    let off = Pane::running("sleep 60; true");
    let g = run_guard(&stall_guard, &off, "60", ENDED, true);
    assert_eq!(g.killed, None, "a seat-off seat is left off: {}", g.out);
}

/// The drain's post-land acts name `boss workflow publish` for a row a
/// car changed — and the hand publish is the ONE door a hold does not
/// bind (infra/platform/workflow-holds/README.md). A session resuming
/// cold from this prompt after the signer car lands would have published
/// a refusal row with no human decision (review 8d088b41 of car 052016d7,
/// finding F3). So the paragraph that names the publish carries the
/// exception, and says how to tell: the tree's one reader of the holds.
#[test]
fn the_drain_never_publishes_a_held_kind() {
    let p = drain_prompt();
    let at = p
        .find("boss workflow publish")
        .expect("drain.md names the publish among its post-land acts");
    let paragraph = p[at..].split("\n\n").next().unwrap();
    for needle in [
        "HELD",
        "NEVER",
        "infra/gcp/workflow-holds.py",
        // A reader that cannot answer is not a reader that says "open"
        // (review 6ff3210e, N4: this clause could be deleted unnoticed).
        "exits non-zero",
        "publish nothing",
        // And the tree it is asked of is one that carries the landing:
        // /work/boss is fast-forwarded hourly and answers for the tree
        // BEFORE the car for up to an hour (same review, N6).
        "origin/main",
    ] {
        assert!(
            paragraph.contains(needle),
            "the paragraph that names the publish must carry `{needle}` — a held kind is never \
             published by a draining session, and the reader is how to tell:\n{paragraph}"
        );
    }
    assert_eq!(
        p.matches("boss workflow publish").count(),
        1,
        "a second place naming the publish needs the same exception"
    );
}

/// The prompt drifted from the tree it drives (backlog abd1c796,
/// measured 2026-11-09 on origin/main 6c9f2d0cd). Last edited
/// 2026-10-18, it still said a row declaring a `writer` or an
/// `executor` is held "by default because" of it — a default a4b57aca
/// retired, so a cold session would hold kinds nothing holds. It never
/// named the shared checkout's renewal door under dev isolation, it
/// ordered work oldest-first against the urgent-first door (90bff1f3),
/// and it capped builders at 5 where David set 8 on 2026-11-09. Each
/// claim is pinned to the words a resuming session acts on.
#[test]
fn the_drain_follows_the_tree_it_drives() {
    let p = drain_prompt();
    let at = p
        .find("boss workflow publish")
        .expect("drain.md names the publish among its post-land acts");
    let publish = p[at..].split("\n\n").next().unwrap();
    assert!(
        !publish.contains("by default because"),
        "drain.md still holds writer/executor rows by default, which a4b57aca retired:\n{publish}"
    );
    assert!(
        publish.contains("a4b57aca"),
        "the publish paragraph names the item that retired the default hold:\n{publish}"
    );
    for needle in ["renew-checkout", "boss dispatch --next"] {
        assert!(p.contains(needle), "drain.md no longer names `{needle}`");
    }
    let force = p
        .lines()
        .find(|l| l.contains("--force-standard"))
        .expect("drain.md names `--force-standard`, the urgent-first door's escape");
    assert!(
        force.contains("Never pass"),
        "`--force-standard` is named only to forbid it:\n{force}"
    );
    assert!(
        p.contains("up to 8 builder") && p.contains("The cap is 8"),
        "the builder cap is 8 (David, 2026-11-09)"
    );
    assert!(!p.contains("at most 5 builder"), "the old cap of 5 is gone");
}
