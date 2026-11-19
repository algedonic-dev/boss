//! A seed cache root the runner refused is on the gate's receipt, not
//! only on its pod log.
//!
//! THE DEFECT (backlog cb78e618, found building 822223ec). Since
//! 822223ec, run.sh replaces anything but a real directory at a seed
//! cache root — a link or a file a gated branch planted on the shared
//! `/gate-seed` — and says so in ONE place: a `gate-runner: REFUSED
//! <path>: <what it was>, …, changed <ctime>` line on the pod log, which
//! dies with the Job (~24 h). The receipt said nothing, so a gate that
//! found and replaced a planted root read exactly like one that found
//! nothing, and the one fact that should reach a human — some branch
//! planted something on the seed — reached nobody. A troubled packet
//! must look troubled.
//!
//! THE FIX. The download caches block keeps each refusal (the path,
//! what was found there, and its ctime — which places the gate that
//! planted it) and the receipt summary carries them as `seed_refusals`:
//! one entry per refusal, an EMPTY list when the roots were checked and
//! all were real, and NO key when the check never ran — so "checked,
//! clean" and "never checked" stay two different records.
//!
//! A RUN THAT DIES KEEPS IT TOO (review f545c047 of car 041ba501, F1).
//! A planted root the runner cannot REPLACE ends the run in `fail_lost`,
//! and the early receipt that writes used to know nothing of the list:
//! the refusal was kept, then dropped, and the `lost` read as "never
//! reached the check" when a planted root is what killed it. The early
//! receipt now carries the list once the check finished or found one,
//! and the error names the root.
//!
//! These pins lift the blocks of run.sh as they ship and run them back
//! to back, as run.sh does.

use boss_testing::{create_dir, repo_root, scratch_dir, write_exec, write_file};
use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::Command;

const RUN_SH: &str = "infra/gate-runner/run.sh";

fn run_sh() -> String {
    fs::read_to_string(repo_root().join(RUN_SH)).expect("read run.sh")
}

/// The text between two markers of run.sh, exactly as it ships.
fn block(begin: &str, end: &str) -> String {
    let src = run_sh();
    let a = src
        .find(begin)
        .unwrap_or_else(|| panic!("{RUN_SH} has no `{begin}` marker"));
    let b = src
        .find(end)
        .unwrap_or_else(|| panic!("{RUN_SH} has no `{end}` marker"));
    assert!(a < b, "`{begin}` must precede `{end}`");
    src[a..b].to_string()
}

/// Run the download caches block with `$SEED` at `seed` (when `caches`
/// is set), then the receipt summary over a receipt holding
/// `receipt_body`; returns the summary as JSON.
fn receipt_after(dir: &Path, seed: &Path, caches: bool, receipt_body: &str) -> serde_json::Value {
    let receipt = dir.join("receipt.json");
    write_file(&receipt, receipt_body);
    let downloads = if caches {
        block(
            "# --- download caches (begin) ---",
            "# --- download caches (end) ---",
        )
    } else {
        String::new()
    };
    let script = format!(
        "set -euo pipefail\nSEED=\"$1\"\n{downloads}\nRECEIPT=\"$2\"\nHEAD_SHA=deadbeef\n{summary}\nprintf 'SUMMARY=%s' \"$SUMMARY\"\n",
        summary = block(
            "# --- receipt summary (begin) ---",
            "# --- receipt summary (end) ---"
        ),
    );
    let out = Command::new("bash")
        .args(["-c", &script, "seed-refusals"])
        .arg(seed)
        .arg(&receipt)
        .current_dir(dir)
        .env_remove("CARGO_HOME")
        .env_remove("BUN_INSTALL_CACHE_DIR")
        .output()
        .expect("bash");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success(),
        "the blocks failed: {stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let (_, text) = stdout
        .rsplit_once("SUMMARY=")
        .unwrap_or_else(|| panic!("no SUMMARY: {stdout}"));
    serde_json::from_str(text).unwrap_or_else(|e| panic!("not JSON ({e}): {text}"))
}

/// `YYYY-MM-DDTHH:MM:SSZ`, the shape of the receipt's other instants.
fn is_utc_instant(s: &str) -> bool {
    let shape = b"dddd-dd-ddTdd:dd:ddZ";
    s.len() == shape.len()
        && s.bytes().zip(shape.iter()).all(|(c, &want)| match want {
            b'd' => c.is_ascii_digit(),
            _ => c == want,
        })
}

fn refusals(got: &serde_json::Value) -> &Vec<serde_json::Value> {
    got["seed_refusals"]
        .as_array()
        .unwrap_or_else(|| panic!("the receipt carries no `seed_refusals` list: {got}"))
}

/// A PLANTED LINK AND A PLANTED FILE ARE BOTH ON THE RECEIPT, each with
/// its path, what it was, and when it was planted — on a readable
/// receipt and on one gate.sh never wrote, since a run that died is
/// exactly the one a reader suspects of something.
#[test]
fn each_refused_root_is_on_the_receipt() {
    for (name, body) in [
        ("readable", r#"{"verdict":"green","head":"c0ffee"}"#),
        ("unreadable", "not json"),
    ] {
        let dir = scratch_dir(&format!("seed-refusals-{name}"));
        let seed = dir.join("seed");
        create_dir(&seed.join("bun-install-cache"));
        write_file(&seed.join("cargo"), "planted\n");
        symlink(dir.join("decoy"), seed.join("bun-install-cache/.tmp")).expect("plant .tmp");

        let got = receipt_after(&dir, &seed, true, body);
        let _ = fs::remove_dir_all(&dir);

        let list = refusals(&got);
        assert_eq!(list.len(), 2, "one entry per refused root: {got}");
        let cargo = &list[0];
        assert_eq!(cargo["path"], format!("{}/cargo", seed.display()), "{got}");
        assert_eq!(cargo["found"], "a regular file", "{got}");
        let tmp = &list[1];
        assert_eq!(
            tmp["path"],
            format!("{}/bun-install-cache/.tmp", seed.display()),
            "{got}"
        );
        assert_eq!(
            tmp["found"],
            format!("a symbolic link to '{}'", dir.join("decoy").display()),
            "{got}"
        );
        for entry in list {
            assert!(
                entry["changed_at"].as_str().is_some_and(is_utc_instant),
                "the ctime is what places the gate that planted it, as the \
                 UTC instant every other receipt time is (review f545c047, F3): {got}"
            );
        }
    }
}

/// CHECKED AND CLEAN IS AN EMPTY LIST; NEVER CHECKED IS NO KEY. A
/// reader must be able to tell a runner that looked and found nothing
/// from one that never looked (an old runner, or one that died before
/// the caches) — no evidence is not a pass.
#[test]
fn clean_is_an_empty_list_and_unchecked_is_no_key() {
    let dir = scratch_dir("seed-refusals-clean");
    let seed = dir.join("seed");
    create_dir(&seed);
    let clean = receipt_after(&dir, &seed, true, r#"{"verdict":"green"}"#);
    let unchecked = receipt_after(&dir, &seed, false, r#"{"verdict":"green"}"#);
    let _ = fs::remove_dir_all(&dir);

    assert!(refusals(&clean).is_empty(), "{clean}");
    assert!(unchecked.get("seed_refusals").is_none(), "{unchecked}");
}

/// THE PLANTED TEXT IS DATA. What a link names is chosen by the branch
/// that planted it, so a target carrying a newline, a tab and a quote
/// must arrive as ONE entry, whole, in valid JSON — never split into a
/// second, forged refusal or a broken receipt.
#[test]
fn a_hostile_link_target_is_one_entry_whole() {
    let dir = scratch_dir("seed-refusals-hostile");
    let seed = dir.join("seed");
    create_dir(&seed);
    // Ends in newlines too: a command substitution strips those, so the
    // text was not whole until the runner kept a sentinel past them
    // (review f545c047, F2).
    let target = "x\ny\t\"z\" cargo /gate-seed/forged\n\n";
    symlink(target, seed.join("cargo")).expect("plant cargo link");

    let got = receipt_after(&dir, &seed, true, r#"{"verdict":"green"}"#);
    let _ = fs::remove_dir_all(&dir);

    let list = refusals(&got);
    assert_eq!(list.len(), 1, "{got}");
    assert_eq!(
        list[0]["found"],
        format!("a symbolic link to '{target}'"),
        "{got}"
    );
    assert_eq!(
        list[0]["path"],
        format!("{}/cargo", seed.display()),
        "{got}"
    );
}

/// Native filenames can contain bytes that are not UTF-8. The actual
/// summary must remain readable by the Rust receipt consumer.
#[test]
fn a_non_utf8_link_target_keeps_the_receipt_readable() {
    use std::os::unix::ffi::OsStrExt;

    let dir = scratch_dir("seed-refusals-non-utf8");
    let seed = dir.join("seed");
    create_dir(&seed);
    symlink(
        std::ffi::OsStr::from_bytes(b"planted\x80target"),
        seed.join("cargo"),
    )
    .expect("plant isolated non-UTF-8 link");

    let got = receipt_after(&dir, &seed, true, r#"{"verdict":"green","head":"c0ffee"}"#);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(got["verdict"], "green");
    let list = refusals(&got);
    assert_eq!(list.len(), 1, "{got}");
    assert_eq!(list[0]["path"], format!("{}/cargo", seed.display()));
    assert_eq!(
        list[0]["found"],
        "a symbolic link to 'planted\u{fffd}target'"
    );
    assert!(
        is_utc_instant(list[0]["changed_at"].as_str().unwrap()),
        "{got}"
    );
}

/// The one line of run.sh that starts with `prefix`, as it ships.
fn line(prefix: &str) -> String {
    run_sh()
        .lines()
        .find(|l| l.starts_with(prefix))
        .unwrap_or_else(|| panic!("{RUN_SH} has no line starting `{prefix}`"))
        .to_string()
}

/// A run that DIES: the verdict carrier, `fail_lost` and its ERR trap as
/// run.sh ships them, then (when `caches`) the download caches block,
/// then `after`. With `rm_refused`, the `rm` on PATH is refused the way a
/// branch makes it at the gate uid — a planted link under a seed
/// directory it owns and chmods 0555 (review f545c047's probe); as a stub
/// so the pin holds as root too, where a mode bit stops nothing. Returns
/// the receipt the dying runner left on its log, and that log.
fn lost_receipt(
    dir: &Path,
    seed: &Path,
    caches: bool,
    rm_refused: bool,
    after: &str,
) -> (serde_json::Value, String) {
    let bin = dir.join("bin");
    create_dir(&bin);
    if rm_refused {
        write_exec(
            &bin.join("rm"),
            "#!/bin/sh\necho \"rm: cannot remove '$3': Permission denied\" >&2\nexit 1\n",
        );
    }
    let downloads = if caches {
        block(
            "# --- download caches (begin) ---",
            "# --- download caches (end) ---",
        )
    } else {
        String::new()
    };
    let script = format!(
        "set -euo pipefail\nSEED=\"$1\"\n{carrier}\nretain_runtime_raw() {{ :; }}\n{fail_lost}\n{trap}\n{downloads}\n{after}\necho 'gate-runner: the runner lived'\n",
        carrier = block(
            "# --- verdict carrier (begin) ---",
            "# --- verdict carrier (end) ---"
        ),
        fail_lost = line("fail_lost()"),
        trap = line("trap 'fail_lost"),
    );
    let out = Command::new("bash")
        .args(["-c", &script, "seed-refusals"])
        .arg(seed)
        .current_dir(dir)
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("GATE_VERDICT_CARRIER", "pod-log")
        .env("GATE_TERMINATION_LOG", dir.join("termination-log"))
        .env("NATIVE_JOB_NAME", "gate-fix-x-abc12")
        .env_remove("CARGO_HOME")
        .env_remove("BUN_INSTALL_CACHE_DIR")
        .env_remove("SEED_ROOTS_CHECKED")
        .output()
        .expect("bash");
    let log = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success() && !log.contains("the runner lived"),
        "the run was meant to die: {log}"
    );
    let receipt = log
        .lines()
        .find_map(|l| l.strip_prefix("gate-runner: receipt "))
        .unwrap_or_else(|| panic!("the dying runner left no receipt: {log}"));
    let got = serde_json::from_str(receipt).unwrap_or_else(|e| panic!("not JSON ({e}): {log}"));
    (got, log)
}

/// A PLANTED ROOT THE RUNNER CANNOT REPLACE IS ON THE RECEIPT IT DIES
/// WITH, and the receipt names it as the cause (review f545c047, F1).
/// The refusal was kept and then dropped: the rm failed, the ERR trap's
/// early receipt knew nothing of SEED_REFUSALS, and a `lost` with no
/// `seed_refusals` key read as "never reached the check" when a planted
/// root is what killed the run — and kills every gate after it, since
/// the root is never replaced.
#[test]
fn a_root_the_runner_cannot_replace_is_on_its_lost_receipt() {
    let dir = scratch_dir("seed-refusals-unremovable");
    let seed = dir.join("seed");
    create_dir(&seed.join("cargo"));
    create_dir(&seed.join("bun-install-cache"));
    symlink(dir.join("decoy"), seed.join("bun-install-cache/.tmp")).expect("plant .tmp");

    let (got, log) = lost_receipt(&dir, &seed, true, true, "");
    let _ = fs::remove_dir_all(&dir);

    let tmp = format!("{}/bun-install-cache/.tmp", seed.display());
    assert_eq!(got["verdict"], "lost", "{log}");
    let list = refusals(&got);
    assert_eq!(list.len(), 1, "{log}");
    assert_eq!(list[0]["path"], tmp, "{log}");
    assert_eq!(
        list[0]["found"],
        format!("a symbolic link to '{}'", dir.join("decoy").display()),
        "{log}"
    );
    assert!(
        got["error"].as_str().is_some_and(|e| e.contains(&tmp)),
        "a verdict must name what failed — the root it could not replace, not a line number: {got}"
    );
}

/// A RUN THAT DIES AFTER THE CHECK KEEPS WHAT THE CHECK FOUND: the roots
/// it replaced, or an empty list when all were real. Anything between
/// the check and the checks' own receipt can die into the ERR trap, and
/// the trap's receipt is the only one that run leaves.
#[test]
fn a_run_that_dies_after_the_check_keeps_what_it_found() {
    let dir = scratch_dir("seed-refusals-dies-after");
    let clean = dir.join("clean");
    create_dir(&clean);
    let planted = dir.join("planted");
    create_dir(&planted);
    write_file(&planted.join("cargo"), "planted\n");

    let (clean_got, clean_log) = lost_receipt(&dir, &clean, true, false, "false");
    let (planted_got, planted_log) = lost_receipt(&dir, &planted, true, false, "false");
    let _ = fs::remove_dir_all(&dir);

    assert_eq!(clean_got["verdict"], "lost", "{clean_log}");
    assert!(refusals(&clean_got).is_empty(), "{clean_log}");
    let list = refusals(&planted_got);
    assert_eq!(list.len(), 1, "{planted_log}");
    assert_eq!(
        list[0]["path"],
        format!("{}/cargo", planted.display()),
        "{planted_log}"
    );
    assert_eq!(list[0]["found"], "a regular file", "{planted_log}");
}

/// A RUN THAT DIES BEFORE THE CHECK STILL CARRIES NO KEY: the early
/// receipt adds the list only once there is something the check said.
#[test]
fn a_run_that_dies_before_the_check_carries_no_key() {
    let dir = scratch_dir("seed-refusals-dies-before");
    let seed = dir.join("seed");
    create_dir(&seed);
    let (got, log) = lost_receipt(&dir, &seed, false, false, "false");
    let _ = fs::remove_dir_all(&dir);

    assert_eq!(got["verdict"], "lost", "{log}");
    assert!(got.get("seed_refusals").is_none(), "{log}");
}
