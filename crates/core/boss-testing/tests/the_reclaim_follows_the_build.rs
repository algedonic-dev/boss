//! THE FORGE'S DISK RECLAIM IS THE HOST'S, AND SO IS ITS RECORD.
//!
//! HOW IT GOT HERE. Measured on the forge host, 2026-09-11 (backlog
//! `0357e0eb`): six observations fifteen minutes apart read 89, 100, 99,
//! **60**, 92, 98 GB free of 227, with the locomotive refusing to START
//! a CI run below 70 — a refusal that happens before any check runs and
//! strikes every car aboard. The fill was event-driven (every CI run
//! builds and pulls a per-train `boss-ci:<sha>` image) and the reclaim
//! timer-driven (`disk-floor-sweep.timer`, hourly), so a `reclaim` job
//! was added to the CI workflow: at the end of every run it measured the
//! disk and, below the sweep's floor, POSTed an ops-request for the
//! `reclaim-disk` verb, signed `automation:ci-disk-reclaim`. This file
//! pinned that trigger.
//!
//! WHY IT IS GONE (backlog `37742794`; David's answer to design-doc
//! `c8502e17`, question `ci-reclaim`, 2026-10-07). A CI job runs branch
//! code. The machine door's gate refuses, once a port enforces, a
//! request with no estate machine token, and decision `c395e62c`
//! (2026-10-05) keeps that token away from anything that runs a branch.
//! So the CI job's packet is DELIBERATELY REFUSED under enforce — and a
//! request whose designed answer is a refusal is a line in every
//! would-refuse window that can never be cleared. The job and its script
//! are deleted. "The reclaim itself still runs; the record of it moves
//! to the forge host disk-floor sweep, which already watches that disk."
//!
//! What is pinned here now:
//!  - no workflow sends anything to the system of record: no address, no
//!    identity header, no jobs-API path, and no trigger script to call;
//!  - the reclaim still runs without it — the hourly timer is there, and
//!    its unit defends a floor ABOVE the one the locomotive refuses at
//!    (the two floors moved to 100 and 40 on 2026-09-16; 30 GB apart
//!    when the trigger was built, 60 now);
//!  - the sweep records what it did where its own packet can carry it:
//!    the floor, free space before and after, what the per-train image
//!    pass freed and how the run ended — on a run that had nothing to
//!    do, on one that ended FLOOR UNMET, and never when nothing asked.
//!
//! WHAT IS NOT CLAIMED. The reclaim no longer follows the build within a
//! minute; it follows the clock. Whether an hour is still short enough
//! is a number to watch on the sweep's packets (`free_gb_before`), and
//! the repair if it is not is the timer's cadence — never a write from a
//! CI job.
//!
//! tree-wide pin — it reads the CI workflows and the forge's units and
//! runs the sweep, none of which a changed-file map attributes to this
//! crate, so every scoped gate runs it (`tree_wide_pins` in
//! infra/gate.sh).

use boss_testing::{repo_root, write_exec};
use std::path::PathBuf;
use std::process::{Command, Output};

fn read(rel: &str) -> String {
    let path = repo_root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

const SWEEP: &str = "infra/forge/disk-floor-sweep.sh";
const SWEEP_UNIT: &str = "infra/forge/disk-floor-sweep.service";
const SWEEP_TIMER: &str = "infra/forge/disk-floor-sweep.timer";
const WORKFLOWS: &str = ".forgejo/workflows";
const ESTATE: &str = "infra/estate/estate.toml";
const GONE: &[&str] = &[
    "infra/forge/request-reclaim-disk.sh",
    "infra/platform/automations/ci-disk-reclaim.toml",
];
const SUMMARY_VAR: &str = "BOSS_RUN_SUMMARY_FILE";

// ---------------------------------------------------------------------
// No CI job writes to the system of record
// ---------------------------------------------------------------------

/// The host of an estate.toml URL value: `sor_url = "http://H:P"` -> H.
fn estate_host(key: &str) -> String {
    let estate = read(ESTATE);
    let url = estate
        .lines()
        .find_map(|l| l.strip_prefix(&format!("{key} = \"")))
        .and_then(|l| l.strip_suffix('"'))
        .unwrap_or_else(|| panic!("{ESTATE} names no {key}"));
    let host = url.split("://").nth(1).unwrap_or(url);
    host.split([':', '/']).next().unwrap_or(host).to_string()
}

/// What a workflow line would have to hold to reach the system of
/// record: its address (either spelling), the variable every sender
/// reads it from, the identity header, or a jobs-API path.
fn record_words() -> Vec<String> {
    vec![
        estate_host("sor_url"),
        estate_host("sor_cluster_url"),
        "BOSS_JOBS_URL".to_string(),
        "x-boss-user".to_string(),
        "/api/jobs".to_string(),
        "boss-maintenance-wrap".to_string(),
        "boss-step.sh".to_string(),
    ]
}

/// `(file, line number, line)` for every live workflow line naming one.
fn workflow_lines_that_reach_the_record(workflows: &[(String, String)]) -> Vec<String> {
    let words = record_words();
    let mut out = Vec::new();
    for (name, text) in workflows {
        for (n, line) in text.lines().enumerate() {
            if line.trim_start().starts_with('#') {
                continue;
            }
            let lower = line.to_ascii_lowercase();
            if let Some(w) = words
                .iter()
                .find(|w| lower.contains(&w.to_ascii_lowercase()))
            {
                out.push(format!("{name}:{}: names `{w}`: {}", n + 1, line.trim()));
            }
        }
    }
    out
}

fn workflows() -> Vec<(String, String)> {
    let dir = repo_root().join(WORKFLOWS);
    let mut out: Vec<(String, String)> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{WORKFLOWS}: {e}"))
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "yml" || x == "yaml"))
        .map(|p| {
            (
                p.file_name().unwrap().to_string_lossy().into_owned(),
                std::fs::read_to_string(&p).expect("a workflow"),
            )
        })
        .collect();
    out.sort();
    assert!(
        out.iter().any(|(n, _)| n == "ci.yml"),
        "the scan found no ci.yml under {WORKFLOWS} — it has stopped seeing the workflows"
    );
    out
}

#[test]
fn no_ci_job_sends_anything_to_the_system_of_record() {
    let found = workflow_lines_that_reach_the_record(&workflows());
    assert!(
        found.is_empty(),
        "a CI job runs branch code, so it may not hold the estate machine token (decision \
         c395e62c), and without one a request to the system of record is one every machine gate \
         refuses under enforce (David, design-doc c8502e17 `ci-reclaim`). These workflow lines \
         reach for it:\n  {}",
        found.join("\n  ")
    );
    for gone in GONE {
        assert!(
            !repo_root().join(gone).exists(),
            "{gone} is back — the CI job's trigger and the identity it signed as were deleted \
             with the job; the reclaim is the host sweep's, on its timer"
        );
    }
    assert!(
        !read(&format!("{WORKFLOWS}/ci.yml")).contains("\n  reclaim:"),
        "ci.yml has a `reclaim` job again"
    );
}

/// The scan has teeth: the job as it stood on 2026-10-07 is refused by
/// each of the lines that made it a sender.
#[test]
fn the_deleted_job_would_be_refused() {
    let sor = estate_host("sor_url");
    let old = format!(
        "  reclaim:\n    runs-on: docker\n    env:\n      BOSS_JOBS_URL: http://{sor}:7900\n    steps:\n      - name: ask\n        run: |\n          # curl -H 'x-boss-user: prose' is not a sender\n          curl -X POST \"$BASE/api/jobs\" -H \"X-Boss-User: $U\"\n"
    );
    let found = workflow_lines_that_reach_the_record(&[("ci.yml".to_string(), old)]);
    assert_eq!(found.len(), 2, "{found:#?}");
    assert!(
        found[0].contains("ci.yml:4:") && found[1].contains("ci.yml:9:"),
        "{found:#?}"
    );
}

// ---------------------------------------------------------------------
// The reclaim still runs: the timer, and a floor above the locomotive's
// ---------------------------------------------------------------------

fn defended_floor() -> u64 {
    read(SWEEP_UNIT)
        .lines()
        .find_map(|l| l.trim().strip_prefix("Environment=BOSS_DISK_FLOOR_GB="))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or_else(|| panic!("{SWEEP_UNIT} no longer pins Environment=BOSS_DISK_FLOOR_GB"))
}

#[test]
fn the_host_timer_reclaims_with_no_trigger_and_defends_more_than_ci_needs() {
    let timer = read(SWEEP_TIMER);
    assert!(
        timer
            .lines()
            .any(|l| l.trim().starts_with("OnUnitActiveSec=")),
        "{SWEEP_TIMER} declares no cadence — with the CI trigger gone the timer is the only \
         thing that runs the reclaim"
    );
    // The sweep's own bare default IS the floor CI refuses under — its
    // header holds the two to one number (§9a, "MUST match the
    // locomotive's") — so the unit's floor is read against that, here,
    // without a second reader of the locomotive's script.
    let refuses_below: u64 = read(SWEEP)
        .lines()
        .find_map(|l| l.split("${BOSS_DISK_FLOOR_GB:-").nth(1))
        .and_then(|rest| rest.split('}').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("{SWEEP} no longer reads BOSS_DISK_FLOOR_GB with a default"));
    let floor = defended_floor();
    assert!(
        floor > refuses_below,
        "the unit defends {floor} GB and CI refuses below {refuses_below}: a floor only buys \
         headroom if it is ABOVE the one being defended, and the margin between them is all \
         that covers a build between two ticks now"
    );
}

// ---------------------------------------------------------------------
// The record: the sweep leaves what it did for its own packet
// ---------------------------------------------------------------------

struct Run {
    out: Output,
    summary: PathBuf,
}

/// The sweep against stubs (the idiom of
/// the_floor_sweep_keeps_the_compilers_cache.rs): `df` reports
/// `free_gb`, both docker daemons answer and free nothing, the record
/// is not asked.
fn sweep(case: &str, free_gb: u64, with_summary: bool) -> Run {
    let dir = boss_testing::scratch_dir(&format!("reclaim-record-{case}"));
    let bin = dir.join("bin");
    boss_testing::create_dir(&bin);
    write_exec(
        &bin.join("df"),
        &format!(
            "#!/usr/bin/env bash\n\
             echo 'Filesystem 1024-blocks Used Available Capacity Mounted on'\n\
             echo \"/dev/fake 1 1 $(({free_gb} * 1024 * 1024)) 50% /\"\n"
        ),
    );
    write_exec(
        &bin.join("docker"),
        "#!/usr/bin/env bash\n\
         case \"$1 $2\" in\n\
         'system df') echo 'TYPE TOTAL ACTIVE SIZE RECLAIMABLE'; echo 'Build Cache 40 12 7.2GB 6.1GB (84%)' ;;\n\
         *) echo 'Total reclaimed space: 0B' ;;\n\
         esac\n\
         exit 0\n",
    );
    write_exec(
        &bin.join("system-docker"),
        "#!/usr/bin/env bash\ncase \"$1\" in info) echo /var/lib/docker;; images) :;; esac\nexit 0\n",
    );
    write_exec(
        &bin.join("curl"),
        "#!/usr/bin/env bash\necho '{}'\nexit 0\n",
    );
    let sor_env = dir.join("sor.env");
    let rendered = Command::new("bash")
        .arg(repo_root().join("infra/estate/render-sor-env.sh"))
        .arg("--to")
        .arg(&sor_env)
        .output()
        .expect("render sor.env");
    assert!(
        rendered.status.success(),
        "{}",
        String::from_utf8_lossy(&rendered.stderr)
    );
    let summary = dir.join("summary.json");
    let _ = std::fs::remove_file(&summary);
    let mut cmd = Command::new("bash");
    cmd.arg(repo_root().join(SWEEP))
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("BOSS_SOR_ENV", &sor_env)
        .env("BOSS_CI_IMAGE_DOCKER", bin.join("system-docker"))
        .env("BOSS_CI_IMAGE_DAEMON_ROOT", "/var/lib/docker")
        .env("BOSS_SWEEP_CURL_CMD", bin.join("curl"))
        .env("BOSS_DISK_FLOOR_GB", "100")
        // Its own layout: no system of record, no token, and the summary
        // only where this test says.
        .env_remove("BOSS_JOBS_URL")
        .env_remove(SUMMARY_VAR)
        .env("BOSS_MACHINE_TOKEN_DIR", dir.join("no-machine-token"))
        .current_dir(repo_root());
    if with_summary {
        cmd.env(SUMMARY_VAR, &summary);
    }
    Run {
        out: cmd.output().expect("run the sweep"),
        summary,
    }
}

fn say(r: &Run) -> String {
    format!(
        "exit {:?}\nstdout: {}\nstderr: {}",
        r.out.status.code(),
        String::from_utf8_lossy(&r.out.stdout),
        String::from_utf8_lossy(&r.out.stderr)
    )
}

fn summary(r: &Run) -> serde_json::Value {
    let text = std::fs::read_to_string(&r.summary).unwrap_or_else(|e| {
        panic!(
            "the sweep left no summary at {} ({e}) — its packet would say `result=ok` and \
             nothing about what the reclaim did, and with the CI job's request gone that was \
             the only record there was\n{}",
            r.summary.display(),
            say(r)
        )
    });
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("the summary is not JSON ({e}): {text}"))
}

#[test]
fn a_sweep_with_nothing_to_do_records_the_disk_it_found() {
    let r = sweep("above", 180, true);
    assert!(r.out.status.success(), "{}", say(&r));
    let s = summary(&r);
    assert_eq!(s["floor_gb"], "100", "{s}");
    assert_eq!(s["free_gb_before"], "180", "{s}");
    assert_eq!(s["free_gb_after"], "180", "{s}");
    assert_eq!(s["freed_mib"], "0", "{s}");
    assert_eq!(s["ci_image_prune_freed_mib"], "0", "{s}");
    assert!(
        s["sweep_outcome"]
            .as_str()
            .is_some_and(|o| o.contains("nothing more to do")),
        "{s}"
    );
}

#[test]
fn a_sweep_that_ends_floor_unmet_records_that_and_still_fails() {
    let r = sweep("unmet", 30, true);
    assert_eq!(
        r.out.status.code(),
        Some(1),
        "the record must not change how the sweep ends: FLOOR UNMET is a failed unit\n{}",
        say(&r)
    );
    let s = summary(&r);
    assert_eq!(s["free_gb_before"], "30", "{s}");
    assert!(
        s["sweep_outcome"]
            .as_str()
            .is_some_and(|o| o.contains("FLOOR UNMET")),
        "{s}"
    );
}

#[test]
fn a_sweep_nobody_asked_for_a_summary_writes_none_and_ends_the_same() {
    for (case, free, code) in [("quiet-above", 180, Some(0)), ("quiet-unmet", 30, Some(1))] {
        let with = sweep(&format!("{case}-with"), free, true);
        let without = sweep(case, free, false);
        assert_eq!(without.out.status.code(), code, "{}", say(&without));
        assert_eq!(with.out.status.code(), code, "{}", say(&with));
        assert!(
            !without.summary.exists(),
            "the reclaim-disk verb and a hand run declare no summary file, and get none"
        );
        assert_eq!(
            String::from_utf8_lossy(&with.out.stdout),
            String::from_utf8_lossy(&without.out.stdout),
            "the summary is a record, not a behaviour: the sweep says and does the same"
        );
    }
}

#[test]
fn the_unit_declares_the_summary_once_and_its_step_carries_it() {
    let unit = read(SWEEP_UNIT);
    let live: Vec<&str> = unit
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect();
    let declared: Vec<&&str> = live
        .iter()
        .filter(|l| l.starts_with(&format!("Environment={SUMMARY_VAR}=")))
        .collect();
    assert_eq!(
        declared.len(),
        1,
        "{SWEEP_UNIT} declares {SUMMARY_VAR} exactly once, in Environment=, so ExecStart (the \
         producer) and ExecStopPost (the consumer) inherit one path: {declared:?}"
    );
    let path = declared[0].rsplit('=').next().unwrap_or("");
    assert!(
        path.starts_with('/') && !path.starts_with("/tmp/") && !path.starts_with("/var/tmp/"),
        "the summary file ({path}) is not in a shared temp directory, where another account \
         on the host could plant what the step then records"
    );
    assert!(
        live.iter().any(|l| l.starts_with("ExecStopPost=")
            && l.contains("boss-step.sh maintenance-disk-floor-sweep run")),
        "{SWEEP_UNIT}: boss-step.sh closes the run step from ExecStopPost, which is what reads \
         and deletes the summary"
    );
    assert!(
        read("infra/boss-step.sh").contains(SUMMARY_VAR),
        "infra/boss-step.sh no longer merges {SUMMARY_VAR} onto the step"
    );
}
