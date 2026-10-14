//! The durable journal has a private mount ancestry, prepared without privileges.
use boss_testing::{repo_root, scratch::scratch_dir, write_exec};
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    process::Command,
};

fn run(home: &std::path::Path, bin: Option<&std::path::Path>) -> std::process::Output {
    let mut command = Command::new("bash");
    command
        .arg(repo_root().join("infra/train/prepare-diagnostics.sh"))
        .arg(home);
    if let Some(bin) = bin {
        command.env("PATH", format!("{}:/usr/bin:/bin", bin.display()));
    }
    command.output().unwrap()
}

#[test]
fn declared_fs_group_volume_prepares_private_durable_child_and_reuses_it_unchanged() {
    let cell = scratch_dir("conductor-diagnostic-home");
    let home = cell.join("home");
    fs::create_dir(&home).unwrap();
    let gid = String::from_utf8(Command::new("id").arg("-g").output().unwrap().stdout)
        .unwrap()
        .trim()
        .parse::<u32>()
        .unwrap();
    std::os::unix::fs::chown(&home, None, Some(gid)).unwrap();
    // mode-bits-ok: directory models the declared fsGroup PVC root, not an executable.
    fs::set_permissions(&home, fs::Permissions::from_mode(0o2775)).unwrap();
    let before = fs::metadata(&home).unwrap();
    let output = run(&home, None);
    assert!(
        output.status.success(),
        "nonroot preparation must work on fsGroup PVC: {output:?}"
    );
    let child = home.join("diagnostics");
    let metadata = fs::symlink_metadata(&child).unwrap();
    assert!(metadata.is_dir());
    assert_eq!(metadata.uid(), before.uid());
    assert_eq!(metadata.mode() & 0o777, 0o700);
    fs::write(child.join("original"), b"immutable acknowledgement").unwrap();
    let again = run(&home, None);
    assert!(
        again.status.success(),
        "restart must retain the private journal: {again:?}"
    );
    assert_eq!(
        fs::read(child.join("original")).unwrap(),
        b"immutable acknowledgement"
    );
    assert_eq!(fs::metadata(&child).unwrap().mode(), metadata.mode());
    let after = fs::metadata(&home).unwrap();
    assert_eq!(
        (after.uid(), after.gid(), after.mode()),
        (before.uid(), before.gid(), before.mode())
    );
}

#[test]
fn unsafe_existing_children_are_refused_without_changing_retained_bytes_or_modes() {
    for fault in ["symlink", "writable", "foreign-owner", "file"] {
        let cell = scratch_dir("conductor-diagnostic-refusal");
        let home = cell.join("home");
        fs::create_dir(&home).unwrap();
        let child = home.join("diagnostics");
        let target = cell.join("target");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("original"), b"original retained evidence").unwrap();
        if fault == "symlink" {
            std::os::unix::fs::symlink(&target, &child).unwrap();
        } else if fault == "file" {
            fs::write(&child, b"not a directory").unwrap();
        } else {
            fs::create_dir(&child).unwrap();
            // mode-bits-ok: directory fixture tests refusal without mode repair.
            fs::set_permissions(
                &child,
                fs::Permissions::from_mode(if fault == "writable" { 0o770 } else { 0o700 }),
            )
            .unwrap();
            fs::write(child.join("original"), b"original retained evidence").unwrap();
        }
        let before = fs::symlink_metadata(&child).unwrap();
        let bin = cell.join("bin");
        fs::create_dir(&bin).unwrap();
        if fault == "foreign-owner" {
            write_exec(
                &bin.join("id"),
                &format!(
                    "#!/bin/sh\nprintf '%s\\n' '{}'\n",
                    before.uid().saturating_add(1)
                ),
            );
        }
        let output = run(&home, Some(&bin));
        assert_eq!(output.status.code(), Some(78), "{fault}: {output:?}");
        let after = fs::symlink_metadata(&child).unwrap();
        assert_eq!((after.uid(), after.mode()), (before.uid(), before.mode()));
        assert_eq!(
            fs::read(target.join("original")).unwrap(),
            b"original retained evidence"
        );
        if fault == "writable" || fault == "foreign-owner" {
            assert_eq!(
                fs::read(child.join("original")).unwrap(),
                b"original retained evidence"
            );
        }
    }
}

#[test]
fn declared_mount_excludes_shared_root_and_restart_does_not_widen_journals() {
    let root = repo_root();
    let manifest =
        fs::read_to_string(root.join("infra/cluster/manifests/boss-conductor.yaml")).unwrap();
    assert!(
        manifest.contains("fsGroupChangePolicy: OnRootMismatch"),
        "fsGroup must not recursively widen retained journal permissions on each restart"
    );
    assert!(manifest.contains("BOSS_TRAIN_DIAGNOSTICS_HOME: \"/var/lib/boss-train-diagnostics\""));
    assert!(manifest.contains(
        "{name: home, mountPath: /var/lib/boss-train-diagnostics, subPath: diagnostics}"
    ));
    assert!(manifest.contains("name: prepare-diagnostics"));
    assert!(manifest.contains("command: [\"/bin/bash\", \"/opt/boss/infra/train/prepare-diagnostics.sh\", \"/conductor-home\"]"));
    assert!(manifest.contains("{name: home, mountPath: /conductor-home}"));
    let init = manifest
        .split("      initContainers:\n")
        .nth(1)
        .unwrap()
        .split("      containers:\n")
        .next()
        .unwrap();
    assert!(
        !init.contains("forge-token")
            && !init.contains("machine-token")
            && !init.contains("conductor-api")
    );
    assert!(manifest.contains("automountServiceAccountToken: false"));
    assert!(manifest.contains("{name: conductor-api, mountPath: /var/run/secrets/kubernetes.io/serviceaccount, readOnly: true}"));
    assert!(manifest.contains("serviceAccountToken: {path: token, expirationSeconds: 3600}"));
    assert!(manifest.contains(
        "name: kube-root-ca.crt\n                  items: [{key: ca.crt, path: ca.crt}]"
    ));
    assert!(manifest.contains(
        "items: [{path: namespace, fieldRef: {apiVersion: v1, fieldPath: metadata.namespace}}]"
    ));
    let dockerfile = fs::read_to_string(root.join("infra/oss-quickstart/Dockerfile")).unwrap();
    assert!(dockerfile.contains(
        "COPY infra/train/prepare-diagnostics.sh /opt/boss/infra/train/prepare-diagnostics.sh"
    ));
    let converge = fs::read_to_string(root.join("infra/forge/cluster-deploy-runner.sh")).unwrap();
    assert!(
        converge.contains("deploy/boss-conductor \"conductor=$REGISTRY:$HEAD\" \"prepare-diagnostics=$REGISTRY:$HEAD\""),
        "conductor and its preparation image must advance in one revision"
    );
}
