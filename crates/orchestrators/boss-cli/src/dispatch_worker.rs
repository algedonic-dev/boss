//! The source executor's worker belongs to its run (f0de9c6f), through
//! the published controller. The API records its receipt, not its authority.

use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::Value;

pub(crate) const KEY: &str = "worker_session";
pub(crate) const RETIREMENT_KEY: &str = "worker_retirement";

pub(crate) async fn record_retirement_at(
    http: &crate::machine_http::MachineClient,
    base: &str,
    run: &str,
    actor: &str,
    retirement: Value,
) -> Result<()> {
    let path = format!("/api/jobs/{run}");
    let signature = crate::identity::Signature::As(actor.to_string());
    crate::sor_client::api_at_signed(
        http,
        base,
        reqwest::Method::PATCH,
        &format!("{path}/metadata"),
        Some(serde_json::json!({RETIREMENT_KEY:retirement})),
        signature.clone(),
    )
    .await?;
    let after =
        crate::sor_client::api_at_signed(http, base, reqwest::Method::GET, &path, None, signature)
            .await?
            .context("worker retirement readback returned no run")?;
    if after.pointer(&format!("/metadata/{RETIREMENT_KEY}")) != Some(&retirement) {
        bail!("worker retirement did not read back; the controller retains its receipt for retry");
    }
    Ok(())
}

#[async_trait::async_trait]
pub(crate) trait Workers: Send + Sync {
    async fn ensure(&self, run: &str, root: &Path) -> Result<Option<Value>>;
    async fn stop(&self, run: &str) -> Result<Option<Value>>;
}

pub(crate) struct Published;

impl Published {
    async fn call(&self, args: &[&std::ffi::OsStr]) -> Result<Option<Value>> {
        let launcher = match crate::door_env::dev_worker_route() {
            crate::door_env::DevChild::Local => return Ok(None),
            crate::door_env::DevChild::Forward(path) => path,
            crate::door_env::DevChild::Refuse(why) => bail!("dev worker REFUSED: {why}"),
        };
        let mut command = tokio::process::Command::new(launcher);
        command
            .env_clear()
            .env("HOME", "/work/home")
            .env("PATH", "/usr/bin:/bin");
        for name in [
            "KUBERNETES_SERVICE_HOST",
            "KUBERNETES_SERVICE_PORT",
            "KUBERNETES_SERVICE_PORT_HTTPS",
        ] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        let output = command
            .args(args)
            .output()
            .await
            .context("calling the published worker controller")?;
        if !output.status.success() {
            // Both streams are the controller's only diagnosis, including
            // the allocation refusal and retained recovery identity.
            bail!(
                "published worker controller exited {}:\n{}{}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(Some(
            serde_json::from_slice(&output.stdout)
                .context("the worker controller returned no receipt")?,
        ))
    }
}

#[async_trait::async_trait]
impl Workers for Published {
    async fn ensure(&self, run: &str, root: &Path) -> Result<Option<Value>> {
        if matches!(
            crate::door_env::dev_worker_route(),
            crate::door_env::DevChild::Local
        ) {
            return Ok(None);
        }
        let root = tokio::fs::canonicalize(root)
            .await
            .context("resolving the run's source worktree")?;
        let output = tokio::process::Command::from(crate::git_auth::command())
            .arg("-C")
            .arg(&root)
            .args(["rev-parse", "HEAD"])
            .output()
            .await?;
        if !output.status.success() {
            bail!(
                "reading the run's source head: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let head = String::from_utf8(output.stdout)?.trim().to_string();
        self.call(&[
            "ensure-run".as_ref(),
            run.as_ref(),
            head.as_ref(),
            "--local-root".as_ref(),
            root.as_os_str(),
        ])
        .await
    }

    async fn stop(&self, run: &str) -> Result<Option<Value>> {
        self.call(&["stop-run".as_ref(), run.as_ref()]).await
    }
}
