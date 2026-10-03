//! A Codex or Gemini session on the dev pod can reach the doors a BOSS
//! agent works through (backlog 5840c068, from the selector builder,
//! run 3f46595c, 2026-10-01).
//!
//! Every door is network: `boss-api` and the `boss` shim speak HTTP to
//! the system of record at the address in `infra/dev/sor-url`, and a
//! car is fetched from and pushed to the forge. codex-cli 0.159.3's
//! `workspace-write` sandbox leaves `[sandbox_workspace_write]
//! network_access` at its default, which is OFF (the key read off the
//! installed binary: struct SandboxWorkspaceWrite, fields writable_roots
//! / network_access / exclude_tmpdir_env_var / exclude_slash_tmp), so
//! the versioned config as first written gave Codex a session that
//! could edit the checkout and reach nothing — an agent that cannot
//! reach its doors cannot do the work it is registered for.
//!
//! gemini-cli 0.62.0 runs no sandbox unless one is asked for:
//! `tools.sandbox` defaults to undefined and `security.toolSandboxing`
//! to false (its bundled docs/reference/configuration.md and
//! docs/cli/settings.md). If the versioned settings ever turn either
//! on, `tools.sandboxNetworkAccess` (default false) must come with it.
//!
//! The versioned file is what lands: dev-session.sh places each config
//! only where the PVC home has none, and none exists there yet.

use boss_testing::repo_root;

#[test]
fn the_codex_config_lets_a_workspace_write_session_reach_the_network() {
    let path = repo_root().join("infra/dev/cli/codex-config.toml");
    let codex: toml::Value = toml::from_str(
        &std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())),
    )
    .expect("codex-config.toml parses as TOML");
    assert_eq!(
        codex.get("sandbox_mode").and_then(toml::Value::as_str),
        Some("workspace-write"),
        "the sandbox this setting belongs to"
    );
    assert_eq!(
        codex
            .get("sandbox_workspace_write")
            .and_then(|t| t.get("network_access"))
            .and_then(toml::Value::as_bool),
        Some(true),
        "[sandbox_workspace_write] network_access = true — codex's default is off, and with it \
         off boss-api, boss and git fetch all fail inside the session: {codex}"
    );
}

#[test]
fn the_gemini_settings_run_no_sandbox_that_would_cut_the_network() {
    let path = repo_root().join("infra/dev/cli/gemini-settings.json");
    let gemini: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())),
    )
    .expect("gemini-settings.json parses as JSON");
    let on = |pointer: &str| {
        gemini.pointer(pointer).is_some_and(|v| {
            // `tools.sandbox` may be a boolean, a profile path or a
            // command name; anything but absent or false turns it on.
            !(v.is_null() || v == &serde_json::Value::Bool(false))
        })
    };
    let sandboxed = on("/tools/sandbox") || on("/security/toolSandboxing");
    let networked = gemini.pointer("/tools/sandboxNetworkAccess") == Some(&true.into());
    assert!(
        !sandboxed || networked,
        "a sandbox is enabled without tools.sandboxNetworkAccess = true, so boss-api, boss and \
         git fetch would fail inside the session: {gemini}"
    );
}
