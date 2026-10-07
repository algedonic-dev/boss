//! Gemini's plan mode is not read-only: it permits plan writes and mode exit.
//! A launcher-owned administrator policy restricts tools in every mode;
//! private user settings disable hooks and skills. Environment isolation,
//! empty MCP configuration and explicit extension exclusion remove startup
//! execution paths that a tool policy alone cannot constrain.
use anyhow::{Context, Result, bail};
use serde_json::json;
use std::path::{Path, PathBuf};

/// Preserve native statistics without converting them to billing. In
/// particular stream-json omits reasoning and cache-write semantics. The
/// aggregate must still conserve the five counters its own models report.
pub fn complete_stats(value: &serde_json::Value) -> bool {
    let Some(models) = value.get("models").and_then(serde_json::Value::as_object) else {
        return false;
    };
    if ["duration_ms", "tool_calls"].iter().any(|field| {
        value
            .get(field)
            .and_then(serde_json::Value::as_u64)
            .is_none()
    }) {
        return false;
    }
    for field in [
        "total_tokens",
        "input_tokens",
        "output_tokens",
        "cached",
        "input",
    ] {
        let mut total = 0u64;
        for (model, counters) in models {
            if model.trim().is_empty() {
                return false;
            }
            let Some(count) = counters.get(field).and_then(serde_json::Value::as_u64) else {
                return false;
            };
            let Some(sum) = total.checked_add(count) else {
                return false;
            };
            total = sum;
        }
        if value.get(field).and_then(serde_json::Value::as_u64) != Some(total) {
            return false;
        }
    }
    true
}

const POLICY: &str = r#"[[rule]]
toolName = "*"
decision = "deny"
priority = 998
denyMessage = "This BOSS execution permits native file reads only."

[[rule]]
toolName = ["read_file", "list_directory", "glob", "grep_search"]
decision = "allow"
priority = 999
"#;

async fn refuse_system_policy(directory: &Path) -> Result<()> {
    let mut entries = match tokio::fs::read_dir(directory).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).context("Gemini administrator policy directory is unreadable");
        }
    };
    while let Some(entry) = entries.next_entry().await? {
        if entry.path().extension().is_some_and(|e| e == "toml") {
            // The native loader ignores --admin-policy when system TOMLs exist.
            bail!(
                "Gemini system administrator policies would override the launcher policy; no process started"
            );
        }
    }
    Ok(())
}

pub struct Prepared {
    pub policy: PathBuf,
    pub home: PathBuf,
    pub system_settings: PathBuf,
    pub system_defaults: PathBuf,
}
impl Prepared {
    pub fn environment(&self, command: &mut tokio::process::Command) {
        command.env_clear();
        for key in ["PATH", "HOME", "LANG"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        command
            .env("GEMINI_CLI_HOME", &self.home)
            .env("GEMINI_CLI_SYSTEM_SETTINGS_PATH", &self.system_settings)
            .env("GEMINI_CLI_SYSTEM_DEFAULTS_PATH", &self.system_defaults);
    }
}

async fn require_absent(path: &Path) -> Result<()> {
    match tokio::fs::symlink_metadata(path).await {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("Gemini configuration absence cannot be established"),
        Ok(_) => bail!(
            "unsupported ambient Gemini configuration at {}; no model process started",
            path.display()
        ),
    }
}

async fn refuse_workspace_configuration(workspace: &Path) -> Result<()> {
    require_absent(&workspace.join(".gemini")).await?;
    // Native dotenv discovery walks ancestors. Refuse by metadata; never read
    // the file to decide whether its values would be safe.
    let mut current = Some(workspace);
    while let Some(path) = current {
        require_absent(&path.join(".env")).await?;
        require_absent(&path.join(".gemini/.env")).await?;
        current = path.parent();
    }
    Ok(())
}

pub async fn prepare(scratch: &Path, workspace: &Path, effort: &str) -> Result<Prepared> {
    if !cfg!(target_os = "linux") {
        bail!("the Gemini administrator-policy boundary is verified only on Linux");
    }
    refuse_system_policy(Path::new("/etc/gemini-cli/policies")).await?;
    refuse_workspace_configuration(workspace).await?;
    // Explicit adapter controls, not claims that provider reasoning consumed
    // this budget. The admitted 2.5 Pro range is 128..=32768; high uses its
    // maximum, medium the native chat-base-2.5 preset, low a smaller budget.
    let thinking_budget = match effort {
        "low" => 1024,
        "medium" => 8192,
        "high" => 32768,
        _ => bail!("Gemini supports only declared low/medium/high effort"),
    };
    let policy = scratch.join("read-only-policy.toml");
    let home = scratch.join("gemini-home");
    tokio::fs::create_dir(&home).await?;
    let config = home.join(".gemini");
    tokio::fs::create_dir(&config).await?;
    let settings = config.join("settings.json");
    tokio::fs::write(&policy, POLICY).await?;
    tokio::fs::write(&settings, serde_json::to_vec_pretty(&json!({
        "hooksConfig":{"enabled":false},
        "skills":{"enabled":false},
        "advanced":{"ignoreLocalEnv":true},
        "mcpServers":{},"mcp":{"allowed":[]},
        "security":{"disableYoloMode":true,"disableAlwaysAllow":true,"auth":{"selectedType":"gemini-api-key"}},
        "modelConfigs":{"customOverrides":[{"match":{"model":"gemini-2.5-pro"},"modelConfig":{"model":"gemini-2.5-pro","generateContentConfig":{"thinkingConfig":{"thinkingBudget":thinking_budget}}}}]}
    }))?).await?;
    // User-owned system settings are skipped by the native loader. Fixed
    // missing paths instead prevent fallback to ambient system/default files;
    // the private USER settings above are the actual applied controls.
    Ok(Prepared {
        policy,
        home,
        system_settings: scratch.join("missing-system-settings.json"),
        system_defaults: scratch.join("missing-system-defaults.json"),
    })
}

pub async fn verify_version(
    executable: &Path,
    scratch: &Path,
    workspace: &Path,
    prepared: &Prepared,
) -> Result<()> {
    let mut command = tokio::process::Command::new(executable);
    prepared.environment(&mut command);
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        command
            .current_dir(workspace)
            .arg("--version")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("Gemini version verification timed out")??;
    tokio::fs::write(scratch.join("version.stdout"), &output.stdout).await?;
    tokio::fs::write(scratch.join("version.stderr"), &output.stderr).await?;
    if !output.status.success() || output.stdout.as_slice() != b"0.62.0\n" {
        bail!(
            "Gemini version does not match the verified 0.62.0 policy/settings contract; no model process started"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn ambient_configuration_is_refused_by_metadata_without_reading_it() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        tokio::fs::create_dir(&workspace).await.unwrap();
        refuse_workspace_configuration(&workspace).await.unwrap();
        for path in [
            workspace.join(".gemini"),
            root.path().join(".env"),
            root.path().join(".gemini/.env"),
        ] {
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await.unwrap();
            }
            tokio::fs::write(&path, "must not be read or parsed")
                .await
                .unwrap();
            assert!(refuse_workspace_configuration(&workspace).await.is_err());
            tokio::fs::remove_file(path).await.unwrap();
        }
        let link = workspace.join(".env");
        std::os::unix::fs::symlink(root.path().join("missing-target"), &link).unwrap();
        assert!(
            refuse_workspace_configuration(&workspace).await.is_err(),
            "dangling configuration is not absence"
        );
    }
    #[test]
    fn native_model_statistics_conserve_exact_counters_without_billing_inference() {
        let stats = json!({"duration_ms":1,"tool_calls":0,"total_tokens":7,"input_tokens":5,"output_tokens":2,"cached":3,"input":2,"models":{"reported-model":{"total_tokens":7,"input_tokens":5,"output_tokens":2,"cached":3,"input":2}}});
        assert!(complete_stats(&stats));
        for field in [
            "duration_ms",
            "tool_calls",
            "total_tokens",
            "input_tokens",
            "output_tokens",
            "cached",
            "input",
            "models",
        ] {
            let mut absent = stats.clone();
            absent.as_object_mut().unwrap().remove(field);
            assert!(!complete_stats(&absent), "{field}");
        }
        let mut inconsistent = stats.clone();
        inconsistent["input_tokens"] = json!(4);
        assert!(!complete_stats(&inconsistent));
        let mut negative = stats.clone();
        negative["models"]["reported-model"]["cached"] = json!(-1);
        assert!(!complete_stats(&negative));
        let mut overflow = stats.clone();
        overflow["models"]["second-model"] = overflow["models"]["reported-model"].clone();
        overflow["models"]["second-model"]["total_tokens"] = json!(u64::MAX);
        assert!(!complete_stats(&overflow));
    }
    #[tokio::test]
    async fn installed_admin_policy_is_a_refusal_not_an_assumed_override() {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("absent");
        refuse_system_policy(&missing).await.unwrap();
        tokio::fs::create_dir(&missing).await.unwrap();
        refuse_system_policy(&missing).await.unwrap();
        tokio::fs::write(missing.join("custom.toml"), "")
            .await
            .unwrap();
        assert!(refuse_system_policy(&missing).await.is_err());
    }
    #[tokio::test]
    async fn unreadable_admin_policy_location_is_not_absence() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("not-a-directory");
        tokio::fs::write(&file, "x").await.unwrap();
        assert!(refuse_system_policy(&file).await.is_err());
    }
}
