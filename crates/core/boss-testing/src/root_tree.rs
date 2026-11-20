//! A fixture of ROOT'S TREE, in the shape `infra/forge/root-tree.sh`
//! keeps it on the forge: a bare store `rt/repo.git` and one exported
//! generation per commit, `rt/gen/<sha>`, stamped with `.boss-tree-sha`.
//!
//! WHY THIS IS SHARED (review a04e64e9 P1, 2026-11-15). Every verb that
//! derives what root may delete from a tree's files proves that tree is
//! such a generation (`infra/lib/root-generation.sh`): delete-orphan-object
//! since backlog 1dde630c, and retire-volume-claim and retire-access-object
//! since that review. So all three suites need the same fixture, and it
//! lived in `delete_orphan_object_sh.rs` alone; CLAUDE.md §9a — one
//! definition rather than three copies of the export's two commands.

use std::path::{Path, PathBuf};
use std::process::Command;

fn git(dir: &Path, args: &[&str], index: Option<&Path>) -> String {
    let mut cmd = Command::new("git");
    cmd.args(args).current_dir(dir);
    if let Some(index) = index {
        cmd.env("GIT_INDEX_FILE", index);
    }
    let out = cmd
        .output()
        .unwrap_or_else(|e| panic!("git {args:?} in {}: {e}", dir.display()));
    assert!(
        out.status.success(),
        "git {args:?} in {} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn chmod(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .unwrap_or_else(|e| panic!("chmod {mode:o} {}: {e}", path.display()));
}

/// Export `work`'s HEAD as a generation of root's tree beside it: fetched
/// into the bare store `<work>/../rt/repo.git`, exported whole as
/// `rt/gen/<sha>` — read-tree and checkout-index, the export's own two
/// commands — and stamped with `.boss-tree-sha`. Returns the generation,
/// which is what a verb is pointed at. Each directory is 0755, as
/// root-tree.sh's umask makes them, so the run's own umask cannot decide
/// a write-permission verdict. Call it again after a commit for the next
/// generation; the store is shared, as root's is.
pub fn export_root_generation(work: &Path) -> PathBuf {
    let parent = work
        .parent()
        .unwrap_or_else(|| panic!("{} has no parent for rt/", work.display()));
    let rt = parent.join("rt");
    let store = rt.join("repo.git");
    if !store.exists() {
        std::fs::create_dir_all(&rt).unwrap_or_else(|e| panic!("mkdir {}: {e}", rt.display()));
        git(&rt, &["init", "-q", "--bare", "repo.git"], None);
    }
    let store_arg = store.display().to_string();
    git(
        work,
        &["push", "-q", &store_arg, "HEAD:refs/boss/accepted"],
        None,
    );
    let sha = git(work, &["rev-parse", "HEAD"], None);
    let generation = rt.join("gen").join(&sha);
    std::fs::create_dir_all(&generation)
        .unwrap_or_else(|e| panic!("mkdir {}: {e}", generation.display()));
    let index = rt.join(format!(".{sha}.idx"));
    let git_dir = format!("--git-dir={}", store.display());
    let work_tree = format!("--work-tree={}", generation.display());
    git(
        &rt,
        &[&git_dir, &work_tree, "read-tree", &sha],
        Some(&index),
    );
    git(
        &rt,
        &[&git_dir, &work_tree, "checkout-index", "-a", "-f"],
        Some(&index),
    );
    std::fs::remove_file(&index).unwrap_or_else(|e| panic!("rm {}: {e}", index.display()));
    let stamp = generation.join(".boss-tree-sha");
    std::fs::write(&stamp, format!("{sha}\n"))
        .unwrap_or_else(|e| panic!("write {}: {e}", stamp.display()));
    for d in [&rt, &rt.join("gen"), &generation, &store] {
        chmod(d, 0o755);
    }
    generation
}
