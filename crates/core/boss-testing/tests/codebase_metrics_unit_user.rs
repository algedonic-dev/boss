//! The boss-codebase-metrics unit boss-gcp installs runs as root over a
//! tree root owns. On 2026-09-12 05:14Z the timer's first-ever firing,
//! as root with no `User=` over david's /opt/boss, exited 3 with
//! `fatal: detected dubious ownership in repository at '/opt/boss'`,
//! the cadence stayed silent (436a2e91), the unit read unhealthy
//! (fdd10ec8), and two landed cars waited on a packet that could not
//! exist (read off the packet the ops-runner answered, 988bb127). The
//! answer then was `User=david`, the checkout's owner. The answer since
//! boss-gcp's stage of design c98c79aa (db018094; backlog c440ab8d
//! brought this pin level, 2026-11-21) is that root reads ROOT's tree:
//! install-units.sh installs infra/gcp/root-tree-units/
//! boss-codebase-metrics.service.in, `User=root` working in
//! /var/lib/boss/gcp-tree/current, so no ownership git reads is foreign
//! and no person's account runs the chore. infra/boss-codebase-metrics.service,
//! `User=david` still, is only the fixture copy and the timer's pair
//! (no_unattended_unit_runs_from_a_home.rs, INSTALLED_FROM_ROOTS_TREE).

use boss_testing::repo_root;

/// The unit boss-gcp installs for this stem.
const INSTALLED: &str = "infra/gcp/root-tree-units/boss-codebase-metrics.service.in";

#[test]
fn the_codebase_metrics_unit_runs_as_root_over_roots_own_tree() {
    let unit = std::fs::read_to_string(repo_root().join(INSTALLED)).expect(INSTALLED);
    assert!(
        unit.lines().any(|l| l.trim() == "User=root"),
        "{INSTALLED} runs as root: root's tree is root's, so git reads no foreign ownership"
    );
    assert!(
        unit.lines()
            .any(|l| l.trim() == "WorkingDirectory=/var/lib/boss/gcp-tree/current"),
        "{INSTALLED} measures root's tree, never a checkout another account owns — \
         root over /opt/boss is 'dubious ownership' and the measurement exits 3"
    );
}

/// The value of the first `<key>="…"` assignment line in a shell script,
/// with a `${VAR:-default}` reduced to its default.
fn shell_default(script: &str, key: &str) -> String {
    let prefix = format!("{key}=\"");
    let line = script
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with(&prefix))
        .unwrap_or_else(|| panic!("no {key}=\"…\" line"));
    let value = line[prefix.len()..].trim_end_matches('"');
    match value.split_once(":-") {
        Some((_, default)) => default.trim_end_matches('}').to_string(),
        None => value.to_string(),
    }
}

/// Execute only the actual store assignment with its shared pure helper;
/// parsing shell command substitution as text does not resolve its default.
fn resolved_store(script: &str, custom: Option<&str>) -> String {
    let assignment = script
        .lines()
        .find(|line| line.starts_with("STORE="))
        .unwrap();
    let mut command = std::process::Command::new("bash");
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("HELPER", repo_root().join("infra/estate/cli-compatible.sh"))
        .args([
            "-c",
            &format!(". \"$HELPER\" || exit 1\n{assignment}\nprintf '%s' \"$STORE\""),
        ]);
    if let Some(custom) = custom {
        command.env("BOSS_CLI_STORE", custom);
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

/// THE COUNTER IS WHERE THE CHORE RUNS (backlog c4d60110). Every
/// maintenance-codebase-metrics packet from 2026-09-17 to 2026-09-27 — 11
/// of 11 — carried `registry.code_branches_on_kind: null`, "No
/// boss-leaked-policy on this machine", because nothing put the counter
/// on boss-gcp and the unit named none. The decision (f82b05a9): the
/// counter ships with what the boss-gcp converge already installs, and
/// the unit names it with BOSS_LEAKED_POLICY_BIN.
///
/// The path is a fact that lives twice — the installer's store and
/// member on one side, the unit's Environment= line on the other — so it
/// is pinned equal here (CLAUDE.md §9a): the unit must name exactly
/// `<the installer's store>/current/<the member's basename>`, the
/// converge must run that installer, and the image the installer pulls
/// must carry the member.
#[test]
fn the_codebase_metrics_unit_names_the_counter_the_converge_installs() {
    let root = repo_root();
    let read =
        |p: &str| std::fs::read_to_string(root.join(p)).unwrap_or_else(|e| panic!("{p}: {e}"));
    let unit = read(INSTALLED);
    let installer = read("infra/estate/install-cli-from-image.sh");
    let converge = read("infra/gcp/boss-gcp-converge.sh");
    let dockerfile = read("infra/oss-quickstart/Dockerfile");

    let named = unit
        .lines()
        .map(str::trim)
        .find_map(|l| l.strip_prefix("Environment=BOSS_LEAKED_POLICY_BIN="))
        .expect(
            "boss-codebase-metrics.service must name the counter with \
             Environment=BOSS_LEAKED_POLICY_BIN=… — unnamed, the chore searches \
             paths the host has never had and files null",
        );

    let store = resolved_store(&installer, None);
    assert_eq!(
        resolved_store(&installer, Some("/fixture/custom store")),
        "/fixture/custom store"
    );
    let member = shell_default(&installer, "COUNTER_MEMBER");
    let basename = member.rsplit('/').next().unwrap_or_default();
    assert_eq!(
        basename, "boss-leaked-policy",
        "the installer's COUNTER_MEMBER"
    );
    assert_eq!(
        named,
        format!("{store}/current/{basename}"),
        "the unit must name the counter where install-cli-from-image.sh puts it: \
         its store ({store}), through `current`, so every converge moves it"
    );

    assert!(
        converge.contains("infra/estate/install-cli-from-image.sh"),
        "boss-gcp-converge.sh runs the installer that puts the counter down"
    );

    // The image carries the member: every `boss-*` release binary is
    // copied to /out and /out to /usr/local/bin, and the counter is a bin
    // of a workspace crate with no required feature.
    assert!(
        dockerfile.contains("-name 'boss-*'")
            && dockerfile.contains("COPY --from=rust-build /out/ /usr/local/bin/"),
        "the image's runtime stage must still copy every boss-* release binary \
         into /usr/local/bin, or {member} is not in the image to pull"
    );
    assert!(
        root.join("crates/core/boss-testing/src/bin/boss-leaked-policy.rs")
            .is_file(),
        "the counter's bin is where cargo's --bins pass finds it"
    );
}
