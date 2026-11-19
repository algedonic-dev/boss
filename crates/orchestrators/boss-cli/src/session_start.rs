//! `boss session start <codex|claude-b>` — start an interactive agent
//! session on the dev pod that is not running, and put the act on that
//! agent's record (backlog 2a2d11b2, David 2026-11-10).
//!
//! THE CASE. The dev pod rolled at about 01:40Z on 2026-11-10. The
//! interactive Codex (dev-session option 2, tmux `codex`) and Claude B
//! (option 5, tmux `claude-b`) start only from an interactive login, so
//! both waited for David: by 04:12Z neither had written its work-session,
//! seven of Claude B's packets and one of Codex's were stranded behind
//! them, and two silent alarms were open. The conductor's one recorded
//! door to a factory, `boss factory restart`, starts option 6 only, and
//! Claude B had no door at all. David, 05:3xZ: build a control that boots
//! those sessions so they are there for tmux.
//!
//! WHAT IT DOES. Exactly one bounded act: it runs the MAIN checkout's
//! `infra/dev/dev-session.sh --detached <2|5>` — the menu's own option,
//! so the session name, the identity check against the agents registry,
//! the CLI config, Claude B's login home and the command are the menu's,
//! defined once. Detached, a running session is REFUSED and never joined,
//! and the codex TUI is never started beside the factory loop (one
//! identity, one queue). Nothing is killed and nothing is typed: the
//! session it starts sits at its prompt, and its first prompt is the
//! tmux door (`boss peer send` for codex), a separate act.
//!
//! It then RECORDS what it saw and did as `session_start_<stamp>` on the
//! agent's newest work-session (the one `boss factory restart` records
//! on): the outcome (`started`, with the tmux session and its pane pid,
//! or `not_started`), the script's exit and everything it said on both
//! streams, who ran it and why. A refusal is recorded too — "already
//! running" is a fact about the session worth the record.
//!
//! READ BEFORE ACTING. The signer, the agents registry and a session to
//! record on are read first; a gap in any of them refuses with nothing
//! started. The script is the main checkout's, `$BOSS_DEV_ROOT/boss`
//! (`/work/boss` on the pod), never the caller's worktree: a worktree's
//! copy is unreviewed bytes, and these start a process that holds a
//! credential (as `boss factory restart`'s helper, N3 of review
//! b6467d24).

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Stdio;

/// The script the act runs, relative to the main checkout.
pub(crate) const SCRIPT: &str = "infra/dev/dev-session.sh";

/// The record key's prefix.
pub(crate) const RECORD_PREFIX: &str = "session_start_";

/// The interactive agent sessions this verb can start.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Agent {
    /// The interactive Codex TUI: dev-session option 2, tmux `codex`.
    Codex,
    /// Claude B, the second Claude account: option 5, tmux `claude-b`.
    ClaudeB,
}

/// What one agent is on the pod: its menu option, its tmux session and
/// the registry actor its work-session names.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Target {
    pub option: &'static str,
    pub session: &'static str,
    pub actor: &'static str,
}

impl Agent {
    pub(crate) fn target(self) -> Target {
        match self {
            Agent::Codex => Target {
                option: "2",
                session: "codex",
                actor: "agent-codex",
            },
            Agent::ClaudeB => Target {
                option: "5",
                session: "claude-b",
                actor: "agent-claude-b",
            },
        }
    }
}

/// The record key for an act at `now`, in `boss factory restart`'s stamp.
pub(crate) fn record_key(now: DateTime<Utc>) -> String {
    format!("{RECORD_PREFIX}{}", now.format("%Y%m%dT%H%M%SZ"))
}

/// The pane pid dev-session.sh printed for `session` (`<session> <pid>`,
/// its `-P -F` line), or `None` when it printed none.
pub(crate) fn launched_pid(stdout: &str, session: &str) -> Option<String> {
    stdout.lines().find_map(|l| {
        let (name, pid) = l.trim_end().split_once(' ')?;
        (name == session && !pid.is_empty() && pid.chars().all(|c| c.is_ascii_digit()))
            .then(|| pid.to_string())
    })
}

/// `started` only when the script exited 0 AND printed the session's pid;
/// anything else started nothing the record can name.
pub(crate) fn outcome(exit: i32, pid: Option<&str>) -> &'static str {
    if exit == 0 && pid.is_some() {
        "started"
    } else {
        "not_started"
    }
}

/// What the script did, as it said it.
pub(crate) struct Launched {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// What one act left behind, for the record.
pub(crate) struct Start<'a> {
    pub at: DateTime<Utc>,
    pub by: &'a str,
    pub why: &'a str,
    pub target: &'a Target,
    pub work_session: &'a str,
    pub launched: &'a Launched,
}

/// The note: what was seen and done, the script's words kept whole.
pub(crate) fn note(s: &Start<'_>) -> Value {
    let pid = launched_pid(&s.launched.stdout, s.target.session);
    json!({
        "at": s.at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "by": format!("{} boss session start", s.by),
        "why": s.why,
        "actor": s.target.actor,
        "option": s.target.option,
        "tmux_session": s.target.session,
        "outcome": outcome(s.launched.code, pid.as_deref()),
        "pid": pid,
        "script": SCRIPT,
        "script_exit": s.launched.code,
        "script_stdout": s.launched.stdout,
        "script_stderr": s.launched.stderr,
        "work_session": s.work_session,
    })
}

/// `$BOSS_DEV_ROOT`, `/work` when it is unset.
fn dev_root() -> PathBuf {
    std::env::var_os("BOSS_DEV_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/work"))
}

/// Run `<dev_root>/boss/infra/dev/dev-session.sh --detached <option>` with
/// no terminal, handing it the same `BOSS_DEV_ROOT` it was found under.
pub(crate) async fn launch(dev_root: &Path, option: &str) -> Result<Launched> {
    let script = crate::factory::main_checkout(Some(dev_root.into())).join(SCRIPT);
    if !script.is_file() {
        bail!(
            "REFUSED: {} is not there — the session starts only through the main checkout's \
             menu ($BOSS_DEV_ROOT/boss, /work/boss on the dev pod), never the caller's own; \
             nothing started",
            script.display()
        );
    }
    let out = tokio::process::Command::new("bash")
        .arg(&script)
        .args(["--detached", option])
        .env("BOSS_DEV_ROOT", dev_root)
        .stdin(Stdio::null())
        .output()
        .await
        .with_context(|| format!("running {}", script.display()))?;
    Ok(Launched {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
    })
}

pub(crate) async fn run(agent: Agent, why: &str) -> Result<()> {
    if why.is_empty() {
        bail!("--why is empty: a start says why it was run");
    }
    let target = agent.target();
    let by = crate::factory::signer("PATCH", "/api/jobs/{id}/metadata (the session_start_ note)")?;
    let http = crate::sor_client::machine_client()?;
    let spellings = crate::factory::registered(&http, target.actor).await?;
    let session = crate::factory::record_session(&http, target.actor, &spellings).await?;

    let launched = launch(&dev_root(), target.option).await?;
    eprint!("{}", launched.stderr);
    let now = boss_clock_client::wall_now();
    let key = record_key(now);
    let value = note(&Start {
        at: now,
        by: &by,
        why,
        target: &target,
        work_session: &session,
        launched: &launched,
    });
    let said = value["outcome"]
        .as_str()
        .unwrap_or("not_started")
        .to_string();
    let sent = json!({ key.clone(): value });
    let recorded = crate::factory::write_confirmed(&http, &session, &sent).await;
    let pid = launched_pid(&launched.stdout, target.session);
    println!(
        "boss session start: {said} — tmux {} pid {} (option {}, exit {})",
        target.session,
        pid.as_deref().unwrap_or("-"),
        target.option,
        launched.code
    );
    if launched.code != 0 {
        print!("{}", launched.stdout);
    }
    match recorded {
        Ok(()) => println!("boss session start: {key} recorded on {session}, read back"),
        Err(e) => bail!(
            "the act was done ({said}) and is NOT on the record for {session}: {e:#} — the note \
             it could not write: {sent}"
        ),
    }
    if said != "started" {
        bail!(
            "{SCRIPT} --detached {} started nothing (exit {}): {}",
            target.option,
            launched.code,
            launched.stdout.trim()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn tree(path: &str) -> String {
        std::fs::read_to_string(boss_testing::repo_root().join(path))
            .unwrap_or_else(|e| panic!("{path}: {e}"))
    }

    fn at() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-10T05:30:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    /// CLAUDE.md §9a: each agent's option and session are the menu's own
    /// (the printf line that shows it), and `--detached` accepts the
    /// option.
    #[test]
    fn each_agent_is_its_menu_option_and_session() {
        let menu = tree(SCRIPT);
        for agent in [Agent::Codex, Agent::ClaudeB] {
            let t = agent.target();
            let line = menu
                .lines()
                .find(|l| {
                    l.trim_start()
                        .starts_with(&format!("printf '  {}) ", t.option))
                })
                .unwrap_or_else(|| panic!("the menu's option {} line", t.option));
            assert!(
                line.contains(&format!(" {} \"$(status {})\"", t.session, t.session)),
                "option {} must be tmux `{}`: {line}",
                t.option,
                t.session
            );
            let accepts = menu
                .lines()
                .find(|l| l.contains("detached=$2"))
                .expect("the --detached option case");
            assert!(
                accepts.contains(t.option),
                "--detached must take {}: {accepts}",
                t.option
            );
        }
    }

    /// The actor each session records on is the one a harness file signs
    /// as (infra/platform/harnesses), so the record lands on the session
    /// the agent itself writes.
    #[test]
    fn each_agent_is_the_actor_its_harness_signs_as() {
        for (agent, file) in [
            (Agent::Codex, "infra/platform/harnesses/codex-exec.toml"),
            (
                Agent::ClaudeB,
                "infra/platform/harnesses/claude-code-b.toml",
            ),
        ] {
            let actor = agent.target().actor;
            assert!(
                tree(file)
                    .lines()
                    .any(|l| l.trim() == format!("actor = \"{actor}\"")),
                "{file} must sign as {actor}"
            );
        }
    }

    #[test]
    fn the_record_key_is_stamped_like_the_factory_restart() {
        assert_eq!(record_key(at()), "session_start_20261110T053000Z");
    }

    #[test]
    fn the_pid_is_the_sessions_own_launch_line() {
        let out = "placed /work/home/.codex/config.toml from the versioned infra/dev/cli/codex-config.toml\ncodex 4242\n";
        assert_eq!(launched_pid(out, "codex").as_deref(), Some("4242"));
        assert_eq!(launched_pid("codex-factory 4242\n", "codex"), None);
        assert_eq!(launched_pid("codex abc\n", "codex"), None);
        assert_eq!(launched_pid("codex \n", "codex"), None);
        assert_eq!(
            launched_pid(
                "dev-session: tmux `claude-b` is already running - --detached never joins, nothing started\n",
                "claude-b"
            ),
            None
        );
    }

    #[test]
    fn started_needs_both_a_clean_exit_and_a_pid() {
        assert_eq!(outcome(0, Some("1")), "started");
        assert_eq!(outcome(0, None), "not_started");
        assert_eq!(outcome(1, Some("1")), "not_started");
    }

    #[test]
    fn the_note_carries_what_was_seen_and_done() {
        let t = Agent::ClaudeB.target();
        let launched = Launched {
            code: 0,
            stdout: "claude-b 777\n".into(),
            stderr: "dev-session: WARNING - this copy is 2 commits behind\n".into(),
        };
        let v = note(&Start {
            at: at(),
            by: "claude@algedonic.dev",
            why: "the pod rolled",
            target: &t,
            work_session: "sess-b",
            launched: &launched,
        });
        assert_eq!(v["at"], "2026-11-10T05:30:00Z");
        assert_eq!(v["by"], "claude@algedonic.dev boss session start");
        assert_eq!(v["why"], "the pod rolled");
        assert_eq!(v["actor"], "agent-claude-b");
        assert_eq!(v["option"], "5");
        assert_eq!(v["tmux_session"], "claude-b");
        assert_eq!(v["outcome"], "started");
        assert_eq!(v["pid"], "777");
        assert_eq!(v["script"], SCRIPT);
        assert_eq!(v["script_exit"], 0);
        assert_eq!(v["script_stdout"], "claude-b 777\n");
        assert_eq!(
            v["script_stderr"],
            "dev-session: WARNING - this copy is 2 commits behind\n"
        );
        assert_eq!(v["work_session"], "sess-b");

        let refused = Launched {
            code: 1,
            stdout: "dev-session: tmux `claude-b` is already running - --detached never joins, nothing started\n".into(),
            stderr: String::new(),
        };
        let v = note(&Start {
            at: at(),
            by: "claude@algedonic.dev",
            why: "w",
            target: &t,
            work_session: "sess-b",
            launched: &refused,
        });
        assert_eq!(v["outcome"], "not_started");
        assert_eq!(v["pid"], Value::Null);
        assert!(
            v["script_stdout"]
                .as_str()
                .unwrap()
                .contains("already running")
        );
    }

    /// The script is the MAIN checkout's under `BOSS_DEV_ROOT`, run with
    /// `--detached <option>`, no terminal, and that same root handed on —
    /// never one found from the caller's checkout.
    #[tokio::test]
    async fn the_launch_runs_the_main_checkouts_menu_detached() {
        let root = boss_testing::scratch_dir("session-start-launch");
        let dir = root.join("boss/infra/dev");
        std::fs::create_dir_all(&dir).unwrap();
        boss_testing::write_exec(
            &dir.join("dev-session.sh"),
            "#!/usr/bin/env bash\n[ -t 0 ] && echo tty\necho \"args $*\"\necho \"root $BOSS_DEV_ROOT\"\necho \"codex 4242\"\necho said >&2\nexit 0\n",
        );
        let l = launch(&root, "2").await.unwrap();
        assert_eq!(l.code, 0);
        assert_eq!(
            l.stdout,
            format!("args --detached 2\nroot {}\ncodex 4242\n", root.display())
        );
        assert_eq!(l.stderr, "said\n");
        assert_eq!(launched_pid(&l.stdout, "codex").as_deref(), Some("4242"));

        let empty = boss_testing::scratch_dir("session-start-no-menu");
        let e = launch(&empty, "5").await.err().expect("refused");
        assert!(format!("{e:#}").contains("nothing started"), "{e:#}");

        let src = tree("crates/orchestrators/boss-cli/src/session_start.rs");
        let code = src.split("#[cfg(test)]").next().unwrap();
        assert!(
            !code.contains("repo_root()"),
            "the verb never locates the menu from the caller's checkout"
        );
    }

    #[test]
    fn start_takes_one_of_two_agents_and_a_reason() {
        for ok in [
            vec!["boss", "session", "start", "codex", "--why", "rolled"],
            vec![
                "boss",
                "session",
                "start",
                "claude-b",
                "--why-file",
                "w.txt",
            ],
        ] {
            assert!(crate::Cli::try_parse_from(&ok).is_ok(), "{ok:?}");
        }
        for bad in [
            vec!["boss", "session", "start", "codex"],
            vec!["boss", "session", "start", "gemini", "--why", "w"],
            vec!["boss", "session", "start", "codex-factory", "--why", "w"],
            vec!["boss", "session", "start", "--why", "w"],
        ] {
            assert!(crate::Cli::try_parse_from(&bad).is_err(), "{bad:?}");
        }
    }
}
