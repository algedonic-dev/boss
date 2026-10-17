//! The existing installer retires only its obsolete, owned address drop-in.
// not a tree-wide pin: exercises the GCP installer's retirement boundary.
use boss_testing::{copy_exec, create_dir, repo_root, scratch_dir, write_exec, write_file};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::PathBuf;
use std::process::{Command, Output};

struct Fixture {
    dir: PathBuf,
    source: PathBuf,
    etc: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = scratch_dir("gcp-jobs-url-cleanup");
        let source = dir.join("source");
        for sub in ["gcp", "lib", "estate", "ops"] {
            create_dir(&source.join("infra").join(sub));
        }
        for path in [
            "gcp/install-units.sh",
            "lib/host-check.sh",
            "lib/jq.sh",
            "run-summary.sh",
        ] {
            copy_exec(
                &repo_root().join("infra").join(path),
                &source.join("infra").join(path),
            );
        }
        write_file(
            &source.join("infra/estate/roles.toml"),
            "[always]\nunits = [\"boss-gcp-converge\"]\n[roles.selected]\nunits = [\"fixture-derived\"]\n[roles.other]\nunits = [\"fixture-other\"]\n",
        );
        for stem in ["boss-gcp-converge", "fixture-derived", "fixture-other"] {
            write_file(
                &source.join(format!("infra/{stem}.timer")),
                "[Timer]\nOnBootSec=1h\n",
            );
            write_file(
                &source.join(format!("infra/{stem}.service")),
                "[Service]\nEnvironmentFile=/etc/boss/sor.env\n",
            );
        }
        // The bootstrap's optional address-file directive is actual source,
        // not a stronger substitute invented by this fixture.
        std::fs::copy(
            repo_root().join("infra/gcp/boss-gcp-converge.service"),
            source.join("infra/boss-gcp-converge.service"),
        )
        .unwrap();
        for path in ["journal-door-ensure.sh", "ops/install-ops-runner.sh"] {
            write_exec(&source.join("infra").join(path), "#!/bin/sh\nexit 0\n");
        }
        write_exec(
            &dir.join("systemctl"),
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$CALL_LOG\"\nexit 0\n",
        );
        let etc = dir.join("etc");
        create_dir(&etc);
        Self { dir, source, etc }
    }
    fn drop_in(&self, stem: &str) -> PathBuf {
        self.etc.join(format!("{stem}.service.d/jobs-url.conf"))
    }
    fn plant(&self, stem: &str) {
        // Exact installed shape measured by native unit-cat on 2026-10-15
        // (6856303c): a two-line obsolete Environment= drop-in.
        create_dir(self.drop_in(stem).parent().unwrap());
        write_file(
            &self.drop_in(stem),
            "[Service]\nEnvironment=BOSS_JOBS_URL=http://127.0.0.1:7900\n",
        );
    }
    fn run(&self) -> Output {
        Command::new("bash")
            .arg(self.source.join("infra/gcp/install-units.sh"))
            .arg("units")
            .env("BOSS_REPO_ROOT", &self.source)
            .env("BOSS_NODE_ROLES", "selected")
            .env_remove("BOSS_GCP_ACCEPTED_SHA")
            .env("INSTALL_ETC", &self.etc)
            .env("INSTALL_SYSTEMCTL", self.dir.join("systemctl"))
            .env("INSTALL_APT_GET", self.dir.join("never-run-apt-get"))
            .env("BOSS_OPS_RUNNER_RETIRED", self.dir.join("retired"))
            .env("BOSS_RUN_SUMMARY_FILE", self.dir.join("summary.json"))
            .env("CALL_LOG", self.dir.join("calls"))
            .output()
            .unwrap()
    }
}

#[test]
fn a_converge_removes_existing_owned_residue_and_keeps_other_drop_ins() {
    let f = Fixture::new();
    for stem in [
        "boss-gcp-converge",
        "fixture-derived",
        "fixture-other",
        "unregistered",
    ] {
        f.plant(stem);
    }
    let unrelated = f.etc.join("fixture-derived.service.d/zz-sor.conf");
    write_file(&unrelated, "[Service]\nEnvironment=KEEP_THIS=unchanged\n");
    for _ in 0..2 {
        let out = f.run();
        assert!(out.status.success(), "{out:?}");
        assert!(
            !f.drop_in("boss-gcp-converge").exists(),
            "the installed bootstrap residue must be retired"
        );
        assert!(
            !f.drop_in("fixture-derived").exists(),
            "the roster, not a duplicated unit list, owns this subject"
        );
        assert!(f.drop_in("fixture-other").exists());
        assert!(f.drop_in("unregistered").exists());
        assert_eq!(
            std::fs::read_to_string(&unrelated).unwrap(),
            "[Service]\nEnvironment=KEEP_THIS=unchanged\n"
        );
        assert!(
            std::fs::read_to_string(f.etc.join("boss-gcp-converge.service"))
                .unwrap()
                .contains("EnvironmentFile=-/etc/boss/sor.env")
        );
    }
    let calls = std::fs::read_to_string(f.dir.join("calls")).unwrap();
    assert!(calls.contains("daemon-reload"));
    assert!(!calls.lines().any(|line| {
        ["restart", "stop", "disable"]
            .iter()
            .any(|verb| line.starts_with(verb))
    }));
}

#[test]
fn linked_drop_in_paths_are_refused_without_deleting_their_targets() {
    for directory_link in [false, true] {
        let f = Fixture::new();
        let outside = f.dir.join("outside");
        create_dir(&outside);
        let target = outside.join("jobs-url.conf");
        write_file(
            &target,
            "[Service]\nEnvironment=BOSS_JOBS_URL=http://127.0.0.1:7900\n",
        );
        let path = f.drop_in("boss-gcp-converge");
        if directory_link {
            symlink(&outside, path.parent().unwrap()).unwrap();
        } else {
            create_dir(path.parent().unwrap());
            symlink(&target, &path).unwrap();
        }
        let out = f.run();
        assert_eq!(out.status.code(), Some(78), "{out:?}");
        assert!(
            std::fs::symlink_metadata(if directory_link {
                path.parent().unwrap()
            } else {
                &path
            })
            .unwrap()
            .file_type()
            .is_symlink()
        );
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "[Service]\nEnvironment=BOSS_JOBS_URL=http://127.0.0.1:7900\n"
        );
    }
}

#[test]
fn an_unrecognized_file_or_replaceable_directory_is_not_retired() {
    for writable in [false, true] {
        let f = Fixture::new();
        f.plant("boss-gcp-converge");
        let path = f.drop_in("boss-gcp-converge");
        if writable {
            std::fs::set_permissions(
                path.parent().unwrap(),
                // mode-bits-ok: directory made replaceable to test refusal; no file is executed.
                std::fs::Permissions::from_mode(0o777),
            )
            .unwrap();
        } else {
            write_file(&path, "[Service]\nEnvironment=KEEP_THIS=unchanged\n");
        }
        let before = std::fs::read(&path).unwrap();
        let out = f.run();
        assert_eq!(out.status.code(), Some(78), "{out:?}");
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}
