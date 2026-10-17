//! Private original command observations survive worker TTL and observer restart.
use anyhow::{Context, Result, bail};
use base64::Engine;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Output;

// A refusal leaves every original on disk. Evidence size is not permission to
// exhaust the controller that records unrelated gates (23007610, 2026-10-16).
pub(super) const MAX_READ_BYTES: u64 = 16 * 1024 * 1024;
pub(super) const MAX_READ_FILES: usize = 4096;

pub(super) fn check_read_bound(files: usize, bytes: u64) -> Result<()> {
    if files > MAX_READ_FILES || bytes > MAX_READ_BYTES {
        bail!(
            "diagnostic evidence UNKNOWN: complete-read bound exceeded ({files} files, {bytes} bytes; limits {MAX_READ_FILES} files/{MAX_READ_BYTES} bytes); all original files retained unchanged"
        );
    }
    Ok(())
}

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
    // Validate one row at a time: counting must not clone the whole journal
    // on every append. Inventory refuses oversized input before reading bodies.
    let paths = original_paths(path)?;
    let mut remaining = MAX_READ_BYTES;
    for (sequence, file) in paths.iter().enumerate() {
        read_original(file, sequence, &mut remaining)?;
    }
    let sequence = paths.len();
    let retained_bytes = paths.iter().try_fold(0u64, |total, file| -> Result<u64> {
        total
            .checked_add(std::fs::symlink_metadata(file)?.len())
            .context("retained journal size overflow")
    })?;
    let staging = private_directory(&path.join("pending"))?;
    // Leave every interrupted write in staging. An accepted sequence is
    // published only after its complete bytes have reached durable storage.
    let staged = tempfile::Builder::new()
        .prefix("original-")
        .tempfile_in(&staging)?;
    let (mut file, staged_path) = staged.keep()?;
    // Serialize borrowed data directly to durable staging. Building json!(...)
    // cloned the whole observation before allocating its second serialized copy.
    #[derive(serde::Serialize)]
    struct Row<'a> {
        observation: &'a Value,
        sequence: usize,
    }
    serde_json::to_writer(
        &mut file,
        &Row {
            observation: value,
            sequence,
        },
    )?;
    file.flush()?;
    file.sync_all()?;
    std::fs::File::open(&staging)?.sync_all()?;
    let (bytes, digest) = file_identity(&staged_path)?;
    // On overflow the COMPLETE encoded stream stays in pending, never a
    // truncated accepted record or a claimed successful observation.
    check_read_bound(
        sequence + 1,
        retained_bytes
            .checked_add(bytes)
            .context("appended journal size overflow")?,
    )?;
    let target = path.join(format!("{sequence:020}-{digest}.json"));
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
            if file_identity(&target)? != (bytes, digest) {
                bail!("original diagnostic journal observation differs");
            }
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn file_identity(path: &Path) -> Result<(u64, String)> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut bytes = 0u64;
    let mut buffer = [0u8; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        bytes = bytes
            .checked_add(count as u64)
            .context("journal stream size overflow")?;
        hash.update(&buffer[..count]);
    }
    Ok((bytes, hex::encode(hash.finalize())))
}

pub(super) fn command(
    path: &Path,
    args: &[&str],
    input: Option<&[u8]>,
    output: &Output,
) -> Result<()> {
    keep(path, &command_value(args, input, output))
}

pub(super) fn command_value(args: &[&str], input: Option<&[u8]>, output: &Output) -> Value {
    let base64 = base64::engine::general_purpose::STANDARD;
    json!({"version":1,"argv":args,
        "input_b64":input.map(|bytes|base64.encode(bytes)),
        "stdout_b64":base64.encode(&output.stdout),"stderr_b64":base64.encode(&output.stderr),
        "exit":output.status.code()})
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

fn original_paths(path: &Path) -> Result<Vec<PathBuf>> {
    use std::os::unix::fs::MetadataExt;
    let uid = rustix::process::geteuid().as_raw();
    let mut paths = Vec::new();
    let mut bytes = 0u64;
    for entry in std::fs::read_dir(path)? {
        let file = entry?.path();
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
        bytes = bytes
            .checked_add(metadata.len())
            .context("diagnostic evidence byte count overflow")?;
        check_read_bound(paths.len() + 1, bytes)?;
        paths.push(file);
    }
    paths.sort();
    Ok(paths)
}

fn read_original(file: &Path, sequence: usize, remaining: &mut u64) -> Result<Value> {
    let raw = read_bytes(file, remaining)?;
    let value: Value =
        serde_json::from_slice(&raw).context("original diagnostic record is malformed")?;
    if value["sequence"].as_u64() != Some(sequence as u64) {
        bail!("diagnostic original sequence is incomplete or duplicated");
    }
    let expected = format!(
        "{:020}-{}.json",
        sequence,
        hex::encode(Sha256::digest(&raw))
    );
    if file.file_name().and_then(|name| name.to_str()) != Some(expected.as_str()) {
        bail!("diagnostic original hash/identity differs");
    }
    Ok(value)
}

pub(super) fn read_bytes(file: &Path, remaining: &mut u64) -> Result<Vec<u8>> {
    // Bound the actual read too: metadata is not a lock against a writer.
    use std::io::Read;
    let mut raw = Vec::new();
    std::fs::File::open(file)?
        .take(*remaining + 1)
        .read_to_end(&mut raw)?;
    if raw.len() as u64 > *remaining {
        bail!(
            "diagnostic evidence UNKNOWN: cumulative actual-byte read bound exceeded; all original files retained unchanged"
        );
    }
    *remaining -= raw.len() as u64;
    Ok(raw)
}

pub(super) fn originals(path: &Path) -> Result<Vec<Value>> {
    let mut remaining = MAX_READ_BYTES;
    original_paths(path)?
        .iter()
        .enumerate()
        .map(|(sequence, file)| read_original(file, sequence, &mut remaining))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn an_oversized_original_refuses_materialization_without_changing_any_bytes() {
        use std::os::unix::fs::PermissionsExt;
        let parent = tempfile::tempdir().unwrap();
        let path = private_directory(&parent.path().join("journal")).unwrap();
        let raw = serde_json::to_vec(
            &json!({"sequence":0,"observation":{"original":"x".repeat(16 * 1024 * 1024)}}),
        )
        .unwrap();
        let digest = hex::encode(Sha256::digest(&raw));
        let file = path.join(format!("{:020}-{digest}.json", 0));
        std::fs::write(&file, &raw).unwrap();
        // mode-bits-ok: private original journal fixture, never executable.
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        let Err(error) = originals(&path) else {
            panic!("oversized evidence must remain UNKNOWN before materialization");
        };
        assert!(error.to_string().contains("bound"), "{error:#}");
        assert_eq!(std::fs::read(&file).unwrap(), raw);
    }

    #[test]
    fn oversized_append_keeps_the_whole_staged_stream_and_refuses_publication() {
        let root = tempfile::tempdir().unwrap();
        let path = private_directory(&root.path().join("journal")).unwrap();
        let value = json!({"original":"x".repeat(16 * 1024 * 1024)});
        let Err(error) = keep(&path, &value) else {
            panic!("oversized append must remain UNKNOWN");
        };
        assert!(error.to_string().contains("bound"), "{error:#}");
        assert!(originals(&path).unwrap().is_empty());
        let pending = std::fs::read_dir(path.join("pending"))
            .unwrap()
            .map(|p| p.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(pending.len(), 1);
        assert_eq!(
            std::fs::read(&pending[0]).unwrap(),
            serde_json::to_vec(&json!({"sequence":0,"observation":value})).unwrap()
        );
    }

    #[test]
    fn actual_byte_budget_refuses_growth_after_a_small_inventory() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let path = private_directory(&root.path().join("journal")).unwrap();
        let first = path.join("first");
        let second = path.join("second");
        std::fs::write(&first, vec![b'a'; 10 * 1024 * 1024]).unwrap();
        std::fs::write(&second, b"small").unwrap();
        for file in [&first, &second] {
            // mode-bits-ok: private byte-budget fixture, never executable.
            std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert_eq!(
            original_paths(&path).unwrap().len(),
            2,
            "initial inventory fits"
        );
        let original = vec![b'b'; 8 * 1024 * 1024];
        std::fs::write(&second, &original).unwrap();
        let mut remaining = MAX_READ_BYTES;
        let _first = read_bytes(&first, &mut remaining).unwrap();
        assert!(
            read_bytes(&second, &mut remaining).is_err(),
            "post-inventory growth exceeded the cumulative actual-byte budget"
        );
        assert_eq!(std::fs::read(second).unwrap(), original);
    }

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
