//! The conductor's forge credential rides on its OWN git commands — as
//! configuration only those child processes can see — and is written
//! into nobody's `~/.gitconfig`.
//!
//! WHY. The conductor moved into the cluster with only a token file
//! (`BOSS_TRAIN_FORGE_TOKEN_FILE`), and its clone, fetch and push all
//! need it on a private forge. The first fix wrote it with `git config
//! --global http.extraHeader` on every preflight, reasoning that the
//! container's HOME is its own and does not survive a roll. Both true —
//! and on 2026-09-05 an operator ran one `boss train cancel` on the
//! shared dev pod with a different token file, and that write replaced
//! the pod's working push credential with a read-only token: every
//! `git push` from the pod answered 403 on the receive-pack handshake
//! (reads fine) until the header was found and removed by hand
//! (packet 10bb1e1a). A verb given a credential for ITS clone must not
//! reach into the operator's global git config with it.
//!
//! HOW. git reads `GIT_CONFIG_COUNT` / `GIT_CONFIG_KEY_n` /
//! `GIT_CONFIG_VALUE_n` from the environment as configuration of the
//! highest precedence — the same precedence as `git -c`, above every
//! file. [`command`] returns a `git` `Command` carrying the header as
//! exactly that, scoped to the forge URL (`http.<forge>/.extraHeader`,
//! so the token never rides to another host), computed from the token
//! file each time. Nothing is written anywhere; a fresh pod has the
//! credential the moment it has the token file; an operator's own git
//! config is never touched. A parent that already exports
//! `GIT_CONFIG_COUNT` keeps its entries — ours is appended after them.
//!
//! NO PROXY. Beside the header goes an EMPTY `http.<forge>/.proxy`,
//! scoped the same way. git's curl otherwise reads the proxy from
//! `http_proxy` / `all_proxy` and their spellings, loopback and plain
//! http included, and sends the header to whatever answers there: the
//! Rust senders of the same token were closed against that in 6162aff2
//! and git was not (review a2172ed3 of car 01872c8e, backlog 3107bfb1).
//! An empty `http.proxy` is git's own "disable proxying" — it is read
//! before any variable and handed to curl as an empty CURLOPT_PROXY —
//! so no exemption list has to name the forge's host the way curl
//! would spell it. Measured with git 2.39.5, the Debian bookworm git
//! the conductor's runtime image (`debian:bookworm-slim`) installs, by
//! a test that runs git against a recording proxy.
//!
//! Best-effort by design: no token file, or an empty one, means the
//! command runs anonymous and fails loudly at the next remote op rather
//! than silently pretending to be configured.

use std::fs;
use std::process::Command;

use crate::train::env_or;

/// The forge credential: the header carrying the token, and the forge
/// URL both of its git configuration entries are scoped to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ForgeAuth {
    /// The forge URL with exactly one trailing slash, so a scoped key
    /// matches every repository under it and no other host.
    pub(crate) forge: String,
    /// The header line itself.
    pub(crate) value: String,
}

impl ForgeAuth {
    /// `http.<forge>/.extraHeader` — the token, on requests to the forge.
    pub(crate) fn header_key(&self) -> String {
        format!("http.{}.extraHeader", self.forge)
    }

    /// `http.<forge>/.proxy` — set EMPTY beside the header, which git
    /// reads as "no proxy for this URL" ahead of every proxy variable
    /// (backlog 3107bfb1). Without it git's curl went wherever the
    /// environment's proxy pointed, header and all.
    pub(crate) fn proxy_key(&self) -> String {
        format!("http.{}.proxy", self.forge)
    }
}

/// The credential from the environment the conductor runs in, or None
/// when there is no usable token file.
pub(crate) fn forge_auth() -> Option<ForgeAuth> {
    forge_auth_from(
        &env_or("BOSS_TRAIN_FORGE_TOKEN_FILE", "/etc/boss-train/forge.token"),
        &env_or("BOSS_TRAIN_FORGE_URL", "http://10.20.0.15:3000"),
    )
}

/// The credential from a token file and a forge URL. Pure apart from
/// the file read, so the shape is pinned without touching the process
/// environment.
pub(crate) fn forge_auth_from(token_file: &str, forge_url: &str) -> Option<ForgeAuth> {
    let token = fs::read_to_string(token_file).ok()?;
    let token = token.trim();
    if token.is_empty() {
        return None;
    }
    Some(ForgeAuth {
        forge: format!("{}/", forge_url.trim_end_matches('/')),
        value: format!("Authorization: token {token}"),
    })
}

/// A `git` command carrying the forge credential, if there is one.
pub(crate) fn command() -> Command {
    command_with(forge_auth().as_ref())
}

/// A `git` command carrying `auth`, if given — [`command`] with the
/// credential passed in, so a caller can pin that its git carries one
/// without writing the process environment (backlog cb3d8952).
pub(crate) fn command_with(auth: Option<&ForgeAuth>) -> Command {
    let mut cmd = Command::new("git");
    if let Some(auth) = auth {
        let base = std::env::var("GIT_CONFIG_COUNT")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(0);
        apply_at(&mut cmd, auth, base);
    }
    cmd
}

/// Put `auth` on `cmd` as the `n`-th and `n+1`-th `GIT_CONFIG_*`
/// entries — the header, then the empty proxy that keeps it on the
/// direct route — keeping any entries `0..n` the parent already exports.
/// The two are written together or not at all: a header without its
/// proxy entry is the leak this module closed (backlog 3107bfb1).
pub(crate) fn apply_at(cmd: &mut Command, auth: &ForgeAuth, n: usize) {
    cmd.env("GIT_CONFIG_COUNT", (n + 2).to_string());
    cmd.env(format!("GIT_CONFIG_KEY_{n}"), auth.header_key());
    cmd.env(format!("GIT_CONFIG_VALUE_{n}"), &auth.value);
    let p = n + 1;
    cmd.env(format!("GIT_CONFIG_KEY_{p}"), auth.proxy_key());
    cmd.env(format!("GIT_CONFIG_VALUE_{p}"), "");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token_file(contents: &str) -> tempfile::NamedTempFile {
        let f = tempfile::NamedTempFile::new().unwrap();
        fs::write(f.path(), contents).unwrap();
        f
    }

    #[test]
    fn the_credential_is_a_header_scoped_to_the_forge_url() {
        let f = token_file("abc123\n");
        let auth = forge_auth_from(f.path().to_str().unwrap(), "http://10.20.0.15:3000/").unwrap();
        assert_eq!(
            auth.header_key(),
            "http.http://10.20.0.15:3000/.extraHeader"
        );
        assert_eq!(auth.proxy_key(), "http.http://10.20.0.15:3000/.proxy");
        assert_eq!(auth.value, "Authorization: token abc123");
        // Spelled with or without the slash, the scope is the same.
        let bare = forge_auth_from(f.path().to_str().unwrap(), "http://10.20.0.15:3000").unwrap();
        assert_eq!(bare, auth);
    }

    #[test]
    fn no_token_means_no_credential_not_a_broken_one() {
        assert_eq!(
            forge_auth_from("/nonexistent/forge.token", "http://f"),
            None
        );
        let empty = token_file("  \n");
        assert_eq!(
            forge_auth_from(empty.path().to_str().unwrap(), "http://f"),
            None
        );
    }

    #[test]
    fn git_reads_the_credential_off_the_command_and_nothing_is_written() {
        // The behavioural pin: a git child of this command sees the
        // header as configuration — and only that child does. The
        // process's own environment and every config file stay as
        // they were, which is the whole point.
        let f = token_file("secret-token");
        let auth = forge_auth_from(f.path().to_str().unwrap(), "http://forge.test").unwrap();
        let mut cmd = Command::new("git");
        apply_at(&mut cmd, &auth, 0);
        let out = cmd
            .args(["config", "--get", &auth.header_key()])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "Authorization: token secret-token"
        );
        // A plain git in the same process sees nothing of it.
        let plain = Command::new("git")
            .args(["config", "--get", &auth.header_key()])
            .output()
            .unwrap();
        assert!(
            !plain.status.success(),
            "the header leaked outside the command"
        );
    }

    #[test]
    fn a_parents_git_config_entries_are_kept_and_ours_appended() {
        let f = token_file("t");
        let auth = forge_auth_from(f.path().to_str().unwrap(), "http://forge.test").unwrap();
        let mut cmd = Command::new("git");
        cmd.env("GIT_CONFIG_COUNT", "1");
        cmd.env("GIT_CONFIG_KEY_0", "boss.parent");
        cmd.env("GIT_CONFIG_VALUE_0", "kept");
        apply_at(&mut cmd, &auth, 1);
        let out = cmd
            .args([
                "config",
                "--get-regexp",
                "^(boss\\.parent|http\\..*extraheader)$",
            ])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("boss.parent kept"), "{text}");
        assert!(text.contains("Authorization: token t"), "{text}");
        // And the proxy entry rides after the header, as the third.
        let proxy = cmd_env(&cmd, "GIT_CONFIG_KEY_2");
        assert_eq!(proxy.as_deref(), Some("http.http://forge.test/.proxy"));
        assert_eq!(cmd_env(&cmd, "GIT_CONFIG_COUNT").as_deref(), Some("3"));
    }

    fn cmd_env(cmd: &Command, name: &str) -> Option<String> {
        cmd.get_envs()
            .find(|(k, _)| *k == name)
            .and_then(|(_, v)| v)
            .map(|v| v.to_string_lossy().into_owned())
    }

    /// Accept one connection, record its request head, answer 404 so the
    /// git that sent it gives up at once. A listener nobody reaches
    /// leaves its thread parked on `accept` until the test binary exits.
    fn record_once(listener: std::net::TcpListener) -> std::sync::mpsc::Receiver<String> {
        use std::io::{Read, Write};
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let Ok((mut sock, _)) = listener.accept() else {
                return;
            };
            let mut head = Vec::new();
            let mut buf = [0u8; 1024];
            while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                match sock.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => head.extend_from_slice(&buf[..n]),
                }
            }
            let _ = tx.send(String::from_utf8_lossy(&head).into_owned());
            let _ = sock.write_all(
                b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
            );
        });
        rx
    }

    /// What the forge and a proxy each saw of one `git ls-remote` against
    /// the forge, run by the command `git_for` builds for that forge's
    /// URL, with every proxy variable pointed at the proxy. The proxy is
    /// `127.0.0.2`, a different host from the forge's `127.0.0.1`.
    /// The variables are set on the CHILD (after the one helper strips
    /// the suite-runner's own), so no other test in this binary sees
    /// them and this test needs no binary of its own.
    fn ls_remote_under_a_proxy(
        git_for: impl FnOnce(&str) -> Command,
    ) -> (Option<String>, Option<String>) {
        let forge = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let forge_url = format!("http://{}", forge.local_addr().unwrap());
        let forge_saw = record_once(forge);
        let proxy = std::net::TcpListener::bind("127.0.0.2:0").unwrap();
        let proxy_url = format!("http://{}", proxy.local_addr().unwrap());
        let proxy_saw = record_once(proxy);
        let mut cmd = git_for(&forge_url);
        boss_testing::without_proxy(&mut cmd);
        // The first six of the one list are the proxies themselves; the
        // last two are the exemptions, left removed by the helper.
        for var in &boss_testing::PROXY_VARS[..6] {
            cmd.env(var, &proxy_url);
        }
        let _ = cmd
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .args(["ls-remote", &format!("{forge_url}/david/boss.git")])
            .output()
            .unwrap();
        let wait = std::time::Duration::from_millis(1500);
        (
            forge_saw.recv_timeout(wait).ok(),
            proxy_saw.recv_timeout(wait).ok(),
        )
    }

    /// Backlog 3107bfb1 (review a2172ed3 of car 01872c8e): the Rust
    /// senders of the forge token stopped following an ambient proxy
    /// (6162aff2), and git did not — curl, under git, reads the proxy
    /// variables, so the train's clone, fetch and push handed the WRITE
    /// token to whatever answered at the proxy.
    #[test]
    fn a_git_carrying_the_forge_token_follows_no_ambient_proxy() {
        const TOKEN: &str = "forge-write-token-value";
        let f = token_file(TOKEN);
        let token_path = f.path().to_str().unwrap().to_string();

        // The control: a git given ONLY the header, the shape this module
        // built before, goes to the proxy with the token on it — so the
        // fixture is one this git reads, and a leak is one it catches.
        let (forge, proxy) = ls_remote_under_a_proxy(|url| {
            let auth = forge_auth_from(&token_path, url).unwrap();
            let mut cmd = Command::new("git");
            cmd.env("GIT_CONFIG_COUNT", "1")
                .env("GIT_CONFIG_KEY_0", auth.header_key())
                .env("GIT_CONFIG_VALUE_0", auth.value);
            cmd
        });
        assert_eq!(
            forge, None,
            "the control went direct: the fixture proxies nothing"
        );
        assert!(
            proxy.as_deref().is_some_and(|h| h.contains(TOKEN)),
            "the control's proxy did not see the token, so this fixture cannot catch a leak: \
             {proxy:?}"
        );

        // The command every git here is built by.
        let (forge, proxy) =
            ls_remote_under_a_proxy(|url| command_with(forge_auth_from(&token_path, url).as_ref()));
        assert_eq!(
            proxy, None,
            "a git carrying the forge token sent it to the proxy the environment names"
        );
        assert!(
            forge.as_deref().is_some_and(|h| h.contains(TOKEN)),
            "the forge did not receive the credentialed request directly: {forge:?}"
        );
    }
}
