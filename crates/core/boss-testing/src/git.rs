//! Git that reads the fixture's config and nothing the host brings.
//!
//! WHY THIS IS SHARED (backlog 3bef4198, from a8d8c956 on 2026-09-27).
//! A test that proves a script REFUSES a repository git says another user
//! owns (`GIT_TEST_ASSUME_DIFFERENT_OWNER=1`) depends on git actually
//! refusing. `safe.directory` switches that refusal off, and git reads it
//! from exactly four places: the system file, the global file, and the
//! two command-line channels (`GIT_CONFIG_COUNT` + `GIT_CONFIG_KEY_n` /
//! `GIT_CONFIG_VALUE_n`, and `GIT_CONFIG_PARAMETERS`). GitHub's ubuntu
//! runner image appends `[safe] directory = *` to /etc/gitconfig
//! (actions/runner-images install-git.sh), so on the public mirror every
//! ownership case that left one channel open ran a HEALTHY git, the
//! script read the fixture, and the assertion that it must refuse failed
//! — seven tests red there and green on the cluster gate. Two files had
//! each written their own four lines to close the channels; this is the
//! one copy, and `ownership_refusal_tests_close_every_config_channel.rs`
//! holds every test that asks git for that refusal to it (CLAUDE.md §9a:
//! a fact that lives twice gets an equality test, or one definition).
//!
//! Closing the command-line channel also drops any `safe.directory` a
//! caller exported for the REAL checkout (the gate appends one for its
//! own uid, `infra/gate.sh`). That costs nothing here: a fixture is never
//! the real checkout, and a child script that needs its own slot appends
//! after `GIT_CONFIG_COUNT`, which this sets to 0.

use std::process::Command;

/// Close every channel git reads `safe.directory` from: the system and
/// global files (`/dev/null`), the counted env channel (`0`), and the
/// flat `GIT_CONFIG_PARAMETERS` one (removed). Returns the command for
/// chaining.
pub fn git_config_isolated(cmd: &mut Command) -> &mut Command {
    cmd.env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_COUNT", "0")
        .env_remove("GIT_CONFIG_PARAMETERS")
}

#[cfg(test)]
mod tests {
    use super::git_config_isolated;
    use std::ffi::OsStr;
    use std::process::Command;

    fn env_of<'a>(cmd: &'a Command, key: &str) -> Option<Option<&'a OsStr>> {
        cmd.get_envs()
            .find(|(k, _)| *k == OsStr::new(key))
            .map(|(_, v)| v)
    }

    /// Every one of the four channels is named: the two files point at
    /// nothing, the counted channel is emptied, and the flat channel is
    /// removed — so a runner's `[safe] directory = *` cannot reach a
    /// fixture through any of them.
    #[test]
    fn it_closes_all_four_channels_git_reads_safe_directory_from() {
        let mut cmd = Command::new("git");
        git_config_isolated(&mut cmd);
        let dev_null = Some(Some(OsStr::new("/dev/null")));
        assert_eq!(env_of(&cmd, "GIT_CONFIG_SYSTEM"), dev_null);
        assert_eq!(env_of(&cmd, "GIT_CONFIG_GLOBAL"), dev_null);
        assert_eq!(
            env_of(&cmd, "GIT_CONFIG_COUNT"),
            Some(Some(OsStr::new("0")))
        );
        assert_eq!(
            env_of(&cmd, "GIT_CONFIG_PARAMETERS"),
            Some(None),
            "GIT_CONFIG_PARAMETERS must be REMOVED, not set: git parses any \
             value it is given as config"
        );
    }
}
