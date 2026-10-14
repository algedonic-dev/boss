//! A private mounted child must be prepared before candidate execution.
use boss_testing::{repo_root, scratch::scratch_dir, write_exec};
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    process::Command,
};

#[test]
fn inherited_setgid_is_removed_only_from_the_new_owned_private_child() {
    let cell = scratch_dir("consist-private-setgid");
    let volume = cell.join("volume");
    fs::create_dir(&volume).unwrap();
    let group = Command::new("id").arg("-g").output().unwrap();
    assert!(group.status.success());
    let gid = String::from_utf8(group.stdout)
        .unwrap()
        .trim()
        .parse::<u32>()
        .unwrap();
    // The owner may choose its own group without CAP_CHOWN; a root test
    // outside the inherited group otherwise silently loses SGID at chmod.
    std::os::unix::fs::chown(&volume, None, Some(gid)).unwrap();
    // mode-bits-ok: directory fixture models Kubernetes fsGroup emptyDir.
    fs::set_permissions(&volume, fs::Permissions::from_mode(0o2770)).unwrap();
    assert_eq!(
        fs::metadata(&volume).unwrap().permissions().mode() & 0o7777,
        0o2770
    );
    let before = fs::metadata(&volume).unwrap();
    let out = Command::new("bash")
        .arg("-c")
        .arg("source \"$1\" && prepare_consist_tmp \"$2\" private")
        .arg("consist-fixture")
        .arg(repo_root().join("infra/consist-worker/private-tmp.sh"))
        .arg(&volume)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "fsGroup volume must yield exact private child: {out:?}"
    );
    let child = fs::symlink_metadata(volume.join("private")).unwrap();
    assert!(child.is_dir() && !child.file_type().is_symlink());
    assert_eq!(child.uid(), before.uid());
    assert_eq!(child.permissions().mode() & 0o7777, 0o700);
    let after = fs::metadata(&volume).unwrap();
    assert_eq!(
        (after.uid(), after.gid(), after.permissions().mode()),
        (before.uid(), before.gid(), before.permissions().mode())
    );
}

#[test]
fn wrong_owner_or_failed_normalization_cannot_change_the_child_or_ancestor() {
    for fault in ["id", "chmod"] {
        let cell = scratch_dir("consist-private-fault");
        let volume = cell.join("volume");
        fs::create_dir(&volume).unwrap();
        let group = Command::new("id").arg("-g").output().unwrap();
        let gid = String::from_utf8(group.stdout)
            .unwrap()
            .trim()
            .parse::<u32>()
            .unwrap();
        std::os::unix::fs::chown(&volume, None, Some(gid)).unwrap();
        // mode-bits-ok: owned directory fixture retains the fsGroup SGID bit.
        fs::set_permissions(&volume, fs::Permissions::from_mode(0o2770)).unwrap();
        let before = fs::metadata(&volume).unwrap();
        let bin = cell.join("bin");
        fs::create_dir(&bin).unwrap();
        let body = if fault == "id" {
            format!(
                "#!/bin/sh\nprintf '%s\\n' '{}'\n",
                before.uid().saturating_add(1)
            )
        } else {
            "#!/bin/sh\nexit 1\n".to_owned()
        };
        write_exec(&bin.join(fault), &body);
        let out = Command::new("bash")
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .arg("-c")
            .arg("source \"$1\" && prepare_consist_tmp \"$2\" private")
            .arg("consist-fixture")
            .arg(repo_root().join("infra/consist-worker/private-tmp.sh"))
            .arg(&volume)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(78), "{fault}: {out:?}");
        let why = if fault == "id" {
            "created child owner differs"
        } else {
            "child mode normalization failed"
        };
        assert!(
            String::from_utf8_lossy(&out.stderr).contains(why),
            "{out:?}"
        );
        let child = fs::metadata(volume.join("private")).unwrap();
        assert_eq!(child.permissions().mode() & 0o7777, 0o2700);
        let after = fs::metadata(&volume).unwrap();
        assert_eq!(
            (after.uid(), after.gid(), after.permissions().mode()),
            (before.uid(), before.gid(), before.permissions().mode())
        );
    }
}

#[test]
fn trusted_bootstrap_creates_owned_private_scratch_and_refuses_existing_names() {
    let cell = scratch_dir("consist-private-tmp");
    let volume = cell.join("volume");
    fs::create_dir(&volume).unwrap();
    // mode-bits-ok: directory fixture models the group-writable volume root;
    // mode-bits-ok: private child bootstrap must not change this directory ancestor.
    fs::set_permissions(&volume, fs::Permissions::from_mode(0o770)).unwrap();
    let helper = repo_root().join("infra/consist-worker/private-tmp.sh");
    let run = || {
        Command::new("bash")
            .arg("-c")
            .arg("source \"$1\" && prepare_consist_tmp \"$2\" private")
            .arg("consist-fixture")
            .arg(&helper)
            .arg(&volume)
            .output()
            .unwrap()
    };
    let out = run();
    assert!(
        out.status.success(),
        "bootstrap must prepare its real mounted child: {out:?}"
    );
    let child = volume.join("private");
    let md = fs::symlink_metadata(&child).unwrap();
    assert!(md.is_dir() && !md.file_type().is_symlink());
    assert_eq!(md.permissions().mode() & 0o7777, 0o700);
    assert_eq!(md.uid(), fs::metadata(&volume).unwrap().uid());
    assert_eq!(
        fs::metadata(&volume).unwrap().permissions().mode() & 0o7777,
        0o770
    );
    fs::write(child.join("retained"), b"original").unwrap();
    let again = run();
    assert!(
        !again.status.success(),
        "a reused worker scratch name must be refused"
    );
    assert!(
        String::from_utf8_lossy(&again.stderr)
            .contains("consist-private-tmp: refused: child already exists")
    );
    assert_eq!(fs::read(child.join("retained")).unwrap(), b"original");
    assert_eq!(
        fs::metadata(&child).unwrap().permissions().mode() & 0o7777,
        0o700
    );
}

#[test]
fn linked_volume_is_refused_without_touching_its_target() {
    let cell = scratch_dir("consist-private-link");
    let target = cell.join("target");
    fs::create_dir(&target).unwrap();
    let link = cell.join("volume");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let out = Command::new("bash")
        .arg("-c")
        .arg("source \"$1\" && prepare_consist_tmp \"$2\" private")
        .arg("consist-fixture")
        .arg(repo_root().join("infra/consist-worker/private-tmp.sh"))
        .arg(&link)
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(fs::read_dir(&target).unwrap().next().is_none());
}

#[test]
fn a_child_name_cannot_escape_its_fresh_volume() {
    let cell = scratch_dir("consist-private-name");
    let volume = cell.join("volume");
    fs::create_dir(&volume).unwrap();
    for name in ["../escaped", "/escaped", "", ".", "..", "a/b"] {
        let out = Command::new("bash")
            .arg("-c")
            .arg("source \"$1\" && prepare_consist_tmp \"$2\" \"$3\"")
            .arg("consist-fixture")
            .arg(repo_root().join("infra/consist-worker/private-tmp.sh"))
            .arg(&volume)
            .arg(name)
            .output()
            .unwrap();
        assert!(
            !out.status.success(),
            "invalid child must be refused: {name}"
        );
        assert!(fs::read_dir(&volume).unwrap().next().is_none());
        assert!(!cell.join("escaped").exists());
    }
}
