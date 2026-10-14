//! Private original command observations survive worker TTL and observer restart.
use anyhow::{Context, Result, bail};
use base64::Engine;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Output;

pub(super) fn private_directory(path: &Path) -> Result<PathBuf> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    let uid = rustix::process::geteuid().as_raw();
    for ancestor in path
        .ancestors()
        .skip(1)
        .filter(|ancestor| ancestor.exists())
    {
        let metadata = std::fs::symlink_metadata(ancestor)?;
        let mode = metadata.permissions().mode();
        if !metadata.is_dir()
            || (metadata.uid() != uid && metadata.uid() != 0)
            || (mode & 0o022 != 0 && !(mode & 0o1000 != 0 && metadata.uid() == 0))
        {
            bail!("diagnostic journal ancestry is replaceable or unowned");
        }
    }
    if !path.exists() {
        std::fs::DirBuilder::new()
            .recursive(true)
            // mode-bits-ok: a private journal directory, never an executable file
            .mode(0o700)
            .create(path)?;
    }
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.uid() != uid || metadata.permissions().mode() & 0o077 != 0 {
        bail!("diagnostic journal is not a private owned directory");
    }
    Ok(path.to_path_buf())
}

pub(super) fn keep(path: &Path, value: &Value) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let lock_path = path.join("journal.lock");
    if lock_path.exists() && !std::fs::symlink_metadata(&lock_path)?.is_file() {
        bail!("diagnostic journal lock is not regular");
    }
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .open(lock_path)?;
    lock.lock()?;
    let sequence = originals(path)?.len();
    let raw = serde_json::to_vec(&json!({"sequence":sequence,"observation":value}))?;
    let name = format!("{sequence:020}-{}.json", hex::encode(Sha256::digest(&raw)));
    let target = path.join(name);
    let staging = private_directory(&path.join("pending"))?;
    // Leave every interrupted write in staging. An accepted sequence is
    // published only after its complete bytes have reached durable storage.
    let staged = tempfile::Builder::new()
        .prefix("original-")
        .tempfile_in(&staging)?;
    let (mut file, staged_path) = staged.keep()?;
    file.write_all(&raw)?;
    file.sync_all()?;
    std::fs::File::open(&staging)?.sync_all()?;
    match std::fs::hard_link(&staged_path, &target) {
        Ok(()) => std::fs::File::open(path)?.sync_all()?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            use std::os::unix::fs::MetadataExt;
            let metadata = std::fs::symlink_metadata(&target)?;
            if !metadata.is_file()
                || metadata.uid() != rustix::process::geteuid().as_raw()
                || metadata.mode() & 0o077 != 0
            {
                bail!("existing diagnostic original is not a private owned regular file");
            }
            let old = std::fs::read(&target)?;
            if old != raw {
                bail!("original diagnostic journal observation differs");
            }
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

pub(super) fn command(
    path: &Path,
    args: &[&str],
    input: Option<&[u8]>,
    output: &Output,
) -> Result<()> {
    let base64 = base64::engine::general_purpose::STANDARD;
    keep(
        path,
        &json!({"version":1,"argv":args,
        "input_b64":input.map(|bytes|base64.encode(bytes)),
        "stdout_b64":base64.encode(&output.stdout),"stderr_b64":base64.encode(&output.stderr),
        "exit":output.status.code()}),
    )
}

pub(super) fn recovered_launch(
    path: &Path,
    name: &str,
    baseline: &str,
) -> Result<Option<super::diagnostics::Launch>> {
    let commands = originals(path)?;
    let mut accepted = None;
    for row in &commands {
        let command = &row["observation"];
        if command["argv"] != json!(["create", "-f", "-", "-o", "json"]) || command["exit"] != 0 {
            continue;
        }
        let decode = |key: &str| -> Result<Vec<u8>> {
            Ok(base64::engine::general_purpose::STANDARD.decode(
                command[key]
                    .as_str()
                    .with_context(|| format!("retained native create {key} absent"))?,
            )?)
        };
        let manifest: Value = serde_json::from_slice(&decode("input_b64")?)?;
        let native: Value = serde_json::from_slice(&decode("stdout_b64")?)?;
        if manifest["metadata"]["name"] != name || native["metadata"]["name"] != name {
            bail!("retained native create acknowledgement names another attempt");
        }
        if manifest["metadata"]["annotations"]["boss.pipeline/baseline"] != baseline
            || native["metadata"]["annotations"] != manifest["metadata"]["annotations"]
            || !contains_declared(&native["spec"]["template"], &manifest["spec"]["template"])
        {
            bail!("retained native create acknowledgement baseline/template differs");
        }
        let assembled = manifest["metadata"]["annotations"]["boss.pipeline/assembled"]
            .as_str()
            .filter(|head| head.len() == 40 && head.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .context("retained acknowledged assembled identity absent")?;
        let uid = native["metadata"]["uid"]
            .as_str()
            .filter(|uid| !uid.is_empty())
            .context("retained native create acknowledgement UID absent")?;
        if accepted.is_some() {
            bail!("multiple retained acknowledged creates require reconciliation; never pick one");
        }
        accepted = Some(super::diagnostics::Launch {
            job_uid: uid.into(),
            assembled: assembled.into(),
            manifest,
            original_output: json!({"commands":commands,"created":native}),
        });
    }
    Ok(accepted)
}

fn contains_declared(actual: &Value, declared: &Value) -> bool {
    match declared {
        Value::Object(fields) => actual.as_object().is_some_and(|actual| {
            fields.iter().all(|(key, value)| {
                actual
                    .get(key)
                    .is_some_and(|actual| contains_declared(actual, value))
            })
        }),
        Value::Array(fields) => actual.as_array().is_some_and(|actual| {
            actual.len() == fields.len()
                && actual
                    .iter()
                    .zip(fields)
                    .all(|(actual, declared)| contains_declared(actual, declared))
        }),
        _ => actual == declared,
    }
}

pub(super) fn originals(path: &Path) -> Result<Vec<Value>> {
    use std::os::unix::fs::MetadataExt;
    let uid = rustix::process::geteuid().as_raw();
    let mut paths = std::fs::read_dir(path)?
        .map(|row| row.map(|row| row.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    paths.sort();
    let mut originals = Vec::new();
    for file in paths {
        if file.file_name().and_then(|name| name.to_str()) == Some("journal.lock") {
            continue;
        }
        if file.file_name().and_then(|name| name.to_str()) == Some("pending") {
            private_directory(&file)?;
            continue;
        }
        let metadata = std::fs::symlink_metadata(&file)?;
        if !metadata.is_file() || metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
            bail!("diagnostic original is not a private owned regular file");
        }
        let raw = std::fs::read(&file)?;
        let value: Value =
            serde_json::from_slice(&raw).context("original diagnostic record is malformed")?;
        if value["sequence"].as_u64() != Some(originals.len() as u64) {
            bail!("diagnostic original sequence is incomplete or duplicated");
        }
        let expected = format!(
            "{:020}-{}.json",
            originals.len(),
            hex::encode(Sha256::digest(&raw))
        );
        if file.file_name().and_then(|name| name.to_str()) != Some(expected.as_str()) {
            bail!("diagnostic original hash/identity differs");
        }
        originals.push(value);
    }
    Ok(originals)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn fs_group_home_is_refused_but_private_mounted_journal_retains_restart_evidence() {
        use std::os::unix::fs::PermissionsExt;
        let parent = tempfile::tempdir().unwrap();
        let shared = parent.path().join("shared-home");
        std::fs::create_dir(&shared).unwrap();
        // mode-bits-ok: directory models the measured fsGroup PVC root.
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o2775)).unwrap();
        let refused = private_directory(&shared.join("diagnostics").join("request"));
        assert!(
            refused
                .unwrap_err()
                .to_string()
                .contains("ancestry is replaceable")
        );
        assert!(!shared.join("diagnostics").exists());
        // A separate mount presents the owned child outside shared-home's
        // ancestry. This models the visible tree, not Kubernetes mount execution.
        let mounted = private_directory(&parent.path().join("mounted-journal")).unwrap();
        let request = private_directory(&mounted.join("request")).unwrap();
        keep(&request, &json!({"native":"original acknowledgement"})).unwrap();
        let restarted = private_directory(&mounted.join("request")).unwrap();
        assert_eq!(
            originals(&restarted).unwrap()[0]["observation"]["native"],
            "original acknowledgement"
        );
        assert_eq!(
            std::fs::metadata(&shared).unwrap().permissions().mode() & 0o7777,
            0o2775
        );
    }

    #[test]
    fn interrupted_staging_preserves_prior_complete_originals_and_all_pending_bytes() {
        let parent = tempfile::tempdir().unwrap();
        let path = private_directory(&parent.path().join("journal")).unwrap();
        let first = json!({"stdout_b64":"//4=","stderr_b64":"/Q==","exit":0});
        keep(&path, &first).unwrap();
        let staging = path.join("pending");
        private_directory(&staging).unwrap();
        let partial = staging.join("interrupted");
        std::fs::write(&partial, b"original bytes not fully written").unwrap();
        let originals = originals(&path).unwrap();
        assert_eq!(originals.len(), 1);
        assert_eq!(originals[0]["observation"], first);
        assert_eq!(
            std::fs::read(partial).unwrap(),
            b"original bytes not fully written"
        );
        keep(&path, &json!({"exit":1,"stderr_b64":"full-original"})).unwrap();
        assert_eq!(super::originals(&path).unwrap().len(), 2);
    }

    #[test]
    fn acknowledged_create_recovers_exact_original_without_new_compute() {
        use std::os::unix::process::ExitStatusExt;
        let parent = tempfile::tempdir().unwrap();
        let path = private_directory(&parent.path().join("journal")).unwrap();
        let baseline = "b".repeat(40);
        let assembled = "a".repeat(40);
        let manifest = json!({"metadata":{"name":"consist-original","annotations":{
            "boss.pipeline/assembled":assembled,"boss.pipeline/baseline":baseline}},"spec":{"template":{"ordinary":"fixture"}}});
        let mut native = manifest.clone();
        native["metadata"]["uid"] = json!(Uuid::new_v4().to_string());
        native["spec"]["template"]["server_default"] = json!("ordinary native default");
        let output = Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: serde_json::to_vec(&native).unwrap(),
            stderr: b"full acknowledgement diagnostic".to_vec(),
        };
        command(
            &path,
            &["create", "-f", "-", "-o", "json"],
            Some(&serde_json::to_vec(&manifest).unwrap()),
            &output,
        )
        .unwrap();
        let recovered = recovered_launch(&path, "consist-original", &baseline)
            .unwrap()
            .unwrap();
        assert_eq!(recovered.manifest, manifest);
        assert_eq!(recovered.assembled, assembled);
        assert_eq!(
            recovered.job_uid,
            native["metadata"]["uid"].as_str().unwrap()
        );
        assert_eq!(
            recovered.original_output["commands"],
            json!(originals(&path).unwrap())
        );
        assert!(recovered_launch(&path, "consist-original", &"c".repeat(40)).is_err());
        let changed = serde_json::json!({"ordinary":"changed declared value"});
        assert!(!contains_declared(&changed, &manifest["spec"]["template"]));
    }
}
