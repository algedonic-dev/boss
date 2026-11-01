//! A child process that reads no proxy from the shell that ran the suite.
//!
//! WHY THIS IS SHARED (backlog 50708d76, F-b of review 0a666c59). The
//! machine-token host rule in `infra/lib/secret-header.sh` withholds both
//! secrets from a request curl would send through a proxy
//! (`machine_token_proxy`, F4 of review 927f8602), and curl hands a
//! loopback request to a proxy like any other. So a test that runs a
//! sender against a loopback stub and expects the header stamped reads
//! "withheld" — or reaches no stub at all — the moment the developer
//! shell that ran it has a proxy exported, a red the script did not
//! cause. The gate runner sets none, so the receipt stays green and the
//! red belongs only to whoever runs the suite by hand. Measured
//! 2026-10-10 by run 71ba06b3: every boss-testing binary run with the six
//! proxy variables pointed at a dark loopback port, against the same run
//! with none: 28 binaries red only under the proxy, all green after.
//!
//! Before this, seven test files each carried their own list, and the
//! copies had drifted: some removed two or four of the six proxy
//! variables, one removed five, three kept their own eight-entry list. A
//! list that lives seven times is the §9a shape (CLAUDE.md); this is the
//! one copy,
//! and `every_test_strips_the_proxy_through_the_one_helper.rs` refuses
//! any of these names quoted on a code line anywhere else, outside the
//! files it names as setting a proxy on purpose (backlog 9a12c070).

use std::process::Command;

/// Every variable curl, Python's urllib or reqwest reads a proxy from for
/// http and https URLs, in both spellings, and the two that exempt a host
/// from one. Not every proxy curl can read: it also takes a
/// `<scheme>_proxy` for its other protocols and `proxy=` from a curlrc,
/// and neither reaches an http(s) sender (review ea6cbc2f, finding 3).
pub const PROXY_VARS: [&str; 8] = [
    "http_proxy",
    "HTTP_PROXY",
    "https_proxy",
    "HTTPS_PROXY",
    "all_proxy",
    "ALL_PROXY",
    "no_proxy",
    "NO_PROXY",
];

/// Remove every proxy variable from `cmd`'s environment, so the child
/// connects where its URL says unless the test sets one itself AFTER
/// this call. Returns the command for chaining.
pub fn without_proxy(cmd: &mut Command) -> &mut Command {
    for var in PROXY_VARS {
        cmd.env_remove(var);
    }
    cmd
}

#[cfg(test)]
mod tests {
    use super::{PROXY_VARS, without_proxy};
    use std::process::Command;

    /// Each of the eight is REMOVED (an explicit `None`), not merely left
    /// unset in the builder: a removed variable is withheld from the
    /// child even when the parent process has it exported.
    #[test]
    fn it_removes_every_proxy_variable_in_both_spellings() {
        let mut cmd = Command::new("true");
        without_proxy(&mut cmd);
        let removed: Vec<String> = cmd
            .get_envs()
            .filter(|(_, v)| v.is_none())
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        for var in PROXY_VARS {
            assert!(removed.iter().any(|r| r == var), "{var} not removed");
        }
        for scheme in ["http", "https", "all", "no"] {
            assert!(PROXY_VARS.contains(&format!("{scheme}_proxy").as_str()));
            assert!(PROXY_VARS.contains(&format!("{}_PROXY", scheme.to_uppercase()).as_str()));
        }
    }

    /// By effect: a child of a process holding a proxy sees none.
    #[test]
    fn a_child_sees_no_proxy_the_test_did_not_set() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("env");
        for var in PROXY_VARS {
            cmd.env(var, "http://127.0.0.1:9");
        }
        without_proxy(&mut cmd);
        let out = cmd.output().unwrap();
        let env = String::from_utf8_lossy(&out.stdout);
        for var in PROXY_VARS {
            assert!(
                !env.lines().any(|l| l.starts_with(&format!("{var}="))),
                "{var} reached the child: {env}"
            );
        }
        // And a variable set after it does reach the child.
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("env");
        without_proxy(&mut cmd).env("http_proxy", "http://127.0.0.1:9");
        let out = cmd.output().unwrap();
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("http_proxy=http://127.0.0.1:9"),
            "a proxy the test sets after the helper is the test's own"
        );
    }
}
