//! boss-codebase-metrics.service runs as the owner of the checkout it
//! reads. With no `User=` the unit ran as root, and root reading a
//! checkout owned by another user is what git calls "dubious ownership":
//! on 2026-09-12 05:14Z the timer's first-ever firing exited 3 with
//! `fatal: detected dubious ownership in repository at '/opt/boss'`,
//! the cadence stayed silent (436a2e91), the unit read unhealthy
//! (fdd10ec8), and two landed cars waited on a packet that could not
//! exist. Read off the packet the ops-runner answered (988bb127), not
//! an ssh session. The siblings that run git against /opt/boss
//! (boss-search-reindex, boss-forge-token-audit) already say User=david;
//! this pins that this one does too, and says why in the unit.

use boss_testing::repo_root;

#[test]
fn the_codebase_metrics_unit_runs_as_the_checkout_owner() {
    let unit = std::fs::read_to_string(repo_root().join("infra/boss-codebase-metrics.service"))
        .expect("infra/boss-codebase-metrics.service");
    assert!(
        unit.lines().any(|l| l.trim() == "User=david"),
        "boss-codebase-metrics.service must run as the checkout owner (User=david): \
         root reading /opt/boss is 'dubious ownership' and the measurement exits 3"
    );
    assert!(
        unit.contains("dubious ownership"),
        "the unit names the refusal the User= line prevents, so the next reader does not remove it"
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
    let unit = read("infra/boss-codebase-metrics.service");
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

    let store = shell_default(&installer, "STORE");
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
