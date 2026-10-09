//! `infra/ops/boss-ops-runner.service` keeps a mutating verb's token off
//! persistent disk WITHOUT a mount namespace (backlog 5f77b205,
//! 2026-09-28).
//!
//! THE DEFECT. github-act.sh kept its GitHub installation token's header
//! file in a `mktemp -d -t` directory under the host's shared /tmp. Its
//! EXIT trap removes it on a normal end and on timeout's SIGTERM, so it
//! survived only a SIGKILL, an OOM kill or a power loss — but then it
//! stayed on disk, and a git or curl crash could core-dump the token.
//!
//! THE FIX, and the two it is not. The unit gets its own
//! `RuntimeDirectory=` (tmpfs under /run, mode 0700, removed by systemd
//! when the oneshot run stops, gone on power loss), github-act.sh makes
//! its scratch there, and `LimitCORE=0` means no verb's git or curl
//! dumps core. The packet proposed `PrivateTmp=yes`; the alternative a
//! unit-wide `TMPDIR` was also considered. Both were refused:
//!
//!   * PrivateTmp= (and every other namespacing option below) gives the
//!     unit its own mount namespace, and systemd.exec(5) says so in the
//!     ReadOnlyPaths= note PrivateTmp= refers to: "these settings will
//!     disconnect propagation of mounts from the unit's processes to the
//!     host. This means that this setting may not be used for services
//!     which shall be able to install mount points in the main mount
//!     namespace." THREE VERBS THIS UNIT RUNS INSTALL MOUNTS ON THE HOST:
//!     `commission-a-disk` (mount), `move-forgejo-data` (mount --fstab)
//!     and `roll-back-forgejo-data-move` (umount). Under a namespace each
//!     would mount inside the run only, its own findmnt check would see
//!     the mount and report success, and the host would never see it —
//!     a Forgejo move recorded as verified while Forgejo kept serving
//!     the old path. A wrong target answers instead of erroring.
//!   * a unit-wide TMPDIR would reach every verb, including any that
//!     drops to another user with its environment kept, and point it at
//!     a root-only directory.
//!
//! So this pins the three lines present, refuses every option that
//! would give the unit a mount namespace, and keeps the reason live: if
//! the three verbs ever stop mounting, the refusal's `why` goes with
//! them and this test says so.
//!
//! THE SECOND UNIT (backlog 359a811c, 2026-09-28): `forge-converge.service`
//! had the same defect — forge-converge.sh made the forge token's
//! Authorization header with `mktemp -t` under the shared /tmp — and takes
//! the same three lines under the same refusal. Its reason is its own:
//! the converge IS the host's installer (install.sh, run under it, writes
//! /etc/systemd/system), so ProtectSystem= would fail the install and
//! every other option would put the unit that changes the host in a mount
//! namespace of its own. The script's side — the header made under
//! RUNTIME_DIRECTORY, a refusal without it — is run, not read, in
//! forge_converge_sh.rs.

use boss_testing::repo_root;

fn read(rel: &str) -> String {
    std::fs::read_to_string(repo_root().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// The unit's directive lines, comments and blanks dropped, trimmed.
fn directives(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with(';'))
        .map(str::to_string)
        .collect()
}

/// Every option known to set up a mount namespace for the unit — the
/// systemd.exec(5) sandboxing settings plus what systemd's own
/// `exec_needs_mount_namespace()` honours (adversarial review of this car,
/// 2026-09-28: ProcSubset=pid and NoExecPaths= are exactly what a later
/// hardening pass would reach for). Each disconnects mount propagation to
/// the host. A newer systemd may add more; the list is a floor, not a
/// proof of completeness.
const NAMESPACING: &[&str] = &[
    // [Unit]: joining another unit's namespace joins its mount namespace.
    "JoinsNamespaceOf=",
    // Implies PrivateTmp= and ProtectSystem=strict.
    "DynamicUser=",
    "ProcSubset=",
    "ExecPaths=",
    "NoExecPaths=",
    "MountImages=",
    "ExtensionImages=",
    "ExtensionDirectories=",
    "LogNamespace=",
    "PrivateIPC=",
    "IPCNamespacePath=",
    "PrivateNetwork=",
    "NetworkNamespacePath=",
    "MountAPIVFS=",
    "PrivateTmp=",
    "PrivateDevices=",
    "ProtectSystem=",
    "ProtectHome=",
    "ReadOnlyPaths=",
    "ReadWritePaths=",
    "InaccessiblePaths=",
    "TemporaryFileSystem=",
    "BindPaths=",
    "BindReadOnlyPaths=",
    "ProtectKernelTunables=",
    "ProtectKernelModules=",
    "ProtectKernelLogs=",
    "ProtectControlGroups=",
    "ProtectProc=",
    "PrivateMounts=",
    "MountFlags=",
    "RootDirectory=",
    "RootImage=",
];

/// The three lines a unit that writes a token's scratch carries.
fn keeps_its_scratch_in_a_runtime_directory(rel: &str, dir: &str, item: &str) {
    let unit = directives(&read(rel));
    for want in [
        format!("RuntimeDirectory={dir}"),
        "RuntimeDirectoryMode=0700".to_string(),
        "LimitCORE=0".to_string(),
    ] {
        assert!(
            unit.contains(&want),
            "{rel} must carry `{want}` (backlog {item}): a token's scratch lives on tmpfs \
             systemd removes when the run stops, and no git or curl dumps a core holding it"
        );
    }
}

/// Panics naming `what`, the option and `why` when any NAMESPACING
/// option appears in `lines`.
fn refuse_a_mount_namespace(what: &str, lines: &[String], why: &str) {
    for key in NAMESPACING {
        let hit = lines.iter().find(|l| l.contains(key));
        assert!(
            hit.is_none(),
            "{what} carries `{}`: that gives the unit a mount namespace, which \
             disconnects mount propagation to the host (systemd.exec(5)) — {why}",
            hit.unwrap()
        );
    }
}

#[test]
fn the_runner_unit_keeps_its_scratch_in_a_runtime_directory_and_dumps_no_core() {
    keeps_its_scratch_in_a_runtime_directory(
        "infra/ops/boss-ops-runner.service",
        "boss-ops-runner",
        "5f77b205",
    );
}

#[test]
fn the_runner_unit_never_gets_a_mount_namespace() {
    let why = "commission-a-disk, move-forgejo-data and roll-back-forgejo-data-move would \
               mount inside the run only and report a mount the host never saw \
               (backlog 5f77b205)";
    refuse_a_mount_namespace(
        "infra/ops/boss-ops-runner.service",
        &directives(&read("infra/ops/boss-ops-runner.service")),
        why,
    );
    // The per-host drop-in is written by the installer; it must not add
    // one either.
    refuse_a_mount_namespace(
        "infra/ops/install-ops-runner.sh (the drop-in it writes)",
        &directives(&read("infra/ops/install-ops-runner.sh")),
        why,
    );
}

#[test]
fn the_forge_converge_unit_keeps_its_header_in_a_runtime_directory_and_dumps_no_core() {
    keeps_its_scratch_in_a_runtime_directory(
        "infra/forge/forge-converge.service",
        "forge-converge",
        "359a811c",
    );
}

#[test]
fn the_forge_converge_unit_never_gets_a_mount_namespace() {
    refuse_a_mount_namespace(
        "infra/forge/forge-converge.service",
        &directives(&read("infra/forge/forge-converge.service")),
        "the converge is the host's installer: install.sh, run under it, writes \
         /etc/systemd/system, and ProtectSystem= would fail that write while the rest \
         would change the host through a view of the unit's own (backlog 359a811c)",
    );
}

/// The converge's refusal of a namespace keeps its reason only while the
/// converge still installs the host's units; if it ever stops, say so
/// here and in the unit's comment together.
#[test]
fn the_forge_converge_still_installs_the_hosts_units() {
    assert!(
        directives(&read("infra/forge/forge-converge.sh"))
            .iter()
            .any(|l| l.contains("\"$RUN_FROM/forge/install.sh\"")),
        "forge-converge.sh no longer runs install.sh; re-read whether a mount namespace is \
         still refused for a reason, and update this pin and the unit's comment together"
    );
    assert!(
        directives(&read("infra/forge/install.sh"))
            .iter()
            .any(|l| l == "ETC=\"${INSTALL_ETC:-/etc/systemd/system}\""),
        "install.sh no longer writes /etc/systemd/system by default; re-read whether \
         forge-converge.service still refuses a mount namespace for a reason"
    );
}

#[test]
fn the_verbs_that_forbid_a_namespace_still_install_mounts() {
    // (verb file, the script its argv runs, a mount command that script runs)
    for (verb, script, cmd) in [
        (
            "commission-a-disk",
            "infra/forge/commission-a-disk.sh",
            "MOUNT_CMD=(mount \"$MOUNT\")",
        ),
        (
            "move-forgejo-data",
            "infra/forge/move-forgejo-data.sh",
            "mount --fstab \"$FSTAB\" \"$SRC\"",
        ),
        (
            "roll-back-forgejo-data-move",
            "infra/forge/move-forgejo-data.sh",
            "umount -- \"$SRC\"",
        ),
    ] {
        let v: serde_json::Value =
            serde_json::from_str(&read(&format!("infra/ops/verbs/{verb}.json")))
                .unwrap_or_else(|e| panic!("{verb}.json: {e}"));
        assert_eq!(
            v["argv"][0].as_str(),
            Some(script),
            "{verb} no longer runs {script}; re-read whether boss-ops-runner still runs a \
             mount-installing verb, and update this pin and its unit's comment together"
        );
        assert!(
            directives(&read(script)).iter().any(|l| l.contains(cmd)),
            "{script} no longer runs `{cmd}`; if no verb under boss-ops-runner installs a \
             mount any more, the refusal of a mount namespace has lost its reason — say so \
             in the unit and here, together"
        );
    }
}

#[test]
fn github_act_makes_its_token_scratch_in_the_runtime_directory() {
    let code = directives(&read("infra/forge/github-act.sh"));
    assert!(
        code.iter()
            .any(|l| l.contains("mktemp -d -p \"${RUNTIME_DIRECTORY:?}\" github-act.XXXXXX")),
        "github-act.sh must make its scratch (the token's header file) under the unit's \
         RUNTIME_DIRECTORY, never under the shared /tmp (backlog 5f77b205)"
    );
    assert!(
        !code
            .iter()
            .any(|l| l.contains("mktemp") && l.contains(" -t")),
        "github-act.sh still makes a scratch directory under $TMPDIR / the shared /tmp"
    );
}
