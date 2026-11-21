//! `infra/dev/dev-session.sh` — the dev pod's login selector (backlog
//! e2d63c28, David 2026-10-01).
//!
//! An interactive ssh login used to exec straight into the one durable
//! tmux session, `dev` (the Remote Control / drain session). It now
//! offers six: 1 Claude + Claude Code (`dev`), 2 GPT + Codex (`codex`),
//! 3 Gemini + Code Assist (`gemini`), 4 Shell (`shell`), 5 Claude B +
//! Claude Code (`claude-b`, a second account's own login), 6 GPT + Codex
//! as the supervised factory loop (`codex-factory`, 628dc540) — and 7, an
//! ssh to the standby shell's door (design 0491591b). Each joins its
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
/// STUB_API_STATUS (default 200), or fails when the API is dark —
/// always (STUB_API_DOWN=1), or for its first STUB_API_DOWN_CALLS calls
/// and then answers, as an API still starting at a pod's boot does
/// (backlog 4be9ef95).
const BOSS_API_STUB: &str = r#"#!/usr/bin/env bash
echo "boss-api $*" >> "$STUB_LOG"
[ "${STUB_API_DOWN:-}" = 1 ] && { echo "curl: (7) Failed to connect" >&2; exit 7; }
if [ -n "${STUB_API_DOWN_CALLS:-}" ]; then
  n=0
  [ -r "$STUB_LOG.calls" ] && n=$(<"$STUB_LOG.calls")
  echo $((n + 1)) > "$STUB_LOG.calls"
  [ "$n" -lt "$STUB_API_DOWN_CALLS" ] && { echo "curl: (7) Failed to connect" >&2; exit 7; }
fi
[ "$1 $2" = "GET /api/agents" ] || exit 9
code=${STUB_API_STATUS:-200}
printf '%s\n' "$STUB_AGENTS"
printf 'HTTP:%s\n' "$code" >&2
case $code in 2??) exit 0 ;; *) exit 1 ;; esac
"#;

/// The registry as it stands with both new agents posted.
const AGENTS_ALL: &str = r#"{"data":[{"aliases":["claude@algedonic.dev"],"id":"agent-claude"},{"aliases":["codex@algedonic.dev"],"id":"agent-codex"},{"aliases":["gemini@algedonic.dev"],"id":"agent-gemini"},{"aliases":["claude-b@algedonic.dev"],"id":"agent-claude-b"}],"total":4}"#;

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
        // read, the pause between a boot's passes, and what placing a
        // CLI config takes.
        for tool in ["bash", "mkdir", "chmod", "timeout", "sleep", "cp", "jq"] {
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
        "5) Claude B + Claude Code",
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

/// An alias the registry does not hold starts its INTERACTIVE session
/// with BOSS_ACTOR UNSET — every boss write refused naming the fix,
/// every read marked operator:unidentified — and says why. It never
/// falls back to Claude's identity. A person is at the terminal to read
/// why; a `--boot` seat has nobody, so it is not started at all
/// (a_boot_seat_never_starts_with_an_empty_boss_actor, backlog 4be9ef95).
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
/// alias: the INTERACTIVE session starts unsigned, and the reason names
/// the read — a dark API, and a door that answers non-2xx (exit 1, its
/// code on stderr) even with a body that names the alias. The `--boot`
/// path retries the read and never starts the seat unsigned
/// (a_boot_retries_an_unreadable_registry_and_starts_the_seat_signed).
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

/// Option 5 is a SECOND Claude account (David 2026-10-17: practise
/// scaling to two accounts). Its own login home, `$HOME/.claude-b`, is
/// a `-e CLAUDE_CONFIG_DIR` on the session, so the login the person makes
/// with `/login` there never touches option 1's. It signs as its own
/// registered alias, claude-b@algedonic.dev, like codex and gemini.
#[test]
fn option_five_is_a_second_claude_account_with_its_own_login_home() {
    let alias = "claude-b@algedonic.dev";
    let who = "Claude (engineering, account B)";
    for running in ["", "claude-b"] {
        let f = Fixture::new(&format!("claude-b-{}", running.len()), &["claude"]);
        let (rc, out) = f.run(running, "5\n", &[]);
        assert_eq!(rc, 0, "{out}");
        let signed = running.is_empty();
        assert_eq!(
            f.attached(),
            format!(
                "tmux new-session -A -s claude-b -c {} {} -e CLAUDE_CONFIG_DIR={}/.claude-b claude HOME={}",
                f.boss(),
                identity(alias, who, signed),
                f.home(),
                f.home()
            ),
            "option 5 with sessions `{running}`: {out}"
        );
    }
}

/// Creating option 5 PREPARES its login home when missing: the account's
/// settings copied from option 1's, and memory (`projects`), skills and
/// plugins linked so both accounts work from one record. It never copies
/// or links a credential: the login is the person's own act, on first
/// launch, with `/login`.
#[test]
fn option_five_prepares_its_home_and_never_copies_a_credential() {
    let f = Fixture::new("claude-b-home", &["claude"]);
    let a = std::path::Path::new(&f.home()).join(".claude");
    std::fs::create_dir_all(a.join("projects")).unwrap();
    std::fs::create_dir_all(a.join("skills")).unwrap();
    std::fs::create_dir_all(a.join("plugins")).unwrap();
    std::fs::write(a.join("settings.json"), "{}").unwrap();
    std::fs::write(a.join(".credentials.json"), "secret").unwrap();
    let (rc, out) = f.run("", "5\n", &[]);
    assert_eq!(rc, 0, "{out}");
    let b = std::path::Path::new(&f.home()).join(".claude-b");
    for linked in ["projects", "skills", "plugins"] {
        assert_eq!(
            std::fs::read_link(b.join(linked)).unwrap_or_else(|e| panic!("{linked}: {e}: {out}")),
            a.join(linked),
            "{linked} must be linked to option 1's"
        );
    }
    assert_eq!(
        std::fs::read_to_string(b.join("settings.json")).unwrap(),
        "{}"
    );
    assert!(
        !b.join(".credentials.json").exists(),
        "a credential must never be copied into the second account's home: {out}"
    );
}

/// Option 6 is the factory loop (backlog 628dc540): the SAME codex
/// identity as option 2, running infra/dev/factory-loop.sh rather than
/// the TUI, in its own session. Joined when it runs, created when not.
#[test]
fn option_six_runs_the_factory_loop_as_codex() {
    let codex = "codex@algedonic.dev";
    for running in ["", "codex-factory"] {
        let f = Fixture::new(&format!("opt6-{}", running.len()), &["codex"]);
        let (rc, out) = f.run(running, "6\n", &[]);
        assert_eq!(rc, 0, "{out}");
        assert_eq!(
            f.attached(),
            format!(
                "tmux new-session -A -s codex-factory -c {} {} {}/infra/dev/factory-loop.sh HOME={}",
                f.boss(),
                identity(codex, "Codex (engineering)", running.is_empty()),
                f.boss(),
                f.home()
            ),
            "sessions `{running}`: {out}"
        );
    }
}

/// The TUI and the loop sign as one agent, so neither is CREATED while
/// the other runs — two CLIs on one identity would race for one queue.
/// The refusal names the other session and goes back to the menu.
#[test]
fn the_codex_tui_and_the_factory_loop_never_start_beside_each_other() {
    for (choice, wanted, other) in [
        ("6", "codex-factory", "codex"),
        ("2", "codex", "codex-factory"),
    ] {
        let f = Fixture::new(&format!("exclusive-{wanted}"), &["codex"]);
        let (rc, out) = f.run(other, &format!("{choice}\n4\n"), &[]);
        assert_eq!(rc, 0, "{out}");
        assert!(
            out.contains(&format!(
                "tmux `{other}` is running and signs as the same agent"
            )),
            "{out}"
        );
        assert!(
            f.attached().starts_with("tmux new-session -A -s shell "),
            "the refusal returns to the menu: {}",
            f.log()
        );
        assert!(!f.log().contains(&format!("-s {wanted} ")), "{}", f.log());
    }
}

/// `--detached 6` (backlog ccd985ef): `boss factory restart`, through
/// infra/dev/agent-pane.sh, starts the factory loop with option 6's own
/// command — the same session, identity and loop — in a DETACHED session,
/// from inside the conductor's tmux and with no terminal, and prints the
/// session and its pane pid. No menu is shown.
#[test]
fn detached_six_starts_option_six_without_a_terminal() {
    let codex = "codex@algedonic.dev";
    let f = Fixture::new("detached-6", &["codex"]);
    let tmux = format!("{},1,0", f.root.join("tmux-socket").display());
    let out = f
        .cmd("dev", &[("TMUX", &tmux)])
        .args(["--detached", "6"])
        .stdin(Stdio::null())
        .output()
        .expect("run dev-session.sh");
    let (rc, out) = collect(out);
    assert_eq!(rc, 0, "{out}");
    assert!(!out.contains("1)"), "no menu: {out}");
    assert_eq!(
        f.attached(),
        format!(
            "tmux new-session -d -P -F #{{session_name}} #{{pane_pid}} -s codex-factory -c {} {} \
             {}/infra/dev/factory-loop.sh HOME={}",
            f.boss(),
            identity(codex, "Codex (engineering)", true),
            f.boss(),
            f.home()
        )
    );
}

/// `--detached 2` and `--detached 5` (backlog 2a2d11b2, David
/// 2026-11-10): after a pod roll the interactive Codex and Claude B wait
/// for an interactive login, so `boss session start` starts them with the
/// menu's own option — the same session, identity check, config and
/// command — DETACHED, and prints the session and its pane pid. Option 5
/// prepares its login home as the menu does; the registry is read, so
/// each signs as its own registered alias.
#[test]
fn detached_two_and_five_start_their_options_without_a_terminal() {
    for (option, cli) in [("2", "codex"), ("5", "claude")] {
        let f = Fixture::new(&format!("detached-{option}"), &[cli]);
        let command = if option == "2" {
            format!(
                "-s codex -c {} {} codex",
                f.boss(),
                identity("codex@algedonic.dev", "Codex (engineering)", true)
            )
        } else {
            format!(
                "-s claude-b -c {} {} -e CLAUDE_CONFIG_DIR={}/.claude-b claude",
                f.boss(),
                identity(
                    "claude-b@algedonic.dev",
                    "Claude (engineering, account B)",
                    true
                ),
                f.home()
            )
        };
        let expected = format!(
            "tmux new-session -d -P -F #{{session_name}} #{{pane_pid}} {command} HOME={}",
            f.home()
        );
        let out = f
            .cmd("dev", &[])
            .args(["--detached", option])
            .stdin(Stdio::null())
            .output()
            .expect("run dev-session.sh");
        let (rc, out) = collect(out);
        assert_eq!(rc, 0, "option {option}: {out}");
        assert!(!out.contains("1)"), "no menu: {out}");
        assert_eq!(f.attached(), expected, "option {option}: {out}");
        assert!(
            f.log().contains("boss-api GET /api/agents"),
            "option {option}: the alias is checked against the registry: {}",
            f.log()
        );
        if option == "5" {
            assert!(
                Path::new(&f.home()).join(".claude-b").is_dir(),
                "option 5 prepares its login home detached as on the menu: {out}"
            );
        }
    }
}

/// `--detached` takes options 2, 5 and 6 alone — never 1 (the drain's
/// `dev`, which the boot owns), 3 or 4 — and never joins: a running
/// session, a running session on the same codex identity, or a missing
/// CLI is refused with nothing started and no install offered.
#[test]
fn detached_refuses_anything_but_a_fresh_session() {
    for (name, args, sessions, clis, said) in [
        (
            "other",
            vec!["--detached", "3"],
            "",
            vec!["codex", "gemini"],
            "takes only 2, 5 or 6",
        ),
        (
            "dev",
            vec!["--detached", "1"],
            "",
            vec!["claude"],
            "takes only 2, 5 or 6",
        ),
        (
            "shell",
            vec!["--detached", "4"],
            "",
            vec![],
            "takes only 2, 5 or 6",
        ),
        (
            "none",
            vec!["--detached"],
            "",
            vec!["codex"],
            "takes only 2, 5 or 6",
        ),
        (
            "tui-running",
            vec!["--detached", "2"],
            "codex",
            vec!["codex"],
            "already running",
        ),
        (
            "tui-beside-factory",
            vec!["--detached", "2"],
            "codex-factory",
            vec!["codex"],
            "signs as the same agent",
        ),
        (
            "tui-nocli",
            vec!["--detached", "2"],
            "",
            vec![],
            "not installed",
        ),
        (
            "claude-b-running",
            vec!["--detached", "5"],
            "claude-b",
            vec!["claude"],
            "already running",
        ),
        (
            "claude-b-nocli",
            vec!["--detached", "5"],
            "",
            vec![],
            "not installed",
        ),
        (
            "running",
            vec!["--detached", "6"],
            "codex-factory",
            vec!["codex"],
            "already running",
        ),
        (
            "tui",
            vec!["--detached", "6"],
            "codex",
            vec!["codex"],
            "signs as the same agent",
        ),
        (
            "nocli",
            vec!["--detached", "6"],
            "",
            vec![],
            "not installed",
        ),
    ] {
        let f = Fixture::new(&format!("detached-refused-{name}"), &clis);
        let out = f
            .cmd(sessions, &[])
            .args(&args)
            .stdin(Stdio::null())
            .output()
            .expect("run dev-session.sh");
        let (rc, out) = collect(out);
        assert_ne!(rc, 0, "{name}: {out}");
        assert!(out.contains(said), "{name}: {out}");
        assert!(!out.contains("1)"), "{name}: no menu: {out}");
        assert!(!f.log().contains("new-session"), "{name}: {}", f.log());
        assert!(!f.log().contains("npm "), "{name}: {}", f.log());
        assert!(!f.log().contains("curl "), "{name}: {}", f.log());
    }
}

/// The seats of infra/dev/seats.tsv, as `(seat, cli, twin)`, in order.
fn seats() -> Vec<(String, String, String)> {
    std::fs::read_to_string(repo_root().join("infra/dev/seats.tsv"))
        .expect("infra/dev/seats.tsv")
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            // The seventh is the loop's `pull` (35f828cf), not read here.
            assert_eq!(f.len(), 7, "a seat row has seven columns: {l}");
            (f[0].to_string(), f[1].to_string(), f[5].to_string())
        })
        .collect()
}

/// A CLI stub that answers its login check (`codex login status`,
/// `claude auth status`) with `rc`, recording the login home it was
/// asked in.
fn login_stub(f: &Fixture, cli: &str, rc: i32) {
    write_exec(
        &Path::new(&f.home()).join(".local/bin").join(cli),
        &format!(
            "#!/usr/bin/env bash\necho \"{cli} $* config=${{CLAUDE_CONFIG_DIR:-}}\" >> \"$STUB_LOG\"\nexit {rc}\n"
        ),
    );
}

/// `--boot` (backlog b513514c, David's morning plan 2026-11-17 item 3):
/// a pod roll brings back EVERY seat of infra/dev/seats.tsv, not only
/// Codex's factory — on 2026-11-17 the conductor and Claude B stood idle
/// 7.5 h with nothing supervising them (5627a4e1). Each seat is started
/// detached through join(), as its interactive twin's identity, running
/// factory-loop.sh with its own name (the first seat, option 6, with the
/// bare path agent-pane.sh proves); each start is one `seat` line.
#[test]
fn boot_starts_every_seat_of_the_list_supervised() {
    let f = Fixture::new("boot", &["codex", "claude"]);
    login_stub(&f, "codex", 0);
    login_stub(&f, "claude", 0);
    let out = f
        .cmd("dev", &[])
        .arg("--boot")
        .stdin(Stdio::null())
        .output()
        .expect("run dev-session.sh");
    let (rc, out) = collect(out);
    assert_eq!(rc, 0, "{out}");
    assert!(!out.contains("1)"), "no menu: {out}");
    let log = f.log();
    let seats = seats();
    assert!(
        seats
            .iter()
            .any(|(s, c, _)| s == "conductor" && c == "claude")
            && seats
                .iter()
                .any(|(s, c, t)| s == "claude-b-factory" && c == "claude" && t == "claude-b")
            && seats[0] == ("codex-factory".into(), "codex".into(), "codex".into()),
        "the list seats the conductor and Claude B beside Codex's factory: {seats:?}"
    );
    for (i, (seat, _, _)) in seats.iter().enumerate() {
        assert!(
            out.contains(&format!("seat {seat}: started")),
            "{seat} is started: {out}"
        );
        let loop_cmd = if i == 0 {
            format!("{}/infra/dev/factory-loop.sh HOME=", f.boss())
        } else {
            format!("{}/infra/dev/factory-loop.sh {seat} HOME=", f.boss())
        };
        let line = log
            .lines()
            .find(|l| {
                l.starts_with("ATTACHED tmux new-session -d -P -F")
                    && l.contains(&format!(" -s {seat} "))
            })
            .unwrap_or_else(|| panic!("{seat} has a detached new-session: {log}"));
        assert!(line.contains(&loop_cmd), "{seat} runs its loop: {line}");
    }
    let session = |seat: &str| {
        log.lines()
            .find(|l| l.starts_with("ATTACHED ") && l.contains(&format!(" -s {seat} ")))
            .unwrap_or_default()
            .to_string()
    };
    assert!(
        session("claude-b-factory").contains(&identity(
            "claude-b@algedonic.dev",
            "Claude (engineering, account B)",
            true
        )) && session("claude-b-factory")
            .contains(&format!("-e CLAUDE_CONFIG_DIR={}/.claude-b ", f.home())),
        "Claude B's seat signs as Claude B, in its own login home: {log}"
    );
    assert!(
        session("codex-factory").contains(&identity(
            "codex@algedonic.dev",
            "Codex (engineering)",
            true
        )),
        "{log}"
    );
    assert!(
        !session("conductor").contains("BOSS_ACTOR"),
        "the conductor signs as the pod's Claude, the server's own identity: {log}"
    );
    assert!(
        log.contains(&format!("claude auth status config={}/.claude-b", f.home()))
            && log.contains("claude auth status config=\n")
            && log.contains("codex login status"),
        "each seat's login is its CLI's own answer, in the seat's login home: {log}"
    );
}

/// A seat that cannot run is named and the rest still start: a CLI with
/// no login, a running seat, and a seat whose interactive twin holds the
/// identity are each one line, and `--boot` exits 0 for the boot.
#[test]
fn boot_names_each_seat_it_does_not_start_and_starts_the_rest() {
    let f = Fixture::new("boot-refused", &["codex", "claude"]);
    login_stub(&f, "codex", 1);
    login_stub(&f, "claude", 0);
    let out = f
        .cmd("dev claude-b conductor", &[])
        .arg("--boot")
        .stdin(Stdio::null())
        .output()
        .expect("run dev-session.sh");
    let (rc, out) = collect(out);
    assert_eq!(rc, 0, "{out}");
    for said in [
        "seat codex-factory: not started - codex is not logged in",
        "seat claude-b-factory: not started (exit 1): tmux `claude-b` is running and signs as the same agent",
        "seat conductor: already running",
    ] {
        assert!(out.contains(said), "{said}: {out}");
    }
    assert!(!f.log().contains("new-session"), "{}", f.log());

    let none = Fixture::new("boot-nocli", &[]);
    let out = none
        .cmd("", &[])
        .arg("--boot")
        .stdin(Stdio::null())
        .output()
        .expect("run dev-session.sh");
    let (rc, out) = collect(out);
    assert_eq!(rc, 0, "{out}");
    for (seat, cli, _) in seats() {
        assert!(
            out.contains(&format!(
                "seat {seat}: not started - {cli} is not installed"
            )),
            "{seat}: {out}"
        );
    }
    assert!(!none.log().contains("new-session"), "{}", none.log());
    assert!(
        !none.log().contains("npm "),
        "never installs: {}",
        none.log()
    );
}

/// Review ce8970ce N2, N3, N4. A seat with its off switch on the PVC
/// stays off across a roll (N3); `--boot --except <seat>` never starts
/// that seat, which is how the boot keeps the conductor's seat from
/// draining beside tmux `dev` when the boot left the drain there (N2).
/// N4 named a seat started with its identity unconfirmed as `started
/// UNSIGNED`; since backlog 4be9ef95 such a seat is not started at all,
/// and its line says so.
#[test]
fn boot_skips_an_off_or_excepted_seat_and_never_starts_one_unidentified() {
    let f = Fixture::new("boot-off", &["codex", "claude"]);
    login_stub(&f, "codex", 0);
    login_stub(&f, "claude", 0);
    let off = Path::new(&f.home()).join(".config/boss/seat-off");
    boss_testing::create_dir(&off);
    std::fs::write(off.join("codex-factory"), "").expect("the off switch");
    let out = f
        .cmd(
            "dev",
            &[("STUB_AGENTS", ""), ("BOSS_DEV_BOOT_IDENTITY_WAIT", "0")],
        )
        .args(["--boot", "--except", "conductor"])
        .stdin(Stdio::null())
        .output()
        .expect("run dev-session.sh");
    let (rc, out) = collect(out);
    assert_eq!(rc, 0, "{out}");
    assert!(
        out.contains(&format!(
            "seat codex-factory: off - {}/codex-factory exists",
            off.display()
        )),
        "{out}"
    );
    assert!(
        out.contains("seat conductor: not started - the boot left the drain in tmux dev"),
        "{out}"
    );
    assert!(
        out.contains("seat claude-b-factory: not started - identity unconfirmed"),
        "{out}"
    );
    assert!(!out.contains("started UNSIGNED"), "{out}");
    let log = f.log();
    for seat in ["codex-factory", "conductor", "claude-b-factory"] {
        assert!(
            !log.lines()
                .any(|l| l.contains("new-session") && l.contains(&format!(" -s {seat} "))),
            "{seat} is not started: {log}"
        );
    }
}

/// Run `--boot` on a fixture whose CLIs are installed and logged in, with
/// `dev` running, and `env` on top.
fn boot(f: &Fixture, env: &[(&str, &str)]) -> (i32, String) {
    login_stub(f, "codex", 0);
    login_stub(f, "claude", 0);
    let out = f
        .cmd("dev", env)
        .arg("--boot")
        .stdin(Stdio::null())
        .output()
        .expect("run dev-session.sh");
    collect(out)
}

/// The seats that sign as an agent of their own (codex, Claude B), whose
/// identity the registry must confirm before they start.
const SIGNED_SEATS: [&str; 2] = ["codex-factory", "claude-b-factory"];

/// No created session carries a BLANK BOSS_ACTOR: the stub tmux records
/// every new-session, so a blank one is a line holding `-e BOSS_ACTOR= `.
fn no_seat_started_unsigned(log: &str, label: &str) {
    let unsigned: Vec<&str> = log
        .lines()
        .filter(|l| l.starts_with("ATTACHED ") && l.contains(" -e BOSS_ACTOR= "))
        .collect();
    assert!(
        unsigned.is_empty(),
        "{label}: a --boot seat must never start with BOSS_ACTOR blank: {unsigned:?}"
    );
}

/// THE CASE (backlog 4be9ef95, measured 2026-11-18): at the 13:19Z boot
/// the registry could not be read, the codex-factory seat started anyway
/// with BOSS_ACTOR blank, and its loop ran 3h40m unable to heartbeat or
/// open a work-session — the conductor's orders went unread. A `--boot`
/// seat whose identity the registry does not confirm — a dark API, a
/// non-2xx, an empty answer, or an alias the registry does not hold — is
/// NOT started: its line says `not started - identity unconfirmed` and
/// why, and no tmux session is created for it. A seat that signs as no
/// agent of its own (the conductor) still starts. The interactive menu
/// keeps starting unsigned and saying so: a person is there to read it.
#[test]
fn a_boot_seat_never_starts_with_an_empty_boss_actor() {
    for (label, env, said) in [
        (
            "api-down",
            ("STUB_API_DOWN", "1"),
            "could not read the agents registry",
        ),
        (
            "api-403",
            ("STUB_API_STATUS", "403"),
            "could not read the agents registry",
        ),
        (
            "api-empty",
            ("STUB_AGENTS", ""),
            "could not read the agents registry",
        ),
        (
            "unregistered",
            ("STUB_AGENTS", r#"{"data":[],"total":0}"#),
            "is not a registered agent",
        ),
    ] {
        let f = Fixture::new(&format!("boot-unsigned-{label}"), &["codex", "claude"]);
        let (rc, out) = boot(&f, &[env, ("BOSS_DEV_BOOT_IDENTITY_WAIT", "0")]);
        assert_eq!(rc, 0, "{label}: the boot reads the lines: {out}");
        let log = f.log();
        no_seat_started_unsigned(&log, label);
        for seat in SIGNED_SEATS {
            let line = out
                .lines()
                .find(|l| l.starts_with(&format!("seat {seat}: ")))
                .unwrap_or_else(|| panic!("{label}: {seat} has a seat line: {out}"));
            assert!(
                line.starts_with(&format!("seat {seat}: not started - identity unconfirmed")),
                "{label}: {line}"
            );
            assert!(line.contains(said), "{label}: names why: {line}");
            assert!(
                line.contains("dev-session.sh --boot"),
                "{label}: names what starts it later: {line}"
            );
            assert!(
                !log.lines()
                    .any(|l| l.contains("new-session") && l.contains(&format!(" -s {seat} "))),
                "{label}: {seat} is not started: {log}"
            );
        }
        assert!(
            out.contains("seat conductor: started"),
            "{label}: a seat with no identity of its own is not held back: {out}"
        );
        assert!(
            !out.contains("The session starts with BOSS_ACTOR blank"),
            "{label}: {out}"
        );
    }
}

/// An API still starting at a pod's boot is the usual reason the registry
/// cannot be read, so the boot RETRIES a seat it could not confirm, on a
/// later pass within a bound, and starts it SIGNED once the registry
/// answers. Here the first two reads (one per signed seat, pass one) fail
/// and the third answers.
#[test]
fn a_boot_retries_an_unreadable_registry_and_starts_the_seat_signed() {
    let f = Fixture::new("boot-retry", &["codex", "claude"]);
    let (rc, out) = boot(
        &f,
        &[
            ("STUB_API_DOWN_CALLS", "2"),
            ("BOSS_DEV_BOOT_IDENTITY_WAIT", "30"),
            ("BOSS_DEV_BOOT_IDENTITY_PAUSE", "0"),
        ],
    );
    assert_eq!(rc, 0, "{out}");
    let log = f.log();
    no_seat_started_unsigned(&log, "retry");
    let session = |seat: &str| {
        log.lines()
            .find(|l| l.starts_with("ATTACHED ") && l.contains(&format!(" -s {seat} ")))
            .unwrap_or_else(|| panic!("{seat} is started on a later pass: {out} {log}"))
            .to_string()
    };
    assert!(
        session("codex-factory").contains(&identity(
            "codex@algedonic.dev",
            "Codex (engineering)",
            true
        )),
        "{log}"
    );
    assert!(
        session("claude-b-factory").contains(&identity(
            "claude-b@algedonic.dev",
            "Claude (engineering, account B)",
            true
        )),
        "{log}"
    );
    for seat in SIGNED_SEATS {
        assert!(
            out.contains(&format!("seat {seat}: waiting - identity unconfirmed")),
            "the first pass names the wait: {out}"
        );
        assert!(
            out.contains(&format!("seat {seat}: started ")),
            "{seat}: {out}"
        );
        assert!(
            !out.contains(&format!("seat {seat}: not started")),
            "{seat}: {out}"
        );
    }
    assert!(
        log.matches("boss-api GET /api/agents").count() >= 3,
        "the registry is read again: {log}"
    );
}

/// The retry is BOUNDED: a registry that never answers ends the boot
/// with each signed seat named as not started, after reading the
/// registry more than once — and well inside the 300 s the pod's
/// dev-factory.sh gives the whole boot. A registry that ANSWERED, without
/// the alias, is not retried at all: no pass would change that answer.
#[test]
fn a_boot_bounds_the_retry_and_never_retries_an_answer() {
    let f = Fixture::new("boot-bound", &["codex", "claude"]);
    let started = std::time::Instant::now();
    let (rc, out) = boot(
        &f,
        &[
            ("STUB_API_DOWN", "1"),
            ("BOSS_DEV_BOOT_IDENTITY_WAIT", "1"),
            ("BOSS_DEV_BOOT_IDENTITY_PAUSE", "0.2"),
        ],
    );
    assert_eq!(rc, 0, "{out}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "bounded: {out}"
    );
    let log = f.log();
    no_seat_started_unsigned(&log, "bound");
    assert!(
        log.matches("boss-api GET /api/agents").count() >= 2 * SIGNED_SEATS.len(),
        "each signed seat's read is retried at least once: {log}"
    );
    for seat in SIGNED_SEATS {
        assert!(
            out.contains(&format!("seat {seat}: not started - identity unconfirmed")),
            "{seat}: {out}"
        );
    }

    let f = Fixture::new("boot-answered", &["codex", "claude"]);
    let (rc, out) = boot(
        &f,
        &[
            ("STUB_AGENTS", AGENTS_CLAUDE_ONLY),
            ("BOSS_DEV_BOOT_IDENTITY_WAIT", "30"),
            ("BOSS_DEV_BOOT_IDENTITY_PAUSE", "0"),
        ],
    );
    assert_eq!(rc, 0, "{out}");
    assert_eq!(
        f.log().matches("boss-api GET /api/agents").count(),
        SIGNED_SEATS.len(),
        "one read per signed seat: {}",
        f.log()
    );
    assert!(
        !out.contains("waiting - identity unconfirmed"),
        "an answer is not waited on: {out}"
    );
}

/// The refusal beside a running seat names the switch that keeps the
/// seat off across a roll (N3), since otherwise every roll brings the
/// seat back and the refusal with it.
#[test]
fn the_refusal_beside_a_seat_names_its_off_switch() {
    let f = Fixture::new("claude-b-beside-seat-off", &["claude"]);
    let out = f
        .cmd("claude-b-factory", &[])
        .args(["--detached", "5"])
        .stdin(Stdio::null())
        .output()
        .expect("run dev-session.sh");
    let (_, out) = collect(out);
    assert!(
        out.contains(&format!(
            "touch {}/.config/boss/seat-off/claude-b-factory",
            f.home()
        )),
        "{out}"
    );
}

/// Option 5 is refused while Claude B's seat runs, as option 2 is beside
/// the factory: one identity, one queue.
#[test]
fn claude_b_is_never_started_beside_its_seat() {
    let f = Fixture::new("claude-b-beside-seat", &["claude"]);
    let out = f
        .cmd("claude-b-factory", &[])
        .args(["--detached", "5"])
        .stdin(Stdio::null())
        .output()
        .expect("run dev-session.sh");
    let (rc, out) = collect(out);
    assert_ne!(rc, 0, "{out}");
    assert!(
        out.contains("tmux `claude-b-factory` is running and signs as the same agent"),
        "{out}"
    );
    assert!(!f.log().contains("new-session"), "{}", f.log());
}

/// An `ssh` stand-in: logs its argv and exits STUB_SSH_EXIT (default 0),
/// as a session that connected and was left would.
const SSH_STUB: &str =
    "#!/usr/bin/env bash\necho \"ssh $*\" >> \"$STUB_LOG\"\nexit \"${STUB_SSH_EXIT:-0}\"\n";

/// The standby door's `lan`, read from infra/estate/doors.toml — the
/// one place the menu reads it from too (the address's other copy, the
/// Service pin, is held equal by the_door_is_watched_from_outside_the_pod.rs).
fn standby_lan() -> String {
    let toml = std::fs::read_to_string(repo_root().join("infra/estate/doors.toml"))
        .expect("read doors.toml");
    toml.split("[[door]]")
        .find(|b| b.contains("id = \"dev-standby-ssh\""))
        .and_then(|b| {
            b.lines()
                .find_map(|l| l.trim().strip_prefix("lan = \""))
                .map(|v| v.trim_end_matches('"').to_string())
        })
        .expect("doors.toml declares door dev-standby-ssh with a lan half")
}

/// Option 7 is the standby shell (design 0491591b, backlog 47601829): an
/// ssh to the standby's LAN door, at the address doors.toml declares, so
/// the operator reaches it without remembering an address. It starts no
/// tmux session here — the standby is another pod — and a session that
/// ended is the login's end, as leaving any option's tmux is.
#[test]
fn option_seven_is_an_ssh_to_the_standby_at_its_declared_door() {
    let f = Fixture::new("opt7", &["claude"]);
    write_exec(&Path::new(&f.path).join("ssh"), SSH_STUB);
    let (rc, out) = f.run("dev", "7\n", &[]);
    assert_eq!(rc, 0, "{out}");
    let lan = standby_lan();
    let (addr, port) = lan.rsplit_once(':').expect("lan is address:port");
    assert!(
        out.contains("7) Standby shell") && out.contains(&format!("root@{addr}")),
        "the menu offers the standby and names where it goes: {out}"
    );
    assert!(
        f.log()
            .contains(&format!("ssh -o ConnectTimeout=10 -p {port} root@{addr}")),
        "{}",
        f.log()
    );
    assert!(
        !f.log().contains("new-session"),
        "the standby is not a tmux session on this pod: {}",
        f.log()
    );
}

/// ssh's own failure (exit 255: no route, refused, no key it will
/// take) is said with where else the standby is reached from, and goes
/// back to the menu: a login that chose the standby must never be lost
/// to a standby that did not answer.
#[test]
fn a_standby_that_does_not_answer_is_said_and_returns_to_the_menu() {
    let f = Fixture::new("opt7-fails", &["claude"]);
    write_exec(&Path::new(&f.path).join("ssh"), SSH_STUB);
    let (rc, out) = f.run("dev", "7\n4\n", &[("STUB_SSH_EXIT", "255")]);
    assert_eq!(rc, 0, "{out}");
    let lan = standby_lan();
    let (addr, _) = lan.rsplit_once(':').expect("lan is address:port");
    assert!(
        out.contains(&format!("could not reach the standby at {lan}")),
        "{out}"
    );
    assert!(
        out.contains(&format!("ssh root@{addr}")),
        "the refusal names the bastion's way in: {out}"
    );
    assert!(
        f.attached().starts_with("tmux new-session -A -s shell "),
        "the failure returns to the menu: {}",
        f.log()
    );
}
