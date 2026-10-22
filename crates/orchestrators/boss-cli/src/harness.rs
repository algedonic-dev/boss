//! A HARNESS IS A ROW, NOT A BRANCH (backlog 2f7b8c00, 2026-10-07).
//!
//! A harness is what turns a dispatched prompt into a running agent:
//! Claude Code's Agent tool, the Codex CLI. Until this module, which one
//! ran a step was decided by whoever typed the command — `boss dispatch`
//! printed a prompt and the coordinator's own harness ran it, whatever
//! model the step's block named — and the one process launcher there was
//! (`boss launch`) picked its harness from a closed `--harness` list and
//! a `gpt-` prefix. So a step declaring `gpt-6.1-sol` ran on Opus unless
//! a Codex session dispatched it to itself, and 86 runs did exactly that.
//!
//! THE FACTS ARE DATA: one file per harness under [`DIR`], read from the
//! tree the verb stands in. A file says who the harness signs as, how it
//! is launched, what environment it may inherit, which sandbox each
//! profile gets, how its stdout names its runtime, and where its
//! transcript lands. Nothing here names a harness; the tests pin the
//! shipped files, and a third harness is a third file plus — only if its
//! transcript is a new format — one more name in [`ADAPTERS`].
//!
//! WHICH HARNESS RUNS A MODEL is two reads ([`for_model`]): the agents
//! registry row whose `default_model` is the model names the actor, and
//! the file whose `actor` is that row's id is the harness. None, or more
//! than one, of either is a refusal that names what it found.

use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The roster: every `*.toml` here is one harness.
pub(crate) const DIR: &str = "infra/platform/harnesses";

/// The transcript formats `transcript_usage` can read. A file naming
/// another is refused at load: its runs would be launched and then
/// reported "not metered" for ever, which is the defect this module
/// was built on.
pub(crate) const ADAPTERS: [&str; 2] = ["claude-subagent", "codex-rollout"];

/// Names a pass list may never carry: the launcher sets the first and
/// last itself, from the harness's own row, and the file variable is the
/// other way to name an actor. The same three are what the published
/// dev-isolation door (`infra/dev/control-tools.py`) must hand the CLI
/// (backlog 19dbaab9; pinned in `door_env.rs`).
pub(crate) const IDENTITY_ENV: [&str; 3] = [
    crate::identity::ACTOR_ENV,
    crate::identity::ACTOR_FILE_ENV,
    crate::gate::AGENT_RUN_ENV,
];

/// A name a pass list may not carry, and why (review f7f0b689, B1). The
/// three identity names; then anything that reads as a credential or as
/// another harness's session, judged on the NAME, case-blind. Read over
/// `env` AND over `transcript.home_env`, which is a variable the launcher
/// passes on too. This is a floor, NOT a safe-list — `DATABASE_URL`,
/// `KUBECONFIG` and `NETRC` pass it — and not a proof that a list is safe: a secret under an innocent
/// name passes it, which is one reason every change under [`DIR`] is
/// held for review (`mutating_verb::ALWAYS_DIRS`). The other reason is
/// `argv` and `sandbox`, which this module does not try to judge at
/// all — no list of "safe" commands would be true.
pub(crate) fn forbidden_env(name: &str) -> Option<&'static str> {
    let upper = name.to_ascii_uppercase();
    if upper.is_empty()
        || !upper
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        || upper.starts_with(|c: char| c.is_ascii_digit())
    {
        return Some("is not a plain variable name");
    }
    if IDENTITY_ENV.iter().any(|i| upper == *i) || upper.starts_with("BOSS_ACTOR") {
        return Some(
            "names an actor or a run — the launcher sets the harness's own and passes no other",
        );
    }
    if upper.starts_with("BOSS_MACHINE_TOKEN") {
        return Some("is the estate machine token's");
    }
    if ["ANTHROPIC", "CLAUDE"].iter().any(|p| upper.starts_with(p)) {
        return Some("belongs to another harness's session");
    }
    // What the process could NOT get by reading the account's files
    // (review e9514316, N13): a live agent socket, and the variables
    // that make it run code the file's argv never names.
    if upper == "SSH_AUTH_SOCK" {
        return Some("hands over a live agent socket");
    }
    // THE LOADERS, BY THE MEMBERS KNOWN TODAY. N13's list named one
    // member of most families; review b149c579 found eleven siblings and
    // review b40dbff7 eighteen more, five of them run as attacker-named
    // code on the dev pod. So this list is NOT complete and does not
    // claim to be: it holds the members of each family known so far —
    // the dynamic linker (LD_, DYLD_), every git variable but its
    // identity text, the programs ssh, sudo, cargo, a shell and a pager
    // run on their own, the configuration homes and library configs
    // whose files name code to load, and each interpreter's module path
    // and start-up options. The control on a pass list is the dock hold
    // every change under [`DIR`] gets; this floor only spares that review
    // the names it already knows.
    //
    // git is a PREFIX (b40dbff7): GIT_DIR, GIT_COMMON_DIR and
    // GIT_WORK_TREE choose whose config — core.fsmonitor, core.pager,
    // hooks — git reads, and naming members one at a time is how two
    // reviews each found more. GIT_AUTHOR_* and GIT_COMMITTER_* stay
    // passable: text a commit records, which runs nothing.
    //
    // cargo, rustup, npm's config, pip and Lua are PREFIXES for the same
    // reason (re-review 702da60d, backlog 015ed327): cargo reads every
    // config key from a CARGO_<KEY> variable — CARGO_BUILD_RUSTC_WRAPPER
    // and CARGO_HOME (whose config.toml sets build.rustc-wrapper) were
    // each measured running a named wrapper — rustup picks the toolchain
    // whose binaries run from RUSTUP_TOOLCHAIN (a path is accepted) and
    // RUSTUP_HOME, npm and pip read every option from npm_config_<key> and
    // PIP_<OPTION> (script-shell names a program; a config file names an
    // index whose packages build by running their own code), and every
    // LUA_ variable is a start-up chunk or a module path.
    let git_identity = upper.starts_with("GIT_AUTHOR_") || upper.starts_with("GIT_COMMITTER_");
    if upper.starts_with("LD_")
        || upper.starts_with("DYLD_")
        || (upper.starts_with("GIT_") && !git_identity)
        || ["CARGO_", "RUSTUP_", "NPM_CONFIG_", "PIP_", "LUA_"]
            .iter()
            .any(|p| upper.starts_with(p))
        || [
            // Shells.
            "BASH_ENV",
            "ENV",
            "PROMPT_COMMAND",
            "ZDOTDIR",
            "SHELLOPTS",
            "PS4",
            // Programs ssh, sudo and other tools run by themselves.
            "SSH_ASKPASS",
            "SSH_SK_PROVIDER",
            "SSH_SK_HELPER",
            "SUDO_ASKPASS",
            "EDITOR",
            "VISUAL",
            "PAGER",
            "MANPAGER",
            "LESSOPEN",
            "LESSCLOSE",
            // cargo runs BROWSER for `cargo doc --open`.
            "BROWSER",
            // The compilers and wrappers cargo runs. CARGO and RUSTFMT
            // are bare names the CARGO_ prefix cannot see: cargo-clippy
            // and cargo-fmt run CARGO as cargo, cargo-fmt runs RUSTFMT
            // as rustfmt (review dfd43fbd, backlog 91a545eb).
            "CARGO",
            "RUSTFMT",
            "RUSTC_WRAPPER",
            "RUSTC_WORKSPACE_WRAPPER",
            "RUSTFLAGS",
            "RUSTC",
            "RUSTDOC",
            "RUSTDOCFLAGS",
            "CC",
            "CXX",
            // Configuration homes and library configs whose files name
            // programs to run or modules to load.
            "XDG_CONFIG_HOME",
            "AWS_CONFIG_FILE",
            "DOCKER_CONFIG",
            "GNUPGHOME",
            "GEM_HOME",
            "GEM_PATH",
            "XDG_CONFIG_DIRS",
            "OPENSSL_CONF",
            "OPENSSL_MODULES",
            "GCONV_PATH",
            // Interpreters' module paths and start-up options.
            "NODE_OPTIONS",
            "NODE_PATH",
            "PYTHONSTARTUP",
            "PYTHONPATH",
            "PYTHONHOME",
            "PYTHONUSERBASE",
            "PYTHONWARNINGS",
            "PYTHONBREAKPOINT",
            "PERL5OPT",
            "PERL5LIB",
            "PERLLIB",
            "RUBYOPT",
            "RUBYLIB",
            "JAVA_TOOL_OPTIONS",
            "_JAVA_OPTIONS",
        ]
        .contains(&upper.as_str())
    {
        return Some("makes the process load or run code its argv never names");
    }
    // WHAT THE TOOLCHAIN AND THE cc CRATE READ (backlog 685ce87c,
    // 2026-10-21). The list above grew one review at a time; the test
    // `the_floor_judges_every_name_the_pinned_toolchain_carries` now reads
    // the names out of cargo, cargo-clippy, cargo-fmt, clippy-driver and
    // rustfmt themselves and fails on one this floor neither refuses nor
    // passes with a written reason. Read off 1.96.1, the toolchain's own
    // namespaces are refused as PREFIXES: RUSTC (RUSTC_BOOTSTRAP unlocks
    // -Z codegen-backend, a library rustc loads; RUSTC_LINKER is the
    // linker cc infers its compiler from), RUSTDOC, CLIPPY_ (clippy-driver
    // appends CLIPPY_ARGS to rustc's command line, where -C linker= names
    // a program), and cargo's and clippy's private channels __CARGO and
    // __CLIPPY (__CARGO_FIX_PLZ makes cargo act as rustc's wrapper and
    // rewrite source). CARGO, RUSTFMT and BROWSER are the bare programs
    // cargo-clippy, cargo-fmt and `cargo doc --open` run (review dfd43fbd,
    // item 91a545eb, whose car names the same three in the list above).
    //
    // cc is a library, so no binary carries its names: they are read from
    // its source ([`CC_TOOLS`]) and pinned to the version Cargo.lock holds.
    // Each tool or flag variable is refused bare, as NAME_<target>, and as
    // HOST_NAME / TARGET_NAME — the four spellings cc 1.2.64's
    // `target_envs` reads. CROSS_COMPILE is the compiler's program prefix,
    // WASI_SDK_PATH the directory whose clang cc runs, SDKROOT, LIB and the
    // two wasm sysroots name libraries linked into what it builds, and
    // LDFLAGS carries -B and -fuse-ld= to the linker other build tools run.
    let cc_name = upper
        .strip_prefix("HOST_")
        .or_else(|| upper.strip_prefix("TARGET_"))
        .unwrap_or(&upper);
    if ["RUSTC", "RUSTDOC", "CLIPPY_", "__CARGO", "__CLIPPY"]
        .iter()
        .any(|p| upper.starts_with(p))
        || CC_TOOLS
            .iter()
            .any(|t| cc_name == *t || cc_name.starts_with(&format!("{t}_")))
        || [
            "CARGO",
            "RUSTFMT",
            "BROWSER",
            "CROSS_COMPILE",
            "WASI_SDK_PATH",
            "WASI_SYSROOT",
            "WASM_MUSL_SYSROOT",
            "SDKROOT",
            "LIB",
            "LDFLAGS",
        ]
        .contains(&upper.as_str())
    {
        return Some("makes the process load or run code its argv never names");
    }
    if ["TOKEN", "KEY", "SECRET", "PASSWORD", "CREDENTIAL"]
        .iter()
        .any(|w| upper.contains(w))
    {
        return Some("reads as a credential");
    }
    None
}

/// The cc crate's tool and flag variables (cc 1.2.64, src/lib.rs: each
/// `getenv_with_target_prefixes` and `envflags` call). Each names a
/// program a build script runs or flags handed to one.
pub(crate) const CC_TOOLS: [&str; 10] = [
    "CC",
    "CXX",
    "AR",
    "RANLIB",
    "NVCC",
    "CXXSTDLIB",
    "CFLAGS",
    "CXXFLAGS",
    "ARFLAGS",
    "RANLIBFLAGS",
];

/// Whose HOME a launched process gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Home {
    /// An empty directory made for the launch. The harness's own home
    /// variable (`transcript.home_env`) is pointed at its real login
    /// directory, so it still finds its sign-in and writes its
    /// transcripts where the meter looks — and the account's actor file,
    /// the other harness's login and the git credential helper are not
    /// under the HOME it is given.
    Own,
    /// The launching account's HOME, passed through. Everything under
    /// it is one relative path away.
    Inherit,
}

/// How a harness is started. Two mechanisms, not two harnesses: any
/// number of files may be a `process`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Launch {
    /// `boss dispatch --launch` spawns `argv`.
    Process,
    /// A trusted launcher observes a private cluster Job. Selection does
    /// not authorize activation or supply image/login credentials.
    Cluster,
    /// The coordinator's own session starts it (an Agent tool call);
    /// there is nothing for `boss` to spawn.
    HostTool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Event {
    /// The `type` of the stdout JSONL event.
    pub event: String,
    /// The key on that event holding the runtime id (`started` only).
    #[serde(default)]
    pub key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Transcript {
    pub adapter: String,
    /// The variable that relocates the harness's home, when set.
    pub home_env: String,
    /// Its home under `$HOME` otherwise.
    pub home_default: String,
    /// Every transcript, relative to the home; `*` matches within one
    /// path segment.
    pub glob: String,
    /// The one transcript of a known runtime id (`{runtime}`).
    #[serde(default)]
    pub runtime_glob: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Harness {
    pub id: String,
    /// The registered agent id (`agent-<slug>`) this harness signs as.
    pub actor: String,
    pub launch: Launch,
    #[serde(default)]
    pub argv: Vec<String>,
    #[serde(default)]
    pub env: Vec<String>,
    /// Required of a process harness — there is no default, because the
    /// weaker answer must be one somebody wrote down.
    #[serde(default)]
    pub home: Option<Home>,
    #[serde(default)]
    pub sandbox: BTreeMap<String, String>,
    #[serde(default)]
    pub started: Option<Event>,
    #[serde(default)]
    pub completed: Option<Event>,
    pub transcript: Transcript,
}

/// The placeholders an `argv` word may carry.
const PLACEHOLDERS: [&str; 4] = ["model", "effort", "sandbox", "last_message"];

fn placeholders(word: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut rest = word;
    while let Some(open) = rest.find('{') {
        let Some(close) = rest[open..].find('}') else {
            break;
        };
        found.push(&rest[open + 1..open + close]);
        rest = &rest[open + close + 1..];
    }
    found
}

impl Harness {
    /// One file's text, checked. `stem` is the file's name without
    /// `.toml`, which the `id` must equal — the directory is the roster.
    pub(crate) fn parse(stem: &str, text: &str) -> std::result::Result<Harness, String> {
        let h: Harness = toml::from_str(text).map_err(|e| e.to_string())?;
        if h.id != stem {
            return Err(format!("id {:?} is not the file's name {stem:?}", h.id));
        }
        if !boss_jobs::agents::types::is_agent_id(&h.actor) {
            return Err(format!(
                "actor {:?} is not a registered agent id (agent-<slug>) — a login is an alias \
                 of one, not an identity",
                h.actor
            ));
        }
        if !ADAPTERS.contains(&h.transcript.adapter.as_str()) {
            return Err(format!(
                "transcript adapter {:?} is not one the meter reads ({})",
                h.transcript.adapter,
                ADAPTERS.join(", ")
            ));
        }
        if let Some((name, why)) = h
            .env
            .iter()
            .find_map(|n| forbidden_env(n).map(|why| (n, why)))
        {
            return Err(format!("env passes {name:?}, which {why}"));
        }
        match h.launch {
            Launch::HostTool => {
                if !h.argv.is_empty()
                    || h.started.is_some()
                    || h.completed.is_some()
                    || h.home.is_some()
                    || !h.env.is_empty()
                {
                    return Err(
                        "a host-tool harness is started by its coordinator: it declares no \
                         argv, env, home, started or completed"
                            .into(),
                    );
                }
            }
            Launch::Process | Launch::Cluster => {
                if h.argv.is_empty() {
                    return Err("a process harness declares its argv".into());
                }
                // The home variable is a pass channel too (review e9514316, N12),
                // for a PROCESS (a host-tool harness is handed nothing):
                // for a home of its own the launcher hands the process the
                // launching session's VALUE of it. It was judged by nothing, so
                // `home_env = "ANTHROPIC_API_KEY"` handed that key over.
                if let Some(why) = forbidden_env(&h.transcript.home_env) {
                    return Err(format!(
                        "transcript.home_env names {:?}, which {why} — the launcher passes that \
                         variable's value to the process",
                        h.transcript.home_env
                    ));
                }
                for word in &h.argv {
                    if let Some(unknown) = placeholders(word)
                        .into_iter()
                        .find(|p| !PLACEHOLDERS.contains(p))
                    {
                        return Err(format!(
                            "argv word {word:?} carries {{{unknown}}}, which the launcher does \
                             not fill ({})",
                            PLACEHOLDERS.join(", ")
                        ));
                    }
                }
                if !h.argv.iter().any(|w| w.contains("{model}")) {
                    return Err(
                        "argv never names {model}: the process would run whatever its own \
                         configuration says, under a run that declares another"
                            .into(),
                    );
                }
                match &h.started {
                    Some(e) if e.key.as_deref().is_some_and(|k| !k.is_empty()) => {}
                    _ => {
                        return Err(
                            "a process harness declares [started] event and key: without a \
                             runtime id its transcript cannot be named"
                                .into(),
                        );
                    }
                }
                if h.completed.is_none() {
                    return Err(
                        "a process harness declares [completed] event: an exit code alone \
                         does not say a turn finished"
                            .into(),
                    );
                }
                if h.transcript.runtime_glob.is_none() {
                    return Err("a process harness declares transcript.runtime_glob".into());
                }
                match h.home {
                    None => {
                        return Err(
                            "a process harness declares home = \"own\" or home = \"inherit\": \
                             whose HOME the process gets is not left to a default"
                                .into(),
                        );
                    }
                    Some(Home::Own) => {
                        if let Some(set) = h
                            .env
                            .iter()
                            .find(|n| *n == "HOME" || **n == h.transcript.home_env)
                        {
                            return Err(format!(
                                "env passes {set} and home = \"own\": the launcher sets HOME \
                                 and {} itself for a home of its own",
                                h.transcript.home_env
                            ));
                        }
                    }
                    Some(Home::Inherit) => {}
                }
            }
        }
        Ok(h)
    }

    /// The argv to spawn, every placeholder filled. `sandbox` is the
    /// profile's row, already chosen by [`Harness::sandbox_for`].
    pub(crate) fn argv_for(
        &self,
        model: &str,
        effort: &str,
        sandbox: &str,
        last_message: &Path,
    ) -> Vec<String> {
        let last = last_message.display().to_string();
        self.argv
            .iter()
            .map(|w| {
                w.replace("{model}", model)
                    .replace("{effort}", effort)
                    .replace("{sandbox}", sandbox)
                    .replace("{last_message}", &last)
            })
            .collect()
    }

    /// The sandbox this harness gives `profile`, or the refusal: a
    /// profile with no row is one this harness is not set up to run.
    pub(crate) fn sandbox_for(&self, profile: &str) -> std::result::Result<&str, String> {
        self.sandbox
            .get(profile)
            .map(String::as_str)
            .ok_or_else(|| {
                format!(
                    "harness {} declares no sandbox for profile `{profile}` ({DIR}/{}.toml \
                 [sandbox] holds: {}) — it is not set up to run that lane",
                    self.id,
                    self.id,
                    self.sandbox
                        .keys()
                        .map(String::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
    }

    /// [`Harness::home`]'s kind; a host-tool harness, which launches
    /// nothing, reads as inheriting.
    pub(crate) fn home_kind(&self) -> Home {
        self.home.unwrap_or(Home::Inherit)
    }

    /// The WHOLE environment of the launched process — the caller clears
    /// the environment first: the pass list's names that `inherited`
    /// holds; for `home = "own"`, HOME = `own_home` and the harness's
    /// home variable = its real home as the launcher resolves it; then
    /// the harness's own actor and the run.
    ///
    /// WHAT THAT IS AND IS NOT (review f7f0b689, B4; corrected by review
    /// e9514316, N12). A variable of the launching session reaches the
    /// process by exactly two channels — the pass list, and for a home
    /// of its own the file's `transcript.home_env` — and
    /// [`Harness::parse`] holds BOTH to [`forbidden_env`]: no identity,
    /// credential, other harness's session, agent socket or code loader
    /// that it knows by name — a list known to be incomplete. That is all
    /// it is. The
    /// process runs as the same account: it can read the account's
    /// files by their paths and set `BOSS_ACTOR` to anything, and
    /// nothing downstream authenticates that header.
    pub(crate) fn environment(
        &self,
        inherited: impl Fn(&str) -> Option<OsString>,
        run_id: &str,
        own_home: &Path,
    ) -> Vec<(String, OsString)> {
        let home = match self.home_kind() {
            Home::Own => std::iter::once(("HOME".to_string(), own_home.as_os_str().to_owned()))
                .chain(
                    self.home(&inherited)
                        .map(|real| (self.transcript.home_env.clone(), real.into_os_string())),
                )
                .collect::<Vec<_>>(),
            Home::Inherit => Vec::new(),
        };
        self.env
            .iter()
            .filter_map(|name| inherited(name).map(|v| (name.clone(), v)))
            .chain(home)
            .chain([
                (
                    crate::identity::ACTOR_ENV.to_string(),
                    OsString::from(&self.actor),
                ),
                (
                    crate::gate::AGENT_RUN_ENV.to_string(),
                    OsString::from(run_id),
                ),
            ])
            .collect()
    }

    /// Where this harness keeps its files: its `home_env` when set, else
    /// `home_default` under `$HOME`.
    pub(crate) fn home(&self, env: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
        let set = |name: &str| env(name).filter(|v| !v.is_empty());
        set(&self.transcript.home_env)
            .map(PathBuf::from)
            .or_else(|| set("HOME").map(|h| PathBuf::from(h).join(&self.transcript.home_default)))
    }

    /// The transcript of runtime `id` under `home`, when exactly one
    /// file matches. `Err` says how many did.
    pub(crate) fn transcript_of(
        &self,
        home: &Path,
        runtime: &str,
    ) -> std::result::Result<PathBuf, String> {
        // The id goes into a path pattern: only a plain token may.
        if runtime.is_empty()
            || !runtime
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(format!("runtime id {runtime:?} is not a plain token"));
        }
        let pattern = self
            .transcript
            .runtime_glob
            .as_deref()
            .ok_or("this harness declares no transcript.runtime_glob")?
            .replace("{runtime}", runtime);
        let mut found = glob_files(home, &pattern);
        match found.len() {
            1 => Ok(found.remove(0)),
            // The home's PATH is left out: it is the value of a variable,
            // and this sentence is recorded (review e9514316, N12).
            n => Err(format!("{n} files match {pattern}")),
        }
    }
}

/// `*` within one segment; everything else is literal.
fn segment_matches(pattern: &str, name: &str) -> bool {
    let mut parts = pattern.split('*');
    let first = parts.next().unwrap_or("");
    let Some(mut rest) = name.strip_prefix(first) else {
        return false;
    };
    let parts: Vec<&str> = parts.collect();
    if parts.is_empty() {
        return rest.is_empty();
    }
    for (i, part) in parts.iter().enumerate() {
        if i + 1 == parts.len() {
            return rest.ends_with(part);
        }
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    true
}

/// Every FILE under `root` whose path relative to it matches `pattern`,
/// sorted. A directory that cannot be read holds nothing.
pub(crate) fn glob_files(root: &Path, pattern: &str) -> Vec<PathBuf> {
    let segments: Vec<&str> = pattern.split('/').filter(|s| !s.is_empty()).collect();
    let mut level = vec![root.to_path_buf()];
    for (depth, segment) in segments.iter().enumerate() {
        let last = depth + 1 == segments.len();
        level = level
            .iter()
            .flat_map(|dir| {
                std::fs::read_dir(dir)
                    .map(|rd| rd.filter_map(|e| e.ok()).collect::<Vec<_>>())
                    .unwrap_or_default()
            })
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .is_some_and(|n| segment_matches(segment, n))
            })
            .map(|e| e.path())
            .filter(|p| if last { p.is_file() } else { p.is_dir() })
            .collect();
    }
    level.sort();
    level
}

/// Every harness the tree declares, sorted by id. A file that does not
/// parse refuses the whole read, naming it: a roster with a hole would
/// answer "no harness runs that model" for a model one does.
pub(crate) fn read_all(repo: &Path) -> Result<Vec<Harness>> {
    read_all_in(&repo.join(DIR))
}

/// [`read_all`] over the roster directory itself.
fn read_all_in(dir: &Path) -> Result<Vec<Harness>> {
    // A SYMLINKED ROSTER IS REFUSED (review e9514316, N15). The dock
    // holds a change by its PATH under [`DIR`]; a file changed behind a
    // directory that has become a link is under no path it names, so the
    // read that would launch from it does not follow one.
    if std::fs::symlink_metadata(dir).is_ok_and(|m| m.file_type().is_symlink()) {
        bail!(
            "the harness roster {} is a symbolic link — the roster is read where it is held \
             for review, and a link is somewhere else",
            dir.display()
        );
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("reading the harness roster {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .collect();
    files.sort();
    let mut all = Vec::new();
    for file in files {
        let stem = file
            .file_stem()
            .and_then(|s| s.to_str())
            .with_context(|| format!("{} has no UTF-8 name", file.display()))?;
        let text = std::fs::read_to_string(&file)
            .with_context(|| format!("reading {}", file.display()))?;
        let h =
            Harness::parse(stem, &text).map_err(|e| anyhow::anyhow!("{DIR}/{stem}.toml: {e}"))?;
        all.push(h);
    }
    if all.is_empty() {
        bail!("{DIR} declares no harness");
    }
    // One harness per actor: the actor is how a model reaches its file.
    for (i, h) in all.iter().enumerate() {
        if let Some(twin) = all[..i].iter().find(|o| o.actor == h.actor) {
            bail!(
                "{DIR}: {} and {} both sign as {} — an actor names one harness",
                twin.id,
                h.id,
                h.actor
            );
        }
    }
    Ok(all)
}

/// [`read_all`] from the first tree that holds [`DIR`]: `repo` when the
/// verb stands in a checkout, else the two places the trust verbs also
/// fall back to (backlog a693cf9d) — a report is often run from a
/// scratch directory that is no worktree.
pub(crate) fn read_for_the_verb(repo: Option<&Path>) -> Result<Vec<Harness>> {
    let named = std::env::var_os(crate::trust_boundary::TREE_ENV)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    let built = PathBuf::from(crate::trust_boundary::BUILT_FROM);
    let tree = repo
        .map(Path::to_path_buf)
        .into_iter()
        .chain(named)
        .chain([built])
        .find(|t| t.join(DIR).is_dir())
        .with_context(|| {
            format!(
                "no {DIR} to read the harnesses from: not in the checkout the verb stands in, \
                 not under ${}, not in the checkout this binary was built from",
                crate::trust_boundary::TREE_ENV
            )
        })?;
    read_all(&tree)
}

/// The harness that runs `model`, and the registered id it signs as.
///
/// Two reads, both data: the agents registry rows (`GET /api/agents`)
/// whose `default_model` is `model` name the actor; the harness whose
/// `actor` is that id is the one. Exactly one of each, or the sentence
/// saying what was found instead.
pub(crate) fn for_model<'a>(
    harnesses: &'a [Harness],
    agents: &[Value],
    model: &str,
) -> std::result::Result<&'a Harness, String> {
    let ids: Vec<&str> = agents
        .iter()
        .filter(|a| a.get("default_model").and_then(Value::as_str) == Some(model))
        .filter_map(|a| a.get("id").and_then(Value::as_str))
        .collect();
    let id = match ids.as_slice() {
        [one] => *one,
        [] => {
            return Err(format!(
                "no registered agent runs `{model}`: no row of the agents registry has it as \
                 its default_model, so no harness is named for it"
            ));
        }
        many => {
            return Err(format!(
                "{} registered agents run `{model}` ({}) — refusing to choose which signs the run",
                many.len(),
                many.join(", ")
            ));
        }
    };
    harnesses.iter().find(|h| h.actor == id).ok_or_else(|| {
        format!(
            "`{model}` is run by {id}, and no file under {DIR} declares `actor = \"{id}\"` — \
             there is no harness to start it with"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn repo() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .canonicalize()
            .unwrap()
    }

    fn shipped() -> Vec<Harness> {
        read_all(&repo()).expect("every shipped harness file parses")
    }

    #[test]
    fn a_cluster_backend_is_registry_selected_without_changing_the_shipped_default() {
        let source = std::fs::read_to_string(
            boss_testing::repo_root().join("infra/platform/harnesses/codex-exec.toml"),
        )
        .unwrap();
        assert_eq!(
            Harness::parse("codex-exec", &source).unwrap().launch,
            Launch::Process
        );
        let selected = source.replace("launch = \"process\"", "launch = \"cluster\"");
        assert!(Harness::parse("codex-exec", &selected).is_ok());
    }

    /// The registry rows the shipped Workflow blocks need, shaped as
    /// `boss-api GET /api/agents` answers. Read live 2026-10-07; then
    /// agent-claude's `default_model` moved from `opus-5[1m]` to
    /// `opus-5-5` with the blocks (backlog ad82090a, R2). A test cannot
    /// read the live row, so this is a stated copy: the live row is a
    /// person's write, and the dispatch path reads THAT one.
    fn registry() -> Vec<Value> {
        vec![
            json!({"id":"agent-claude","default_model":"opus-5-5","aliases":["claude@algedonic.dev"],"role":"platform-admin"}),
            json!({"id":"agent-codex","default_model":"gpt-6.1-sol","aliases":["codex@algedonic.dev"],"role":null}),
        ]
    }

    #[test]
    fn the_registry_row_that_runs_a_model_names_its_harness() {
        let all = shipped();
        let codex = for_model(&all, &registry(), "gpt-6.1-sol").unwrap();
        assert_eq!(codex.actor, "agent-codex");
        assert_eq!(codex.launch, Launch::Process);
        let claude = for_model(&all, &registry(), "opus-5-5").unwrap();
        assert_eq!(claude.actor, "agent-claude");
        assert_eq!(claude.launch, Launch::HostTool);
    }

    #[test]
    fn a_model_no_row_or_two_rows_or_no_file_names_is_refused_by_what_was_found() {
        let all = shipped();
        let none = for_model(&all, &registry(), "gemini-2.5-pro").unwrap_err();
        assert!(
            none.contains("no registered agent runs `gemini-2.5-pro`"),
            "{none}"
        );
        let mut two = registry();
        two.push(json!({"id":"agent-other","default_model":"gpt-6.1-sol"}));
        let both = for_model(&all, &two, "gpt-6.1-sol").unwrap_err();
        assert!(both.contains("agent-codex, agent-other"), "{both}");
        let orphan = vec![json!({"id":"agent-gemini","default_model":"gemini-2.5-pro"})];
        let no_file = for_model(&all, &orphan, "gemini-2.5-pro").unwrap_err();
        assert!(no_file.contains("actor = \"agent-gemini\""), "{no_file}");
    }

    /// THE IDENTITY RULE, on the shipped files: the launched process is
    /// handed its harness's own actor and the run, and nothing of the
    /// session that launched it — whatever that session's environment
    /// holds.
    #[test]
    fn a_launched_process_inherits_no_identity_but_its_harnesss_own() {
        let coordinator = |name: &str| -> Option<OsString> {
            match name {
                "BOSS_ACTOR" => Some("claude@algedonic.dev".into()),
                "BOSS_ACTOR_FILE" => Some("/somewhere/actor".into()),
                "BOSS_AGENT_RUN" => Some("the-coordinators-own-run".into()),
                "CLAUDE_CODE_SESSION_ID"
                | "CLAUDECODE"
                | "CLAUDE_CONFIG_DIR"
                | "ANTHROPIC_API_KEY"
                | "OPENAI_API_KEY" => Some("coordinator".into()),
                "HOME" => Some("/work/home".into()),
                "PATH" => Some("/bin".into()),
                _ => None,
            }
        };
        for h in shipped().iter().filter(|h| h.launch == Launch::Process) {
            let env = h.environment(coordinator, "run-1", Path::new("/launch/home"));
            let get = |k: &str| {
                env.iter()
                    .filter(|(n, _)| n == k)
                    .map(|(_, v)| v.to_str().unwrap())
                    .collect::<Vec<_>>()
            };
            assert_eq!(get("BOSS_ACTOR"), vec![h.actor.as_str()], "{}", h.id);
            assert_eq!(get("BOSS_AGENT_RUN"), vec!["run-1"], "{}", h.id);
            // A home of its own; the harness's login found by its own
            // variable, resolved from the launcher's HOME.
            assert_eq!(h.home, Some(Home::Own), "{}", h.id);
            assert_eq!(get("HOME"), vec!["/launch/home"]);
            assert_eq!(
                get(&h.transcript.home_env),
                vec![format!("/work/home/{}", h.transcript.home_default)]
            );
            for (name, value) in &env {
                assert!(
                    !name.starts_with("CLAUDE") && !name.starts_with("ANTHROPIC"),
                    "{}: passes {name}",
                    h.id
                );
                assert_ne!(name, "BOSS_ACTOR_FILE");
                assert_ne!(
                    value, "coordinator",
                    "{}: {name} is the coordinator's",
                    h.id
                );
            }
        }
    }

    #[test]
    fn a_file_that_would_pass_an_actor_or_a_run_is_refused_at_load() {
        let text = std::fs::read_to_string(repo().join(DIR).join("codex-exec.toml")).unwrap();
        for name in IDENTITY_ENV {
            let planted = text.replace("\"PATH\",", &format!("\"PATH\", \"{name}\","));
            assert_ne!(planted, text, "the plant landed");
            let refused = Harness::parse("codex-exec", &planted).unwrap_err();
            assert!(refused.contains(name), "{refused}");
        }
    }

    /// B1 (review f7f0b689): each of these was accepted by the parser.
    #[test]
    fn a_pass_list_naming_a_credential_or_another_harnesss_session_is_refused() {
        let text = std::fs::read_to_string(repo().join(DIR).join("codex-exec.toml")).unwrap();
        for name in [
            "ANTHROPIC_API_KEY",
            "CLAUDE_CODE_MESSAGING_TOKEN",
            "claude_config_dir",
            "BOSS_MACHINE_TOKEN_DIR",
            "FORGE_TOKEN",
            "OPENAI_API_KEY",
            "boss_actor",
            "BOSS_ACTOR_OVERRIDE",
            "DB_PASSWORD",
            "AWS_SECRET",
            "A B",
        ] {
            let planted = text.replace("\"PATH\",", &format!("\"PATH\", \"{name}\","));
            assert_ne!(planted, text, "the plant landed");
            let refused =
                Harness::parse("codex-exec", &planted).expect_err(&format!("{name} is refused"));
            assert!(refused.contains(name), "{refused}");
        }
        for name in [
            "PATH",
            "LANG",
            "TMPDIR",
            "SSL_CERT_FILE",
            "BOSS_JOBS_URL",
            "BOSS_TREE",
        ] {
            assert_eq!(forbidden_env(name), None, "{name}");
        }
        // Judged case-blind, and a lower-case name is still a name.
        assert_eq!(forbidden_env("http_proxy"), None);
        assert_eq!(
            forbidden_env("claude_config_dir"),
            Some("belongs to another harness's session")
        );
        assert_eq!(forbidden_env("Forge_Token"), Some("reads as a credential"));
    }

    /// N12 (review e9514316): `transcript.home_env` is a second way a
    /// variable reaches the process — the launcher passes its value on
    /// for a home of its own — so it meets the same floor as `env`.
    #[test]
    fn the_home_variable_meets_the_same_floor_as_the_pass_list() {
        let text = std::fs::read_to_string(repo().join(DIR).join("codex-exec.toml")).unwrap();
        for name in [
            "ANTHROPIC_API_KEY",
            "BOSS_MACHINE_TOKEN_DIR",
            "LD_PRELOAD",
            "BOSS_ACTOR",
        ] {
            let planted = text.replace(
                "home_env = \"CODEX_HOME\"",
                &format!("home_env = \"{name}\""),
            );
            assert_ne!(planted, text);
            let refused = Harness::parse("codex-exec", &planted).expect_err(name);
            assert!(
                refused.contains("home_env") && refused.contains(name),
                "{refused}"
            );
        }
    }

    /// N13: the names that hand over what the process could NOT get by
    /// reading the account's files — an agent socket, or code the file's
    /// argv never names.
    #[test]
    fn a_pass_list_naming_an_agent_socket_or_a_code_loader_is_refused() {
        for name in [
            "SSH_AUTH_SOCK",
            "LD_PRELOAD",
            "LD_LIBRARY_PATH",
            "BASH_ENV",
            "GIT_ASKPASS",
            "GIT_SSH_COMMAND",
            "GIT_CONFIG_GLOBAL",
            "NODE_OPTIONS",
            "ld_preload",
        ] {
            assert!(forbidden_env(name).is_some(), "{name}");
        }
        // Not a safe-list: these pass, and the hold is what reads them.
        for name in ["DATABASE_URL", "KUBECONFIG", "NETRC", "GIT_AUTHOR_NAME"] {
            assert_eq!(forbidden_env(name), None, "{name}");
        }
    }

    /// The rest of the floor (review b149c579): every name below reached
    /// a launched process through a pass list the parser accepted on
    /// origin/main 7124c4c2. Each makes a program the file's argv never
    /// names load or run code — a module path (PYTHONPATH, PERL5OPT,
    /// RUBYOPT, NODE_PATH), a program git or a shell runs by itself
    /// (GIT_EXEC_PATH, GIT_PROXY_COMMAND, GIT_EXTERNAL_DIFF, EDITOR,
    /// PAGER, PROMPT_COMMAND), or a configuration home whose files name
    /// such programs (XDG_CONFIG_HOME holds git's own config: core.pager,
    /// core.hooksPath, core.fsmonitor). The first eleven are the review's
    /// list; the rest are the KNOWN members of the same families. That is
    /// not every member — the next review found more (review b40dbff7,
    /// the test below) — and this test does not claim it: the floor is
    /// deliberately incomplete, and the control on a pass list is the
    /// dock hold every change under [`DIR`] gets, not this list.
    #[test]
    fn a_pass_list_naming_a_module_path_a_program_tools_run_or_a_config_home_is_refused() {
        let text = std::fs::read_to_string(repo().join(DIR).join("codex-exec.toml")).unwrap();
        for name in [
            "XDG_CONFIG_HOME",
            "PYTHONPATH",
            "PERL5OPT",
            "RUBYOPT",
            "NODE_PATH",
            "GIT_EXEC_PATH",
            "GIT_PROXY_COMMAND",
            "GIT_EXTERNAL_DIFF",
            "EDITOR",
            "PAGER",
            "PROMPT_COMMAND",
            // The families.
            "XDG_CONFIG_DIRS",
            "PYTHONHOME",
            "PYTHONUSERBASE",
            "PERL5LIB",
            "PERLLIB",
            "RUBYLIB",
            "GIT_EDITOR",
            "GIT_SEQUENCE_EDITOR",
            "GIT_PAGER",
            "GIT_TEMPLATE_DIR",
            "GIT_CONFIG_PARAMETERS",
            "GIT_CONFIG_COUNT",
            "GIT_CONFIG_KEY_0",
            "VISUAL",
            "MANPAGER",
            "LESSOPEN",
            "LESSCLOSE",
            "ZDOTDIR",
            "JAVA_TOOL_OPTIONS",
            "_JAVA_OPTIONS",
            "DYLD_INSERT_LIBRARIES",
            "xdg_config_home",
        ] {
            assert_eq!(
                forbidden_env(name),
                Some("makes the process load or run code its argv never names"),
                "{name}"
            );
            let planted = text.replace("\"PATH\",", &format!("\"PATH\", \"{name}\","));
            assert_ne!(planted, text, "the plant landed");
            let refused =
                Harness::parse("codex-exec", &planted).expect_err(&format!("{name} is refused"));
            assert!(refused.contains(name), "{refused}");
        }
        // Still a floor and not a safe-list, and the shipped pass list
        // is untouched by it: its names, and the plain ones beside them,
        // pass.
        for name in [
            "PATH",
            "LANG",
            "LC_ALL",
            "TERM",
            "TMPDIR",
            "SSL_CERT_FILE",
            "XDG_RUNTIME_DIR",
            "GIT_AUTHOR_NAME",
        ] {
            assert_eq!(forbidden_env(name), None, "{name}");
        }
    }

    /// Review b40dbff7 of car d47c0edd: the floor above still passed
    /// these, and the reviewer ran attacker-named code through five of
    /// them on the dev pod under `env -i` — GIT_DIR (a repository whose
    /// config sets core.fsmonitor), SSH_SK_PROVIDER (a dlopened library's
    /// constructor), SSH_SK_HELPER (exec'd by ssh), RUSTC_WRAPPER (exec'd
    /// by cargo) and OPENSSL_CONF (a provider module dlopened at library
    /// init under python3's ssl). The rest name a program or a module the
    /// same way. git is closed as a PREFIX — GIT_DIR, GIT_COMMON_DIR and
    /// GIT_WORK_TREE choose whose config git reads, and git(1) documents
    /// many more GIT_ variables — with only GIT_AUTHOR_* and GIT_COMMITTER_*
    /// left passable, which are identity text a commit records and run
    /// nothing. Like the test above this lists the members KNOWN today and
    /// claims no more: the floor is deliberately incomplete and the dock
    /// hold is the control.
    #[test]
    fn a_pass_list_naming_a_repository_a_helper_a_wrapper_or_a_library_config_is_refused() {
        let text = std::fs::read_to_string(repo().join(DIR).join("codex-exec.toml")).unwrap();
        for name in [
            // The review's list, each by name.
            "GIT_DIR",
            "GIT_COMMON_DIR",
            "GIT_WORK_TREE",
            "SSH_SK_PROVIDER",
            "SSH_SK_HELPER",
            "SUDO_ASKPASS",
            "RUSTC_WRAPPER",
            "RUSTC_WORKSPACE_WRAPPER",
            "RUSTFLAGS",
            "CC",
            "CXX",
            "OPENSSL_CONF",
            "OPENSSL_MODULES",
            "GCONV_PATH",
            "PYTHONWARNINGS",
            "PYTHONBREAKPOINT",
            "SHELLOPTS",
            "PS4",
            // git by prefix, not by name: two more it reads.
            "GIT_INDEX_FILE",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "git_dir",
        ] {
            assert_eq!(
                forbidden_env(name),
                Some("makes the process load or run code its argv never names"),
                "{name}"
            );
            let planted = text.replace("\"PATH\",", &format!("\"PATH\", \"{name}\","));
            assert_ne!(planted, text, "the plant landed");
            let refused =
                Harness::parse("codex-exec", &planted).expect_err(&format!("{name} is refused"));
            assert!(refused.contains(name), "{refused}");
        }
        // The prefix keeps git's identity text passable.
        for name in [
            "GIT_AUTHOR_NAME",
            "GIT_AUTHOR_EMAIL",
            "GIT_AUTHOR_DATE",
            "GIT_COMMITTER_NAME",
            "GIT_COMMITTER_EMAIL",
            "GIT_COMMITTER_DATE",
        ] {
            assert_eq!(forbidden_env(name), None, "{name}");
        }
    }

    /// Re-review 702da60d of car d47c0edd (backlog 015ed327): the floor
    /// above still passed CARGO_BUILD_RUSTC_WRAPPER and CARGO_HOME (whose
    /// config.toml sets build.rustc-wrapper), and the reviewer measured
    /// each running a named wrapper, 4 runs each against a control of 0.
    /// cargo reads EVERY `[build]`, `[target]` and `[env]` config key from
    /// a CARGO_<KEY> variable as well as from a file, and rustup chooses
    /// the toolchain whose binaries run from RUSTUP_TOOLCHAIN (a path is
    /// accepted) and RUSTUP_HOME — so both are closed as a PREFIX, the
    /// same lesson git's prefix taught (b40dbff7), with RUSTC and RUSTDOC
    /// (the compilers cargo runs) and RUSTDOCFLAGS (RUSTFLAGS' twin) by
    /// name. The rest point a tool at a configuration file that names a
    /// program to run or a module to load: AWS_CONFIG_FILE
    /// (credential_process), DOCKER_CONFIG (credsStore helpers), GNUPGHOME
    /// (gpg.conf and the agent programs it names), PIP_CONFIG_FILE and
    /// every PIP_<option> beside it, NPM_CONFIG_USERCONFIG and every
    /// npm_config_<key> beside it (script-shell names a program), GEM_HOME
    /// and GEM_PATH (where rubygems loads from), and Lua's whole variable
    /// family — LUA_INIT runs a chunk at start-up and LUA_PATH / LUA_CPATH
    /// are module paths, each with versioned twins. KUBECONFIG stays
    /// passable on purpose, as it does above; the dock hold reads it.
    #[test]
    fn a_pass_list_naming_a_cargo_or_rustup_setting_or_a_program_naming_config_is_refused() {
        let text = std::fs::read_to_string(repo().join(DIR).join("codex-exec.toml")).unwrap();
        for name in [
            // The two the re-review measured.
            "CARGO_BUILD_RUSTC_WRAPPER",
            "CARGO_HOME",
            // cargo and rustup by prefix, not by name.
            "CARGO_BUILD_RUSTC",
            "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER",
            "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER",
            "CARGO_TARGET_DIR",
            "RUSTUP_TOOLCHAIN",
            "RUSTUP_HOME",
            "cargo_home",
            // The compilers cargo runs, by name.
            "RUSTC",
            "RUSTDOC",
            "RUSTDOCFLAGS",
            // Config files that name a program or a module.
            "AWS_CONFIG_FILE",
            "DOCKER_CONFIG",
            "GNUPGHOME",
            "PIP_CONFIG_FILE",
            "PIP_INDEX_URL",
            "NPM_CONFIG_USERCONFIG",
            "NPM_CONFIG_SCRIPT_SHELL",
            "npm_config_globalconfig",
            "LUA_INIT",
            "LUA_INIT_5_4",
            "LUA_PATH",
            "LUA_CPATH",
            "GEM_HOME",
            "GEM_PATH",
        ] {
            assert_eq!(
                forbidden_env(name),
                Some("makes the process load or run code its argv never names"),
                "{name}"
            );
            let planted = text.replace("\"PATH\",", &format!("\"PATH\", \"{name}\","));
            assert_ne!(planted, text, "the plant landed");
            let refused =
                Harness::parse("codex-exec", &planted).expect_err(&format!("{name} is refused"));
            assert!(refused.contains(name), "{refused}");
        }
        // The prefixes are cargo's, rustup's, npm's config and pip's, not
        // every name that starts with the same letters: these still pass.
        for name in [
            "KUBECONFIG",
            "RUST_LOG",
            "RUST_BACKTRACE",
            "PIPENV_VENV_IN_PROJECT",
            "NPM_PREFIX_DIR",
            "AWS_REGION",
            "DOCKER_HOST",
            "PATH",
        ] {
            assert_eq!(forbidden_env(name), None, "{name}");
        }
    }

    /// Review dfd43fbd of car cc571bcf (backlog 91a545eb, 2026-10-18):
    /// the CARGO_ prefix needs its underscore, so a bare CARGO passed —
    /// and cargo-clippy and cargo-fmt run whatever it names as cargo (4
    /// of 4 runs each on the dev pod, 0 with it unset). Its siblings by
    /// the same measure, read off the 1.96.1 toolchain's own binaries on
    /// 2026-10-19: cargo-fmt runs RUSTFMT as rustfmt, and cargo runs
    /// BROWSER for `cargo doc --open`. Each is a bare program name a
    /// prefix test cannot see, so each is refused by name.
    #[test]
    fn a_pass_list_naming_bare_cargo_or_a_program_cargo_tools_run_is_refused() {
        let text = std::fs::read_to_string(repo().join(DIR).join("codex-exec.toml")).unwrap();
        for name in ["CARGO", "cargo", "RUSTFMT", "BROWSER"] {
            assert_eq!(
                forbidden_env(name),
                Some("makes the process load or run code its argv never names"),
                "{name}"
            );
            let planted = text.replace("\"PATH\",", &format!("\"PATH\", \"{name}\","));
            assert_ne!(planted, text, "the plant landed");
            let refused =
                Harness::parse("codex-exec", &planted).expect_err(&format!("{name} is refused"));
            assert!(refused.contains(name), "{refused}");
        }
        // Names that only START with these letters still pass.
        for name in ["CARGOES", "RUSTFMT_LOG", "BROWSERS"] {
            assert_eq!(forbidden_env(name), None, "{name}");
        }
    }

    /// N15: a roster directory that is a symlink is refused at the read —
    /// a file changed behind it would be under no path the hold names.
    /// Two Claude accounts run on the dev pod (David, 2026-10-17: session
    /// A coordinates as agent-claude, session B builds as agent-claude-b),
    /// and `--report` finds a run's transcript only through the file that
    /// signs as the run's agent. Until this pin every agent-claude-b run
    /// landed unmetered ("no file under infra/platform/harnesses signs as
    /// agent-claude-b") unless the operator passed `--transcript` by hand,
    /// measured on runs 93784166 and d0651edc the night the account went
    /// live. Both accounts are the same host tool reading the same
    /// transcript layout under their own `CLAUDE_CONFIG_DIR`; only the
    /// home differs.
    #[test]
    fn each_claude_account_has_a_host_tool_harness_reading_the_same_transcript_layout() {
        let all = shipped();
        let a = all
            .iter()
            .find(|h| h.actor == "agent-claude")
            .expect("agent-claude's harness");
        let b = all
            .iter()
            .find(|h| h.actor == "agent-claude-b")
            .expect("a file under infra/platform/harnesses signs as agent-claude-b");
        assert_eq!(b.launch, Launch::HostTool);
        assert_eq!(b.transcript.adapter, a.transcript.adapter);
        assert_eq!(b.transcript.home_env, a.transcript.home_env);
        assert_eq!(b.transcript.glob, a.transcript.glob);
        assert_ne!(
            b.transcript.home_default, a.transcript.home_default,
            "two accounts, two homes"
        );
    }

    #[test]
    fn a_roster_directory_that_is_a_symlink_is_refused() {
        let tree = boss_testing::scratch_dir("harness-symlinked-roster");
        let real = tree.join("elsewhere");
        std::fs::create_dir_all(&real).unwrap();
        for name in ["claude-code.toml", "codex-exec.toml"] {
            std::fs::copy(repo().join(DIR).join(name), real.join(name)).unwrap();
        }
        assert_eq!(
            read_all_in(&real).unwrap().len(),
            2,
            "the files themselves are good"
        );
        std::fs::create_dir_all(tree.join("infra/platform")).unwrap();
        std::os::unix::fs::symlink(&real, tree.join(DIR)).unwrap();
        let refused = read_all(&tree).unwrap_err().to_string();
        assert!(refused.contains("symbolic link"), "{refused}");
    }

    /// B4: whose HOME the process gets is written down, and a home of
    /// its own cannot also be handed the account's.
    #[test]
    fn a_process_harness_says_whose_home_it_gets() {
        let text = std::fs::read_to_string(repo().join(DIR).join("codex-exec.toml")).unwrap();
        let unsaid = text.replace("home = \"own\"\n", "");
        assert_ne!(unsaid, text);
        assert!(
            Harness::parse("codex-exec", &unsaid)
                .unwrap_err()
                .contains("home = \"own\" or home = \"inherit\"")
        );
        for name in ["HOME", "CODEX_HOME"] {
            let both = text.replace("\"PATH\",", &format!("\"PATH\", \"{name}\","));
            let refused = Harness::parse("codex-exec", &both).unwrap_err();
            assert!(refused.contains("home = \"own\""), "{name}: {refused}");
        }
        let inherit = text
            .replace("home = \"own\"", "home = \"inherit\"")
            .replace("\"PATH\",", "\"PATH\", \"HOME\",");
        let h = Harness::parse("codex-exec", &inherit).expect("inherit is sayable");
        let env = h.environment(
            |n| (n == "HOME").then(|| "/work/home".into()),
            "r",
            Path::new("/launch/home"),
        );
        assert!(env.contains(&("HOME".to_string(), "/work/home".into())));
        assert!(!env.iter().any(|(n, _)| n == "CODEX_HOME"));
    }

    #[test]
    fn a_file_is_held_to_its_name_its_actor_shape_and_a_readable_transcript() {
        let text = std::fs::read_to_string(repo().join(DIR).join("codex-exec.toml")).unwrap();
        assert!(Harness::parse("codex-exec", &text).is_ok());
        assert!(Harness::parse("another-name", &text).is_err());
        let login = text.replace("actor = \"agent-codex\"", "actor = \"codex@algedonic.dev\"");
        assert!(
            Harness::parse("codex-exec", &login)
                .unwrap_err()
                .contains("agent-<slug>")
        );
        let adapter = text.replace("codex-rollout", "gemini-stats");
        assert!(
            Harness::parse("codex-exec", &adapter)
                .unwrap_err()
                .contains("adapter")
        );
        let no_model = text.replace("{model}", "gpt-fixed");
        assert!(
            Harness::parse("codex-exec", &no_model)
                .unwrap_err()
                .contains("{model}")
        );
        let unknown = text.replace("{sandbox}", "{worktree}");
        assert!(
            Harness::parse("codex-exec", &unknown)
                .unwrap_err()
                .contains("{worktree}")
        );
        let extra = format!("{text}\nsurprise = 1\n");
        assert!(
            Harness::parse("codex-exec", &extra).is_err(),
            "unknown keys are refused"
        );
    }

    /// The argv, as `codex exec --help` (codex-cli 0.159.3) spells it.
    ///
    /// THE SANDBOX WORD IS `danger-full-access` FOR BOTH READING LANES
    /// (design 99a71246, decided by David 2026-10-08). `read-only` was
    /// what this test held until the first live launch, run cff223ac
    /// (2026-10-08 00:50Z, on backlog 2f7b8c00 as
    /// `codex_live_run_1_20261008`): Codex wraps every command in bwrap,
    /// the dev pod refuses unprivileged user namespaces, and the review
    /// came back having read nothing. So no sandbox holds a Codex
    /// analyst or reviewer here — it is read-only because its rules say
    /// so, as a Claude reviewer subagent is — and the test says the word
    /// that is true of the file rather than the one that reads safer.
    #[test]
    fn the_codex_argv_fills_every_placeholder_and_reads_the_prompt_from_stdin() {
        let all = shipped();
        let codex = for_model(&all, &registry(), "gpt-6.1-sol").unwrap();
        assert_eq!(
            codex.sandbox_for("analyst").unwrap(),
            "danger-full-access",
            "the analyst lane starts under the same word as the reviewer's"
        );
        let sandbox = codex.sandbox_for("reviewer").unwrap();
        let argv = codex.argv_for("gpt-6.1-sol", "high", sandbox, Path::new("/d/last.txt"));
        assert_eq!(
            argv,
            [
                "codex",
                "exec",
                "--json",
                "--ignore-user-config",
                "--model",
                "gpt-6.1-sol",
                "-c",
                "model_reasoning_effort=\"high\"",
                "--sandbox",
                "danger-full-access",
                "--output-last-message",
                "/d/last.txt",
                "-",
            ]
        );
        assert!(argv.iter().all(|w| !w.contains('{')));
        let builder = codex.sandbox_for("builder").unwrap_err();
        assert!(
            builder.contains("no sandbox for profile `builder`"),
            "{builder}"
        );
    }

    #[test]
    fn a_transcript_is_found_by_its_runtime_id_and_only_when_exactly_one_matches() {
        let home = boss_testing::scratch_dir("harness-transcript");
        let day = home.join("sessions/2026/10/07");
        std::fs::create_dir_all(&day).unwrap();
        let mine = day.join("rollout-2026-10-07T10-00-00-01a1-thread.jsonl");
        std::fs::write(&mine, "{}").unwrap();
        std::fs::write(day.join("rollout-2026-10-07T10-00-01-other.jsonl"), "{}").unwrap();
        let all = shipped();
        let codex = for_model(&all, &registry(), "gpt-6.1-sol").unwrap();
        assert_eq!(codex.transcript_of(&home, "01a1-thread").unwrap(), mine);
        assert!(
            codex
                .transcript_of(&home, "absent")
                .unwrap_err()
                .starts_with("0 files")
        );
        assert!(
            codex.transcript_of(&home, "../x").is_err(),
            "an id is a token, not a path"
        );
        assert_eq!(glob_files(&home, &codex.transcript.glob).len(), 2);
        assert_eq!(
            codex.home(|n| (n == "HOME").then(|| "/h".into())),
            Some(PathBuf::from("/h/.codex"))
        );
        assert_eq!(
            codex.home(|n| (n == "CODEX_HOME").then(|| "/elsewhere".into())),
            Some(PathBuf::from("/elsewhere"))
        );
    }

    #[test]
    fn a_star_matches_within_one_segment_only() {
        assert!(segment_matches("rollout-*.jsonl", "rollout-a-b.jsonl"));
        assert!(segment_matches("rollout-*-t1.jsonl", "rollout-x-t1.jsonl"));
        assert!(!segment_matches("rollout-*-t1.jsonl", "rollout-x-t2.jsonl"));
        assert!(segment_matches("*", "anything"));
        assert!(segment_matches("subagents", "subagents"));
        assert!(!segment_matches("subagents", "subagents2"));
        assert!(!segment_matches("agent-*.jsonl", "other-1.jsonl"));
    }

    /// What a Claude Code subagent's transcript says it ran, as the
    /// model attachment and every billed turn spell it. Measured
    /// 2026-10-09 on three subagent transcripts of the dispatching
    /// session (55, 80 and 367 billed turns, each `claude-opus-5-5`,
    /// identity `claude-opus-5-5` with no `[1m]` — a subagent does not
    /// run the 1M window), and on the newest 200 agent-runs: 115 of 115
    /// Claude rows read `observed: opus-5-5`. `.claude/agents/effort-*.md`
    /// say `model: opus`, an alias the harness resolves; when it moves
    /// to a newer Opus this measurement is re-taken and the blocks follow.
    const CLAUDE_SUBAGENT_RUNS: &str = "claude-opus-5-5";

    /// A WORKFLOW BLOCK DECLARES THE MODEL THAT RUNS (backlog ad82090a,
    /// R2). Every block said `opus-5[1m]` while every Claude run was
    /// observed as `opus-5-5`, so `--report` printed "they DIFFER" on 106
    /// of 108 observed runs (finish ledger, 2026-10-09) — a warning
    /// nobody reads, and a record naming a model that did not run.
    ///
    /// Three halves, for every distinct model a shipped block names: the
    /// rate card prices it; the dispatch path's registry read
    /// ([`for_model`]) names a harness for it; and when that harness is
    /// a Claude subagent, the observation `--report` makes of such a
    /// transcript ([`crate::transcript_usage::read_models`], the one
    /// spelling rule) equals the declaration, so [`model_line`] does not
    /// say DIFFER. Exact equality, no alias table (decided 2026-10-09,
    /// decision 2).
    ///
    /// [`model_line`]: crate::dispatch_launch::model_line
    #[test]
    fn every_workflow_block_names_a_priced_model_its_harness_is_observed_running() {
        let harnesses = shipped();
        let models: std::collections::BTreeSet<String> =
            boss_jobs::seed_loader::load_workflows(boss_jobs::registry::platform_bundle_path())
                .expect("the platform bundle parses")
                .into_iter()
                .flat_map(|w| w.steps.into_iter().filter_map(|s| s.agent.map(|a| a.model)))
                .collect();
        assert!(!models.is_empty(), "the bundle declares agent blocks");
        let transcript = [
            format!(
                r#"{{"type":"attachment","attachment":{{"type":"model","identity":{{"modelId":"{CLAUDE_SUBAGENT_RUNS}"}}}}}}"#
            ),
            format!(
                r#"{{"type":"assistant","message":{{"id":"msg_a","model":"{CLAUDE_SUBAGENT_RUNS}","usage":{{"input_tokens":1,"output_tokens":1}}}}}}"#
            ),
        ]
        .join("\n");
        let observed = crate::transcript_usage::read_models(&transcript).recorded();
        for model in &models {
            assert!(
                boss_jobs::agent_spec::known_models().contains(model),
                "a block names `{model}`, which the rate card does not price"
            );
            let harness = for_model(&harnesses, &registry(), model)
                .unwrap_or_else(|why| panic!("a block names `{model}`: {why}"));
            if harness.transcript.adapter == "claude-subagent" {
                let line = crate::dispatch_launch::model_line(model, observed.as_deref(), None);
                assert!(
                    !line.contains("DIFFER") && observed.as_deref() == Some(model.as_str()),
                    "a block declares `{model}` for {}, whose runs are observed as {observed:?}: {line}",
                    harness.id
                );
            }
        }
    }

    /// The toolchain binaries whose names [`forbidden_env`] is held to:
    /// the programs a harness that builds this tree runs (backlog
    /// 685ce87c).
    const TOOLCHAIN_BINARIES: [&str; 5] = [
        "cargo",
        "cargo-clippy",
        "cargo-fmt",
        "clippy-driver",
        "rustfmt",
    ];

    /// The toolchain's own namespaces. Inside a binary every string
    /// literal sits beside the next with no separator, so the end of a
    /// name cannot be read off the bytes — but its START can, wherever
    /// one of these stems begins a word.
    const TOOLCHAIN_STEMS: [&str; 9] = [
        "CARGO", "RUSTC", "RUSTDOC", "RUSTFMT", "RUSTUP", "RUST_", "CLIPPY", "__CARGO", "__CLIPPY",
    ];

    /// Every stem-led word the 1.96.1 binaries carry that the floor
    /// PASSES, and why. A word here is either a variable the toolchain
    /// reads that names no code, or not a variable at all — a symbol, a
    /// lint name, or literals that happen to abut. An entry no binary
    /// carries any longer fails the test, so this cannot grow stale.
    const TOOLCHAIN_PASSES: &[(&str, &str)] = &[
        (
            "RUST_LOG",
            "a log filter; read by cargo and rustfmt, names no program",
        ),
        ("RUST_BACKTRACE", "whether a panic prints a backtrace"),
        (
            "RUST_LIB_BACKTRACE",
            "whether an error captures a backtrace",
        ),
        ("RUST_MIN_STACK", "a thread's stack size in bytes"),
        (
            "RUST_FLAGS",
            "read only to warn it is ignored: cargo's text is `ignoring environment variable`",
        ),
        (
            "RUST_",
            "not a variable: the tail of OpenSSL's X509_TRUST_ symbols, linked into cargo",
        ),
        (
            "RUST_BACH",
            "not a variable: RUST_BACKTRACE's first eight bytes as an instruction immediate",
        ),
        (
            "RUSTFMT_H",
            "not a variable: RUSTFMT_LOG's first eight bytes as an instruction immediate",
        ),
    ];

    /// Every word in `bytes` that starts with a toolchain stem, read to
    /// the end of its run of `[A-Z0-9_]`. A stem straight after an
    /// underscore is inside a longer name (CARGO_PKG_RUST_VERSION) and
    /// is that name's, not a word of its own.
    fn stem_words(bytes: &[u8]) -> std::collections::BTreeSet<String> {
        let in_name = |b: &u8| b.is_ascii_uppercase() || b.is_ascii_digit() || *b == b'_';
        bytes
            .split(|b| !in_name(b))
            .flat_map(|run| {
                (0..run.len())
                    .filter(move |&i| {
                        (i == 0 || run[i - 1] != b'_')
                            && TOOLCHAIN_STEMS
                                .iter()
                                .any(|s| run[i..].starts_with(s.as_bytes()))
                    })
                    .map(move |i| String::from_utf8_lossy(&run[i..]).into_owned())
            })
            .collect()
    }

    /// The judgement a stem word gets: the longest name it can be read
    /// as that the floor refuses or the pass table names. A name ends
    /// where the word ends or where another stem word begins
    /// (CARGORUSTC_WORKSPACE_WRAPPER is CARGO, then RUSTC_…) — never
    /// anywhere else, which would let a refused CARGO stand for an unread
    /// CARGOX, or the pass table's RUST_ for any RUST_ name at all.
    fn judged(word: &str) -> Option<&str> {
        (1..=word.len()).rev().find_map(|k| {
            let name = &word[..k];
            let ends = k == word.len() || TOOLCHAIN_STEMS.iter().any(|s| word[k..].starts_with(s));
            (ends
                && (forbidden_env(name).is_some()
                    || TOOLCHAIN_PASSES.iter().any(|(n, _)| *n == name)))
            .then_some(name)
        })
    }

    /// THE FLOOR AGAINST THE TOOLCHAIN ITSELF (backlog 685ce87c,
    /// 2026-10-21). The hand list above grew one review at a time — CARGO,
    /// RUSTFMT and BROWSER were each found by a reviewer after the floor
    /// had shipped without them (dfd43fbd). This reads the names out of
    /// the pinned toolchain's own binaries and fails on any word in the
    /// toolchain's namespaces that the floor neither refuses nor passes
    /// with a written reason, so the next one fails here, not in a review.
    ///
    /// WHAT IT CANNOT SEE, said plainly: a name outside those namespaces
    /// (BROWSER, HOME) is indistinguishable from the rest of a binary's
    /// text, so those stay with the hand list and the dock hold; a stem
    /// word that runs into a literal starting with `_` reads as one longer
    /// name; and a name the compiler builds from eight-byte instruction
    /// immediates shows only its first eight bytes (RUST_BACH, RUSTFMT_H),
    /// which this then makes someone judge by hand.
    #[test]
    fn the_floor_judges_every_name_the_pinned_toolchain_carries() {
        let bin = Path::new(env!("CARGO"))
            .parent()
            .expect("cargo lives in a directory");
        let pinned = std::fs::read_to_string(repo().join("rust-toolchain.toml")).unwrap();
        let channel = pinned
            .lines()
            .find_map(|l| l.strip_prefix("channel = \""))
            .and_then(|l| l.strip_suffix('"'))
            .expect("rust-toolchain.toml names a channel");
        let version = std::process::Command::new(bin.join("rustc"))
            .arg("--version")
            .output()
            .expect("the toolchain's rustc runs");
        let version = String::from_utf8_lossy(&version.stdout).into_owned();
        assert!(
            version.starts_with(&format!("rustc {channel} ")),
            "{} is not the pinned toolchain {channel}: {version}",
            bin.display()
        );
        let mut unjudged = Vec::new();
        let mut used = std::collections::BTreeSet::new();
        for name in TOOLCHAIN_BINARIES {
            let bytes = std::fs::read(bin.join(name))
                .unwrap_or_else(|e| panic!("{} is unreadable: {e}", bin.join(name).display()));
            let words = stem_words(&bytes);
            assert!(
                !words.is_empty(),
                "{name}: no toolchain word read, so nothing was judged"
            );
            for word in words {
                match judged(&word) {
                    Some(n) => {
                        used.insert(n.to_owned());
                    }
                    None => unjudged.push(format!("{name}: {word}")),
                }
            }
        }
        assert!(
            unjudged.is_empty(),
            "the floor neither refuses nor passes, with a reason in TOOLCHAIN_PASSES, \
             these words the pinned toolchain carries:\n{}",
            unjudged.join("\n")
        );
        for (name, why) in TOOLCHAIN_PASSES {
            assert!(!why.is_empty(), "{name} passes with no reason");
            assert_eq!(
                forbidden_env(name),
                None,
                "{name} is in the pass table AND refused"
            );
            assert!(
                used.contains(*name),
                "{name} is in the pass table but no binary carries it any longer"
            );
        }
    }

    /// THE cc CRATE'S FAMILY (backlog 685ce87c, half 1). A build script
    /// that uses cc runs the program CC, CXX, AR, RANLIB or NVCC names,
    /// and hands the compiler and archiver CFLAGS, CXXFLAGS, ARFLAGS,
    /// RANLIBFLAGS and CXXSTDLIB — where -fplugin=, -B<dir>, -wrapper and
    /// a library name each name code. cc reads each under four spellings
    /// (`target_envs`, cc 1.2.64 src/lib.rs): NAME_<target>, NAME_<target
    /// with underscores>, HOST_NAME or TARGET_NAME, and NAME. Every one is
    /// refused here, for two targets, and so are the program prefix
    /// (CROSS_COMPILE), the directory whose clang cc runs (WASI_SDK_PATH),
    /// the sysroots whose libraries it links, and LDFLAGS, which other
    /// build tools hand the linker. What cc reads that names no code
    /// passes, each with its reason.
    #[test]
    fn the_cc_crate_family_is_refused_under_every_spelling_it_reads() {
        let text = std::fs::read_to_string(repo().join(DIR).join("codex-exec.toml")).unwrap();
        let mut names: Vec<String> = [
            "CROSS_COMPILE",
            "WASI_SDK_PATH",
            "WASI_SYSROOT",
            "WASM_MUSL_SYSROOT",
            "SDKROOT",
            "LIB",
            "LDFLAGS",
            "CC_KNOWN_WRAPPER_CUSTOM",
            "CC_SHELL_ESCAPED_FLAGS",
            "CC_ENABLE_DEBUG_OUTPUT",
            "CC_FORCE_DISABLE",
            "RUSTC_LINKER",
            "RUSTC_BOOTSTRAP",
        ]
        .map(String::from)
        .to_vec();
        for tool in CC_TOOLS {
            for target in ["x86_64-unknown-linux-gnu", "wasm32-wasip1"] {
                names.push(format!("{tool}_{}", target.replace(['-', '.'], "_")));
            }
            names.extend([
                tool.to_owned(),
                format!("HOST_{tool}"),
                format!("TARGET_{tool}"),
            ]);
        }
        for name in &names {
            assert_eq!(
                forbidden_env(name),
                Some("makes the process load or run code its argv never names"),
                "{name}"
            );
            let planted = text.replace("\"PATH\",", &format!("\"PATH\", \"{name}\","));
            assert_ne!(planted, text, "the plant landed");
            let refused =
                Harness::parse("codex-exec", &planted).expect_err(&format!("{name} is refused"));
            assert!(refused.contains(name.as_str()), "{refused}");
        }
        // The hyphenated spelling is not a plain name and never was.
        assert_eq!(
            forbidden_env("CC_x86_64-unknown-linux-gnu"),
            Some("is not a plain variable name")
        );
        // What cc reads that names no code passes, and why.
        for (name, _why) in [
            ("TARGET", "cargo sets it for every build script it runs"),
            ("HOST", "cargo sets it for every build script it runs"),
            ("OPT_LEVEL", "cargo sets it for every build script it runs"),
            ("DEBUG", "cargo sets it for every build script it runs"),
            ("OUT_DIR", "cargo sets it for every build script it runs"),
            ("NUM_JOBS", "cargo sets it for every build script it runs"),
            (
                "CRATE_CC_NO_DEFAULTS",
                "drops cc's default flags; names nothing",
            ),
            ("MACOSX_DEPLOYMENT_TARGET", "an OS version number"),
            ("IPHONEOS_DEPLOYMENT_TARGET", "an OS version number"),
            (
                "ZERO_AR_DATE",
                "a timestamp switch cc sets on its own archiver",
            ),
            ("VSLANG", "the language of MSVC's messages"),
            ("LC_ALL", "a locale"),
            (
                "PATH",
                "every pass list's own; what it names is the account's",
            ),
            // Names that only start with a family's letters still pass.
            ("CCACHE_DIR", "not cc's: no CC_ underscore"),
            ("ARCH", "not cc's: no AR_ underscore"),
            ("HOSTNAME", "not cc's: no HOST_ underscore"),
        ] {
            assert_eq!(forbidden_env(name), None, "{name}");
        }
    }

    /// The cc names above were read from the source of the cc the tree
    /// LOCKS, and a bump changes what a build script reads. So the
    /// version they were read from is held equal to Cargo.lock's (CLAUDE.md
    /// §9a): moving cc fails here until someone re-reads its
    /// `target_envs` and `get_env` calls and says so by moving this.
    #[test]
    fn the_cc_names_were_read_from_the_cc_the_tree_locks() {
        const CC_READ_FROM: &str = "1.2.64";
        let lock = std::fs::read_to_string(repo().join("Cargo.lock")).unwrap();
        let locked: Vec<&str> = lock
            .split("[[package]]")
            .filter(|p| p.lines().any(|l| l == "name = \"cc\""))
            .filter_map(|p| p.lines().find_map(|l| l.strip_prefix("version = \"")))
            .map(|v| v.trim_end_matches('"'))
            .collect();
        assert_eq!(
            locked,
            [CC_READ_FROM],
            "Cargo.lock's cc moved: re-read its env names, then move CC_READ_FROM"
        );
    }
}
