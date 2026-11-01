//! A green gate's verdict does not wait for its seed housekeeping.
//!
//! THE DEFECT (backlog 3e2c8ddc, folded with 82ffaff3). The conductor
//! records a gate's verdict only once its `gate` container has ENDED
//! (boss-cli `train/carried_verdict.rs`), and `run.sh` refreshed the warm
//! seed and emptied its workspace before ending. So a green waited out its
//! own housekeeping: gate-run 86dd2d48 (2026-10-16) recorded its receipt,
//! then copied the seed for 638 s; gate-run 4aad01a9 (2026-10-17) sat
//! about eight minutes in `find /gate-target -delete`, blocked in
//! uninterruptible I/O, with its verdict already written.
//!
//! THE FIX (design b4bf8bba, approved 2026-10-16). On the judged-init
//! layout `gate` is an init container and `seed-refresh` the regular one
//! after it: a green leaves ONE line for the helper and ends on its
//! verdict, and `seed-refresh.sh maintain` refreshes, empties the
//! workspace, each under a bound, and leaves one record line the
//! conductor writes beside the verdict. The old layout — every manifest
//! rendered before this one — refreshes inside the gate as before,
//! through the same `seed-refresh.sh refresh`, so the refresh lives once.
//!
//! These pins RUN what ships: the helper script itself against a scratch
//! seed and workspace, and run.sh's housekeeping block lifted between its
//! markers.

use boss_testing::{repo_root, scratch_dir, write_exec};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const HELPER: &str = "infra/gate-runner/seed-refresh.sh";
const RUN_SH: &str = "infra/gate-runner/run.sh";
const HEAD: &str = "26b7e3081a52923879dd07cda85c2f3f74e7abdf";
const OLD_HEAD: &str = "0000000000000000000000000000000000000001";

struct Rig {
    dir: PathBuf,
    seed: PathBuf,
    ws: PathBuf,
    term: PathBuf,
}

/// A seed holding OLD_HEAD's target and a workspace holding a fresh one
/// with two builds of the same test binary.
fn rig(tag: &str) -> Rig {
    let dir = scratch_dir(&format!("verdict-not-waiting-{tag}"));
    let seed = dir.join("gate-seed");
    let ws = dir.join("gate-target");
    fs::create_dir_all(seed.join("target/debug")).unwrap();
    fs::write(seed.join("target/debug/old-artifact"), b"old").unwrap();
    fs::write(seed.join(".seed-head"), format!("{OLD_HEAD}\n")).unwrap();
    let deps = ws.join("target/debug/deps");
    fs::create_dir_all(&deps).unwrap();
    fs::write(deps.join("libboss-1111111111111111.rlib"), b"lib").unwrap();
    write_exec(&deps.join("boss-aaaaaaaaaaaaaaaa"), "old build");
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    let secs = old.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    assert!(
        Command::new("touch")
            .arg("-d")
            .arg(format!("@{secs}"))
            .arg(deps.join("boss-aaaaaaaaaaaaaaaa"))
            .status()
            .unwrap()
            .success()
    );
    write_exec(&deps.join("boss-bbbbbbbbbbbbbbbb"), "new build");
    fs::write(ws.join("gate.log"), b"the run's log").unwrap();
    Rig {
        term: dir.join("termination-log"),
        dir,
        seed,
        ws,
    }
}

/// `seed-refresh.sh <args>` as it ships, against the rig, with `env`.
fn helper(r: &Rig, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new("bash");
    cmd.arg(repo_root().join(HELPER))
        .args(args)
        .env("GATE_SEED_DIR", &r.seed)
        .env("GATE_WORKSPACE_DIR", &r.ws)
        .env("GATE_TERMINATION_LOG", &r.term)
        .env("NATIVE_JOB_NAME", "gate-fix-x-abc12")
        .stdin(Stdio::null());
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().unwrap()
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// The helper's record line, as the termination message carries it.
fn record(r: &Rig) -> String {
    fs::read_to_string(&r.term).unwrap_or_default()
}

fn field<'a>(record: &'a str, key: &str) -> &'a str {
    record
        .trim_end()
        .split(' ')
        .find_map(|f| f.strip_prefix(&format!("{key}=")))
        .unwrap_or_else(|| panic!("no `{key}` in {record:?}"))
}

fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .map(|rd| {
            rd.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// The ordinary green: the helper installs the workspace's target as the
/// new seed (pruned, staged, renamed, then marked), empties the workspace,
/// and leaves its one record line — on stdout and as its termination
/// message, naming its own Job.
#[test]
fn the_helper_refreshes_then_empties_then_records() {
    let r = rig("refresh");
    fs::write(r.ws.join(".seed-handoff"), format!("refresh {HEAD}\n")).unwrap();
    let out = helper(&r, &["maintain"], &[]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(
        fs::read_to_string(r.seed.join(".seed-head"))
            .unwrap()
            .trim(),
        HEAD
    );
    assert_eq!(
        listing(&r.seed.join("target/debug/deps")),
        vec!["boss-bbbbbbbbbbbbbbbb", "libboss-1111111111111111.rlib"],
        "the newest build per stem and every library — the old seed is gone"
    );
    assert!(!r.seed.join("target.partial").exists());
    assert!(
        listing(&r.ws).is_empty(),
        "the workspace is emptied: {:?}",
        listing(&r.ws)
    );
    let rec = record(&r);
    assert!(
        rec.starts_with("gate-runner: seed-maintenance v1 "),
        "{rec:?}"
    );
    assert_eq!(rec.lines().count(), 1, "{rec:?}");
    assert_eq!(field(&rec, "job"), "gate-fix-x-abc12");
    assert_eq!(field(&rec, "head"), HEAD);
    assert_eq!(field(&rec, "refresh"), "refreshed");
    assert_eq!(field(&rec, "cleanup"), "done");
    assert_eq!(field(&rec, "deadline_s"), "900");
    assert!(field(&rec, "refresh_s").parse::<u64>().is_ok());
    assert!(field(&rec, "cleanup_s").parse::<u64>().is_ok());
    assert!(
        text(&out).contains(rec.trim_end()),
        "the record is in the log too"
    );
}

/// The gate's own decision not to refresh is carried through, and the
/// workspace is still emptied; a missing handoff, or one that is not of
/// the fixed shape — a path, a command, another word — refreshes NOTHING.
#[test]
fn a_skip_a_missing_or_a_foreign_handoff_refreshes_nothing() {
    for (tag, handoff, want) in [
        (
            "skip",
            Some("skip behind-main\n".to_string()),
            "skipped:behind-main",
        ),
        ("none", None, "skipped:no-handoff"),
        (
            "path",
            Some("refresh ../../gate-seed/target\n".into()),
            "refused:handoff-shape",
        ),
        (
            "command",
            Some(format!("refresh {HEAD}; rm -rf /\n")),
            "refused:handoff-shape",
        ),
        (
            "upper",
            Some(format!("refresh {}\n", HEAD.to_uppercase())),
            "refused:handoff-shape",
        ),
        (
            "word",
            Some("skip Behind main\n".into()),
            "refused:handoff-shape",
        ),
    ] {
        let r = rig(&format!("skip-{tag}"));
        if let Some(h) = handoff {
            fs::write(r.ws.join(".seed-handoff"), h).unwrap();
        }
        let out = helper(&r, &["maintain"], &[]);
        assert!(out.status.success(), "{tag}: {}", text(&out));
        let rec = record(&r);
        assert_eq!(field(&rec, "refresh"), want, "{tag}: {rec}");
        assert_eq!(field(&rec, "head"), "-", "{tag}");
        assert_eq!(
            fs::read_to_string(r.seed.join(".seed-head"))
                .unwrap()
                .trim(),
            OLD_HEAD,
            "{tag}: the seed is untouched"
        );
        assert!(r.seed.join("target/debug/old-artifact").exists(), "{tag}");
        assert_eq!(field(&rec, "cleanup"), "done", "{tag}");
        assert!(listing(&r.ws).is_empty(), "{tag}: {:?}", listing(&r.ws));
    }
}

/// Best-effort housekeeping does not queue: another writer holding the
/// seed lock means skip, at once; and a seed some other refresher already
/// brought to this head is not rewritten — asked again INSIDE the lock.
#[test]
fn a_busy_lock_or_a_current_seed_is_skipped_inside_the_lock() {
    let r = rig("busy");
    fs::write(r.ws.join(".seed-handoff"), format!("refresh {HEAD}\n")).unwrap();
    let mut holder = Command::new("flock")
        .arg("-x")
        .arg(r.seed.join(".seed.lock"))
        .arg("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    // The holder has the lock once flock -n from here cannot take it.
    let lock = r.seed.join(".seed.lock");
    let t0 = std::time::Instant::now();
    while Command::new("flock")
        .args(["-x", "-n"])
        .arg(&lock)
        .arg("true")
        .status()
        .unwrap()
        .success()
    {
        assert!(
            t0.elapsed().as_secs() < 10,
            "the holder never took the lock"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let out = helper(&r, &["maintain"], &[]);
    holder.kill().ok();
    holder.wait().ok();
    assert!(out.status.success(), "{}", text(&out));
    let rec = record(&r);
    assert_eq!(field(&rec, "refresh"), "skipped:lock-busy", "{rec}");
    assert_eq!(
        fs::read_to_string(r.seed.join(".seed-head"))
            .unwrap()
            .trim(),
        OLD_HEAD
    );

    let r = rig("current");
    fs::write(r.seed.join(".seed-head"), format!("{HEAD}\n")).unwrap();
    fs::write(r.ws.join(".seed-handoff"), format!("refresh {HEAD}\n")).unwrap();
    let out = helper(&r, &["maintain"], &[]);
    let rec = record(&r);
    assert_eq!(
        field(&rec, "refresh"),
        "skipped:already-current",
        "{}",
        text(&out)
    );
    assert!(r.seed.join("target/debug/old-artifact").exists());
}

/// THE BOUND (design b4bf8bba, Q2). A refresh past its deadline is ended
/// and said `timed-out`, and what it leaves is NO ready seed — never a
/// staged copy under a fresh marker — and the workspace is still emptied.
#[test]
fn a_refresh_past_its_deadline_is_ended_and_leaves_no_torn_seed() {
    let r = rig("deadline");
    fs::write(r.ws.join(".seed-handoff"), format!("refresh {HEAD}\n")).unwrap();
    // A copy that never finishes: `cp` on this PATH sleeps.
    let bin = r.dir.join("bin");
    fs::create_dir_all(&bin).unwrap();
    write_exec(&bin.join("cp"), "#!/bin/sh\nexec sleep 60\n");
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let t0 = std::time::Instant::now();
    let out = helper(
        &r,
        &["maintain"],
        &[
            ("PATH", &path),
            ("GATE_SEED_MAINTENANCE_DEADLINE_SECS", "2"),
            ("GATE_SEED_MAINTENANCE_GRACE_SECS", "1"),
        ],
    );
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        t0.elapsed().as_secs() < 30,
        "the deadline ended the copy: {:?}",
        t0.elapsed()
    );
    let rec = record(&r);
    assert_eq!(field(&rec, "refresh"), "timed-out", "{rec}");
    assert_eq!(field(&rec, "deadline_s"), "2");
    assert!(
        !r.seed.join(".seed-head").exists(),
        "no marker may name a seed the refresh did not finish"
    );
    assert!(
        !r.seed.join("target").exists(),
        "no ready target: the next gate builds cold, never torn"
    );
    assert_eq!(field(&rec, "cleanup"), "done");
    assert!(listing(&r.ws).is_empty());
}

/// run.sh's housekeeping block, lifted between its markers and run with
/// git stubbed to say "at main's tip": what it decides and where.
fn housekeeping(tag: &str, layout: &str, verdict: &str) -> (Rig, String, PathBuf) {
    const BEGIN: &str = "# --- seed housekeeping (begin) ---";
    const END: &str = "# --- seed housekeeping (end) ---";
    let src = fs::read_to_string(repo_root().join(RUN_SH)).unwrap();
    let block = &src
        [src.find(BEGIN).expect("begin marker")..src.find(END).expect("end marker") + END.len()];
    let r = rig(&format!("block-{tag}"));
    let bin = r.dir.join("bin");
    fs::create_dir_all(&bin).unwrap();
    write_exec(
        &bin.join("git"),
        "#!/bin/sh\ncase \"$1\" in rev-list) echo 0 ;; esac\nexit 0\n",
    );
    // The old layout's in-container refresh, as a stand-in beside the
    // lifted script: it says what it was asked.
    let calls = r.dir.join("refresh-calls");
    write_exec(
        &r.dir.join("seed-refresh.sh"),
        &format!("#!/bin/sh\necho \"$@\" >> {}\n", calls.display()),
    );
    let script = r.dir.join("block.sh");
    fs::write(
        &script,
        format!(
            "set -euo pipefail\n\
             SEED={seed}\nVERDICT={verdict}\nREPORTED=1\nHEAD_SHA={HEAD}\n\
             SEED_MAIN=fetched\n\
             GATE_VERDICT_CARRIER=pod-log\nGATE_LAYOUT={layout}\nHANDED_OFF=0\n\
             carried() {{ [ \"$GATE_VERDICT_CARRIER\" = pod-log ]; }}\n\
             judged_init() {{ carried && [ \"$GATE_LAYOUT\" = judged-init ]; }}\n\
             {block}\n\
             echo \"HANDED_OFF=$HANDED_OFF\"\n",
            seed = r.seed.display(),
            block = block.replace("/gate-target", &r.ws.display().to_string()),
        ),
    )
    .unwrap();
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = Command::new("bash")
        .arg(&script)
        .env("PATH", path)
        .output()
        .unwrap();
    let said = text(&out);
    assert!(out.status.success(), "{tag}: {said}");
    (r, said, calls)
}

/// THE JUDGED-INIT GREEN ends on its verdict: it leaves `refresh <head>`
/// for the helper, keeps its workspace for it (HANDED_OFF), and refreshes
/// nothing itself.
#[test]
fn a_judged_init_green_hands_its_seed_to_the_helper_and_refreshes_nothing() {
    let (r, said, calls) = housekeeping("judged-green", "judged-init", "green");
    assert_eq!(
        fs::read_to_string(r.ws.join(".seed-handoff")).unwrap(),
        format!("refresh {HEAD}\n")
    );
    assert!(said.contains("HANDED_OFF=1"), "{said}");
    assert!(
        !calls.exists(),
        "the gate container refreshed in place: {said}"
    );
}

/// The old layout refreshes in the gate container, through the helper
/// script's own `refresh`, and leaves no handoff; a red refreshes nothing
/// on either layout and keeps its workspace for its own EXIT trap.
#[test]
fn the_old_layout_refreshes_in_place_and_a_red_never_hands_off() {
    let (r, said, calls) = housekeeping("old-green", "", "green");
    assert_eq!(
        fs::read_to_string(&calls).unwrap().trim(),
        format!("refresh {HEAD}")
    );
    assert!(!r.ws.join(".seed-handoff").exists());
    assert!(said.contains("HANDED_OFF=0"), "{said}");
    for layout in ["", "judged-init"] {
        let (r, said, calls) = housekeeping(&format!("red-{layout}"), layout, "failed");
        assert!(!calls.exists(), "{layout}: {said}");
        assert!(!r.ws.join(".seed-handoff").exists(), "{layout}");
        assert!(said.contains("HANDED_OFF=0"), "{layout}: {said}");
    }
}

/// The EXIT trap's `empty_workspace` keeps a handed-off workspace for the
/// helper and empties every other — keyed on the script's own variable,
/// never on a file branch code could plant.
#[test]
fn the_exit_trap_spares_only_a_workspace_this_script_handed_off() {
    let src = fs::read_to_string(repo_root().join(RUN_SH)).unwrap();
    let start = src.find("empty_workspace() {").expect("empty_workspace");
    let body = &src[start..start + src[start..].find("\n}\n").unwrap() + 3];
    for (handed, kept) in [("1", true), ("0", false)] {
        let r = rig(&format!("exit-{handed}"));
        fs::write(r.ws.join(".seed-handoff"), format!("refresh {HEAD}\n")).unwrap();
        let script = format!(
            "HANDED_OFF={handed}\n{}\nempty_workspace\n",
            body.replace("/gate-target", &r.ws.display().to_string())
        );
        let out = Command::new("bash").arg("-c").arg(script).output().unwrap();
        assert!(out.status.success(), "{}", text(&out));
        assert_eq!(
            r.ws.join("gate.log").exists(),
            kept,
            "HANDED_OFF={handed}: {:?}",
            listing(&r.ws)
        );
    }
}

/// The layout variable is run.sh's to know and its children's never: a
/// test the gate runs must not see GATE_LAYOUT and act as a gate.
#[test]
fn the_layout_is_kept_from_the_gates_children() {
    let src = fs::read_to_string(repo_root().join(RUN_SH)).unwrap();
    assert!(
        src.contains("export -n GATE_LAYOUT"),
        "GATE_LAYOUT must not be exported"
    );
    assert!(
        src.contains("judged_init() { carried && [ \"$GATE_LAYOUT\" = judged-init ]; }"),
        "the judged-init layout is the carried layout's alone"
    );
}
