//! The gateway's auth-event sink is configured where the gateway runs
//! (backlog d49b4355).
//!
//! The gateway stages login, guest, break-glass and elevation events on
//! the outbox only when the variable its sink reads is set. Until
//! 2026-09-28 that was a variable of its own, set in one place — a
//! bare-metal systemd drop-in the cluster never read — so in the cluster
//! every auth event was a warn line in the pod log, and
//! `/api/events/tail?source=gateway` answered `[]` while an unfiltered
//! tail had rows. Nothing noticed, because an unset sink is a legal
//! configuration (it degrades to the warn line by design).
//!
//! THE FACT THAT LIVES TWICE (CLAUDE.md §9a): the name of the variable
//! the gateway reads (`AUDIT_SINK_URL_VAR` in boss-gateway's audit.rs)
//! and the env the cluster manifest gives the container the gateway runs
//! in. This reads the first from its definition and asserts the second
//! sets it from the database Secret — a literal would put a credential in
//! the tree, and absence is the silent degrade this item was.

use boss_testing::repo_root;

const AUDIT_RS: &str = "crates/core/boss-gateway/src/audit.rs";
const MANIFEST: &str = "infra/cluster/manifests/boss.yaml";

fn read(rel: &str) -> String {
    std::fs::read_to_string(repo_root().join(rel))
        .unwrap_or_else(|e| panic!("{rel}: {e} — nothing here is a verdict"))
}

/// The variable name, read from its `pub const` definition (never a
/// mention in a comment).
fn sink_var() -> String {
    let src = read(AUDIT_RS);
    let marker = "pub const AUDIT_SINK_URL_VAR: &str = \"";
    let start = src
        .find(marker)
        .unwrap_or_else(|| panic!("{AUDIT_RS} no longer defines `{marker}…\"`"))
        + marker.len();
    let len = src[start..]
        .find('"')
        .unwrap_or_else(|| panic!("{AUDIT_RS}: unterminated AUDIT_SINK_URL_VAR"));
    src[start..start + len].to_string()
}

/// The `boss` container's block: from its `- name: boss` line to the end
/// of that YAML document. The gateway is one of the binaries the
/// container's launcher starts, with the container's whole environment.
fn boss_container(manifest: &str) -> &str {
    let start = manifest
        .find("\n      containers:\n        - name: boss\n")
        .unwrap_or_else(|| panic!("{MANIFEST} has no `boss` container"));
    let rest = &manifest[start..];
    let end = rest.find("\n---").unwrap_or(rest.len());
    &rest[..end]
}

#[test]
fn the_boss_container_sets_the_gateway_sink_from_the_database_secret() {
    let var = sink_var();
    let manifest = read(MANIFEST);
    let container = boss_container(&manifest);
    let lines: Vec<&str> = container.lines().collect();
    let at = lines
        .iter()
        .position(|l| l.trim() == format!("- name: {var}"))
        .unwrap_or_else(|| {
            panic!(
                "the `boss` container in {MANIFEST} does not set {var}, the variable the \
                 gateway's auth-event sink reads — every login, guest session and \
                 break-glass use would be a warn line and nothing in the log (d49b4355)"
            )
        });
    let source = lines.get(at + 1).map(|l| l.trim()).unwrap_or("");
    assert_eq!(
        source, "valueFrom: {secretKeyRef: {name: boss-secrets, key: database-url}}",
        "{var} in the `boss` container must come from the database Secret, never a \
         literal (a literal is a credential in the tree)"
    );
}

/// The retired variable and its drop-in stay retired: a copy of either
/// would bring back the literal-password URL (backlog 7ec7113b).
#[test]
fn the_drop_in_and_its_variable_are_gone() {
    assert!(
        !repo_root().join("infra/gateway").exists(),
        "infra/gateway/ is back — its unit and drop-ins were bare-metal residue whose \
         installer left on 2026-09-18"
    );
    let retired = concat!("BOSS_GATEWAY_", "AUDIT_DB_URL");
    for rel in [MANIFEST, "infra/oss-quickstart/docker-compose.yml"] {
        assert!(
            !read(rel).contains(retired),
            "{rel} sets {retired}, which nothing reads any more"
        );
    }
}
