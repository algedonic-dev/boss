//! The forge's three replayed spools are not where any account can make
//! them (backlog dda26693; review acab446c of car 1386dd64, finding F8a).
//!
//! WHO WRITES AND WHO READS, AND AS WHOM. Each is written and read back
//! by ONE account. Until backlog a604a35b that was the checkout owner's
//! for all of them (`User=david`); since it the watchdog runs as root
//! and keeps its three under its own root-only state directory, and the
//! cluster converge — still that owner's — keeps an alert spool of its
//! own and replays it itself. No account replays a file another wrote:
//!
//! | spool | written by | read (replayed) by |
//! |---|---|---|
//! | the alert spool (`ALERT_SPOOL`) | `alert` in alert-lib.sh, from cluster-watchdog.sh and from the converge's tenant check (cluster-deploy-runner.sh) | `alert_replay` — by the unit that wrote it: cluster-watchdog.sh every five minutes, cluster-deploy-lib.sh `replay_kept_alerts` every converge tick; each `*.json` is POSTed as a platform-admin automation actor |
//! | the door spool (`DOOR_SPOOL_DIR`) | observe-door.sh `spool_put` (second line of cluster-watchdog.service) | the same script's `spool_replay`: each `*.json` is POSTed to the estate door |
//! | the door's dark-since state (`DOOR_STATE_DIR`) | observe-door.sh `dark_since` | the same function, next run: the first line becomes `dark_since` in an observation the alarm band is judged on |
//!
//! Each defaulted to a NAME UNDER /var/tmp, which is sticky and
//! world-writable. The directory is made by the first writer — and a
//! reading or an alert is only kept when the system of record is dark, so
//! on a healthy host the directory may never have been made. The probe
//! account (`boss-probe`, which runs text a car's builder wrote) could
//! then make it first, own it, and plant a body the owner's next replay
//! files as its own. Nothing checked who owned the directory.
//!
//! THE FIX IS WHERE THEY LIVE, not a filter on what is in them: where
//! only the unit's own account and root can create a name — a root
//! unit's under its 0700 `StateDirectory=`, an account's under its home. What a tree can prove of that is below; the
//! kernel's answer on the host is the `spool:` lines of the
//! probe-account-controls verb (probe_account_sh.rs).

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

use boss_testing::{create_dir, repo_root, scratch_dir, write_exec, write_file};

fn read(rel: &str) -> String {
    std::fs::read_to_string(repo_root().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// `Environment=<name>=<value>` of a unit, when it has exactly one.
fn unit_env(unit: &str, name: &str) -> Option<String> {
    let prefix = format!("Environment={name}=");
    let found: Vec<&str> = unit
        .lines()
        .filter_map(|l| l.strip_prefix(prefix.as_str()))
        .collect();
    assert!(found.len() <= 1, "{name} is set {} times", found.len());
    found.first().map(|s| s.to_string())
}

fn unit_user(unit: &str) -> String {
    unit.lines()
        .find_map(|l| l.strip_prefix("User="))
        .expect("the unit names its user")
        .to_string()
}

/// A directory only the unit's own account and root can make a name in,
/// and under no directory every account can write. For a root unit that
/// is its own `StateDirectory=`, which systemd makes root's with the mode
/// the unit declares — 0700, or anyone could list and plant; for an
/// account's unit it is a name under that account's home.
fn assert_the_users_own(what: &str, dir: &str, unit: &str) {
    let user = unit_user(unit);
    for shared in ["/tmp/", "/var/tmp/", "/dev/shm/", "/run/lock/"] {
        assert!(
            !dir.starts_with(shared),
            "{what} is {dir}: any account can make a name under {shared}"
        );
    }
    assert!(!dir.contains("/../"), "{what} is {dir}");
    if user == "root" {
        let state: Vec<&str> = unit
            .lines()
            .filter_map(|l| l.strip_prefix("StateDirectory="))
            .collect();
        assert_eq!(
            state.len(),
            1,
            "{what}: a root unit that replays declares one StateDirectory="
        );
        assert!(
            unit.lines().any(|l| l == "StateDirectoryMode=0700"),
            "{what}: the state directory must be 0700 — only root may make a name in it"
        );
        let own = format!("/var/lib/{}/", state[0]);
        assert!(
            dir.starts_with(&own) && dir.len() > own.len(),
            "{what} is {dir}; a root unit keeps it under its own state directory {own}"
        );
    } else {
        let home = format!("/home/{user}/");
        assert!(
            dir.starts_with(&home) && dir.len() > home.len(),
            "{what} is {dir}; it must be a name under {home}, which only {user} and root can make"
        );
    }
}

/// The scripts a forge script sources, followed to the end: `. "…/x.sh"`
/// lines, resolved by file name within infra/forge, infra/estate and
/// infra/lib.
fn sourced_closure(start: &str) -> BTreeSet<String> {
    let root = repo_root();
    let mut seen = BTreeSet::new();
    let mut todo = vec![start.to_string()];
    while let Some(rel) = todo.pop() {
        if !seen.insert(rel.clone()) {
            continue;
        }
        let Ok(body) = std::fs::read_to_string(root.join(&rel)) else {
            continue;
        };
        for line in body.lines() {
            let t = line.trim_start();
            if !(t.starts_with(". ") || t.starts_with("source ")) {
                continue;
            }
            let Some(end) = t.find(".sh") else { continue };
            let name = t[..end + 3]
                .rsplit(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.'))
                .next()
                .unwrap_or("");
            for dir in ["infra/forge", "infra/estate", "infra/lib", "infra"] {
                let candidate = format!("{dir}/{name}");
                if root.join(&candidate).is_file() {
                    todo.push(candidate);
                    break;
                }
            }
        }
    }
    seen
}

/// Every forge unit as (name, text, the repo-relative scripts its
/// ExecStart lines run).
fn forge_units() -> Vec<(String, String, Vec<String>)> {
    let dir = repo_root().join("infra/forge");
    let mut units = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap().filter_map(Result::ok) {
        let path = entry.path();
        if path.extension().is_none_or(|x| x != "service") {
            continue;
        }
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        let text = std::fs::read_to_string(&path).unwrap();
        let scripts = text
            .lines()
            .filter_map(|l| l.strip_prefix("ExecStart="))
            .map(|v| v.trim_start_matches('-'))
            .filter_map(|v| v.split_whitespace().next())
            .filter_map(|p| {
                p.strip_prefix("/var/lib/boss/tree/current/")
                    .or_else(|| p.strip_prefix("/home/david/boss/"))
            })
            .map(str::to_string)
            .collect();
        units.push((name, text, scripts));
    }
    units.sort();
    units
}

#[test]
fn the_watchdog_unit_keeps_its_three_spools_where_only_its_account_can_write() {
    let unit = read("infra/forge/cluster-watchdog.service");
    let mut dirs = BTreeSet::new();
    for name in ["ALERT_SPOOL", "DOOR_SPOOL_DIR", "DOOR_STATE_DIR"] {
        let dir = unit_env(&unit, name).unwrap_or_else(|| {
            panic!(
                "cluster-watchdog.service must set {name}: the script's default is a name \
                 under a directory any account can write"
            )
        });
        assert_the_users_own(&format!("the watchdog's {name}"), &dir, &unit);
        dirs.insert(dir);
    }
    assert_eq!(
        dirs.len(),
        3,
        "three directories: two spools that name files by time must not share one"
    );
}

/// Every unit that can raise an alert keeps what it could not file in a
/// spool of ITS OWN ACCOUNT'S, and replays that spool itself. Two units
/// of two accounts never name one directory: the one that replays would
/// be filing, as itself and with its own credential, what the other
/// wrote (backlog a604a35b — the watchdog is root and presents the
/// machine token; the cluster converge is not and does not). Found from
/// what each unit's scripts source, so a third unit that starts raising
/// alerts is held to the same lines.
#[test]
fn every_forge_unit_that_raises_an_alert_keeps_and_replays_its_own_spool() {
    let mut raisers = BTreeSet::new();
    let mut by_spool: std::collections::BTreeMap<String, BTreeSet<String>> = Default::default();
    for (name, text, scripts) in forge_units() {
        let closure: BTreeSet<String> = scripts.iter().flat_map(|s| sourced_closure(s)).collect();
        if !closure.contains("infra/forge/alert-lib.sh") {
            continue;
        }
        raisers.insert(name.clone());
        let spool = unit_env(&text, "ALERT_SPOOL").unwrap_or_else(|| {
            panic!("{name}.service sources alert-lib.sh; it must name its ALERT_SPOOL — the default is a name under /var/tmp")
        });
        assert_the_users_own(&format!("{name}'s ALERT_SPOOL"), &spool, &text);
        by_spool.entry(spool).or_default().insert(unit_user(&text));
        // It replays what it keeps: the watchdog in its own script, the
        // converge through the lib function its script calls.
        let replays = closure.iter().any(|f| {
            let body = read(f);
            body.lines().any(|l| {
                let t = l.trim();
                !t.starts_with('#')
                    && (t.starts_with("alert_replay") || t.starts_with("replay_kept_alerts"))
            })
        });
        assert!(
            replays,
            "{name}.service keeps alerts in a spool nothing it runs replays: an alert kept there is never filed"
        );
    }
    for (spool, users) in &by_spool {
        assert_eq!(
            users.len(),
            1,
            "{spool} is named by units of more than one account ({users:?}): one would replay what the other wrote"
        );
    }
    // The closure found the two that exist today; finding none would be
    // a parser that reads nothing.
    for known in ["cluster-watchdog", "cluster-deploy-runner"] {
        assert!(
            raisers.contains(known),
            "{known} sources alert-lib.sh and was not found: {raisers:?}"
        );
    }
    // And the door observer runs in the unit that names its two.
    let door_units: Vec<String> = forge_units()
        .into_iter()
        .filter(|(_, _, scripts)| scripts.iter().any(|s| s == "infra/estate/observe-door.sh"))
        .map(|(name, _, _)| name)
        .collect();
    assert_eq!(door_units, ["cluster-watchdog"]);
}

struct Door {
    dir: PathBuf,
    home: PathBuf,
    /// The door's LAN port, held dark for as long as the door is observed.
    _dark: boss_testing::DarkPort,
}

impl Door {
    /// A door that is dark, and a system of record that takes nothing:
    /// the run that writes both the state and the spool.
    fn new(tag: &str) -> Self {
        let dir = scratch_dir(tag);
        let bin = dir.join("bin");
        create_dir(&bin);
        write_exec(&bin.join("ssh-keyscan"), "#!/bin/sh\nexit 0\n");
        write_exec(
            &bin.join("getent"),
            "#!/bin/sh\n[ \"$1\" = hosts ] || exit 2\necho \"203.0.113.7     $2\"\n",
        );
        write_exec(
            &bin.join("curl"),
            "#!/bin/sh\ncat > /dev/null\nprintf '%s\\n%s' '{}' 503\n",
        );
        // A port nothing listens on, held so for as long as this `Door`
        // lives (backlog ec131700): it was bound and released here, before
        // the observer that reads it had even started.
        let dark = boss_testing::dark_port();
        let port = dark.port;
        write_file(
            &dir.join("doors.toml"),
            &format!(
                "[[door]]\nid = \"dev-ssh\"\nlan = \"127.0.0.1:{port}\"\n\
                 public = \"dev.example.test\"\ndark_band_minutes = \"15\"\n"
            ),
        );
        let home = dir.join("home");
        create_dir(&home);
        Door {
            dir,
            home,
            _dark: dark,
        }
    }

    fn command(&self) -> Command {
        let mut c = Command::new("bash");
        c.arg(repo_root().join("infra/estate/observe-door.sh"))
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.dir.join("bin").display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("DOORS_FILE", self.dir.join("doors.toml"))
            .env("DOOR_TIMEOUT_S", "2")
            .env("JOBS_API", "http://stub")
            .env_remove("DOOR_STATE_DIR")
            .env_remove("DOOR_SPOOL_DIR")
            .env_remove("SPOOL_DIR");
        c
    }
}

fn names_in(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|d| {
            d.filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// By effect: told nothing about where to keep them, the door observer
/// keeps its dark-since state and its unposted reading under the HOME of
/// whoever runs it — a hand run on the forge reads the same directories
/// the unit does, never a name any account could have made first.
#[test]
fn the_door_observer_keeps_state_and_spool_in_its_callers_home_by_default() {
    let d = Door::new("door-default-dirs");
    let o = d.command().env("HOME", &d.home).output().unwrap();
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(
        said.contains("retained: door observation spooled"),
        "the run must have kept its reading: {said}"
    );
    assert_eq!(
        names_in(&d.home.join(".boss-door-state")),
        ["dev-ssh.lan"],
        "the dark half's first reading is kept under HOME: {said}"
    );
    let kept = names_in(&d.home.join(".boss-door-spool"));
    assert_eq!(kept.len(), 1, "{kept:?}: {said}");
    assert!(kept[0].ends_with(".json"), "{kept:?}");
}

/// With no HOME either there is nowhere of the caller's own, and the
/// observer says so rather than fall back to a shared directory.
#[test]
fn the_door_observer_with_nowhere_of_its_own_refuses_by_name() {
    let d = Door::new("door-no-home");
    let o = d.command().env_remove("HOME").output().unwrap();
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("DOOR_STATE_DIR"), "{err}");
    assert!(
        !String::from_utf8_lossy(&o.stdout).contains("retained"),
        "nothing was probed or kept"
    );
    // The spool alone unnamed is refused the same way, by its own name.
    let o = d
        .command()
        .env_remove("HOME")
        .env("DOOR_STATE_DIR", d.dir.join("state-only"))
        .output()
        .unwrap();
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("DOOR_SPOOL_DIR"), "{err}");
    assert!(!d.dir.join("state-only").exists(), "nothing was probed");
    // Named, it needs no HOME.
    let o = d
        .command()
        .env_remove("HOME")
        .env("DOOR_STATE_DIR", d.dir.join("state"))
        .env("DOOR_SPOOL_DIR", d.dir.join("spool"))
        .output()
        .unwrap();
    assert_eq!(names_in(&d.dir.join("state")), ["dev-ssh.lan"]);
    assert_eq!(names_in(&d.dir.join("spool")).len(), 1);
    assert!(String::from_utf8_lossy(&o.stdout).contains("retained"));
}
