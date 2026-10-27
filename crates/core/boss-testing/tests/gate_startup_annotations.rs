//! Startup refusals must reach public Actions annotations before any check runs.
use std::process::Command;

#[test]
fn low_startup_floor_is_publicly_annotated() {
    startup_case("low");
}
#[test]
fn unmeasured_startup_floor_is_publicly_annotated() {
    startup_case("unmeasured");
}
#[test]
fn sufficient_startup_floor_is_not_refused() {
    startup_case("sufficient");
}

fn startup_case(selected: &str) {
    let dir = boss_testing::scratch_dir("gate-startup-annotations");
    let source = std::fs::read_to_string(boss_testing::repo_root().join("infra/gate.sh")).unwrap();
    let end = source.find("\nrequire_headroom \"to start\"\n").unwrap()
        + "\nrequire_headroom \"to start\"\n".len();
    // Run the real startup, stopping at its boundary rather than launching suites.
    let tree = dir.join("tree");
    boss_testing::copy_gate_sh(&tree);
    let startup = tree.join("infra/gate.sh");
    boss_testing::write_file(
        &startup,
        &format!("{}\nprintf 'STARTUP PASSED\\n'\n", &source[..end]),
    );
    for (name, available, omit, expected) in [
        ("low", "1048576", false, Some("1GB free, need 12GB")),
        (
            "unmeasured",
            "943718400",
            true,
            Some("could not read free space"),
        ),
        ("sufficient", "943718400", false, None),
    ] {
        if name != selected {
            continue;
        }
        let df = dir.join(format!("df-{name}"));
        boss_testing::write_exec(
            &df,
            &format!(
                "#!/usr/bin/env bash\necho 'Filesystem 1024-blocks Used Available Capacity Mounted on'\nfor path in \"$@\"; do\n{}\necho '/dev/test 1 1 {} 1% /test'\ndone\n",
                if omit {
                    "echo 'df: denied % path' >&2; exit 1"
                } else {
                    ":"
                },
                available
            ),
        );
        for actions in ["true", "false"] {
            let out = Command::new("bash")
                .arg(&startup)
                .arg("--auto")
                .current_dir(&tree)
                .env("BOSS_GATE_DF_CMD", &df)
                .env("BOSS_GATE_MIN_FREE_GB", "12")
                .env("GITHUB_ACTIONS", actions)
                .output()
                .unwrap();
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            assert_eq!(
                out.status.code(),
                Some(if expected.is_some() { 2 } else { 0 }),
                "{name}: {text}"
            );
            assert_eq!(
                text.contains("STARTUP PASSED"),
                expected.is_none(),
                "{name}: {text}"
            );
            assert!(!text.contains("GATE FAIL"), "{name}: {text}");
            if let Some(why) = expected {
                assert!(text.contains(why), "{name}: {text}");
                assert_eq!(
                    text.matches("::error title=GATE REFUSED::").count(),
                    usize::from(actions == "true"),
                    "{name}: {text}"
                );
            } else {
                assert!(!text.contains("::error"), "{name}: {text}");
            }
        }
    }
}
