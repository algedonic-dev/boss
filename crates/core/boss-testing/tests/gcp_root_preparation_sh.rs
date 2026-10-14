//! Preparation itself never adopts units or substitutes untrusted source.
// not a tree-wide pin: copies only the GCP helper's fixed infra subtrees into its owning fixture.
use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &to.join(entry.file_name()));
        } else if entry.file_type().unwrap().is_file() {
            std::fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
        }
    }
}

struct Fixture {
    dir: PathBuf,
    source: PathBuf,
    prepared: PathBuf,
    tree: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let dir = scratch_dir("gcp-root-preparation");
        let source = dir.join("source/infra");
        for name in ["gcp", "lib", "estate"] {
            copy_dir(&repo_root().join("infra").join(name), &source.join(name));
        }
        Self {
            prepared: dir.join("prepared"),
            tree: dir.join("tree"),
            dir,
            source,
        }
    }
    fn run(&self, args: &[&str]) -> Output {
        Command::new("bash")
            .arg(repo_root().join("infra/gcp/prepare-root-tree.sh"))
            .args(args)
            .env("BOSS_GCP_PREP_SOURCE", &self.source)
            .env("BOSS_GCP_PREPARED_DIR", &self.prepared)
            .env("BOSS_GCP_ROOT_TREE", &self.tree)
            .env(
                "BOSS_GCP_ROOT_TREE_TOKEN",
                self.dir.join("never-read-token"),
            )
            .output()
            .unwrap()
    }
    fn prepare(&self) {
        let out = self.run(&["bootstrap"]);
        assert!(out.status.success(), "{out:?}");
    }
    fn git(&self, dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=fixture",
                "-c",
                "user.email=fixture@example.invalid",
            ])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        String::from_utf8_lossy(&out.stdout).trim().into()
    }
    fn accepted(&self) -> String {
        // Synthetic already-accepted state; actual authenticated fetch is
        // exercised by the adjacent gcp_root_tree_sh owning fixture.
        let repo = self.dir.join("accepted-source");
        std::fs::create_dir(&repo).unwrap();
        self.git(&repo, &["init", "-q"]);
        write_file(&repo.join("accepted"), "source\n");
        copy_dir(
            &self.source.join("gcp/root-tree-units"),
            &repo.join("infra/gcp/root-tree-units"),
        );
        self.git(&repo, &["add", "."]);
        self.git(&repo, &["commit", "-qm", "accepted"]);
        let sha = self.git(&repo, &["rev-parse", "HEAD"]);
        std::fs::create_dir_all(self.tree.join("gen")).unwrap();
        let store = self.tree.join("repo.git");
        self.git(
            &repo,
            &["clone", "-q", "--bare", ".", store.to_str().unwrap()],
        );
        self.git(&store, &["update-ref", "refs/boss/accepted", &sha]);
        let generation = self.tree.join("gen").join(&sha);
        self.git(&repo, &["clone", "-q", ".", generation.to_str().unwrap()]);
        write_file(&generation.join(".boss-tree-sha"), &sha);
        write_file(&self.tree.join(".lock"), "");
        std::os::unix::fs::symlink(format!("gen/{sha}"), self.tree.join("current")).unwrap();
        sha
    }
}

#[test]
fn bootstrap_prepares_the_fixed_closure_without_credentials_or_unit_adoption() {
    let f = Fixture::new();
    std::fs::write(f.dir.join("never-read-token"), "sentinel\n").unwrap();
    f.prepare();
    let bootstrap = f.prepared.join("bootstrap/infra");
    for path in [
        "gcp/prepare-root-tree.sh",
        "gcp/root-tree.sh",
        "gcp/root-tree-credential.sh",
        "gcp/gcp-root-launch.sh",
        "lib/host-check.sh",
        "lib/sor.sh",
        "lib/sor-reader.sh",
        "estate/estate.toml",
    ] {
        assert_eq!(
            std::fs::read(bootstrap.join(path)).unwrap(),
            std::fs::read(f.source.join(path)).unwrap()
        );
    }
    assert!(
        !f.tree.exists(),
        "preparation never fetches or creates a tree"
    );
    assert!(!f.prepared.join("units").exists());
    assert_eq!(
        std::fs::read_to_string(f.dir.join("never-read-token")).unwrap(),
        "sentinel\n"
    );
    // The integration now calls preparation before installing. This component
    // still only publishes private files; its behavioral checks above own that
    // boundary. The adoption fixture owns the caller and installation ordering.
}

#[test]
fn accepted_readiness_prepares_four_units_and_refuses_unaccepted_or_moved_heads() {
    let f = Fixture::new();
    f.prepare();
    let sha = f.accepted();
    let out = f.run(&["units", &sha]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        std::fs::read_to_string(f.prepared.join("units/accepted-sha"))
            .unwrap()
            .trim(),
        sha
    );
    let mut count = 0;
    for entry in std::fs::read_dir(f.source.join("gcp/root-tree-units")).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(stem) = name.strip_suffix(".in") {
            assert_eq!(
                std::fs::read(f.prepared.join("units").join(stem)).unwrap(),
                std::fs::read(entry.path()).unwrap()
            );
            count += 1;
        }
    }
    assert_eq!(count, 4);
    assert!(!f.run(&["units", &"b".repeat(40)]).status.success());
    f.git(
        &f.tree.join("repo.git"),
        &["update-ref", "-d", "refs/boss/accepted"],
    );
    assert!(!f.run(&["units", &sha]).status.success());
    assert_eq!(
        std::fs::read_to_string(f.prepared.join("units/accepted-sha"))
            .unwrap()
            .trim(),
        sha
    );
}

#[test]
fn linked_source_and_replaceable_source_ancestry_never_create_prepared_files() {
    let f = Fixture::new();
    let entry = f.source.join("gcp/root-tree.sh");
    std::fs::remove_file(&entry).unwrap();
    std::os::unix::fs::symlink(repo_root().join("infra/gcp/root-tree.sh"), entry).unwrap();
    assert!(!f.run(&["bootstrap"]).status.success());
    assert!(!f.prepared.exists());
    let f = Fixture::new();
    std::fs::set_permissions(
        f.source.parent().unwrap(),
        // mode-bits-ok: unsafe fixture ancestry, never executed.
        std::fs::Permissions::from_mode(0o777),
    )
    .unwrap();
    assert!(!f.run(&["bootstrap"]).status.success());
    assert!(!f.prepared.exists());
}

#[test]
fn another_owner_cannot_supply_bootstrap_bytes() {
    let f = Fixture::new();
    let file = f.source.join("gcp/root-tree.sh");
    if std::fs::metadata(&f.dir).unwrap().uid() == 0 {
        let foreign = f.dir.join("foreign");
        std::fs::create_dir(&foreign).unwrap();
        // mode-bits-ok: another uid creates a synthetic file here; no executable runs from it.
        std::fs::set_permissions(&foreign, std::fs::Permissions::from_mode(0o777)).unwrap();
        let out = Command::new("setpriv")
            .args([
                "--reuid=65534",
                "--regid=65534",
                "--clear-groups",
                "sh",
                "-c",
                "printf 'foreign source\\n' > \"$1\"",
                "sh",
            ])
            .arg(foreign.join("source"))
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        std::fs::rename(foreign.join("source"), &file).unwrap();
        assert!(!f.run(&["bootstrap"]).status.success());
    } else {
        // Gate uid cannot mint another owner; exercise the same stat boundary.
        let bin = f.dir.join("bin");
        std::fs::create_dir(&bin).unwrap();
        write_exec(
            &bin.join("stat"),
            &format!(
                "#!/bin/bash\nif [ \"$2\" = %u ] && [ \"${{@: -1}}\" = '{}' ]; then echo 0; else exec /usr/bin/stat \"$@\"; fi\n",
                file.display()
            ),
        );
        let out = Command::new("bash")
            .arg(repo_root().join("infra/gcp/prepare-root-tree.sh"))
            .arg("bootstrap")
            .env("BOSS_GCP_PREP_SOURCE", &f.source)
            .env("BOSS_GCP_PREPARED_DIR", &f.prepared)
            .env(
                "PATH",
                format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
            )
            .output()
            .unwrap();
        assert!(!out.status.success(), "{out:?}");
    }
    assert!(!f.prepared.exists());
}

#[test]
fn prepared_templates_must_equal_the_accepted_generation() {
    let f = Fixture::new();
    f.prepare();
    let sha = f.accepted();
    let template = f
        .prepared
        .join("bootstrap/infra/gcp/root-tree-units/boss-surface-usage.service.in");
    let original = std::fs::read_to_string(&template).unwrap();
    write_file(
        &template,
        &format!("{original}\n# a different prepared source\n"),
    );
    let out = f.run(&["units", &sha]);
    assert!(
        !out.status.success(),
        "readiness must bind prepared bytes to accepted source: {out:?}"
    );
    assert!(!f.prepared.join("units").exists());
}

#[test]
fn untrusted_source_or_destination_preserves_previous_preparation() {
    let f = Fixture::new();
    f.prepare();
    let old = std::fs::read(f.prepared.join("bootstrap/infra/gcp/root-tree.sh")).unwrap();
    std::fs::set_permissions(
        f.source.join("gcp/root-tree.sh"),
        // mode-bits-ok: deliberately unsafe source file, never executed.
        std::fs::Permissions::from_mode(0o777),
    )
    .unwrap();
    assert!(!f.run(&["bootstrap"]).status.success());
    assert_eq!(
        std::fs::read(f.prepared.join("bootstrap/infra/gcp/root-tree.sh")).unwrap(),
        old
    );
    let f = Fixture::new();
    let outside = f.dir.join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, &f.prepared).unwrap();
    assert!(!f.run(&["bootstrap"]).status.success());
    assert!(std::fs::read_dir(outside).unwrap().next().is_none());
}

#[test]
fn units_require_an_exact_accepted_owned_generation_before_any_output() {
    let f = Fixture::new();
    f.prepare();
    let out = f.run(&["units", &"a".repeat(40)]);
    assert!(
        !out.status.success(),
        "missing authenticated accepted tree: {out:?}"
    );
    assert!(!f.prepared.join("units").exists());
    let out = f.run(&["units", "not-a-sha"]);
    assert!(!out.status.success());
    assert!(!f.prepared.join("units").exists());
}
