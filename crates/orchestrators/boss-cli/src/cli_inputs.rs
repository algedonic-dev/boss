//! Compiler input evidence beside the existing image CLI, never a new identity.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

const GENERATOR_SOURCES: [(&str, &[u8]); 2] = [
    (
        "crates/orchestrators/boss-cli/build.rs",
        include_bytes!("../build.rs"),
    ),
    (
        "crates/orchestrators/boss-cli/workflow_roster.rs",
        include_bytes!("../workflow_roster.rs"),
    ),
];

fn identity_trigger() -> PathBuf {
    Path::new("crates/orchestrators/boss-cli").join(crate::workflow_roster::IDENTITY_RERUN_TRIGGER)
}

fn bind_generator(
    root: &Path,
    files: &mut BTreeMap<String, Value>,
    directories: &mut BTreeSet<String>,
) -> Result<()> {
    for (path, bytes) in GENERATOR_SOURCES {
        source_input(root, &root.join(path), files, directories)?;
        if files[path]["sha256"] != hash(bytes) {
            bail!("compiler input generator differs from this reader: {path}");
        }
    }
    Ok(())
}

fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn dependency_paths(raw: &str) -> Result<Vec<String>> {
    let mut escaped = false;
    let mut colon = None;
    for (i, c) in raw.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if c == '\\' {
            escaped = true;
        } else if c == ':' {
            colon = Some(i);
            break;
        }
    }
    let mut chars = raw[colon.context("compiler dependency record has no target separator")? + 1..]
        .chars()
        .peekable();
    let mut paths = Vec::new();
    let mut word = String::new();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars
                .next()
                .context("truncated compiler dependency escape")?
            {
                '\n' => {}
                c => word.push(c),
            },
            '$' if chars.peek() == Some(&'$') => {
                chars.next();
                word.push('$');
            }
            '#' => bail!("unescaped comment in compiler dependency record"),
            c if c.is_whitespace() => {
                if !word.is_empty() {
                    paths.push(std::mem::take(&mut word));
                }
            }
            c => word.push(c),
        }
    }
    if !word.is_empty() {
        paths.push(word);
    }
    if paths.is_empty() {
        bail!("compiler dependency population is empty");
    }
    Ok(paths)
}

fn relative(root: &Path, path: &Path) -> Result<String> {
    let path = path
        .strip_prefix(root)
        .context("compiler input is outside its declared source")?;
    if path
        .components()
        .any(|p| !matches!(p, std::path::Component::Normal(_)))
    {
        bail!("compiler input has a non-canonical source path");
    }
    let text = path.to_str().context("compiler input path is not UTF-8")?;
    if text.is_empty() || text.chars().any(char::is_control) {
        bail!("compiler input path is empty or contains controls");
    }
    Ok(text.to_owned())
}

fn physical(path: &Path) -> Result<PathBuf> {
    let mut normalized = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::ParentDir => {
                if !normalized.pop() {
                    bail!("compiler input escapes its root");
                }
            }
            std::path::Component::CurDir => {}
            part => normalized.push(part.as_os_str()),
        }
    }
    let canonical = path
        .canonicalize()
        .with_context(|| format!("resolving compiler input {}", path.display()))?;
    if canonical != normalized {
        bail!(
            "compiler input traverses a linked parent: {}",
            path.display()
        );
    }
    Ok(canonical)
}

fn source_input(
    root: &Path,
    path: &Path,
    files: &mut BTreeMap<String, Value>,
    directories: &mut BTreeSet<String>,
) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("reading compiler input {}", path.display()))?;
    if metadata.file_type().is_symlink() {
        bail!("linked compiler input {}", path.display());
    }
    let canonical = physical(path)?;
    let path = canonical.as_path();
    let name = relative(root, path)?;
    if metadata.is_dir() {
        directories.insert(name);
        for entry in std::fs::read_dir(path)? {
            source_input(root, &entry?.path(), files, directories)?;
        }
    } else if metadata.is_file() {
        files.insert(
            name,
            json!({"sha256":hash(&std::fs::read(path)?),
            "mode":if metadata.permissions().mode() & 0o111 == 0 {"100644"} else {"100755"}}),
        );
    } else {
        bail!("non-regular compiler input {}", path.display());
    }
    Ok(())
}

/// A local build-side read. Failure retains the original stream and explicitly
/// disables reuse; it does not turn a diagnostic into publication authority.
pub(crate) async fn run(messages: &Path, artifacts: &Path, repo: Option<&Path>) -> Result<()> {
    let raw = tokio::fs::read_to_string(messages).await?;
    let current = std::env::current_dir()?;
    let root = repo.unwrap_or(&current);
    let mut receipt = match build_receipt(root, artifacts, &raw) {
        Ok(receipt) => receipt,
        Err(error) => {
            json!({"schema":1,"reusable":false,"error":format!("{error:#}"),"cargo_messages":raw})
        }
    };
    if receipt["reusable"] == true {
        let recipe = "infra/oss-quickstart/Dockerfile";
        match std::fs::read(root.join(recipe)) {
            Ok(bytes) => {
                receipt["files"][recipe] = json!({"sha256":hash(&bytes),"mode":"100644"});
            }
            Err(error) => {
                receipt["reusable"] = json!(false);
                receipt["error"] = json!(format!("image recipe unread: {error}"));
            }
        }
    }
    println!("{}", serde_json::to_string_pretty(&receipt)?);
    Ok(())
}

async fn compatible(head: &str, repo: Option<&Path>, reader: Option<&Path>) -> Result<Value> {
    if crate::built_from::COMPILED_FROM != "unknown" {
        bail!("only the published image CLI can judge supported image reuse");
    }
    let image = crate::built_from::built_from();
    if ![40, 64].contains(&image.len()) || !image.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("published image identity is unread");
    }
    let executable = std::env::current_exe()?.canonicalize()?;
    let receipt_path = executable
        .parent()
        .context("image CLI directory unread")?
        .join("boss-inputs.json");
    let metadata = std::fs::symlink_metadata(&receipt_path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        bail!("image compiler receipt is not regular");
    }
    let raw = tokio::fs::read_to_string(&receipt_path).await?;
    let receipt: Value = serde_json::from_str(&raw).context("image compiler receipt malformed")?;
    if receipt["binaries"]["boss"] != hash(&std::fs::read(&executable)?)
        || receipt
            .pointer("/files/infra~1oss-quickstart~1Dockerfile/sha256")
            .and_then(Value::as_str)
            .is_none()
    {
        bail!("image compiler receipt does not bind the real CLI and recipe");
    }
    let current = std::env::current_dir()?;
    let repo = repo.unwrap_or(&current);
    let plan = crate::delivery_plan::request(
        &json!({"operation":"git_plan","repo":repo,"base":image,"head":head}),
    )
    .await?;
    let mut report = json!({"schema":1,"compatible":false,"binary_image":image,
        "compatible_source":plan["head"],"plan":plan,
        "compiler_receipt":{"path":receipt_path,"bytes":raw.len(),"sha256":hash(raw.as_bytes())}});
    if plan["route"] != "data" {
        report["reason"] = json!("source change has no qualified lighter delivery plan");
        return Ok(report);
    }
    let head = plan["head"]
        .as_str()
        .context("compatible source identity unread")?;
    if !matches_source(repo, head, &receipt).await? {
        report["reason"] = json!("actual compiler inputs differ from the published image");
        return Ok(report);
    }
    report["compiler_inputs_equal"] = json!(true);
    let observation = crate::delivery_plan::observe_with_reader(
        repo,
        &plan,
        head,
        &crate::sor_client::jobs_base()?,
        reader,
    )
    .await?;
    report["compatible"] = json!(observation["arrived"] == true);
    report["delivery_evidence"] = observation;
    Ok(report)
}

pub(crate) async fn run_compatible(
    head: &str,
    repo: Option<&Path>,
    reader: Option<&Path>,
) -> Result<()> {
    let report = match compatible(head, repo, reader).await {
        Ok(report) => report,
        Err(error) => {
            json!({"schema":1,"compatible":false,"requested_source":head,"error":format!("{error:#}")})
        }
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

/// Cargo's complete dependency records, not an inferred file-label allowlist.
/// Original compiler messages and dep-info stay in the receipt. Generated
/// compiler inputs are retained separately; their declared source dependencies
/// are Cargo's directory/file entries in the same dep-info records.
pub(crate) fn build_receipt(repo: &Path, artifacts: &Path, messages: &str) -> Result<Value> {
    let root = repo.canonicalize()?;
    let artifacts = artifacts.canonicalize()?;
    let records: Vec<Value> = messages
        .lines()
        .map(serde_json::from_str)
        .collect::<std::result::Result<_, _>>()
        .context("Cargo message stream is malformed")?;
    let finished: Vec<_> = records
        .iter()
        .filter(|r| r["reason"] == "build-finished")
        .collect();
    if finished.is_empty() || finished.iter().any(|r| r["success"] != true) {
        bail!("Cargo did not record a successful build");
    }
    let mut binaries = BTreeMap::new();
    let mut dep_info = BTreeMap::new();
    let mut files = BTreeMap::new();
    let mut directories = BTreeSet::new();
    let mut generated = BTreeMap::new();
    let mut absent = BTreeSet::new();
    let mut direct_workflow_input = false;
    source_input(
        &root,
        &root.join("Cargo.toml"),
        &mut files,
        &mut directories,
    )?;
    source_input(
        &root,
        &root.join("Cargo.lock"),
        &mut files,
        &mut directories,
    )?;
    for record in &records {
        if record["reason"] != "compiler-artifact" {
            continue;
        }
        if let Some(manifest) = record["manifest_path"].as_str() {
            let manifest = Path::new(manifest);
            if manifest.starts_with(&root) {
                source_input(&root, manifest, &mut files, &mut directories)?;
            }
        }
        let Some(executable) = record["executable"].as_str() else {
            continue;
        };
        let executable = Path::new(executable);
        let Some(name) = executable.file_name().and_then(|s| s.to_str()) else {
            bail!("compiler executable name unread");
        };
        if name != "boss" && !name.starts_with("boss-") {
            continue;
        }
        let binary = artifacts.join(name);
        if !binary.is_file() || std::fs::symlink_metadata(&binary)?.file_type().is_symlink() {
            bail!("published binary {name} is absent or linked");
        }
        let bytes = std::fs::read(&binary)?;
        if bytes != std::fs::read(executable)? {
            bail!("published binary {name} differs from Cargo's artifact");
        }
        binaries.insert(name.to_owned(), hash(&bytes));
        let raw = std::fs::read_to_string(executable.with_extension("d"))
            .with_context(|| format!("reading complete Cargo dep-info for {name}"))?;
        let profile = executable
            .parent()
            .context("compiler artifact profile unread")?
            .canonicalize()?;
        for dependency in dependency_paths(&raw)? {
            let path = PathBuf::from(dependency);
            let path = if path.is_absolute() {
                path
            } else {
                root.join(path)
            };
            if path
                .file_name()
                .is_some_and(|name| name == crate::workflow_roster::IDENTITY_RERUN_TRIGGER)
                && path.parent().is_some_and(|parent| {
                    physical(parent)
                        .is_ok_and(|parent| parent == root.join("crates/orchestrators/boss-cli"))
                })
                && std::fs::symlink_metadata(&path)
                    .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
            {
                bind_generator(&root, &mut files, &mut directories)?;
                absent.insert(identity_trigger());
                continue;
            }
            let path = physical(&path)?;
            if path.starts_with(&root) && !path.starts_with(profile.join("build")) {
                direct_workflow_input |=
                    path.is_file() && path.starts_with(root.join("infra/platform/workflows"));
                source_input(&root, &path, &mut files, &mut directories)?;
            } else if path.starts_with(profile.join("build")) {
                if !path.components().any(|c| c.as_os_str() == "out")
                    || std::fs::symlink_metadata(&path)?.file_type().is_symlink()
                {
                    bail!(
                        "generated compiler input is outside an output directory: {}",
                        path.display()
                    );
                }
                generated.insert(
                    path.to_string_lossy().into_owned(),
                    hash(&std::fs::read(&path)?),
                );
            } else {
                bail!("unbound external compiler input {}", path.display());
            }
        }
        dep_info.insert(name.to_owned(), raw);
    }
    if !binaries.contains_key("boss") {
        bail!("Cargo recorded no installed CLI artifact");
    }
    // An unreported executable left in a build cache is not current-tree proof.
    for entry in std::fs::read_dir(&artifacts)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let metadata = entry.metadata()?;
        if (name == "boss" || name.starts_with("boss-"))
            && metadata.is_file()
            && metadata.permissions().mode() & 0o111 != 0
            && !binaries.contains_key(&name)
        {
            bail!("published executable {name} is absent from Cargo's artifact record");
        }
    }
    let mut projection = Value::Null;
    if directories.contains("infra/platform/workflows") && !direct_workflow_input {
        bind_generator(&root, &mut files, &mut directories)?;
        let outputs = generated
            .keys()
            .filter(|path| {
                Path::new(path)
                    .file_name()
                    .is_some_and(|name| name == "platform_workflow_kinds.rs")
            })
            .collect::<Vec<_>>();
        if outputs.len() != 1 {
            bail!("workflow roster has no unique generated compiler input");
        }
        let generated_text = std::fs::read_to_string(outputs[0])?;
        let names = std::fs::read_dir(root.join("infra/platform/workflows"))?
            .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
            .collect::<std::io::Result<Vec<_>>>()?;
        if crate::workflow_roster::render(&names).map_err(anyhow::Error::msg)? != generated_text {
            bail!("workflow roster output differs from its actual compiler input");
        }
        projection = json!({"directory":"infra/platform/workflows","generated":generated_text});
    }
    Ok(
        json!({"schema":1,"reusable":true,"binaries":binaries,"files":files,
        "directories":directories,"generated_inputs":generated,
        "workflow_roster":projection,
        "absent_inputs":absent,
        "original_dep_info":dep_info,"cargo_messages":messages}),
    )
}

pub(crate) async fn matches_source(repo: &Path, head: &str, receipt: &Value) -> Result<bool> {
    if receipt["schema"] != 1 || receipt["reusable"] != true {
        return Ok(false);
    }
    let files = receipt["files"]
        .as_object()
        .context("compiler input file population unread")?;
    if files.is_empty() {
        return Ok(false);
    }
    let git = |args: Vec<String>| async move {
        let borrowed: Vec<_> = args.iter().map(String::as_str).collect();
        crate::delivery_plan::git_bytes(repo, &borrowed).await
    };
    if head.starts_with('-') || head.chars().any(char::is_control) {
        bail!("compiler input source ref malformed");
    }
    let frozen = String::from_utf8(
        git(vec![
            "rev-parse".into(),
            "--verify".into(),
            format!("{head}^{{commit}}"),
        ])
        .await?,
    )?;
    let head = frozen.trim();
    if !matches!(head.len(), 40 | 64) || !head.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("compiler input source identity unread");
    }
    let projection = receipt
        .get("workflow_roster")
        .filter(|value| value.is_object());
    if let Some(projection) = projection {
        if !roster_matches(repo, head, projection).await? {
            return Ok(false);
        }
        for (path, _) in GENERATOR_SOURCES {
            if !files.contains_key(path) {
                bail!("compiler roster generator source is absent");
            }
        }
    }
    for absent in receipt["absent_inputs"]
        .as_array()
        .context("compiler input absence population unread")?
    {
        let path = absent
            .as_str()
            .context("compiler input absence path unread")?;
        if Path::new(path) != identity_trigger()
            || GENERATOR_SOURCES
                .iter()
                .any(|(source, _)| !files.contains_key(*source))
        {
            bail!("unknown compiler input absence contract");
        }
        let raw = git(vec![
            "ls-tree".into(),
            "-z".into(),
            head.into(),
            "--".into(),
            format!(":(literal){path}"),
        ])
        .await?;
        if !raw.is_empty() {
            return Ok(false);
        }
    }
    for (path, input) in files {
        relative(Path::new(""), Path::new(path))?;
        if projection.is_some() && path.starts_with("infra/platform/workflows/") {
            continue;
        }
        let bytes = git(vec!["show".into(), format!("{head}:{path}")]).await?;
        if input["sha256"] != hash(&bytes) {
            return Ok(false);
        }
        let raw = git(vec![
            "ls-tree".into(),
            "-z".into(),
            head.into(),
            "--".into(),
            path.clone(),
        ])
        .await?;
        let text = std::str::from_utf8(&raw)?;
        if text.split_whitespace().next() != input["mode"].as_str() {
            return Ok(false);
        }
    }
    for directory in receipt["directories"]
        .as_array()
        .context("compiler input directories unread")?
    {
        let directory = directory
            .as_str()
            .context("compiler input directory unread")?;
        if projection.is_some() && directory == "infra/platform/workflows" {
            continue;
        }
        relative(Path::new(""), Path::new(directory))?;
        let raw = git(vec![
            "ls-tree".into(),
            "-r".into(),
            "--name-only".into(),
            "-z".into(),
            head.into(),
            "--".into(),
            directory.into(),
        ])
        .await?;
        let observed: BTreeSet<_> = std::str::from_utf8(&raw)?
            .split('\0')
            .filter(|s| !s.is_empty())
            .collect();
        let prefix = format!("{directory}/");
        let expected: BTreeSet<_> = files
            .keys()
            .filter(|p| p.starts_with(&prefix))
            .map(String::as_str)
            .collect();
        if observed != expected {
            return Ok(false);
        }
    }
    Ok(true)
}

async fn roster_matches(repo: &Path, head: &str, projection: &Value) -> Result<bool> {
    if projection["directory"] != "infra/platform/workflows" {
        bail!("unknown compiler directory projection");
    }
    let raw = crate::delivery_plan::git_bytes(
        repo,
        &[
            "ls-tree",
            "-z",
            head,
            "--",
            ":(literal)infra/platform/workflows/",
        ],
    )
    .await?;
    // ls-tree without recursion names precisely the immediate roster entries.
    let text = std::str::from_utf8(&raw)?;
    let mut names = Vec::new();
    for entry in text.split('\0').filter(|entry| !entry.is_empty()) {
        let (metadata, path) = entry
            .split_once('\t')
            .context("compiler roster tree entry malformed")?;
        if !metadata.starts_with("100644 blob ") {
            return Ok(false);
        }
        let name = path
            .strip_prefix("infra/platform/workflows/")
            .context("compiler roster path escaped its directory")?;
        if name.contains('/') {
            return Ok(false);
        }
        names.push(name.to_owned());
    }
    let rendered = crate::workflow_roster::render(&names).map_err(anyhow::Error::msg)?;
    Ok(projection["generated"].as_str() == Some(rendered.as_str()))
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn a_candidate_cannot_qualify_itself_as_the_supported_image_cli() {
        if crate::built_from::COMPILED_FROM != "unknown" {
            let error = compatible("HEAD", None, None).await.unwrap_err();
            assert!(error.to_string().contains("published image"), "{error:#}");
        }
    }
    use super::*;
    use crate::car_unland::tests::git_in;
    use serde_json::json;

    #[test]
    fn roster_projection_is_the_actual_compiler_input() {
        let directory =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../infra/platform/workflows");
        let names = std::fs::read_dir(directory)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            crate::workflow_roster::render(&names).unwrap(),
            include_str!(concat!(env!("OUT_DIR"), "/platform_workflow_kinds.rs"))
        );
        assert_eq!(
            crate::workflow_roster::render(&["z.toml".into(), "a.toml".into(), "README.md".into()])
                .unwrap(),
            crate::workflow_roster::render(&["a.toml".into(), "z.toml".into()]).unwrap()
        );
        assert!(crate::workflow_roster::render(&["README.md".into()]).is_err());
    }

    #[tokio::test]
    async fn workflow_body_changes_preserve_the_compiled_roster_but_new_kinds_do_not() {
        let root = boss_testing::scratch::scratch_dir("cli-roster-source");
        std::fs::create_dir_all(root.join("infra/platform/workflows")).unwrap();
        git_in(&root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("infra/platform/workflows/a.toml"), "body one").unwrap();
        git_in(&root, &["add", "."]);
        git_in(&root, &["commit", "-qm", "one kind"]);
        let projection = json!({"directory":"infra/platform/workflows",
            "generated":crate::workflow_roster::render(&["a.toml".into()]).unwrap()});
        assert!(roster_matches(&root, "HEAD", &projection).await.unwrap());
        std::fs::write(root.join("infra/platform/workflows/a.toml"), "body two").unwrap();
        git_in(&root, &["add", "."]);
        git_in(&root, &["commit", "-qm", "edit body"]);
        assert!(roster_matches(&root, "HEAD", &projection).await.unwrap());
        std::fs::write(root.join("infra/platform/workflows/b.toml"), "new kind").unwrap();
        git_in(&root, &["add", "."]);
        git_in(&root, &["commit", "-qm", "add kind"]);
        assert!(!roster_matches(&root, "HEAD", &projection).await.unwrap());
    }

    #[tokio::test]
    async fn actual_receipt_preserves_roster_originals_and_never_projects_a_direct_body_input() {
        let root = boss_testing::scratch::scratch_dir("cli-roster-receipt");
        let repo = root.join("source");
        let artifacts = root.join("target/debug");
        let generated = artifacts.join("build/boss-cli-unit/out/platform_workflow_kinds.rs");
        std::fs::create_dir_all(generated.parent().unwrap()).unwrap();
        std::fs::create_dir_all(repo.join("infra/platform/workflows")).unwrap();
        std::fs::create_dir_all(repo.join("crates/orchestrators/boss-cli")).unwrap();
        for (path, bytes) in [
            ("Cargo.toml", b"workspace".as_slice()),
            ("Cargo.lock", b"locked".as_slice()),
            (
                "crates/orchestrators/boss-cli/build.rs",
                include_bytes!("../build.rs").as_slice(),
            ),
            (
                "crates/orchestrators/boss-cli/workflow_roster.rs",
                include_bytes!("../workflow_roster.rs").as_slice(),
            ),
            (
                "infra/platform/workflows/a.toml",
                b"original body".as_slice(),
            ),
        ] {
            std::fs::write(repo.join(path), bytes).unwrap();
        }
        let compiled = crate::workflow_roster::render(&["a.toml".into()]).unwrap();
        std::fs::write(&generated, &compiled).unwrap();
        let binary = artifacts.join("boss");
        boss_testing::write_exec(&binary, "compiled CLI");
        let dep = format!(
            "{}: {} {}\n",
            binary.display(),
            repo.join("infra/platform/workflows").display(),
            generated.display()
        );
        std::fs::write(binary.with_extension("d"), &dep).unwrap();
        let messages = format!(
            "{}\n{}\n",
            json!({"reason":"compiler-artifact","executable":binary}),
            json!({"reason":"build-finished","success":true})
        );
        let receipt = build_receipt(&repo, &artifacts, &messages).unwrap();
        assert_eq!(receipt["workflow_roster"]["generated"], compiled);
        assert_eq!(receipt["original_dep_info"]["boss"], dep);
        assert_eq!(
            receipt["files"]["infra/platform/workflows/a.toml"]["sha256"],
            hash(b"original body")
        );
        let direct = format!(
            "{}: {} {} {}\n",
            binary.display(),
            repo.join("infra/platform/workflows").display(),
            generated.display(),
            repo.join("infra/platform/workflows/a.toml").display()
        );
        std::fs::write(binary.with_extension("d"), direct).unwrap();
        assert!(build_receipt(&repo, &artifacts, &messages).unwrap()["workflow_roster"].is_null());
        git_in(&repo, &["init", "-q", "-b", "main"]);
        git_in(&repo, &["add", "."]);
        git_in(&repo, &["commit", "-qm", "original compiler inputs"]);
        assert!(matches_source(&repo, "HEAD", &receipt).await.unwrap());
        std::fs::write(
            repo.join("infra/platform/workflows/a.toml"),
            "new registry body",
        )
        .unwrap();
        git_in(&repo, &["add", "."]);
        git_in(&repo, &["commit", "-qm", "registry body"]);
        assert!(matches_source(&repo, "HEAD", &receipt).await.unwrap());
        std::fs::write(repo.join("infra/platform/workflows/b.toml"), "new kind").unwrap();
        git_in(&repo, &["add", "."]);
        git_in(&repo, &["commit", "-qm", "new roster kind"]);
        assert!(!matches_source(&repo, "HEAD", &receipt).await.unwrap());
    }

    #[tokio::test]
    async fn actual_file_and_directory_inputs_bind_reuse_without_relabeling_the_binary() {
        let root = boss_testing::scratch::scratch_dir("cli-input-receipt");
        let repo = root.join("source");
        let artifacts = root.join("target/debug");
        std::fs::create_dir_all(repo.join("crates/example/src")).unwrap();
        std::fs::create_dir_all(repo.join("infra/inputs")).unwrap();
        std::fs::create_dir_all(repo.join("crates/orchestrators/boss-cli")).unwrap();
        std::fs::write(
            repo.join("crates/orchestrators/boss-cli/build.rs"),
            include_bytes!("../build.rs"),
        )
        .unwrap();
        std::fs::write(
            repo.join("crates/orchestrators/boss-cli/workflow_roster.rs"),
            include_bytes!("../workflow_roster.rs"),
        )
        .unwrap();
        std::fs::create_dir_all(&artifacts).unwrap();
        git_in(&repo, &["init", "-q", "-b", "main"]);
        for (path, body) in [
            ("Cargo.toml", "[workspace]\nmembers=[]\n"),
            ("Cargo.lock", "locked\n"),
            (
                "crates/example/Cargo.toml",
                "[package]\nname='example'\nversion='0.1.0'\n",
            ),
            ("crates/example/src/lib.rs", "pub fn actual() {}\n"),
            ("infra/inputs/with space.txt", "input\n"),
        ] {
            std::fs::write(repo.join(path), body).unwrap();
        }
        git_in(&repo, &["add", "."]);
        git_in(&repo, &["commit", "-qm", "compiler inputs"]);
        let binary = artifacts.join("boss");
        boss_testing::write_exec(&binary, "actual compiled bytes");
        let trigger =
            repo.join("crates/orchestrators/boss-cli/.boss-built-from-is-recomputed-every-build");
        let dep = format!(
            "{}: {} {} {}\n",
            binary.display(),
            repo.join("crates/example/src/../src/lib.rs").display(),
            repo.join("infra/inputs").display(),
            trigger.display()
        );
        std::fs::write(artifacts.join("boss.d"), &dep).unwrap();
        let messages = format!(
            "{}\n{}\n",
            json!({"reason":"compiler-artifact","executable":binary,
            "manifest_path":repo.join("crates/example/Cargo.toml"),"package_id":"example"}),
            json!({"reason":"build-finished","success":true})
        );
        let receipt = build_receipt(&repo, &artifacts, &messages).unwrap();
        assert_eq!(receipt["original_dep_info"]["boss"], dep);
        assert_eq!(receipt["cargo_messages"], messages);
        assert!(matches_source(&repo, "HEAD", &receipt).await.unwrap());
        assert_eq!(
            receipt["absent_inputs"][0],
            "crates/orchestrators/boss-cli/.boss-built-from-is-recomputed-every-build"
        );
        std::fs::write(
            binary.with_extension("d"),
            format!(
                "{} {}\n",
                dep.trim_end(),
                repo.join("unread-input.rs").display()
            ),
        )
        .unwrap();
        assert!(
            build_receipt(&repo, &artifacts, &messages).is_err(),
            "only the bound rerun trigger may be absent"
        );
        std::fs::write(binary.with_extension("d"), &dep).unwrap();
        std::fs::write(&trigger, "now present").unwrap();
        git_in(&repo, &["add", "."]);
        git_in(&repo, &["commit", "-qm", "rerun trigger became present"]);
        assert!(!matches_source(&repo, "HEAD", &receipt).await.unwrap());
        std::fs::remove_file(&trigger).unwrap();
        git_in(&repo, &["add", "."]);
        git_in(&repo, &["commit", "-qm", "restore rerun trigger absence"]);
        let failed = format!(
            "{messages}{}\n",
            json!({"reason":"build-finished","success":false})
        );
        assert!(build_receipt(&repo, &artifacts, &failed).is_err());
        std::fs::create_dir_all(repo.join("docs")).unwrap();
        std::fs::write(repo.join("docs/arrived.md"), "published documentation\n").unwrap();
        git_in(&repo, &["add", "."]);
        git_in(&repo, &["commit", "-qm", "docs"]);
        assert!(matches_source(&repo, "HEAD", &receipt).await.unwrap());
        std::fs::write(
            repo.join("crates/example/Cargo.toml"),
            "[package]\nname='example'\nversion='0.2.0'\n",
        )
        .unwrap();
        git_in(&repo, &["add", "."]);
        git_in(&repo, &["commit", "-qm", "changed compiler package"]);
        assert!(!matches_source(&repo, "HEAD", &receipt).await.unwrap());
        std::fs::write(
            repo.join("crates/example/Cargo.toml"),
            "[package]\nname='example'\nversion='0.1.0'\n",
        )
        .unwrap();
        git_in(&repo, &["add", "."]);
        git_in(&repo, &["commit", "-qm", "restore compiler package"]);
        std::fs::write(repo.join("infra/inputs/new.txt"), "new compiler input\n").unwrap();
        git_in(&repo, &["add", "."]);
        git_in(&repo, &["commit", "-qm", "changed input population"]);
        assert!(!matches_source(&repo, "HEAD", &receipt).await.unwrap());
        std::fs::remove_file(repo.join("infra/inputs/new.txt")).unwrap();
        std::fs::write(
            repo.join("crates/example/src/lib.rs"),
            "pub fn changed() {}\n",
        )
        .unwrap();
        git_in(&repo, &["add", "."]);
        git_in(&repo, &["commit", "-qm", "changed code"]);
        assert!(!matches_source(&repo, "HEAD", &receipt).await.unwrap());
        assert_eq!(std::fs::read(&binary).unwrap(), b"actual compiled bytes");
        // Optional local integration witness after an ordinary frozen Cargo
        // build. The fixture controls above always run; this never executes the
        // candidate CLI or grants image/source qualification.
        match (
            std::env::var_os("BOSS_TEST_CARGO_STREAM"),
            std::env::var_os("BOSS_TEST_CARGO_ARTIFACTS"),
        ) {
            (Some(stream), Some(artifacts)) => {
                let source = Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../..")
                    .canonicalize()
                    .unwrap();
                let raw = std::fs::read_to_string(&stream).unwrap();
                let actual = build_receipt(&source, Path::new(&artifacts), &raw).unwrap();
                assert!(
                    matches_source(&source, "HEAD", &actual).await.unwrap(),
                    "actual compiler inputs must bind the frozen committed source"
                );
                assert_eq!(actual["cargo_messages"], raw);
                assert!(!actual["files"].as_object().unwrap().is_empty());
                assert_eq!(
                    actual["binaries"]["boss"],
                    hash(&std::fs::read(Path::new(&artifacts).join("boss")).unwrap())
                );
                std::fs::write(
                    Path::new(&stream).with_extension("receipt.json"),
                    serde_json::to_vec_pretty(&actual).unwrap(),
                )
                .unwrap();
            }
            (None, None) => {}
            _ => panic!(
                "the actual Cargo witness requires both original stream and artifact directory"
            ),
        }
    }

    #[test]
    fn dependency_decoding_keeps_escaped_paths_and_refuses_truncated_records() {
        assert_eq!(
            dependency_paths(
                "/target/boss: /source/a\\ b /source/hash\\#file /source/back\\\\slash\n"
            )
            .unwrap(),
            vec!["/source/a b", "/source/hash#file", "/source/back\\slash"]
        );
        assert!(dependency_paths("not a dependency record").is_err());
        assert!(dependency_paths("/target/boss: /source/dangling\\").is_err());
    }

    #[test]
    fn a_linked_parent_cannot_supply_an_unbound_file_as_a_repository_input() {
        let root = boss_testing::scratch::scratch_dir("cli-input-link");
        let repo = root.join("source");
        let outside = root.join("outside");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("not-source.txt"), "controlled external input").unwrap();
        std::os::unix::fs::symlink(&outside, repo.join("alias")).unwrap();
        assert!(
            source_input(
                &repo,
                &repo.join("alias/not-source.txt"),
                &mut BTreeMap::new(),
                &mut BTreeSet::new()
            )
            .is_err()
        );
    }
}
