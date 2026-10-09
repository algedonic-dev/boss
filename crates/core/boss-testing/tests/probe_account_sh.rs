//! `infra/forge/probe-account.sh` — the account a landed car's recorded
//! probe runs as on the forge host, the read-only view it reads, what is
//! left of it after a runner tick, and the controls that say whether any
//! of that holds on a host (backlog 703358ce; decided by David on
//! design-doc bdc60b65, question probe-user, 2026-10-06).
//!
//! WHAT A TREE CAN PROVE, AND WHAT IT CANNOT. Three kinds of leg here:
//!
//! * ANY UID — the installer's own logic against stub `useradd`, `id`,
//!   `getent`, `usermod`, `passwd` and `sudo`: what it creates, what it
//!   repairs, when it withholds the drop-in, that a second run changes
//!   nothing, and that the view follows the checkout. The "account"
//!   there is a fixture; the refusal to write is a stub's answer.
//! * ROOT WITH A SECOND UID (the dev pod; NOT the gate, whose uid 65534
//!   can become nobody else) — the kernel's own answers: a real other
//!   uid cannot write the view, git refuses it a root-owned repository
//!   until the system gitconfig names the view, the fetch is served as
//!   the checkout's owner where root's own read is refused, a setsid
//!   child is gone after `reap`, and the controls go red when a secret
//!   is made readable. Each prints `NOT RUN` where it cannot run, and a
//!   green gate therefore vouches for the first kind only.
//! * THE HOST ONLY — everything about the real account: that `useradd`
//!   made what the stub was told, what `sudo -l` says, both docker
//!   sockets, david's home, the token paths, cron. That is the
//!   `probe-account-controls` ops verb, run on the forge after landing.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use boss_testing::{create_dir, repo_root, scratch_dir, write_exec, write_file};

const ACCOUNT: &str = "boss-probe";

fn script() -> PathBuf {
    repo_root().join("infra/forge/probe-account.sh")
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
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
    assert!(o.status.success(), "git {args:?}: {}", text(&o));
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

/// A checkout shaped like the converged one: the reader on its PATH
/// directory, committed.
fn source(dir: &Path) -> PathBuf {
    let src = dir.join("checkout");
    create_dir(&src.join("infra/forge/probe-bin"));
    git(&src, &["init", "-q"]);
    write_exec(
        &src.join("infra/forge/probe-bin/boss-sor-read"),
        "#!/bin/sh\n# boss-sor-read fixture\n",
    );
    git(&src, &["add", "-A"]);
    git(&src, &["commit", "-q", "-m", "one"]);
    src
}

/// One fixture host: stub account tools whose state is files in
/// `state/`, a scratch /etc, and the seams pointed at them.
struct Host {
    dir: PathBuf,
    state: PathBuf,
    src: PathBuf,
}

impl Host {
    fn new(name: &str) -> Self {
        let dir = scratch_dir(&format!("probe-account-{name}"));
        let state = dir.join("state");
        let bin = dir.join("bin");
        create_dir(&state);
        create_dir(&bin);
        create_dir(&dir.join("etc/systemd"));
        create_dir(&dir.join("hostetc"));
        write_file(
            &dir.join("etc/systemd/boss-ops-runner.service"),
            "[Service]\n",
        );
        write_file(
            &state.join("sudo.txt"),
            "User boss-probe is not allowed to run sudo on forge.\n",
        );
        let real = std::env::var("PATH").unwrap_or_default();
        write_file(&state.join("real-path"), &real);
        // Each stub answers for the fixture account and hands every
        // other question to the real tool.
        let pre = "#!/bin/sh\nS=\"$STUB_STATE\"\nREAL=\"$(cat \"$S/real-path\")\"\nlast=\"\"\nfor a in \"$@\"; do last=\"$a\"; done\n";
        write_exec(
            &bin.join("id"),
            &format!(
                "{pre}if [ \"$last\" = {ACCOUNT} ]; then\n  [ -f \"$S/uid\" ] || {{ echo \"id: no such user\" >&2; exit 1; }}\n  case \"$1\" in -u) cat \"$S/uid\" ;; -g) cat \"$S/gid\" ;; -G) cat \"$S/groups\" ;; *) exit 1 ;; esac\n  exit 0\nfi\nPATH=\"$REAL\" exec id \"$@\"\n"
            ),
        );
        write_exec(
            &bin.join("getent"),
            &format!(
                "{pre}if [ \"$1\" = passwd ] && [ \"$last\" = {ACCOUNT} ]; then\n  [ -f \"$S/uid\" ] || exit 2\n  echo \"{ACCOUNT}:x:$(cat \"$S/uid\"):$(cat \"$S/gid\")::/nonexistent:$(cat \"$S/shell\")\"\n  exit 0\nfi\nPATH=\"$REAL\" exec getent \"$@\"\n"
            ),
        );
        write_exec(
            &bin.join("useradd"),
            "#!/bin/sh\nS=\"$STUB_STATE\"\necho \"$*\" >> \"$S/useradd.log\"\n[ -f \"$S/useradd-fails\" ] && { echo 'useradd: cannot lock /etc/passwd' >&2; exit 1; }\necho 4242 > \"$S/uid\"; echo 4242 > \"$S/gid\"; echo 4242 > \"$S/groups\"\necho /usr/sbin/nologin > \"$S/shell\"\n",
        );
        write_exec(
            &bin.join("usermod"),
            "#!/bin/sh\nS=\"$STUB_STATE\"\necho \"$*\" >> \"$S/usermod.log\"\n[ -f \"$S/usermod-is-ignored\" ] && exit 0\ncase \"$1\" in -G) cp \"$S/gid\" \"$S/groups\" ;; -s) echo \"$2\" > \"$S/shell\" ;; esac\n",
        );
        write_exec(
            &bin.join("passwd"),
            &format!(
                "#!/bin/sh\nlock=L\n[ -f \"$STUB_STATE/passwd-status\" ] && lock=\"$(cat \"$STUB_STATE/passwd-status\")\"\necho \"{ACCOUNT} $lock 2026-10-06 0 99999 7 -1\"\n"
            ),
        );
        // Who owns a path is the kernel's answer; a fixture with one uid
        // says "somebody else" through this stub (state/view-owner), and
        // every other question goes to the real tool.
        write_exec(
            &bin.join("stat"),
            "#!/bin/sh\nS=\"$STUB_STATE\"\nREAL=\"$(cat \"$S/real-path\")\"\nif [ \"$1\" = -c ] && [ \"$2\" = %u ] && [ -f \"$S/view-owner\" ]; then cat \"$S/view-owner\"; exit 0; fi\nPATH=\"$REAL\" exec stat \"$@\"\n",
        );
        write_exec(
            &bin.join("sudo"),
            "#!/bin/sh\nfor a in \"$@\"; do [ \"$a\" = -l ] && { cat \"$STUB_STATE/sudo.txt\"; exit 1; }; done\nexit 1\n",
        );
        // "As the account", for a box with one uid: every command runs,
        // and the one question whose answer is the kernel's — may it
        // write this? — is answered no. The real answer is the
        // second-uid legs' and the host's.
        write_exec(
            &bin.join("fake-runas"),
            "#!/bin/sh\nif [ \"$1\" = test ] && [ \"$2\" = -w ]; then exit 1; fi\nexec \"$@\"\n",
        );
        // The host's `boss`, as far as `ensure` asks: does it know the
        // marker the unattended door requires?
        write_file(&dir.join("boss-cli"), "…BOSS_PROBE_VERIFIED_FILE…\n");
        create_dir(&dir.join("run"));
        let src = source(&dir);
        Host { dir, state, src }
    }

    fn exists(self) -> Self {
        for (f, v) in [
            ("uid", "4242"),
            ("gid", "4242"),
            ("groups", "4242"),
            ("shell", "/usr/sbin/nologin"),
        ] {
            write_file(&self.state.join(f), &format!("{v}\n"));
        }
        self
    }

    fn view(&self) -> PathBuf {
        self.dir.join("view")
    }

    /// What verify-tick writes ahead of a tick, and the door requires.
    fn marker(&self) -> PathBuf {
        self.dir.join("run/probe-account.verified")
    }

    /// One runner tick's ExecStartPre.
    fn tick(&self) -> (i32, String) {
        let o = self.command("verify-tick").output().unwrap();
        (o.status.code().unwrap_or(-1), text(&o))
    }

    fn dropin(&self) -> PathBuf {
        self.dir
            .join("etc/systemd/boss-ops-runner.service.d/probe-account.conf")
    }

    fn command(&self, mode: &str) -> Command {
        let mut c = Command::new("bash");
        c.arg(script())
            .arg(mode)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.dir.join("bin").display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("STUB_STATE", &self.state)
            .env("BOSS_PROBE_VIEW", self.view())
            .env("BOSS_PROBE_VIEW_SOURCE", &self.src)
            .env("PROBE_SOURCE_RUNAS", "")
            .env("PROBE_RUNAS", self.dir.join("bin/fake-runas"))
            .env("INSTALL_ETC", self.dir.join("etc/systemd"))
            .env("INSTALL_PROBE_ETC", self.dir.join("hostetc"))
            .env("INSTALL_PROBE_LIBEXEC", self.dir.join("libexec"))
            .env("INSTALL_PROBE_GITCONFIG", self.dir.join("gitconfig"))
            .env("INSTALL_PROBE_CLI", self.dir.join("boss-cli"))
            .env("INSTALL_PROBE_MARKER", self.marker())
            .env("BOSS_PROBE_VERIFIED_FILE", self.marker())
            .env("BOSS_RUN_SUMMARY_FILE", self.dir.join("summary.json"))
            .env_remove("BOSS_FORGE_REPO_DIR");
        c
    }

    fn ensure(&self) -> (i32, String) {
        let o = self.command("ensure").output().unwrap();
        (o.status.code().unwrap_or(-1), text(&o))
    }
}

#[test]
fn a_fresh_host_gets_the_account_the_view_and_the_one_drop_in() {
    let h = Host::new("fresh");
    let (rc, out) = h.ensure();
    assert_eq!(rc, 0, "{out}");

    // The account, exactly as defined: system, a group of its own, no
    // home made, no shell.
    let added = read(&h.state.join("useradd.log"));
    assert_eq!(added.lines().count(), 1, "{added}");
    for word in [
        "--system",
        "--user-group",
        "--no-create-home",
        "--home-dir /nonexistent",
        "--shell /usr/sbin/nologin",
    ] {
        assert!(added.contains(word), "useradd must say {word}: {added}");
    }
    assert!(added.trim_end().ends_with(ACCOUNT), "{added}");

    // The one place BOSS_PROBE_USER is set, with the view beside it and
    // root's own copy — never the checkout's — on the two Exec lines.
    let libexec = h.dir.join("libexec/probe-account");
    let dropin = read(&h.dropin());
    for line in [
        "[Service]".to_string(),
        format!("Environment=BOSS_PROBE_USER={ACCOUNT}"),
        format!("Environment=BOSS_PROBE_DIR={}", h.view().display()),
        // Ahead of an every-minute tick the fetch is bounded short: a
        // oneshot has no start timeout of its own.
        "Environment=PROBE_FETCH_TIMEOUT=30".to_string(),
        format!(
            "Environment=BOSS_PROBE_VERIFIED_FILE={}",
            h.marker().display()
        ),
        format!("ExecStartPre=-{} verify-tick", libexec.display()),
        format!("ExecStopPost=-{} reap", libexec.display()),
    ] {
        assert!(
            dropin.lines().any(|l| l == line),
            "the drop-in must carry `{line}`:\n{dropin}"
        );
    }
    assert_eq!(read(&libexec), read(&script()), "root's copy is this file");

    // The converge writes no marker: only a tick's own verify does, and
    // it names the account the door is about to drop to.
    assert!(!h.marker().exists(), "ensure must not vouch for a tick");
    let (rc, out) = h.tick();
    assert_eq!(rc, 0, "{out}");
    assert_eq!(read(&h.marker()), format!("{ACCOUNT}\n"));

    // The view is the checkout's HEAD, and the system gitconfig names it.
    assert_eq!(
        git(&h.view(), &["rev-parse", "HEAD"]),
        git(&h.src, &["rev-parse", "HEAD"])
    );
    assert!(
        read(&h.dir.join("gitconfig")).contains(&format!("directory = {}", h.view().display())),
        "{}",
        read(&h.dir.join("gitconfig"))
    );
    // No cron, no at, a bounded process count.
    assert_eq!(
        read(&h.dir.join("hostetc/cron.deny")),
        format!("{ACCOUNT}\n")
    );
    assert_eq!(read(&h.dir.join("hostetc/at.deny")), format!("{ACCOUNT}\n"));
    assert!(
        read(&h.dir.join("hostetc/security/limits.d/boss-probe.conf"))
            .contains(&format!("{ACCOUNT} hard nproc 256"))
    );
    // And the converge packet says what it found.
    let summary = read(&h.dir.join("summary.json"));
    assert!(summary.contains("probes run as boss-probe"), "{summary}");
    assert!(summary.contains("probe_account_sudo"), "{summary}");

    // IDEMPOTENT: a second tick creates nothing and appends nothing.
    let before = read(&h.dropin());
    let (rc, out) = h.ensure();
    assert_eq!(rc, 0, "{out}");
    assert_eq!(read(&h.state.join("useradd.log")).lines().count(), 1);
    assert_eq!(
        read(&h.dir.join("hostetc/cron.deny")),
        format!("{ACCOUNT}\n")
    );
    assert_eq!(
        read(&h.dir.join("gitconfig"))
            .matches("directory =")
            .count(),
        1
    );
    assert_eq!(read(&h.dropin()), before);
    assert!(!h.state.join("usermod.log").exists(), "nothing had drifted");
}

/// The stop, as a tick sees it (decision c98c79aa, `fallback`): no
/// marker, and the reason said.
fn assert_stopped(h: &Host, why: &str) {
    let (rc, out) = h.tick();
    assert_eq!(rc, 1, "{out}");
    assert!(out.contains("NO PROBE RUNS THIS TICK"), "{out}");
    assert!(out.contains(why), "the tick must say `{why}`: {out}");
    assert!(
        !h.marker().exists(),
        "a marker was written for an unverified account: {out}"
    );
}

/// The drop-in an unverified host still gets: it names the account and
/// the marker, so the door refuses before runuser — never a line that
/// would let a probe run as anyone else.
fn assert_dropin_stops(h: &Host) {
    let dropin = read(&h.dropin());
    assert!(
        dropin.contains(&format!(
            "Environment=BOSS_PROBE_VERIFIED_FILE={}",
            h.marker().display()
        )),
        "an unverified host's drop-in must name the marker:\n{dropin}"
    );
    assert!(
        dropin.contains(&format!("Environment=BOSS_PROBE_USER={ACCOUNT}")),
        "{dropin}"
    );
}

/// THE FIRST CONVERGE THAT CANNOT MAKE THE ACCOUNT. No account, so no
/// marker on any tick: probes STOP — the drop-in is written anyway, and
/// names the marker the door requires — and the converge is red with the
/// reason. Nothing runs as the checkout's owner. The converge is not the
/// account, so the next one that can make it does, and the very next
/// tick writes the marker: probes resume without a hand.
#[test]
fn a_host_where_the_account_cannot_be_made_stops_probes_and_resumes_by_itself() {
    let h = Host::new("absent");
    write_file(&h.state.join("useradd-fails"), "");
    let (rc, out) = h.ensure();
    assert_eq!(rc, 1, "{out}");
    assert!(out.contains("useradd could not create boss-probe"), "{out}");
    assert!(out.contains("PROBES ARE STOPPED ON THIS HOST"), "{out}");
    assert_dropin_stops(&h);
    let summary = read(&h.dir.join("summary.json"));
    assert!(
        summary.contains("STOPPED: useradd could not create"),
        "{summary}"
    );
    assert!(
        summary.contains("no probe runs on this host until the account verifies"),
        "{summary}"
    );
    assert_stopped(&h, "there is no account boss-probe");

    // A marker left by an earlier tick does not outlive this one.
    write_file(&h.marker(), "boss-probe\n");
    assert_stopped(&h, "there is no account boss-probe");

    // The boring reason passes; nobody touches the host.
    std::fs::remove_file(h.state.join("useradd-fails")).unwrap();
    let (rc, out) = h.ensure();
    assert_eq!(rc, 0, "{out}");
    let (rc, out) = h.tick();
    assert_eq!(rc, 0, "{out}");
    assert_eq!(read(&h.marker()), format!("{ACCOUNT}\n"));
}

/// THE ONE DELAY. A `boss` on the host that predates the marker would
/// ignore it and hand an unverified name to runuser, whose exit 1 reads
/// as NOT PROVEN. With such a CLI and no verified account, no drop-in is
/// written and the packet says the host is not converged onto the rule.
#[test]
fn an_unverified_host_whose_cli_predates_the_stop_gets_no_drop_in_yet() {
    let h = Host::new("old-cli");
    write_file(&h.state.join("useradd-fails"), "");
    write_file(&h.dir.join("boss-cli"), "an older binary\n");
    let (rc, out) = h.ensure();
    assert_eq!(rc, 1, "{out}");
    assert!(!h.dropin().exists(), "{out}");
    assert!(
        read(&h.dir.join("summary.json")).contains("NOT CONVERGED"),
        "{out}"
    );
}

#[test]
fn an_account_in_another_group_is_repaired_and_one_that_stays_there_stops_probes() {
    // In `docker`, say: repaired, read back, and only then vouched for.
    let h = Host::new("grouped").exists();
    write_file(&h.state.join("groups"), "4242 998\n");
    let (rc, out) = h.ensure();
    assert_eq!(rc, 0, "{out}");
    assert!(
        read(&h.state.join("usermod.log")).starts_with("-G "),
        "{out}"
    );
    assert!(h.dropin().exists());
    assert!(!h.state.join("useradd.log").exists(), "it already existed");
    assert_eq!(h.tick().0, 0);

    // The repair is not believed: when the host still answers two
    // groups, probes stop and the packet says STOPPED — and when a later
    // converge's repair does take, the next tick resumes them.
    let h = Host::new("grouped-still").exists();
    write_file(&h.state.join("groups"), "4242 998\n");
    write_file(&h.state.join("usermod-is-ignored"), "");
    let (rc, out) = h.ensure();
    assert_eq!(rc, 1, "{out}");
    assert!(out.contains("has supplementary groups"), "{out}");
    assert_dropin_stops(&h);
    assert!(read(&h.dir.join("summary.json")).contains("STOPPED"));
    assert_stopped(&h, "has supplementary groups");

    std::fs::remove_file(h.state.join("usermod-is-ignored")).unwrap();
    let (rc, out) = h.ensure();
    assert_eq!(rc, 0, "{out}");
    let (rc, out) = h.tick();
    assert_eq!(rc, 0, "{out}");
    assert!(h.marker().exists());
}

#[test]
fn an_account_that_gains_a_sudo_rule_stops_probes_at_the_next_tick() {
    let h = Host::new("sudo").exists();
    write_file(
        &h.state.join("sudo.txt"),
        "User boss-probe may run the following commands on forge:\n    (root) NOPASSWD: /usr/bin/docker\n",
    );
    let (rc, out) = h.ensure();
    assert_eq!(rc, 1, "{out}");
    assert!(out.contains("sudo -l -U boss-probe does not say"), "{out}");
    assert_dropin_stops(&h);
    assert_stopped(&h, "sudo -l -U boss-probe does not say");

    // Verified, proving, and THEN the rule appears: the very next tick
    // has no marker — it does not wait for a converge — the converge
    // that follows is red and says STOPPED, and it cannot repair a
    // sudoers rule, so probes stay stopped until someone removes it.
    let h = Host::new("sudo-later").exists();
    assert_eq!(h.ensure().0, 0);
    assert_eq!(h.tick().0, 0);
    assert!(h.marker().exists());
    write_file(
        &h.state.join("sudo.txt"),
        "User boss-probe may run the following commands on forge:\n    (ALL) ALL\n",
    );
    assert_stopped(&h, "sudo -l -U boss-probe does not say");
    let (rc, out) = h.ensure();
    assert_eq!(rc, 1, "{out}");
    assert_dropin_stops(&h);
    assert!(read(&h.dir.join("summary.json")).contains("STOPPED"));
    assert_stopped(&h, "sudo -l -U boss-probe does not say");

    // The same for a shell: a drift the converge CAN repair stops the
    // tick that sees it and is gone after the next converge.
    let h = Host::new("shell-later").exists();
    assert_eq!(h.ensure().0, 0);
    write_file(&h.state.join("shell"), "/bin/bash\n");
    assert_stopped(&h, "shell is /bin/bash");
    let (rc, out) = h.ensure();
    assert_eq!(rc, 0, "{out}");
    assert_eq!(h.tick().0, 0);
}

#[test]
fn a_name_that_is_already_root_and_a_cron_allow_that_names_it_stop_probes() {
    let h = Host::new("uid0").exists();
    write_file(&h.state.join("uid"), "0\n");
    let (rc, out) = h.ensure();
    assert_eq!(rc, 1, "{out}");
    assert!(out.contains("is uid 0"), "{out}");
    assert_dropin_stops(&h);
    assert_stopped(&h, "is uid 0");

    let h = Host::new("cron-allow").exists();
    write_file(&h.dir.join("hostetc/cron.allow"), "boss-probe\n");
    let (rc, out) = h.ensure();
    assert_eq!(rc, 1, "{out}");
    assert!(out.contains("cron.allow names boss-probe"), "{out}");
    assert_stopped(&h, "cron.allow names boss-probe");
}

/// A view the account can write is not a view to run probes in: with
/// "as the account" landing on the uid that owns the view (the seam set
/// empty), the write test answers yes and probes stop.
#[test]
fn a_view_the_account_can_write_stops_probes() {
    let h = Host::new("writable-view").exists();
    let o = h.command("ensure").env("PROBE_RUNAS", "").output().unwrap();
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("can WRITE the view"), "{out}");
    let o = h
        .command("verify-tick")
        .env("PROBE_RUNAS", "")
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(!h.marker().exists());
}

#[test]
fn a_host_with_no_ops_runner_gets_the_account_and_no_drop_in() {
    let h = Host::new("no-runner");
    std::fs::remove_file(h.dir.join("etc/systemd/boss-ops-runner.service")).unwrap();
    let (rc, out) = h.ensure();
    assert_eq!(rc, 0, "{out}");
    assert!(!h.dropin().exists(), "{out}");
    assert!(read(&h.dir.join("summary.json")).contains("no ops runner on this host"));
}

/// The copy root runs lives alone in a directory: it may source nothing.
/// Driven from such a copy, the view follows the checkout, a second
/// call is a no-op, and a view that is a symlink is refused.
#[test]
fn the_view_follows_the_checkout_from_a_copy_that_stands_alone() {
    let h = Host::new("view");
    let alone = h.dir.join("alone");
    create_dir(&alone);
    let copy = alone.join("probe-account");
    write_exec(&copy, &read(&script()));
    let refresh = |h: &Host| {
        let o = Command::new("bash")
            .arg(&copy)
            .arg("refresh-view")
            .env("BOSS_PROBE_VIEW", h.view())
            .env("BOSS_PROBE_VIEW_SOURCE", &h.src)
            .env("PROBE_SOURCE_RUNAS", "")
            .env_remove("BOSS_FORGE_REPO_DIR")
            .output()
            .unwrap();
        (o.status.code().unwrap_or(-1), text(&o))
    };
    let (rc, out) = refresh(&h);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(
        git(&h.view(), &["rev-parse", "HEAD"]),
        git(&h.src, &["rev-parse", "HEAD"])
    );
    assert!(
        h.view()
            .join("infra/forge/probe-bin/boss-sor-read")
            .exists()
    );

    let (rc, out) = refresh(&h);
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("already"), "{out}");

    write_file(&h.src.join("landed.txt"), "a train arrived\n");
    git(&h.src, &["add", "-A"]);
    git(&h.src, &["commit", "-q", "-m", "two"]);
    let (rc, out) = refresh(&h);
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("moved"), "{out}");
    assert_eq!(
        git(&h.view(), &["show", "HEAD:landed.txt"]),
        "a train arrived"
    );

    // A view that is a link to somewhere else is not the view.
    let h2 = Host::new("view-link");
    create_dir(&h2.dir.join("elsewhere"));
    std::os::unix::fs::symlink(h2.dir.join("elsewhere"), h2.view()).unwrap();
    let (rc, out) = refresh(&h2);
    assert_eq!(rc, 1, "{out}");
    assert!(out.contains("is a symlink"), "{out}");
}

/// `kill -1` reaches every process its caller may signal, so `reap`
/// measures who it would run as first. With a seam that lands on this
/// process — or anywhere but the account — nothing is signalled.
#[test]
fn reap_signals_nothing_unless_it_would_run_as_the_account() {
    let h = Host::new("reap-refused").exists();
    let o = h.command("reap").env("PROBE_RUNAS", "").output().unwrap();
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("reap REFUSED"), "{out}");
    assert!(out.contains("nothing was signalled"), "{out}");

    // No such account: nothing to reap, and that is not a failure —
    // the ops runner's ExecStopPost runs on hosts mid-transition.
    let h = Host::new("reap-absent");
    let o = h.command("reap").output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
}

/// Controls that could not be run are exit 2, never a pass: if running
/// "as the account" lands on some other uid, every refusal below it
/// would be a refusal of the wrong caller.
#[test]
fn controls_that_cannot_run_as_the_account_are_not_a_pass() {
    let h = Host::new("controls-not-run").exists();
    let o = h
        .command("controls")
        .env("PROBE_RUNAS", "")
        .output()
        .unwrap();
    let out = text(&o);
    assert_eq!(o.status.code(), Some(2), "{out}");
    assert!(out.contains("NOT RUN the controls"), "{out}");
    assert!(!out.contains("every control held"), "{out}");
}

/// THE SPOOLS THE CHECKOUT OWNER'S UNITS REPLAY ARE TRIED (backlog
/// dda26693, review acab446c F8a: "the controls try none of the three").
/// The controls read each directory off the units INSTALLED on the host
/// and ask the one question the plant needs answered yes: can the account
/// write the directory, or — when it is not there yet — make it? The
/// account here is a stub whose writes are a list in a file; the kernel's
/// answer for a real second uid is the dev pod's leg below, and the
/// host's is the verb's.
#[test]
fn the_controls_try_every_spool_the_installed_units_name() {
    let h = Host::new("controls-spools").exists();
    let units = h.dir.join("etc/systemd");
    let home = h.dir.join("owner-home");
    let shared = h.dir.join("shared-tmp");
    create_dir(&home.join(".boss-door-state"));
    create_dir(&shared);
    let can_write = h.state.join("account-can-write");
    write_file(&can_write, "");
    // "As the account": uid 4242 with its one group; every write is
    // refused unless the path is listed; nothing is left running.
    let runas = h.dir.join("bin/account-with-listed-writes");
    write_exec(
        &runas,
        "#!/bin/sh\ncase \"$1 $2\" in\n  'id -u'|'id -G') echo 4242; exit 0 ;;\n  'test -w') grep -qxF -- \"$3\" \"$STUB_STATE/account-can-write\"; exit $? ;;\nesac\n[ \"$1\" = setsid ] && exit 0\nexec \"$@\"\n",
    );
    let run = || {
        let o = h
            .command("controls")
            .env("PROBE_RUNAS", &runas)
            .env("PROBE_CONTROL_SECRETS", "")
            .env("PROBE_CONTROL_NOWRITE", "")
            .env("PROBE_CONTROL_SOCKETS", "")
            .output()
            .unwrap();
        let out = text(&o);
        let spool: Vec<String> = out
            .lines()
            .filter(|l| l.contains(" spool: "))
            .map(str::to_string)
            .collect();
        (o.status.code().unwrap_or(-1), spool, out)
    };

    // No installed unit names one: the scripts' defaults are in force,
    // and that is a FAIL, never an absence.
    let (rc, spool, out) = run();
    assert_eq!(rc, 1, "{out}");
    assert_eq!(spool.len(), 1, "{out}");
    assert!(
        spool[0].starts_with("FAIL    spool: no installed unit"),
        "{out}"
    );

    let alert = home.join(".boss-alert-spool");
    let state = home.join(".boss-door-state");
    let planted = shared.join("boss-door-spool");
    write_file(
        &units.join("cluster-watchdog.service"),
        &format!(
            "[Service]\nUser=david\nEnvironment=ALERT_SPOOL={}\nEnvironment=DOOR_STATE_DIR={}\nEnvironment=DOOR_SPOOL_DIR={}\n",
            alert.display(),
            state.display(),
            planted.display()
        ),
    );
    // The same spool named by a second unit is tried once.
    write_file(
        &units.join("cluster-deploy-runner.service"),
        &format!(
            "[Service]\nUser=david\nEnvironment=ALERT_SPOOL={}\n",
            alert.display()
        ),
    );
    // The shared directory is one the account can write, as /var/tmp is.
    write_file(&can_write, &format!("{}\n", shared.display()));
    let (rc, spool, out) = run();
    assert_eq!(rc, 1, "{out}");
    assert_eq!(spool.len(), 3, "one line per directory: {out}");
    let line = |dir: &Path| {
        spool
            .iter()
            .find(|l| l.contains(&format!(" {} ", dir.display())))
            .unwrap_or_else(|| panic!("no line for {}: {out}", dir.display()))
            .clone()
    };
    // Not there yet, in a directory the account cannot write: it cannot
    // be made by the account.
    assert!(line(&alert).starts_with("PASS    spool:"), "{out}");
    assert!(line(&alert).contains("is not there yet"), "{out}");
    // There, and refused.
    assert!(line(&state).starts_with("PASS    spool:"), "{out}");
    // Not there, in a directory the account CAN write: the plant.
    assert!(line(&planted).starts_with("FAIL    spool:"), "{out}");
    assert!(
        line(&planted).contains(&format!("can write {}", shared.display())),
        "{out}"
    );

    // And a directory that exists and takes the account's write.
    write_file(&can_write, &format!("{}\n", state.display()));
    let (_, spool, out) = run();
    let bad = spool
        .iter()
        .find(|l| l.contains(&format!(" {} ", state.display())))
        .unwrap_or_else(|| panic!("{out}"));
    assert!(bad.starts_with("FAIL    spool:"), "{out}");
}

// ── a probe never runs against an old tree, or one that is not root's ──
//
// Review acab446c F2 (backlog dda26693). The question these answer is
// whether a probe can run against a tree that is NOT the converged one and
// so prove a car that has not converged. The script refused in every case
// below when the review mutated it — and no test noticed when it stopped:
// a marker written though the refresh failed, a failed fetch read as
// current, a view another uid owns, an unchecked pack, root's git running
// the view's hooks, the account reading a different tree than root.
// Each test here was seen red against the mutant it names.

/// A train arrives at the fixture checkout: one more commit.
fn land(h: &Host, name: &str) -> String {
    write_file(&h.src.join(name), "a train arrived\n");
    git(&h.src, &["add", "-A"]);
    git(&h.src, &["commit", "-q", "-m", name]);
    git(&h.src, &["rev-parse", "HEAD"])
}

/// A verified host whose view is at the checkout's first commit, with
/// the marker a good tick wrote still standing.
fn verified(name: &str) -> (Host, String) {
    let h = Host::new(name).exists();
    let (rc, out) = h.ensure();
    assert_eq!(rc, 0, "{out}");
    let (rc, out) = h.tick();
    assert_eq!(rc, 0, "{out}");
    assert!(h.marker().exists(), "{out}");
    let one = git(&h.view(), &["rev-parse", "HEAD"]);
    (h, one)
}

/// One tick with seams of this test's own.
fn tick_with(h: &Host, envs: &[(&str, &Path)]) -> (i32, String) {
    let mut c = h.command("verify-tick");
    for (k, v) in envs {
        c.env(k, v);
    }
    let o = c.output().unwrap();
    (o.status.code().unwrap_or(-1), text(&o))
}

/// No marker, the reason said, and the view still where it was.
fn assert_no_marker_over(h: &Host, tick: &(i32, String), why: &str, view_at: &str) {
    let (rc, out) = tick;
    assert_eq!(*rc, 1, "{out}");
    assert!(out.contains(why), "the tick must say `{why}`: {out}");
    assert!(
        !h.marker().exists(),
        "a marker stands over a view that is not the converged tree: {out}"
    );
    assert_eq!(git(&h.view(), &["rev-parse", "HEAD"]), view_at, "{out}");
}

/// T3. The checkout's HEAD cannot be read this tick (its owner's git
/// answers nothing): the view stays at the last tree it had, and the
/// marker the previous tick wrote is gone and not written again.
#[test]
fn a_tick_that_cannot_make_the_view_current_writes_no_marker() {
    let (h, one) = verified("stale-refresh");
    land(&h, "two.txt");
    let dark = h.dir.join("bin/owner-is-dark");
    write_exec(&dark, "#!/bin/sh\nexit 1\n");
    let t = tick_with(&h, &[("PROBE_SOURCE_RUNAS", &dark)]);
    assert_no_marker_over(&h, &t, "refresh-view FAILED", &one);
    assert!(
        t.1.contains("NO PROBE RUNS THIS TICK — the view could not be made current"),
        "{}",
        t.1
    );
    // The control: the same host, its checkout readable again.
    let (rc, out) = h.tick();
    assert_eq!(rc, 0, "{out}");
    assert!(h.marker().exists());
    assert_eq!(
        git(&h.view(), &["rev-parse", "HEAD"]),
        git(&h.src, &["rev-parse", "HEAD"])
    );
}

/// R6. The checkout moved and the fetch failed (the serving side
/// answers nothing). The view holds an OLDER FETCH_HEAD from the last
/// good fetch — a failed fetch read as a current one would check that
/// out and call the view moved.
#[test]
fn a_fetch_that_fails_is_not_a_current_view() {
    let (h, one) = verified("failed-fetch");
    let two = land(&h, "two.txt");
    let no_serve = h.dir.join("bin/owner-serves-nothing");
    write_exec(
        &no_serve,
        "#!/bin/sh\n[ \"$2\" = upload-pack ] && exit 1\nexec \"$@\"\n",
    );
    let t = tick_with(&h, &[("PROBE_SOURCE_RUNAS", &no_serve)]);
    assert_no_marker_over(&h, &t, "refresh-view FAILED — fetching", &one);
    let (rc, out) = h.tick();
    assert_eq!(rc, 0, "{out}");
    assert_eq!(git(&h.view(), &["rev-parse", "HEAD"]), two);
}

/// R1. A view some other uid owns is one that uid can rewrite under a
/// probe: refused, whatever it holds. (The owner here is the stub's
/// answer; a real second uid is the dev pod's leg below.)
#[test]
fn a_view_another_uid_owns_is_not_the_view() {
    let (h, one) = verified("foreign-view");
    land(&h, "two.txt");
    write_file(&h.state.join("view-owner"), "31337\n");
    let t = h.tick();
    assert_no_marker_over(&h, &t, "refresh-view REFUSED", &one);
    assert!(t.1.contains("is owned by uid 31337"), "{}", t.1);
    // And the converge says STOPPED for it.
    let (rc, out) = h.ensure();
    assert_eq!(rc, 1, "{out}");
    assert!(read(&h.dir.join("summary.json")).contains("STOPPED"));
}

/// R4. What root parses from the checkout's owner is a pack, CHECKED. A
/// commit git itself calls malformed is served by an unchecked fetch
/// without complaint (the control) and never reaches the view.
#[test]
fn a_malformed_object_in_the_checkout_never_reaches_the_view() {
    let (h, one) = verified("fsck");
    let tree = git(&h.src, &["rev-parse", "HEAD^{tree}"]);
    let mut child = Command::new("git")
        .arg("-C")
        .arg(&h.src)
        .args([
            "hash-object",
            "-t",
            "commit",
            "-w",
            "--stdin",
            "--literally",
        ])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    boss_testing::feed_stdin(
        &mut child,
        format!("tree {tree}\nparent {one}\n\nno author line\n").as_bytes(),
    );
    let o = child.wait_with_output().unwrap();
    assert!(o.status.success(), "{}", text(&o));
    let bad = String::from_utf8_lossy(&o.stdout).trim().to_string();
    git(&h.src, &["update-ref", "HEAD", &bad]);
    assert_eq!(git(&h.src, &["rev-parse", "HEAD^{commit}"]), bad);

    // The control: an unchecked fetch takes it.
    let unchecked = h.dir.join("unchecked");
    create_dir(&unchecked);
    git(&unchecked, &["init", "-q"]);
    git(
        &unchecked,
        &["fetch", "-q", "--no-tags", h.src.to_str().unwrap(), "HEAD"],
    );
    assert_eq!(git(&unchecked, &["rev-parse", "FETCH_HEAD"]), bad);

    let t = h.tick();
    assert_no_marker_over(&h, &t, "refresh-view FAILED — fetching", &one);
    assert!(
        t.1.contains("fsck"),
        "the refusal is the check's own: {}",
        t.1
    );
}

/// R5. Root's git runs no hook in the view. Planted hooks would run on
/// the checkout that moves the view (the control: the same checkout by
/// a git that does not disable them runs both).
#[test]
fn roots_git_runs_no_hook_in_the_view() {
    let (h, _) = verified("hooks");
    let ran = h.dir.join("hook-ran");
    for hook in ["post-checkout", "reference-transaction"] {
        write_exec(
            &h.view().join(".git/hooks").join(hook),
            &format!("#!/bin/sh\necho {hook} >> '{}'\n", ran.display()),
        );
    }
    let two = land(&h, "two.txt");
    let (rc, out) = h.tick();
    assert_eq!(rc, 0, "{out}");
    assert_eq!(git(&h.view(), &["rev-parse", "HEAD"]), two);
    assert!(
        !ran.exists(),
        "root's git ran a hook in the view: {}",
        read(&ran)
    );
    git(&h.view(), &["checkout", "-qf", "--detach", "HEAD~1"]);
    assert!(
        read(&ran).contains("post-checkout"),
        "the control: a git that leaves hooks on runs them ({})",
        read(&ran)
    );

    // THE FETCH'S OWN TWO SETTINGS, AS TEXT. No hook fires on a fetch
    // that updates no ref (measured with pre-auto-gc, reference-
    // transaction and post-checkout planted, git 2.39.5), so the fetch's
    // hook setting cannot be shown by effect; the check beside it is
    // (the test above). Both ride on one command line.
    assert!(
        read(&script()).contains(
            "git -c core.hooksPath=/dev/null -c fetch.fsckObjects=true -C \"$VIEW\" \\\n"
        ),
        "the view's fetch must disable hooks and check every object"
    );
}

/// V7. Root reads the view at one commit and the account, asked the same
/// question, answers another (or cannot answer): the account is not
/// reading the tree this tick vouches for.
#[test]
fn an_account_that_reads_another_tree_than_roots_stops_probes() {
    let (h, one) = verified("other-tree");
    land(&h, "two.txt");
    assert_eq!(h.ensure().0, 0);
    let older = h.dir.join("bin/account-sees-the-old-tree");
    write_exec(
        &older,
        &format!(
            "#!/bin/sh\nif [ \"$1\" = test ] && [ \"$2\" = -w ]; then exit 1; fi\nif [ \"$1\" = git ] && [ \"$4\" = rev-parse ]; then echo {one}; exit 0; fi\nexec \"$@\"\n"
        ),
    );
    let (rc, out) = tick_with(&h, &[("PROBE_RUNAS", &older)]);
    assert_eq!(rc, 1, "{out}");
    assert!(out.contains("cannot read the view"), "{out}");
    assert!(out.contains(&one), "the tick says what it answered: {out}");
    assert!(!h.marker().exists(), "{out}");

    let refused = h.dir.join("bin/account-is-refused-the-view");
    write_exec(
        &refused,
        "#!/bin/sh\nif [ \"$1\" = test ] && [ \"$2\" = -w ]; then exit 1; fi\nif [ \"$1\" = git ]; then echo 'fatal: detected dubious ownership in repository' >&2; exit 128; fi\nexec \"$@\"\n",
    );
    let (rc, out) = tick_with(&h, &[("PROBE_RUNAS", &refused)]);
    assert_eq!(rc, 1, "{out}");
    assert!(
        out.contains("cannot read the view: git (exit 128)"),
        "{out}"
    );
    assert!(!h.marker().exists(), "{out}");
}

/// V8, and the other half of the write test. A view without the reader
/// a probe is promised is not a view to run probes in; and `.git` being
/// writable is the view being writable, whatever the working tree says.
#[test]
fn a_view_without_the_reader_or_with_a_writable_git_dir_stops_probes() {
    let h = Host::new("no-reader").exists();
    git(&h.src, &["rm", "-q", "infra/forge/probe-bin/boss-sor-read"]);
    git(&h.src, &["commit", "-q", "-m", "the reader is gone"]);
    let (rc, out) = h.ensure();
    assert_eq!(rc, 1, "{out}");
    let why = "cannot read HEAD:infra/forge/probe-bin/boss-sor-read in the view";
    assert!(out.contains(why), "{out}");
    assert_stopped(&h, why);

    let (h, _) = verified("writable-git-dir");
    let writes_git = h.dir.join("bin/account-writes-dot-git");
    write_exec(
        &writes_git,
        "#!/bin/sh\nif [ \"$1\" = test ] && [ \"$2\" = -w ]; then case \"$3\" in */.git) exit 0 ;; *) exit 1 ;; esac; fi\nexec \"$@\"\n",
    );
    let (rc, out) = tick_with(&h, &[("PROBE_RUNAS", &writes_git)]);
    assert_eq!(rc, 1, "{out}");
    assert!(out.contains("can WRITE the view"), "{out}");
    assert!(!h.marker().exists(), "{out}");

    // And the working tree alone, with `.git` refused.
    let writes_tree = h.dir.join("bin/account-writes-the-tree");
    write_exec(
        &writes_tree,
        "#!/bin/sh\nif [ \"$1\" = test ] && [ \"$2\" = -w ]; then case \"$3\" in */.git) exit 1 ;; *) exit 0 ;; esac; fi\nexec \"$@\"\n",
    );
    let (rc, out) = tick_with(&h, &[("PROBE_RUNAS", &writes_tree)]);
    assert_eq!(rc, 1, "{out}");
    assert!(out.contains("can WRITE the view"), "{out}");
    assert!(!h.marker().exists(), "{out}");
}

/// R3. What the checkout's owner's git prints is data from another
/// account: an answer that is not a commit name is refused before it is
/// compared with anything, though the same owner would serve a fetch.
#[test]
fn an_owner_whose_git_answers_something_that_is_not_a_commit_is_refused() {
    let (h, one) = verified("not-a-commit");
    land(&h, "two.txt");
    let words = h.dir.join("bin/owner-answers-words");
    write_exec(
        &words,
        "#!/bin/sh\nfor a in \"$@\"; do [ \"$a\" = rev-parse ] && { echo 'HEAD; echo planted'; exit 0; }; done\nexec \"$@\"\n",
    );
    let t = tick_with(&h, &[("PROBE_SOURCE_RUNAS", &words)]);
    assert_no_marker_over(&h, &t, "something that is not a commit", &one);
}

/// The marker itself: a tick that cannot place it exactly where the
/// drop-in says, or cannot clear the last one, vouches for nothing.
#[test]
fn a_marker_that_cannot_be_cleared_or_written_is_no_marker() {
    let (h, _) = verified("marker-path");
    // Not an absolute path — and no name at all — is refused before
    // anything is written relative to wherever the tick happens to run.
    let o = h
        .command("verify-tick")
        .env("BOSS_PROBE_VERIFIED_FILE", "run/relative.verified")
        .current_dir(&h.dir)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(text(&o).contains("is not an absolute path"), "{}", text(&o));
    assert!(!h.dir.join("run/relative.verified").exists());
    let o = h
        .command("verify-tick")
        .env_remove("BOSS_PROBE_VERIFIED_FILE")
        .current_dir(&h.dir)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(text(&o).contains("is not an absolute path"), "{}", text(&o));

    // The last tick's marker cannot be removed (here: it is a directory
    // with something in it). Nothing is written into or beside it.
    let stuck = h.dir.join("run/stuck.verified");
    create_dir(&stuck);
    write_file(&stuck.join("kept"), "");
    let (rc, out) = tick_with(&h, &[("BOSS_PROBE_VERIFIED_FILE", &stuck)]);
    assert_eq!(rc, 1, "{out}");
    assert!(out.contains("could not be removed"), "{out}");
    assert_eq!(std::fs::read_dir(&stuck).unwrap().count(), 1, "{out}");

    // The marker cannot be written (its directory is not there): the
    // tick fails rather than report a marker it did not place.
    let nowhere = h.dir.join("no-such-run/probe-account.verified");
    let (rc, out) = tick_with(&h, &[("BOSS_PROBE_VERIFIED_FILE", &nowhere)]);
    assert_eq!(rc, 1, "{out}");
    assert!(out.contains("could not write"), "{out}");
    assert!(!nowhere.exists());
}

/// The two identity answers nothing else pinned: a primary group that is
/// root's stops probes, and an account whose password is not locked is
/// locked by the converge.
#[test]
fn a_root_primary_group_stops_probes_and_an_unlocked_account_is_locked() {
    let h = Host::new("gid0").exists();
    write_file(&h.state.join("gid"), "0\n");
    write_file(&h.state.join("groups"), "0\n");
    let (rc, out) = h.ensure();
    assert_eq!(rc, 1, "{out}");
    assert!(out.contains("primary group is gid 0"), "{out}");
    assert_stopped(&h, "primary group is gid 0");

    let h = Host::new("at-allow").exists();
    write_file(&h.dir.join("hostetc/at.allow"), "boss-probe\n");
    let (rc, out) = h.ensure();
    assert_eq!(rc, 1, "{out}");
    assert_stopped(&h, "at.allow names boss-probe");

    let h = Host::new("unlocked").exists();
    write_file(&h.state.join("passwd-status"), "P\n");
    let (rc, out) = h.ensure();
    assert_eq!(rc, 0, "{out}");
    assert_eq!(
        read(&h.state.join("usermod.log")),
        format!("-L {ACCOUNT}\n"),
        "{out}"
    );
    assert!(
        read(&h.dir.join("summary.json")).contains("(locked)"),
        "{out}"
    );
}

// ── the kernel's own answers: root with a second uid ──────────────────

fn my_uid() -> u32 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self")
        .map(|m| m.uid())
        .unwrap_or(u32::MAX)
}

fn runas(uid: u32) -> String {
    format!("setpriv --reuid={uid} --regid={uid} --clear-groups")
}

fn as_uid(uid: u32, args: &[&str]) -> Output {
    Command::new("setpriv")
        .args([
            &format!("--reuid={uid}"),
            &format!("--regid={uid}"),
            "--clear-groups",
        ])
        .args(args)
        .current_dir("/")
        .output()
        .unwrap()
}

/// Two uids nobody on this box runs as, which this process can become
/// and which can reach `dir`. `None` — with the reason printed — under
/// the gate's uid, or where the scratch root is closed to other uids.
fn spare_uids(slot: u32, dir: &Path, what: &str) -> Option<(u32, u32)> {
    let not_run = |why: &str| {
        println!(
            "NOT RUN {what}: {why} — the kernel's answer for a second uid is proven on the dev pod and by the probe-account-controls verb on the forge"
        );
        None
    };
    if my_uid() != 0 {
        return not_run(&format!(
            "this process is uid {}, which can become no other",
            my_uid()
        ));
    }
    let base = 61000 + (std::process::id() % 1500) * 2 + slot * 3000;
    let (a, b) = (base, base + 1);
    for uid in [a, b] {
        let Ok(o) = Command::new("setpriv")
            .args([
                &format!("--reuid={uid}"),
                &format!("--regid={uid}"),
                "--clear-groups",
                "test",
                "-x",
            ])
            .arg(dir)
            .output()
        else {
            return not_run("setpriv is not on this box");
        };
        if !o.status.success() {
            return not_run(&format!(
                "uid {uid} cannot be assumed here, or cannot reach {}",
                dir.display()
            ));
        }
        let busy = Command::new("pgrep")
            .args(["-U", &uid.to_string()])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(true);
        if busy {
            return not_run(&format!("uid {uid} already runs something on this box"));
        }
    }
    Some((a, b))
}

/// Point the fixture's stubs at a REAL second uid.
fn real_account(h: &Host, uid: u32) {
    for f in ["uid", "gid", "groups"] {
        write_file(&h.state.join(f), &format!("{uid}\n"));
    }
    write_file(&h.state.join("shell"), "/usr/sbin/nologin\n");
}

/// The view is root's; a real other uid reads it only once the system
/// gitconfig names it, and cannot write it either way. The checkout is
/// a THIRD uid's, which root's own git refuses to read — and the fetch
/// is served as that owner, so it never has to.
#[test]
fn a_second_uid_reads_the_view_through_the_system_gitconfig_and_cannot_write_it() {
    let h = Host::new("dac");
    let Some((acct, owner)) = spare_uids(0, &h.dir, "the view's ownership legs") else {
        return;
    };
    // A checkout its owner made, in a directory only it can write.
    let owned = h.dir.join("owned");
    create_dir(&owned);
    assert!(
        Command::new("chmod")
            .arg("0777")
            .arg(&owned)
            .status()
            .unwrap()
            .success()
    );
    let src = owned.join("checkout");
    let src_s = src.display().to_string();
    let home = format!("HOME={}", owned.display());
    for args in [
        vec![
            "env",
            &home,
            "GIT_CONFIG_NOSYSTEM=1",
            "git",
            "init",
            "-q",
            &src_s,
        ],
        vec![
            "env",
            &home,
            "GIT_CONFIG_NOSYSTEM=1",
            "git",
            "-C",
            &src_s,
            "-c",
            "user.name=o",
            "-c",
            "user.email=o@example.invalid",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "one",
        ],
    ] {
        let o = as_uid(owner, &args);
        assert!(o.status.success(), "{}", text(&o));
    }
    // THE REFUSAL THIS DESIGN ROUTES AROUND, shown first: root's own git
    // will not read a repository another uid owns.
    let o = Command::new("git")
        .arg("-C")
        .arg(&src)
        .args(["rev-parse", "HEAD"])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(text(&o).contains("dubious ownership"), "{}", text(&o));

    let o = Command::new("bash")
        .arg(script())
        .arg("refresh-view")
        .env("BOSS_PROBE_VIEW", h.view())
        .env("BOSS_PROBE_VIEW_SOURCE", &src)
        .env("PROBE_SOURCE_RUNAS", runas(owner))
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    let head =
        String::from_utf8_lossy(&as_uid(owner, &["git", "-C", &src_s, "rev-parse", "HEAD"]).stdout)
            .trim()
            .to_string();
    assert_eq!(git(&h.view(), &["rev-parse", "HEAD"]), head);

    // The account, without the setting: refused. With it: the same HEAD.
    let view = h.view().display().to_string();
    let o = as_uid(
        acct,
        &[
            "env",
            "GIT_CONFIG_NOSYSTEM=1",
            "git",
            "-C",
            &view,
            "rev-parse",
            "HEAD",
        ],
    );
    assert!(!o.status.success());
    assert!(text(&o).contains("dubious ownership"), "{}", text(&o));
    let cfg = h.dir.join("gitconfig");
    git(
        &h.dir,
        &[
            "config",
            "--file",
            &cfg.display().to_string(),
            "--add",
            "safe.directory",
            &view,
        ],
    );
    let sys = format!("GIT_CONFIG_SYSTEM={}", cfg.display());
    let o = as_uid(
        acct,
        &["env", &sys, "git", "-C", &view, "rev-parse", "HEAD"],
    );
    assert_eq!(
        String::from_utf8_lossy(&o.stdout).trim(),
        head,
        "{}",
        text(&o)
    );

    // And it cannot write what it reads.
    for target in [
        view.clone(),
        format!("{view}/.git"),
        format!("{view}/.git/HEAD"),
    ] {
        assert!(
            !as_uid(acct, &["test", "-w", &target]).status.success(),
            "{target}"
        );
    }
    assert!(
        !as_uid(acct, &["touch", &format!("{view}/planted")])
            .status
            .success()
    );
    let _ = as_uid(owner, &["rm", "-rf", &src_s]);
}

/// A probe that leaves a setsid child and a temp file behind: after
/// `reap`, the account has no process and the file is gone.
#[test]
fn a_setsid_child_and_a_temp_file_the_account_left_are_gone_after_reap() {
    let h = Host::new("reap");
    let Some((acct, _)) = spare_uids(1, &h.dir, "reap") else {
        return;
    };
    real_account(&h, acct);
    let tmp = h.dir.join("tmp");
    create_dir(&tmp);
    assert!(
        Command::new("chmod")
            .arg("1777")
            .arg(&tmp)
            .status()
            .unwrap()
            .success()
    );
    let left = tmp.join("left-behind");
    assert!(
        as_uid(acct, &["touch", &left.display().to_string()])
            .status
            .success()
    );
    let mut child = Command::new("setpriv")
        .args([
            &format!("--reuid={acct}"),
            &format!("--regid={acct}"),
            "--clear-groups",
            "setsid",
            "sleep",
            "600",
        ])
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(400));
    let running = |uid: u32| {
        Command::new("pgrep")
            .args(["-U", &uid.to_string()])
            .output()
            .unwrap()
            .status
            .success()
    };
    assert!(
        running(acct),
        "the fixture's own child must be running first"
    );

    let o = h
        .command("reap")
        .env("PROBE_RUNAS", runas(acct))
        .env("PROBE_TMP_DIRS", &tmp)
        .output()
        .unwrap();
    let out = text(&o);
    let _ = child.wait();
    assert_eq!(o.status.code(), Some(0), "{out}");
    assert!(out.contains("left behind were killed"), "{out}");
    assert!(!running(acct), "{out}");
    assert!(
        !left.exists(),
        "the account's temp file must be gone: {out}"
    );
}

/// The controls against a fixture host with a REAL second uid: green
/// when every path refuses it, red — naming the file — when one secret
/// is made readable or one protected path writable.
#[test]
fn the_controls_hold_for_a_real_second_uid_and_go_red_when_a_secret_opens() {
    let h = Host::new("controls");
    let Some((acct, _)) = spare_uids(2, &h.dir, "the controls") else {
        return;
    };
    real_account(&h, acct);
    // The view, made the way the host makes it.
    let o = h.command("refresh-view").output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    let cfg = h.dir.join("gitconfig");
    git(
        &h.dir,
        &[
            "config",
            "--file",
            &cfg.display().to_string(),
            "--add",
            "safe.directory",
            &h.view().display().to_string(),
        ],
    );
    let secrets = h.dir.join("boss-ops");
    create_dir(&secrets);
    write_file(&secrets.join("kubeconfig"), "not a credential\n");
    assert!(
        Command::new("chmod")
            .arg("0600")
            .arg(secrets.join("kubeconfig"))
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("chmod")
            .arg("0700")
            .arg(&secrets)
            .status()
            .unwrap()
            .success()
    );
    let units = h.dir.join("etc/systemd");
    // The spools the owner's units replay (backlog dda26693): one not
    // made yet under a home only its owner can write, one that exists.
    let owner_home = h.dir.join("owner-home");
    let alert_spool = owner_home.join(".boss-alert-spool");
    let door_state = owner_home.join(".boss-door-state");
    create_dir(&door_state);
    write_file(
        &units.join("cluster-watchdog.service"),
        &format!(
            "[Service]\nEnvironment=ALERT_SPOOL={}\nEnvironment=DOOR_STATE_DIR={}\n",
            alert_spool.display(),
            door_state.display()
        ),
    );
    let run = |h: &Host| {
        let o = h
            .command("controls")
            .env("PROBE_RUNAS", runas(acct))
            .env("GIT_CONFIG_SYSTEM", &cfg)
            .env("PROBE_TMP_DIRS", h.dir.join("no-such-tmp"))
            .env(
                "PROBE_CONTROL_SECRETS",
                format!("{} {}", secrets.display(), h.dir.join("absent").display()),
            )
            .env(
                "PROBE_CONTROL_NOWRITE",
                format!("{} {}", h.view().display(), units.display()),
            )
            .env("PROBE_CONTROL_SOCKETS", "")
            .output()
            .unwrap();
        (o.status.code().unwrap_or(-1), text(&o))
    };
    let (rc, out) = run(&h);
    assert_eq!(rc, 0, "{out}");
    for want in [
        "PASS    method:",
        "PASS    identity: no group but its own",
        &format!(
            "PASS    read: {} refused (1 file(s) tried",
            secrets.display()
        ),
        &format!("ABSENT  read: {}", h.dir.join("absent").display()),
        &format!("PASS    write: {} refused", h.view().display()),
        "PASS    signal:",
        "PASS    ptrace:",
        "PASS    leftover: a setsid child of boss-probe was running and is gone after reap",
        "PASS    probe: git reads the view's HEAD",
        "PASS    probe: git show HEAD:<path> reads the converged tree",
        &format!(
            "PASS    spool: {} is not there yet and boss-probe cannot make it",
            alert_spool.display()
        ),
        &format!("PASS    spool: {} refused", door_state.display()),
        "every control held",
    ] {
        assert!(out.contains(want), "missing `{want}`:\n{out}");
    }

    // RED: the spool's directory becomes what /var/tmp is — sticky and
    // open to every account. The spool is still not there, and now the
    // account can make it; then it does, and the kernel lets it.
    let chmod = |mode: &str, p: &Path| {
        assert!(
            Command::new("chmod")
                .arg(mode)
                .arg(p)
                .status()
                .unwrap()
                .success()
        );
    };
    chmod("1777", &owner_home);
    let (rc, out) = run(&h);
    assert_eq!(rc, 1, "{out}");
    assert!(
        out.contains(&format!(
            "FAIL    spool: {} — boss-probe can write {}",
            alert_spool.display(),
            owner_home.display()
        )),
        "{out}"
    );
    let planted = as_uid(acct, &["mkdir", &alert_spool.display().to_string()]);
    assert!(
        planted.status.success(),
        "the plant this control is about: {}",
        text(&planted)
    );
    let _ = as_uid(acct, &["rmdir", &alert_spool.display().to_string()]);
    chmod("0755", &owner_home);
    assert!(
        !as_uid(acct, &["mkdir", &alert_spool.display().to_string()])
            .status
            .success(),
        "closed again, the account cannot make the spool"
    );
    assert_eq!(run(&h).0, 0);

    // RED: the secret opens.
    assert!(
        Command::new("chmod")
            .arg("0755")
            .arg(&secrets)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("chmod")
            .arg("0644")
            .arg(secrets.join("kubeconfig"))
            .status()
            .unwrap()
            .success()
    );
    let (rc, out) = run(&h);
    assert_eq!(rc, 1, "{out}");
    assert!(
        out.contains(&format!(
            "FAIL    read: boss-probe can read {}",
            secrets.join("kubeconfig").display()
        )),
        "{out}"
    );
    assert!(out.contains("did NOT hold"), "{out}");
    assert!(
        Command::new("chmod")
            .arg("0700")
            .arg(&secrets)
            .status()
            .unwrap()
            .success()
    );

    // RED: a unit directory it can write.
    assert!(
        Command::new("chmod")
            .arg("0777")
            .arg(&units)
            .status()
            .unwrap()
            .success()
    );
    let (rc, out) = run(&h);
    assert_eq!(rc, 1, "{out}");
    assert!(
        out.contains(&format!(
            "FAIL    write: boss-probe can write {}",
            units.display()
        )),
        "{out}"
    );
}

// ── where it is wired ────────────────────────────────────────────────

/// install.sh runs `ensure` on every converge, AFTER the ops runner's
/// installer (whose drop-in directory it writes beside) and BEFORE the
/// daemon-reload, and carries its exit: a host where the account cannot
/// be made still installs every unit and enables every timer.
#[test]
fn the_forge_install_runs_ensure_and_carries_its_exit() {
    let install = read(&repo_root().join("infra/forge/install.sh"));
    let call = install
        .find("probe-account.sh\" ensure || probe_account_rc=$?")
        .expect("install.sh must run probe-account.sh ensure and carry its exit");
    let runner = install.find("install-ops-runner.sh\" forge").unwrap();
    let reload = install.find("\"$SYSTEMCTL\" daemon-reload").unwrap();
    assert!(
        runner < call && call < reload,
        "after the ops runner, before the reload"
    );
    assert!(
        install.contains("exit \"$probe_account_rc\""),
        "the carried exit reds the run at the end, after every unit is installed"
    );
}

/// The controls are an ops verb — a machine-run receipt on a packet —
/// for the forge only, READ-ONLY, taking no argument. And the probe verb
/// itself is untouched: the account arrives through the runner's
/// environment, not through a second definition of how a probe is run.
#[test]
fn the_controls_are_a_read_verb_on_the_forge() {
    let verb: serde_json::Value = serde_json::from_str(&read(
        &repo_root().join("infra/ops/verbs/probe-account-controls.json"),
    ))
    .unwrap();
    assert_eq!(verb["hosts"], serde_json::json!(["forge"]));
    assert_eq!(
        verb["argv"],
        serde_json::json!(["infra/forge/probe-account.sh", "controls"])
    );
    assert_eq!(verb["params"], serde_json::json!([]));
    let about = verb["about"].as_str().unwrap();
    assert!(about.starts_with("READ-ONLY"), "{about}");
    assert!(!about.contains("MUTATING"), "{about}");

    let probe: serde_json::Value = serde_json::from_str(&read(
        &repo_root().join("infra/ops/verbs/run-car-probe.json"),
    ))
    .unwrap();
    assert_eq!(
        probe["argv"],
        serde_json::json!(["boss", "prove", "{1}", "--from-car", "--unattended"])
    );
}
