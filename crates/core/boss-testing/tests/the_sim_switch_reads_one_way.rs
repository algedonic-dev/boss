//! The sim switch reads one way in both languages (backlog d65bd066,
//! 2026-09-27).
//!
//! `BOSS_SIM_ENABLED` is one deployment fact with two readers: the
//! launcher's `sim_enabled` (infra/oss-quickstart/tenant-launch.sh),
//! which decides whether the brewery tick daemon starts, and
//! `boss_policy_client::sim_enabled_value`, which decides whether every
//! Rust service honours the sim bypass. Until this car they disagreed:
//! the shell read UNSET, and any value that was not false-like (`yes`,
//! `on`, a typo), as ON, while Rust accepted only `1`/`true` and read
//! unset as OFF — so `BOSS_SIM_ENABLED=yes` started a tick daemon that
//! every service then refused, failing closed with the sim silently
//! broken. CLAUDE.md §9a: a fact that lives twice gets an equality test.
//!
//! The rule both now follow: ON is `1` or `true` (ASCII case-insensitive,
//! surrounding whitespace ignored); everything else, and UNSET, is OFF.
//! Off is the safe default because a simulator writing into a real
//! instance is the worse accident — an instance asks for a sim, it never
//! gets one by saying nothing.
//!
//! One table, both readers: each row runs the shell function with the
//! value in its environment (or with the variable removed) and calls the
//! Rust parser on the same value, and all three — shell, Rust, the row's
//! expected answer — must agree.

use boss_testing::repo_root;
use std::process::Command;

const LAUNCH_LIB: &str = "infra/oss-quickstart/tenant-launch.sh";

/// `(value, on?)` — `None` is the variable absent from the environment.
const TABLE: &[(Option<&str>, bool)] = &[
    (None, false),
    (Some(""), false),
    (Some(" "), false),
    (Some("true"), true),
    (Some("TRUE"), true),
    (Some("True"), true),
    (Some(" true "), true),
    (Some("\ttrue\n"), true),
    (Some("1"), true),
    (Some(" 1 "), true),
    (Some("false"), false),
    (Some("FALSE"), false),
    (Some("0"), false),
    (Some("no"), false),
    (Some("off"), false),
    // From here down, with the unset and blank rows above: values the
    // old shell read as ON and Rust as OFF — the defect.
    (Some("yes"), false),
    (Some("on"), false),
    (Some("ture"), false),
    (Some("01"), false),
    (Some("2"), false),
    (Some("truee"), false),
    (Some("t"), false),
];

/// The shell reader's answer for `value`: source the launcher library
/// and run its `sim_enabled` in a clean child.
fn shell_says(value: Option<&str>) -> bool {
    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg(r#". "$1" && sim_enabled"#)
        .arg("sim-switch")
        .arg(repo_root().join(LAUNCH_LIB))
        .env_remove("BOSS_SIM_ENABLED");
    if let Some(v) = value {
        cmd.env("BOSS_SIM_ENABLED", v);
    }
    let out = cmd.output().expect("run bash");
    match out.status.code() {
        Some(0) => true,
        Some(1) => false,
        other => panic!(
            "sim_enabled exited {other:?} for {value:?} — neither on (0) nor off (1):\n{}",
            String::from_utf8_lossy(&out.stderr)
        ),
    }
}

#[test]
fn the_launcher_and_the_services_read_the_sim_switch_the_same_way() {
    let disagreements: Vec<String> = TABLE
        .iter()
        .filter_map(|&(value, want)| {
            let shell = shell_says(value);
            let rust = boss_policy_client::sim_enabled_value(value);
            (shell != want || rust != want).then(|| {
                format!("{value:?}: expected {want}, shell sim_enabled says {shell}, Rust sim_enabled_value says {rust}")
            })
        })
        .collect();
    assert!(
        disagreements.is_empty(),
        "BOSS_SIM_ENABLED is read differently by {LAUNCH_LIB} and boss-policy-client — \
         the tick daemon and the services' sim bypass must agree on every value \
         (backlog d65bd066):\n  {}",
        disagreements.join("\n  ")
    );
}
