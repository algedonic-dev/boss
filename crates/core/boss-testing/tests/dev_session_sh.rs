//! `infra/dev/dev-session.sh` — the dev pod's login selector (backlog
//! e2d63c28, David 2026-10-01).
//!
//! An interactive ssh login used to exec straight into the one durable
//! tmux session, `dev` (the Remote Control / drain session). It now
//! offers four: 1 Claude + Claude Code (`dev`), 2 GPT + Codex (`codex`),
//! 3 Gemini + Code Assist (`gemini`), 4 Shell (`shell`). Each joins its
//! session when it runs and creates it when it does not, and Enter or the
//! timeout is option 1, so anything that relied on landing in `dev` still
//! lands there. The selector is VERSIONED in the checkout and the
//! manifest's /work/dev-session.sh only execs it, so a menu edit rides a
//! train instead of rolling the pod.
//!
//! Pinned against a stub `tmux` (sessions it "has" come from
//! STUB_SESSIONS; every call is logged, and `new-session` prints what it
//! was asked for and exits, standing in for the attach), a stub `npm` and
//! `curl` for the install offer, and a fixture root in place of `/work`
//! (`BOSS_DEV_ROOT`) whose `home/.local/bin` holds whichever CLIs a case
//! installs. PATH is the stubs plus `bash` alone, so a CLI really
//! installed on the machine running this suite cannot answer for a stub.
//! A terminal is asserted with `BOSS_DEV_SESSION_ASSUME_TTY=1` (a test
//! cannot hand the script a pty); the non-interactive case runs without
//! it, against the script's real `[ -t 0 ]`.

use boss_testing::{create_dir, repo_root, scratch_dir, write_exec};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const SCRIPT: &str = "infra/dev/dev-session.sh";

const TMUX_STUB: &str = r#"#!/usr/bin/env bash
echo "tmux $*" >> "$STUB_LOG"
case "$1" in
  has-session)
    t=${3#=}
    case " ${STUB_SESSIONS:-} " in *" $t "*) exit 0;; esac
    exit 1
    ;;
  new-session)
    echo "ATTACHED tmux $* HOME=$HOME" >> "$STUB_LOG"
    exit 0
    ;;
esac
exit 0
"#;

/// `npm i -g --prefix <p> <pkg>` leaves `<p>/bin/<cli>`, as the real
/// one does — unless the case asks it to fail.
const NPM_STUB: &str = r#"#!/usr/bin/env bash
echo "npm $*" >> "$STUB_LOG"
[ "${STUB_NPM_FAILS:-}" = 1 ] && { echo "npm ERR! network" >&2; exit 1; }
prefix=$4
case "$5" in
  @openai/codex) cli=codex ;;
  @google/gemini-cli) cli=gemini ;;
  *) exit 9 ;;
esac
mkdir -p "$prefix/bin"
printf '#!/usr/bin/env bash\n' > "$prefix/bin/$cli"
chmod +x "$prefix/bin/$cli"
"#;

/// The pod's `boss-api` door, its streams split as the real one splits
/// them (infra/dev/boss-api: the body on stdout, `HTTP:<code>` on
/// STDERR, exit 1 on a non-2xx — review 23f1c6fd B2: a stub that put
/// the code on stdout let a check pass here that the real door never
/// satisfies). `GET /api/agents` answers STUB_AGENTS with
/// STUB_API_STATUS (default 200), or fails when the API is dark.
const BOSS_API_STUB: &str = r#"#!/usr/bin/env bash
echo "boss-api $*" >> "$STUB_LOG"
[ "${STUB_API_DOWN:-}" = 1 ] && { echo "curl: (7) Failed to connect" >&2; exit 7; }
[ "$1 $2" = "GET /api/agents" ] || exit 9
code=${STUB_API_STATUS:-200}
printf '%s\n' "$STUB_AGENTS"
printf 'HTTP:%s\n' "$code" >&2
case $code in 2??) exit 0 ;; *) exit 1 ;; esac
"#;

/// The registry as it stands with both new agents posted.
const AGENTS_ALL: &str = r#"{"data":[{"aliases":["claude@algedonic.dev"],"id":"agent-claude"},{"aliases":["codex@algedonic.dev"],"id":"agent-codex"},{"aliases":["gemini@algedonic.dev"],"id":"agent-gemini"}],"total":3}"#;

/// The registry as it stood on 2026-10-01: Claude alone.
const AGENTS_CLAUDE_ONLY: &str =
    r#"{"data":[{"aliases":["claude@algedonic.dev"],"id":"agent-claude"}],"total":1}"#;

struct Fixture {
    root: PathBuf,
    path: String,
    log: PathBuf,
}

impl Fixture {
    /// A fixture `/work`: home, boss checkout, dev-claude.sh, and the
    /// CLIs named in `clis` on the PVC's `home/.local/bin`.
    fn new(name: &str, clis: &[&str]) -> Self {
        let root = scratch_dir(&format!("dev-session-{name}"));
        let stubs = root.join("stubs");
        create_dir(&stubs);
        write_exec(&stubs.join("tmux"), TMUX_STUB);
        write_exec(&stubs.join("npm"), NPM_STUB);
        write_exec(
            &stubs.join("curl"),
            "#!/usr/bin/env bash\necho \"curl $*\" >> \"$STUB_LOG\"\nexit 22\n",
        );
        write_exec(&stubs.join("boss-api"), BOSS_API_STUB);
        // The only real programs on PATH: the shell, what the npm stub
        // needs to leave a binary behind, the bound on the registry
        // read, and what placing a CLI config takes.
        for tool in ["bash", "mkdir", "chmod", "timeout", "cp", "jq"] {
            let real = ["/usr/bin", "/bin"]
                .into_iter()
                .map(|d| Path::new(d).join(tool))
                .find(|p| p.exists())
                .unwrap_or_else(|| panic!("{tool} in /usr/bin or /bin"));
            std::os::unix::fs::symlink(&real, stubs.join(tool))
                .unwrap_or_else(|e| panic!("link {tool}: {e}"));
        }
        create_dir(&root.join("boss"));
        create_dir(&root.join("home/.local/bin"));
        write_exec(&root.join("dev-claude.sh"), "#!/usr/bin/env bash\n");
        for cli in clis {
            write_exec(
                &root.join("home/.local/bin").join(cli),
                "#!/usr/bin/env bash\n",
            );
        }
        let path = stubs.display().to_string();
        let log = root.join("stub.log");
        Self { root, path, log }
    }

    fn cmd(&self, sessions: &str, env: &[(&str, &str)]) -> Command {
        let mut cmd = Command::new(repo_root().join(SCRIPT));
        cmd.env_clear()
            .env("PATH", &self.path)
            .env("BOSS_DEV_ROOT", &self.root)
            // Generous: typed input is in the pipe before the first read
            // and end of input answers at once, so no case waits on it
            // except the timeout's own, which sets it short — a short
            // one here would let a loaded gate time a typed answer out.
            .env("BOSS_DEV_SESSION_TIMEOUT", "20")
            .env("STUB_LOG", &self.log)
            .env("STUB_AGENTS", AGENTS_ALL)
            .env("STUB_SESSIONS", sessions);
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd
    }

    /// Run on a "terminal" with `input` typed, then end of input.
    fn run(&self, sessions: &str, input: &str, env: &[(&str, &str)]) -> (i32, String) {
        let mut cmd = self.cmd(sessions, env);
        cmd.env("BOSS_DEV_SESSION_ASSUME_TTY", "1");
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn dev-session.sh");
        boss_testing::feed_stdin(&mut child, input.as_bytes());
        collect(child.wait_with_output().expect("wait"))
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    /// The one session the script attached to, as tmux was asked.
    fn attached(&self) -> String {
        let log = self.log();
        let lines: Vec<&str> = log.lines().filter(|l| l.starts_with("ATTACHED ")).collect();
        assert_eq!(
            lines.len(),
            1,
            "exactly one tmux new-session must be the script's last act: {log}"
        );
        lines[0].trim_start_matches("ATTACHED ").to_string()
    }

    fn boss(&self) -> String {
        self.root.join("boss").display().to_string()
    }

    fn home(&self) -> String {
        self.root.join("home").display().to_string()
    }
}

fn collect(out: std::process::Output) -> (i32, String) {
    let merged = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.code().unwrap_or(-1), merged)
}

#[test]
fn the_script_is_in_the_tree_and_executable() {
    use std::os::unix::fs::PermissionsExt;
    let path = repo_root().join(SCRIPT);
    let meta = std::fs::metadata(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert!(
        meta.permissions().mode() & 0o111 != 0,
        "{SCRIPT} must be executable: the pod's /work/dev-session.sh execs it by path"
    );
}

/// Enter is option 1 — the Remote Control / drain session `dev` — so a
/// login that answers nothing lands exactly where it always did.
#[test]
fn enter_joins_the_dev_session() {
    let f = Fixture::new("enter", &["claude"]);
    let (rc, out) = f.run("dev", "\n", &[]);
    assert_eq!(rc, 0, "{out}");
    for line in [
        "1) Claude + Claude Code",
        "2) GPT + Codex",
        "3) Gemini + Code Assist",
        "4) Shell",
    ] {
        assert!(out.contains(line), "the menu must offer `{line}`: {out}");
    }
    assert_eq!(
        f.attached(),
        format!(
            "tmux new-session -A -s dev -c {} {}/dev-claude.sh HOME={}",
            f.boss(),
            f.root.display(),
            f.home()
        )
    );
}

/// No answer within the timeout is option 1 too: a login left at the
/// menu does not wait forever, and it lands in `dev`. Input stays OPEN
/// here (nothing typed, nothing closed), so this is the timeout, not
/// end of input.
#[test]
fn the_timeout_joins_the_dev_session() {
    let f = Fixture::new("timeout", &["claude"]);
    let mut cmd = f.cmd(
        "dev",
        &[
            ("BOSS_DEV_SESSION_ASSUME_TTY", "1"),
            ("BOSS_DEV_SESSION_TIMEOUT", "0.3"),
        ],
    );
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn dev-session.sh");
    let held_open = child.stdin.take();
    let (rc, out) = collect(child.wait_with_output().expect("wait"));
    drop(held_open);
    assert_eq!(rc, 0, "{out}");
    assert!(
        out.contains("no answer"),
        "a timeout must say what it chose: {out}"
    );
    assert!(
        f.attached().starts_with("tmux new-session -A -s dev "),
        "{}",
        f.log()
    );
}

/// Each line says whether its session is running, read from tmux.
#[test]
fn each_option_shows_whether_its_session_is_running() {
    let f = Fixture::new("status", &["claude", "codex", "gemini"]);
    let (_, out) = f.run("dev gemini", "\n", &[]);
    let line = |n: &str| {
        out.lines()
            .find(|l| l.trim_start().starts_with(n))
            .unwrap_or_else(|| panic!("no menu line {n}: {out}"))
            .to_string()
    };
    assert!(line("1)").ends_with(" running"), "{out}");
    assert!(line("2)").ends_with("not running"), "{out}");
    assert!(line("3)").ends_with(" running"), "{out}");
    assert!(!line("3)").ends_with("not running"), "{out}");
    assert!(line("4)").ends_with("not running"), "{out}");
    for s in ["dev", "codex", "gemini", "shell"] {
        assert!(
            f.log().contains(&format!("tmux has-session -t ={s}")),
            "the status of `{s}` must be read with an exact-name has-session: {}",
            f.log()
        );
    }
}

/// The `-e` flags a created agent session carries: its own git author
/// and committer, the actor file pointed at nothing (the pod's
/// /work/home/.config/boss/actor names Claude), and BOSS_ACTOR — its
/// alias when the registry holds it, BLANK when not. Blank, in the
/// SESSION env, because the tmux server holds the pod's
/// BOSS_ACTOR=claude@algedonic.dev globally and every later pane takes
/// the global value unless the session overrides it (review 23f1c6fd
/// B1); every reader treats blank as no answer.
fn identity(alias: &str, who: &str, registered: bool) -> String {
    let actor = if registered { alias } else { "" };
    format!(
        "-e BOSS_ACTOR_FILE=/dev/null -e GIT_AUTHOR_NAME={who} -e GIT_AUTHOR_EMAIL={alias} \
         -e GIT_COMMITTER_NAME={who} -e GIT_COMMITTER_EMAIL={alias} -e BOSS_ACTOR={actor}"
    )
}

/// Options 2-4 each join or create their own session, running their own
/// command, with the pod's HOME and the checkout as cwd. Joining reads
/// no registry and passes the UNSIGNED identity — tmux ignores it on an
/// attach, and a session that ended in between is then created signing
/// as nobody, never as the pod's Claude.
#[test]
fn each_option_joins_or_creates_its_session_with_its_command() {
    let id = |alias: &str, who: &str, registered: bool| identity(alias, who, registered);
    let (codex, gemini) = ("codex@algedonic.dev", "gemini@algedonic.dev");
    for (choice, session, created, joined) in [
        (
            "2",
            "codex",
            format!("{} codex", id(codex, "Codex (engineering)", true)),
            format!("{} codex", id(codex, "Codex (engineering)", false)),
        ),
        (
            "3",
            "gemini",
            format!("{} gemini", id(gemini, "Gemini (engineering)", true)),
            format!("{} gemini", id(gemini, "Gemini (engineering)", false)),
        ),
        ("4", "shell", "bash -l".to_string(), "bash -l".to_string()),
    ] {
        for running in ["", session] {
            let f = Fixture::new(
                &format!("opt{choice}-{}", running.len()),
                &["codex", "gemini"],
            );
            let (rc, out) = f.run(running, &format!("{choice}\n"), &[]);
            assert_eq!(rc, 0, "{out}");
            let command = if running.is_empty() {
                &created
            } else {
                &joined
            };
            assert_eq!(
                f.attached(),
                format!(
                    "tmux new-session -A -s {session} -c {} {command} HOME={}",
                    f.boss(),
                    f.home()
                ),
                "option {choice} with sessions `{running}`: {out}"
            );
        }
    }
}

/// A codex or gemini session signs as ITSELF, never as Claude (David
/// 2026-10-01: the pod sets BOSS_ACTOR=claude@algedonic.dev
/// container-wide, so without this every boss write from those sessions
/// would be credited to Claude in the audit log).
#[test]
fn an_agent_session_signs_as_its_own_registered_alias() {
    for (choice, session, alias) in [
        ("2", "codex", "codex@algedonic.dev"),
        ("3", "gemini", "gemini@algedonic.dev"),
    ] {
        let f = Fixture::new(&format!("signs-{session}"), &["codex", "gemini"]);
        let (rc, out) = f.run("", &format!("{choice}\n"), &[]);
        assert_eq!(rc, 0, "{out}");
        let attached = f.attached();
        assert!(
            attached.contains(&format!("-e BOSS_ACTOR={alias} ")),
            "{attached}"
        );
        assert!(!attached.contains("claude@"), "{attached}");
        assert!(
            f.log().contains("boss-api GET /api/agents"),
            "the alias is checked against the registry before the session starts: {}",
            f.log()
        );
    }
}

/// An alias the registry does not hold starts its session with
/// BOSS_ACTOR UNSET — every boss write refused naming the fix, every
/// read marked operator:unidentified — and says why. It never falls
/// back to Claude's identity.
#[test]
fn an_unregistered_alias_starts_unsigned_and_says_why() {
    let f = Fixture::new("unregistered", &["codex"]);
    let (rc, out) = f.run("", "2\n", &[("STUB_AGENTS", AGENTS_CLAUDE_ONLY)]);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(
        f.attached(),
        format!(
            "tmux new-session -A -s codex -c {} {} codex HOME={}",
            f.boss(),
            identity("codex@algedonic.dev", "Codex (engineering)", false),
            f.home()
        )
    );
    assert!(
        out.contains("codex@algedonic.dev is not a registered agent"),
        "{out}"
    );
    assert!(
        out.contains("POST /api/agents/batch"),
        "names the fix: {out}"
    );
}

/// A registry that cannot be read is not a registry that holds the
/// alias: the session starts unsigned, and the reason names the read —
/// a dark API, and a door that answers non-2xx (exit 1, its code on
/// stderr) even with a body that names the alias.
#[test]
fn an_unreadable_registry_starts_unsigned_and_says_so() {
    for (label, env) in [
        ("api-down", ("STUB_API_DOWN", "1")),
        ("api-403", ("STUB_API_STATUS", "403")),
        // A 200 with no document: jq -e over silence exits 0 on jq-1.6.
        ("api-empty", ("STUB_AGENTS", "")),
    ] {
        let f = Fixture::new(label, &["gemini"]);
        let (rc, out) = f.run("", "3\n", &[env]);
        assert_eq!(rc, 0, "{out}");
        assert!(
            f.attached().contains(" -e BOSS_ACTOR= gemini HOME="),
            "{label}: {}",
            f.attached()
        );
        assert!(
            out.contains("could not read the agents registry"),
            "{label}: {out}"
        );
    }
}

/// Registered means the alias is in an agent's `aliases`, matched
/// exactly — not the address appearing anywhere in the body (review
/// 23f1c6fd B2, minor).
#[test]
fn an_alias_named_outside_an_aliases_list_is_not_registered() {
    let body = r#"{"data":[{"aliases":["claude@algedonic.dev"],"display_name":"codex@algedonic.dev","id":"agent-claude"},{"aliases":["xcodex@algedonic.dev"],"id":"agent-x"}],"total":2}"#;
    let f = Fixture::new("alias-elsewhere", &["codex"]);
    let (rc, out) = f.run("", "2\n", &[("STUB_AGENTS", body)]);
    assert_eq!(rc, 0, "{out}");
    assert!(
        f.attached().contains(" -e BOSS_ACTOR= codex HOME="),
        "{}",
        f.attached()
    );
    assert!(
        out.contains("codex@algedonic.dev is not a registered agent"),
        "{out}"
    );
}

/// REAL tmux, on a private socket (review 23f1c6fd B1, reproduced): the
/// server is started the way the boot starts it, with the pod's
/// BOSS_ACTOR=claude@algedonic.dev in its environment; the selector
/// creates `codex`; then a SECOND window is opened in that session, as
/// a person would with prefix-c. That window must not sign as Claude —
/// blank or unset, never the pod's — for the unsigned case and the
/// registered one.
/// A private tmux server, killed when the test ends — on a panic too,
/// so a failed run leaves no server behind on a long-lived pod.
struct PrivateServer {
    tmux: PathBuf,
    sock: PathBuf,
    home: PathBuf,
}

impl PrivateServer {
    /// Run tmux against the private socket with the environment the
    /// boot's server has: the pod's BOSS_ACTOR=claude in it.
    ///
    /// SHELL IS NAMED, because tmux runs every pane through
    /// `default-shell`, taken from SHELL at server start and otherwise
    /// from the passwd entry — and the gate runs as uid 65534, whose
    /// shell is /usr/sbin/nologin. Unnamed there, every pane exited at
    /// once, the server with them, and the first gate of this test read
    /// "no server running" (gate dc46a16e; reproduced with setpriv
    /// --reuid=65534: has-session 1 without SHELL, 0 with SHELL=/bin/sh).
    fn tmux(&self, args: &[&str]) -> std::process::Output {
        Command::new(&self.tmux)
            .arg("-S")
            .arg(&self.sock)
            .args(args)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("SHELL", "/bin/sh")
            .env("HOME", &self.home)
            .env("BOSS_ACTOR", "claude@algedonic.dev")
            .output()
            .expect("run tmux")
    }

    /// Poll until `pred` holds, up to ~10 s; whether it did.
    fn until(pred: impl Fn() -> bool) -> bool {
        (0..100).any(|_| {
            let ok = pred();
            if !ok {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            ok
        })
    }
}

impl Drop for PrivateServer {
    fn drop(&mut self) {
        let _ = self.tmux(&["kill-server"]);
    }
}

/// REAL tmux, on a private socket (review 23f1c6fd B1, reproduced): the
/// server is started the way the boot starts it, with the pod's
/// BOSS_ACTOR=claude@algedonic.dev in its environment; the selector
/// creates `codex`; then a SECOND window is opened in that session, as
/// a person would with prefix-c. That window must not sign as Claude —
/// blank or unset, never the pod's — for the unsigned case and the
/// registered one.
#[test]
fn a_second_window_in_an_agent_session_never_signs_as_claude() {
    let tmux = ["/usr/bin/tmux", "/bin/tmux"]
        .into_iter()
        .map(Path::new)
        .find(|p| p.exists())
        .expect("tmux at /usr/bin/tmux — the gate image (boss-ci) and the dev pod both carry it");
    let sleep = ["/usr/bin/sleep", "/bin/sleep"]
        .into_iter()
        .map(Path::new)
        .find(|p| p.exists())
        .expect("sleep");
    for (label, agents, expect) in [
        ("unsigned", AGENTS_CLAUDE_ONLY, ""),
        ("registered", AGENTS_ALL, "codex@algedonic.dev"),
    ] {
        let f = Fixture::new(&format!("real-tmux-{label}"), &[]);
        let server = PrivateServer {
            tmux: tmux.to_path_buf(),
            sock: f.root.join("sock"),
            home: f.root.join("home"),
        };
        // The fake codex holds its pane — and so the session and the
        // server — open until the server is killed; the shim stands in
        // for an attach (there is no terminal here), creating detached.
        write_exec(
            &f.root.join("home/.local/bin/codex"),
            &format!("#!/usr/bin/env bash\nexec {} 300\n", sleep.display()),
        );
        write_exec(
            &f.root.join("stubs/tmux"),
            &format!(
                "#!/usr/bin/env bash\n\
                 args=()\n\
                 for a in \"$@\"; do [ \"$a\" = -A ] && a=-d; args+=(\"$a\"); done\n\
                 exec {} -S {} \"${{args[@]}}\"\n",
                tmux.display(),
                server.sock.display()
            ),
        );
        let boot = server.tmux(&["new-session", "-d", "-s", "dev", "sleep 300"]);
        assert!(boot.status.success(), "{label}: boot server: {boot:?}");
        // The server stays up between sessions, whatever the panes do.
        let _ = server.tmux(&["set-option", "-s", "exit-empty", "off"]);

        let (rc, out) = f.run("", "2\n", &[("STUB_AGENTS", agents)]);
        assert_eq!(rc, 0, "{label}: {out}");
        let has_codex = || {
            server
                .tmux(&["has-session", "-t", "=codex"])
                .status
                .success()
        };
        assert!(
            PrivateServer::until(has_codex),
            "{label}: the selector's codex session never appeared\nselector: {out}\nls: {:?}",
            server.tmux(&["list-sessions"])
        );

        let probe = f.root.join("second-window.env");
        let window = server.tmux(&[
            "new-window",
            "-t",
            "=codex:",
            &format!(
                "printf '%s|%s' \"${{BOSS_ACTOR-UNSET}}\" \"$BOSS_ACTOR_FILE\" > {}",
                probe.display()
            ),
        ]);
        assert!(
            window.status.success(),
            "{label}: new-window: {window:?}\nselector: {out}"
        );
        let read = || {
            std::fs::read_to_string(&probe)
                .ok()
                .filter(|s| !s.is_empty())
        };
        assert!(
            PrivateServer::until(|| read().is_some()),
            "{label}: the second window wrote nothing to {}",
            probe.display()
        );
        let seen = read().unwrap_or_default();
        let (actor, file) = seen.split_once('|').unwrap_or((&seen, ""));
        assert_eq!(
            file, "/dev/null",
            "{label}: second window's actor file: {seen}"
        );
        if expect.is_empty() {
            assert!(
                actor.is_empty() || actor == "UNSET",
                "{label}: a second window signed as `{actor}`, not nobody: {seen}"
            );
        } else {
            assert_eq!(actor, expect, "{label}: {seen}");
        }
        assert!(!seen.contains("claude@"), "{label}: {seen}");
    }
}

/// Joining a running agent session reads no registry: the session
/// already has the identity it was created with.
#[test]
fn joining_an_agent_session_reads_no_registry() {
    let f = Fixture::new("join-no-read", &["codex"]);
    let (rc, out) = f.run("codex", "2\n", &[]);
    assert_eq!(rc, 0, "{out}");
    assert!(!f.log().contains("boss-api"), "{}", f.log());
}

/// The Claude and Shell sessions keep the pod's identity untouched.
#[test]
fn dev_and_shell_carry_no_identity_flags() {
    for (input, session) in [("1\n", "dev"), ("4\n", "shell")] {
        let f = Fixture::new(&format!("no-ident-{session}"), &["claude"]);
        let (rc, out) = f.run("", input, &[]);
        assert_eq!(rc, 0, "{out}");
        assert!(!f.attached().contains(" -e "), "{}", f.attached());
        assert!(!f.log().contains("boss-api"), "{}", f.log());
    }
}

/// Each CLI's versioned config is placed when the PVC home has none,
/// never over one David has edited, and a difference is said aloud.
#[test]
fn a_cli_config_is_placed_only_when_absent_and_a_difference_is_said() {
    for (choice, session, src, dest) in [
        (
            "2",
            "codex",
            "infra/dev/cli/codex-config.toml",
            ".codex/config.toml",
        ),
        (
            "3",
            "gemini",
            "infra/dev/cli/gemini-settings.json",
            ".gemini/settings.json",
        ),
    ] {
        let f = Fixture::new(&format!("config-{session}"), &["codex", "gemini"]);
        let versioned = f.root.join("boss").join(src);
        create_dir(versioned.parent().expect("parent"));
        boss_testing::write_file(&versioned, "versioned\n");
        let placed = f.root.join("home").join(dest);

        // Absent: placed, and said.
        let (rc, out) = f.run("", &format!("{choice}\n"), &[]);
        assert_eq!(rc, 0, "{out}");
        assert_eq!(
            std::fs::read_to_string(&placed).unwrap_or_default(),
            "versioned\n",
            "{out}"
        );
        assert!(
            out.contains(&format!("placed {}", placed.display())),
            "{out}"
        );

        // Present and the same: nothing said.
        let (_, out) = f.run("", &format!("{choice}\n"), &[]);
        assert!(!out.contains(&placed.display().to_string()), "{out}");

        // Present with lines the CLI appended itself (codex 0.159.3
        // writes `[tui] screen_reader_detection_done = true` into its
        // config on first run — review 23f1c6fd N3): every versioned
        // line is still there, so nothing is said, or the note would
        // fire on every create and nobody would read it.
        boss_testing::write_file(
            &placed,
            "versioned\n\n[tui]\nscreen_reader_detection_done = true\n",
        );
        let (_, out) = f.run("", &format!("{choice}\n"), &[]);
        assert!(!out.contains(&placed.display().to_string()), "{out}");

        // Present and edited: kept, and the difference named.
        boss_testing::write_file(&placed, "David's own\n");
        let (_, out) = f.run("", &format!("{choice}\n"), &[]);
        assert_eq!(
            std::fs::read_to_string(&placed).unwrap_or_default(),
            "David's own\n",
            "an edited config is never overwritten"
        );
        assert!(
            out.contains(&format!("{} differs from", placed.display())),
            "{out}"
        );
    }
}

/// `dev` absent is created running dev-claude.sh — the same script the
/// boot starts it with, so drain-on-boot and Remote Control are its own.
#[test]
fn a_missing_dev_session_is_created_running_dev_claude() {
    let f = Fixture::new("dev-absent", &["claude"]);
    let (rc, out) = f.run("", "1\n", &[]);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(
        f.attached(),
        format!(
            "tmux new-session -A -s dev -c {} {}/dev-claude.sh HOME={}",
            f.boss(),
            f.root.display(),
            f.home()
        )
    );
}

/// A session that is running is joined even when its CLI is not on
/// PATH: joining needs tmux, not the binary.
#[test]
fn a_running_session_is_joined_without_its_cli() {
    let f = Fixture::new("join-no-cli", &[]);
    let (rc, out) = f.run("codex", "2\n", &[]);
    assert_eq!(rc, 0, "{out}");
    assert!(!out.contains("not installed"), "{out}");
    assert!(f.attached().starts_with("tmux new-session -A -s codex "));
}

/// A missing CLI is REPORTED with the command that installs it, and the
/// offer declined goes back to the menu — it neither crashes nor creates
/// a session whose command cannot start.
#[test]
fn a_missing_cli_is_reported_and_declining_returns_to_the_menu() {
    let f = Fixture::new("missing-decline", &["claude"]);
    let (rc, out) = f.run("dev", "2\nn\n4\n", &[]);
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("codex is not installed"), "{out}");
    assert!(
        out.contains(&format!(
            "npm i -g --prefix {}/.local @openai/codex",
            f.home()
        )),
        "the report must name the install command: {out}"
    );
    assert!(!f.log().contains("npm "), "declined must not install");
    assert!(!f.log().contains("-s codex"), "{}", f.log());
    assert!(
        f.attached().starts_with("tmux new-session -A -s shell "),
        "after declining, the menu is offered again: {}",
        f.log()
    );
}

/// Accepted, the offer runs the install onto the PVC prefix and then
/// creates the session.
#[test]
fn a_missing_cli_installed_on_request_then_starts_its_session() {
    let f = Fixture::new("missing-accept", &[]);
    let (rc, out) = f.run("", "3\ny\n", &[]);
    assert_eq!(rc, 0, "{out}");
    assert!(
        f.log().contains(&format!(
            "npm i -g --prefix {}/.local @google/gemini-cli",
            f.home()
        )),
        "{}",
        f.log()
    );
    assert_eq!(
        f.attached(),
        format!(
            "tmux new-session -A -s gemini -c {} {} gemini HOME={}",
            f.boss(),
            identity("gemini@algedonic.dev", "Gemini (engineering)", true),
            f.home()
        )
    );
}

/// An install that fails says so and returns to the menu; end of input
/// there is option 1.
#[test]
fn a_failed_install_is_reported_not_crashed() {
    let f = Fixture::new("install-fails", &["claude"]);
    let (rc, out) = f.run("", "2\ny\n", &[("STUB_NPM_FAILS", "1")]);
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("did not install codex"), "{out}");
    assert!(!f.log().contains("-s codex"), "{}", f.log());
    assert!(f.attached().starts_with("tmux new-session -A -s dev "));
}

/// Claude missing with `dev` absent: reported like any CLI, and declined
/// it still lands in `dev` as a plain shell — what a login did before
/// the selector — never stranded at the menu.
#[test]
fn claude_missing_still_lands_in_dev() {
    let f = Fixture::new("no-claude", &[]);
    let (rc, out) = f.run("", "\nn\n", &[]);
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("claude is not installed"), "{out}");
    assert!(out.contains("https://claude.ai/install.sh"), "{out}");
    assert_eq!(
        f.attached(),
        format!(
            "tmux new-session -A -s dev -c {} HOME={}",
            f.boss(),
            f.home()
        )
    );
}

/// No terminal, no menu: the script is today's dev-session.sh exactly.
#[test]
fn a_non_interactive_shell_gets_no_menu() {
    let f = Fixture::new("non-interactive", &["claude"]);
    let out = f
        .cmd("", &[])
        .stdin(Stdio::null())
        .output()
        .expect("run dev-session.sh");
    let (rc, out) = collect(out);
    assert_eq!(rc, 0, "{out}");
    assert!(!out.contains("1)"), "no menu without a terminal: {out}");
    assert_eq!(
        f.attached(),
        format!("tmux new-session -A -s dev HOME={}", f.home())
    );
}

/// Inside tmux — the `shell` session's own login bash, or any session's
/// — the menu is never shown again, by the script and by the .profile
/// that would exec it.
#[test]
fn a_shell_inside_tmux_gets_no_menu() {
    let f = Fixture::new("in-tmux", &["claude"]);
    // $TMUX as tmux sets it: socket path, server pid, session index.
    let tmux = format!("{},1,0", f.root.join("tmux-socket").display());
    let (_, out) = f.run("shell", "4\n", &[("TMUX", &tmux)]);
    assert!(!out.contains("1)"), "no menu inside tmux: {out}");
    assert!(!f.log().contains("-s shell"), "{}", f.log());

    let m = std::fs::read_to_string(repo_root().join("infra/cluster/manifests/boss-dev.yaml"))
        .expect("boss-dev.yaml");
    let profile = m
        .lines()
        .find(|l| l.contains("exec /work/dev-session.sh"))
        .expect("the .profile execs /work/dev-session.sh");
    for guard in ["*i*", "[ -t 0 ]", "[ -z \"${TMUX:-}\" ]"] {
        assert!(
            profile.contains(guard),
            "the .profile must keep `{guard}` before it execs the selector: {profile}"
        );
    }
}

/// The manifest's /work/dev-session.sh is a thin exec of the checkout's
/// copy, with today's attach as its fallback when the copy is missing or
/// does not parse — a bad menu edit must not lock ssh logins out.
#[test]
fn the_pods_dev_session_execs_the_checkouts_copy_with_a_fallback() {
    let m = std::fs::read_to_string(repo_root().join("infra/cluster/manifests/boss-dev.yaml"))
        .expect("boss-dev.yaml");
    let wrapper = m
        .split("cat > /work/dev-session.sh <<'LS'")
        .nth(1)
        .and_then(|rest| rest.split("\n                    LS\n").next())
        .expect("boss-dev.yaml writes /work/dev-session.sh");
    assert!(
        wrapper.contains("bash -n /work/boss/infra/dev/dev-session.sh"),
        "{wrapper}"
    );
    assert!(
        wrapper.contains("exec /work/boss/infra/dev/dev-session.sh"),
        "{wrapper}"
    );
    assert!(
        wrapper.contains("exec tmux new-session -A -s dev"),
        "the fallback is today's attach: {wrapper}"
    );
}

/// The boot's own "not started" line sends a login with no claude
/// credential to Shell (4): Enter now leads into dev-claude.sh, not a
/// shell to log in from (review a27c860d F3).
#[test]
fn the_not_started_message_points_at_the_shell_option() {
    let m = std::fs::read_to_string(repo_root().join("infra/cluster/manifests/boss-dev.yaml"))
        .expect("boss-dev.yaml");
    let line = m
        .lines()
        .find(|l| l.contains("dev session: not started"))
        .expect("the boot says when the dev session did not start");
    assert!(line.contains("choose Shell (4)"), "{line}");
}

/// The CLI installs never hold the boot (review a27c860d F1, measured on
/// the live pod): the kubelet starts the postgres and reclaim sidecars,
/// and marks the pod Ready (which opens the ssh LoadBalancer), only once
/// the dev container's postStart returns, so an install INSIDE the hook
/// holds all three.
#[test]
fn the_boot_installs_codex_and_gemini_detached_from_the_hook() {
    let m = std::fs::read_to_string(repo_root().join("infra/cluster/manifests/boss-dev.yaml"))
        .expect("boss-dev.yaml");
    let opener = "setsid bash -c '";
    let closer = "' < /dev/null >> /work/ssh/postStart.log 2>&1 &";
    let detached = m
        .split(opener)
        .nth(1)
        .and_then(|rest| rest.split(closer).next().filter(|_| rest.contains(closer)))
        .unwrap_or_else(|| {
            panic!(
                "the postStart must run the CLI installs as `{opener} … {closer}` - \
                 its own session, stdin closed, output to the log, in the background"
            )
        });
    let npm_lines: Vec<&str> = m.lines().filter(|l| l.contains("npm i -g")).collect();
    assert_eq!(npm_lines.len(), 2, "{npm_lines:?}");
    for line in npm_lines {
        assert!(
            detached.contains(line.trim()),
            "every npm install runs inside the detached block, never in the hook itself: {line}"
        );
    }
}

/// The two new CLIs are installed onto the PVC prefix at boot, best
/// effort, and no boot step places a credential for either: each login
/// is David's, once, inside its session.
#[test]
fn the_boot_installs_codex_and_gemini_best_effort_and_places_no_credential() {
    let m = std::fs::read_to_string(repo_root().join("infra/cluster/manifests/boss-dev.yaml"))
        .expect("boss-dev.yaml");
    for pkg in ["@openai/codex", "@google/gemini-cli"] {
        let line = m
            .lines()
            .find(|l| l.contains("npm i -g --prefix /work/home/.local") && l.contains(pkg))
            .unwrap_or_else(|| panic!("the boot installs {pkg} onto /work/home/.local"));
        assert!(
            line.contains("|| echo"),
            "a failed {pkg} install must be reported, never fail the boot: {line}"
        );
        assert!(
            line.contains("timeout -k 10 300 npm"),
            "the {pkg} install is bounded, and killed if it ignores TERM: {line}"
        );
    }
    for secret in [
        "OPENAI_API_KEY",
        "GEMINI_API_KEY",
        "GOOGLE_API_KEY",
        ".codex/auth.json",
    ] {
        assert!(
            !m.contains(secret),
            "boss-dev.yaml must place no credential for the CLIs (found {secret})"
        );
    }
}

/// ONE INSTRUCTIONS FILE (David 2026-10-01): Codex reads AGENTS.md, Claude
/// reads CLAUDE.md, and Gemini is configured to read AGENTS.md. Two
/// copies of the operating rules would drift (CLAUDE.md §9a), so
/// AGENTS.md is a symlink to CLAUDE.md and nothing else.
#[test]
fn agents_md_is_a_symlink_to_claude_md() {
    let agents = repo_root().join("AGENTS.md");
    let target = std::fs::read_link(&agents)
        .unwrap_or_else(|e| panic!("AGENTS.md must be a symlink to CLAUDE.md: {e}"));
    assert_eq!(
        target,
        Path::new("CLAUDE.md"),
        "a relative link, so every checkout resolves it"
    );
    assert_eq!(
        std::fs::read_to_string(&agents).expect("AGENTS.md resolves"),
        std::fs::read_to_string(repo_root().join("CLAUDE.md")).expect("CLAUDE.md"),
    );
}

/// The versioned CLI configs parse as what each CLI reads: TOML for
/// codex, JSON for gemini. A file that does not parse is placed on a
/// fresh PVC and breaks the CLI it was meant to configure.
#[test]
fn the_versioned_cli_configs_parse() {
    let codex = std::fs::read_to_string(repo_root().join("infra/dev/cli/codex-config.toml"))
        .expect("infra/dev/cli/codex-config.toml");
    let codex: toml::Value = toml::from_str(&codex).expect("codex-config.toml parses as TOML");
    assert_eq!(
        codex.get("sandbox_mode").and_then(|v| v.as_str()),
        Some("workspace-write")
    );
    assert_eq!(
        codex.get("approval_policy").and_then(|v| v.as_str()),
        Some("on-request")
    );
    let gemini = std::fs::read_to_string(repo_root().join("infra/dev/cli/gemini-settings.json"))
        .expect("infra/dev/cli/gemini-settings.json");
    let gemini: serde_json::Value =
        serde_json::from_str(&gemini).expect("gemini-settings.json parses as JSON");
    // Key paths as gemini-cli 0.62.0 reads them (its bundled
    // docs/reference/configuration.md): nested, not the older flat
    // `contextFileName`, which that version reads only in extensions.
    assert_eq!(
        gemini.pointer("/context/fileName").and_then(|v| v.as_str()),
        Some("AGENTS.md"),
        "gemini reads the one instructions file: {gemini}"
    );
    assert_eq!(
        gemini
            .pointer("/general/defaultApprovalMode")
            .and_then(|v| v.as_str()),
        Some("default"),
        "gemini asks before acting: {gemini}"
    );
    assert_eq!(
        gemini
            .pointer("/security/disableYoloMode")
            .and_then(|v| v.as_bool()),
        Some(true),
        "--yolo refused even when passed: {gemini}"
    );
    assert_eq!(
        codex
            .get("project_doc_max_bytes")
            .and_then(|v| v.as_integer()),
        Some(131_072),
        "codex reads AGENTS.md -> CLAUDE.md whole"
    );
    let claude_md = std::fs::metadata(repo_root().join("CLAUDE.md"))
        .expect("CLAUDE.md")
        .len();
    assert!(
        claude_md <= 131_072,
        "CLAUDE.md is {claude_md} bytes, past codex's project_doc_max_bytes: raise it"
    );
}
