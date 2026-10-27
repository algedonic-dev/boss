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
    // Before the bound is judged, so a journal already at it still sheds
    // the twins an earlier build left (194cb548): the refusal below is
    // every later pass on such a journal, and pruning only on success
    // would never reach the one it was measured on.
    let released = release_published_twins(path, &paths)?;
    if released > 0 {
        super::log(format!(
            "diagnostic journal {}: released {released} staged twin(s) of published rows from pending/ (no byte removed; backlog 194cb548)",
            path.display()
        ));
    }
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
    // Serialize borrowed data directly: building json!(...) cloned the whole
    // observation before allocating its second serialized copy.
    #[derive(serde::Serialize)]
    struct Row<'a> {
        observation: &'a Value,
        sequence: usize,
    }
    let row = Row {
        observation: value,
        sequence,
    };
    // Judge the bound BEFORE anything is written. Checked only after
    // staging, every refused row stayed in pending/ for good, so a request
    // at its bound grew by one unread file per pass with no end (53 on
    // assessment 28e96fbc, measured 2026-10-17, backlog daab12f4). A doomed
    // append writes nothing and stays UNKNOWN; every original is unchanged.
    let encoded = {
        let mut counted = CountingSink(0);
        serde_json::to_writer(&mut counted, &row)?;
        counted.0
    };
    check_read_bound(
        sequence + 1,
        retained_bytes
            .checked_add(encoded)
            .context("appended journal size overflow")?,
    )?;
    let staging = private_directory(&path.join("pending"))?;
    // Leave every interrupted write in staging. An accepted sequence is
    // published only after its complete bytes have reached durable storage.
    let staged = tempfile::Builder::new()
        .prefix("original-")
        .tempfile_in(&staging)?;
    let (mut file, staged_path) = staged.keep()?;
    serde_json::to_writer(&mut file, &row)?;
    file.flush()?;
    file.sync_all()?;
    std::fs::File::open(&staging)?.sync_all()?;
    let (bytes, digest) = file_identity(&staged_path)?;
    if bytes != encoded {
        bail!("staged diagnostic original differs from its judged size; it stays in pending");
    }
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
    // The published name is durable, so the staged name is only a twin of
    // the same bytes. Kept, a whole-journal snapshot counted every accepted
    // row twice and the effective read bound was half the declared one
    // (evidence 2026-10-19 on daab12f4). An interrupted write never gets
    // here, so pending/ still holds exactly what never published.
    std::fs::remove_file(&staged_path)?;
    std::fs::File::open(&staging)?.sync_all()?;
    Ok(())
}

/// The (device, inode) of every published row: what a staged twin shares.
pub(super) fn published_inodes<'a>(
    published: impl IntoIterator<Item = &'a std::fs::Metadata>,
) -> std::collections::BTreeSet<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    published.into_iter().map(|m| (m.dev(), m.ino())).collect()
}

/// A pending/ entry that is a second name for a published row's inode.
/// It holds no byte the published name does not, so a reader skips it and
/// the writer may release it. Anything else in pending/ — an interrupted
/// write, a row refused at the bound — has bytes held nowhere else and is
/// never matched here (backlog 194cb548).
pub(super) fn is_published_twin(
    inodes: &std::collections::BTreeSet<(u64, u64)>,
    entry: &std::fs::Metadata,
) -> bool {
    use std::os::unix::fs::MetadataExt;
    entry.is_file() && inodes.contains(&(entry.dev(), entry.ino()))
}

/// Release the staged twins an earlier build left in pending/ (backlog
/// 194cb548): before daab12f4 every accepted row kept its staged name, so
/// a journal written then carries one twin per published row and is
/// counted double by a whole-journal read. Removing a twin unlinks a name,
/// never bytes — the published name still links the same inode — which is
/// the act keep() already performs for every row it publishes since
/// daab12f4. Called only under the journal's exclusive lock, bounded by
/// the published set (at most MAX_READ_FILES inodes), idempotent: once the
/// twins are gone it matches nothing and removes nothing.
fn release_published_twins(path: &Path, published: &[PathBuf]) -> Result<usize> {
    let staging = path.join("pending");
    if !staging.exists() {
        return Ok(0);
    }
    private_directory(&staging)?;
    let published = published
        .iter()
        .map(std::fs::symlink_metadata)
        .collect::<std::io::Result<Vec<_>>>()?;
    let inodes = published_inodes(&published);
    let mut released = 0usize;
    for entry in std::fs::read_dir(&staging)? {
        let file = entry?.path();
        if is_published_twin(&inodes, &std::fs::symlink_metadata(&file)?) {
            std::fs::remove_file(&file)?;
            released += 1;
        }
    }
    if released > 0 {
        std::fs::File::open(&staging)?.sync_all()?;
    }
    Ok(released)
}

/// Counts the bytes an encoding would write, writing none of them.
struct CountingSink(u64);

impl Write for CountingSink {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self.0.saturating_add(bytes.len() as u64);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
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
    fn oversized_append_refuses_publication_before_staging_anything() {
        let root = tempfile::tempdir().unwrap();
        let path = private_directory(&root.path().join("journal")).unwrap();
        let value = json!({"original":"x".repeat(16 * 1024 * 1024)});
        let Err(error) = keep(&path, &value) else {
            panic!("oversized append must remain UNKNOWN");
        };
        assert!(error.to_string().contains("bound"), "{error:#}");
        assert!(originals(&path).unwrap().is_empty());
        // Judged before staging (daab12f4): a refused row is never written,
        // so a pass refused at the bound leaves nothing behind.
        assert_eq!(pending_files(&path), 0);
    }

    fn pending_files(path: &Path) -> usize {
        std::fs::read_dir(path.join("pending")).map_or(0, |entries| entries.count())
    }

    /// Plant `count` valid published originals directly, the shape keep()
    /// leaves, without paying a durable sync per row.
    fn plant_originals(path: &Path, count: usize, filler: usize) {
        use std::os::unix::fs::PermissionsExt;
        for sequence in 0..count {
            let raw = serde_json::to_vec(
                &json!({"observation":{"original":"x".repeat(filler)},"sequence":sequence}),
            )
            .unwrap();
            let digest = hex::encode(Sha256::digest(&raw));
            let file = path.join(format!("{sequence:020}-{digest}.json"));
            std::fs::write(&file, &raw).unwrap();
            // mode-bits-ok: private original journal fixture, never executable.
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    /// Backlog daab12f4 (measured 2026-10-17: 53 never-published files in
    /// one assessment's pending/, one more every ~2 minutes): a journal
    /// already at its file bound refused every later row only AFTER staging
    /// it, so each doomed pass left one more file behind. The bound is
    /// checked before anything is written, the refusal stays UNKNOWN, and
    /// however many passes are refused, pending/ does not grow.
    #[test]
    fn a_doomed_append_at_the_file_bound_refuses_without_staging_anything() {
        let root = tempfile::tempdir().unwrap();
        let path = private_directory(&root.path().join("journal")).unwrap();
        plant_originals(&path, MAX_READ_FILES, 1);
        for _ in 0..3 {
            let Err(error) = keep(&path, &json!({"phase":"native-invocation"})) else {
                panic!("an append past the file bound must remain UNKNOWN");
            };
            assert!(error.to_string().contains("bound"), "{error:#}");
        }
        assert_eq!(pending_files(&path), 0, "a refused pass left a staged file");
        assert_eq!(original_paths(&path).unwrap().len(), MAX_READ_FILES);
    }

    #[test]
    fn a_doomed_append_at_the_byte_bound_refuses_without_staging_anything() {
        let root = tempfile::tempdir().unwrap();
        let path = private_directory(&root.path().join("journal")).unwrap();
        plant_originals(&path, 1, MAX_READ_BYTES as usize - 100);
        let Err(error) = keep(
            &path,
            &json!({"phase":"native-invocation","pad":"x".repeat(200)}),
        ) else {
            panic!("an append past the byte bound must remain UNKNOWN");
        };
        assert!(error.to_string().contains("bound"), "{error:#}");
        assert_eq!(pending_files(&path), 0, "a refused pass left a staged file");
        assert_eq!(original_paths(&path).unwrap().len(), 1);
    }

    /// Evidence 2026-10-19 on daab12f4 (run b186ffca): every accepted row
    /// stayed in pending/ as a hard-linked twin of its published file, so a
    /// whole-journal snapshot counted each row twice and the effective read
    /// bound was half the declared one. Once the publication is durable the
    /// staged name is released; an interrupted write still stays.
    #[test]
    fn a_published_row_leaves_no_staged_twin() {
        let root = tempfile::tempdir().unwrap();
        let path = private_directory(&root.path().join("journal")).unwrap();
        for index in 0..3 {
            keep(&path, &json!({"row":index})).unwrap();
        }
        assert_eq!(originals(&path).unwrap().len(), 3);
        assert_eq!(
            pending_files(&path),
            0,
            "a published row kept a staged twin"
        );
    }

    /// Give every published row the staged twin a build before daab12f4
    /// left behind: a second name in pending/ for the same inode.
    fn plant_twins(path: &Path) -> usize {
        let staging = private_directory(&path.join("pending")).unwrap();
        let published = original_paths(path).unwrap();
        for (index, file) in published.iter().enumerate() {
            std::fs::hard_link(file, staging.join(format!("original-twin{index}"))).unwrap();
        }
        published.len()
    }

    /// A refused row the old order staged and never published: its own
    /// inode, bytes held nowhere else.
    fn plant_refused(path: &Path, name: &str, sequence: usize) -> Vec<u8> {
        use std::os::unix::fs::PermissionsExt;
        let raw =
            serde_json::to_vec(&json!({"observation":{"phase":"whoami"},"sequence":sequence}))
                .unwrap();
        let file = path.join("pending").join(name);
        std::fs::write(&file, &raw).unwrap();
        // mode-bits-ok: private original journal fixture, never executable.
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        raw
    }

    fn pending_names(path: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(path.join("pending"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    fn published_bytes(path: &Path) -> Vec<Vec<u8>> {
        original_paths(path)
            .unwrap()
            .iter()
            .map(|file| std::fs::read(file).unwrap())
            .collect()
    }

    /// Backlog 194cb548: daab12f4 stopped NEW twins, but a journal written
    /// by an earlier build keeps one in pending/ for every published row.
    /// The next append under the lock releases each such second name — no
    /// byte goes with it, the published name still links the inode — and
    /// leaves every pending entry that is not a twin exactly as it was.
    #[test]
    fn an_append_releases_twins_an_earlier_build_left_and_nothing_else() {
        let root = tempfile::tempdir().unwrap();
        let path = private_directory(&root.path().join("journal")).unwrap();
        plant_originals(&path, 3, 8);
        assert_eq!(plant_twins(&path), 3);
        let refused = plant_refused(&path, "original-refused", 3);
        let before = published_bytes(&path);
        keep(&path, &json!({"row":"after"})).unwrap();
        assert_eq!(pending_names(&path), vec!["original-refused".to_string()]);
        assert_eq!(
            std::fs::read(path.join("pending").join("original-refused")).unwrap(),
            refused,
            "a never-published entry is evidence and stays byte for byte"
        );
        let after = published_bytes(&path);
        assert_eq!(after.len(), 4);
        assert_eq!(after[..3], before[..], "a published row changed");
    }

    /// The measured shape (28e96fbc, run 4ec033fd): every row of a journal
    /// at its file bound has its twin, beside never-published refusals.
    /// The append still refuses — the bound is the bound — but the twins
    /// are released on that pass, so the journal stops carrying them.
    #[test]
    fn a_journal_at_its_bound_releases_its_twins_even_when_the_append_refuses() {
        let root = tempfile::tempdir().unwrap();
        let path = private_directory(&root.path().join("journal")).unwrap();
        plant_originals(&path, MAX_READ_FILES, 1);
        assert_eq!(plant_twins(&path), MAX_READ_FILES);
        plant_refused(&path, "original-refused-a", MAX_READ_FILES);
        plant_refused(&path, "original-refused-b", MAX_READ_FILES);
        let Err(error) = keep(&path, &json!({"phase":"whoami"})) else {
            panic!("an append past the file bound must remain UNKNOWN");
        };
        assert!(error.to_string().contains("bound"), "{error:#}");
        assert_eq!(
            pending_names(&path),
            vec![
                "original-refused-a".to_string(),
                "original-refused-b".to_string()
            ]
        );
        assert_eq!(original_paths(&path).unwrap().len(), MAX_READ_FILES);
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
